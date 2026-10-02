//! On-disk token cache. It holds live credentials: keep it private.

use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AuthError, ChatKeys};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenCache {
    pub msa: Option<CachedMsa>,
    pub minecraft: Option<CachedMinecraft>,
    pub chat_keys: Option<ChatKeys>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedMsa {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedMinecraft {
    pub access_token: String,
    /// Unix seconds.
    pub expires_at: i64,
    pub name: String,
    pub id: Uuid,
}

impl TokenCache {
    pub fn load(path: &Path) -> Result<Self, AuthError> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), AuthError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}
