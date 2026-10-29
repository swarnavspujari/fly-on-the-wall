//! The connector hub (one per app) and the per-turn tool set handed to the
//! Ask harness.
//!
//! A [`ConnectorTurn`] enforces the call-time rules, in this order:
//! 1. only tools offered this turn can be called; nothing is offered on a
//!    small local model or when the turn's scope includes Keep-local notes,
//!    and [`ExternalTools::mark_keep_local`] refuses everything after;
//! 2. once any connector result (or server error) has been returned, every
//!    later proposal in the turn is refused;
//! 3. proposals are capped per turn;
//! 4. arguments must match the pinned input schema;
//! 5. the tool is re-listed live: if its definition changed or it vanished,
//!    it's turned off and the call refused (the user reviews the diff);
//! 6. the user sees the exact, editable arguments on a confirm card and must
//!    click Run ([`CallApprover`]); edited arguments are validated again;
//! 7. the result is fenced, schema-checked and size-capped ([`crate::fence`]).
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

use crate::config::{ConfigError, ServerConfig, TransportConfig};
use crate::fence::{self, SchemaCheck, DEFAULT_MAX_RESULT_CHARS};
use crate::netguard::NetPolicy;
use crate::oauth;
use crate::pin::{self, kind, model_tool_name, ReconcileReport, ToolKind};
use crate::registry::{
    append_log, apply_permissions, env_secret_key, plan_permissions, secret_keys_for, CallLogEntry,
    CallOutcome, ConnectorStore, PermissionPlan, RefusalCode, Registry, ServerEntry,
    ToolPermission,
};
use crate::schema;
use crate::session::{self, ConnectOptions, Connection, SessionError, CALL_TIMEOUT};

/// Most connector tools offered in one turn (context budget).
pub const MAX_OFFERED_TOOLS: usize = 24;
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
    /// Host or local program the call goes to.
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

/// Opens connections. The app uses [`LiveConnector`]; tests plug in a
/// fixture server.
#[async_trait]
pub trait Connector: Send + Sync {
    async fn connect(&self, entry: &ServerEntry) -> Result<Connection, SessionError>;
}

/// Connects for real: stdio programs with their keychain variables, HTTP
/// servers with a fresh OAuth token when they use sign-in.
pub struct LiveConnector {
    pub secrets: Arc<dyn SecretStore>,
    pub policy: NetPolicy,
}

#[async_trait]
impl Connector for LiveConnector {
    async fn connect(&self, entry: &ServerEntry) -> Result<Connection, SessionError> {
        let mut opts = ConnectOptions::default();
        if let TransportConfig::Stdio { env_names, .. } = &entry.config.transport {
            for name in env_names {
                if let Ok(Some(v)) = self.secrets.get(&env_secret_key(&entry.id, name)) {
                    opts.env.push((name.clone(), v));
                }
            }
        }
        if entry.config.uses_oauth() {
            opts.access_token = Some(
                oauth::access_token(entry, self.secrets.clone(), self.policy)
                    .await
                    .map_err(|e| match e {
                        oauth::OAuthError::SignInRequired => SessionError::AuthRequired,
                        other => SessionError::Connect(other.to_string()),
                    })?,
            );
        }
        session::connect(&entry.config, self.policy, opts).await
    }
}

/// Called with a server id when its stored state changed outside a Settings
/// command (for example a tool turned off mid-turn), so the UI can refresh.
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

fn now() -> i64 {
    chrono::Utc::now().timestamp()
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

    /// Store a configuration the user approved in a native dialog. The
    /// approval is bound to the configuration's hash.
    pub fn add_approved(&self, config: ServerConfig) -> Result<String, HubError> {
        let config = self.check_config(config)?;
        self.update(|reg| Ok(reg.add_approved(config, now())?))
    }

    /// Remove a connector, its keychain entries and its connection.
    pub async fn remove(&self, id: &str) -> Result<(), HubError> {
        let removed = self.update(|reg| reg.remove(id).ok_or(HubError::NotFound))?;
        for key in secret_keys_for(&removed) {
            if let Err(e) = self.secrets.delete(&key) {
                tracing::warn!(key, error = %e, "couldn't delete a connector keychain entry");
            }
        }
        if let Some(c) = self.connections.lock().await.remove(id) {
            drop(c);
        }
        Ok(())
    }

    /// Connect, list tools and update the pins. New tools arrive turned off.
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

    fn reconcile(&self, id: &str, live: &[pin::LiveTool]) -> Result<ReconcileReport, HubError> {
        self.update(|reg| {
            let entry = reg.server_mut(id).ok_or(HubError::NotFound)?;
            let report = pin::reconcile(&mut entry.tools, live, now())
                .map_err(|e| HubError::Invalid(e.to_string()))?;
            entry.last_sync = Some(now());
            Ok(report)
        })
    }

    /// Sort requested permission changes into grants and removals, without
    /// applying them (grants need the user's yes in a native dialog first).
    pub fn plan_permissions(
        &self,
        id: &str,
        changes: &[ToolPermission],
    ) -> Result<PermissionPlan, HubError> {
        let reg = self.registry();
        let entry = reg.server(id).ok_or(HubError::NotFound)?;
        plan_permissions(entry, changes).map_err(HubError::Invalid)
    }

    /// Apply changes; re-planned against current state so a stale plan can't
    /// grant more than the user saw.
    pub fn apply_permissions(
        &self,
        id: &str,
        changes: &[ToolPermission],
        approved: &PermissionPlan,
    ) -> Result<(), HubError> {
        self.update(|reg| {
            let entry = reg.server_mut(id).ok_or(HubError::NotFound)?;
            let plan = plan_permissions(entry, changes).map_err(HubError::Invalid)?;
            if plan != *approved {
                return Err(HubError::Invalid(
                    "The connector changed while you were deciding; review it again.".into(),
                ));
            }
            apply_permissions(entry, &plan);
            Ok(())
        })
    }

    /// Accept a tool's pending definition change (after the user's yes).
    /// `expected_hash` is the pending hash the user reviewed.
    pub fn accept_change(&self, id: &str, tool: &str, expected_hash: &str) -> Result<(), HubError> {
        self.update(|reg| {
            let entry = reg.server_mut(id).ok_or(HubError::NotFound)?;
            let pin = entry
                .tool_mut(tool)
                .ok_or_else(|| HubError::Invalid("No such tool.".into()))?;
            if pin.pending.as_ref().map(|p| p.hash.as_str()) != Some(expected_hash) {
                return Err(HubError::Invalid(
                    "The tool changed again; review the latest change.".into(),
                ));
            }
            pin::accept_change(pin);
            Ok(())
        })
    }

    /// Store a stdio connector's environment variable in the keychain.
    pub fn set_env_secret(&self, id: &str, name: &str, value: &str) -> Result<(), HubError> {
        let reg = self.registry();
        let entry = reg.server(id).ok_or(HubError::NotFound)?;
        match &entry.config.transport {
            TransportConfig::Stdio { env_names, .. } if env_names.iter().any(|n| n == name) => {}
            _ => {
                return Err(HubError::Invalid(
                    "That variable isn't part of this connector.".into(),
                ))
            }
        }
        if value.len() > 8 * 1024 || value.contains('\0') {
            return Err(HubError::Invalid("That value is too long.".into()));
        }
        let key = env_secret_key(id, name);
        let res = if value.is_empty() {
            self.secrets.delete(&key)
        } else {
            self.secrets.set(&key, value)
        };
        res.map_err(|e| HubError::Store(e.to_string()))
    }

    /// Which env variables have a stored value (never the values).
    pub fn env_set(&self, entry: &ServerEntry) -> Vec<(String, bool)> {
        match &entry.config.transport {
            TransportConfig::Stdio { env_names, .. } => env_names
                .iter()
                .map(|n| {
                    let set =
                        matches!(self.secrets.get(&env_secret_key(&entry.id, n)), Ok(Some(_)));
                    (n.clone(), set)
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn signed_in(&self, entry: &ServerEntry) -> bool {
        crate::secrets::KeychainCredentials::new(self.secrets.clone(), entry.id.clone())
            .has_tokens()
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
        crate::secrets::KeychainCredentials::new(self.secrets.clone(), entry.id)
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
        let conn = Arc::new(self.connector.connect(entry).await?);
        conns.insert(
            entry.id.clone(),
            Cached {
                conn: conn.clone(),
                last_used: Instant::now(),
            },
        );
        Ok(conn)
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
     turned it off until the user reviews the change in Settings. Answer without it.";

impl ConnectorTurn {
    fn new(hub: Arc<ConnectorHub>, opts: TurnOptions, approver: Arc<dyn CallApprover>) -> Self {
        let mut specs = Vec::new();
        let mut routes = HashMap::new();
        if opts.tools_allowed && !opts.keep_local_in_scope && opts.max_calls > 0 {
            let reg = hub.registry();
            'outer: for entry in reg.servers.iter().filter(|e| e.is_approved()) {
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
                        ToolKind::Read => "read-only",
                        ToolKind::Write => "may change data",
                    };
                    let what = d
                        .title
                        .as_deref()
                        .map(|t| format!("{t}. "))
                        .unwrap_or_default();
                    specs.push(ToolSpec {
                        name: name.clone(),
                        description: format!(
                            "[{} connector, {label}; the user confirms every call] {what}{}",
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

    /// The live definition still matches the pin, and the tool is still on.
    async fn verify_live(&self, route: &Route, conn: &Connection) -> Result<bool, SessionError> {
        let live = conn.list_tools().await?;
        let report = self
            .hub
            .reconcile(&route.server_id, &live)
            .map_err(|e| SessionError::Listing(e.to_string()))?;
        if !report.is_empty() {
            self.hub.notify(&route.server_id);
        }
        let reg = self.hub.registry();
        let ok = reg.server(&route.server_id).is_some_and(|e| {
            e.is_approved()
                && e.tool(&route.tool)
                    .is_some_and(|p| p.offerable() && kind(p) == route.kind)
        });
        Ok(ok)
    }
}

#[derive(Debug, Default)]
struct LogExtra {
    duration_ms: u64,
    result_chars: usize,
    truncated: bool,
    schema: SchemaCheck,
    args_edited: bool,
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
        if !args.is_object() {
            return self.refuse(
                &route,
                RefusalCode::InvalidArguments,
                "The arguments must be a JSON object.",
            );
        }
        if let Err(errs) = schema::validate(&route.input_schema, &args) {
            return self.refuse(&route, RefusalCode::InvalidArguments, schema_errors(errs));
        }

        let entry = self.hub.registry().server(&route.server_id).cloned();
        let Some(entry) = entry.filter(|e| e.is_approved()) else {
            return self.refuse(&route, RefusalCode::NotApproved, CHANGED_MSG);
        };
        let conn = match self.hub.connection(&entry).await {
            Ok(c) => c,
            Err(e) => {
                // Error text can carry the server's words: the model gets a
                // fixed message; details stay in the local log.
                tracing::warn!(connector = %route.server_id, error = %short_err(&e), "connector unreachable");
                self.log(&route, CallOutcome::Failed, None, LogExtra::default());
                return Err(format!(
                    "Couldn't reach {} right now. Answer without it.",
                    route.server_name
                ));
            }
        };
        match self.verify_live(&route, &conn).await {
            Ok(true) => {}
            Ok(false) => return self.refuse(&route, RefusalCode::ToolChanged, CHANGED_MSG),
            Err(e) => {
                self.hub.drop_connection(&route.server_id).await;
                self.log(&route, CallOutcome::Failed, None, LogExtra::default());
                tracing::warn!(connector = %route.server_id, error = %short_err(&e), "connector tool check failed");
                return Err(format!(
                    "Couldn't check {}'s tools right now. Answer without it.",
                    route.server_name
                ));
            }
        }

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
        let final_args = match self.approver.decide(proposal).await {
            Decision::Run { args: a } => a,
            Decision::Decline => {
                self.log(&route, CallOutcome::Declined, None, LogExtra::default());
                return Err(DECLINED_MSG.into());
            }
            Decision::TimedOut => return self.refuse(&route, RefusalCode::TimedOut, TIMED_OUT_MSG),
        };
        let Value::Object(map) = final_args.clone() else {
            return self.refuse(
                &route,
                RefusalCode::InvalidArguments,
                "The arguments must be a JSON object.",
            );
        };
        if let Err(errs) = schema::validate(&route.input_schema, &final_args) {
            return self.refuse(&route, RefusalCode::InvalidArguments, schema_errors(errs));
        }
        let args_edited = final_args != args;

        // From here on, whatever comes back is the server's: seal the turn.
        self.state.lock().unwrap().sealed = true;
        let started = Instant::now();
        let result = conn.call_tool(&route.tool, map, CALL_TIMEOUT).await;
        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Err(e) => {
                self.hub.drop_connection(&route.server_id).await;
                self.log(
                    &route,
                    CallOutcome::Failed,
                    None,
                    LogExtra {
                        duration_ms,
                        args_edited,
                        ..Default::default()
                    },
                );
                // Transport errors can carry the server's words: the model
                // gets a fixed message; details stay in the local log.
                tracing::warn!(connector = %route.server_id, error = %short_err(&e), "connector call failed");
                Err(format!(
                    "The call to {} failed ({}). Answer without it.",
                    route.server_name,
                    match e {
                        SessionError::Timeout => "it took too long",
                        SessionError::AuthRequired => "sign in again in Settings",
                        SessionError::Unsupported(_) => {
                            "the service asked for something FOTW doesn't support"
                        }
                        _ => "connection or protocol error",
                    }
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
