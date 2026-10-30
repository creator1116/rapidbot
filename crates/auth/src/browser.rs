//! Browser-based Microsoft sign-in with OAuth PKCE.
//!
//! The bot listens on a loopback callback, opens the authorization URL, and
//! exchanges the returned code for Microsoft tokens. No client secret is
//! involved: the PKCE verifier proves that the callback belongs to this
//! process. The device-code flow remains the default because some titles only
//! allow their registered redirect URI; [`Authenticator::login_browser`]
//! documents that caveat.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::debug;

use crate::AuthError;

const CALLBACK_TIMEOUT_SECS: u64 = 300;

pub(crate) struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

impl Pkce {
    pub fn new() -> Self {
        let verifier = random_token(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
            state: random_token(16),
        }
    }
}

fn random_token(bytes: usize) -> String {
    let mut bytes = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Builds the `oauth20_authorize.srf` URL used by the Live Connect PKCE flow.
pub(crate) fn authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    pkce: &Pkce,
) -> String {
    format!(
        "{}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}\
         &code_challenge={}&code_challenge_method=S256&prompt=select_account",
        authorization_endpoint.trim_end_matches('?'),
        encode(client_id),
        encode(redirect_uri),
        encode(crate::microsoft::SCOPE),
        encode(&pkce.state),
        encode(&pkce.challenge),
    )
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// Waits for the browser callback and returns the authorization code.
/// Invalid state values are rejected without dropping the listener, so a
/// stray request cannot consume the sign-in.
pub(crate) async fn wait_for_code(
    listener: TcpListener,
    expected_state: &str,
) -> Result<String, AuthError> {
    let wait = async {
        loop {
            let (mut socket, _) = listener.accept().await?;
            let request = match read_request_head(&mut socket).await {
                Ok(request) => request,
                Err(e) => {
                    debug!("could not read browser callback: {e}");
                    continue;
                }
            };
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let (path, query) = target.split_once('?').unwrap_or((target, ""));
            if path != "/callback" {
                respond(&mut socket, 404, "Not found").await;
                continue;
            }
            if query_param(query, "state").as_deref() != Some(expected_state) {
                respond(&mut socket, 400, "Sign-in state did not match.").await;
                continue;
            }
            if let Some(error) = query_param(query, "error") {
                let description =
                    query_param(query, "error_description").unwrap_or_else(|| "no details".into());
                respond(
                    &mut socket,
                    200,
                    "Sign-in was cancelled. You can close this window.",
                )
                .await;
                return Err(AuthError::Microsoft(format!("{error}: {description}")));
            }
            let Some(code) = query_param(query, "code") else {
                respond(&mut socket, 400, "Sign-in response had no code.").await;
                continue;
            };
            respond(
                &mut socket,
                200,
                "Sign-in complete. You can close this window.",
            )
            .await;
            return Ok(code);
        }
    };
    tokio::time::timeout(Duration::from_secs(CALLBACK_TIMEOUT_SECS), wait)
        .await
        .map_err(|_| AuthError::Microsoft("browser sign-in timed out".into()))?
}

async fn read_request_head(socket: &mut TcpStream) -> std::io::Result<String> {
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        socket.read_exact(&mut byte).await?;
        request.push(byte[0]);
        if request.len() > 16 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "callback request headers too large",
            ));
        }
    }
    Ok(String::from_utf8_lossy(&request).into_owned())
}

async fn respond(socket: &mut TcpStream, status: u16, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>rapidbot</title><p>{}</p>",
        message.replace('&', "&amp;").replace('<', "&lt;")
    );
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        (name == key).then(|| percent_decode_str(value).decode_utf8_lossy().into_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_sha256_of_the_verifier() {
        let pkce = Pkce::new();
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes())),
            pkce.challenge
        );
        assert!(!pkce.verifier.contains('='));
        assert!(!pkce.challenge.contains('='));
    }

    #[test]
    fn authorize_url_has_the_pkce_parameters() {
        let pkce = Pkce {
            verifier: "v".into(),
            challenge: "c".into(),
            state: "s".into(),
        };
        let url = authorize_url(
            "https://login.live.com/oauth20_authorize.srf",
            "client",
            "http://127.0.0.1:1234/callback",
            &pkce,
        );
        assert!(url.starts_with("https://login.live.com/oauth20_authorize.srf?"));
        let query = url.split_once('?').unwrap().1;
        assert_eq!(query_param(query, "client_id").as_deref(), Some("client"));
        assert_eq!(query_param(query, "response_type").as_deref(), Some("code"));
        assert_eq!(
            query_param(query, "redirect_uri").as_deref(),
            Some("http://127.0.0.1:1234/callback")
        );
        assert_eq!(query_param(query, "code_challenge").as_deref(), Some("c"));
        assert_eq!(
            query_param(query, "code_challenge_method").as_deref(),
            Some("S256")
        );
        assert_eq!(query_param(query, "state").as_deref(), Some("s"));
    }

    #[tokio::test]
    async fn callback_ignores_a_wrong_state_and_returns_the_code() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { wait_for_code(listener, "expected").await });
        let wrong = request(
            address,
            "GET /callback?code=bad&state=wrong HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        assert!(wrong.starts_with("HTTP/1.1 400"));
        let right = request(
            address,
            "GET /callback?code=good%2Bcode&state=expected HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        assert!(right.starts_with("HTTP/1.1 200"));
        assert_eq!(server.await.unwrap().unwrap(), "good+code");
    }

    async fn request(address: std::net::SocketAddr, head: &str) -> String {
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket.write_all(head.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    }
}
