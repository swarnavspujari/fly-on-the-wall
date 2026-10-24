//! Privacy controls (the code side of SOC 2 Confidentiality / Privacy):
//!
//! * per-meeting / per-folder **Keep local** — the commands here only read
//!   and write the flag; enforcement lives where providers are chosen
//!   (`llm_commands::build_provider`, `pipeline::run_with`);
//! * **audio retention** — a sweep on startup and every 24 h that deletes
//!   recording media older than the chosen period (transcripts and notes
//!   kept), through fly-storage's containment-guarded `delete_meeting_audio`;
//! * **Delete all my data** — wipes the database in place, deletes the
//!   keychain entries the app owns, and leaves a marker so the restart
//!   removes every file in the data folder before anything reopens it;
//! * **recording notice** helper (clipboard) and the participants-notified
//!   flag;
//! * a best-effort **disk encryption** probe (BitLocker / FileVault) that
//!   never needs elevation and says nothing when it can't tell.
//!
//! The retention setting is an internal key (`privacy.*`), written only by
//! the validating command below — never via the webview's generic settings
//! access, because it steers file deletes.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use fly_secrets::SecretStore;
use fly_storage::{MeetingPrivacy, Storage, RETENTION_CHOICES};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

fn err_str<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// Keep local + participants notified
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_meeting_privacy(
    state: State<'_, AppState>,
    meeting_id: String,
) -> CmdResult<MeetingPrivacy> {
    state
        .storage
        .lock()
        .unwrap()
        .get_meeting_privacy(&meeting_id)
        .map_err(err_str)
}

/// Turn "Keep local" on/off for every meeting of a note.
#[tauri::command]
pub fn set_note_keep_local(
    state: State<'_, AppState>,
    note_id: String,
    keep_local: bool,
) -> CmdResult<()> {
    let storage = state.storage.lock().unwrap();
    storage.get_note(&note_id).map_err(err_str)?;
    storage
        .set_note_keep_local(&note_id, keep_local)
        .map_err(err_str)?;
    tracing::info!(note_id, keep_local, "keep-local changed");
    Ok(())
}

#[tauri::command]
pub fn set_folder_keep_local(
    state: State<'_, AppState>,
    folder_id: String,
    keep_local: bool,
) -> CmdResult<()> {
    state
        .storage
        .lock()
        .unwrap()
        .set_folder_keep_local(&folder_id, keep_local)
        .map_err(err_str)
}

#[tauri::command]
pub fn list_local_only_folders(state: State<'_, AppState>) -> CmdResult<Vec<String>> {
    state
        .storage
        .lock()
        .unwrap()
        .local_only_folders()
        .map_err(err_str)
}

#[tauri::command]
pub fn set_participants_notified(
    state: State<'_, AppState>,
    meeting_id: String,
    notified: bool,
) -> CmdResult<()> {
    state
        .storage
        .lock()
        .unwrap()
        .set_participants_notified(&meeting_id, notified)
        .map_err(err_str)
}

/// Longest recording notice the clipboard helper accepts.
const MAX_NOTICE_LEN: usize = 2000;

/// Copy the recording notice to the clipboard (native write: the webview's
/// clipboard API is patchy across platforms).
#[tauri::command]
pub fn copy_recording_notice(app: tauri::AppHandle, text: String) -> CmdResult<()> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    if text.trim().is_empty() || text.len() > MAX_NOTICE_LEN {
        return Err("recording notice must be 1–2000 characters".into());
    }
    app.clipboard().write_text(text).map_err(err_str)
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

/// How often the retention sweep re-runs while the app is open.
const SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Let startup settle (self-heal, queue recovery) before the first sweep.
const FIRST_SWEEP_DELAY: Duration = Duration::from_secs(60);

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct SweepReport {
    pub meetings: usize,
    pub files_removed: usize,
    pub bytes_removed: u64,
    /// Meetings with at least one file that couldn't be deleted (locked);
    /// the next sweep retries them.
    pub meetings_failed: usize,
}

/// Delete audio of every finished meeting older than the configured period.
/// `busy` meetings (a pipeline or re-diarize holding their files) are left
/// for the next sweep. No-op when retention is off.
pub fn sweep_retention(
    storage: &Storage,
    now: chrono::DateTime<Utc>,
    busy: &dyn Fn(&str) -> bool,
) -> SweepReport {
    let mut report = SweepReport::default();
    let Some(days) = storage.audio_retention_days() else {
        return report;
    };
    let cutoff = now - chrono::Duration::days(i64::from(days));
    let ids = match storage.retention_candidates(cutoff) {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "retention: listing candidates failed");
            return report;
        }
    };
    for id in ids.iter().filter(|id| !busy(id)) {
        match storage.delete_meeting_audio(id) {
            Ok(p) => {
                report.files_removed += p.files_removed;
                report.bytes_removed += p.bytes_removed;
                if p.failed.is_empty() {
                    report.meetings += 1;
                } else {
                    report.meetings_failed += 1;
                }
            }
            Err(e) => {
                report.meetings_failed += 1;
                tracing::warn!(meeting_id = %id, error = %e, "retention: delete failed");
            }
        }
    }
    if report.meetings + report.meetings_failed > 0 {
        tracing::info!(
            days,
            meetings = report.meetings,
            files = report.files_removed,
            bytes = report.bytes_removed,
            failed = report.meetings_failed,
            "retention sweep deleted recording audio"
        );
    }
    report
}

fn sweep_now(state: &AppState) -> SweepReport {
    // Recording wins: never touch the recordings tree mid-capture.
    if crate::scheduler::recording_active(state) {
        return SweepReport::default();
    }
    let busy_ids: Vec<String> = state
        .pipeline_stage
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let storage = state.storage.lock().unwrap();
    sweep_retention(&storage, Utc::now(), &|id| busy_ids.iter().any(|b| b == id))
}

/// Retention worker: one sweep shortly after startup, then daily.
pub fn spawn_retention<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_SWEEP_DELAY).await;
        loop {
            let handle = app.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                sweep_now(&handle.state::<AppState>());
            })
            .await;
            tokio::time::sleep(SWEEP_INTERVAL).await;
        }
    });
}

#[derive(Serialize)]
pub struct PrivacySettings {
    /// None = keep audio forever (the default).
    pub audio_retention_days: Option<u32>,
    pub retention_choices: Vec<u32>,
    /// What the last "Delete all my data" restart couldn't remove, if any.
    pub last_wipe: Option<WipeReport>,
}

#[tauri::command]
pub fn get_privacy_settings(state: State<'_, AppState>) -> CmdResult<PrivacySettings> {
    let storage = state.storage.lock().unwrap();
    let last_wipe = storage
        .get_setting(WIPE_REPORT_SETTING)
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok());
    Ok(PrivacySettings {
        audio_retention_days: storage.audio_retention_days(),
        retention_choices: RETENTION_CHOICES.to_vec(),
        last_wipe,
    })
}

/// How many meetings would lose their audio right now under `days`.
#[tauri::command]
pub fn preview_audio_retention(state: State<'_, AppState>, days: u32) -> CmdResult<usize> {
    if !RETENTION_CHOICES.contains(&days) {
        return Err(format!(
            "retention must be one of {RETENTION_CHOICES:?} days"
        ));
    }
    let cutoff = Utc::now() - chrono::Duration::days(i64::from(days));
    state
        .storage
        .lock()
        .unwrap()
        .retention_candidates(cutoff)
        .map(|ids| ids.len())
        .map_err(err_str)
}

/// Set (or turn off with `None`) audio retention, then sweep right away so
/// the setting takes effect without waiting a day.
#[tauri::command]
pub async fn set_audio_retention(
    state: State<'_, AppState>,
    days: Option<u32>,
) -> CmdResult<SweepReport> {
    state
        .storage
        .lock()
        .unwrap()
        .set_audio_retention_days(days)
        .map_err(err_str)?;
    tracing::info!(?days, "audio retention changed");
    Ok(sweep_now(&state))
}

#[tauri::command]
pub fn dismiss_wipe_report(state: State<'_, AppState>) -> CmdResult<()> {
    state
        .storage
        .lock()
        .unwrap()
        .delete_setting(WIPE_REPORT_SETTING)
        .map_err(err_str)
}

// ---------------------------------------------------------------------------
// Delete all my data
// ---------------------------------------------------------------------------

/// Marker left in the data dir: the next launch wipes the folder before the
/// database (or logging) opens anything in it.
pub const WIPE_MARKER: &str = ".delete-all-pending.json";
/// Internal setting holding what the restart wipe couldn't remove.
const WIPE_REPORT_SETTING: &str = "privacy.last_wipe_report";
/// Text the UI must send back — a guard against an accidental call, on top
/// of the UI's own two confirmations.
pub const WIPE_CONFIRMATION: &str = "delete everything";
/// Folders holding downloaded engines/models (re-downloadable, not personal).
const MODEL_DIRS: &[&str] = &["models", "bin"];
/// Leftover paths reported back at most (the log has the rest).
const MAX_REPORTED: usize = 50;

#[derive(Debug, Serialize, Deserialize)]
struct WipeMarker {
    keep_models: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct WipeReport {
    pub removed: usize,
    /// Data-dir-relative paths that couldn't be deleted (locked by another
    /// program, permissions). Capped at MAX_REPORTED.
    pub failed: Vec<String>,
    /// Keychain entries that couldn't be removed.
    pub secrets_failed: Vec<String>,
    /// True when the restart still has to finish the job.
    pub restart_required: bool,
}

impl WipeReport {
    fn fail(&mut self, root: &Path, path: &Path) {
        if self.failed.len() < MAX_REPORTED {
            let rel = path.strip_prefix(root).unwrap_or(path);
            self.failed.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// Every keychain entry the app owns.
fn owned_secret_keys() -> Vec<&'static str> {
    let mut keys = fly_secrets::keys::ALL.to_vec();
    keys.push(crate::calendar_commands::GOOGLE_CLIENT_SECRET_KEY);
    keys
}

/// Delete every owned key from each store; returns the keys that failed.
pub fn delete_owned_secrets(stores: &[&dyn SecretStore]) -> Vec<String> {
    let mut failed = Vec::new();
    for store in stores {
        for key in owned_secret_keys() {
            if let Err(e) = store.delete(key) {
                tracing::warn!(key, error = %e, "could not delete keychain entry");
                if !failed.iter().any(|k| k == key) {
                    failed.push(key.to_string());
                }
            }
        }
    }
    failed
}

/// Remove `path` (file or tree) as completely as possible: a locked file
/// only strands itself (and its parent folders), never its siblings.
fn remove_best_effort(root: &Path, path: &Path, report: &mut WipeReport) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_dir() {
        // fast path first
        if std::fs::remove_dir_all(path).is_ok() {
            report.removed += 1;
            return;
        }
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                remove_best_effort(root, &entry.path(), report);
            }
        }
        // A folder left non-empty already reported its stuck children; an
        // empty one that still won't go is itself the leftover.
        if std::fs::remove_dir(path).is_ok() {
            report.removed += 1;
        } else if std::fs::read_dir(path).is_ok_and(|mut d| d.next().is_none()) {
            report.fail(root, path);
        }
    } else {
        // symlinks are removed as links, never followed
        match std::fs::remove_file(path) {
            Ok(()) => report.removed += 1,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "wipe: could not delete");
                report.fail(root, path);
            }
        }
    }
}

/// Delete the contents of the data dir, keeping only the marker (removed
/// by the caller) and, when `keep_models`, the downloaded engines/models.
pub fn wipe_data_dir(data_dir: &Path, keep_models: bool, only: Option<&[&str]>) -> WipeReport {
    let mut report = WipeReport::default();
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return report;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == WIPE_MARKER {
            continue;
        }
        if keep_models && MODEL_DIRS.contains(&name.as_str()) {
            continue;
        }
        if let Some(only) = only {
            if !only.contains(&name.as_str()) {
                continue;
            }
        }
        remove_best_effort(data_dir, &entry.path(), &mut report);
    }
    report
}

/// Leftovers of a restart wipe, recorded once the fresh database is open.
static PENDING_WIPE_REPORT: Mutex<Option<WipeReport>> = Mutex::new(None);

/// Startup half of "Delete all my data": when the marker is present, wipe
/// the data dir before the database or log files are opened, then drop the
/// marker (one attempt only — a stuck file must never make every later
/// launch delete the user's new data). Call before `logging::init`.
pub fn run_pending_wipe(data_dir: &Path) {
    let marker_path = data_dir.join(WIPE_MARKER);
    let Ok(raw) = std::fs::read_to_string(&marker_path) else {
        return;
    };
    let keep_models = serde_json::from_str::<WipeMarker>(&raw)
        .map(|m| m.keep_models)
        .unwrap_or(true);
    let report = wipe_data_dir(data_dir, keep_models, None);
    let _ = std::fs::remove_file(&marker_path);
    *PENDING_WIPE_REPORT.lock().unwrap() = Some(report);
}

/// Record (and log) what the startup wipe left behind, now that storage and
/// logging are up. A clean wipe records nothing.
pub fn record_pending_wipe(storage: &Storage) {
    let Some(report) = PENDING_WIPE_REPORT.lock().unwrap().take() else {
        return;
    };
    tracing::info!(
        removed = report.removed,
        leftovers = report.failed.len(),
        "finished deleting all data at startup"
    );
    if !report.failed.is_empty() {
        if let Ok(json) = serde_json::to_string(&report) {
            let _ = storage.set_setting(WIPE_REPORT_SETTING, &json);
        }
    }
}

/// Content folders deleted right away (before the restart) — the most
/// sensitive files, gone even if the user never restarts.
const CONTENT_DIRS: &[&str] = &["recordings", "notes", "attachments", "transcripts"];

/// "Delete all my data": empties the database in place, removes recordings,
/// notes, attachments and transcripts now, deletes every keychain entry the
/// app owns, and leaves the marker so the restart removes everything else
/// (the database file itself, logs, and — unless kept — downloaded models).
/// The UI restarts the app when this returns.
#[tauri::command]
pub fn delete_all_data(
    state: State<'_, AppState>,
    include_models: bool,
    confirmation: String,
) -> CmdResult<WipeReport> {
    if confirmation != WIPE_CONFIRMATION {
        return Err("confirmation text doesn't match — nothing was deleted".into());
    }
    if crate::scheduler::recording_active(&state) {
        return Err("stop the recording first — nothing was deleted".into());
    }
    perform_delete_all(
        &state,
        include_models,
        &fly_secrets::KeychainSecretStore::with_service(crate::state::LEGACY_KEYCHAIN_SERVICE),
    )
}

fn perform_delete_all(
    state: &AppState,
    include_models: bool,
    legacy_secrets: &dyn SecretStore,
) -> CmdResult<WipeReport> {
    let data_dir = state.data_dir.clone();
    tracing::warn!(include_models, "deleting all user data");
    // Marker first: whatever happens next, the restart finishes the job.
    let marker = serde_json::to_string(&WipeMarker {
        keep_models: !include_models,
    })
    .map_err(err_str)?;
    std::fs::write(data_dir.join(WIPE_MARKER), marker)
        .map_err(|e| format!("couldn't start deleting (data folder not writable): {e}"))?;

    // Hold the storage lock throughout so no worker writes mid-wipe.
    let storage = state.storage.lock().unwrap();
    if let Err(e) = storage.erase_all_content() {
        tracing::warn!(error = %e, "could not empty the database in place");
    }
    let mut report = wipe_data_dir(&data_dir, true, Some(CONTENT_DIRS));
    drop(storage);
    report.secrets_failed = delete_owned_secrets(&[state.secrets.as_ref(), legacy_secrets]);
    report.restart_required = true;
    Ok(report)
}

// ---------------------------------------------------------------------------
// Disk encryption
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EncryptionState {
    On,
    Off,
    /// Couldn't tell — the UI says nothing.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskEncryption {
    pub state: EncryptionState,
    /// "BitLocker" / "FileVault", for the banner text.
    pub method: Option<&'static str>,
    /// The volume checked (e.g. "C:"), when known.
    pub volume: Option<String>,
}

/// Shell.Application's `System.Volume.BitLockerProtection` (readable without
/// elevation, unlike manage-bde / Get-BitLockerVolume): 1 on, 2 off,
/// 3 encrypting, 6 on (locked). Everything else (decrypting, suspended,
/// waiting for activation, empty) is not clear-cut → unknown.
pub fn classify_bitlocker(stdout: &str) -> EncryptionState {
    match stdout.trim() {
        "1" | "3" | "6" => EncryptionState::On,
        "2" => EncryptionState::Off,
        _ => EncryptionState::Unknown,
    }
}

/// `fdesetup isactive` prints "true" / "false".
pub fn classify_fdesetup(stdout: &str) -> EncryptionState {
    match stdout.trim() {
        "true" => EncryptionState::On,
        "false" => EncryptionState::Off,
        _ => EncryptionState::Unknown,
    }
}

/// "C:" for a path on a lettered drive; None for UNC/network paths. The
/// letter is validated before it is ever put in a command line.
pub fn windows_drive(path: &Path) -> Option<String> {
    let s = path.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let mut chars = s.chars();
    let letter = chars.next()?;
    (letter.is_ascii_alphabetic() && chars.next() == Some(':'))
        .then(|| format!("{}:", letter.to_ascii_uppercase()))
}

#[cfg(target_os = "windows")]
fn probe(data_dir: &Path) -> DiskEncryption {
    use std::os::windows::process::CommandExt;
    let Some(drive) = windows_drive(data_dir) else {
        return DiskEncryption {
            state: EncryptionState::Unknown,
            method: None,
            volume: None,
        };
    };
    let script = format!(
        "(New-Object -ComObject Shell.Application).NameSpace('{drive}').Self.ExtendedProperty('System.Volume.BitLockerProtection')"
    );
    let state = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        // CREATE_NO_WINDOW — no console flash from a GUI process
        .creation_flags(0x0800_0000)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| classify_bitlocker(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(EncryptionState::Unknown);
    DiskEncryption {
        state,
        method: Some("BitLocker"),
        volume: Some(drive),
    }
}

#[cfg(target_os = "macos")]
fn probe(_data_dir: &Path) -> DiskEncryption {
    let state = std::process::Command::new("/usr/bin/fdesetup")
        .arg("isactive")
        .output()
        .ok()
        .map(|o| classify_fdesetup(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(EncryptionState::Unknown);
    DiskEncryption {
        state,
        method: Some("FileVault"),
        volume: None,
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn probe(_data_dir: &Path) -> DiskEncryption {
    DiskEncryption {
        state: EncryptionState::Unknown,
        method: None,
        volume: None,
    }
}

/// Probe result, cached for the session (the startup check fills it).
static DISK_ENCRYPTION: Mutex<Option<DiskEncryption>> = Mutex::new(None);

fn disk_encryption_cached(data_dir: PathBuf) -> DiskEncryption {
    if let Some(d) = DISK_ENCRYPTION.lock().unwrap().clone() {
        return d;
    }
    let d = probe(&data_dir);
    tracing::info!(state = ?d.state, method = ?d.method, "disk encryption check");
    *DISK_ENCRYPTION.lock().unwrap() = Some(d.clone());
    d
}

/// Startup check, off the startup path.
pub fn spawn_disk_encryption_check(data_dir: PathBuf) {
    tauri::async_runtime::spawn(async move {
        let _ =
            tauri::async_runtime::spawn_blocking(move || disk_encryption_cached(data_dir)).await;
    });
}

#[tauri::command]
pub async fn disk_encryption_status(state: State<'_, AppState>) -> CmdResult<DiskEncryption> {
    let data_dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || disk_encryption_cached(data_dir))
        .await
        .map_err(err_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_secrets::MemorySecretStore;

    #[test]
    fn bitlocker_values_map_conservatively() {
        assert_eq!(classify_bitlocker("1\r\n"), EncryptionState::On);
        assert_eq!(classify_bitlocker("3"), EncryptionState::On);
        assert_eq!(classify_bitlocker("2"), EncryptionState::Off);
        for unclear in ["", "0", "4", "5", "8", "garbage"] {
            assert_eq!(
                classify_bitlocker(unclear),
                EncryptionState::Unknown,
                "{unclear:?}"
            );
        }
        assert_eq!(classify_fdesetup("true\n"), EncryptionState::On);
        assert_eq!(classify_fdesetup("false"), EncryptionState::Off);
        assert_eq!(classify_fdesetup("Error: …"), EncryptionState::Unknown);
    }

    #[test]
    fn drive_letter_is_validated() {
        assert_eq!(
            windows_drive(Path::new(r"C:\Users\a\AppData\Roaming\FlyOnTheWall")).as_deref(),
            Some("C:")
        );
        assert_eq!(
            windows_drive(Path::new(r"\\?\d:\data")).as_deref(),
            Some("D:")
        );
        assert_eq!(windows_drive(Path::new(r"\\server\share\x")), None);
        assert_eq!(windows_drive(Path::new("/home/a")), None);
        assert_eq!(windows_drive(Path::new("'; rm")), None);
    }

    #[test]
    fn owned_secrets_are_all_deleted() {
        let store = MemorySecretStore::default();
        for k in owned_secret_keys() {
            store.set(k, "secret").unwrap();
        }
        store.set("unrelated", "keep").unwrap();
        let failed = delete_owned_secrets(&[&store]);
        assert!(failed.is_empty());
        for k in owned_secret_keys() {
            assert_eq!(store.get(k).unwrap(), None, "{k}");
        }
        assert!(owned_secret_keys().contains(&fly_secrets::keys::ANTHROPIC_API_KEY));
        assert!(owned_secret_keys().contains(&fly_secrets::keys::MS_OAUTH_TOKEN));
    }

    fn populated_data_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        for d in [
            "recordings/m1",
            "notes",
            "attachments/n1",
            "logs",
            "models/asr",
            "bin",
        ] {
            std::fs::create_dir_all(p.join(d)).unwrap();
        }
        for f in [
            "flyonthewall.db",
            "flyonthewall.db-wal",
            "recordings/m1/recording.mic.wav",
            "notes/a.md",
            "attachments/n1/x.pdf",
            "logs/app.log",
            "models/asr/ggml.bin",
            "bin/whisper-cli.exe",
        ] {
            std::fs::write(p.join(f), b"x").unwrap();
        }
        dir
    }

    #[test]
    fn wipe_keeps_models_only_when_asked() {
        let dir = populated_data_dir();
        let r = wipe_data_dir(dir.path(), true, None);
        assert!(r.failed.is_empty());
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let mut left = left;
        left.sort();
        assert_eq!(left, vec!["bin", "models"]);
        assert!(dir.path().join("models/asr/ggml.bin").exists());

        let r = wipe_data_dir(dir.path(), false, None);
        assert!(r.failed.is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn pending_wipe_runs_once_and_drops_the_marker() {
        let dir = populated_data_dir();
        std::fs::write(
            dir.path().join(WIPE_MARKER),
            serde_json::to_string(&WipeMarker { keep_models: false }).unwrap(),
        )
        .unwrap();
        run_pending_wipe(dir.path());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // a later launch without the marker never deletes anything
        std::fs::write(dir.path().join("flyonthewall.db"), b"new").unwrap();
        run_pending_wipe(dir.path());
        assert!(dir.path().join("flyonthewall.db").exists());
    }

    /// The command path: DB emptied in place, content folders gone now,
    /// secrets removed from both keychain services, marker left for restart.
    #[test]
    fn delete_all_empties_db_content_and_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = std::sync::Arc::new(MemorySecretStore::default());
        secrets.set(fly_secrets::keys::GROQ_API_KEY, "gsk").unwrap();
        let state = AppState::init_with(dir.path().to_path_buf(), secrets.clone()).unwrap();
        let legacy = MemorySecretStore::default();
        legacy.set(fly_secrets::keys::OPENAI_API_KEY, "sk").unwrap();
        {
            let s = state.storage.lock().unwrap();
            s.create_note("Confidential", None).unwrap();
        }
        std::fs::create_dir_all(dir.path().join("recordings/m")).unwrap();
        std::fs::write(dir.path().join("recordings/m/a.wav"), b"x").unwrap();

        let report = perform_delete_all(&state, false, &legacy).unwrap();

        assert!(report.restart_required);
        assert!(report.secrets_failed.is_empty());
        assert_eq!(secrets.get(fly_secrets::keys::GROQ_API_KEY).unwrap(), None);
        assert_eq!(legacy.get(fly_secrets::keys::OPENAI_API_KEY).unwrap(), None);
        assert!(!dir.path().join("recordings").exists());
        assert!(dir.path().join(WIPE_MARKER).exists());
        assert!(state
            .storage
            .lock()
            .unwrap()
            .list_recent_notes(10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn sweep_is_a_noop_when_retention_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        let r = sweep_retention(&s, Utc::now(), &|_| false);
        assert_eq!(r, SweepReport::default());
    }
}
