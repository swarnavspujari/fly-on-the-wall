//! Tauri commands: the entire surface the frontend can call.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

#[derive(Serialize)]
pub struct AppInfo {
    pub version: String,
    pub data_dir: String,
}

/// Smoke-test command: the M0 frontend calls this to prove IPC works.
#[tauri::command]
pub fn ping() -> String {
    "pong".to_string()
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        data_dir: state.data_dir.display().to_string(),
    }
}
