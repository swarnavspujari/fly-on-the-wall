//! Connectors (beta): the app side of `fly-mcp-client`.
//!
//! * State lives in backend-only settings (`connectors.registry`,
//!   `connectors.call_log`): `set_app_setting` refuses `connectors.*`, so a
//!   script in the webview can't add a server, loosen a permission or forge
//!   an approval.
//! * Everything that grants trust — adding or editing a connector (its
//!   approval is bound to the config hash), turning one back on, loosening a
//!   tool's permission, approving a changed or new tool, sending a call to a
//!   change tool — happens here after a yes in a native dialog the webview
//!   can't draw or click (the pattern of cloud consent). Taking trust away
//!   (turn off, block, remove, sign out) needs no dialog.
//! * Calls on Needs approval reach the webview as `connectors:proposal`
//!   events and wait for `decide_connector_call`. Edited arguments are
//!   re-validated here; a change tool's call also needs the native confirm
//!   showing the final arguments. "Always allow" from a card is offered only
//!   for read-only tools.
//! * Secret values (API keys, secret headers and settings) go straight to the
//!   keychain and are never returned to the webview.
//! * "Delete all my data" removes every connector keychain entry
//!   ([`stored_secret_keys`]).
//!
//! The Ask harness gets a per-turn tool set from [`turn_for_ask`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fly_llm::external_tools::ExternalTools;
use fly_mcp_client::claude_import::{self, ImportCandidate};
use fly_mcp_client::config::{AuthConfig, ServerConfig, TransportConfig};
use fly_mcp_client::directory::{self, DirectoryEntry};
use fly_mcp_client::hub::{
    args_problem, CallApprover, CallProposal, ConnectorHub, Decision, HubError, NewSecrets,
    ProbeResult, TurnOptions,
};
use fly_mcp_client::oauth::BrowserOpener;
use fly_mcp_client::pin::{self, model_tool_name, FieldChange, Permission, ToolKind};
use fly_mcp_client::registry::{
    CallLogEntry, ConnectProblem, ConnectorStore, PermissionChange, Registry, ServerEntry,
};
use fly_mcp_client::sanitize::clean_capped;
use fly_mcp_client::session::SessionError;
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
/// The beta switch (a webview-writable UI preference: it shows the feature
/// and lets Ask offer connector tools, but grants nothing by itself).
pub const BETA_KEY: &str = "ui.beta.connectors";

pub const EVENT_PROPOSAL: &str = "connectors:proposal";
pub const EVENT_SETTLED: &str = "connectors:settled";
pub const EVENT_CHANGED: &str = "connectors:changed";

/// A card nobody answers is withdrawn after this long.
const DECISION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Longest argument text a native confirm shows. Longer arguments are
/// refused rather than cut, so the dialog always shows what is sent.
const DIALOG_ARGS_CHARS: usize = 3000;
/// Longest program output shown under "Show details".
const DETAILS_CHARS: usize = 1500;

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

/// Close connector connections (local programs stop) on exit.
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

async fn ask(st: &ConnectorsState, app: &AppHandle, prompt: &ConsentPrompt) -> bool {
    let _one = st.dialog.lock().await;
    ask_consent_natively(app, prompt).await == ConsentAnswer::Allow
}

fn q(s: &str, max: usize) -> String {
    format!("\"{}\"", clean_capped(s, max))
}

// ---------------------------------------------------------------------------
// Native dialog copy (UX brief §8). The safe answer is always the default.
// ---------------------------------------------------------------------------

fn add_prompt(c: &ServerConfig, turning_on: bool) -> ConsentPrompt {
    if let Some(run) = c.will_run() {
        let mut body = format!(
            "{} will run: {}\n\nIt has the same access to your files as you.",
            q(&c.name, 60),
            clean_capped(&run, 600)
        );
        let secrets = c.secret_names();
        if !secrets.is_empty() {
            body.push_str(&format!(
                "\n\nIt gets these settings from your system keychain: {}.",
                secrets.join(", ")
            ));
        }
        body.push_str("\n\nOnly add programs you trust. Every tool starts as Needs approval.");
        return ConsentPrompt {
            title: if turning_on {
                "Turn on a program on this computer?".into()
            } else {
                "Run a program on this computer?".into()
            },
            message: body,
            allow: if turning_on { "Turn on" } else { "Run and add" }.into(),
            decline: "Cancel".into(),
        };
    }
    let mut body = format!(
        "It can receive questions and meeting details you approve, sent to {}. Every tool starts \
         as Needs approval.",
        clean_capped(&c.destination(), 120)
    );
    if c.uses_oauth() {
        body.push_str("\n\nYou'll sign in in your browser next.");
    }
    body.push_str("\n\nFly on the Wall hasn't checked this connector. Only add ones you trust.");
    ConsentPrompt {
        title: if turning_on {
            format!("Turn on {}?", q(&c.name, 60))
        } else {
            format!("Add {} connector?", q(&c.name, 60))
        },
        message: body,
        allow: if turning_on {
            "Turn on"
        } else {
            "Add connector"
        }
        .into(),
        decline: "Cancel".into(),
    }
}

fn edit_prompt(c: &ServerConfig) -> ConsentPrompt {
    let mut p = add_prompt(c, false);
    p.title = format!("Save changes to {}?", q(&c.name, 60));
    p.allow = "Save changes".into();
    p
}

const SPEECH_WARNING: &str = "Something said in a meeting could steer the AI into using it.";

fn loosen_prompt(entry: &ServerEntry, loosen: &[PermissionChange]) -> ConsentPrompt {
    let name = clean_capped(&entry.config.name, 60);
    let kind_of = |tool: &str| entry.tool(tool).map(pin::kind).unwrap_or(ToolKind::Write);
    if let [one] = loosen {
        let tool = q(&one.tool, 80);
        return match one.permission {
            Permission::AlwaysAllow => ConsentPrompt {
                title: format!("Let the AI use {tool} without asking?"),
                message: match kind_of(&one.tool) {
                    ToolKind::Write => format!("This can change data in {name}. {SPEECH_WARNING}"),
                    ToolKind::Read => format!(
                        "The AI will send {name} what it chooses, without showing you first. \
                         {SPEECH_WARNING}"
                    ),
                },
                allow: "Always allow".into(),
                decline: "Keep asking".into(),
            },
            _ => ConsentPrompt {
                title: format!("Let the AI ask to use {tool}?"),
                message: format!("You'll see each call to {name} before it's sent."),
                allow: "Allow with approval".into(),
                decline: "Keep blocked".into(),
            },
        };
    }
    let list = |p: Permission| {
        loosen
            .iter()
            .filter(|c| c.permission == p)
            .map(|c| clean_capped(&c.tool, 80))
            .collect::<Vec<_>>()
    };
    let always = list(Permission::AlwaysAllow);
    let ask = list(Permission::NeedsApproval);
    let mut lines = Vec::new();
    if !always.is_empty() {
        lines.push(format!("Use without asking: {}", always.join(", ")));
    }
    if !ask.is_empty() {
        lines.push(format!("Ask before each call: {}", ask.join(", ")));
    }
    if loosen
        .iter()
        .any(|c| c.permission == Permission::AlwaysAllow && kind_of(&c.tool) == ToolKind::Write)
    {
        lines.push(format!(
            "Some of these can change data in {name}. {SPEECH_WARNING}"
        ));
    }
    ConsentPrompt {
        title: format!("Change permissions for {} tools in {name}?", loosen.len()),
        message: lines.join("\n\n"),
        allow: "Change permissions".into(),
        decline: "Keep as is".into(),
    }
}

fn review_prompt(entry: &ServerEntry, tool: &pin::ToolPin) -> ConsentPrompt {
    let name = clean_capped(&entry.config.name, 60);
    let (body, desc) = match &tool.pending {
        Some(p) => {
            let fields: Vec<String> = pin::diff(&tool.definition, &p.definition)
                .into_iter()
                .map(|c| c.field)
                .collect();
            (
                format!(
                    "{name} changed this tool since you approved it. Changed: {}.",
                    if fields.is_empty() {
                        "details".into()
                    } else {
                        fields.join(", ")
                    }
                ),
                p.definition.description.clone(),
            )
        }
        None => (
            format!("{name} added this tool after you approved the connector."),
            tool.definition.description.clone(),
        ),
    };
    ConsentPrompt {
        title: format!("Approve changes to {}?", q(&tool.name, 80)),
        message: format!(
            "{body}\n\nDescription:\n{}\n\nIt will be on Needs approval.",
            clean_capped(&desc, 600)
        ),
        allow: "Approve".into(),
        decline: "Keep blocked".into(),
    }
}

/// The final arguments for a native confirm, or why they can't be shown in
/// full.
fn dialog_args(args: &Value) -> Result<String, String> {
    let s = serde_json::to_string_pretty(args).map_err(err_str)?;
    if s.chars().count() > DIALOG_ARGS_CHARS {
        return Err(
            "These details are too long to show in the confirm dialog. Shorten them and try again."
                .into(),
        );
    }
    Ok(s)
}

fn send_prompt(p: &CallProposal, args_text: &str) -> ConsentPrompt {
    let warning = if p.destructive {
        "The service says this can delete or overwrite data."
    } else {
        "This can change data in the service."
    };
    ConsentPrompt {
        title: format!("Send this to {}?", q(&p.server_name, 60)),
        message: format!(
            "{} — {}\n\nSends to: {}\n\n{args_text}",
            clean_capped(&p.tool, 80),
            warning,
            clean_capped(&p.destination, 120),
        ),
        allow: "Send".into(),
        decline: "Don't send".into(),
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

/// Withdraws the card if the call is abandoned (turn cancelled).
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
                // Remove first so the drop guard doesn't also report it.
                let st = self.app.state::<ConnectorsState>();
                st.pending.lock().unwrap().remove(&id);
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
    /// The card was answered and can close.
    ok: bool,
    /// Why it stays open (bad arguments). `ok: false` with no error: the
    /// user said no in the native dialog; the card stays open.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardAction {
    Deny,
    AllowOnce,
    /// Read-only tools only: set Always allow (native dialog) and run.
    AlwaysAllow,
}

/// The user's answer to a confirm card. `args` are the (possibly edited)
/// arguments; `None` runs the proposal as shown.
#[tauri::command]
pub async fn decide_connector_call(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    proposal_id: String,
    action: CardAction,
    args: Option<Value>,
) -> CmdResult<DecideResult> {
    let proposal = st
        .pending
        .lock()
        .unwrap()
        .get(&proposal_id)
        .map(|p| p.proposal.clone())
        .ok_or("That call is no longer waiting for an answer.")?;
    let settle = |decision: Decision, outcome: &str| -> bool {
        match st.pending.lock().unwrap().remove(&proposal_id) {
            Some(p) => {
                let _ = p.reply.send(decision);
                let _ = app.emit(
                    EVENT_SETTLED,
                    Settled {
                        proposal_id: &proposal_id,
                        outcome,
                    },
                );
                true
            }
            None => false,
        }
    };
    let done = DecideResult {
        ok: true,
        error: None,
    };
    let stay = |error: Option<String>| DecideResult { ok: false, error };
    if action == CardAction::Deny {
        settle(Decision::Decline, "declined");
        return Ok(done);
    }
    let final_args = args.unwrap_or_else(|| proposal.args.clone());
    if let Some(p) = args_problem(&final_args) {
        return Ok(stay(Some(p)));
    }
    if let Err(errs) = fly_mcp_client::schema::validate(&proposal.input_schema, &final_args) {
        return Ok(stay(Some(format!(
            "These details don't fit the tool: {}",
            errs.join("; ")
        ))));
    }
    match (action, proposal.kind) {
        (CardAction::AlwaysAllow, ToolKind::Write) => {
            return Ok(stay(Some(
                "Tools that can change data can only be set to Always allow in Settings.".into(),
            )));
        }
        (CardAction::AlwaysAllow, ToolKind::Read) => {
            let changes = vec![PermissionChange {
                tool: proposal.tool.clone(),
                permission: Permission::AlwaysAllow,
            }];
            let reg = st.hub.registry();
            let entry = reg
                .server(&proposal.server_id)
                .ok_or("That connector is gone.")?;
            let plan = st
                .hub
                .plan_permissions(&proposal.server_id, &changes)
                .map_err(err_str)?;
            if !ask(&st, &app, &loosen_prompt(entry, &plan.loosen)).await {
                return Ok(stay(None));
            }
            st.hub
                .apply_loosening(&proposal.server_id, &changes, &plan.loosen)
                .map_err(err_str)?;
        }
        (_, ToolKind::Write) => {
            let text = match dialog_args(&final_args) {
                Ok(t) => t,
                Err(e) => return Ok(stay(Some(e))),
            };
            let _one = st.dialog.lock().await;
            // A second click may have waited for the first dialog.
            if !st.pending.lock().unwrap().contains_key(&proposal_id) {
                return Ok(done);
            }
            if ask_consent_natively(&app, &send_prompt(&proposal, &text)).await
                != ConsentAnswer::Allow
            {
                return Ok(stay(None));
            }
        }
        _ => {}
    }
    settle(Decision::Run { args: final_args }, "ran");
    Ok(done)
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretView {
    name: String,
    set: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsView {
    api_key_set: bool,
    client_secret_set: bool,
    /// Secret headers (web) or settings (local): set or not, never values.
    values: Vec<SecretView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolView {
    name: String,
    title: Option<String>,
    description: String,
    kind: ToolKind,
    /// The server labelled it read-only or not; unlabelled tools are change
    /// tools ("Not labelled — treated as a change tool").
    labelled: bool,
    destructive: bool,
    permission: Permission,
    /// "ok", "changed", "new", "missing" or "unusable".
    status: &'static str,
    problem: Option<String>,
    pinned_hash: String,
    /// Pass back to approve a change or a new tool.
    review_hash: Option<String>,
    diff: Vec<FieldChange>,
    input_schema: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorView {
    id: String,
    name: String,
    /// "web" or "local".
    kind: &'static str,
    transport: TransportConfig,
    auth: AuthConfig,
    destination: String,
    will_run: Option<String>,
    approved: bool,
    enabled: bool,
    config_hash: String,
    /// "connected", "needs_sign_in", "review", "could_not_start",
    /// "unreachable", "not_checked", "off" or "not_approved".
    status: &'static str,
    review_count: usize,
    /// `null`: no browser sign-in.
    signed_in: Option<bool>,
    secrets: SecretsView,
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
    /// False when the native dialog was declined.
    saved: bool,
    connector: Option<ConnectorView>,
    /// A follow-up step failed (e.g. listing tools after adding).
    error: Option<String>,
}

fn short(hash: &str) -> String {
    hash.chars().take(12).collect()
}

fn tool_view(p: &pin::ToolPin) -> ToolView {
    let status = if p.pending.is_some() {
        "changed"
    } else if p.unreviewed {
        "new"
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
        kind: pin::kind(p),
        labelled: p.labelled(),
        destructive: p.definition.annotations.destructive_hint == Some(true),
        permission: p.permission,
        status,
        problem: p.definition.problem.clone(),
        pinned_hash: short(&p.hash),
        review_hash: pin::review_hash(p).map(str::to_string),
        diff: match &p.pending {
            Some(c) => pin::diff(&p.definition, &c.definition),
            None => Vec::new(),
        },
        input_schema: p.definition.input_schema.clone(),
    }
}

fn view(hub: &ConnectorHub, e: &ServerEntry) -> ConnectorView {
    let review_count = e.tools.iter().filter(|t| t.needs_review()).count();
    let signed_in = e.config.uses_oauth().then(|| hub.signed_in(e));
    let status = if !e.is_approved() {
        "not_approved"
    } else if !e.enabled {
        "off"
    } else if signed_in == Some(false) || e.last_problem == Some(ConnectProblem::NeedsSignIn) {
        "needs_sign_in"
    } else if review_count > 0 {
        "review"
    } else {
        match e.last_problem {
            Some(ConnectProblem::CouldNotStart | ConnectProblem::ProgramNotFound) => {
                "could_not_start"
            }
            Some(ConnectProblem::Unreachable) => "unreachable",
            _ if e.last_sync.is_none() => "not_checked",
            _ => "connected",
        }
    };
    let s = hub.secret_status(e);
    ConnectorView {
        id: e.id.clone(),
        name: e.config.name.clone(),
        kind: if e.config.is_local() { "local" } else { "web" },
        transport: e.config.transport.clone(),
        auth: e.config.auth.clone(),
        destination: e.config.destination(),
        will_run: e.config.will_run(),
        approved: e.is_approved(),
        enabled: e.enabled,
        config_hash: short(e.approved_hash.as_deref().unwrap_or("")),
        status,
        review_count,
        signed_in,
        secrets: SecretsView {
            api_key_set: s.api_key_set,
            client_secret_set: s.client_secret_set,
            values: s
                .values
                .into_iter()
                .map(|(name, set)| SecretView { name, set })
                .collect(),
        },
        last_sync: e.last_sync,
        tools: e.tools.iter().map(tool_view).collect(),
    }
}

fn view_of(hub: &ConnectorHub, id: &str) -> CmdResult<ConnectorView> {
    let reg = hub.registry();
    let e = reg.server(id).ok_or("No connector with that id.")?;
    Ok(view(hub, e))
}

/// Replace every secret value with dots, so program output shown in the UI
/// can't leak a key.
fn scrub(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for s in secrets.iter().filter(|s| s.chars().count() >= 4) {
        out = out.replace(s.as_str(), "••••");
    }
    out
}

/// Every secret value a connector holds (for [`scrub`]).
fn secret_values(hub: &ConnectorHub, e: &ServerEntry, extra: &NewSecrets) -> Vec<String> {
    let store = hub.secrets();
    let mut out: Vec<String> = fly_mcp_client::registry::secret_keys_for(e)
        .iter()
        .filter_map(|k| store.get(k).ok().flatten())
        .collect();
    out.extend(extra.values.values().cloned());
    out.extend(extra.api_key.clone());
    out
}

/// A person-readable error. Program output goes after a `\n---\n` line, with
/// secrets scrubbed (the UI shows it under "Show details").
fn describe(e: HubError, secrets: &[String]) -> String {
    match e {
        HubError::Session(SessionError::ProgramNotFound(p)) => {
            let file = p
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&p)
                .to_ascii_lowercase();
            if file.starts_with("npx") {
                "Couldn't find npx. Install Node.js and try again.".into()
            } else if file.starts_with("uvx") {
                "Couldn't find uvx. Install uv (Python) and try again.".into()
            } else {
                format!(
                    "Couldn't find {}. Check the program.",
                    clean_capped(&p, 200)
                )
            }
        }
        HubError::Session(SessionError::Exited(tail)) => {
            let tail = scrub(&tail, secrets);
            let chars: Vec<char> = tail.chars().collect();
            let start = chars.len().saturating_sub(DETAILS_CHARS);
            let end: String = chars[start..].iter().collect();
            format!(
                "The program stopped right away.\n---\n{}",
                clean_capped(&end, DETAILS_CHARS)
            )
        }
        HubError::Session(SessionError::AuthRequired) => "Sign in to this connector first.".into(),
        HubError::Session(SessionError::Timeout) => "The connector took too long to answer.".into(),
        HubError::Session(SessionError::Connect(_) | SessionError::Listing(_)) => {
            "That address didn't answer like a connector.".into()
        }
        other => scrub(&other.to_string(), secrets),
    }
}

#[tauri::command]
pub fn connectors_overview(app: AppHandle, st: State<'_, ConnectorsState>) -> Overview {
    let reg = st.hub.registry();
    Overview {
        beta: beta_enabled(&app),
        connectors: reg.servers.iter().map(|e| view(&st.hub, e)).collect(),
    }
}

#[tauri::command]
pub fn connectors_directory() -> Vec<DirectoryEntry> {
    directory::directory()
}

/// What the Add forms send: the configuration plus secret values bound for
/// the keychain.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorInput {
    config: ServerConfig,
    #[serde(default)]
    secrets: NewSecrets,
}

/// Look at a web connector before it's added ("Checking…"): its tools, or
/// that it wants a browser sign-in. Local programs are never started here.
#[tauri::command]
pub async fn probe_connector(
    st: State<'_, ConnectorsState>,
    input: ConnectorInput,
) -> CmdResult<ProbeResult> {
    let secrets: Vec<String> = input
        .secrets
        .values
        .values()
        .cloned()
        .chain(input.secrets.api_key.clone())
        .collect();
    st.hub
        .probe(input.config, &input.secrets)
        .await
        .map_err(|e| describe(e, &secrets))
}

/// After adding (or editing): list tools now when no browser sign-in is
/// needed. Failure leaves the connector saved and says why.
async fn first_sync(st: &ConnectorsState, id: &str, extra: &NewSecrets) -> Option<String> {
    let entry = st.hub.registry().server(id).cloned()?;
    if entry.config.uses_oauth() && !st.hub.signed_in(&entry) {
        return None;
    }
    match st.hub.sync_tools(id).await {
        Ok(_) => None,
        Err(e) => Some(describe(e, &secret_values(&st.hub, &entry, extra))),
    }
}

/// Add a connector after a yes in a native dialog. The stored approval is
/// bound to this exact configuration's hash; secret values go to the
/// keychain.
#[tauri::command]
pub async fn add_connector(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    input: ConnectorInput,
) -> CmdResult<SaveResult> {
    let cfg = st.hub.check_config(input.config).map_err(err_str)?;
    if st
        .hub
        .registry()
        .servers
        .iter()
        .any(|s| s.config.name.eq_ignore_ascii_case(&cfg.name))
    {
        return Err("A connector with that name already exists.".into());
    }
    if !ask(&st, &app, &add_prompt(&cfg, false)).await {
        return Ok(SaveResult {
            saved: false,
            connector: None,
            error: None,
        });
    }
    let id = st.hub.add_approved(cfg, &input.secrets).map_err(err_str)?;
    tracing::info!(connector = %id, "connector added");
    let error = first_sync(&st, &id, &input.secrets).await;
    Ok(SaveResult {
        saved: true,
        connector: Some(view_of(&st.hub, &id)?),
        error,
    })
}

/// Edit a connector after a yes in a native dialog (the approval moves to
/// the new configuration's hash). Empty secret values keep what's stored.
#[tauri::command]
pub async fn update_connector(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    id: String,
    input: ConnectorInput,
) -> CmdResult<SaveResult> {
    let cfg = st.hub.check_config(input.config).map_err(err_str)?;
    if !ask(&st, &app, &edit_prompt(&cfg)).await {
        return Ok(SaveResult {
            saved: false,
            connector: Some(view_of(&st.hub, &id)?),
            error: None,
        });
    }
    st.hub
        .update_approved(&id, cfg, &input.secrets)
        .await
        .map_err(err_str)?;
    let error = first_sync(&st, &id, &input.secrets).await;
    Ok(SaveResult {
        saved: true,
        connector: Some(view_of(&st.hub, &id)?),
        error,
    })
}

/// The connector's switch: off at once, on after a native dialog.
#[tauri::command]
pub async fn set_connector_enabled(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    id: String,
    enabled: bool,
) -> CmdResult<SaveResult> {
    if enabled {
        let entry = st
            .hub
            .registry()
            .server(&id)
            .cloned()
            .ok_or("No connector with that id.")?;
        if !entry.is_approved() {
            return Err("Edit this connector to approve it again.".into());
        }
        if !ask(&st, &app, &add_prompt(&entry.config, true)).await {
            return Ok(SaveResult {
                saved: false,
                connector: Some(view_of(&st.hub, &id)?),
                error: None,
            });
        }
    }
    st.hub.set_enabled(&id, enabled).await.map_err(err_str)?;
    Ok(SaveResult {
        saved: true,
        connector: Some(view_of(&st.hub, &id)?),
        error: None,
    })
}

/// Remove a connector, its keychain entries and its connection.
#[tauri::command]
pub async fn remove_connector(st: State<'_, ConnectorsState>, id: String) -> CmdResult<()> {
    st.hub.remove(&id).await.map_err(err_str)?;
    tracing::info!(connector = %id, "connector removed");
    Ok(())
}

/// Connect and refresh the tool list. Changed tools are blocked with a diff;
/// tools added since approval are blocked until reviewed.
#[tauri::command]
pub async fn sync_connector_tools(
    st: State<'_, ConnectorsState>,
    id: String,
) -> CmdResult<ConnectorView> {
    let entry = st
        .hub
        .registry()
        .server(&id)
        .cloned()
        .ok_or("No connector with that id.")?;
    st.hub
        .sync_tools(&id)
        .await
        .map_err(|e| describe(e, &secret_values(&st.hub, &entry, &NewSecrets::default())))?;
    view_of(&st.hub, &id)
}

/// Set tool permissions. Tightening applies at once; loosening needs a yes
/// in a native dialog (`saved: false` when declined; tightening in the same
/// request still applies).
#[tauri::command]
pub async fn set_connector_tool_permissions(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    id: String,
    changes: Vec<PermissionChange>,
) -> CmdResult<SaveResult> {
    if changes.len() > 512 {
        return Err("Too many changes at once.".into());
    }
    let plan = st.hub.plan_permissions(&id, &changes).map_err(err_str)?;
    st.hub.apply_tightening(&id, &changes).map_err(err_str)?;
    let mut saved = true;
    if plan.grants_trust() {
        let reg = st.hub.registry();
        let entry = reg.server(&id).ok_or("No connector with that id.")?;
        if ask(&st, &app, &loosen_prompt(entry, &plan.loosen)).await {
            st.hub
                .apply_loosening(&id, &changes, &plan.loosen)
                .map_err(err_str)?;
        } else {
            saved = false;
        }
    }
    Ok(SaveResult {
        saved,
        connector: Some(view_of(&st.hub, &id)?),
        error: None,
    })
}

/// Approve a changed tool (or one added since approval) after a yes in a
/// native dialog. `review_hash` is the definition the user reviewed; a newer
/// one is refused. The tool goes to Needs approval.
#[tauri::command]
pub async fn approve_connector_tool(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    id: String,
    tool: String,
    review_hash: String,
) -> CmdResult<SaveResult> {
    let reg = st.hub.registry();
    let entry = reg.server(&id).ok_or("No connector with that id.")?;
    let pin = entry.tool(&tool).ok_or("No such tool.")?;
    if pin::review_hash(pin) != Some(review_hash.as_str()) {
        return Err("The tool changed again; review the latest change.".into());
    }
    if !ask(&st, &app, &review_prompt(entry, pin)).await {
        return Ok(SaveResult {
            saved: false,
            connector: Some(view_of(&st.hub, &id)?),
            error: None,
        });
    }
    st.hub
        .accept_change(&id, &tool, &review_hash)
        .map_err(err_str)?;
    Ok(SaveResult {
        saved: true,
        connector: Some(view_of(&st.hub, &id)?),
        error: None,
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

/// Sign in in the system browser (the user signs in themselves), store the
/// tokens in the keychain, then list tools. Waits up to five minutes.
#[tauri::command]
pub async fn sign_in_connector(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    id: String,
) -> CmdResult<SaveResult> {
    st.hub
        .sign_in(&id, &SystemBrowser { app: app.clone() })
        .await
        .map_err(|e| match e {
            HubError::OAuth(fly_mcp_client::oauth::OAuthError::Denied(_)) => {
                "Sign-in was cancelled.".to_string()
            }
            other => err_str(other),
        })?;
    let error = first_sync(&st, &id, &NewSecrets::default()).await;
    Ok(SaveResult {
        saved: true,
        connector: Some(view_of(&st.hub, &id)?),
        error,
    })
}

#[tauri::command]
pub async fn sign_out_connector(
    st: State<'_, ConnectorsState>,
    id: String,
) -> CmdResult<ConnectorView> {
    st.hub.sign_out(&id).await.map_err(err_str)?;
    view_of(&st.hub, &id)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeImport {
    found: bool,
    candidates: Vec<ImportCandidate>,
}

/// Where Claude Desktop keeps its config: the standard location, plus the
/// Windows Store (MSIX) one. Read only, never written.
fn claude_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(d) = dirs::config_dir() {
        paths.push(d.join("Claude").join("claude_desktop_config.json"));
    }
    #[cfg(windows)]
    if let Some(local) = dirs::data_local_dir() {
        if let Ok(rd) = std::fs::read_dir(local.join("Packages")) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("Claude_") {
                    paths.push(
                        e.path()
                            .join("LocalCache")
                            .join("Roaming")
                            .join("Claude")
                            .join("claude_desktop_config.json"),
                    );
                }
            }
        }
    }
    paths
}

/// The most recently changed Claude Desktop config, if any.
fn read_claude_config() -> Option<String> {
    let newest = claude_config_paths()
        .into_iter()
        .filter_map(|p| {
            let m = std::fs::metadata(&p).ok()?;
            (m.is_file() && m.len() as usize <= claude_import::MAX_CONFIG_BYTES)
                .then(|| (m.modified().ok(), p))
        })
        .max_by_key(|(t, _)| *t)?;
    std::fs::read_to_string(newest.1).ok()
}

/// Servers in Claude Desktop's config (names only; values stay in the file
/// until a server is imported).
#[tauri::command]
pub async fn claude_desktop_candidates() -> CmdResult<ClaudeImport> {
    let read = tauri::async_runtime::spawn_blocking(read_claude_config)
        .await
        .map_err(err_str)?;
    Ok(match read {
        Some(json) => ClaudeImport {
            found: true,
            candidates: claude_import::candidates(&json),
        },
        None => ClaudeImport {
            found: false,
            candidates: Vec::new(),
        },
    })
}

/// Import one server from Claude Desktop's config after a yes in a native
/// dialog. Its settings move to the keychain; permissions aren't imported.
#[tauri::command]
pub async fn import_claude_desktop_connector(
    app: AppHandle,
    st: State<'_, ConnectorsState>,
    key: String,
) -> CmdResult<SaveResult> {
    let json = tauri::async_runtime::spawn_blocking(read_claude_config)
        .await
        .map_err(err_str)?
        .ok_or("No Claude Desktop setup found on this computer.")?;
    let (mut cfg, secrets) = claude_import::to_config(&json, &key)
        .ok_or("That connector isn't in Claude Desktop's setup.")?;
    // A name already in use gets a suffix rather than failing.
    let taken = |n: &str| {
        st.hub
            .registry()
            .servers
            .iter()
            .any(|s| s.config.name.eq_ignore_ascii_case(n))
    };
    if taken(&cfg.name) {
        let base: String = cfg.name.chars().take(30).collect();
        cfg.name = (2..100)
            .map(|n| format!("{base} ({n})"))
            .find(|n| !taken(n))
            .unwrap_or(base);
    }
    let cfg = st.hub.check_config(cfg).map_err(err_str)?;
    if !ask(&st, &app, &add_prompt(&cfg, false)).await {
        return Ok(SaveResult {
            saved: false,
            connector: None,
            error: None,
        });
    }
    let id = st.hub.add_approved(cfg, &secrets).map_err(err_str)?;
    tracing::info!(connector = %id, "connector imported from Claude Desktop");
    let error = first_sync(&st, &id, &secrets).await;
    Ok(SaveResult {
        saved: true,
        connector: Some(view_of(&st.hub, &id)?),
        error,
    })
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

/// Try one tool from Settings. Goes through the same rules as an Ask turn,
/// including the confirm card for tools on Needs approval.
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
        return Err("This tool is blocked or waiting for review.".into());
    }
    Ok(match turn.call(&name, args).await {
        Ok(text) => TestResult { ok: true, text },
        Err(text) => TestResult { ok: false, text },
    })
}

/// "Choose a file…" for a local connector's program: a native file picker
/// opened by the backend (the webview has no file-dialog access). The path
/// only becomes a connector after the native "Run a program" approval.
#[tauri::command]
pub async fn pick_connector_program(app: AppHandle) -> CmdResult<Option<String>> {
    pick_file(&app).await
}

#[cfg(any(windows, target_os = "macos"))]
async fn pick_file(app: &AppHandle) -> CmdResult<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let mut dialog = rfd::AsyncFileDialog::new().set_title("Choose the connector program");
        if let Some(window) = handle.get_webview_window("main") {
            dialog = dialog.set_parent(&window);
        }
        let picked = dialog.pick_file();
        std::thread::spawn(move || {
            let file = tauri::async_runtime::block_on(picked);
            let _ = tx.send(file.map(|f| f.path().display().to_string()));
        });
    })
    .map_err(err_str)?;
    Ok(rx.await.unwrap_or(None))
}

#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
async fn pick_file(_app: &AppHandle) -> CmdResult<Option<String>> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Choose the connector program")
            .pick_file()
            .map(|p| p.display().to_string())
    })
    .await
    .map_err(err_str)
}

#[cfg(not(any(
    windows,
    target_os = "macos",
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
async fn pick_file(_app: &AppHandle) -> CmdResult<Option<String>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_mcp_client::config::EnvConfig;

    #[test]
    fn connector_state_is_backend_only() {
        for key in [
            REGISTRY_KEY,
            CALL_LOG_KEY,
            "connectors.anything",
            "Connectors.Registry",
        ] {
            assert!(!crate::commands::is_webview_setting_key(key), "{key}");
            assert!(
                !crate::commands::is_webview_writable_setting_key(key),
                "{key}"
            );
        }
        // The beta switch is a UI preference and grants nothing.
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
                        headers: vec![],
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
        assert!(keys.contains(&format!("connector.{id}.apikey")));
    }

    fn proposal(kind: ToolKind, destructive: bool) -> CallProposal {
        CallProposal {
            proposal_id: "p1".into(),
            turn_id: "t".into(),
            server_id: "c1".into(),
            server_name: "Acme\u{202E} CRM".into(),
            destination: "mcp.example.com".into(),
            tool: "delete_contact".into(),
            tool_title: None,
            kind,
            destructive,
            args: serde_json::json!({"id": "c-1"}),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn native_dialogs_use_the_brief_copy_and_safe_defaults() {
        let p = proposal(ToolKind::Write, true);
        let text = dialog_args(&p.args).unwrap();
        let send = send_prompt(&p, &text);
        assert_eq!(send.title, "Send this to \"Acme CRM\"?");
        assert!(send.message.contains("delete or overwrite"));
        assert!(send.message.contains("\"id\": \"c-1\""));
        assert_eq!(send.decline, "Don't send");

        let local = ServerConfig {
            name: "Docs".into(),
            transport: TransportConfig::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "@example/docs".into()],
                env: vec![EnvConfig {
                    name: "DOCS_TOKEN".into(),
                    value: None,
                }],
            },
            auth: AuthConfig::None,
        };
        let add = add_prompt(&local, false);
        assert_eq!(add.title, "Run a program on this computer?");
        assert!(add.message.contains("will run: npx -y @example/docs"));
        assert!(add.message.contains("DOCS_TOKEN"));
        assert_eq!(
            (add.allow.as_str(), add.decline.as_str()),
            ("Run and add", "Cancel")
        );

        let web = ServerConfig {
            name: "Docs".into(),
            transport: TransportConfig::Http {
                url: "https://mcp.example.com/mcp".into(),
                headers: vec![],
            },
            auth: AuthConfig::None,
        };
        let add = add_prompt(&web, false);
        assert_eq!(add.title, "Add \"Docs\" connector?");
        assert!(add.message.contains("sent to mcp.example.com"));
    }

    #[test]
    fn arguments_too_long_for_the_dialog_are_refused_not_cut() {
        let big = serde_json::json!({"body": "x".repeat(DIALOG_ARGS_CHARS)});
        assert!(dialog_args(&big).is_err());
    }

    #[test]
    fn program_output_is_scrubbed_of_secrets() {
        let err = HubError::Session(SessionError::Exited(
            "starting\nfatal: token sk-live-1234 rejected".into(),
        ));
        let msg = describe(err, &["sk-live-1234".into()]);
        assert!(msg.starts_with("The program stopped right away.\n---\n"));
        assert!(
            !msg.contains("sk-live-1234") && msg.contains("••••"),
            "{msg}"
        );
        let npx = describe(
            HubError::Session(SessionError::ProgramNotFound("npx".into())),
            &[],
        );
        assert_eq!(npx, "Couldn't find npx. Install Node.js and try again.");
    }
}
