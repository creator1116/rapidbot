//! Microsoft account OAuth via login.live.com (device code flow).

use std::time::Duration;

use serde::Deserialize;
use tracing::debug;

use crate::{AuthError, CachedMsa, now_secs};

const DEVICE_CODE_URL: &str = "https://login.live.com/oauth20_connect.srf";
const TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";
const SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";

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
    refresh_token: String,
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
    on_device_code: &impl Fn(&DeviceCode),
) -> Result<CachedMsa, AuthError> {
    let code: DeviceCode = http
        .post(DEVICE_CODE_URL)
        .form(&[("client_id", client_id), ("scope", SCOPE), ("response_type", "device_code")])
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
            .post(TOKEN_URL)
            .form(&[
                ("client_id", client_id),
                ("device_code", code.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(to_cached(response.json().await?));
        }
        let err: ErrorResponse = response.json().await?;
        match err.error.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            _ => return Err(AuthError::Microsoft(format!("{}: {}", err.error, err.error_description))),
        }
        debug!("waiting for device code sign-in");
    }
    Err(AuthError::DeviceCodeExpired)
}

pub(crate) async fn refresh(
    http: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<CachedMsa, AuthError> {
    let response = http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("scope", SCOPE),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        let err: ErrorResponse = response.json().await?;
        return Err(AuthError::Microsoft(format!("{}: {}", err.error, err.error_description)));
    }
    Ok(to_cached(response.json().await?))
}

fn to_cached(t: TokenResponse) -> CachedMsa {
    CachedMsa { access_token: t.access_token, refresh_token: t.refresh_token, expires_at: now_secs() + t.expires_in }
}
