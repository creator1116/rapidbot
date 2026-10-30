//! Minecraft Realms: list a player's worlds and get the address to join one.
//!
//! Mirrors `com.mojang.realmsclient.client.RealmsClient`: the access token is
//! sent as the `sid` cookie together with the player name and game version,
//! and the version headers select the Java client's API behaviour. The
//! `realms_in_aks` feature flag moves requests to the alternate host, as it
//! does in vanilla.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AuthError, MinecraftAccount};

const BASE_URL: &str = "https://pc.realms.minecraft.net";
const ALTERNATIVE_URL: &str = "https://java.frontendlegacy.realms.minecraft-services.net";
const VERSION: &str = "26.3";
const PROTOCOL: i32 = 777;

/// `RealmsServer.State`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RealmState {
    Closed,
    Open,
    Uninitialized,
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for RealmState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::Uninitialized => "uninitialized",
            Self::Unknown => "unknown",
        })
    }
}

/// A player in a Realm's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealmPlayer {
    pub name: String,
    pub uuid: Uuid,
    #[serde(default)]
    pub operator: bool,
    #[serde(default)]
    pub accepted: bool,
    #[serde(default)]
    pub online: bool,
}

/// One entry from `GET /worlds`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Realm {
    pub id: u64,
    pub name: Option<String>,
    pub description: String,
    pub state: RealmState,
    pub expired: bool,
    pub expired_trial: bool,
    pub days_left: i32,
    pub world_type: String,
    pub active_slot: i32,
    pub players: Vec<RealmPlayer>,
}

/// `RealmsJoinInformation` from `GET /worlds/v1/{id}/join/pc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealmJoin {
    pub address: Option<String>,
    pub resource_pack_url: Option<String>,
    pub resource_pack_hash: Option<String>,
}

/// `RealmsClient.CompatibleVersionResponse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RealmCompatibility {
    Compatible,
    Outdated,
    Other,
    Unknown,
}

/// A Realms API client for one signed-in Minecraft account.
#[derive(Debug, Clone)]
pub struct RealmsClient {
    http: reqwest::Client,
    base_url: String,
    alternative_url: String,
    access_token: String,
    username: String,
    version: String,
    protocol: i32,
    feature_flags: tokio::sync::OnceCell<Vec<String>>,
}

impl RealmsClient {
    pub fn new(account: &MinecraftAccount) -> Self {
        Self::with_endpoints(account, BASE_URL, ALTERNATIVE_URL, VERSION, PROTOCOL)
    }

    fn with_endpoints(
        account: &MinecraftAccount,
        base_url: &str,
        alternative_url: &str,
        version: &str,
        protocol: i32,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            alternative_url: alternative_url.trim_end_matches('/').to_owned(),
            access_token: account.access_token.clone(),
            username: account.name.clone(),
            version: version.to_owned(),
            protocol,
            feature_flags: tokio::sync::OnceCell::new(),
        }
    }

    /// `RealmsClient.listRealms`.
    pub async fn list(&self) -> Result<Vec<Realm>, AuthError> {
        let body = self.get("/worlds").await?;
        let response: RealmListResponse = serde_json::from_str(&body)
            .map_err(|e| AuthError::Realms(format!("bad worlds response: {e}")))?;
        Ok(response.servers.into_iter().map(Realm::from).collect())
    }

    /// `RealmsClient.join`: the address and optional resource pack for a world.
    pub async fn join(&self, id: u64) -> Result<RealmJoin, AuthError> {
        let body = self.get(&format!("/worlds/v1/{id}/join/pc")).await?;
        serde_json::from_str(&body)
            .map_err(|e| AuthError::Realms(format!("bad join response for world {id}: {e}")))
    }

    /// The address to pass to the server list, with a clearer error when the
    /// Realm is closed or the address is missing.
    pub async fn join_address(&self, id: u64) -> Result<String, AuthError> {
        self.join(id)
            .await?
            .address
            .ok_or_else(|| AuthError::Realms(format!("world {id} did not return a join address")))
    }

    /// `RealmsClient.hasParentalConsent`.
    pub async fn available(&self) -> Result<bool, AuthError> {
        let body = self.get("/mco/available").await?;
        body.trim()
            .parse()
            .map_err(|_| AuthError::Realms(format!("bad availability response: {body:?}")))
    }

    /// `RealmsClient.clientCompatible`.
    pub async fn compatibility(&self) -> Result<RealmCompatibility, AuthError> {
        let body = self.get("/mco/client/compatible").await?;
        Ok(match body.trim() {
            "COMPATIBLE" => RealmCompatibility::Compatible,
            "OUTDATED" => RealmCompatibility::Outdated,
            "OTHER" => RealmCompatibility::Other,
            _ => RealmCompatibility::Unknown,
        })
    }

    async fn get(&self, path: &str) -> Result<String, AuthError> {
        let base = if self.use_alternative_url().await {
            &self.alternative_url
        } else {
            &self.base_url
        };
        self.get_from(base, path).await
    }

    async fn use_alternative_url(&self) -> bool {
        let flags = self
            .feature_flags
            .get_or_init(|| async {
                match self.get_from(&self.base_url, "/feature/v1").await {
                    Ok(body) => serde_json::from_str(&body).unwrap_or_else(|e| {
                        tracing::debug!("could not parse Realms feature flags: {e}");
                        Vec::new()
                    }),
                    Err(e) => {
                        tracing::debug!("could not fetch Realms feature flags: {e}");
                        Vec::new()
                    }
                }
            })
            .await;
        flags.iter().any(|flag| flag == "realms_in_aks")
    }

    async fn get_from(&self, base: &str, path: &str) -> Result<String, AuthError> {
        let response = self
            .http
            .get(format!("{base}{path}"))
            .header("Accept", "application/json")
            .header(
                "Cookie",
                format!(
                    "sid={};user={};version={}",
                    self.access_token, self.username, self.version
                ),
            )
            .header("Client-Ref", &self.version)
            .header("X-NetworkProtocolVersion", self.protocol.to_string())
            .header("Is-Prerelease", "false")
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if status.is_success() {
            Ok(body)
        } else {
            let body: String = body.chars().take(500).collect();
            Err(AuthError::Realms(format!("HTTP {status}: {body}")))
        }
    }
}

#[derive(Deserialize)]
struct RealmListResponse {
    #[serde(default)]
    servers: Vec<RealmResponse>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RealmResponse {
    id: u64,
    name: Option<String>,
    #[serde(default)]
    motd: String,
    #[serde(default = "closed_state")]
    state: RealmState,
    #[serde(default)]
    expired: bool,
    #[serde(default)]
    expired_trial: bool,
    #[serde(default)]
    days_left: i32,
    #[serde(default)]
    world_type: String,
    #[serde(default)]
    active_slot: i32,
    #[serde(default)]
    players: Vec<RealmPlayer>,
}

fn closed_state() -> RealmState {
    RealmState::Closed
}

impl From<RealmResponse> for Realm {
    fn from(r: RealmResponse) -> Self {
        Self {
            id: r.id,
            name: r.name,
            description: r.motd,
            state: r.state,
            expired: r.expired,
            expired_trial: r.expired_trial,
            days_left: r.days_left,
            world_type: r.world_type,
            active_slot: r.active_slot,
            players: r.players,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    async fn mock_server(
        feature_flags: &'static str,
        worlds: &'static str,
        join: &'static str,
    ) -> (String, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server_requests = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let requests = server_requests.clone();
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
                    let path = headers
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_owned();
                    requests.lock().unwrap().push(headers);
                    let body = if path.ends_with("/feature/v1") {
                        feature_flags
                    } else if path.ends_with("/join/pc") {
                        join
                    } else if path.ends_with("/worlds") {
                        worlds
                    } else {
                        r#"{"error":"unexpected request"}"#
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        (base, requests, server)
    }

    fn account() -> MinecraftAccount {
        MinecraftAccount {
            name: "Steve".into(),
            id: Uuid::from_u128(1),
            access_token: "mc-access".into(),
            chat_keys: None,
        }
    }

    fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
        request.lines().find_map(|line| {
            let (header, value) = line.split_once(':')?;
            header.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    #[tokio::test]
    async fn list_uses_the_realms_headers_and_parses_worlds() {
        let (base, requests, server) = mock_server(
            "[]",
            r#"{"servers":[{"id":42,"name":"Home","motd":"Friends","state":"OPEN","expired":false,"expiredTrial":false,"daysLeft":29,"worldType":"normal","activeSlot":1,"players":[{"name":"Alex","uuid":"00000000-0000-0000-0000-000000000002","operator":true,"accepted":true,"online":true}]}]}"#,
            "{}",
        )
        .await;
        let client = RealmsClient::with_endpoints(
            &account(),
            &format!("{base}/base"),
            &format!("{base}/alt"),
            "26.3",
            777,
        );
        let realms = client.list().await.unwrap();
        assert_eq!(realms.len(), 1);
        assert_eq!(realms[0].id, 42);
        assert_eq!(realms[0].name.as_deref(), Some("Home"));
        assert_eq!(realms[0].state, RealmState::Open);
        assert_eq!(realms[0].players[0].name, "Alex");

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /base/feature/v1 "));
        assert!(requests[1].starts_with("GET /base/worlds "));
        let request = &requests[1];
        assert_eq!(
            header(request, "Cookie"),
            Some("sid=mc-access;user=Steve;version=26.3")
        );
        assert_eq!(header(request, "Client-Ref"), Some("26.3"));
        assert_eq!(header(request, "X-NetworkProtocolVersion"), Some("777"));
        assert_eq!(header(request, "Is-Prerelease"), Some("false"));
        assert_eq!(header(request, "Accept"), Some("application/json"));

        server.abort();
    }

    #[tokio::test]
    async fn the_aks_flag_switches_to_the_alternative_host() {
        let (base, requests, server) =
            mock_server("[\"realms_in_aks\"]", r#"{"servers":[]}"#, "{}").await;
        let client = RealmsClient::with_endpoints(
            &account(),
            &format!("{base}/base"),
            &format!("{base}/alt"),
            "26.3",
            777,
        );
        assert!(client.list().await.unwrap().is_empty());

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].starts_with("GET /alt/worlds "));
        server.abort();
    }

    #[tokio::test]
    async fn join_returns_the_address_and_pack() {
        let (base, requests, server) = mock_server(
            "[]",
            "{}",
            r#"{"address":"127.0.0.1:25565","resourcePackUrl":"https://example.invalid/pack.zip","resourcePackHash":"abc"}"#,
        )
        .await;
        let client = RealmsClient::with_endpoints(
            &account(),
            &format!("{base}/base"),
            &format!("{base}/alt"),
            "26.3",
            777,
        );
        let info = client.join(42).await.unwrap();
        assert_eq!(info.address.as_deref(), Some("127.0.0.1:25565"));
        assert_eq!(
            info.resource_pack_url.as_deref(),
            Some("https://example.invalid/pack.zip")
        );
        assert_eq!(info.resource_pack_hash.as_deref(), Some("abc"));
        assert_eq!(client.join_address(42).await.unwrap(), "127.0.0.1:25565");

        let requests = requests.lock().unwrap();
        assert!(requests[1].starts_with("GET /base/worlds/v1/42/join/pc "));
        server.abort();
    }

    #[tokio::test]
    async fn http_errors_are_reported_with_the_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
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
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let path = headers
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let (status, body) = if path.ends_with("/feature/v1") {
                    ("200 OK", "[]")
                } else {
                    ("401 Unauthorized", r#"{"errorMsg":"unauthorized"}"#)
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        let client = RealmsClient::with_endpoints(&account(), &base, &base, "26.3", 777);
        let error = client.list().await.unwrap_err();
        assert!(matches!(&error, AuthError::Realms(message) if message.contains("HTTP 401")));
        server.abort();
    }
}
