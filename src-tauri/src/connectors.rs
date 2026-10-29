//! Connectors (beta): the app side of `fly-mcp-client`.
//!
//! * State lives in backend-only settings (`connectors.registry`,
//!   `connectors.call_log`): `set_app_setting` refuses `connectors.*`, so a
//!   script in the webview can't add a server, flip a permission or forge an
//!   approval.
//! * Everything that grants trust — adding a server (its approval is bound to
//!   the config hash), turning tools on, marking them read-only, accepting a
//!   changed tool definition — happens here after a yes in a native dialog
//!   the webview can't draw or click (same pattern as cloud consent). Taking
//!   trust away (remove, turn off, sign out) needs no dialog.
//! * Proposed calls reach the webview as `connectors:proposal` events and
//!   wait for `decide_connector_call`. Edited arguments are re-validated
//!   here; a call to a tool that may change data also needs a yes in a
//!   native dialog showing the final arguments.
//! * "Delete all my data" removes every connector keychain entry
//!   ([`stored_secret_keys`]).
//!
//! The Ask harness gets a per-turn tool set from [`turn_for_ask`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fly_llm::external_tools::ExternalTools;
use fly_mcp_client::config::{AuthConfig, ServerConfig, TransportConfig};
use fly_mcp_client::hub::{CallApprover, CallProposal, ConnectorHub, Decision, TurnOptions};
use fly_mcp_client::oauth::BrowserOpener;
use fly_mcp_client::pin::{self, model_tool_name, FieldChange, ToolKind};
use fly_mcp_client::registry::{
    CallLogEntry, ConnectorStore, PermissionPlan, Registry, ServerEntry, ToolPermission,
};
use fly_mcp_client::sanitize::clean_capped;
use fly_mcp_client::secrets::KeychainCredentials;
use fly_storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::llm_commands::{ask_consent_natively, ConsentAnswer, ConsentPrompt};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Backend-only setting holding the registry (servers, approvals, pins).
pub const REGISTRY_KEY: &str = "connectors.registry";
/// Backend-only setting holding the call log.
pub const CALL_LOG_KEY: &str = "connectors.call_log";
/// The beta flag (webview-writable UI preference; it only hides the feature,
/// it grants nothing).
pub const BETA_KEY: &str = "ui.beta.connectors";

pub const EVENT_PROPOSAL: &str = "connectors:proposal";
pub const EVENT_SETTLED: &str = "connectors:settled";
pub const EVENT_CHANGED: &str = "connectors:changed";

/// A card nobody answers is withdrawn after this long.
const DECISION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Longest argument text shown in a native dialog.
const DIALOG_ARGS_CHARS: usize = 1200;

fn err_str<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// [`ConnectorStore`] on the app database's settings table.
struct SettingsStore {
    app: AppHandle,
}

impl SettingsStore {
    fn with_storage<T>(&self, f: impl FnOnce(&Storage) -> T) -> T {
        let state = self.app.state::<AppState>();
        let storage = state.storage.lock().unwrap();
        f(&storage)
    }
}

impl ConnectorStore for SettingsStore {
    fn load(&self) -> Registry {
        self.with_storage(|s| match s.get_setting(REGISTRY_KEY) {
            Ok(Some(json)) => Registry::from_json(&json),
            _ => Registry::default(),
        })
    }

    fn save(&self, registry: &Registry) -> Result<(), String> {
        self.with_storage(|s| s.set_setting(REGISTRY_KEY, &registry.to_json()))
            .map_err(err_str)
    }

    fn load_log(&self) -> Vec<CallLogEntry> {
        self.with_storage(|s| match s.get_setting(CALL_LOG_KEY) {
            Ok(Some(json)) => serde_json::from_str(&json).unwrap_or_default(),
            _ => Vec::new(),
        })
    }

    fn save_log(&self, log: &[CallLogEntry]) -> Result<(), String> {
        let json = serde_json::to_string(log).map_err(err_str)?;
        self.with_storage(|s| s.set_setting(CALL_LOG_KEY, &json))
            .map_err(err_str)
    }
}

struct PendingCall {
    proposal: CallProposal,
    reply: tokio::sync::oneshot::Sender<Decision>,
}

/// Tauri-managed connector state.
pub struct ConnectorsState {
    pub hub: Arc<ConnectorHub>,
    pending: Mutex<HashMap<String, PendingCall>>,
    /// One native dialog at a time.
    dialog: tokio::sync::Mutex<()>,
}

/// Build and manage the connector state (called once from `setup`).
pub fn init(app: &AppHandle) {
    let secrets = app.state::<AppState>().secrets.clone();
    let emitter = app.clone();
    let hub = ConnectorHub::live(Arc::new(SettingsStore { app: app.clone() }), secrets)
        .with_change_listener(Arc::new(move |id: &str| {
            let _ = emitter.emit(EVENT_CHANGED, serde_json::json!({ "id": id }));
        }));
    app.manage(ConnectorsState {
        hub: Arc::new(hub),
        pending: Mutex::new(HashMap::new()),
        dialog: tokio::sync::Mutex::new(()),
    });
}

/// Close connector connections (stdio programs stop) on exit.
pub fn shutdown(app: &AppHandle) {
    if let Some(st) = app.try_state::<ConnectorsState>() {
        tauri::async_runtime::block_on(st.hub.close_all());
    }
}

fn beta_enabled(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let v = state.storage.lock().unwrap().get_setting(BETA_KEY);
    matches!(v.ok().flatten().as_deref(), Some("1" | "true"))
}

/// The connector tool set for one Ask turn, or `None` when the beta is off.
/// `tier` is the Ask capability tier ("S", "M" or "C"); tier S gets no
/// tools. `keep_local_in_scope`: the turn's scope includes Keep-local notes,
/// so nothing is offered.
pub fn turn_for_ask(
    app: &AppHandle,
    turn_id: &str,
    tier: &str,
    keep_local_in_scope: bool,
) -> Option<Arc<dyn ExternalTools>> {
    if !beta_enabled(app) {
        return None;
    }
    let st = app.try_state::<ConnectorsState>()?;
    let approver = Arc::new(AppApprover { app: app.clone() });
    Some(Arc::new(st.hub.turn(
        TurnOptions::for_tier(turn_id, tier, keep_local_in_scope),
        approver,
    )))
}

/// Every keychain key the stored connectors may own (read before "Delete all
/// my data" empties the database).
pub fn stored_secret_keys(storage: &Storage) -> Vec<String> {
    match storage.get_setting(REGISTRY_KEY) {
        Ok(Some(json)) => Registry::from_json(&json).secret_keys(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Approver: the confirm card
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settled<'a> {
    proposal_id: &'a str,
    outcome: &'a str,
}

struct AppApprover {
    app: AppHandle,
}

/// Withdraws the card if the call is abandoned (turn cancelled, timeout).
struct WithdrawOnDrop {
    app: AppHandle,
    id: String,
}

impl Drop for WithdrawOnDrop {
    fn drop(&mut self) {
        let st = self.app.state::<ConnectorsState>();
        let was_pending = st.pending.lock().unwrap().remove(&self.id).is_some();
        if was_pending {
            let _ = self.app.emit(
                EVENT_SETTLED,
                Settled {
                    proposal_id: &self.id,
                    outcome: "withdrawn",
                },
            );
        }
    }
}

#[async_trait]
impl CallApprover for AppApprover {
    async fn decide(&self, proposal: CallProposal) -> Decision {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = proposal.proposal_id.clone();
        {
            let st = self.app.state::<ConnectorsState>();
            st.pending.lock().unwrap().insert(
                id.clone(),
                PendingCall {
                    proposal: proposal.clone(),
                    reply: tx,
                },
            );
        }
        let _guard = WithdrawOnDrop {
            app: self.app.clone(),
            id: id.clone(),
        };
        let _ = self.app.emit(EVENT_PROPOSAL, &proposal);
        match tokio::time::timeout(DECISION_TIMEOUT, rx).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) => Decision::Decline,
            Err(_) => {
                let _ = self.app.emit(
                    EVENT_SETTLED,
                    Settled {
                        proposal_id: &id,
                        outcome: "timed_out",
                    },
                );
                Decision::TimedOut
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideResult {
    ok: bool,
    error: Option<String>,
}

/// Proposals waiting for the user (the card catches up on mount).
#[tauri::command]
pub fn pending_connector_calls(st: State<'_, ConnectorsState>) -> Vec<CallProposal> {
    st.pending
        .lock()
        .unwrap()
        .values()
        .map(|p| p.proposal.clone())
        .collect()
}

/// The user's answer to a confirm card. `args` are the (possibly edited)
/// arguments; `None` runs the proposal as shown. Invalid arguments keep the
/// card open. A call that may change data also needs a yes in a native
/// dialog showing the final arguments.
#[tauri::command]
pub async fn decide_connector_call(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    proposal_id: String,
    run: bool,
    args: Option<Value>,
) -> CmdResult<DecideResult> {
    let proposal = st
        .pending
        .lock()
        .unwrap()
        .get(&proposal_id)
        .map(|p| p.proposal.clone())
        .ok_or("That call is no longer waiting for an answer.")?;
    let settle = |decision: Decision, outcome: &str| {
        if let Some(p) = st.pending.lock().unwrap().remove(&proposal_id) {
            let _ = p.reply.send(decision);
            let _ = app.emit(
                EVENT_SETTLED,
                Settled {
                    proposal_id: &proposal_id,
                    outcome,
                },
            );
        }
    };
    if !run {
        settle(Decision::Decline, "declined");
        return Ok(DecideResult {
            ok: true,
            error: None,
        });
    }
    let final_args = args.unwrap_or_else(|| proposal.args.clone());
    if !final_args.is_object() {
        return Ok(DecideResult {
            ok: false,
            error: Some("The arguments must be a JSON object.".into()),
        });
    }
    if let Err(errs) = fly_mcp_client::schema::validate(&proposal.input_schema, &final_args) {
        return Ok(DecideResult {
            ok: false,
            error: Some(format!(
                "These arguments don't fit the tool: {}",
                errs.join("; ")
            )),
        });
    }
    if proposal.kind == ToolKind::Write {
        let prompt = write_call_prompt(&proposal, &final_args);
        let _one = st.dialog.lock().await;
        if ask_consent_natively(&app, &prompt).await != ConsentAnswer::Allow {
            settle(Decision::Decline, "declined");
            return Ok(DecideResult {
                ok: true,
                error: None,
            });
        }
    }
    settle(Decision::Run { args: final_args }, "ran");
    Ok(DecideResult {
        ok: true,
        error: None,
    })
}

fn pretty_capped(v: &Value) -> String {
    let s = serde_json::to_string_pretty(v).unwrap_or_default();
    clean_capped(&s, DIALOG_ARGS_CHARS)
}

fn write_call_prompt(p: &CallProposal, args: &Value) -> ConsentPrompt {
    let tool = clean_capped(&p.tool, 80);
    let server = clean_capped(&p.server_name, 60);
    let warning = if p.destructive {
        "The service says this tool can delete or overwrite data."
    } else {
        "This tool may change data in the service."
    };
    ConsentPrompt {
        title: format!("Run {tool} on {server}?"),
        message: format!(
            "{warning}\n\nSends to: {}\n\nWith these details:\n{}\n\nRun it only if this is what you want.",
            clean_capped(&p.destination, 120),
            pretty_capped(args)
        ),
        allow: "Run".into(),
        decline: "Don't run".into(),
    }
}

// ---------------------------------------------------------------------------
// Settings › Connectors
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvView {
    name: String,
    set: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolView {
    name: String,
    title: Option<String>,
    description: String,
    enabled: bool,
    read_only: bool,
    kind: ToolKind,
    can_mark_read_only: bool,
    destructive: bool,
    status: &'static str,
    problem: Option<String>,
    pinned_hash: String,
    pending_hash: Option<String>,
    diff: Vec<FieldChange>,
    input_schema: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorView {
    id: String,
    name: String,
    transport: TransportConfig,
    auth: AuthConfig,
    destination: String,
    approved: bool,
    config_hash: String,
    signed_in: Option<bool>,
    env: Vec<EnvView>,
    last_sync: Option<i64>,
    tools: Vec<ToolView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    beta: bool,
    connectors: Vec<ConnectorView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveResult {
    saved: bool,
    connector: Option<ConnectorView>,
}

fn short(hash: &str) -> String {
    hash.chars().take(12).collect()
}

fn tool_view(p: &pin::ToolPin) -> ToolView {
    let status = if p.pending.is_some() {
        "changed"
    } else if p.missing {
        "missing"
    } else if p.definition.problem.is_some() {
        "unusable"
    } else {
        "ok"
    };
    ToolView {
        name: p.name.clone(),
        title: p.definition.title.clone(),
        description: p.definition.description.clone(),
        enabled: p.enabled,
        read_only: p.read_only,
        kind: pin::kind(p),
        can_mark_read_only: p.can_mark_read_only(),
        destructive: p.definition.annotations.destructive_hint == Some(true),
        status,
        problem: p.definition.problem.clone(),
        pinned_hash: short(&p.hash),
        pending_hash: p.pending.as_ref().map(|c| c.hash.clone()),
        diff: p
            .pending
            .as_ref()
            .map(|c| pin::diff(&p.definition, &c.definition))
            .unwrap_or_default(),
        input_schema: p.definition.input_schema.clone(),
    }
}

fn view(hub: &ConnectorHub, e: &ServerEntry) -> ConnectorView {
    ConnectorView {
        id: e.id.clone(),
        name: e.config.name.clone(),
        transport: e.config.transport.clone(),
        auth: e.config.auth.clone(),
        destination: e.config.destination(),
        approved: e.is_approved(),
        config_hash: short(e.approved_hash.as_deref().unwrap_or("")),
        signed_in: e.config.uses_oauth().then(|| hub.signed_in(e)),
        env: hub
            .env_set(e)
            .into_iter()
            .map(|(name, set)| EnvView { name, set })
            .collect(),
        last_sync: e.last_sync,
        tools: e.tools.iter().map(tool_view).collect(),
    }
}

fn view_of(hub: &ConnectorHub, id: &str) -> CmdResult<ConnectorView> {
    let reg = hub.registry();
    let e = reg.server(id).ok_or("No connector with that id.")?;
    Ok(view(hub, e))
}

#[tauri::command]
pub fn connectors_overview(app: AppHandle, st: State<'_, ConnectorsState>) -> Overview {
    let reg = st.hub.registry();
    Overview {
        beta: beta_enabled(&app),
        connectors: reg.servers.iter().map(|e| view(&st.hub, e)).collect(),
    }
}

/// What the Add form sends.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorConfigInput {
    name: String,
    transport: TransportConfig,
    #[serde(default)]
    auth: AuthConfig,
    #[serde(default)]
    client_secret: Option<String>,
}

fn add_prompt(c: &ServerConfig) -> ConsentPrompt {
    let mut lines = vec![format!(
        "Ask will be able to use tools from {}.",
        clean_capped(&c.name, 60)
    )];
    match &c.transport {
        TransportConfig::Http { url } => {
            lines.push(format!("Connects to: {}", clean_capped(url, 300)));
        }
        TransportConfig::Stdio {
            command,
            args,
            env_names,
        } => {
            lines.push(format!(
                "Runs this program on your computer: {} {}",
                clean_capped(command, 300),
                clean_capped(&args.join(" "), 400)
            ));
            if !env_names.is_empty() {
                lines.push(format!(
                    "With these variables from your keychain: {}",
                    env_names.join(", ")
                ));
            }
        }
    }
    if c.uses_oauth() {
        lines.push("You'll sign in to it in your browser.".into());
    }
    lines.push(
        "Its tools start turned off. Every call is shown to you first, with its exact details, \
         and runs only if you click Run. Notes you keep local are never used with connectors."
            .into(),
    );
    ConsentPrompt {
        title: format!("Connect {}?", clean_capped(&c.name, 60)),
        message: lines.join("\n\n"),
        allow: "Connect".into(),
        decline: "Cancel".into(),
    }
}

/// Add a connector after a yes in a native dialog. The approval stored is
/// bound to this exact configuration's hash.
#[tauri::command]
pub async fn add_connector(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    config: ConnectorConfigInput,
) -> CmdResult<SaveResult> {
    let cfg = st
        .hub
        .check_config(ServerConfig {
            name: config.name,
            transport: config.transport,
            auth: config.auth,
        })
        .map_err(err_str)?;
    let secret = config.client_secret.filter(|s| !s.trim().is_empty());
    if secret.is_some()
        && !matches!(
            &cfg.auth,
            AuthConfig::OAuth {
                client_id: Some(_),
                ..
            }
        )
    {
        return Err("A client secret needs a client ID.".into());
    }
    if st
        .hub
        .registry()
        .servers
        .iter()
        .any(|s| s.config.name.eq_ignore_ascii_case(&cfg.name))
    {
        return Err("A connector with that name already exists.".into());
    }
    let prompt = add_prompt(&cfg);
    let answer = {
        let _one = st.dialog.lock().await;
        ask_consent_natively(&app, &prompt).await
    };
    if answer != ConsentAnswer::Allow {
        return Ok(SaveResult {
            saved: false,
            connector: None,
        });
    }
    let id = st.hub.add_approved(cfg).map_err(err_str)?;
    if let Some(s) = secret {
        KeychainCredentials::new(st.hub.secrets(), id.clone())
            .set_client_secret(Some(s.trim()))
            .map_err(err_str)?;
    }
    tracing::info!(connector = %id, "connector added");
    Ok(SaveResult {
        saved: true,
        connector: Some(view_of(&st.hub, &id)?),
    })
}

/// Remove a connector, its keychain entries and its connection.
#[tauri::command]
pub async fn remove_connector(st: State<'_, ConnectorsState>, id: String) -> CmdResult<()> {
    st.hub.remove(&id).await.map_err(err_str)?;
    tracing::info!(connector = %id, "connector removed");
    Ok(())
}

/// Connect and refresh the tool list. New tools arrive turned off; changed
/// ones are turned off with their diff kept.
#[tauri::command]
pub async fn sync_connector_tools(
    st: State<'_, ConnectorsState>,
    id: String,
) -> CmdResult<ConnectorView> {
    st.hub.sync_tools(&id).await.map_err(err_str)?;
    view_of(&st.hub, &id)
}

fn permissions_prompt(name: &str, plan: &PermissionPlan) -> ConsentPrompt {
    let mut lines = Vec::new();
    if !plan.enable.is_empty() {
        lines.push(format!("Turn on: {}", plan.enable.join(", ")));
    }
    if !plan.mark_read_only.is_empty() {
        lines.push(format!(
            "Mark as read-only (their calls are labelled \"Read\"): {}",
            plan.mark_read_only.join(", ")
        ));
    }
    if !plan.disable.is_empty() {
        lines.push(format!("Turn off: {}", plan.disable.join(", ")));
    }
    lines.push("You still confirm every call before it runs.".into());
    ConsentPrompt {
        title: format!("Change what {} can do?", clean_capped(name, 60)),
        message: lines.join("\n\n"),
        allow: "Save".into(),
        decline: "Cancel".into(),
    }
}

/// Save tool toggles. Turning tools on or marking them read-only needs a
/// yes in a native dialog; turning them off doesn't.
#[tauri::command]
pub async fn save_connector_tools(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    id: String,
    changes: Vec<ToolPermission>,
) -> CmdResult<SaveResult> {
    if changes.len() > 512 {
        return Err("Too many changes at once.".into());
    }
    let plan = st.hub.plan_permissions(&id, &changes).map_err(err_str)?;
    if plan.grants_trust() {
        let name = st
            .hub
            .registry()
            .server(&id)
            .map(|e| e.config.name.clone())
            .unwrap_or_default();
        let answer = {
            let _one = st.dialog.lock().await;
            ask_consent_natively(&app, &permissions_prompt(&name, &plan)).await
        };
        if answer != ConsentAnswer::Allow {
            return Ok(SaveResult {
                saved: false,
                connector: Some(view_of(&st.hub, &id)?),
            });
        }
    }
    st.hub
        .apply_permissions(&id, &changes, &plan)
        .map_err(err_str)?;
    Ok(SaveResult {
        saved: true,
        connector: Some(view_of(&st.hub, &id)?),
    })
}

/// Accept a tool's changed definition after a yes in a native dialog.
/// `pending_hash` is the change the user reviewed; a newer one is refused.
#[tauri::command]
pub async fn accept_connector_tool_change(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    id: String,
    tool: String,
    pending_hash: String,
) -> CmdResult<SaveResult> {
    let reg = st.hub.registry();
    let entry = reg.server(&id).ok_or("No connector with that id.")?;
    let pin = entry.tool(&tool).ok_or("No such tool.")?;
    let change = pin
        .pending
        .as_ref()
        .ok_or("This tool has no pending change.")?;
    if change.hash != pending_hash {
        return Err("The tool changed again; review the latest change.".into());
    }
    let fields: Vec<String> = pin::diff(&pin.definition, &change.definition)
        .into_iter()
        .map(|c| c.field)
        .collect();
    let prompt =
        ConsentPrompt {
            title: format!("Accept the new version of {}?", clean_capped(&tool, 80)),
            message: format!(
            "{} changed this tool since you approved it. Changed: {}.\n\nNew description:\n{}\n\n\
             Accepting pins this version. If it was read-only, mark it again.",
            clean_capped(&entry.config.name, 60),
            if fields.is_empty() { "details".into() } else { fields.join(", ") },
            clean_capped(&change.definition.description, 600),
        ),
            allow: "Accept".into(),
            decline: "Cancel".into(),
        };
    let answer = {
        let _one = st.dialog.lock().await;
        ask_consent_natively(&app, &prompt).await
    };
    if answer != ConsentAnswer::Allow {
        return Ok(SaveResult {
            saved: false,
            connector: Some(view_of(&st.hub, &id)?),
        });
    }
    st.hub
        .accept_change(&id, &tool, &pending_hash)
        .map_err(err_str)?;
    Ok(SaveResult {
        saved: true,
        connector: Some(view_of(&st.hub, &id)?),
    })
}

struct SystemBrowser {
    app: AppHandle,
}

impl BrowserOpener for SystemBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        use tauri_plugin_opener::OpenerExt;
        self.app
            .opener()
            .open_url(url, None::<&str>)
            .map_err(err_str)
    }
}

/// Sign in in the system browser (the user signs in themselves) and store
/// the tokens in the keychain. Waits up to five minutes.
#[tauri::command]
pub async fn sign_in_connector(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    id: String,
) -> CmdResult<ConnectorView> {
    st.hub
        .sign_in(&id, &SystemBrowser { app: app.clone() })
        .await
        .map_err(err_str)?;
    view_of(&st.hub, &id)
}

#[tauri::command]
pub async fn sign_out_connector(
    st: State<'_, ConnectorsState>,
    id: String,
) -> CmdResult<ConnectorView> {
    st.hub.sign_out(&id).await.map_err(err_str)?;
    view_of(&st.hub, &id)
}

/// Store (or clear, when empty) a local program's variable in the keychain.
#[tauri::command]
pub fn set_connector_env(
    st: State<'_, ConnectorsState>,
    id: String,
    name: String,
    value: String,
) -> CmdResult<ConnectorView> {
    st.hub.set_env_secret(&id, &name, &value).map_err(err_str)?;
    view_of(&st.hub, &id)
}

#[tauri::command]
pub fn connector_call_log(st: State<'_, ConnectorsState>) -> Vec<CallLogEntry> {
    st.hub.call_log()
}

#[tauri::command]
pub fn clear_connector_call_log(st: State<'_, ConnectorsState>) -> CmdResult<()> {
    st.hub.clear_call_log().map_err(err_str)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    ok: bool,
    text: String,
}

/// Try one enabled tool from Settings. Goes through the same rules as an
/// Ask turn, confirm card included.
#[tauri::command]
pub async fn test_connector_tool(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    id: String,
    tool: String,
    args: Value,
) -> CmdResult<TestResult> {
    if !beta_enabled(&app) {
        return Err("Turn on connectors first.".into());
    }
    let reg = st.hub.registry();
    let entry = reg.server(&id).ok_or("No connector with that id.")?;
    let name = model_tool_name(&entry.slug, &tool);
    let opts = TurnOptions {
        turn_id: "settings-test".into(),
        tools_allowed: true,
        keep_local_in_scope: false,
        max_calls: 1,
        max_result_chars: fly_mcp_client::fence::DEFAULT_MAX_RESULT_CHARS,
    };
    let turn = st
        .hub
        .turn(opts, Arc::new(AppApprover { app: app.clone() }));
    if !turn.owns(&name) {
        return Err("Turn this tool on (and save) before trying it.".into());
    }
    Ok(match turn.call(&name, args).await {
        Ok(text) => TestResult { ok: true, text },
        Err(text) => TestResult { ok: false, text },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_state_is_backend_only() {
        for key in [REGISTRY_KEY, CALL_LOG_KEY, "connectors.anything"] {
            assert!(!crate::commands::is_webview_setting_key(key), "{key}");
            assert!(
                !crate::commands::is_webview_writable_setting_key(key),
                "{key}"
            );
        }
        // The beta flag is a UI preference and grants nothing.
        assert!(crate::commands::is_webview_writable_setting_key(BETA_KEY));
    }

    #[test]
    fn stored_secret_keys_come_from_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        assert!(stored_secret_keys(&storage).is_empty());
        let mut reg = Registry::default();
        let id = reg
            .add_approved(
                ServerConfig {
                    name: "Docs".into(),
                    transport: TransportConfig::Http {
                        url: "https://mcp.example.com/mcp".into(),
                    },
                    auth: AuthConfig::OAuth {
                        scopes: vec![],
                        client_id: None,
                    },
                },
                0,
            )
            .unwrap();
        storage.set_setting(REGISTRY_KEY, &reg.to_json()).unwrap();
        let keys = stored_secret_keys(&storage);
        assert!(keys.contains(&format!("connector.{id}.access")));
        assert!(keys.contains(&format!("connector.{id}.refresh")));
    }

    #[test]
    fn dialogs_show_cleaned_server_text() {
        let p = CallProposal {
            proposal_id: "p1".into(),
            turn_id: "t".into(),
            server_id: "c1".into(),
            server_name: "Acme\u{202E} CRM".into(),
            destination: "mcp.example.com".into(),
            tool: "delete_contact".into(),
            tool_title: None,
            kind: ToolKind::Write,
            destructive: true,
            args: serde_json::json!({"id": "c-1"}),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let prompt = write_call_prompt(&p, &serde_json::json!({"id": "x".repeat(5000)}));
        assert_eq!(prompt.title, "Run delete_contact on Acme CRM?");
        assert!(prompt.message.contains("delete or overwrite"));
        assert!(prompt.message.chars().count() < DIALOG_ARGS_CHARS + 400);
        assert_eq!(
            prompt.decline, "Don't run",
            "the safe answer is the default button"
        );
    }
}
