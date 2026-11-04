//! Mojang's chat profile-key set (`getPublicKeys`).
//!
//! A player's chat session carries a key pair that Mojang signs. Verifying
//! that signature is what stops a server from announcing a key of its own
//! choosing for a player, so it is the root of trust for signed chat. The
//! signing keys come from the services `publickeys` endpoint
//! (`ServicesKeyType.PROFILE_KEY`, the `playerCertificateKeys` list).

use base64::Engine as _;
use rsa::RsaPublicKey;
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::pkcs8::DecodePublicKey;
use serde::Deserialize;
use sha1::{Digest as _, Sha1};
use uuid::Uuid;

use crate::AuthError;

/// `MinecraftServicesKeyInfo.KeySetResponse`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeySetResponse {
    #[serde(default)]
    player_certificate_keys: Vec<KeyData>,
}

/// `MinecraftServicesKeyInfo.KeyData`. Gson reads a `ByteBuffer` field from a
/// base64 string; the bytes are X.509 SubjectPublicKeyInfo DER.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyData {
    public_key: String,
}

/// The `PROFILE_KEY` half of Mojang's `ServicesKeySet`.
#[derive(Debug, Clone, Default)]
pub struct ServicesKeySet {
    profile_keys: Vec<RsaPublicKey>,
}

impl ServicesKeySet {
    /// Reads a `/publickeys` body. Keys that cannot be read are dropped, so a
    /// malformed entry leaves the set smaller rather than failing the fetch.
    pub fn from_json(body: &str) -> Result<Self, AuthError> {
        let response: KeySetResponse = serde_json::from_str(body)?;
        let keys: Vec<Vec<u8>> = response
            .player_certificate_keys
            .iter()
            .filter_map(|key| {
                base64::engine::general_purpose::STANDARD
                    .decode(&key.public_key)
                    .ok()
            })
            .collect();
        Ok(Self::from_der_keys(&keys))
    }

    /// From X.509 SubjectPublicKeyInfo DER, for callers that cache the keys.
    pub fn from_der_keys(keys: &[Vec<u8>]) -> Self {
        Self {
            profile_keys: keys
                .iter()
                .filter_map(|der| RsaPublicKey::from_public_key_der(der).ok())
                .collect(),
        }
    }

    /// Whether any key can validate. `SignatureValidator.from(keySet, type)`
    /// is null for an empty set, and vanilla then drops every announced chat
    /// session.
    pub fn is_empty(&self) -> bool {
        self.profile_keys.is_empty()
    }

    pub fn len(&self) -> usize {
        self.profile_keys.len()
    }

    /// `ProfilePublicKey.Data.validateSignature` with Mojang's `PROFILE_KEY`
    /// keys: `SHA1withRSA` (`MinecraftServicesKeyInfo.signature`) over the
    /// profile id, the expiry and the key itself, big endian
    /// (`signedPayload`). Any key may match.
    pub fn validate_profile_key(
        &self,
        profile_id: Uuid,
        expires_at: i64,
        key_der: &[u8],
        signature: &[u8],
    ) -> bool {
        let mut payload = Vec::with_capacity(24 + key_der.len());
        payload.extend_from_slice(profile_id.as_bytes());
        payload.extend_from_slice(&expires_at.to_be_bytes());
        payload.extend_from_slice(key_der);
        let digest = Sha1::digest(&payload);
        self.profile_keys.iter().any(|key| {
            key.verify(Pkcs1v15Sign::new::<Sha1>(), &digest, signature)
                .is_ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use rsa::pkcs1v15::SigningKey;
    use rsa::pkcs8::EncodePublicKey;
    use rsa::signature::{SignatureEncoding, Signer};

    use super::*;

    /// A service key pair and its X.509 DER, as `/publickeys` publishes it.
    fn service_key() -> (rsa::RsaPrivateKey, Vec<u8>) {
        let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 1024).unwrap();
        let der = key
            .to_public_key()
            .to_public_key_der()
            .unwrap()
            .as_bytes()
            .to_vec();
        (key, der)
    }

    fn payload(profile: Uuid, expires: i64, key_der: &[u8]) -> Vec<u8> {
        let mut payload = profile.as_bytes().to_vec();
        payload.extend_from_slice(&expires.to_be_bytes());
        payload.extend_from_slice(key_der);
        payload
    }

    #[test]
    fn a_profile_key_is_verified_against_the_services_keys() {
        let (service, der) = service_key();
        let keys = ServicesKeySet::from_der_keys(&[der]);
        assert_eq!(keys.len(), 1);
        assert!(!keys.is_empty());

        let profile = Uuid::from_u128(7);
        let expires = 1_700_000_000_000i64;
        let key_der = vec![1, 2, 3, 4];
        let signature = SigningKey::<Sha1>::new(service)
            .sign(&payload(profile, expires, &key_der))
            .to_vec();

        assert!(keys.validate_profile_key(profile, expires, &key_der, &signature));
        // A different profile, expiry or key is a different payload.
        assert!(!keys.validate_profile_key(Uuid::from_u128(8), expires, &key_der, &signature));
        assert!(!keys.validate_profile_key(profile, expires + 1, &key_der, &signature));
        assert!(!keys.validate_profile_key(profile, expires, &[9, 9], &signature));
        // A tampered signature fails, and an empty set validates nothing.
        let mut bad = signature.clone();
        bad[0] ^= 0xff;
        assert!(!keys.validate_profile_key(profile, expires, &key_der, &bad));
        assert!(
            !ServicesKeySet::default().validate_profile_key(profile, expires, &key_der, &signature)
        );
    }

    #[test]
    fn the_publickeys_body_is_read() {
        let (_, der) = service_key();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&der);
        let body = format!(
            r#"{{"profilePropertyKeys":[{{"publicKey":"AAAA"}}],"playerCertificateKeys":[{{"publicKey":"{encoded}"}}]}}"#
        );
        let keys = ServicesKeySet::from_json(&body).unwrap();
        assert_eq!(keys.len(), 1, "only the player certificate keys are used");

        // An empty or unreadable entry leaves the set empty rather than
        // failing, and malformed JSON is an error.
        let none =
            ServicesKeySet::from_json(r#"{"playerCertificateKeys":[{"publicKey":"!!"}]}"#).unwrap();
        assert!(none.is_empty());
        assert!(ServicesKeySet::from_json("not json").is_err());
        assert!(ServicesKeySet::from_json("{}").unwrap().is_empty());
    }
}
