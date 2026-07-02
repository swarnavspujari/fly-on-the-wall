//! Tauri commands: the entire surface the frontend can call.

use looma_core::{Folder, Note};
use looma_storage::{NoteSummary, SearchHit};
use serde::Serialize;
use tauri::State;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::state::AppState;

/// Commands surface errors to the UI as strings; details stay in the log.
type CmdResult<T> = Result<T, String>;

fn err_str<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[derive(Serialize)]
pub struct AppInfo {
    pub version: String,
    pub data_dir: String,
}

/// Smoke-test command: proves IPC works.
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

/// Generic non-secret app settings (consent flags, UI prefs). Secrets never
/// go through here — they belong to looma-secrets.
#[tauri::command]
pub fn get_app_setting(state: State<'_, AppState>, key: String) -> CmdResult<Option<String>> {
    state
        .storage
        .lock()
        .unwrap()
        .get_setting(&key)
        .map_err(err_str)
}

#[tauri::command]
pub fn set_app_setting(state: State<'_, AppState>, key: String, value: String) -> CmdResult<()> {
    state
        .storage
        .lock()
        .unwrap()
        .set_setting(&key, &value)
        .map_err(err_str)
}

// ---------------------------------------------------------------------------
// Folders
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_folders(state: State<'_, AppState>) -> CmdResult<Vec<Folder>> {
    state
        .storage
        .lock()
        .unwrap()
        .list_folders()
        .map_err(err_str)
}

#[tauri::command]
pub fn create_folder(
    state: State<'_, AppState>,
    name: String,
    parent_id: Option<String>,
) -> CmdResult<Folder> {
    state
        .storage
        .lock()
        .unwrap()
        .create_folder(&name, parent_id.as_deref())
        .map_err(err_str)
}

#[tauri::command]
pub fn rename_folder(state: State<'_, AppState>, id: String, name: String) -> CmdResult<()> {
    state
        .storage
        .lock()
        .unwrap()
        .rename_folder(&id, &name)
        .map_err(err_str)
}

#[tauri::command]
pub fn move_folder(
    state: State<'_, AppState>,
    id: String,
    parent_id: Option<String>,
) -> CmdResult<()> {
    state
        .storage
        .lock()
        .unwrap()
        .move_folder(&id, parent_id.as_deref())
        .map_err(err_str)
}

#[tauri::command]
pub fn delete_folder(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state
        .storage
        .lock()
        .unwrap()
        .delete_folder(&id)
        .map_err(err_str)
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn create_note(
    state: State<'_, AppState>,
    title: String,
    folder_id: Option<String>,
) -> CmdResult<Note> {
    state
        .storage
        .lock()
        .unwrap()
        .create_note(&title, folder_id.as_deref())
        .map_err(err_str)
}

#[tauri::command]
pub fn get_note(state: State<'_, AppState>, id: String) -> CmdResult<Note> {
    state.storage.lock().unwrap().get_note(&id).map_err(err_str)
}

#[tauri::command]
pub fn list_notes_in_folder(
    state: State<'_, AppState>,
    folder_id: Option<String>,
) -> CmdResult<Vec<NoteSummary>> {
    state
        .storage
        .lock()
        .unwrap()
        .list_notes_in_folder(folder_id.as_deref())
        .map_err(err_str)
}

#[tauri::command]
pub fn list_recent_notes(state: State<'_, AppState>, limit: usize) -> CmdResult<Vec<NoteSummary>> {
    state
        .storage
        .lock()
        .unwrap()
        .list_recent_notes(limit)
        .map_err(err_str)
}

#[tauri::command]
pub fn update_note_title(state: State<'_, AppState>, id: String, title: String) -> CmdResult<Note> {
    state
        .storage
        .lock()
        .unwrap()
        .update_note_title(&id, &title)
        .map_err(err_str)
}

#[tauri::command]
pub fn update_note_scratchpad(
    state: State<'_, AppState>,
    id: String,
    scratchpad: String,
) -> CmdResult<Note> {
    state
        .storage
        .lock()
        .unwrap()
        .update_note_scratchpad(&id, &scratchpad)
        .map_err(err_str)
}

#[tauri::command]
pub fn move_note(
    state: State<'_, AppState>,
    id: String,
    folder_id: Option<String>,
) -> CmdResult<()> {
    state
        .storage
        .lock()
        .unwrap()
        .move_note(&id, folder_id.as_deref())
        .map_err(err_str)
}

#[tauri::command]
pub fn delete_note(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state
        .storage
        .lock()
        .unwrap()
        .delete_note(&id)
        .map_err(err_str)
}

// ---------------------------------------------------------------------------
// Attachments & files
// ---------------------------------------------------------------------------

/// Open a native file picker and attach the chosen file. Returns the updated
/// note, or None if the user cancelled. Async so the blocking dialog never
/// runs on the main thread.
#[tauri::command]
pub async fn attach_file(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    note_id: String,
) -> CmdResult<Option<Note>> {
    let Some(picked) = app.dialog().file().blocking_pick_file() else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(err_str)?;
    let note = state
        .storage
        .lock()
        .unwrap()
        .add_attachment(&note_id, &path)
        .map_err(err_str)?;
    Ok(Some(note))
}

/// Save-as copy of the note's plain-markdown mirror (provenance flattened
/// by the storage layer's exporter). Returns the chosen path, or None if
/// the user cancelled.
#[tauri::command]
pub async fn export_note(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    note_id: String,
) -> CmdResult<Option<String>> {
    let (suggested, src) = {
        let storage = state.storage.lock().unwrap();
        let note = storage.get_note(&note_id).map_err(err_str)?;
        let safe: String = note
            .title
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || " -_.".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        (
            format!("{}.md", safe.trim()),
            state.data_dir.join("notes").join(format!("{note_id}.md")),
        )
    };
    if !src.exists() {
        return Err("note has no markdown mirror yet — edit it once first".into());
    }
    let Some(picked) = app
        .dialog()
        .file()
        .set_file_name(&suggested)
        .add_filter("Markdown", &["md"])
        .blocking_save_file()
    else {
        return Ok(None);
    };
    let dest = picked.into_path().map_err(err_str)?;
    std::fs::copy(&src, &dest).map_err(err_str)?;
    Ok(Some(dest.display().to_string()))
}

#[tauri::command]
pub fn remove_attachment(
    state: State<'_, AppState>,
    note_id: String,
    attachment_id: String,
) -> CmdResult<Note> {
    state
        .storage
        .lock()
        .unwrap()
        .remove_attachment(&note_id, &attachment_id)
        .map_err(err_str)
}

#[tauri::command]
pub fn open_attachment(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    rel_path: String,
) -> CmdResult<()> {
    let abs = state.storage.lock().unwrap().attachment_abs_path(&rel_path);
    app.opener()
        .open_path(abs.display().to_string(), None::<&str>)
        .map_err(err_str)
}

#[tauri::command]
pub fn reveal_attachment(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    rel_path: String,
) -> CmdResult<()> {
    let abs = state.storage.lock().unwrap().attachment_abs_path(&rel_path);
    app.opener().reveal_item_in_dir(abs).map_err(err_str)
}

/// "Reveal in file explorer" for the whole data dir (spec §10 data ownership).
#[tauri::command]
pub fn reveal_data_dir(app: tauri::AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    app.opener()
        .reveal_item_in_dir(&state.data_dir)
        .map_err(err_str)
}

/// Claude Desktop / MCP client config snippet pointing at the looma-mcp
/// binary that ships next to the app executable.
#[tauri::command]
pub fn mcp_config() -> CmdResult<String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .ok_or("cannot resolve the app directory")?;
    let mcp = exe_dir.join(if cfg!(windows) {
        "looma-mcp.exe"
    } else {
        "looma-mcp"
    });
    serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "looma": { "command": mcp.display().to_string(), "args": [] }
        }
    }))
    .map_err(err_str)
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn search(state: State<'_, AppState>, query: String) -> CmdResult<Vec<SearchHit>> {
    state
        .storage
        .lock()
        .unwrap()
        .search(&query, 30)
        .map_err(err_str)
}
