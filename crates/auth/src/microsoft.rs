//! Microsoft account OAuth via login.live.com (device code flow).

use std::time::Duration;

use serde::Deserialize;
use tracing::debug;

use crate::{AuthError, CachedMsa, now_secs};

pub(crate) const SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";

/// What the user needs to sign in: open `verification_uri`, enter `user_code`.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub user_code: String,
    pub device_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    /// OAuth token responses may omit a rotated refresh token; callers must
    /// keep the one they already have in that case.
    refresh_token: Option<String>,
    expires_in: i64,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
    #[serde(default)]
    error_description: String,
}

pub(crate) async fn device_code_login(
    http: &reqwest::Client,
    client_id: &str,
    device_code_url: &str,
    token_url: &str,
    on_device_code: &impl Fn(&DeviceCode),
) -> Result<CachedMsa, AuthError> {
    let code: DeviceCode = http
        .post(device_code_url)
        .form(&[
            ("client_id", client_id),
            ("scope", SCOPE),
            ("response_type", "device_code"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    on_device_code(&code);

    let deadline = now_secs() + code.expires_in as i64;
    let mut interval = code.interval.max(1);
    while now_secs() < deadline {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let response = http
            .post(token_url)
            .form(&[
                ("client_id", client_id),
                ("device_code", code.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?;
        if response.status().is_success() {
            return to_cached(response.json().await?, None);
        }
        let err = error_response(response).await?;
        match err.error.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            _ => return Err(error_to_auth(err)),
        }
        debug!("waiting for device code sign-in");
    }
    Err(AuthError::DeviceCodeExpired)
}

pub(crate) async fn authorization_code(
    http: &reqwest::Client,
    client_id: &str,
    token_url: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<CachedMsa, AuthError> {
    let response = http
        .post(token_url)
        .form(&[
            ("client_id", client_id),
            ("scope", SCOPE),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(match error_response(response).await {
            Ok(err) => error_to_auth(err),
            Err(err) => err,
        });
    }
    to_cached(response.json().await?, None)
}

pub(crate) async fn refresh(
    http: &reqwest::Client,
    client_id: &str,
    token_url: &str,
    refresh_token: &str,
) -> Result<CachedMsa, AuthError> {
    let response = http
        .post(token_url)
        .form(&[
            ("client_id", client_id),
            ("scope", SCOPE),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(match error_response(response).await {
            Ok(err) => error_to_auth(err),
            Err(err) => err,
        });
    }
    to_cached(response.json().await?, Some(refresh_token))
}

async fn error_response(response: reqwest::Response) -> Result<ErrorResponse, AuthError> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    serde_json::from_str(&body).map_err(|_| {
        let body: String = body.chars().take(500).collect();
        AuthError::Microsoft(format!("HTTP {status}: {body}"))
    })
}

fn error_to_auth(err: ErrorResponse) -> AuthError {
    AuthError::Microsoft(format!("{}: {}", err.error, err.error_description))
}

fn to_cached(t: TokenResponse, previous_refresh: Option<&str>) -> Result<CachedMsa, AuthError> {
    let refresh_token = t
        .refresh_token
        .or_else(|| previous_refresh.map(str::to_owned))
        .ok_or_else(|| AuthError::Microsoft("token response omitted refresh_token".into()))?;
    Ok(CachedMsa {
        access_token: t.access_token,
        refresh_token,
        expires_at: now_secs() + t.expires_in,
    })
}
