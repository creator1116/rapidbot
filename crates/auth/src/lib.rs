//! Microsoft account login for Minecraft Java Edition.
//!
//! Chain: Microsoft OAuth (device code) → Xbox Live user token → XSTS token
//! for `rp://api.minecraftservices.com/` → Minecraft access token → profile.
//! Tokens are refreshed as they expire, so the device-code prompt only
//! appears on first use; [`Authenticator::refresh_if_needed`] is the
//! background-safe entry point. [`AccountStore`] keeps one encrypted cache per
//! alias and puts its random key in the OS keyring; [`Authenticator::new`]
//! still accepts a plain-JSON path for tools and scripts.
//!
//! None of this reaches the game server directly: it only learns whether
//! [`join_server`] succeeded before it sent the encryption request.

mod accounts;
mod browser;
mod cache;
mod microsoft;
mod minecraft;
mod realms;
mod store;
mod xbox;

use std::net::Ipv4Addr;
use std::path::Path;

use tokio::net::TcpListener;
use tracing::{debug, info};
use uuid::Uuid;

pub use accounts::{AccountStatus, AccountStore, StoredAccount};
pub use cache::{CachedMinecraft, CachedMsa, TokenCache};
pub use microsoft::DeviceCode;
pub use minecraft::{ChatKeys, JoinError, join_server};
pub use realms::{Realm, RealmCompatibility, RealmJoin, RealmPlayer, RealmState, RealmsClient};
pub use store::{
    EncryptedFileTokenStore, FileTokenStore, KeyProvider, KeyringKeyProvider, TokenStore,
};

/// Public Xbox title client IDs usable with the live.com device-code flow.
pub mod titles {
    pub const MINECRAFT_NINTENDO_SWITCH: &str = "00000000441cc96b";
    pub const MINECRAFT_JAVA: &str = "00000000402b5328";
}

/// Token endpoint URLs. Tests point these at a local server so the whole
/// renewal chain can run without contacting Microsoft.
#[derive(Debug, Clone)]
pub(crate) struct Endpoints {
    pub microsoft_device_code: String,
    pub microsoft_authorize: String,
    pub microsoft_token: String,
    pub xbox_user_auth: String,
    pub xbox_xsts: String,
    pub minecraft_login: String,
    pub minecraft_profile: String,
    pub minecraft_certificates: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            microsoft_device_code: "https://login.live.com/oauth20_connect.srf".into(),
            microsoft_authorize: "https://login.live.com/oauth20_authorize.srf".into(),
            microsoft_token: "https://login.live.com/oauth20_token.srf".into(),
            xbox_user_auth: "https://user.auth.xboxlive.com/user/authenticate".into(),
            xbox_xsts: "https://xsts.auth.xboxlive.com/xsts/authorize".into(),
            minecraft_login: "https://api.minecraftservices.com/authentication/login_with_xbox"
                .into(),
            minecraft_profile: "https://api.minecraftservices.com/minecraft/profile".into(),
            minecraft_certificates: "https://api.minecraftservices.com/player/certificates".into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("cache file: {0}")]
    Io(#[from] std::io::Error),
    #[error("cache file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("token store: {0}")]
    Store(String),
    #[error("Microsoft login: {0}")]
    Microsoft(String),
    #[error("no cached Microsoft refresh token; sign in first")]
    NoRefreshToken,
    #[error("device code expired before it was used")]
    DeviceCodeExpired,
    #[error("Xbox Live: {0}")]
    Xbox(String),
    #[error("Minecraft services: {0}")]
    Minecraft(String),
    #[error("this account does not own Minecraft: Java Edition")]
    NoProfile,
    #[error("Realms: {0}")]
    Realms(String),
}

/// A logged-in Minecraft account.
#[derive(Debug, Clone)]
pub struct MinecraftAccount {
    pub name: String,
    pub id: Uuid,
    pub access_token: String,
    /// Chat signing keys, when [`Authenticator::login`] or
    /// [`Authenticator::refresh`] fetched them.
    pub chat_keys: Option<ChatKeys>,
}

pub struct Authenticator {
    http: reqwest::Client,
    client_id: String,
    endpoints: Endpoints,
    store: Box<dyn TokenStore>,
    cache: TokenCache,
}

pub(crate) const REFRESH_SKEW_SECS: i64 = 300;

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

impl Authenticator {
    /// Loads (or starts) a plain-JSON token cache at `cache_path`.
    pub fn new(cache_path: impl AsRef<Path>) -> Result<Self, AuthError> {
        Self::with_store(Box::new(FileTokenStore::new(cache_path)))
    }

    /// Loads (or starts) a cache from any [`TokenStore`].
    pub fn with_store(store: Box<dyn TokenStore>) -> Result<Self, AuthError> {
        Self::with_store_and_endpoints(store, Endpoints::default())
    }

    fn with_store_and_endpoints(
        store: Box<dyn TokenStore>,
        endpoints: Endpoints,
    ) -> Result<Self, AuthError> {
        let cache = store.load()?;
        Ok(Self {
            http: reqwest::Client::builder().build()?,
            client_id: titles::MINECRAFT_NINTENDO_SWITCH.into(),
            endpoints,
            store,
            cache,
        })
    }

    #[cfg(test)]
    fn with_endpoints(store: Box<dyn TokenStore>, endpoints: Endpoints) -> Result<Self, AuthError> {
        Self::with_store_and_endpoints(store, endpoints)
    }

    /// The cached account, if the store has signed in at least once.
    pub fn cached(&self) -> Option<&CachedMinecraft> {
        self.cache.minecraft.as_ref()
    }

    pub fn with_client_id(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = client_id.into();
        self
    }

    /// Forgets the cached tokens (the OS-keyring key too, if the store keeps
    /// one there).
    pub fn clear(&mut self) -> Result<(), AuthError> {
        self.cache = TokenCache::default();
        self.store.clear()
    }

    /// Returns a valid account, refreshing or re-authenticating as needed.
    /// `on_device_code` is called if the user has to sign in in a browser.
    pub async fn login(
        &mut self,
        on_device_code: impl Fn(&DeviceCode),
    ) -> Result<MinecraftAccount, AuthError> {
        let minecraft = match &self.cache.minecraft {
            Some(mc) if mc.expires_at - REFRESH_SKEW_SECS > now_secs() => {
                debug!("using cached Minecraft token");
                mc.clone()
            }
            _ => {
                let msa = self.msa_token(&on_device_code).await?;
                self.minecraft_login(&msa).await?
            }
        };

        let chat_keys = self.chat_keys(&minecraft.access_token).await?;
        Ok(MinecraftAccount {
            name: minecraft.name,
            id: minecraft.id,
            access_token: minecraft.access_token,
            chat_keys,
        })
    }

    /// Browser sign-in with OAuth PKCE. `on_url` receives the authorization
    /// URL; open it in any browser. The local callback is
    /// `http://127.0.0.1:<random>/callback`.
    ///
    /// The default Live client ID is a public title client: if Microsoft does
    /// not allow its loopback redirect, use [`login`](Self::login) (device
    /// code) or set a client ID registered for native/loopback redirects with
    /// [`with_client_id`](Self::with_client_id).
    pub async fn login_browser(
        &mut self,
        on_url: impl Fn(&str),
    ) -> Result<MinecraftAccount, AuthError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let pkce = browser::Pkce::new();
        let url = browser::authorize_url(
            &self.endpoints.microsoft_authorize,
            &self.client_id,
            &redirect_uri,
            &pkce,
        );
        on_url(&url);

        let code = browser::wait_for_code(listener, &pkce.state).await?;
        let msa = microsoft::authorization_code(
            &self.http,
            &self.client_id,
            &self.endpoints.microsoft_token,
            &code,
            &redirect_uri,
            &pkce.verifier,
        )
        .await?;
        self.cache.msa = Some(msa.clone());
        self.store.save(&self.cache)?;

        let minecraft = self.minecraft_login(&msa.access_token).await?;
        let chat_keys = self.chat_keys(&minecraft.access_token).await?;
        Ok(MinecraftAccount {
            name: minecraft.name,
            id: minecraft.id,
            access_token: minecraft.access_token,
            chat_keys,
        })
    }

    /// The cached account, without any network access. Returns `None` when the
    /// Minecraft token has expired; expired chat keys are omitted so the
    /// result is always safe to send.
    pub fn cached_account(&self) -> Option<MinecraftAccount> {
        let now_secs = now_secs();
        let minecraft = self
            .cache
            .minecraft
            .as_ref()
            .filter(|minecraft| minecraft.expires_at > now_secs)?;
        let now_ms = now_secs * 1000;
        Some(MinecraftAccount {
            name: minecraft.name.clone(),
            id: minecraft.id,
            access_token: minecraft.access_token.clone(),
            chat_keys: self
                .cache
                .chat_keys
                .clone()
                .filter(|keys| keys.expires_at > now_ms),
        })
    }

    /// Whether cached chat keys exist and should be replaced within
    /// `skew_secs`. Missing keys are not treated as expiring: an account can
    /// legitimately play without a chat session.
    pub fn chat_keys_expiring(&self, skew_secs: i64) -> bool {
        let now_ms = now_secs() * 1000;
        self.cache.chat_keys.as_ref().is_some_and(|keys| {
            keys.refreshed_after
                .saturating_sub(skew_secs.saturating_mul(1000))
                <= now_ms
        })
    }

    /// Whether the cached Minecraft token is missing or expires within
    /// `skew_secs`. A background task can use this before calling
    /// [`refresh`](Self::refresh).
    pub fn is_expiring(&self, skew_secs: i64) -> bool {
        self.cache
            .minecraft
            .as_ref()
            .is_none_or(|mc| mc.expires_at - skew_secs <= now_secs())
    }

    /// Renews only when a token is due, otherwise returns the cached account.
    /// This avoids a Microsoft round trip when a timer runs frequently. Chat
    /// keys are fetched with the current Minecraft token when only they are
    /// due; a missing or expiring Minecraft token renews the whole chain.
    pub async fn refresh_if_needed(
        &mut self,
        skew_secs: i64,
    ) -> Result<MinecraftAccount, AuthError> {
        if self.is_expiring(skew_secs) {
            return self.refresh().await;
        }
        let minecraft = self
            .cache
            .minecraft
            .clone()
            .ok_or(AuthError::NoRefreshToken)?;
        if self.chat_keys_expiring(skew_secs) {
            let chat_keys = self.chat_keys(&minecraft.access_token).await?;
            return Ok(MinecraftAccount {
                name: minecraft.name,
                id: minecraft.id,
                access_token: minecraft.access_token,
                chat_keys,
            });
        }
        match self.cached_account() {
            Some(account) => Ok(account),
            None => self.refresh().await,
        }
    }

    /// Renews the Microsoft, Xbox, Minecraft and chat-key tokens from the
    /// cached refresh token. Unlike [`login`](Self::login), this never falls
    /// back to a device-code prompt, so it is safe for a background task. It
    /// fails with [`AuthError::NoRefreshToken`] when the account has never
    /// signed in.
    pub async fn refresh(&mut self) -> Result<MinecraftAccount, AuthError> {
        let refresh_token = self
            .cache
            .msa
            .as_ref()
            .map(|msa| msa.refresh_token.clone())
            .ok_or(AuthError::NoRefreshToken)?;
        let msa = microsoft::refresh(
            &self.http,
            &self.client_id,
            &self.endpoints.microsoft_token,
            &refresh_token,
        )
        .await?;
        self.cache.msa = Some(msa.clone());
        self.store.save(&self.cache)?;

        let minecraft = self.minecraft_login(&msa.access_token).await?;
        let chat_keys = self.chat_keys(&minecraft.access_token).await?;
        Ok(MinecraftAccount {
            name: minecraft.name,
            id: minecraft.id,
            access_token: minecraft.access_token,
            chat_keys,
        })
    }

    async fn minecraft_login(
        &mut self,
        msa_access_token: &str,
    ) -> Result<CachedMinecraft, AuthError> {
        let user =
            xbox::user_token(&self.http, &self.endpoints.xbox_user_auth, msa_access_token).await?;
        let xsts = xbox::xsts_token(&self.http, &self.endpoints.xbox_xsts, &user.token).await?;
        let (access_token, expires_in) = minecraft::login_with_xbox(
            &self.http,
            &self.endpoints.minecraft_login,
            &xsts.uhs,
            &xsts.token,
        )
        .await?;
        let profile =
            minecraft::profile(&self.http, &self.endpoints.minecraft_profile, &access_token)
                .await?;
        info!(name = %profile.name, "Minecraft login complete");
        let mc = CachedMinecraft {
            access_token,
            expires_at: now_secs() + expires_in,
            name: profile.name,
            id: profile.id,
        };
        self.cache.minecraft = Some(mc.clone());
        self.store.save(&self.cache)?;
        Ok(mc)
    }

    async fn msa_token(
        &mut self,
        on_device_code: &impl Fn(&DeviceCode),
    ) -> Result<String, AuthError> {
        let now = now_secs();
        if let Some(msa) = &self.cache.msa {
            if msa.expires_at - REFRESH_SKEW_SECS > now {
                return Ok(msa.access_token.clone());
            }
            match microsoft::refresh(
                &self.http,
                &self.client_id,
                &self.endpoints.microsoft_token,
                &msa.refresh_token,
            )
            .await
            {
                Ok(fresh) => {
                    self.cache.msa = Some(fresh.clone());
                    self.store.save(&self.cache)?;
                    return Ok(fresh.access_token);
                }
                Err(e) => debug!("refresh failed, falling back to device code: {e}"),
            }
        }
        let fresh = microsoft::device_code_login(
            &self.http,
            &self.client_id,
            &self.endpoints.microsoft_device_code,
            &self.endpoints.microsoft_token,
            on_device_code,
        )
        .await?;
        self.cache.msa = Some(fresh.clone());
        self.store.save(&self.cache)?;
        Ok(fresh.access_token)
    }

    /// `AccountProfileKeyPairManager`: reuse cached keys until
    /// `refreshedAfter`, then fetch new ones. Failure is not fatal; vanilla
    /// simply plays without a chat session.
    async fn chat_keys(&mut self, access_token: &str) -> Result<Option<ChatKeys>, AuthError> {
        let now_ms = now_secs() * 1000;
        if let Some(keys) = &self.cache.chat_keys
            && keys.refreshed_after > now_ms
        {
            return Ok(Some(keys.clone()));
        }
        match minecraft::certificates(
            &self.http,
            &self.endpoints.minecraft_certificates,
            access_token,
        )
        .await
        {
            Ok(keys) => {
                self.cache.chat_keys = Some(keys.clone());
                self.store.save(&self.cache)?;
                Ok(Some(keys))
            }
            Err(e) => {
                tracing::warn!("could not fetch chat signing keys: {e}");
                Ok(self
                    .cache
                    .chat_keys
                    .clone()
                    .filter(|k| k.expires_at > now_ms))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::net::TcpStream;
    use tokio::task::JoinHandle;
    use uuid::Uuid;

    async fn mock_auth_server() -> (Endpoints, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let server_hits = hits.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let hits = server_hits.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 2048];
                    let header_end = loop {
                        let Ok(n) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        request.extend_from_slice(&chunk[..n]);
                        if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            break at + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]).into_owned();
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    while request.len() < header_end + content_length {
                        let Ok(n) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..n]);
                    }

                    let path = headers
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .split('?')
                        .next()
                        .unwrap_or("/")
                        .to_owned();
                    hits.lock().unwrap().push(path.clone());
                    let (status, body) = match path.as_str() {
                        "/microsoft/token" => (
                            "200 OK",
                            r#"{"access_token":"msa-access","refresh_token":"new-refresh","expires_in":3600}"#,
                        ),
                        "/microsoft/token-no-rotate" => (
                            "200 OK",
                            r#"{"access_token":"msa-access","expires_in":3600}"#,
                        ),
                        "/microsoft/token-html" => {
                            ("502 Bad Gateway", "<html><body>proxy error</body></html>")
                        }
                        "/xbox/user" => (
                            "200 OK",
                            r#"{"Token":"user-token","DisplayClaims":{"xui":[{"uhs":"uhs"}]}}"#,
                        ),
                        "/xbox/xsts" => (
                            "200 OK",
                            r#"{"Token":"xsts-token","DisplayClaims":{"xui":[{"uhs":"uhs"}]}}"#,
                        ),
                        "/minecraft/login" => (
                            "200 OK",
                            r#"{"access_token":"mc-access","expires_in":86400}"#,
                        ),
                        "/minecraft/profile" => (
                            "200 OK",
                            r#"{"id":"00000000-0000-0000-0000-000000000001","name":"Steve"}"#,
                        ),
                        "/minecraft/certificates" => (
                            "200 OK",
                            r#"{"keyPair":{"privateKey":"private","publicKey":"AQID"},"publicKeySignatureV2":"BA==","expiresAt":"2099-01-01T00:00:00Z","refreshedAfter":"2099-01-01T00:00:00Z"}"#,
                        ),
                        _ => (
                            "500 Internal Server Error",
                            r#"{"error":"unexpected request"}"#,
                        ),
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        let endpoints = Endpoints {
            microsoft_device_code: format!("{base}/microsoft/device"),
            microsoft_authorize: format!("{base}/microsoft/authorize"),
            microsoft_token: format!("{base}/microsoft/token"),
            xbox_user_auth: format!("{base}/xbox/user"),
            xbox_xsts: format!("{base}/xbox/xsts"),
            minecraft_login: format!("{base}/minecraft/login"),
            minecraft_profile: format!("{base}/minecraft/profile"),
            minecraft_certificates: format!("{base}/minecraft/certificates"),
        };
        (endpoints, hits, server)
    }

    fn temp_cache_path() -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("rapidbot-auth-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("tokens.json");
        (dir, path)
    }

    #[tokio::test]
    async fn browser_login_uses_pkce_and_caches_tokens() {
        let (endpoints, hits, server) = mock_auth_server().await;
        let (dir, path) = temp_cache_path();
        let mut auth =
            Authenticator::with_endpoints(Box::new(FileTokenStore::new(&path)), endpoints).unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let opener = tokio::spawn(async move {
            let authorization_url = rx.recv().await.unwrap();
            let url = url::Url::parse(&authorization_url).unwrap();
            let params: std::collections::HashMap<_, _> = url
                .query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect();
            assert_eq!(params["response_type"], "code");
            assert_eq!(params["code_challenge_method"], "S256");
            assert_eq!(params["code_challenge"].len(), 43);

            let redirect = url::Url::parse(&params["redirect_uri"]).unwrap();
            let mut callback = redirect.clone();
            callback.set_query(Some(&format!("code=test-code&state={}", params["state"])));
            let mut socket =
                TcpStream::connect((redirect.host_str().unwrap(), redirect.port().unwrap()))
                    .await
                    .unwrap();
            let request = format!(
                "GET {}?{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                callback.path(),
                callback.query().unwrap()
            );
            socket.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            socket.read_to_end(&mut response).await.unwrap();
            assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
        });

        let account = auth
            .login_browser(|url| tx.send(url.to_owned()).unwrap())
            .await
            .unwrap();
        opener.await.unwrap();
        assert_eq!(account.name, "Steve");
        assert_eq!(account.access_token, "mc-access");
        assert!(account.chat_keys.is_some());
        assert_eq!(
            hits.lock().unwrap().as_slice(),
            [
                "/microsoft/token",
                "/xbox/user",
                "/xbox/xsts",
                "/minecraft/login",
                "/minecraft/profile",
                "/minecraft/certificates"
            ]
        );
        let cache = TokenCache::load(&path).unwrap();
        assert_eq!(cache.msa.unwrap().refresh_token, "new-refresh");

        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refresh_renews_the_whole_chain_without_a_device_code() {
        let (endpoints, hits, server) = mock_auth_server().await;
        let (dir, path) = temp_cache_path();
        TokenCache {
            msa: Some(CachedMsa {
                access_token: "old-msa".into(),
                refresh_token: "old-refresh".into(),
                expires_at: now_secs() - 10,
            }),
            ..Default::default()
        }
        .save(&path)
        .unwrap();

        let mut auth =
            Authenticator::with_endpoints(Box::new(FileTokenStore::new(&path)), endpoints).unwrap();
        assert!(auth.is_expiring(REFRESH_SKEW_SECS));
        let account = auth.refresh().await.unwrap();
        assert_eq!(account.name, "Steve");
        assert_eq!(account.access_token, "mc-access");
        assert!(account.chat_keys.is_some());
        assert!(!auth.is_expiring(REFRESH_SKEW_SECS));

        let cache = TokenCache::load(&path).unwrap();
        assert_eq!(cache.msa.unwrap().refresh_token, "new-refresh");
        assert_eq!(cache.minecraft.unwrap().access_token, "mc-access");
        assert_eq!(
            hits.lock().unwrap().as_slice(),
            [
                "/microsoft/token",
                "/xbox/user",
                "/xbox/xsts",
                "/minecraft/login",
                "/minecraft/profile",
                "/minecraft/certificates"
            ]
        );

        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refresh_keeps_the_old_refresh_token_when_not_rotated() {
        let (mut endpoints, hits, server) = mock_auth_server().await;
        let base = endpoints
            .microsoft_token
            .strip_suffix("/microsoft/token")
            .unwrap()
            .to_owned();
        endpoints.microsoft_token = format!("{base}/microsoft/token-no-rotate");
        let (dir, path) = temp_cache_path();
        TokenCache {
            msa: Some(CachedMsa {
                access_token: "old-msa".into(),
                refresh_token: "old-refresh".into(),
                expires_at: now_secs() - 10,
            }),
            ..Default::default()
        }
        .save(&path)
        .unwrap();

        let mut auth =
            Authenticator::with_endpoints(Box::new(FileTokenStore::new(&path)), endpoints).unwrap();
        auth.refresh().await.unwrap();

        let cache = TokenCache::load(&path).unwrap();
        assert_eq!(cache.msa.unwrap().refresh_token, "old-refresh");
        assert_eq!(
            hits.lock().unwrap().as_slice(),
            [
                "/microsoft/token-no-rotate",
                "/xbox/user",
                "/xbox/xsts",
                "/minecraft/login",
                "/minecraft/profile",
                "/minecraft/certificates"
            ]
        );

        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refresh_reports_a_non_json_http_error() {
        let (mut endpoints, hits, server) = mock_auth_server().await;
        let base = endpoints
            .microsoft_token
            .strip_suffix("/microsoft/token")
            .unwrap()
            .to_owned();
        endpoints.microsoft_token = format!("{base}/microsoft/token-html");
        let (dir, path) = temp_cache_path();
        TokenCache {
            msa: Some(CachedMsa {
                access_token: "old-msa".into(),
                refresh_token: "old-refresh".into(),
                expires_at: now_secs() - 10,
            }),
            ..Default::default()
        }
        .save(&path)
        .unwrap();

        let mut auth =
            Authenticator::with_endpoints(Box::new(FileTokenStore::new(&path)), endpoints).unwrap();
        let error = auth.refresh().await.unwrap_err();
        assert!(matches!(&error, AuthError::Microsoft(message) if message.contains("HTTP 502")));
        assert_eq!(hits.lock().unwrap().as_slice(), ["/microsoft/token-html"]);

        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refresh_if_needed_reuses_fresh_tokens_without_network() {
        let (endpoints, hits, server) = mock_auth_server().await;
        let (dir, path) = temp_cache_path();
        let now = now_secs();
        TokenCache {
            msa: Some(CachedMsa {
                access_token: "msa-access".into(),
                refresh_token: "refresh".into(),
                expires_at: now + 3600,
            }),
            minecraft: Some(CachedMinecraft {
                access_token: "cached-mc".into(),
                expires_at: now + 3600,
                name: "Steve".into(),
                id: Uuid::from_u128(1),
            }),
            chat_keys: Some(ChatKeys {
                private_key_pem: "private".into(),
                public_key_der: vec![],
                signature_v2: vec![],
                expires_at: now * 1000 + 7_200_000,
                refreshed_after: now * 1000 + 3_600_000,
            }),
        }
        .save(&path)
        .unwrap();

        let mut auth =
            Authenticator::with_endpoints(Box::new(FileTokenStore::new(&path)), endpoints).unwrap();
        assert!(!auth.is_expiring(REFRESH_SKEW_SECS));
        assert!(!auth.chat_keys_expiring(REFRESH_SKEW_SECS));
        let account = auth.refresh_if_needed(REFRESH_SKEW_SECS).await.unwrap();
        assert_eq!(account.name, "Steve");
        assert_eq!(account.access_token, "cached-mc");
        assert!(account.chat_keys.is_some());
        assert!(hits.lock().unwrap().is_empty());

        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refresh_if_needed_only_rotates_due_chat_keys() {
        let (endpoints, hits, server) = mock_auth_server().await;
        let (dir, path) = temp_cache_path();
        let now = now_secs();
        TokenCache {
            msa: Some(CachedMsa {
                access_token: "old-msa".into(),
                refresh_token: "refresh".into(),
                expires_at: now + 3600,
            }),
            minecraft: Some(CachedMinecraft {
                access_token: "cached-mc".into(),
                expires_at: now + 3600,
                name: "Steve".into(),
                id: Uuid::from_u128(1),
            }),
            chat_keys: Some(ChatKeys {
                private_key_pem: "private".into(),
                public_key_der: vec![],
                signature_v2: vec![],
                expires_at: now * 1000 + 3_600_000,
                refreshed_after: now * 1000 - 1,
            }),
        }
        .save(&path)
        .unwrap();

        let mut auth =
            Authenticator::with_endpoints(Box::new(FileTokenStore::new(&path)), endpoints).unwrap();
        assert!(auth.chat_keys_expiring(REFRESH_SKEW_SECS));
        let account = auth.refresh_if_needed(REFRESH_SKEW_SECS).await.unwrap();
        assert_eq!(account.access_token, "cached-mc");
        assert!(account.chat_keys.unwrap().expires_at > now * 1000);
        assert_eq!(hits.lock().unwrap().as_slice(), ["/minecraft/certificates"]);
        assert_eq!(auth.cache.msa.unwrap().access_token, "old-msa");

        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cached_account_hides_expired_chat_keys() {
        let (dir, path) = temp_cache_path();
        TokenCache {
            minecraft: Some(CachedMinecraft {
                access_token: "cached".into(),
                expires_at: now_secs() + 3600,
                name: "Steve".into(),
                id: Uuid::from_u128(1),
            }),
            chat_keys: Some(ChatKeys {
                private_key_pem: "private".into(),
                public_key_der: vec![],
                signature_v2: vec![],
                expires_at: now_secs() * 1000 - 1,
                refreshed_after: now_secs() * 1000 - 1,
            }),
            ..Default::default()
        }
        .save(&path)
        .unwrap();

        let auth = Authenticator::new(&path).unwrap();
        let account = auth.cached_account().unwrap();
        assert_eq!(account.access_token, "cached");
        assert!(account.chat_keys.is_none());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cached_account_requires_a_live_minecraft_token() {
        let (dir, path) = temp_cache_path();
        TokenCache {
            minecraft: Some(CachedMinecraft {
                access_token: "expired".into(),
                expires_at: now_secs() - 1,
                name: "Steve".into(),
                id: Uuid::from_u128(1),
            }),
            ..Default::default()
        }
        .save(&path)
        .unwrap();

        let auth = Authenticator::new(&path).unwrap();
        assert!(auth.cached_account().is_none());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refresh_without_a_cached_account_does_not_touch_the_network() {
        let (dir, path) = temp_cache_path();
        let mut auth = Authenticator::new(&path).unwrap();
        assert!(matches!(
            auth.refresh().await,
            Err(AuthError::NoRefreshToken)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn login_reuses_a_valid_cached_minecraft_token() {
        let (dir, path) = temp_cache_path();
        TokenCache {
            minecraft: Some(CachedMinecraft {
                access_token: "cached".into(),
                expires_at: now_secs() + 3600,
                name: "Steve".into(),
                id: Uuid::from_u128(1),
            }),
            chat_keys: Some(ChatKeys {
                private_key_pem: "private".into(),
                public_key_der: vec![],
                signature_v2: vec![],
                expires_at: now_secs() * 1000 + 60_000,
                refreshed_after: now_secs() * 1000 + 60_000,
            }),
            ..Default::default()
        }
        .save(&path)
        .unwrap();

        let mut auth = Authenticator::new(&path).unwrap();
        let account = auth
            .login(|_| panic!("device code prompt for a cached account"))
            .await
            .unwrap();
        assert_eq!(account.access_token, "cached");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
