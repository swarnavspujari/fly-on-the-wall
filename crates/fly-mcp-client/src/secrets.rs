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

use crate::registry::{secret_key, secret_keys_for, SecretSlot, ServerEntry, MAX_SECRET_CHUNKS};

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
    put_chunked_max(store, base, value, MAX_SECRET_CHUNKS)
}

fn put_chunked_max(
    store: &dyn SecretStore,
    base: &str,
    value: &str,
    max_chunks: usize,
) -> Result<(), ChunkError> {
    delete_chunked_max(store, base, max_chunks)?;
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= CHUNK_CHARS && !value.starts_with(CHUNK_HEADER) {
        store.set(base, value)?;
        return Ok(());
    }
    let chunks: Vec<String> = chars
        .chunks(CHUNK_CHARS)
        .map(|c| c.iter().collect())
        .collect();
    if chunks.len() > max_chunks {
        return Err(ChunkError::TooLong);
    }
    for (i, c) in chunks.iter().enumerate() {
        store.set(&format!("{base}.{i}"), c)?;
    }
    store.set(base, &format!("{CHUNK_HEADER}{}", chunks.len()))?;
    Ok(())
}

pub fn get_chunked(store: &dyn SecretStore, base: &str) -> Result<Option<String>, ChunkError> {
    get_chunked_max(store, base, MAX_SECRET_CHUNKS)
}

fn get_chunked_max(
    store: &dyn SecretStore,
    base: &str,
    max_chunks: usize,
) -> Result<Option<String>, ChunkError> {
    let Some(head) = store.get(base)? else {
        return Ok(None);
    };
    let Some(n) = head.strip_prefix(CHUNK_HEADER) else {
        return Ok(Some(head));
    };
    let n: usize = n.parse().map_err(|_| ChunkError::Damaged)?;
    if n == 0 || n > max_chunks {
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
    delete_chunked_max(store, base, MAX_SECRET_CHUNKS)
}

fn delete_chunked_max(
    store: &dyn SecretStore,
    base: &str,
    max_chunks: usize,
) -> Result<(), ChunkError> {
    store.delete(base)?;
    for i in 0..max_chunks {
        store.delete(&format!("{base}.{i}"))?;
    }
    Ok(())
}

/// The keychain's own list of every connector key ever written, by connector
/// id. "Delete all my data" reads it as well as the registry, so keys can't
/// be orphaned by a registry that's gone or unreadable.
pub const INDEX_KEY: &str = "connector.index";
const INDEX_CHUNKS: usize = 32;

type Index = std::collections::BTreeMap<String, Vec<String>>;

fn index_load(store: &dyn SecretStore) -> Index {
    get_chunked_max(store, INDEX_KEY, INDEX_CHUNKS)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn index_save(store: &dyn SecretStore, index: &Index) -> Result<(), ChunkError> {
    if index.is_empty() {
        return delete_chunked_max(store, INDEX_KEY, INDEX_CHUNKS);
    }
    let json = serde_json::to_string(index).unwrap_or_default();
    put_chunked_max(store, INDEX_KEY, &json, INDEX_CHUNKS)
}

/// Record every key `entry` may own (merged with what was recorded before).
pub fn index_record(store: &dyn SecretStore, entry: &ServerEntry) -> Result<(), ChunkError> {
    let mut index = index_load(store);
    let keys = index.entry(entry.id.clone()).or_default();
    for k in secret_keys_for(entry) {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    index_save(store, &index)
}

pub fn index_forget(store: &dyn SecretStore, id: &str) -> Result<(), ChunkError> {
    let mut index = index_load(store);
    if index.remove(id).is_some() {
        index_save(store, &index)?;
    }
    Ok(())
}

/// Every key in the index, plus the index's own entries.
pub fn indexed_keys(store: &dyn SecretStore) -> Vec<String> {
    let mut keys: Vec<String> = index_load(store).into_values().flatten().collect();
    keys.push(INDEX_KEY.into());
    keys.extend((0..INDEX_CHUNKS).map(|i| format!("{INDEX_KEY}.{i}")));
    keys
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

    #[test]
    fn the_index_lists_keys_until_forgotten() {
        let s = MemorySecretStore::default();
        let mut reg = crate::registry::Registry::default();
        let id = reg
            .add_approved(
                crate::config::ServerConfig {
                    name: "Docs".into(),
                    transport: crate::config::TransportConfig::Stdio {
                        command: "srv".into(),
                        args: vec![],
                        env: vec![crate::config::EnvConfig {
                            name: "TOKEN".into(),
                            value: None,
                        }],
                    },
                    auth: crate::config::AuthConfig::None,
                },
                0,
            )
            .unwrap();
        index_record(&s, reg.server(&id).unwrap()).unwrap();
        let keys = indexed_keys(&s);
        assert!(keys.contains(&format!("connector.{id}.env.TOKEN")));
        assert!(keys.contains(&format!("connector.{id}.refresh")));
        assert!(keys.contains(&INDEX_KEY.to_string()));
        index_forget(&s, &id).unwrap();
        assert!(!indexed_keys(&s).iter().any(|k| k.contains(&id)));
        assert!(s.get(INDEX_KEY).unwrap().is_none());
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
