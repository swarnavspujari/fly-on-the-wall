//! The connectors the user added, their approvals and tool pins, and the
//! call log. Persisted by the app through a [`ConnectorStore`] (a
//! backend-only setting); the webview can't write it.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::config::{slug_for, ConfigError, ServerConfig};
use crate::fence::SchemaCheck;
use crate::pin::{ToolKind, ToolPin};

pub const REGISTRY_VERSION: u32 = 1;
pub const MAX_SERVERS: usize = 16;
pub const MAX_LOG_ENTRIES: usize = 200;
/// Chunks a long token may be split into (see `secrets`).
pub const MAX_SECRET_CHUNKS: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION,
            servers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerEntry {
    /// Random, stable id ("c" + 12 hex digits). Keys keychain entries.
    pub id: String,
    /// Unique short name used in model-facing tool names.
    pub slug: String,
    pub config: ServerConfig,
    /// [`ServerConfig::hash`] of the configuration the user approved.
    pub approved_hash: Option<String>,
    pub approved_at: Option<i64>,
    pub added_at: i64,
    #[serde(default)]
    pub tools: Vec<ToolPin>,
    #[serde(default)]
    pub last_sync: Option<i64>,
}

impl ServerEntry {
    /// Approved, and the approval is for exactly this configuration.
    pub fn is_approved(&self) -> bool {
        self.approved_hash.as_deref() == Some(self.config.hash().as_str())
    }

    pub fn tool(&self, name: &str) -> Option<&ToolPin> {
        self.tools.iter().find(|t| t.name == name)
    }

    pub fn tool_mut(&mut self, name: &str) -> Option<&mut ToolPin> {
        self.tools.iter_mut().find(|t| t.name == name)
    }
}

impl Registry {
    /// Parse stored state. Anything unreadable yields an empty registry:
    /// connectors fail closed, they don't guess.
    pub fn from_json(s: &str) -> Self {
        match serde_json::from_str::<Registry>(s) {
            Ok(r) if r.version == REGISTRY_VERSION => r,
            Ok(r) => {
                tracing::warn!(
                    version = r.version,
                    "unknown connector registry version; ignoring"
                );
                Registry::default()
            }
            Err(e) => {
                tracing::warn!(error = %e, "connector registry unreadable; ignoring");
                Registry::default()
            }
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("registry serializes")
    }

    pub fn server(&self, id: &str) -> Option<&ServerEntry> {
        self.servers.iter().find(|s| s.id == id)
    }

    pub fn server_mut(&mut self, id: &str) -> Option<&mut ServerEntry> {
        self.servers.iter_mut().find(|s| s.id == id)
    }

    /// Add a configuration the user just approved. Validation is the
    /// caller's job (it knows the network policy).
    pub fn add_approved(&mut self, config: ServerConfig, now: i64) -> Result<String, ConfigError> {
        if self.servers.len() >= MAX_SERVERS {
            return Err(ConfigError(format!("At most {MAX_SERVERS} connectors.")));
        }
        if self
            .servers
            .iter()
            .any(|s| s.config.name.eq_ignore_ascii_case(&config.name))
        {
            return Err(ConfigError(
                "A connector with that name already exists.".into(),
            ));
        }
        let base = slug_for(&config.name);
        let mut slug = base.clone();
        let mut n = 2;
        while self.servers.iter().any(|s| s.slug == slug) {
            slug = format!("{base}{n}");
            n += 1;
        }
        let id = format!("c{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let hash = config.hash();
        self.servers.push(ServerEntry {
            id: id.clone(),
            slug,
            config,
            approved_hash: Some(hash),
            approved_at: Some(now),
            added_at: now,
            tools: Vec::new(),
            last_sync: None,
        });
        Ok(id)
    }

    pub fn remove(&mut self, id: &str) -> Option<ServerEntry> {
        let i = self.servers.iter().position(|s| s.id == id)?;
        Some(self.servers.remove(i))
    }

    /// Every keychain key any connector here may own (for removal and for
    /// "Delete all my data").
    pub fn secret_keys(&self) -> Vec<String> {
        self.servers.iter().flat_map(secret_keys_for).collect()
    }
}

/// Keychain key names for one connector. Tokens may be split into chunks
/// (Windows caps an entry at ~1,280 characters), so every chunk name is
/// listed.
pub fn secret_keys_for(entry: &ServerEntry) -> Vec<String> {
    let mut keys = Vec::new();
    for base in [
        secret_key(&entry.id, SecretSlot::Access),
        secret_key(&entry.id, SecretSlot::Refresh),
        secret_key(&entry.id, SecretSlot::Client),
    ] {
        keys.push(base.clone());
        keys.extend((0..MAX_SECRET_CHUNKS).map(|i| format!("{base}.{i}")));
    }
    if let crate::config::TransportConfig::Stdio { env_names, .. } = &entry.config.transport {
        keys.extend(env_names.iter().map(|n| env_secret_key(&entry.id, n)));
    }
    keys
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSlot {
    /// Access token plus non-secret token metadata.
    Access,
    /// Refresh token, stored on its own.
    Refresh,
    /// Client secret from registration or the user.
    Client,
}

pub fn secret_key(server_id: &str, slot: SecretSlot) -> String {
    let s = match slot {
        SecretSlot::Access => "access",
        SecretSlot::Refresh => "refresh",
        SecretSlot::Client => "client",
    };
    format!("connector.{server_id}.{s}")
}

pub fn env_secret_key(server_id: &str, name: &str) -> String {
    format!("connector.{server_id}.env.{name}")
}

/// One requested tool permission (from the Settings form).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolPermission {
    pub tool: String,
    pub enabled: bool,
    pub read_only: bool,
}

/// A permission change sorted into trust it grants and trust it removes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionPlan {
    /// Tools being turned on.
    pub enable: Vec<String>,
    /// Tools being marked read-only (calls are labelled reads).
    pub mark_read_only: Vec<String>,
    pub disable: Vec<String>,
    pub unmark_read_only: Vec<String>,
}

impl PermissionPlan {
    pub fn grants_trust(&self) -> bool {
        !self.enable.is_empty() || !self.mark_read_only.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        !self.grants_trust() && self.disable.is_empty() && self.unmark_read_only.is_empty()
    }
}

/// Check and classify requested changes. Refuses enabling a tool that's
/// changed, missing or unusable, and marking read-only a tool the server
/// says writes.
pub fn plan_permissions(
    entry: &ServerEntry,
    changes: &[ToolPermission],
) -> Result<PermissionPlan, String> {
    let mut plan = PermissionPlan::default();
    for c in changes {
        let pin = entry
            .tool(&c.tool)
            .ok_or_else(|| format!("{} has no tool named {:?}.", entry.config.name, c.tool))?;
        if c.enabled && !pin.enabled {
            if pin.pending.is_some() {
                return Err(format!("{} changed; review the change first.", pin.name));
            }
            if pin.missing {
                return Err(format!("{} is no longer offered by the server.", pin.name));
            }
            if let Some(p) = &pin.definition.problem {
                return Err(format!("{} can't be used: {p}", pin.name));
            }
            plan.enable.push(pin.name.clone());
        } else if !c.enabled && pin.enabled {
            plan.disable.push(pin.name.clone());
        }
        if c.read_only && !pin.read_only {
            if !pin.can_mark_read_only() {
                return Err(format!(
                    "The server says {} can change data, so it can't be marked read-only.",
                    pin.name
                ));
            }
            if pin.pending.is_some() {
                return Err(format!("{} changed; review the change first.", pin.name));
            }
            plan.mark_read_only.push(pin.name.clone());
        } else if !c.read_only && pin.read_only {
            plan.unmark_read_only.push(pin.name.clone());
        }
    }
    Ok(plan)
}

/// Apply a plan made by [`plan_permissions`] for the same entry.
pub fn apply_permissions(entry: &mut ServerEntry, plan: &PermissionPlan) {
    for pin in entry.tools.iter_mut() {
        if plan.enable.contains(&pin.name) {
            pin.enabled = true;
        }
        if plan.disable.contains(&pin.name) {
            pin.enabled = false;
        }
        if plan.mark_read_only.contains(&pin.name) {
            pin.read_only = true;
        }
        if plan.unmark_read_only.contains(&pin.name) {
            pin.read_only = false;
        }
    }
}

/// How a proposed call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcome {
    Ran,
    /// The server returned an error result.
    ServerError,
    Declined,
    Refused,
    /// Couldn't connect or the call failed in transport.
    Failed,
}

/// Why FOTW refused a call before asking the user. Codes, never text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    NotOffered,
    KeepLocal,
    AfterResult,
    Budget,
    InvalidArguments,
    ToolChanged,
    NotApproved,
    TimedOut,
}

/// One call-log row: allowlisted fields only — never arguments, results or
/// meeting text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallLogEntry {
    pub at: i64,
    pub server_id: String,
    pub server: String,
    pub tool: String,
    pub kind: ToolKind,
    pub outcome: CallOutcome,
    pub refusal: Option<RefusalCode>,
    pub duration_ms: u64,
    pub result_chars: usize,
    pub truncated: bool,
    pub schema: SchemaCheck,
    pub args_edited: bool,
}

/// Where connector state lives. The app backs it with backend-only
/// settings; tests use [`MemoryConnectorStore`].
pub trait ConnectorStore: Send + Sync {
    fn load(&self) -> Registry;
    fn save(&self, registry: &Registry) -> Result<(), String>;
    fn load_log(&self) -> Vec<CallLogEntry>;
    fn save_log(&self, log: &[CallLogEntry]) -> Result<(), String>;
}

#[derive(Default)]
pub struct MemoryConnectorStore {
    registry: Mutex<Registry>,
    log: Mutex<Vec<CallLogEntry>>,
}

impl MemoryConnectorStore {
    pub fn new(registry: Registry) -> Self {
        Self {
            registry: Mutex::new(registry),
            log: Mutex::new(Vec::new()),
        }
    }
}

impl ConnectorStore for MemoryConnectorStore {
    fn load(&self) -> Registry {
        self.registry.lock().unwrap().clone()
    }
    fn save(&self, registry: &Registry) -> Result<(), String> {
        *self.registry.lock().unwrap() = registry.clone();
        Ok(())
    }
    fn load_log(&self) -> Vec<CallLogEntry> {
        self.log.lock().unwrap().clone()
    }
    fn save_log(&self, log: &[CallLogEntry]) -> Result<(), String> {
        *self.log.lock().unwrap() = log.to_vec();
        Ok(())
    }
}

/// Append to the log, keeping the newest [`MAX_LOG_ENTRIES`].
pub fn append_log(store: &dyn ConnectorStore, entry: CallLogEntry) {
    let mut log = store.load_log();
    log.push(entry);
    let excess = log.len().saturating_sub(MAX_LOG_ENTRIES);
    log.drain(..excess);
    if let Err(e) = store.save_log(&log) {
        tracing::warn!(error = %e, "couldn't save the connector call log");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthConfig, TransportConfig};
    use crate::pin::{reconcile, LiveTool};
    use serde_json::json;

    fn cfg(name: &str) -> ServerConfig {
        ServerConfig {
            name: name.into(),
            transport: TransportConfig::Http {
                url: "https://mcp.example.com/mcp".into(),
            },
            auth: AuthConfig::None,
        }
    }

    #[test]
    fn approval_is_bound_to_the_config_hash() {
        let mut r = Registry::default();
        let id = r.add_approved(cfg("Docs"), 1).unwrap();
        assert!(r.server(&id).unwrap().is_approved());
        r.server_mut(&id).unwrap().config.transport = TransportConfig::Http {
            url: "https://evil.example.net/mcp".into(),
        };
        assert!(
            !r.server(&id).unwrap().is_approved(),
            "a changed config voids approval"
        );
    }

    #[test]
    fn names_and_slugs_are_unique() {
        let mut r = Registry::default();
        r.add_approved(cfg("Docs"), 1).unwrap();
        assert!(r.add_approved(cfg("docs"), 1).is_err());
        let id = r.add_approved(cfg("Docs!"), 1).unwrap();
        assert_eq!(r.server(&id).unwrap().slug, "docs2");
    }

    #[test]
    fn unreadable_state_fails_closed() {
        assert_eq!(Registry::from_json("not json"), Registry::default());
        assert_eq!(
            Registry::from_json(r#"{"version": 99, "servers": []}"#),
            Registry::default()
        );
        let mut r = Registry::default();
        r.add_approved(cfg("Docs"), 1).unwrap();
        assert_eq!(Registry::from_json(&r.to_json()), r);
    }

    #[test]
    fn secret_keys_cover_tokens_chunks_and_env() {
        let mut r = Registry::default();
        let id = r
            .add_approved(
                ServerConfig {
                    name: "Local".into(),
                    transport: TransportConfig::Stdio {
                        command: "srv".into(),
                        args: vec![],
                        env_names: vec!["TOKEN".into()],
                    },
                    auth: AuthConfig::None,
                },
                1,
            )
            .unwrap();
        let keys = r.secret_keys();
        assert!(keys.contains(&format!("connector.{id}.access")));
        assert!(keys.contains(&format!("connector.{id}.refresh.7")));
        assert!(keys.contains(&format!("connector.{id}.client")));
        assert!(keys.contains(&format!("connector.{id}.env.TOKEN")));
    }

    fn entry_with_tools() -> ServerEntry {
        let mut r = Registry::default();
        let id = r.add_approved(cfg("CRM"), 1).unwrap();
        let mut e = r.server(&id).unwrap().clone();
        let mut writes = LiveTool {
            name: "update".into(),
            title: None,
            description: Some("Update".into()),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            annotations: Some(json!({"destructiveHint": true})),
        };
        let reads = LiveTool {
            name: "find".into(),
            annotations: None,
            ..writes.clone()
        };
        writes.name = "update".into();
        reconcile(&mut e.tools, &[reads, writes], 1).unwrap();
        e
    }

    #[test]
    fn permission_plans_separate_grants_from_removals() {
        let mut e = entry_with_tools();
        let plan = plan_permissions(
            &e,
            &[ToolPermission {
                tool: "find".into(),
                enabled: true,
                read_only: true,
            }],
        )
        .unwrap();
        assert!(plan.grants_trust());
        assert_eq!(plan.enable, vec!["find"]);
        assert_eq!(plan.mark_read_only, vec!["find"]);
        apply_permissions(&mut e, &plan);
        let plan = plan_permissions(
            &e,
            &[ToolPermission {
                tool: "find".into(),
                enabled: false,
                read_only: true,
            }],
        )
        .unwrap();
        assert!(!plan.grants_trust());
        assert_eq!(plan.disable, vec!["find"]);
    }

    #[test]
    fn destructive_tools_cannot_be_marked_read_only() {
        let e = entry_with_tools();
        let err = plan_permissions(
            &e,
            &[ToolPermission {
                tool: "update".into(),
                enabled: true,
                read_only: true,
            }],
        )
        .unwrap_err();
        assert!(err.contains("can't be marked read-only"), "{err}");
    }

    #[test]
    fn the_log_keeps_the_newest_entries() {
        let store = MemoryConnectorStore::default();
        for i in 0..(MAX_LOG_ENTRIES + 5) {
            append_log(
                &store,
                CallLogEntry {
                    at: i as i64,
                    server_id: "c1".into(),
                    server: "CRM".into(),
                    tool: "find".into(),
                    kind: ToolKind::Read,
                    outcome: CallOutcome::Ran,
                    refusal: None,
                    duration_ms: 1,
                    result_chars: 1,
                    truncated: false,
                    schema: SchemaCheck::None,
                    args_edited: false,
                },
            );
        }
        let log = store.load_log();
        assert_eq!(log.len(), MAX_LOG_ENTRIES);
        assert_eq!(log[0].at, 5);
    }
}
