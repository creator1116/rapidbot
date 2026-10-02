//! Xbox Live user authentication and XSTS authorisation.

use serde::Deserialize;
use serde_json::json;

use crate::AuthError;

const USER_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MINECRAFT_RELYING_PARTY: &str = "rp://api.minecraftservices.com/";

pub(crate) struct XboxToken {
    pub token: String,
    /// User hash, needed for the Minecraft identity token.
    pub uhs: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct XboxResponse {
    token: String,
    display_claims: DisplayClaims,
}

#[derive(Deserialize)]
struct DisplayClaims {
    xui: Vec<Xui>,
}

#[derive(Deserialize)]
struct Xui {
    uhs: String,
}

#[derive(Deserialize)]
struct XboxError {
    #[serde(rename = "XErr")]
    xerr: u64,
    #[serde(rename = "Message", default)]
    message: String,
}

pub(crate) async fn user_token(http: &reqwest::Client, msa_access_token: &str) -> Result<XboxToken, AuthError> {
    // live.com tokens use the "t=" ticket prefix; Azure (MSAL) ones use "d=".
    let body = json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            "RpsTicket": format!("t={msa_access_token}"),
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT",
    });
    post(http, USER_AUTH_URL, &body, "2").await
}

pub(crate) async fn xsts_token(http: &reqwest::Client, user_token: &str) -> Result<XboxToken, AuthError> {
    let body = json!({
        "Properties": {
            "SandboxId": "RETAIL",
            "UserTokens": [user_token],
        },
        "RelyingParty": MINECRAFT_RELYING_PARTY,
        "TokenType": "JWT",
    });
    post(http, XSTS_URL, &body, "1").await
}

async fn post(
    http: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    contract_version: &str,
) -> Result<XboxToken, AuthError> {
    let response = http
        .post(url)
        .header("Accept", "application/json")
        .header("x-xbl-contract-version", contract_version)
        .json(body)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let detail = match serde_json::from_str::<XboxError>(&text) {
            Ok(e) => format!("{} ({})", xerr_message(e.xerr), e.message),
            Err(_) => format!("HTTP {status}: {text}"),
        };
        return Err(AuthError::Xbox(detail));
    }
    let r: XboxResponse = response.json().await?;
    let uhs = r.display_claims.xui.into_iter().next().map(|x| x.uhs).unwrap_or_default();
    Ok(XboxToken { token: r.token, uhs })
}

fn xerr_message(code: u64) -> &'static str {
    match code {
        2148916227 => "account banned from Xbox",
        2148916229 => "account restricted; a guardian must allow online play",
        2148916233 => "account has no Xbox profile; create one at xbox.com",
        2148916234 => "account has not accepted the Xbox terms of service",
        2148916235 => "Xbox Live is not available in this account's region",
        2148916236 | 2148916237 => "account needs adult verification",
        2148916238 => "account is under 18 and must be added to a family",
        _ => "XSTS authorisation failed",
    }
}
