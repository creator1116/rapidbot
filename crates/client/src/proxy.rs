//! SOCKS5 and HTTP CONNECT proxies for the game connection.
//!
//! Vanilla reaches a server through the JVM's proxy settings; this is the
//! explicit equivalent for one bot. The proxy receives the already-resolved
//! server address, so SRV lookup and the handshake host are unchanged.

use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use percent_encoding::percent_decode_str;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use url::Url;

const MAX_PROXY_RESPONSE: usize = 16 * 1024;

/// A proxy endpoint. `Socks5` also covers `socks5h`: the (already resolved)
/// destination IP is sent to the proxy either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proxy {
    Http {
        host: String,
        port: u16,
        auth: Option<ProxyAuth>,
    },
    Socks5 {
        host: String,
        port: u16,
        auth: Option<ProxyAuth>,
    },
}

/// Proxy credentials, used for HTTP Basic or SOCKS5 username/password auth.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyAuth {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for ProxyAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyAuth")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("invalid proxy URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("proxy URL has no host")]
    MissingHost,
    #[error("unsupported proxy scheme {0:?}; use http, socks5 or socks5h")]
    Scheme(String),
    #[error("proxy credentials are not valid UTF-8")]
    Credentials,
    #[error("proxy I/O: {0}")]
    Io(#[from] io::Error),
    #[error("SOCKS5 proxy: {0}")]
    Socks5(String),
    #[error("HTTP proxy: {0}")]
    Http(String),
}

impl std::str::FromStr for Proxy {
    type Err = ProxyError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

impl Proxy {
    /// Parses `http://[user:pass@]host[:port]` or
    /// `socks5://[user:pass@]host[:port]`. A bare `host:port` means HTTP.
    pub fn parse(input: &str) -> Result<Self, ProxyError> {
        let input = if input.contains("://") {
            input.to_owned()
        } else {
            format!("http://{input}")
        };
        let url = Url::parse(&input)?;
        let host = url.host_str().ok_or(ProxyError::MissingHost)?.to_owned();
        let auth = if url.username().is_empty() && url.password().is_none() {
            None
        } else {
            Some(ProxyAuth {
                username: decode(url.username())?,
                password: decode(url.password().unwrap_or(""))?,
            })
        };
        match url.scheme() {
            "http" => Ok(Self::Http {
                host,
                port: url.port().unwrap_or(8080),
                auth,
            }),
            "socks5" | "socks5h" => Ok(Self::Socks5 {
                host,
                port: url.port().unwrap_or(1080),
                auth,
            }),
            scheme => Err(ProxyError::Scheme(scheme.to_owned())),
        }
    }

    fn endpoint(&self) -> (&str, u16) {
        match self {
            Self::Http { host, port, .. } | Self::Socks5 { host, port, .. } => (host, *port),
        }
    }
}

fn decode(value: &str) -> Result<String, ProxyError> {
    percent_decode_str(value)
        .decode_utf8()
        .map(|value| value.into_owned())
        .map_err(|_| ProxyError::Credentials)
}

/// Opens a tunnel to `target` through `proxy`.
pub(crate) async fn connect(proxy: &Proxy, target: SocketAddr) -> Result<TcpStream, ProxyError> {
    let (host, port) = proxy.endpoint();
    let mut stream = TcpStream::connect((host, port)).await?;
    match proxy {
        Proxy::Http { auth, .. } => http_connect(&mut stream, target, auth.as_ref()).await?,
        Proxy::Socks5 { auth, .. } => socks5_connect(&mut stream, target, auth.as_ref()).await?,
    }
    Ok(stream)
}

async fn http_connect(
    stream: &mut TcpStream,
    target: SocketAddr,
    auth: Option<&ProxyAuth>,
) -> Result<(), ProxyError> {
    let authority = target.to_string();
    let mut request = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: Keep-Alive\r\n"
    );
    if let Some(auth) = auth {
        let credentials = BASE64.encode(format!("{}:{}", auth.username, auth.password));
        request.push_str(&format!("Proxy-Authorization: Basic {credentials}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;

    // Read exactly the response head. A CONNECT response has no body, and
    // stopping at the delimiter leaves no target bytes consumed.
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await?;
        response.push(byte[0]);
        if response.len() > MAX_PROXY_RESPONSE {
            return Err(ProxyError::Http("response headers too large".into()));
        }
    }
    let response = String::from_utf8_lossy(&response);
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");
    if status != "200" {
        let reason = response.lines().next().unwrap_or("").trim();
        return Err(ProxyError::Http(format!("CONNECT failed: {reason}")));
    }
    Ok(())
}

async fn socks5_connect(
    stream: &mut TcpStream,
    target: SocketAddr,
    auth: Option<&ProxyAuth>,
) -> Result<(), ProxyError> {
    let methods: &[u8] = if auth.is_some() {
        &[0x00, 0x02]
    } else {
        &[0x00]
    };
    let mut greeting = Vec::with_capacity(2 + methods.len());
    greeting.push(0x05);
    greeting.push(methods.len() as u8);
    greeting.extend_from_slice(methods);
    stream.write_all(&greeting).await?;

    let mut choice = [0u8; 2];
    stream.read_exact(&mut choice).await?;
    if choice[0] != 0x05 {
        return Err(ProxyError::Socks5(format!(
            "unexpected greeting version {}",
            choice[0]
        )));
    }
    match choice[1] {
        0x00 => {}
        0x02 => {
            let auth = auth.ok_or_else(|| {
                ProxyError::Socks5("proxy requires username/password auth".into())
            })?;
            socks5_auth(stream, auth).await?;
        }
        0xff => return Err(ProxyError::Socks5("no acceptable auth method".into())),
        method => {
            return Err(ProxyError::Socks5(format!(
                "unsupported auth method {method:#x}"
            )));
        }
    }

    let mut request = Vec::with_capacity(22);
    request.extend_from_slice(&[0x05, 0x01, 0x00]);
    match target.ip() {
        IpAddr::V4(ip) => {
            request.push(0x01);
            request.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            request.push(0x04);
            request.extend_from_slice(&ip.octets());
        }
    }
    request.extend_from_slice(&target.port().to_be_bytes());
    stream.write_all(&request).await?;

    let mut reply = [0u8; 4];
    stream.read_exact(&mut reply).await?;
    if reply[0] != 0x05 {
        return Err(ProxyError::Socks5(format!(
            "unexpected reply version {}",
            reply[0]
        )));
    }
    if reply[1] != 0x00 {
        return Err(ProxyError::Socks5(format!(
            "connect failed with code {}",
            reply[1]
        )));
    }
    match reply[3] {
        0x01 => skip(stream, 4).await?,
        0x04 => skip(stream, 16).await?,
        0x03 => {
            let mut length = [0u8; 1];
            stream.read_exact(&mut length).await?;
            skip(stream, length[0] as usize).await?;
        }
        atyp => {
            return Err(ProxyError::Socks5(format!(
                "unsupported address type {atyp}"
            )));
        }
    }
    skip(stream, 2).await?;
    Ok(())
}

async fn socks5_auth(stream: &mut TcpStream, auth: &ProxyAuth) -> Result<(), ProxyError> {
    let username = auth.username.as_bytes();
    let password = auth.password.as_bytes();
    if username.len() > 255 || password.len() > 255 {
        return Err(ProxyError::Socks5(
            "username or password is longer than 255 bytes".into(),
        ));
    }
    let mut request = Vec::with_capacity(3 + username.len() + password.len());
    request.extend_from_slice(&[0x01, username.len() as u8]);
    request.extend_from_slice(username);
    request.push(password.len() as u8);
    request.extend_from_slice(password);
    stream.write_all(&request).await?;

    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply).await?;
    if reply[0] != 0x01 || reply[1] != 0x00 {
        return Err(ProxyError::Socks5("username/password rejected".into()));
    }
    Ok(())
}

async fn skip(stream: &mut TcpStream, mut bytes: usize) -> Result<(), ProxyError> {
    let mut buffer = [0u8; 256];
    while bytes > 0 {
        let take = bytes.min(buffer.len());
        stream.read_exact(&mut buffer[..take]).await?;
        bytes -= take;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn parses_http_and_socks_urls() {
        assert_eq!(
            Proxy::parse("http://user:p%40ss@127.0.0.1:8080").unwrap(),
            Proxy::Http {
                host: "127.0.0.1".into(),
                port: 8080,
                auth: Some(ProxyAuth {
                    username: "user".into(),
                    password: "p@ss".into(),
                }),
            }
        );
        assert_eq!(
            Proxy::parse("socks5://127.0.0.1:1080").unwrap(),
            Proxy::Socks5 {
                host: "127.0.0.1".into(),
                port: 1080,
                auth: None,
            }
        );
        assert_eq!(
            Proxy::parse("socks5h://proxy.example").unwrap(),
            Proxy::Socks5 {
                host: "proxy.example".into(),
                port: 1080,
                auth: None,
            }
        );
        assert_eq!(
            Proxy::parse("localhost:3128").unwrap(),
            Proxy::Http {
                host: "localhost".into(),
                port: 3128,
                auth: None,
            }
        );
        let proxy = Proxy::parse("http://user:secret@localhost").unwrap();
        let debug = format!("{proxy:?}");
        assert!(debug.contains("user"));
        assert!(!debug.contains("secret"));
        assert!(matches!(
            Proxy::parse("https://localhost:8443"),
            Err(ProxyError::Scheme(_))
        ));
    }

    #[tokio::test]
    async fn http_connect_opens_a_tunnel() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_head(&mut socket).await;
            assert!(request.starts_with("CONNECT 127.0.0.1:25565 HTTP/1.1\r\n"));
            assert!(request.contains("Host: 127.0.0.1:25565\r\n"));
            assert!(request.contains("Proxy-Authorization: Basic dXNlcjpwYXNz\r\n"));
            socket
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            echo_once(&mut socket).await;
        });
        let proxy = Proxy::Http {
            host: "127.0.0.1".into(),
            port,
            auth: Some(ProxyAuth {
                username: "user".into(),
                password: "pass".into(),
            }),
        };
        let mut stream = connect(&proxy, "127.0.0.1:25565".parse().unwrap())
            .await
            .unwrap();
        stream.write_all(b"ping").await.unwrap();
        let mut echoed = [0u8; 4];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"ping");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_connect_reports_a_rejected_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = read_head(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
        });
        let proxy = Proxy::Http {
            host: "127.0.0.1".into(),
            port,
            auth: None,
        };
        let error = connect(&proxy, "127.0.0.1:25565".parse().unwrap())
            .await
            .unwrap_err();
        assert!(matches!(error, ProxyError::Http(message) if message.contains("407")));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn socks5_opens_a_tunnel_with_auth() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 4];
            socket.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [0x05, 0x02, 0x00, 0x02]);
            socket.write_all(&[0x05, 0x02]).await.unwrap();

            let mut auth = [0u8; 2];
            socket.read_exact(&mut auth).await.unwrap();
            assert_eq!(auth, [0x01, 4]);
            let mut username = [0u8; 4];
            socket.read_exact(&mut username).await.unwrap();
            assert_eq!(&username, b"user");
            let mut password_len = [0u8; 1];
            socket.read_exact(&mut password_len).await.unwrap();
            let mut password = vec![0u8; password_len[0] as usize];
            socket.read_exact(&mut password).await.unwrap();
            assert_eq!(&password, b"pass");
            socket.write_all(&[0x01, 0x00]).await.unwrap();

            let mut request = [0u8; 10];
            socket.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1, 0x63, 0xdd]);
            socket
                .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();
            echo_once(&mut socket).await;
        });
        let proxy = Proxy::Socks5 {
            host: "127.0.0.1".into(),
            port,
            auth: Some(ProxyAuth {
                username: "user".into(),
                password: "pass".into(),
            }),
        };
        let mut stream = connect(&proxy, "127.0.0.1:25565".parse().unwrap())
            .await
            .unwrap();
        stream.write_all(b"ping").await.unwrap();
        let mut echoed = [0u8; 4];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"ping");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn socks5_reports_a_refused_target() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            socket.read_exact(&mut greeting).await.unwrap();
            socket.write_all(&[0x05, 0x00]).await.unwrap();
            let mut request = [0u8; 10];
            socket.read_exact(&mut request).await.unwrap();
            socket
                .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        });
        let proxy = Proxy::Socks5 {
            host: "127.0.0.1".into(),
            port,
            auth: None,
        };
        let error = connect(&proxy, "127.0.0.1:25565".parse().unwrap())
            .await
            .unwrap_err();
        assert!(matches!(error, ProxyError::Socks5(message) if message.contains("code 5")));
        server.await.unwrap();
    }

    async fn read_head(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
        String::from_utf8(request).unwrap()
    }

    async fn echo_once(socket: &mut TcpStream) {
        let mut buffer = [0u8; 4];
        socket.read_exact(&mut buffer).await.unwrap();
        socket.write_all(&buffer).await.unwrap();
    }
}
