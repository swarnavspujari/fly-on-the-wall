//! The connectors the user added, their approvals and tool permissions, and
//! the call log. Persisted by the app through a [`ConnectorStore`] (a
//! backend-only setting); the webview can't write it.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::config::{slug_for, ConfigError, ServerConfig, TransportConfig};
use crate::fence::SchemaCheck;
use crate::pin::{Permission, ToolKind, ToolPin};

pub const REGISTRY_VERSION: u32 = 2;
pub const MAX_SERVERS: usize = 16;
pub const MAX_LOG_ENTRIES: usize = 200;
/// Chunks a long secret may be split into (see `secrets`).
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

/// Why the last connection failed, as a code (never server text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectProblem {
    NeedsSignIn,
    /// A local program couldn't start or stopped right away.
    CouldNotStart,
    /// A local program wasn't found.
    ProgramNotFound,
    Unreachable,
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
    /// The connector's switch. Off: nothing is offered. Turning it on again
    /// is a trust grant (native dialog).
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub tools: Vec<ToolPin>,
    /// Tools have been listed since the approval; later new tools start
    /// Blocked.
    #[serde(default)]
    pub baseline_done: bool,
    #[serde(default)]
    pub last_sync: Option<i64>,
    #[serde(default)]
    pub last_problem: Option<ConnectProblem>,
}

fn yes() -> bool {
    true
}

impl ServerEntry {
    /// Approved, and the approval is for exactly this configuration.
    pub fn is_approved(&self) -> bool {
        self.approved_hash.as_deref() == Some(self.config.hash().as_str())
    }

    /// Approved and switched on.
    pub fn is_active(&self) -> bool {
        self.enabled && self.is_approved()
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
            enabled: true,
            tools: Vec::new(),
            baseline_done: false,
            last_sync: None,
            last_problem: None,
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

/// Keychain key names for one connector. Long values may be split into
/// chunks (Windows caps an entry at ~1,280 characters), so every chunk name
/// is listed.
pub fn secret_keys_for(entry: &ServerEntry) -> Vec<String> {
    let mut keys = Vec::new();
    for slot in [
        SecretSlot::Access,
        SecretSlot::Refresh,
        SecretSlot::Client,
        SecretSlot::ApiKey,
    ] {
        let base = secret_key(&entry.id, slot);
        keys.extend((0..MAX_SECRET_CHUNKS).map(|i| format!("{base}.{i}")));
        keys.push(base);
    }
    match &entry.config.transport {
        TransportConfig::Stdio { env, .. } => {
            keys.extend(env.iter().map(|e| env_secret_key(&entry.id, &e.name)))
        }
        TransportConfig::Http { headers, .. } => keys.extend(
            headers
                .iter()
                .map(|h| header_secret_key(&entry.id, &h.name)),
        ),
    }
    keys
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSlot {
    /// Access token plus non-secret token metadata.
    Access,
    /// Refresh token, stored on its own.
    Refresh,
    /// OAuth client secret.
    Client,
    /// API key or token sent as a bearer token.
    ApiKey,
}

pub fn secret_key(server_id: &str, slot: SecretSlot) -> String {
    let s = match slot {
        SecretSlot::Access => "access",
        SecretSlot::Refresh => "refresh",
        SecretSlot::Client => "client",
        SecretSlot::ApiKey => "apikey",
    };
    format!("connector.{server_id}.{s}")
}

pub fn env_secret_key(server_id: &str, name: &str) -> String {
    format!("connector.{server_id}.env.{name}")
}

pub fn header_secret_key(server_id: &str, name: &str) -> String {
    format!("connector.{server_id}.header.{}", name.to_ascii_lowercase())
}

/// One requested permission (from Settings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionChange {
    pub tool: String,
    pub permission: Permission,
}

/// Requested changes sorted into trust granted (loosen) and removed
/// (tighten).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionPlan {
    pub loosen: Vec<PermissionChange>,
    pub tighten: Vec<PermissionChange>,
}

impl PermissionPlan {
    pub fn grants_trust(&self) -> bool {
        !self.loosen.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.loosen.is_empty() && self.tighten.is_empty()
    }
}

/// Check and classify requested changes. A tool waiting for review, missing
/// or unusable can only be tightened.
pub fn plan_permissions(
    entry: &ServerEntry,
    changes: &[PermissionChange],
) -> Result<PermissionPlan, String> {
    let mut plan = PermissionPlan::default();
    for c in changes {
        let pin = entry
            .tool(&c.tool)
            .ok_or_else(|| format!("{} has no tool named {:?}.", entry.config.name, c.tool))?;
        if c.permission > pin.permission {
            if pin.needs_review() {
                return Err(format!("Review the change to {} first.", pin.name));
            }
            if pin.missing {
                return Err(format!("{} is no longer offered by the server.", pin.name));
            }
            if let Some(p) = &pin.definition.problem {
                return Err(format!("{} can't be used: {p}", pin.name));
            }
            plan.loosen.push(c.clone());
        } else if c.permission < pin.permission {
            plan.tighten.push(c.clone());
        }
    }
    Ok(plan)
}

/// Apply the tightening half of a plan (no dialog needed).
pub fn apply_tighten(entry: &mut ServerEntry, plan: &PermissionPlan) {
    for c in &plan.tighten {
        if let Some(pin) = entry.tool_mut(&c.tool) {
            pin.permission = pin.permission.min(c.permission);
        }
    }
}

/// Apply the loosening half of a plan (after the user's yes).
pub fn apply_loosen(entry: &mut ServerEntry, plan: &PermissionPlan) {
    for c in &plan.loosen {
        if let Some(pin) = entry.tool_mut(&c.tool) {
            pin.permission = c.permission;
        }
    }
}

/// Who let a call run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    /// The user clicked on the card (and the native confirm, for a change).
    User,
    /// The tool is on Always allow.
    Automatic,
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

/// Why FOTW refused a call before running it. Codes, never text.
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
    #[serde(default)]
    pub approval: Option<Approval>,
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
    use crate::config::{AuthConfig, EnvConfig, HeaderConfig};
    use crate::pin::{reconcile, LiveTool};
    use serde_json::json;

    fn cfg(name: &str) -> ServerConfig {
        ServerConfig {
            name: name.into(),
            transport: TransportConfig::Http {
                url: "https://mcp.example.com/mcp".into(),
                headers: vec![],
            },
            auth: AuthConfig::None,
        }
    }

    #[test]
    fn approval_is_bound_to_the_config_hash() {
        let mut r = Registry::default();
        let id = r.add_approved(cfg("Docs"), 1).unwrap();
        assert!(r.server(&id).unwrap().is_approved());
        assert!(r.server(&id).unwrap().is_active());
        r.server_mut(&id).unwrap().config.transport = TransportConfig::Http {
            url: "https://evil.example.net/mcp".into(),
            headers: vec![],
        };
        assert!(
            !r.server(&id).unwrap().is_approved(),
            "a changed config voids approval"
        );
        assert!(!r.server(&id).unwrap().is_active());
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
    fn secret_keys_cover_tokens_chunks_keys_headers_and_env() {
        let mut r = Registry::default();
        let local = r
            .add_approved(
                ServerConfig {
                    name: "Local".into(),
                    transport: TransportConfig::Stdio {
                        command: "srv".into(),
                        args: vec![],
                        env: vec![EnvConfig {
                            name: "TOKEN".into(),
                            value: None,
                        }],
                    },
                    auth: AuthConfig::None,
                },
                1,
            )
            .unwrap();
        let web = r
            .add_approved(
                ServerConfig {
                    name: "Web".into(),
                    transport: TransportConfig::Http {
                        url: "https://mcp.example.com/mcp".into(),
                        headers: vec![HeaderConfig {
                            name: "X-Team-Key".into(),
                            value: None,
                        }],
                    },
                    auth: AuthConfig::ApiKey,
                },
                1,
            )
            .unwrap();
        let keys = r.secret_keys();
        for k in [
            format!("connector.{local}.access"),
            format!("connector.{local}.refresh.7"),
            format!("connector.{local}.env.TOKEN"),
            format!("connector.{web}.apikey"),
            format!("connector.{web}.apikey.0"),
            format!("connector.{web}.header.x-team-key"),
        ] {
            assert!(keys.contains(&k), "{k}");
        }
    }

    fn entry_with_tools() -> ServerEntry {
        let mut r = Registry::default();
        let id = r.add_approved(cfg("CRM"), 1).unwrap();
        let mut e = r.server(&id).unwrap().clone();
        let find = LiveTool {
            name: "find".into(),
            title: None,
            description: Some("Find".into()),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            annotations: Some(json!({"readOnlyHint": true})),
        };
        let update = LiveTool {
            name: "update".into(),
            annotations: Some(json!({"destructiveHint": true})),
            ..find.clone()
        };
        reconcile(&mut e.tools, &[find, update], 1, true).unwrap();
        e
    }

    #[test]
    fn plans_separate_loosening_from_tightening() {
        let mut e = entry_with_tools();
        let plan = plan_permissions(
            &e,
            &[
                PermissionChange {
                    tool: "find".into(),
                    permission: Permission::AlwaysAllow,
                },
                PermissionChange {
                    tool: "update".into(),
                    permission: Permission::Blocked,
                },
            ],
        )
        .unwrap();
        assert!(plan.grants_trust());
        assert_eq!(plan.loosen[0].tool, "find");
        assert_eq!(plan.tighten[0].tool, "update");
        apply_tighten(&mut e, &plan);
        assert_eq!(e.tool("update").unwrap().permission, Permission::Blocked);
        assert_eq!(
            e.tool("find").unwrap().permission,
            Permission::NeedsApproval
        );
        apply_loosen(&mut e, &plan);
        assert_eq!(e.tool("find").unwrap().permission, Permission::AlwaysAllow);
    }

    #[test]
    fn tools_waiting_for_review_can_only_be_tightened() {
        let mut e = entry_with_tools();
        e.tool_mut("find").unwrap().permission = Permission::Blocked;
        e.tool_mut("find").unwrap().unreviewed = true;
        let err = plan_permissions(
            &e,
            &[PermissionChange {
                tool: "find".into(),
                permission: Permission::NeedsApproval,
            }],
        )
        .unwrap_err();
        assert!(err.contains("Review"), "{err}");
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
                    approval: Some(Approval::User),
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
