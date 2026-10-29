//! The connector hub (one per app) and the per-turn tool set handed to the
//! Ask harness.
//!
//! A [`ConnectorTurn`] enforces the call-time rules, in this order:
//! 1. only tools offered this turn can be called: tools of active
//!    connectors that aren't Blocked or waiting for review. Nothing is
//!    offered on a small local model or when the turn's scope includes
//!    Keep-local notes, and [`ExternalTools::mark_keep_local`] refuses
//!    everything after;
//! 2. once any connector result or error has been returned, every later
//!    proposal in the turn is refused ([`ExternalTools::sealed`]);
//! 3. proposals are capped per turn;
//! 4. arguments must be an object without hidden characters, within a size
//!    cap, matching the pinned input schema;
//! 5. the tool is re-listed live: if its definition changed or it vanished,
//!    it's blocked and the call refused (the user reviews the diff);
//! 6. Needs approval: the user sees the exact, editable arguments on a card
//!    and must allow the call ([`CallApprover`]; the app adds a native
//!    confirm for change tools); edited arguments are checked again. Always
//!    allow (set only by the user in a native dialog) skips the card;
//! 7. the result is fenced, link-stripped, schema-checked and size-capped
//!    ([`crate::fence`]); errors after contact with the server are fenced
//!    the same way.
//!
//! Every routed proposal lands in the call log, without arguments or results.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fly_llm::conversation::ToolSpec;
use fly_llm::external_tools::ExternalTools;
use fly_secrets::SecretStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{AuthConfig, ConfigError, ServerConfig, TransportConfig};
use crate::fence::{self, SchemaCheck, DEFAULT_MAX_RESULT_CHARS};
use crate::netguard::NetPolicy;
use crate::oauth;
use crate::pin::{self, kind, model_tool_name, LiveTool, Permission, ReconcileReport, ToolKind};
use crate::registry::{
    append_log, apply_loosen, apply_tighten, env_secret_key, header_secret_key, plan_permissions,
    secret_key, secret_keys_for, Approval, CallLogEntry, CallOutcome, ConnectProblem,
    ConnectorStore, PermissionChange, PermissionPlan, RefusalCode, Registry, SecretSlot,
    ServerEntry,
};
use crate::sanitize::is_hidden_char;
use crate::schema;
use crate::secrets::{delete_chunked, get_chunked, put_chunked, KeychainCredentials};
use crate::session::{self, ConnectOptions, Connection, SessionError, CALL_TIMEOUT};

/// Most connector tools offered in one turn (context budget).
pub const MAX_OFFERED_TOOLS: usize = 24;
/// Longest argument object accepted, as pretty JSON, in chars.
pub const MAX_ARGS_CHARS: usize = 16_000;
/// A cached connection unused this long is closed before reuse.
const IDLE_CLOSE: Duration = Duration::from_secs(5 * 60);

/// What the user is asked to approve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallProposal {
    pub proposal_id: String,
    pub turn_id: String,
    pub server_id: String,
    pub server_name: String,
    /// Host the call goes to, or "this computer".
    pub destination: String,
    pub tool: String,
    pub tool_title: Option<String>,
    pub kind: ToolKind,
    /// The server says the tool is destructive.
    pub destructive: bool,
    /// The exact arguments the model proposed.
    pub args: Value,
    /// The pinned input schema, for editing.
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Run with these (possibly edited) arguments.
    Run {
        args: Value,
    },
    Decline,
    TimedOut,
}

/// Shows the confirm card and waits for the user. Implementations must be
/// cancel-safe: dropping the future withdraws the card.
#[async_trait]
pub trait CallApprover: Send + Sync {
    async fn decide(&self, proposal: CallProposal) -> Decision;
}

/// Secret values entered in a form, on their way to the keychain. Empty
/// strings mean "keep what's stored". No `Debug`: values must never be
/// logged.
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSecrets {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Secret header (web) or variable (local) values, by name.
    #[serde(default)]
    pub values: HashMap<String, String>,
}

/// Opens connections. The app uses [`LiveConnector`]; tests plug in a
/// fixture server.
#[async_trait]
pub trait Connector: Send + Sync {
    async fn connect(&self, entry: &ServerEntry) -> Result<Connection, SessionError>;
}

/// Connects for real: local programs with their variables, web servers
/// with their headers and a fresh OAuth token or API key.
pub struct LiveConnector {
    pub secrets: Arc<dyn SecretStore>,
    pub policy: NetPolicy,
}

/// Resolve a stored connector's secret values into connection options.
pub async fn resolve_options(
    entry: &ServerEntry,
    secrets: &Arc<dyn SecretStore>,
    policy: NetPolicy,
) -> Result<ConnectOptions, SessionError> {
    let mut opts = ConnectOptions::default();
    match &entry.config.transport {
        TransportConfig::Stdio { env, .. } => {
            for e in env {
                let v = match &e.value {
                    Some(v) => Some(v.clone()),
                    None => secrets
                        .get(&env_secret_key(&entry.id, &e.name))
                        .ok()
                        .flatten(),
                };
                if let Some(v) = v {
                    opts.env.push((e.name.clone(), v));
                }
            }
        }
        TransportConfig::Http { headers, .. } => {
            for h in headers {
                let v = match &h.value {
                    Some(v) => Some(v.clone()),
                    None => secrets
                        .get(&header_secret_key(&entry.id, &h.name))
                        .ok()
                        .flatten(),
                };
                if let Some(v) = v {
                    opts.headers.push((h.name.clone(), v));
                }
            }
        }
    }
    match &entry.config.auth {
        AuthConfig::OAuth { .. } => {
            opts.bearer = Some(
                oauth::access_token(entry, secrets.clone(), policy)
                    .await
                    .map_err(|e| match e {
                        oauth::OAuthError::SignInRequired => SessionError::AuthRequired,
                        other => SessionError::Connect(other.to_string()),
                    })?,
            );
        }
        AuthConfig::ApiKey => {
            let key = get_chunked(secrets.as_ref(), &secret_key(&entry.id, SecretSlot::ApiKey))
                .ok()
                .flatten()
                .ok_or(SessionError::AuthRequired)?;
            opts.bearer = Some(key);
        }
        AuthConfig::None => {}
    }
    Ok(opts)
}

#[async_trait]
impl Connector for LiveConnector {
    async fn connect(&self, entry: &ServerEntry) -> Result<Connection, SessionError> {
        let opts = resolve_options(entry, &self.secrets, self.policy).await?;
        session::connect(&entry.config, self.policy, opts).await
    }
}

/// Called with a server id when its stored state changed outside a Settings
/// command (for example a tool blocked mid-turn), so the UI can refresh.
pub type ChangeListener = Arc<dyn Fn(&str) + Send + Sync>;

struct Cached {
    conn: Arc<Connection>,
    last_used: Instant,
}

pub struct ConnectorHub {
    store: Arc<dyn ConnectorStore>,
    secrets: Arc<dyn SecretStore>,
    policy: NetPolicy,
    connector: Arc<dyn Connector>,
    connections: tokio::sync::Mutex<HashMap<String, Cached>>,
    /// Serializes load-modify-save of the registry.
    write_lock: Mutex<()>,
    on_change: Option<ChangeListener>,
}

#[derive(Debug, thiserror::Error)]
pub enum HubError {
    #[error("no connector with that id")]
    NotFound,
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("{0}")]
    Session(#[from] SessionError),
    #[error("{0}")]
    OAuth(#[from] oauth::OAuthError),
    #[error("{0}")]
    Invalid(String),
    #[error("couldn't save connector settings: {0}")]
    Store(String),
}

/// What a quick look at a web connector found, before the user approves it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    /// The server wants a browser sign-in before it lists tools.
    pub needs_sign_in: bool,
    pub tools: Vec<ToolPreview>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolPreview {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub kind: ToolKind,
    pub labelled: bool,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn problem_for(e: &SessionError) -> ConnectProblem {
    match e {
        SessionError::AuthRequired => ConnectProblem::NeedsSignIn,
        SessionError::ProgramNotFound(_) => ConnectProblem::ProgramNotFound,
        SessionError::Spawn(_) | SessionError::Exited(_) => ConnectProblem::CouldNotStart,
        _ => ConnectProblem::Unreachable,
    }
}

impl ConnectorHub {
    pub fn new(
        store: Arc<dyn ConnectorStore>,
        secrets: Arc<dyn SecretStore>,
        policy: NetPolicy,
        connector: Arc<dyn Connector>,
    ) -> Self {
        Self {
            store,
            secrets,
            policy,
            connector,
            connections: tokio::sync::Mutex::new(HashMap::new()),
            write_lock: Mutex::new(()),
            on_change: None,
        }
    }

    /// The hub the app uses: real connections, strict network policy.
    pub fn live(store: Arc<dyn ConnectorStore>, secrets: Arc<dyn SecretStore>) -> Self {
        let policy = NetPolicy::strict();
        let connector = Arc::new(LiveConnector {
            secrets: secrets.clone(),
            policy,
        });
        Self::new(store, secrets, policy, connector)
    }

    pub fn with_change_listener(mut self, f: ChangeListener) -> Self {
        self.on_change = Some(f);
        self
    }

    pub fn policy(&self) -> NetPolicy {
        self.policy
    }

    pub fn secrets(&self) -> Arc<dyn SecretStore> {
        self.secrets.clone()
    }

    pub fn registry(&self) -> Registry {
        self.store.load()
    }

    pub fn call_log(&self) -> Vec<CallLogEntry> {
        self.store.load_log()
    }

    pub fn clear_call_log(&self) -> Result<(), HubError> {
        self.store.save_log(&[]).map_err(HubError::Store)
    }

    /// Load, change and save the registry under one lock.
    fn update<T>(
        &self,
        f: impl FnOnce(&mut Registry) -> Result<T, HubError>,
    ) -> Result<T, HubError> {
        let _guard = self.write_lock.lock().unwrap();
        let mut reg = self.store.load();
        let out = f(&mut reg)?;
        self.store.save(&reg).map_err(HubError::Store)?;
        Ok(out)
    }

    /// Check a configuration (normalized) before asking the user to approve it.
    pub fn check_config(&self, config: ServerConfig) -> Result<ServerConfig, HubError> {
        let config = config.normalized();
        config.validate(&self.policy)?;
        Ok(config)
    }

    /// Every secret the configuration names must come with a value when it
    /// is first stored.
    fn check_secrets(
        config: &ServerConfig,
        secrets: &NewSecrets,
        existing: bool,
    ) -> Result<(), HubError> {
        if existing {
            return Ok(());
        }
        for name in config.secret_names() {
            if secrets.values.get(&name).is_none_or(|v| v.is_empty()) {
                return Err(HubError::Invalid(format!("Enter a value for {name}.")));
            }
        }
        if matches!(config.auth, AuthConfig::ApiKey)
            && secrets.api_key.as_deref().is_none_or(str::is_empty)
        {
            return Err(HubError::Invalid("Enter the API key or token.".into()));
        }
        Ok(())
    }

    /// Look at a web connector before it's approved: list its tools, or find
    /// out it wants a browser sign-in. Sends nothing but the MCP handshake
    /// and a tool listing. Local programs are never started before approval.
    pub async fn probe(
        &self,
        config: ServerConfig,
        secrets: &NewSecrets,
    ) -> Result<ProbeResult, HubError> {
        let config = self.check_config(config)?;
        let TransportConfig::Http { headers, .. } = &config.transport else {
            return Err(HubError::Invalid(
                "A local program runs only after you approve it.".into(),
            ));
        };
        if config.uses_oauth() {
            return Ok(ProbeResult {
                needs_sign_in: true,
                tools: Vec::new(),
            });
        }
        let mut opts = ConnectOptions::default();
        for h in headers {
            let v = h
                .value
                .clone()
                .or_else(|| secrets.values.get(&h.name).cloned());
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                opts.headers.push((h.name.clone(), v));
            }
        }
        if matches!(config.auth, AuthConfig::ApiKey) {
            opts.bearer = secrets.api_key.clone().filter(|k| !k.is_empty());
        }
        let conn = match session::connect(&config, self.policy, opts).await {
            Ok(c) => c,
            Err(SessionError::AuthRequired) => {
                return Ok(ProbeResult {
                    needs_sign_in: true,
                    tools: Vec::new(),
                })
            }
            Err(e) => return Err(e.into()),
        };
        let live = conn.list_tools().await?;
        conn.close().await;
        let mut pins = Vec::new();
        pin::reconcile(&mut pins, &live, now(), true)
            .map_err(|e| HubError::Invalid(e.to_string()))?;
        Ok(ProbeResult {
            needs_sign_in: false,
            tools: pins
                .iter()
                .map(|p| ToolPreview {
                    name: p.name.clone(),
                    title: p.definition.title.clone(),
                    description: p.definition.description.clone(),
                    kind: kind(p),
                    labelled: p.labelled(),
                })
                .collect(),
        })
    }

    /// Store a configuration the user approved in a native dialog. The
    /// approval is bound to the configuration's hash.
    pub fn add_approved(
        &self,
        config: ServerConfig,
        secrets: &NewSecrets,
    ) -> Result<String, HubError> {
        let config = self.check_config(config)?;
        Self::check_secrets(&config, secrets, false)?;
        let id = self.update(|reg| Ok(reg.add_approved(config, now())?))?;
        if let Err(e) = self.store_secrets(&id, secrets) {
            let _ = self.update(|reg| Ok(reg.remove(&id)));
            return Err(e);
        }
        Ok(id)
    }

    /// Whether saving `config` would point connector `id` somewhere new
    /// (see [`ServerConfig::moves_from`]); the dialog says what that clears.
    pub fn edit_moves(&self, id: &str, config: &ServerConfig) -> Result<bool, HubError> {
        let reg = self.registry();
        let old = reg.server(id).ok_or(HubError::NotFound)?;
        Ok(config.clone().normalized().moves_from(&old.config))
    }

    /// Replace a connector's configuration after the user approved the new
    /// one in a native dialog. A change of name, headers or settings keeps
    /// tool permissions and stored secrets. A move to another address,
    /// program, transport or kind of sign-in is treated like a new connector:
    /// every stored secret and token is deleted (fresh values from the form
    /// are stored), and its tools start over on Needs approval at the next
    /// listing.
    pub async fn update_approved(
        &self,
        id: &str,
        config: ServerConfig,
        secrets: &NewSecrets,
    ) -> Result<(), HubError> {
        let config = self.check_config(config)?;
        let old = self
            .registry()
            .server(id)
            .cloned()
            .ok_or(HubError::NotFound)?;
        if self
            .registry()
            .servers
            .iter()
            .any(|s| s.id != id && s.config.name.eq_ignore_ascii_case(&config.name))
        {
            return Err(HubError::Invalid(
                "A connector with that name already exists.".into(),
            ));
        }
        let moved = config.moves_from(&old.config);
        if moved {
            // Nothing stored carries over, so every secret needs a value now.
            Self::check_secrets(&config, secrets, false)?;
        } else {
            let old_names = old.config.secret_names();
            for name in config.secret_names() {
                if !old_names.contains(&name)
                    && secrets.values.get(&name).is_none_or(|v| v.is_empty())
                {
                    return Err(HubError::Invalid(format!("Enter a value for {name}.")));
                }
            }
        }
        self.update(|reg| {
            let e = reg.server_mut(id).ok_or(HubError::NotFound)?;
            e.approved_hash = Some(config.hash());
            e.approved_at = Some(now());
            e.config = config.clone();
            e.last_problem = None;
            if moved {
                e.tools.clear();
                e.baseline_done = false;
                e.last_sync = None;
            }
            Ok(())
        })?;
        // Delete what no longer applies: everything after a move, otherwise
        // the secrets the new configuration no longer names.
        let kept = if moved {
            Vec::new()
        } else {
            self.registry()
                .server(id)
                .map(secret_keys_for)
                .unwrap_or_default()
        };
        for key in secret_keys_for(&old) {
            if !kept.contains(&key) {
                let _ = self.secrets.delete(&key);
            }
        }
        self.store_secrets(id, secrets)?;
        self.drop_connection(id).await;
        Ok(())
    }

    /// Write the non-empty secret values the form sent.
    fn store_secrets(&self, id: &str, secrets: &NewSecrets) -> Result<(), HubError> {
        let reg = self.registry();
        let entry = reg.server(id).ok_or(HubError::NotFound)?;
        let store = self.secrets.as_ref();
        let err = |e: crate::secrets::ChunkError| HubError::Store(e.to_string());
        if let Some(k) = secrets.api_key.as_deref().filter(|k| !k.is_empty()) {
            if !matches!(entry.config.auth, AuthConfig::ApiKey) {
                return Err(HubError::Invalid(
                    "This connector doesn't use an API key.".into(),
                ));
            }
            if k.len() > 8 * 1024 || k.contains(['\0', '\r', '\n']) {
                return Err(HubError::Invalid("That API key isn't valid.".into()));
            }
            put_chunked(store, &secret_key(id, SecretSlot::ApiKey), k.trim()).map_err(err)?;
        }
        if let Some(s) = secrets.client_secret.as_deref().filter(|s| !s.is_empty()) {
            if !matches!(
                &entry.config.auth,
                AuthConfig::OAuth {
                    client_id: Some(_),
                    ..
                }
            ) {
                return Err(HubError::Invalid(
                    "A client secret needs a client ID.".into(),
                ));
            }
            KeychainCredentials::new(self.secrets.clone(), id)
                .set_client_secret(Some(s.trim()))
                .map_err(err)?;
        }
        let names = entry.config.secret_names();
        for (name, value) in &secrets.values {
            if value.is_empty() {
                continue;
            }
            if !names.contains(name) {
                return Err(HubError::Invalid(format!(
                    "{name} isn't a secret of this connector."
                )));
            }
            if value.chars().count() > crate::config::MAX_VALUE_CHARS
                || value.contains(['\0', '\r', '\n'])
            {
                return Err(HubError::Invalid(format!(
                    "The value for {name} isn't valid."
                )));
            }
            let key = if entry.config.is_local() {
                env_secret_key(id, name)
            } else {
                header_secret_key(id, name)
            };
            self.secrets
                .set(&key, value)
                .map_err(|e| HubError::Store(e.to_string()))?;
        }
        if !matches!(entry.config.auth, AuthConfig::ApiKey) {
            let _ = delete_chunked(store, &secret_key(id, SecretSlot::ApiKey));
        }
        Ok(())
    }

    /// Which secrets have a stored value (never the values).
    pub fn secret_status(&self, entry: &ServerEntry) -> SecretStatus {
        let store = self.secrets.as_ref();
        let names = entry
            .config
            .secret_names()
            .into_iter()
            .map(|n| {
                let key = if entry.config.is_local() {
                    env_secret_key(&entry.id, &n)
                } else {
                    header_secret_key(&entry.id, &n)
                };
                let set = matches!(store.get(&key), Ok(Some(_)));
                (n, set)
            })
            .collect();
        SecretStatus {
            api_key_set: matches!(
                get_chunked(store, &secret_key(&entry.id, SecretSlot::ApiKey)),
                Ok(Some(_))
            ),
            client_secret_set: KeychainCredentials::new(self.secrets.clone(), entry.id.clone())
                .client_secret()
                .is_some(),
            values: names,
        }
    }

    /// Switch a connector off (at once) or on (after the caller's native
    /// dialog).
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), HubError> {
        self.update(|reg| {
            reg.server_mut(id).ok_or(HubError::NotFound)?.enabled = enabled;
            Ok(())
        })?;
        if !enabled {
            self.drop_connection(id).await;
        }
        Ok(())
    }

    /// Remove a connector, its keychain entries and its connection.
    pub async fn remove(&self, id: &str) -> Result<(), HubError> {
        let removed = self.update(|reg| reg.remove(id).ok_or(HubError::NotFound))?;
        for key in secret_keys_for(&removed) {
            if let Err(e) = self.secrets.delete(&key) {
                tracing::warn!(key, error = %e, "couldn't delete a connector keychain entry");
            }
        }
        self.drop_connection(id).await;
        Ok(())
    }

    /// Connect, list tools and update the pins. Tools in the first listing
    /// after approval start on Needs approval; later new tools are blocked
    /// for review.
    pub async fn sync_tools(&self, id: &str) -> Result<ReconcileReport, HubError> {
        let entry = self
            .registry()
            .server(id)
            .cloned()
            .ok_or(HubError::NotFound)?;
        if !entry.is_approved() {
            return Err(HubError::Invalid("This connector isn't approved.".into()));
        }
        let conn = self.connection(&entry).await?;
        let live = match conn.list_tools().await {
            Ok(l) => l,
            Err(e) => {
                self.drop_connection(id).await;
                return Err(e.into());
            }
        };
        self.reconcile(id, &live)
    }

    fn reconcile(&self, id: &str, live: &[LiveTool]) -> Result<ReconcileReport, HubError> {
        self.update(|reg| {
            let entry = reg.server_mut(id).ok_or(HubError::NotFound)?;
            let report = pin::reconcile(&mut entry.tools, live, now(), !entry.baseline_done)
                .map_err(|e| HubError::Invalid(e.to_string()))?;
            entry.baseline_done = true;
            entry.last_sync = Some(now());
            entry.last_problem = None;
            Ok(report)
        })
    }

    /// Sort requested permission changes into loosening (needs the user's
    /// yes in a native dialog) and tightening (applies at once).
    pub fn plan_permissions(
        &self,
        id: &str,
        changes: &[PermissionChange],
    ) -> Result<PermissionPlan, HubError> {
        let reg = self.registry();
        let entry = reg.server(id).ok_or(HubError::NotFound)?;
        plan_permissions(entry, changes).map_err(HubError::Invalid)
    }

    /// Apply the tightening part of a request.
    pub fn apply_tightening(&self, id: &str, changes: &[PermissionChange]) -> Result<(), HubError> {
        self.update(|reg| {
            let entry = reg.server_mut(id).ok_or(HubError::NotFound)?;
            let plan = plan_permissions(entry, changes).map_err(HubError::Invalid)?;
            apply_tighten(entry, &plan);
            Ok(())
        })
    }

    /// Apply the loosening the user approved. Re-planned against current
    /// state, so it can't grant more than the dialog showed.
    pub fn apply_loosening(
        &self,
        id: &str,
        changes: &[PermissionChange],
        approved: &[PermissionChange],
    ) -> Result<(), HubError> {
        self.update(|reg| {
            let entry = reg.server_mut(id).ok_or(HubError::NotFound)?;
            let plan = plan_permissions(entry, changes).map_err(HubError::Invalid)?;
            if plan.loosen != approved {
                return Err(HubError::Invalid(
                    "The connector changed while you were deciding; review it again.".into(),
                ));
            }
            apply_loosen(entry, &plan);
            Ok(())
        })
    }

    /// Approve a tool's pending change or a new tool (after the user's yes).
    /// `hash` is the definition the user reviewed. The tool goes to Needs
    /// approval.
    pub fn accept_change(&self, id: &str, tool: &str, hash: &str) -> Result<(), HubError> {
        self.update(|reg| {
            let entry = reg.server_mut(id).ok_or(HubError::NotFound)?;
            let pin = entry
                .tool_mut(tool)
                .ok_or_else(|| HubError::Invalid("No such tool.".into()))?;
            if pin::accept_change(pin, hash) {
                Ok(())
            } else {
                Err(HubError::Invalid(
                    "The tool changed again; review the latest change.".into(),
                ))
            }
        })
    }

    pub fn signed_in(&self, entry: &ServerEntry) -> bool {
        KeychainCredentials::new(self.secrets.clone(), entry.id.clone()).has_tokens()
    }

    pub async fn sign_in(
        &self,
        id: &str,
        opener: &dyn oauth::BrowserOpener,
    ) -> Result<(), HubError> {
        let entry = self
            .registry()
            .server(id)
            .cloned()
            .ok_or(HubError::NotFound)?;
        if !entry.is_approved() {
            return Err(HubError::Invalid("This connector isn't approved.".into()));
        }
        self.drop_connection(id).await;
        oauth::sign_in(
            &entry,
            self.secrets.clone(),
            self.policy,
            opener,
            oauth::SIGN_IN_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    pub async fn sign_out(&self, id: &str) -> Result<(), HubError> {
        let entry = self
            .registry()
            .server(id)
            .cloned()
            .ok_or(HubError::NotFound)?;
        self.drop_connection(id).await;
        KeychainCredentials::new(self.secrets.clone(), entry.id)
            .clear_tokens()
            .map_err(|e| HubError::Store(e.to_string()))
    }

    async fn connection(&self, entry: &ServerEntry) -> Result<Arc<Connection>, SessionError> {
        let mut conns = self.connections.lock().await;
        conns.retain(|_, c| c.last_used.elapsed() < IDLE_CLOSE);
        if let Some(c) = conns.get_mut(&entry.id) {
            c.last_used = Instant::now();
            return Ok(c.conn.clone());
        }
        match self.connector.connect(entry).await {
            Ok(conn) => {
                let conn = Arc::new(conn);
                conns.insert(
                    entry.id.clone(),
                    Cached {
                        conn: conn.clone(),
                        last_used: Instant::now(),
                    },
                );
                Ok(conn)
            }
            Err(e) => {
                let problem = problem_for(&e);
                let _ = self.update(|reg| {
                    if let Some(s) = reg.server_mut(&entry.id) {
                        s.last_problem = Some(problem);
                    }
                    Ok(())
                });
                Err(e)
            }
        }
    }

    async fn drop_connection(&self, id: &str) {
        self.connections.lock().await.remove(id);
    }

    /// Close every connection (app exit, delete-all).
    pub async fn close_all(&self) {
        self.connections.lock().await.clear();
    }

    /// The tool set for one Ask turn.
    pub fn turn(
        self: &Arc<Self>,
        opts: TurnOptions,
        approver: Arc<dyn CallApprover>,
    ) -> ConnectorTurn {
        ConnectorTurn::new(self.clone(), opts, approver)
    }

    fn notify(&self, id: &str) {
        if let Some(f) = &self.on_change {
            f(id);
        }
    }
}

/// Which of a connector's secrets have a stored value.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
    pub api_key_set: bool,
    pub client_secret_set: bool,
    pub values: Vec<(String, bool)>,
}

/// Why the arguments can't be shown or sent as they are.
pub fn args_problem(args: &Value) -> Option<String> {
    fn hidden(v: &Value, depth: usize) -> bool {
        if depth > 64 {
            return true;
        }
        match v {
            Value::String(s) => s.chars().any(is_hidden_char),
            Value::Array(a) => a.iter().any(|x| hidden(x, depth + 1)),
            Value::Object(m) => m
                .iter()
                .any(|(k, x)| k.chars().any(is_hidden_char) || hidden(x, depth + 1)),
            _ => false,
        }
    }
    if !args.is_object() {
        return Some("The arguments must be a JSON object.".into());
    }
    if hidden(args, 0) {
        return Some("The arguments contain hidden characters.".into());
    }
    let len = serde_json::to_string_pretty(args).map_or(usize::MAX, |s| s.chars().count());
    if len > MAX_ARGS_CHARS {
        return Some("The arguments are too long.".into());
    }
    None
}

/// Per-turn settings from the harness.
#[derive(Debug, Clone)]
pub struct TurnOptions {
    /// Routes confirm cards to the right Ask panel.
    pub turn_id: String,
    /// The model tier may call tools (tiers M and C). Small local models
    /// (tier S) get no connector tools.
    pub tools_allowed: bool,
    /// The turn's scope includes Keep-local content: offer nothing.
    pub keep_local_in_scope: bool,
    /// Most connector proposals this turn.
    pub max_calls: u32,
    pub max_result_chars: usize,
}

impl TurnOptions {
    /// Defaults per Ask tier: "S" no tools, "M" ≤ 3 calls, "C" ≤ 8 calls.
    pub fn for_tier(turn_id: impl Into<String>, tier: &str, keep_local_in_scope: bool) -> Self {
        let (tools_allowed, max_calls) = match tier {
            "M" | "m" => (true, 3),
            "C" | "c" => (true, 8),
            _ => (false, 0),
        };
        Self {
            turn_id: turn_id.into(),
            tools_allowed,
            keep_local_in_scope,
            max_calls,
            max_result_chars: DEFAULT_MAX_RESULT_CHARS,
        }
    }
}

#[derive(Debug, Clone)]
struct Route {
    server_id: String,
    server_name: String,
    destination: String,
    tool: String,
    title: Option<String>,
    input_schema: Value,
    output_schema: Option<Value>,
    kind: ToolKind,
    destructive: bool,
}

#[derive(Debug, Default)]
struct TurnState {
    proposals: u32,
    sealed: bool,
    keep_local: bool,
}

pub struct ConnectorTurn {
    hub: Arc<ConnectorHub>,
    approver: Arc<dyn CallApprover>,
    opts: TurnOptions,
    specs: Vec<ToolSpec>,
    routes: HashMap<String, Route>,
    state: Mutex<TurnState>,
}

const KEEP_LOCAL_MSG: &str = "Connectors are off for this question because it involves notes \
     the user keeps local. Answer without them.";
const AFTER_RESULT_MSG: &str = "A connector result can't lead to another connector call in the \
     same answer. Answer with what you have; the user can ask a follow-up.";
const BUDGET_MSG: &str = "The connector call budget for this answer is used up. Answer with what \
     you have.";
const DECLINED_MSG: &str = "The user chose not to run this call. Don't propose it again; answer \
     without it.";
const TIMED_OUT_MSG: &str = "The user didn't answer the confirm card in time, so the call didn't \
     run.";
const CHANGED_MSG: &str = "This tool changed on the server since the user approved it, so FOTW \
     blocked it until the user reviews the change in Settings. Answer without it.";
const BLOCKED_MSG: &str = "The user has blocked this tool. Answer without it.";

#[derive(Debug, Default)]
struct LogExtra {
    approval: Option<Approval>,
    duration_ms: u64,
    result_chars: usize,
    truncated: bool,
    schema: SchemaCheck,
    args_edited: bool,
}

impl ConnectorTurn {
    fn new(hub: Arc<ConnectorHub>, opts: TurnOptions, approver: Arc<dyn CallApprover>) -> Self {
        let mut specs = Vec::new();
        let mut routes = HashMap::new();
        if opts.tools_allowed && !opts.keep_local_in_scope && opts.max_calls > 0 {
            let reg = hub.registry();
            'outer: for entry in reg.servers.iter().filter(|e| e.is_active()) {
                for pin in entry.tools.iter().filter(|p| p.offerable()) {
                    if specs.len() >= MAX_OFFERED_TOOLS {
                        break 'outer;
                    }
                    let name = model_tool_name(&entry.slug, &pin.name);
                    if routes.contains_key(&name) {
                        continue;
                    }
                    let k = kind(pin);
                    let d = &pin.definition;
                    let label = match k {
                        ToolKind::Read => "looks things up",
                        ToolKind::Write => "can change data",
                    };
                    let what = d
                        .title
                        .as_deref()
                        .map(|t| format!("{t}. "))
                        .unwrap_or_default();
                    specs.push(ToolSpec {
                        name: name.clone(),
                        description: format!(
                            "[{} connector, {label}; the user may review each call] {what}{}",
                            entry.config.name, d.description
                        ),
                        input_schema: d.input_schema.clone(),
                        strict: false,
                    });
                    routes.insert(
                        name,
                        Route {
                            server_id: entry.id.clone(),
                            server_name: entry.config.name.clone(),
                            destination: entry.config.destination(),
                            tool: pin.name.clone(),
                            title: d.title.clone(),
                            input_schema: d.input_schema.clone(),
                            output_schema: d.output_schema.clone(),
                            kind: k,
                            destructive: d.annotations.destructive_hint == Some(true),
                        },
                    );
                }
            }
        }
        Self {
            hub,
            approver,
            opts,
            specs,
            routes,
            state: Mutex::new(TurnState::default()),
        }
    }

    pub fn turn_id(&self) -> &str {
        &self.opts.turn_id
    }

    fn log(
        &self,
        route: &Route,
        outcome: CallOutcome,
        refusal: Option<RefusalCode>,
        extra: LogExtra,
    ) {
        append_log(
            self.hub.store.as_ref(),
            CallLogEntry {
                at: now(),
                server_id: route.server_id.clone(),
                server: route.server_name.clone(),
                tool: route.tool.clone(),
                kind: route.kind,
                outcome,
                approval: extra.approval,
                refusal,
                duration_ms: extra.duration_ms,
                result_chars: extra.result_chars,
                truncated: extra.truncated,
                schema: extra.schema,
                args_edited: extra.args_edited,
            },
        );
    }

    fn refuse(
        &self,
        route: &Route,
        code: RefusalCode,
        msg: impl Into<String>,
    ) -> Result<String, String> {
        self.log(route, CallOutcome::Refused, Some(code), LogExtra::default());
        Err(msg.into())
    }

    /// An error after contact with the server: it counts as a connector
    /// result (the turn is sealed) and is fenced like one.
    fn server_error(&self, route: &Route, msg: String) -> String {
        self.state.lock().unwrap().sealed = true;
        fence::render(
            &route.server_name,
            &route.tool,
            &fence::ToolResult {
                blocks: vec![fence::ResultBlock::Text(msg)],
                structured: None,
                is_error: true,
            },
            None,
            self.opts.max_result_chars.min(500),
        )
        .text
    }

    /// The live definition still matches the pin; returns the tool's current
    /// permission (`None`: no longer offerable).
    async fn verify_live(
        &self,
        route: &Route,
        conn: &Connection,
    ) -> Result<Option<Permission>, SessionError> {
        let live = conn.list_tools().await?;
        let report = self
            .hub
            .reconcile(&route.server_id, &live)
            .map_err(|e| SessionError::Listing(e.to_string()))?;
        if !report.is_empty() {
            self.hub.notify(&route.server_id);
        }
        let reg = self.hub.registry();
        Ok(reg
            .server(&route.server_id)
            .filter(|e| e.is_active())
            .and_then(|e| e.tool(&route.tool))
            .filter(|p| p.offerable() && kind(p) == route.kind)
            .map(|p| p.permission))
    }
}

/// A transport error for the local log: cleaned and short.
fn short_err(e: &SessionError) -> String {
    crate::sanitize::clean_capped(&e.to_string(), 300)
}

fn schema_errors(errs: Vec<String>) -> String {
    format!(
        "The arguments don't match the tool's input schema: {}. Fix them and call again.",
        errs.join("; ")
    )
}

#[async_trait]
impl ExternalTools for ConnectorTurn {
    fn specs(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }

    fn owns(&self, name: &str) -> bool {
        self.routes.contains_key(name)
    }

    fn mark_keep_local(&self) {
        self.state.lock().unwrap().keep_local = true;
    }

    fn sealed(&self) -> bool {
        self.state.lock().unwrap().sealed
    }

    async fn call(&self, name: &str, args: Value) -> Result<String, String> {
        let Some(route) = self.routes.get(name).cloned() else {
            return Err("That connector tool isn't available in this answer.".into());
        };
        {
            let mut st = self.state.lock().unwrap();
            if st.keep_local || self.opts.keep_local_in_scope {
                drop(st);
                return self.refuse(&route, RefusalCode::KeepLocal, KEEP_LOCAL_MSG);
            }
            if st.sealed {
                drop(st);
                return self.refuse(&route, RefusalCode::AfterResult, AFTER_RESULT_MSG);
            }
            if st.proposals >= self.opts.max_calls {
                drop(st);
                return self.refuse(&route, RefusalCode::Budget, BUDGET_MSG);
            }
            st.proposals += 1;
        }
        if let Some(p) = args_problem(&args) {
            return self.refuse(&route, RefusalCode::InvalidArguments, p);
        }
        if let Err(errs) = schema::validate(&route.input_schema, &args) {
            return self.refuse(&route, RefusalCode::InvalidArguments, schema_errors(errs));
        }

        let entry = self.hub.registry().server(&route.server_id).cloned();
        let Some(entry) = entry.filter(|e| e.is_active()) else {
            return self.refuse(&route, RefusalCode::NotApproved, BLOCKED_MSG);
        };
        let conn = match self.hub.connection(&entry).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(connector = %route.server_id, error = %short_err(&e), "connector unreachable");
                self.log(&route, CallOutcome::Failed, None, LogExtra::default());
                return Err(self.server_error(
                    &route,
                    format!(
                        "Couldn't reach {} right now. Answer without it.",
                        route.server_name
                    ),
                ));
            }
        };
        let permission = match self.verify_live(&route, &conn).await {
            Ok(Some(p)) => p,
            Ok(None) => return self.refuse(&route, RefusalCode::ToolChanged, CHANGED_MSG),
            Err(e) => {
                self.hub.drop_connection(&route.server_id).await;
                tracing::warn!(connector = %route.server_id, error = %short_err(&e), "connector tool check failed");
                self.log(&route, CallOutcome::Failed, None, LogExtra::default());
                return Err(self.server_error(
                    &route,
                    format!(
                        "Couldn't check {}'s tools right now. Answer without it.",
                        route.server_name
                    ),
                ));
            }
        };

        let (final_args, approval) = match permission {
            Permission::Blocked => {
                return self.refuse(&route, RefusalCode::NotOffered, BLOCKED_MSG)
            }
            Permission::AlwaysAllow => (args.clone(), Approval::Automatic),
            Permission::NeedsApproval => {
                let proposal = CallProposal {
                    proposal_id: format!("p{}", uuid::Uuid::new_v4().simple()),
                    turn_id: self.opts.turn_id.clone(),
                    server_id: route.server_id.clone(),
                    server_name: route.server_name.clone(),
                    destination: route.destination.clone(),
                    tool: route.tool.clone(),
                    tool_title: route.title.clone(),
                    kind: route.kind,
                    destructive: route.destructive,
                    args: args.clone(),
                    input_schema: route.input_schema.clone(),
                };
                match self.approver.decide(proposal).await {
                    Decision::Run { args: a } => (a, Approval::User),
                    Decision::Decline => {
                        self.log(&route, CallOutcome::Declined, None, LogExtra::default());
                        return Err(DECLINED_MSG.into());
                    }
                    Decision::TimedOut => {
                        return self.refuse(&route, RefusalCode::TimedOut, TIMED_OUT_MSG)
                    }
                }
            }
        };
        if let Some(p) = args_problem(&final_args) {
            return self.refuse(&route, RefusalCode::InvalidArguments, p);
        }
        if let Err(errs) = schema::validate(&route.input_schema, &final_args) {
            return self.refuse(&route, RefusalCode::InvalidArguments, schema_errors(errs));
        }
        let Value::Object(map) = final_args.clone() else {
            return self.refuse(
                &route,
                RefusalCode::InvalidArguments,
                "The arguments must be a JSON object.",
            );
        };
        let args_edited = final_args != args;

        // From here on, whatever comes back is the server's: seal the turn.
        self.state.lock().unwrap().sealed = true;
        let started = Instant::now();
        let result = conn.call_tool(&route.tool, map, CALL_TIMEOUT).await;
        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Err(e) => {
                self.hub.drop_connection(&route.server_id).await;
                tracing::warn!(connector = %route.server_id, error = %short_err(&e), "connector call failed");
                self.log(
                    &route,
                    CallOutcome::Failed,
                    None,
                    LogExtra {
                        approval: Some(approval),
                        duration_ms,
                        args_edited,
                        ..Default::default()
                    },
                );
                let why = match e {
                    SessionError::Timeout => "it took too long",
                    SessionError::AuthRequired => "sign in again in Settings",
                    SessionError::Unsupported(_) => {
                        "the service asked for something FOTW doesn't support"
                    }
                    _ => "connection or protocol error",
                };
                Err(self.server_error(
                    &route,
                    format!(
                        "The call to {} failed ({why}). Answer without it.",
                        route.server_name
                    ),
                ))
            }
            Ok(res) => {
                let r = fence::render(
                    &route.server_name,
                    &route.tool,
                    &res,
                    route.output_schema.as_ref(),
                    self.opts.max_result_chars,
                );
                self.log(
                    &route,
                    if r.is_error {
                        CallOutcome::ServerError
                    } else {
                        CallOutcome::Ran
                    },
                    None,
                    LogExtra {
                        approval: Some(approval),
                        duration_ms,
                        result_chars: r.raw_chars,
                        truncated: r.truncated,
                        schema: r.schema,
                        args_edited,
                    },
                );
                if r.is_error {
                    Err(r.text)
                } else {
                    Ok(r.text)
                }
            }
        }
    }
}
