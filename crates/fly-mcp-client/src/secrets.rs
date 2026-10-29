//! Connector secrets in the OS keychain.
//!
//! The access token (with its non-secret metadata) and the refresh token are
//! separate entries, and a value longer than [`CHUNK_CHARS`] is split across
//! numbered entries: Windows Credential Manager caps one entry at about
//! 1,280 characters. Secret values are never logged.

use std::sync::Arc;

use async_trait::async_trait;
use fly_secrets::SecretStore;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use serde_json::Value;

use crate::registry::{secret_key, SecretSlot, MAX_SECRET_CHUNKS};

/// Longest value stored in one keychain entry, in chars.
pub const CHUNK_CHARS: usize = 1000;
const CHUNK_HEADER: &str = "fotw-chunks:";

#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    #[error("the value is too long for the keychain")]
    TooLong,
    #[error("a keychain entry is missing or damaged")]
    Damaged,
    #[error(transparent)]
    Store(#[from] fly_secrets::SecretError),
}

pub fn put_chunked(store: &dyn SecretStore, base: &str, value: &str) -> Result<(), ChunkError> {
    delete_chunked(store, base)?;
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= CHUNK_CHARS && !value.starts_with(CHUNK_HEADER) {
        store.set(base, value)?;
        return Ok(());
    }
    let chunks: Vec<String> = chars
        .chunks(CHUNK_CHARS)
        .map(|c| c.iter().collect())
        .collect();
    if chunks.len() > MAX_SECRET_CHUNKS {
        return Err(ChunkError::TooLong);
    }
    for (i, c) in chunks.iter().enumerate() {
        store.set(&format!("{base}.{i}"), c)?;
    }
    store.set(base, &format!("{CHUNK_HEADER}{}", chunks.len()))?;
    Ok(())
}

pub fn get_chunked(store: &dyn SecretStore, base: &str) -> Result<Option<String>, ChunkError> {
    let Some(head) = store.get(base)? else {
        return Ok(None);
    };
    let Some(n) = head.strip_prefix(CHUNK_HEADER) else {
        return Ok(Some(head));
    };
    let n: usize = n.parse().map_err(|_| ChunkError::Damaged)?;
    if n == 0 || n > MAX_SECRET_CHUNKS {
        return Err(ChunkError::Damaged);
    }
    let mut out = String::new();
    for i in 0..n {
        out.push_str(
            &store
                .get(&format!("{base}.{i}"))?
                .ok_or(ChunkError::Damaged)?,
        );
    }
    Ok(Some(out))
}

pub fn delete_chunked(store: &dyn SecretStore, base: &str) -> Result<(), ChunkError> {
    store.delete(base)?;
    for i in 0..MAX_SECRET_CHUNKS {
        store.delete(&format!("{base}.{i}"))?;
    }
    Ok(())
}

/// rmcp's credential store, backed by the keychain for one connector.
pub struct KeychainCredentials {
    store: Arc<dyn SecretStore>,
    server_id: String,
}

impl KeychainCredentials {
    pub fn new(store: Arc<dyn SecretStore>, server_id: impl Into<String>) -> Self {
        Self {
            store,
            server_id: server_id.into(),
        }
    }

    fn key(&self, slot: SecretSlot) -> String {
        secret_key(&self.server_id, slot)
    }

    pub fn has_tokens(&self) -> bool {
        matches!(
            get_chunked(self.store.as_ref(), &self.key(SecretSlot::Access)),
            Ok(Some(_))
        )
    }

    pub fn client_secret(&self) -> Option<String> {
        get_chunked(self.store.as_ref(), &self.key(SecretSlot::Client))
            .ok()
            .flatten()
    }

    pub fn set_client_secret(&self, secret: Option<&str>) -> Result<(), ChunkError> {
        match secret {
            Some(s) if !s.is_empty() => {
                put_chunked(self.store.as_ref(), &self.key(SecretSlot::Client), s)
            }
            _ => delete_chunked(self.store.as_ref(), &self.key(SecretSlot::Client)),
        }
    }

    /// Remove tokens (sign out). The client secret stays.
    pub fn clear_tokens(&self) -> Result<(), ChunkError> {
        delete_chunked(self.store.as_ref(), &self.key(SecretSlot::Access))?;
        delete_chunked(self.store.as_ref(), &self.key(SecretSlot::Refresh))
    }
}

fn store_err(e: impl std::fmt::Display) -> AuthError {
    AuthError::CredentialStoreError(e.to_string())
}

#[async_trait]
impl CredentialStore for KeychainCredentials {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let Some(access) =
            get_chunked(self.store.as_ref(), &self.key(SecretSlot::Access)).map_err(store_err)?
        else {
            return Ok(None);
        };
        let mut v: Value = serde_json::from_str(&access).map_err(store_err)?;
        if let Some(refresh) =
            get_chunked(self.store.as_ref(), &self.key(SecretSlot::Refresh)).map_err(store_err)?
        {
            if let Some(tr) = v.get_mut("token_response").and_then(Value::as_object_mut) {
                tr.insert("refresh_token".into(), Value::String(refresh));
            }
        }
        serde_json::from_value(v).map(Some).map_err(store_err)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let mut v = serde_json::to_value(&credentials).map_err(store_err)?;
        let refresh = v
            .get_mut("token_response")
            .and_then(Value::as_object_mut)
            .and_then(|tr| tr.remove("refresh_token"))
            .and_then(|r| r.as_str().map(str::to_string));
        let store = self.store.as_ref();
        match refresh {
            Some(r) => put_chunked(store, &self.key(SecretSlot::Refresh), &r).map_err(store_err)?,
            None => delete_chunked(store, &self.key(SecretSlot::Refresh)).map_err(store_err)?,
        }
        put_chunked(store, &self.key(SecretSlot::Access), &v.to_string()).map_err(store_err)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.clear_tokens().map_err(store_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_secrets::MemorySecretStore;

    #[test]
    fn short_values_use_one_entry_long_ones_chunk() {
        let s = MemorySecretStore::default();
        put_chunked(&s, "k", "short").unwrap();
        assert_eq!(s.get("k").unwrap().as_deref(), Some("short"));
        let long = "é".repeat(CHUNK_CHARS * 3 + 7);
        put_chunked(&s, "k", &long).unwrap();
        assert!(s.get("k").unwrap().unwrap().starts_with(CHUNK_HEADER));
        for i in 0..4 {
            let part = s.get(&format!("k.{i}")).unwrap().unwrap();
            assert!(part.chars().count() <= CHUNK_CHARS);
        }
        assert_eq!(get_chunked(&s, "k").unwrap().unwrap(), long);
        // Overwriting with a short value removes stale chunks.
        put_chunked(&s, "k", "again").unwrap();
        assert!(s.get("k.0").unwrap().is_none());
        assert!(put_chunked(&s, "k", &"x".repeat(CHUNK_CHARS * (MAX_SECRET_CHUNKS + 1))).is_err());
    }

    #[test]
    fn a_missing_chunk_is_damage_not_a_partial_token() {
        let s = MemorySecretStore::default();
        put_chunked(&s, "k", &"x".repeat(CHUNK_CHARS * 2)).unwrap();
        s.delete("k.1").unwrap();
        assert!(matches!(get_chunked(&s, "k"), Err(ChunkError::Damaged)));
    }

    #[tokio::test]
    async fn access_and_refresh_tokens_are_stored_apart() {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
        let creds = KeychainCredentials::new(store.clone(), "c1");
        let stored: StoredCredentials = serde_json::from_value(serde_json::json!({
            "client_id": "fotw-client",
            "token_response": {
                "access_token": "A".repeat(1500),
                "token_type": "bearer",
                "expires_in": 3600,
                "refresh_token": "R-secret"
            },
            "granted_scopes": ["read"],
            "token_received_at": 1700000000u64
        }))
        .unwrap();
        creds.save(stored).await.unwrap();
        let access = get_chunked(store.as_ref(), "connector.c1.access")
            .unwrap()
            .unwrap();
        assert!(
            !access.contains("R-secret"),
            "refresh token kept out of the access entry"
        );
        assert_eq!(
            store.get("connector.c1.refresh").unwrap().as_deref(),
            Some("R-secret")
        );
        // The 1,500-char access token was chunked.
        assert!(store.get("connector.c1.access.1").unwrap().is_some());

        let loaded = creds.load().await.unwrap().unwrap();
        let v = serde_json::to_value(&loaded).unwrap();
        assert_eq!(v["token_response"]["refresh_token"], "R-secret");
        assert_eq!(v["client_id"], "fotw-client");
        assert!(creds.has_tokens());

        creds.clear().await.unwrap();
        assert!(creds.load().await.unwrap().is_none());
        assert!(store.get("connector.c1.refresh").unwrap().is_none());
    }
}
