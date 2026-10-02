//! Login cryptography (`net.minecraft.util.Crypt`).

use md5::{Digest as _, Md5};
use rand::RngCore;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use sha1::Sha1;
use uuid::Uuid;

/// `Crypt.generateSecretKey`: a random 128-bit AES key.
pub fn generate_secret() -> [u8; 16] {
    let mut secret = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut secret);
    secret
}

/// `Crypt.encryptUsingKey`: `Cipher.getInstance("RSA")`, which is
/// RSA/ECB/PKCS1Padding.
pub fn rsa_encrypt(public_key_der: &[u8], data: &[u8]) -> Result<Vec<u8>, rsa::Error> {
    let key = RsaPublicKey::from_public_key_der(public_key_der)
        .map_err(|_| rsa::Error::InvalidArguments)?;
    key.encrypt(&mut rand::thread_rng(), Pkcs1v15Encrypt, data)
}

/// The session server hash: `new BigInteger(Crypt.digestData(..)).toString(16)`.
/// Java's BigInteger makes this signed, so a digest with the top bit set
/// becomes `-` followed by the two's complement magnitude.
pub fn server_hash(server_id: &str, secret: &[u8], public_key_der: &[u8]) -> String {
    let mut sha = Sha1::new();
    // digestData encodes the server ID as ISO-8859-1.
    sha.update(server_id.chars().map(|c| c as u8).collect::<Vec<u8>>());
    sha.update(secret);
    sha.update(public_key_der);
    java_hex_digest(sha.finalize().into())
}

fn java_hex_digest(mut digest: [u8; 20]) -> String {
    let negative = digest[0] & 0x80 != 0;
    if negative {
        // Two's complement negation.
        let mut carry = true;
        for b in digest.iter_mut().rev() {
            *b = !*b;
            if carry {
                let (v, overflow) = b.overflowing_add(1);
                *b = v;
                carry = overflow;
            }
        }
    }
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let trimmed = hex.trim_start_matches('0');
    let trimmed = if trimmed.is_empty() { "0" } else { trimmed };
    if negative { format!("-{trimmed}") } else { trimmed.to_owned() }
}

/// `UUIDUtil.createOfflinePlayerUUID`: `UUID.nameUUIDFromBytes` of
/// `"OfflinePlayer:" + name`, a version 3 UUID.
pub fn offline_uuid(name: &str) -> Uuid {
    let mut hash: [u8; 16] = Md5::digest(format!("OfflinePlayer:{name}").as_bytes()).into();
    hash[6] = (hash[6] & 0x0f) | 0x30;
    hash[8] = (hash[8] & 0x3f) | 0x80;
    Uuid::from_bytes(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha1_hex(name: &str) -> String {
        java_hex_digest(Sha1::digest(name.as_bytes()).into())
    }

    #[test]
    fn java_style_digests() {
        // Well-known examples of Minecraft's signed SHA-1 hex.
        assert_eq!(sha1_hex("Notch"), "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48");
        assert_eq!(sha1_hex("jeb_"), "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1");
        assert_eq!(sha1_hex("simon"), "88e16a1019277b15d58faf0541e11910eb756f6");
    }

    #[test]
    fn offline_uuids() {
        // Matches what offline-mode servers assign.
        assert_eq!(offline_uuid("Notch").to_string(), "b50ad385-829d-3141-a216-7e7d7539ba7f");
    }
}
