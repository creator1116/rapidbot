//! Microsoft account login for Minecraft Java Edition.
//!
//! Chain: Microsoft OAuth (device code) → Xbox Live user token → XSTS token
//! for `rp://api.minecraftservices.com/` → Minecraft access token → profile.
//! Tokens are refreshed as they expire, so the device-code prompt only
//! appears on first use. [`AccountStore`] keeps one encrypted cache per
//! alias and puts its random key in the OS keyring; [`Authenticator::new`]
//! still accepts a plain-JSON path for tools and scripts.
//!
//! None of this reaches the game server directly: it only learns whether
//! [`join_server`] succeeded before it sent the encryption request.

mod accounts;
mod cache;
mod microsoft;
mod minecraft;
mod store;
mod xbox;

use std::path::Path;

use tracing::{debug, info};
use uuid::Uuid;

pub use accounts::{AccountStatus, AccountStore, StoredAccount};
pub use cache::{CachedMinecraft, CachedMsa, TokenCache};
pub use microsoft::DeviceCode;
pub use minecraft::{ChatKeys, JoinError, join_server};
pub use store::{
    EncryptedFileTokenStore, FileTokenStore, KeyProvider, KeyringKeyProvider, TokenStore,
};

/// Public Xbox title client IDs usable with the live.com device-code flow.
pub mod titles {
    pub const MINECRAFT_NINTENDO_SWITCH: &str = "00000000441cc96b";
    pub const MINECRAFT_JAVA: &str = "00000000402b5328";
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
    #[error("device code expired before it was used")]
    DeviceCodeExpired,
    #[error("Xbox Live: {0}")]
    Xbox(String),
    #[error("Minecraft services: {0}")]
    Minecraft(String),
    #[error("this account does not own Minecraft: Java Edition")]
    NoProfile,
}

/// A logged-in Minecraft account.
#[derive(Debug, Clone)]
pub struct MinecraftAccount {
    pub name: String,
    pub id: Uuid,
    pub access_token: String,
    /// Chat signing keys, present when [`Authenticator::login`] fetched them.
    pub chat_keys: Option<ChatKeys>,
}

pub struct Authenticator {
    http: reqwest::Client,
    client_id: String,
    store: Box<dyn TokenStore>,
    cache: TokenCache,
}

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
        let cache = store.load()?;
        Ok(Self {
            http: reqwest::Client::builder().build()?,
            client_id: titles::MINECRAFT_NINTENDO_SWITCH.into(),
            store,
            cache,
        })
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
        let now = now_secs();
        let minecraft = match &self.cache.minecraft {
            Some(mc) if mc.expires_at - 300 > now => {
                debug!("using cached Minecraft token");
                mc.clone()
            }
            _ => {
                let msa = self.msa_token(&on_device_code).await?;
                let user = xbox::user_token(&self.http, &msa).await?;
                let xsts = xbox::xsts_token(&self.http, &user.token).await?;
                let (access_token, expires_in) =
                    minecraft::login_with_xbox(&self.http, &xsts.uhs, &xsts.token).await?;
                let profile = minecraft::profile(&self.http, &access_token).await?;
                info!(name = %profile.name, "Minecraft login complete");
                let mc = CachedMinecraft {
                    access_token,
                    expires_at: now + expires_in,
                    name: profile.name,
                    id: profile.id,
                };
                self.cache.minecraft = Some(mc.clone());
                self.store.save(&self.cache)?;
                mc
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

    async fn msa_token(
        &mut self,
        on_device_code: &impl Fn(&DeviceCode),
    ) -> Result<String, AuthError> {
        let now = now_secs();
        if let Some(msa) = &self.cache.msa {
            if msa.expires_at - 300 > now {
                return Ok(msa.access_token.clone());
            }
            match microsoft::refresh(&self.http, &self.client_id, &msa.refresh_token).await {
                Ok(fresh) => {
                    self.cache.msa = Some(fresh.clone());
                    self.store.save(&self.cache)?;
                    return Ok(fresh.access_token);
                }
                Err(e) => debug!("refresh failed, falling back to device code: {e}"),
            }
        }
        let fresh =
            microsoft::device_code_login(&self.http, &self.client_id, on_device_code).await?;
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
        match minecraft::certificates(&self.http, access_token).await {
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
