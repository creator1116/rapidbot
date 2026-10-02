//! Minecraft services: Xbox login, profile, chat certificates, session join.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::AuthError;

const LOGIN_WITH_XBOX_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";
const CERTIFICATES_URL: &str = "https://api.minecraftservices.com/player/certificates";
const JOIN_URL: &str = "https://sessionserver.mojang.com/session/minecraft/join";

#[derive(Deserialize)]
struct LoginResponse {
    access_token: String,
    expires_in: i64,
}

pub(crate) async fn login_with_xbox(
    http: &reqwest::Client,
    uhs: &str,
    xsts_token: &str,
) -> Result<(String, i64), AuthError> {
    let response = http
        .post(LOGIN_WITH_XBOX_URL)
        .json(&json!({ "identityToken": format!("XBL3.0 x={uhs};{xsts_token}") }))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(AuthError::Minecraft(format!("login_with_xbox: HTTP {status}: {}", response.text().await?)));
    }
    let r: LoginResponse = response.json().await?;
    Ok((r.access_token, r.expires_in))
}

pub(crate) struct Profile {
    pub id: Uuid,
    pub name: String,
}

#[derive(Deserialize)]
struct ProfileResponse {
    id: String,
    name: String,
}

pub(crate) async fn profile(http: &reqwest::Client, access_token: &str) -> Result<Profile, AuthError> {
    let response = http.get(PROFILE_URL).bearer_auth(access_token).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(AuthError::NoProfile);
    }
    let r: ProfileResponse = response.error_for_status()?.json().await?;
    let id = Uuid::parse_str(&r.id).map_err(|e| AuthError::Minecraft(format!("bad profile id: {e}")))?;
    Ok(Profile { id, name: r.name })
}

/// A chat signing key pair (`ProfileKeyPair`) from `/player/certificates`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatKeys {
    /// PKCS#8 private key, PEM (Mojang labels it "RSA PRIVATE KEY").
    pub private_key_pem: String,
    /// X.509 SubjectPublicKeyInfo DER: the bytes `PublicKey.getEncoded()`
    /// returns, sent in `chat_session_update`.
    pub public_key_der: Vec<u8>,
    /// Mojang's signature over profile id, expiry and key (`publicKeySignatureV2`).
    pub signature_v2: Vec<u8>,
    /// Unix milliseconds.
    pub expires_at: i64,
    /// Unix milliseconds after which vanilla fetches a new pair.
    pub refreshed_after: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CertificatesResponse {
    key_pair: KeyPair,
    public_key_signature_v2: String,
    expires_at: String,
    refreshed_after: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyPair {
    private_key: String,
    public_key: String,
}

pub(crate) async fn certificates(http: &reqwest::Client, access_token: &str) -> Result<ChatKeys, AuthError> {
    let r: CertificatesResponse = http
        .post(CERTIFICATES_URL)
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let bad = |what: &str| AuthError::Minecraft(format!("certificates: bad {what}"));
    Ok(ChatKeys {
        private_key_pem: r.key_pair.private_key,
        public_key_der: pem_body(&r.key_pair.public_key).ok_or_else(|| bad("public key"))?,
        signature_v2: BASE64.decode(r.public_key_signature_v2.trim()).map_err(|_| bad("signature"))?,
        expires_at: parse_iso8601_millis(&r.expires_at).ok_or_else(|| bad("expiresAt"))?,
        refreshed_after: parse_iso8601_millis(&r.refreshed_after).ok_or_else(|| bad("refreshedAfter"))?,
    })
}

/// `Crypt.stringToRsaPublicKey`: strip the PEM armour and base64-decode.
fn pem_body(pem: &str) -> Option<Vec<u8>> {
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    BASE64.decode(body.trim()).ok()
}

/// Parses `2026-09-30T12:34:56.789012Z` (and offsets like `+00:00`) to Unix
/// milliseconds.
fn parse_iso8601_millis(s: &str) -> Option<i64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (year, month, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);

    let (clock, offset_secs) = if let Some(c) = time.strip_suffix('Z') {
        (c, 0)
    } else if let Some(i) = time.rfind(['+', '-']) {
        let (c, off) = time.split_at(i);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let (h, m) = off[1..].split_once(':')?;
        (c, sign * (h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60))
    } else {
        (time, 0)
    };
    let (hms, frac) = clock.split_once('.').unwrap_or((clock, "0"));
    let mut t = hms.split(':').map(|p| p.parse::<i64>());
    let (hour, minute, second) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    let millis: i64 = format!("{frac:0<3}")[..3].parse().ok()?;

    // Days from civil (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;

    Some((days * 86400 + hour * 3600 + minute * 60 + second - offset_secs) * 1000 + millis)
}

#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    #[error("HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("session server rejected the join (HTTP {status}): {body}")]
    Rejected { status: u16, body: String },
}

/// `YggdrasilMinecraftSessionService.joinServer`: tells Mojang this account
/// is joining the server identified by `server_hash`. The server then checks
/// with Mojang (`hasJoined`) after we send the encryption key.
pub async fn join_server(access_token: &str, profile_id: Uuid, server_hash: &str) -> Result<(), JoinError> {
    let response = reqwest::Client::new()
        .post(JOIN_URL)
        .json(&json!({
            "accessToken": access_token,
            "selectedProfile": profile_id.simple().to_string(),
            "serverId": server_hash,
        }))
        .send()
        .await?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status().as_u16();
        Err(JoinError::Rejected { status, body: response.text().await.unwrap_or_default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601() {
        assert_eq!(parse_iso8601_millis("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601_millis("2026-09-29T12:00:00.5Z"), Some(1_790_683_200_500));
        assert_eq!(parse_iso8601_millis("2026-09-29T14:00:00.123456+02:00"), Some(1_790_683_200_123));
    }

    #[test]
    fn pem() {
        let pem = "-----BEGIN RSA PUBLIC KEY-----\nAQID\nBA==\n-----END RSA PUBLIC KEY-----\n";
        assert_eq!(pem_body(pem), Some(vec![1, 2, 3, 4]));
    }
}
