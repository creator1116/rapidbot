//! Where the token cache lives.
//!
//! A cache holds a refresh token, access tokens and the chat-signing private
//! key, so it is secret. [`EncryptedFileTokenStore`] keeps it in an
//! AES-256-GCM encrypted file and keeps only the random key in the OS
//! keyring; [`FileTokenStore`] is the old plain-JSON cache for tools that
//! pass an explicit path.

use std::io::Write;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::RngCore;

use crate::{AuthError, TokenCache};

/// The encrypted file format. Keep the version in the magic so a future
/// format can be rejected instead of misread.
const MAGIC: &[u8; 8] = b"RBTOKEN1";
const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;

/// A store for one account's [`TokenCache`].
pub trait TokenStore: Send + Sync {
    fn load(&self) -> Result<TokenCache, AuthError>;
    fn save(&self, cache: &TokenCache) -> Result<(), AuthError>;
    /// Removes the cache and, when separate, its encryption key.
    fn clear(&self) -> Result<(), AuthError>;
}

/// A plain JSON file. Existing callers pass the path themselves.
pub struct FileTokenStore {
    path: PathBuf,
}

impl FileTokenStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl TokenStore for FileTokenStore {
    fn load(&self) -> Result<TokenCache, AuthError> {
        TokenCache::load(&self.path)
    }

    fn save(&self, cache: &TokenCache) -> Result<(), AuthError> {
        cache.save(&self.path)
    }

    fn clear(&self) -> Result<(), AuthError> {
        remove_if_present(&self.path)
    }
}

/// The source of the encryption key. The OS keyring implementation stores a
/// random key; tests can use a fixed one.
pub trait KeyProvider: Send + Sync {
    /// Returns the key, creating and storing one if this is the first use.
    fn key(&self) -> Result<[u8; KEY_BYTES], AuthError>;
    /// Returns an existing key without creating one. Used when loading: a
    /// missing key must not silently be replaced, or the cache would become
    /// undecryptable.
    fn existing_key(&self) -> Result<Option<[u8; KEY_BYTES]>, AuthError>;
    /// Forgets the key, if it exists.
    fn clear(&self) -> Result<(), AuthError>;
}

/// A random key in the OS credential store under `service`/`account`.
pub struct KeyringKeyProvider {
    service: String,
    account: String,
}

impl KeyringKeyProvider {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, AuthError> {
        keyring::Entry::new(&self.service, &self.account)
            .map_err(|e| AuthError::Store(format!("OS keyring: {e}")))
    }
}

impl KeyProvider for KeyringKeyProvider {
    fn key(&self) -> Result<[u8; KEY_BYTES], AuthError> {
        if let Some(key) = self.existing_key()? {
            return Ok(key);
        }
        let entry = self.entry()?;
        let mut key = [0u8; KEY_BYTES];
        OsRng.fill_bytes(&mut key);
        entry
            .set_password(&BASE64.encode(key))
            .map_err(|e| AuthError::Store(format!("OS keyring: {e}")))?;
        Ok(key)
    }

    fn existing_key(&self) -> Result<Option<[u8; KEY_BYTES]>, AuthError> {
        match self.entry()?.get_password() {
            Ok(encoded) => decode_key(&encoded).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AuthError::Store(format!("OS keyring: {e}"))),
        }
    }

    fn clear(&self) -> Result<(), AuthError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AuthError::Store(format!("OS keyring: {e}"))),
        }
    }
}

fn decode_key(encoded: &str) -> Result<[u8; KEY_BYTES], AuthError> {
    let bytes = BASE64
        .decode(encoded.trim())
        .map_err(|_| AuthError::Store("OS keyring returned a malformed key".into()))?;
    bytes
        .try_into()
        .map_err(|_| AuthError::Store("OS keyring returned a malformed key".into()))
}

/// An encrypted token cache. The file contains the magic, nonce and
/// ciphertext; the key is kept by `keys` (in the OS keyring in normal use).
pub struct EncryptedFileTokenStore<K = KeyringKeyProvider> {
    path: PathBuf,
    keys: K,
}

impl<K> EncryptedFileTokenStore<K> {
    pub fn new(path: impl AsRef<Path>, keys: K) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            keys,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl<K: KeyProvider> TokenStore for EncryptedFileTokenStore<K> {
    fn load(&self) -> Result<TokenCache, AuthError> {
        let contents = match std::fs::read(&self.path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TokenCache::default());
            }
            Err(e) => return Err(e.into()),
        };
        if contents.len() < MAGIC.len() + NONCE_BYTES || &contents[..MAGIC.len()] != MAGIC {
            return Err(AuthError::Store(format!(
                "{} is not a rapidbot token cache",
                self.path.display()
            )));
        }
        let Some(key) = self.keys.existing_key()? else {
            return Err(AuthError::Store(format!(
                "no keyring key for {}",
                self.path.display()
            )));
        };
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|_| AuthError::Store("bad token-cache key".into()))?;
        let nonce_at = MAGIC.len();
        let nonce = Nonce::from_slice(&contents[nonce_at..nonce_at + NONCE_BYTES]);
        let plain = cipher
            .decrypt(nonce, &contents[nonce_at + NONCE_BYTES..])
            .map_err(|_| AuthError::Store("could not decrypt the token cache".into()))?;
        Ok(serde_json::from_slice(&plain)?)
    }

    fn save(&self, cache: &TokenCache) -> Result<(), AuthError> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let key = self.keys.key()?;
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|_| AuthError::Store("bad token-cache key".into()))?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let mut plain = serde_json::to_vec(cache)?;
        let sealed = cipher
            .encrypt(&nonce, plain.as_slice())
            .map_err(|_| AuthError::Store("could not encrypt the token cache".into()))?;
        plain.fill(0);

        let mut out = Vec::with_capacity(MAGIC.len() + NONCE_BYTES + sealed.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        write_private(&self.path, &out)
    }

    fn clear(&self) -> Result<(), AuthError> {
        remove_if_present(&self.path)?;
        self.keys.clear()
    }
}

pub(crate) fn write_private(path: &Path, contents: &[u8]) -> Result<(), AuthError> {
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), AuthError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use uuid::Uuid;

    struct FixedKey {
        key: [u8; KEY_BYTES],
        cleared: Arc<AtomicBool>,
    }

    impl KeyProvider for FixedKey {
        fn key(&self) -> Result<[u8; KEY_BYTES], AuthError> {
            Ok(self.key)
        }

        fn existing_key(&self) -> Result<Option<[u8; KEY_BYTES]>, AuthError> {
            Ok(Some(self.key))
        }

        fn clear(&self) -> Result<(), AuthError> {
            self.cleared.store(true, Ordering::Relaxed);
            Ok(())
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rapidbot-auth-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn encrypted_round_trip_and_tamper() {
        let dir = temp_dir();
        let path = dir.join("main.json");
        let cleared = Arc::new(AtomicBool::new(false));
        let store = EncryptedFileTokenStore::new(
            &path,
            FixedKey {
                key: [7; KEY_BYTES],
                cleared: cleared.clone(),
            },
        );
        let cache = TokenCache {
            msa: Some(crate::CachedMsa {
                access_token: "a".into(),
                refresh_token: "r".into(),
                expires_at: 1,
            }),
            ..Default::default()
        };
        store.save(&cache).unwrap();
        assert_eq!(store.load().unwrap().msa.unwrap().refresh_token, "r");
        let mut bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..MAGIC.len()], MAGIC);
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        assert!(store.load().is_err());
        store.clear().unwrap();
        assert!(!path.exists());
        assert!(cleared.load(Ordering::Relaxed));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_cache_is_empty() {
        let dir = temp_dir();
        let store = EncryptedFileTokenStore::new(
            dir.join("main.json"),
            FixedKey {
                key: [1; KEY_BYTES],
                cleared: Arc::new(AtomicBool::new(false)),
            },
        );
        assert!(store.load().unwrap().msa.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    struct NoKey;

    impl KeyProvider for NoKey {
        fn key(&self) -> Result<[u8; KEY_BYTES], AuthError> {
            panic!("load must not create a key")
        }

        fn existing_key(&self) -> Result<Option<[u8; KEY_BYTES]>, AuthError> {
            Ok(None)
        }

        fn clear(&self) -> Result<(), AuthError> {
            Ok(())
        }
    }

    #[test]
    fn missing_key_does_not_replace_the_cache() {
        let dir = temp_dir();
        let path = dir.join("main.json");
        let store = EncryptedFileTokenStore::new(
            &path,
            FixedKey {
                key: [3; KEY_BYTES],
                cleared: Arc::new(AtomicBool::new(false)),
            },
        );
        store.save(&TokenCache::default()).unwrap();
        let no_key = EncryptedFileTokenStore::new(&path, NoKey);
        assert!(no_key.load().is_err());
        assert!(path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
