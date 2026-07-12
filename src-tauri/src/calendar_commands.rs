//! Calendar commands: BYO OAuth app credentials, connect via system
//! browser (PKCE + loopback), upcoming meetings, one-click start.

use std::sync::Arc;

use chrono::{Duration, Utc};
use fly_calendar::google::GoogleCalendarProvider;
use fly_calendar::msgraph::MsGraphProvider;
use fly_calendar::{CalendarEvent, CalendarProvider};
use fly_secrets::SecretStore;
use fly_storage::Storage;
use serde::{Deserialize, Serialize};
use tauri::State;
use tauri_plugin_opener::OpenerExt;

use crate::calendar_defaults;
use crate::recording::{self, RecordingStatus};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Google installed-app client secrets are distributed with the app config,
/// but we still keep them out of plaintext storage.
pub(crate) const GOOGLE_CLIENT_SECRET_KEY: &str = "google_oauth_client_secret";

/// Settings keys holding the JSON array of calendar ids the user toggled off.
const GOOGLE_DISABLED_KEY: &str = "calendar.google.disabled_ids";
const MS_DISABLED_KEY: &str = "calendar.ms.disabled_ids";

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// Load order per provider: the user's own value (settings/keychain) wins;
/// otherwise fall back to the bundled default. `None` only when neither exists.
fn effective_google_client_id(storage: &Storage) -> Option<String> {
    storage
        .get_setting("calendar.google.client_id")
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .or_else(|| non_empty(calendar_defaults::GOOGLE_CLIENT_ID))
}

fn effective_google_secret(secrets: &dyn SecretStore) -> Option<String> {
    secrets
        .get(GOOGLE_CLIENT_SECRET_KEY)
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .or_else(|| non_empty(calendar_defaults::GOOGLE_CLIENT_SECRET))
}

fn effective_ms_client_id(storage: &Storage) -> Option<String> {
    storage
        .get_setting("calendar.ms.client_id")
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .or_else(|| non_empty(calendar_defaults::MS_CLIENT_ID))
}

/// The calendar ids the user has toggled off for a provider (empty = all on).
fn read_disabled(storage: &Storage, key: &str) -> Vec<String> {
    storage
        .get_setting(key)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
}

fn open_url_fn(app: &tauri::AppHandle) -> fly_calendar::google::OpenUrl {
    let app = app.clone();
    Arc::new(move |url: String| {
        let _ = app.opener().open_url(url, None::<&str>);
    })
}

fn build_google(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<GoogleCalendarProvider, String> {
    let (client_id, disabled_calendars) = {
        let storage = state.storage.lock().unwrap();
        let id = effective_google_client_id(&storage)
            .ok_or("Google Calendar is not configured — add a client ID in Settings")?;
        (id, read_disabled(&storage, GOOGLE_DISABLED_KEY))
    };
    let client_secret = effective_google_secret(state.secrets.as_ref())
        .ok_or("Google Calendar needs a client secret — add it in Settings")?;
    Ok(GoogleCalendarProvider {
        client_id,
        client_secret,
        secrets: state.secrets_arc(),
        open_url: open_url_fn(app),
        disabled_calendars,
    })
}

fn build_msgraph(app: &tauri::AppHandle, state: &AppState) -> Result<MsGraphProvider, String> {
    let (client_id, disabled_calendars) = {
        let storage = state.storage.lock().unwrap();
        let id = effective_ms_client_id(&storage)
            .ok_or("Microsoft 365 is not configured — add an application (client) ID in Settings")?;
        (id, read_disabled(&storage, MS_DISABLED_KEY))
    };
    Ok(MsGraphProvider {
        client_id,
        secrets: state.secrets_arc(),
        open_url: open_url_fn(app),
        disabled_calendars,
    })
}

#[derive(Serialize)]
pub struct CalendarStatus {
    /// The user's own client id (empty when relying on the bundled default).
    pub google_client_id: String,
    /// The user's own client secret is stored (bundled default not reflected).
    pub google_has_secret: bool,
    pub google_connected: bool,
    /// Connectable — a usable client id + secret exists (user-supplied OR
    /// bundled). The UI enables "Connect" on this, not on the BYO fields.
    pub google_configured: bool,
    pub ms_client_id: String,
    pub ms_connected: bool,
    pub ms_configured: bool,
}

#[tauri::command]
pub fn get_calendar_settings(state: State<'_, AppState>) -> CmdResult<CalendarStatus> {
    let storage = state.storage.lock().unwrap();
    let get = |k: &str| storage.get_setting(k).ok().flatten().unwrap_or_default();
    let google_client_id = get("calendar.google.client_id");
    let ms_client_id = get("calendar.ms.client_id");
    let google_configured = effective_google_client_id(&storage).is_some()
        && effective_google_secret(state.secrets.as_ref()).is_some();
    let ms_configured = effective_ms_client_id(&storage).is_some();
    drop(storage);
    Ok(CalendarStatus {
        google_client_id,
        google_has_secret: state
            .secrets
            .get(GOOGLE_CLIENT_SECRET_KEY)
            .ok()
            .flatten()
            .is_some(),
        google_connected: state
            .secrets
            .get(fly_secrets::keys::GOOGLE_OAUTH_TOKEN)
            .ok()
            .flatten()
            .is_some(),
        google_configured,
        ms_client_id,
        ms_connected: state
            .secrets
            .get(fly_secrets::keys::MS_OAUTH_TOKEN)
            .ok()
            .flatten()
            .is_some(),
        ms_configured,
    })
}

#[derive(Deserialize)]
pub struct CalendarSettingsUpdate {
    pub google_client_id: String,
    /// Some("") clears; None untouched.
    pub google_client_secret: Option<String>,
    pub ms_client_id: String,
}

#[tauri::command]
pub fn set_calendar_settings(
    state: State<'_, AppState>,
    update: CalendarSettingsUpdate,
) -> CmdResult<()> {
    {
        let storage = state.storage.lock().unwrap();
        storage
            .set_setting("calendar.google.client_id", &update.google_client_id)
            .map_err(|e| e.to_string())?;
        storage
            .set_setting("calendar.ms.client_id", &update.ms_client_id)
            .map_err(|e| e.to_string())?;
    }
    if let Some(secret) = update.google_client_secret {
        if secret.is_empty() {
            state
                .secrets
                .delete(GOOGLE_CLIENT_SECRET_KEY)
                .map_err(|e| e.to_string())?;
        } else {
            state
                .secrets
                .set(GOOGLE_CLIENT_SECRET_KEY, &secret)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Run the interactive OAuth flow (opens the system browser; returns when
/// the user finishes or after a 5-minute timeout).
#[tauri::command]
pub async fn connect_calendar(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    provider: String,
) -> CmdResult<()> {
    match provider.as_str() {
        "google" => {
            let p = build_google(&app, &state)?;
            p.connect().await.map_err(|e| e.to_string())
        }
        "msgraph" => {
            let p = build_msgraph(&app, &state)?;
            p.connect().await.map_err(|e| e.to_string())
        }
        other => Err(format!("unknown calendar provider {other}")),
    }
}

#[tauri::command]
pub async fn disconnect_calendar(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    provider: String,
) -> CmdResult<()> {
    match provider.as_str() {
        "google" => build_google(&app, &state)?
            .disconnect()
            .await
            .map_err(|e| e.to_string()),
        "msgraph" => build_msgraph(&app, &state)?
            .disconnect()
            .await
            .map_err(|e| e.to_string()),
        other => Err(format!("unknown calendar provider {other}")),
    }
}

fn is_connected(state: &AppState, token_key: &str) -> bool {
    state.secrets.get(token_key).ok().flatten().is_some()
}

/// Events in the next 24 h across every enabled calendar of every connected
/// provider. Events with no join link are filtered out here (server-side,
/// before the sort); the result is de-duped and sorted by start.
#[tauri::command]
pub async fn upcoming_meetings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Vec<CalendarEvent>> {
    let from = Utc::now() - Duration::minutes(30); // include in-progress meetings
    let to = Utc::now() + Duration::hours(24);
    let mut events = Vec::new();

    if is_connected(&state, fly_secrets::keys::GOOGLE_OAUTH_TOKEN) {
        match build_google(&app, &state) {
            Ok(p) => match p.upcoming(from, to).await {
                Ok(mut ev) => events.append(&mut ev),
                Err(e) => tracing::warn!("google calendar fetch failed: {e}"),
            },
            Err(e) => tracing::warn!("google calendar not buildable: {e}"),
        }
    }
    if is_connected(&state, fly_secrets::keys::MS_OAUTH_TOKEN) {
        match build_msgraph(&app, &state) {
            Ok(p) => match p.upcoming(from, to).await {
                Ok(mut ev) => events.append(&mut ev),
                Err(e) => tracing::warn!("microsoft calendar fetch failed: {e}"),
            },
            Err(e) => tracing::warn!("microsoft calendar not buildable: {e}"),
        }
    }

    // Drop link-less events, de-dupe, sort by start.
    Ok(fly_calendar::merge_upcoming(events))
}

/// One of the user's calendars plus its on/off state, for the settings list.
#[derive(Serialize)]
pub struct CalendarToggle {
    /// "google" | "msgraph".
    pub provider: String,
    pub id: String,
    pub name: String,
    pub primary: bool,
    pub enabled: bool,
}

/// List every calendar of every connected provider, tagged with whether it is
/// currently included in "Up next". A provider that fails to list is skipped
/// (logged) rather than failing the whole call.
#[tauri::command]
pub async fn list_calendars(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Vec<CalendarToggle>> {
    let mut out = Vec::new();

    if is_connected(&state, fly_secrets::keys::GOOGLE_OAUTH_TOKEN) {
        if let Ok(p) = build_google(&app, &state) {
            let disabled = p.disabled_calendars.clone();
            match p.list_calendars().await {
                Ok(cals) => out.extend(cals.into_iter().map(|c| CalendarToggle {
                    provider: "google".into(),
                    enabled: !disabled.iter().any(|d| d == &c.id),
                    id: c.id,
                    name: c.name,
                    primary: c.primary,
                })),
                Err(e) => tracing::warn!("google list_calendars failed: {e}"),
            }
        }
    }
    if is_connected(&state, fly_secrets::keys::MS_OAUTH_TOKEN) {
        if let Ok(p) = build_msgraph(&app, &state) {
            let disabled = p.disabled_calendars.clone();
            match p.list_calendars().await {
                Ok(cals) => out.extend(cals.into_iter().map(|c| CalendarToggle {
                    provider: "msgraph".into(),
                    enabled: !disabled.iter().any(|d| d == &c.id),
                    id: c.id,
                    name: c.name,
                    primary: c.primary,
                })),
                Err(e) => tracing::warn!("microsoft list_calendars failed: {e}"),
            }
        }
    }

    Ok(out)
}

/// Toggle a single calendar on/off. Persists the change to the provider's
/// disabled-ids set; the next `upcoming_meetings`/`list_calendars` reflects it.
#[tauri::command]
pub fn set_calendar_enabled(
    state: State<'_, AppState>,
    provider: String,
    calendar_id: String,
    enabled: bool,
) -> CmdResult<()> {
    let key = match provider.as_str() {
        "google" => GOOGLE_DISABLED_KEY,
        "msgraph" => MS_DISABLED_KEY,
        other => return Err(format!("unknown calendar provider {other}")),
    };
    let storage = state.storage.lock().unwrap();
    let mut disabled = read_disabled(&storage, key);
    if enabled {
        disabled.retain(|d| d != &calendar_id);
    } else if !disabled.iter().any(|d| d == &calendar_id) {
        disabled.push(calendar_id);
    }
    let json = serde_json::to_string(&disabled).unwrap_or_else(|_| "[]".into());
    storage.set_setting(key, &json).map_err(|e| e.to_string())
}

/// One-click start from a calendar event: note titled after the event,
/// attendees prefilled, recording begins immediately.
#[tauri::command]
pub fn start_meeting_from_event(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    title: String,
    attendees: Vec<String>,
) -> CmdResult<RecordingStatus> {
    recording::start_recording_impl(&app, &state, None, Some(title), &attendees)
}
