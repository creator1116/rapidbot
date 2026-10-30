//! Webshare proxy-list API.
//!
//! The list endpoint returns the same direct proxies shown in the Webshare
//! dashboard. Each entry is converted to an HTTP CONNECT [`Proxy`] with its
//! username/password, ready for [`ClientConfig::proxy`](crate::ClientConfig).

use std::fmt;

use serde::Deserialize;

use crate::proxy::{Proxy, ProxyAuth};

const DEFAULT_BASE_URL: &str = "https://proxy.webshare.io/api/v2";

/// Which Webshare connection mode to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebshareMode {
    Direct,
    Backbone,
}

impl WebshareMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Backbone => "backbone",
        }
    }
}

/// One proxy from `GET /proxy/list/`.
#[derive(Clone, PartialEq, Eq)]
pub struct WebshareProxy {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    password: String,
    pub valid: bool,
    pub country_code: Option<String>,
    pub city_name: Option<String>,
}

impl WebshareProxy {
    /// Converts the API entry to the game-connection proxy type.
    pub fn proxy(&self) -> Proxy {
        let auth = if self.username.is_empty() && self.password.is_empty() {
            None
        } else {
            Some(ProxyAuth {
                username: self.username.clone(),
                password: self.password.clone(),
            })
        };
        Proxy::Http {
            host: self.host.clone(),
            port: self.port,
            auth,
        }
    }
}

impl fmt::Debug for WebshareProxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebshareProxy")
            .field("id", &self.id)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("valid", &self.valid)
            .field("country_code", &self.country_code)
            .field("city_name", &self.city_name)
            .finish()
    }
}

/// A client for the Webshare v2 proxy-list API.
#[derive(Clone)]
pub struct WebshareClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl WebshareClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base(api_key, DEFAULT_BASE_URL)
    }

    fn with_base(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
        }
    }

    /// Fetches every proxy in the plan, following the API's pagination.
    pub async fn list(&self) -> Result<Vec<WebshareProxy>, WebshareError> {
        self.list_mode(WebshareMode::Direct).await
    }

    /// Fetches only proxies the API currently marks as valid.
    pub async fn list_valid(&self) -> Result<Vec<WebshareProxy>, WebshareError> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|proxy| proxy.valid)
            .collect())
    }

    pub async fn list_mode(&self, mode: WebshareMode) -> Result<Vec<WebshareProxy>, WebshareError> {
        let mut next = Some(format!(
            "{}/proxy/list/?mode={}&page=1&page_size=100",
            self.base_url,
            mode.as_str()
        ));
        let mut proxies = Vec::new();
        while let Some(url) = next {
            let page: Page = self.get(&url).await?;
            proxies.extend(page.results.into_iter().map(WebshareProxy::from));
            next = page.next.map(|url| self.resolve(&url));
        }
        Ok(proxies)
    }

    fn resolve(&self, url: &str) -> String {
        if url.starts_with("http://") || url.starts_with("https://") {
            url.to_owned()
        } else {
            format!("{}/{}", self.base_url, url.trim_start_matches('/'))
        }
    }

    async fn get(&self, url: &str) -> Result<Page, WebshareError> {
        let response = self
            .http
            .get(url)
            .header("Authorization", format!("Token {}", self.api_key))
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            let body: String = body.chars().take(500).collect();
            return Err(WebshareError::Status { status, body });
        }
        serde_json::from_str(&body).map_err(WebshareError::Json)
    }
}

impl fmt::Debug for WebshareClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebshareClient")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WebshareError {
    #[error("Webshare HTTP request: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Webshare returned HTTP {status}: {body}")]
    Status {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("Webshare response: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    results: Vec<ProxyEntry>,
}

#[derive(Deserialize)]
struct ProxyEntry {
    id: String,
    username: String,
    password: String,
    proxy_address: String,
    port: u16,
    #[serde(default)]
    valid: bool,
    #[serde(default)]
    country_code: Option<String>,
    #[serde(default)]
    city_name: Option<String>,
}

impl From<ProxyEntry> for WebshareProxy {
    fn from(entry: ProxyEntry) -> Self {
        Self {
            id: entry.id,
            host: entry.proxy_address,
            port: entry.port,
            username: entry.username,
            password: entry.password,
            valid: entry.valid,
            country_code: entry.country_code,
            city_name: entry.city_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn list_follows_pagination_and_builds_proxies() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server_requests = requests.clone();
        let server_base = format!("{base}/api/v2");
        let server = tokio::spawn(async move {
            for page in 1..=2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_head(&mut socket).await;
                server_requests.lock().unwrap().push(request);
                let body = if page == 1 {
                    format!(
                        r#"{{"count":2,"next":"{server_base}/proxy/list/?mode=direct&page=2&page_size=100","results":[{{"id":"d-1","username":"user","password":"secret","proxy_address":"1.2.3.4","port":8168,"valid":true,"country_code":"US","city_name":"New York"}}]}}"#
                    )
                } else {
                    r#"{"count":2,"next":null,"results":[{"id":"d-2","username":"user","password":"secret","proxy_address":"5.6.7.8","port":8169,"valid":false,"country_code":"DE","city_name":null}]}"#.into()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let client = WebshareClient::with_base("test-key", format!("{base}/api/v2"));
        let proxies = client.list().await.unwrap();
        assert_eq!(proxies.len(), 2);
        assert_eq!(proxies[0].id, "d-1");
        assert_eq!(proxies[0].country_code.as_deref(), Some("US"));
        assert_eq!(
            proxies[0].proxy(),
            Proxy::Http {
                host: "1.2.3.4".into(),
                port: 8168,
                auth: Some(ProxyAuth {
                    username: "user".into(),
                    password: "secret".into(),
                }),
            }
        );
        assert!(!proxies[1].valid);
        let debug = format!("{:?}", proxies[0]);
        assert!(debug.contains("user"));
        assert!(!debug.contains("secret"));

        server.await.unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0]
                .starts_with("GET /api/v2/proxy/list/?mode=direct&page=1&page_size=100 HTTP/1.1")
        );
        assert!(
            requests[1]
                .starts_with("GET /api/v2/proxy/list/?mode=direct&page=2&page_size=100 HTTP/1.1")
        );
        assert_eq!(
            header(&requests[0], "Authorization"),
            Some("Token test-key")
        );
    }

    #[tokio::test]
    async fn list_valid_drops_invalid_proxies() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = read_head(&mut socket).await;
            let body = r#"{"count":2,"next":null,"results":[{"id":"d-1","username":"u","password":"p","proxy_address":"1.2.3.4","port":1,"valid":true},{"id":"d-2","username":"u","password":"p","proxy_address":"5.6.7.8","port":2,"valid":false}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let client = WebshareClient::with_base("test-key", &base);
        let proxies = client.list_valid().await.unwrap();
        assert_eq!(proxies.len(), 1);
        assert_eq!(proxies[0].id, "d-1");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn api_errors_include_the_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = read_head(&mut socket).await;
            let body = r#"{"detail":"Invalid API key"}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let client = WebshareClient::with_base("bad-key", &base);
        let error = client.list().await.unwrap_err();
        assert!(matches!(error, WebshareError::Status { status, ref body }
            if status == reqwest::StatusCode::UNAUTHORIZED && body.contains("Invalid API key")));
        server.await.unwrap();
    }

    async fn read_head(socket: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
        String::from_utf8(request).unwrap()
    }

    fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
        request.lines().find_map(|line| {
            let (header, value) = line.split_once(':')?;
            header.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }
}
