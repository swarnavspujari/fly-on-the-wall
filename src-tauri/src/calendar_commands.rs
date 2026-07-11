//! Calendar commands: BYO OAuth app credentials, connect via system
//! browser (PKCE + loopback), upcoming meetings, one-click start.

use std::sync::Arc;

use chrono::{Duration, Utc};
use looma_calendar::google::GoogleCalendarProvider;
use looma_calendar::msgraph::MsGraphProvider;
use looma_calendar::{CalendarEvent, CalendarProvider};
use serde::{Deserialize, Serialize};
use tauri::State;
use tauri_plugin_opener::OpenerExt;

use crate::recording::{self, RecordingStatus};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Google installed-app client secrets are distributed with the app config,
/// but we still keep them out of plaintext storage.
pub(crate) const GOOGLE_CLIENT_SECRET_KEY: &str = "google_oauth_client_secret";

fn open_url_fn(app: &tauri::AppHandle) -> looma_calendar::google::OpenUrl {
    let app = app.clone();
    Arc::new(move |url: String| {
        let _ = app.opener().open_url(url, None::<&str>);
    })
}

fn build_google(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<GoogleCalendarProvider, String> {
    let storage = state.storage.lock().unwrap();
    let client_id = storage
        .get_setting("calendar.google.client_id")
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .ok_or("Google Calendar is not configured — add a client ID in Settings")?;
    drop(storage);
    let client_secret = state
        .secrets
        .get(GOOGLE_CLIENT_SECRET_KEY)
        .map_err(|e| e.to_string())?
        .ok_or("Google Calendar needs a client secret — add it in Settings")?;
    Ok(GoogleCalendarProvider {
        client_id,
        client_secret,
        secrets: state.secrets_arc(),
        open_url: open_url_fn(app),
    })
}

fn build_msgraph(app: &tauri::AppHandle, state: &AppState) -> Result<MsGraphProvider, String> {
    let storage = state.storage.lock().unwrap();
    let client_id = storage
        .get_setting("calendar.ms.client_id")
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .ok_or("Microsoft 365 is not configured — add an application (client) ID in Settings")?;
    drop(storage);
    Ok(MsGraphProvider {
        client_id,
        secrets: state.secrets_arc(),
        open_url: open_url_fn(app),
    })
}

#[derive(Serialize)]
pub struct CalendarStatus {
    pub google_client_id: String,
    pub google_has_secret: bool,
    pub google_connected: bool,
    pub ms_client_id: String,
    pub ms_connected: bool,
}

#[tauri::command]
pub fn get_calendar_settings(state: State<'_, AppState>) -> CmdResult<CalendarStatus> {
    let storage = state.storage.lock().unwrap();
    let get = |k: &str| storage.get_setting(k).ok().flatten().unwrap_or_default();
    let google_client_id = get("calendar.google.client_id");
    let ms_client_id = get("calendar.ms.client_id");
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
            .get(looma_secrets::keys::GOOGLE_OAUTH_TOKEN)
            .ok()
            .flatten()
            .is_some(),
        ms_client_id,
        ms_connected: state
            .secrets
            .get(looma_secrets::keys::MS_OAUTH_TOKEN)
            .ok()
            .flatten()
            .is_some(),
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

/// Events in the next 24 h across all connected calendars, sorted by start.
#[tauri::command]
pub async fn upcoming_meetings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Vec<CalendarEvent>> {
    let from = Utc::now() - Duration::minutes(30); // include in-progress meetings
    let to = Utc::now() + Duration::hours(24);
    let mut events = Vec::new();

    let google_connected = state
        .secrets
        .get(looma_secrets::keys::GOOGLE_OAUTH_TOKEN)
        .ok()
        .flatten()
        .is_some();
    if google_connected {
        match build_google(&app, &state) {
            Ok(p) => match p.upcoming(from, to).await {
                Ok(mut ev) => events.append(&mut ev),
                Err(e) => tracing::warn!("google calendar fetch failed: {e}"),
            },
            Err(e) => tracing::warn!("google calendar not buildable: {e}"),
        }
    }
    let ms_connected = state
        .secrets
        .get(looma_secrets::keys::MS_OAUTH_TOKEN)
        .ok()
        .flatten()
        .is_some();
    if ms_connected {
        match build_msgraph(&app, &state) {
            Ok(p) => match p.upcoming(from, to).await {
                Ok(mut ev) => events.append(&mut ev),
                Err(e) => tracing::warn!("microsoft calendar fetch failed: {e}"),
            },
            Err(e) => tracing::warn!("microsoft calendar not buildable: {e}"),
        }
    }

    events.sort_by_key(|e| e.start);
    Ok(events)
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
