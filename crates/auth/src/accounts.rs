//! A small account directory: one alias per Microsoft account, with the
//! secret token cache kept by [`EncryptedFileTokenStore`] and only the
//! display metadata in `accounts.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use uuid::Uuid;

use crate::store::{EncryptedFileTokenStore, KeyringKeyProvider, TokenStore, write_private};
use crate::{AuthError, Authenticator, MinecraftAccount};

const KEYRING_SERVICE: &str = "rapidbot.minecraft";
const INDEX: &str = "accounts.json";

/// Cached token state for one account, without exposing the tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStatus {
    pub alias: String,
    /// Unix seconds; `None` when no Minecraft token is cached.
    pub minecraft_expires_at: Option<i64>,
    pub has_refresh_token: bool,
    /// Unix milliseconds.
    pub chat_keys_expire_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAccount {
    pub alias: String,
    pub name: String,
    pub id: Uuid,
    /// Unix seconds.
    pub last_login: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    accounts: BTreeMap<String, StoredAccount>,
}

/// A directory of accounts. The aliases are stable names chosen by the owner
/// (`main`, `alt`, ...); the keyring entry is scoped to this directory and
/// alias, while the cache file is keyed by alias alone.
pub struct AccountStore {
    root: PathBuf,
}

impl AccountStore {
    /// Opens (or creates) an account directory.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, AuthError> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        // The keyring entry is tied to the directory, so two account
        // directories with the same alias do not share a key.
        let root = std::fs::canonicalize(root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn keyring_account(&self, alias: &str) -> String {
        let mut hash = Sha1::new();
        hash.update(self.root.as_os_str().to_string_lossy().as_bytes());
        hash.update([0u8]);
        hash.update(alias.as_bytes());
        format!("{alias}:{:x}", hash.finalize())
    }

    fn index_path(&self) -> PathBuf {
        self.root.join(INDEX)
    }

    fn cache_path(&self, alias: &str) -> PathBuf {
        // A prefix keeps aliases such as `accounts` from colliding with the
        // index file.
        self.root.join(format!("account-{alias}.json"))
    }

    fn read_index(&self) -> Result<Index, AuthError> {
        match std::fs::read(self.index_path()) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Index::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn write_index(&self, index: &Index) -> Result<(), AuthError> {
        let bytes = serde_json::to_vec_pretty(index)?;
        write_private(&self.index_path(), &bytes)
    }

    /// An authenticator for `alias`, using the OS keyring for the cache key.
    pub fn authenticator(&self, alias: &str) -> Result<Authenticator, AuthError> {
        validate_alias(alias)?;
        let keys = KeyringKeyProvider::new(KEYRING_SERVICE, self.keyring_account(alias));
        let store = EncryptedFileTokenStore::new(self.cache_path(alias), keys);
        Authenticator::with_store(Box::new(store))
    }

    /// The account directory, sorted by alias. Metadata only; reading tokens
    /// requires [`status`](Self::status).
    pub fn list(&self) -> Result<Vec<StoredAccount>, AuthError> {
        Ok(self.read_index()?.accounts.into_values().collect())
    }

    /// Records a successful login under `alias`.
    pub fn remember(&self, alias: &str, account: &MinecraftAccount) -> Result<(), AuthError> {
        validate_alias(alias)?;
        let mut index = self.read_index()?;
        index.accounts.insert(
            alias.to_owned(),
            StoredAccount {
                alias: alias.to_owned(),
                name: account.name.clone(),
                id: account.id,
                last_login: crate::now_secs(),
            },
        );
        self.write_index(&index)
    }

    /// Reads the cached token metadata for an alias. This touches the OS
    /// keyring, so it can fail on a headless machine even though [`list`]
    /// works.
    pub fn status(&self, alias: &str) -> Result<AccountStatus, AuthError> {
        validate_alias(alias)?;
        let keys = KeyringKeyProvider::new(KEYRING_SERVICE, self.keyring_account(alias));
        let store = EncryptedFileTokenStore::new(self.cache_path(alias), keys);
        let cache = store.load()?;
        Ok(AccountStatus {
            alias: alias.to_owned(),
            minecraft_expires_at: cache.minecraft.as_ref().map(|mc| mc.expires_at),
            has_refresh_token: cache.msa.is_some(),
            chat_keys_expire_at: cache.chat_keys.as_ref().map(|keys| keys.expires_at),
        })
    }

    /// Removes the account's index entry and encrypted cache. The keyring key
    /// is deleted too; if the keyring is unavailable, the file and entry are
    /// still removed and the key is left orphaned.
    pub fn remove(&self, alias: &str) -> Result<(), AuthError> {
        validate_alias(alias)?;
        let keys = KeyringKeyProvider::new(KEYRING_SERVICE, self.keyring_account(alias));
        let store = EncryptedFileTokenStore::new(self.cache_path(alias), keys);
        let key_error = store.clear().err();
        let mut index = self.read_index()?;
        index.accounts.remove(alias);
        self.write_index(&index)?;
        match key_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

fn validate_alias(alias: &str) -> Result<(), AuthError> {
    let ok = !alias.is_empty()
        && alias.len() <= 64
        && alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(AuthError::Store(format!(
            "invalid account alias {alias:?}: use 1-64 letters, digits, '.', '_' or '-'"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChatKeys;

    fn account(name: &str, id: u128) -> MinecraftAccount {
        MinecraftAccount {
            name: name.into(),
            id: Uuid::from_u128(id),
            access_token: "secret".into(),
            chat_keys: Some(ChatKeys {
                private_key_pem: "key".into(),
                public_key_der: vec![],
                signature_v2: vec![],
                expires_at: 1,
                refreshed_after: 2,
            }),
        }
    }

    #[test]
    fn index_round_trip() {
        let dir = std::env::temp_dir().join(format!("rapidbot-accounts-{}", Uuid::new_v4()));
        let store = AccountStore::open(&dir).unwrap();
        store.remember("main", &account("Steve", 1)).unwrap();
        store.remember("alt", &account("Alex", 2)).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].alias, "alt");
        assert_eq!(list[1].name, "Steve");

        // Replacing an alias keeps one entry.
        store.remember("main", &account("Steve2", 3)).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].id, Uuid::from_u128(3));

        assert!(store.authenticator("../escape").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keyring_entries_are_per_directory_and_alias() {
        let base = std::env::temp_dir().join(format!("rapidbot-accounts-{}", Uuid::new_v4()));
        let first = AccountStore::open(base.join("one")).unwrap();
        let second = AccountStore::open(base.join("two")).unwrap();
        assert_ne!(first.keyring_account("main"), first.keyring_account("alt"));
        assert_ne!(
            first.keyring_account("main"),
            second.keyring_account("main")
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn index_is_private_json() {
        let dir = std::env::temp_dir().join(format!("rapidbot-accounts-{}", Uuid::new_v4()));
        let store = AccountStore::open(&dir).unwrap();
        store.remember("main", &account("Steve", 1)).unwrap();
        let bytes = std::fs::read(store.index_path()).unwrap();
        assert!(bytes.starts_with(b"{"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(store.index_path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
