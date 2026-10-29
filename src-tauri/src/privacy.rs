//! Privacy controls (the code side of SOC 2 Confidentiality / Privacy):
//!
//! * per-meeting / per-folder **Keep local** — the commands here only read
//!   and write the flag; enforcement lives where providers are chosen
//!   (`llm_commands::build_provider`, `pipeline::run_with`, the embedding
//!   worker);
//! * **audio retention** — a sweep on startup and every 24 h that deletes
//!   recording media older than the chosen period (transcripts and notes
//!   kept), through fly-storage's containment-guarded `delete_meeting_audio`;
//! * **Delete all my data** — wipes the database in place, deletes the
//!   keychain entries the app owns, and leaves a short-lived marker so the
//!   restart removes every file in the data folder before anything reopens
//!   it;
//! * **recording notice** helper (clipboard) and the participants-notified
//!   flag;
//! * a best-effort **disk encryption** probe (BitLocker / FileVault) that
//!   never needs elevation and says nothing when it can't tell.
//!
//! The retention setting is an internal key (`privacy.*`), written only by
//! the validating command below — never via the webview's generic settings
//! access, because it steers file deletes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
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

/// A meeting self-heal restored and set to Keep local because its manifest
/// didn't record the setting, as the review notice shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RecoveredMeeting {
    pub meeting_id: String,
    pub note_id: String,
    pub title: String,
}

/// Event carrying the current `Vec<RecoveredMeeting>` after self-heal.
pub const RECOVERED_KEPT_LOCAL_EVENT: &str = "privacy:recovered-kept-local";

/// `ids` as notice entries; meetings that no longer exist are left out.
fn recovered_entries(storage: &Storage, ids: &[String]) -> Vec<RecoveredMeeting> {
    ids.iter()
        .filter_map(|id| {
            let meeting = storage.get_meeting(id).ok()?;
            let title = storage
                .get_note(&meeting.note_id)
                .map(|n| n.title)
                .unwrap_or(meeting.title);
            Some(RecoveredMeeting {
                meeting_id: meeting.id,
                note_id: meeting.note_id,
                title,
            })
        })
        .collect()
}

/// Remember the meetings this launch's self-heal kept local only because
/// their manifest didn't say, and return them as notice entries.
pub fn note_recovered_kept_local(state: &AppState, ids: &[String]) -> Vec<RecoveredMeeting> {
    let mut list = state.recovered_kept_local.lock().unwrap();
    for id in ids {
        if !list.contains(id) {
            list.push(id.clone());
        }
    }
    recovered_entries(&state.storage.lock().unwrap(), &list)
}

/// Meetings restored this launch with Keep local set because their privacy
/// setting couldn't be confirmed (the notice catches up on these at mount;
/// later changes arrive as [`RECOVERED_KEPT_LOCAL_EVENT`]).
#[tauri::command]
pub fn recovered_kept_local(state: State<'_, AppState>) -> Vec<RecoveredMeeting> {
    let ids = state.recovered_kept_local.lock().unwrap().clone();
    recovered_entries(&state.storage.lock().unwrap(), &ids)
}

/// Take one meeting (the one being reviewed) or, with no id, all of them off
/// the list. Returns what's left. Changes nothing about Keep local itself.
#[tauri::command]
pub fn dismiss_recovered_kept_local(
    state: State<'_, AppState>,
    meeting_id: Option<String>,
) -> Vec<RecoveredMeeting> {
    dismiss_recovered(&state, meeting_id.as_deref())
}

fn dismiss_recovered(state: &AppState, meeting_id: Option<&str>) -> Vec<RecoveredMeeting> {
    let ids = {
        let mut list = state.recovered_kept_local.lock().unwrap();
        match meeting_id {
            Some(id) => list.retain(|m| m != id),
            None => list.clear(),
        }
        list.clone()
    };
    recovered_entries(&state.storage.lock().unwrap(), &ids)
}

/// Longest recording notice the clipboard helper accepts, in characters.
const MAX_NOTICE_CHARS: usize = 2000;

fn valid_notice(text: &str) -> bool {
    !text.trim().is_empty() && text.chars().count() <= MAX_NOTICE_CHARS
}

/// Copy the recording notice to the clipboard (native write: the webview's
/// clipboard API is patchy across platforms).
#[tauri::command]
pub fn copy_recording_notice(app: tauri::AppHandle, text: String) -> CmdResult<()> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    if !valid_notice(&text) {
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
/// Internal: when the last sweep ran (RFC 3339) — the clock-skew guard.
const LAST_SWEEP_SETTING: &str = "privacy.retention_last_sweep";
/// Internal: how many meetings a background sweep held back because it
/// would have deleted too much at once (cleared by an explicit sweep).
const HELD_SETTING: &str = "privacy.retention_held";
/// The clock may wobble; only a clear jump backwards stops the sweep.
const CLOCK_SKEW_TOLERANCE: chrono::Duration = chrono::Duration::hours(1);
/// A background sweep never deletes more than this share of the meetings
/// still holding audio in one go (explicit sweeps, confirmed by the user in
/// Settings, may) — a clock jumping years ahead would otherwise make every
/// recording "old".
const BULK_MAX_SHARE: f64 = 0.5;
/// Below this many meetings the share guard doesn't apply.
const BULK_MIN_COUNT: usize = 3;

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct SweepReport {
    pub meetings: usize,
    pub files_removed: usize,
    pub bytes_removed: u64,
    /// Meetings with at least one file that couldn't be deleted (locked);
    /// the next sweep retries them.
    pub meetings_failed: usize,
    /// Set when a background sweep held back instead of deleting (clock
    /// moved backwards, or too much audio at once): why, for the log/UI.
    pub held: Option<String>,
}

/// Delete audio of every finished meeting older than the configured period.
/// `busy` meetings (a pipeline or re-diarize holding their files) are left
/// for the next sweep. No-op when retention is off.
///
/// `explicit` = the user just confirmed this in Settings (preview shown).
/// Background sweeps (`explicit = false`) refuse when the clock moved
/// backwards since the last sweep, and hold back when they would delete
/// more than half the remaining audio at once.
pub fn sweep_retention(
    storage: &Storage,
    now: DateTime<Utc>,
    busy: &dyn Fn(&str) -> bool,
    explicit: bool,
) -> SweepReport {
    let mut report = SweepReport::default();
    let Some(days) = storage.audio_retention_days() else {
        let _ = storage.delete_setting(HELD_SETTING);
        return report;
    };
    let last = storage
        .get_setting(LAST_SWEEP_SETTING)
        .ok()
        .flatten()
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|d| d.with_timezone(&Utc));
    if let Some(last) = last {
        if now + CLOCK_SKEW_TOLERANCE < last && !explicit {
            tracing::warn!("retention: clock is behind the last sweep; skipping");
            report.held = Some("the system clock moved backwards".into());
            return report;
        }
    }
    let cutoff = now - chrono::Duration::days(i64::from(days));
    let ids: Vec<String> = match storage.retention_candidates(cutoff) {
        Ok(ids) => ids.into_iter().filter(|id| !busy(id)).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "retention: listing candidates failed");
            return report;
        }
    };
    if !explicit && ids.len() >= BULK_MIN_COUNT {
        let with_audio = storage.meetings_with_audio().unwrap_or(0).max(1);
        if ids.len() as f64 > with_audio as f64 * BULK_MAX_SHARE {
            tracing::warn!(
                candidates = ids.len(),
                with_audio,
                "retention: would delete most audio at once; waiting for confirmation"
            );
            let _ = storage.set_setting(HELD_SETTING, &ids.len().to_string());
            report.held = Some(format!(
                "{} meetings would lose their audio at once",
                ids.len()
            ));
            return report;
        }
    }
    let _ = storage.delete_setting(HELD_SETTING);
    if last.is_none_or(|l| now > l) {
        let _ = storage.set_setting(LAST_SWEEP_SETTING, &now.to_rfc3339());
    }
    for id in &ids {
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

fn sweep_now(state: &AppState, explicit: bool) -> SweepReport {
    // Recording wins: never touch the recordings tree mid-capture.
    if crate::scheduler::recording_active(state) {
        return SweepReport::default();
    }
    // Transcribe, retranscribe and re-diarize all hold a pipeline_stage
    // entry while they read a meeting's audio.
    // Recording compression holds one meeting's folder at a time.
    let busy_ids: Vec<String> = state
        .pipeline_stage
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .chain(state.audio_store.current())
        .collect();
    let storage = state.storage.lock().unwrap();
    // The lease is taken under this same lock, so a meeting the job picked
    // up after `busy_ids` was read is still skipped.
    sweep_retention(
        &storage,
        Utc::now(),
        &|id| busy_ids.iter().any(|b| b == id) || storage.meeting_folder_leased(id),
        explicit,
    )
}

/// Retention worker: one sweep shortly after startup, then daily.
pub fn spawn_retention<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_SWEEP_DELAY).await;
        loop {
            let handle = app.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                sweep_now(&handle.state::<AppState>(), false);
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
    /// Meetings a background sweep held back (would have deleted too much
    /// at once); the UI asks the user to confirm.
    pub retention_held: Option<usize>,
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
    let retention_held = storage
        .get_setting(HELD_SETTING)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok());
    Ok(PrivacySettings {
        audio_retention_days: storage.audio_retention_days(),
        retention_choices: RETENTION_CHOICES.to_vec(),
        retention_held,
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
/// the setting takes effect without waiting a day. The UI previews the
/// affected count and asks first, so this sweep is an explicit one.
#[tauri::command]
pub async fn set_audio_retention(
    app: tauri::AppHandle,
    days: Option<u32>,
) -> CmdResult<SweepReport> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state
            .storage
            .lock()
            .unwrap()
            .set_audio_retention_days(days)
            .map_err(err_str)?;
        tracing::info!(?days, "audio retention changed");
        Ok(sweep_now(&state, true))
    })
    .await
    .map_err(err_str)?
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
// Busy tracking (imports aren't pipeline jobs until they're queued)
// ---------------------------------------------------------------------------

static IMPORTS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Held by import commands while they copy/convert files into the data
/// folder, so "Delete all my data" can refuse to run under them.
pub struct ImportGuard;

impl ImportGuard {
    /// Mark an import as running until the guard drops.
    pub fn enter() -> Self {
        IMPORTS_IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        ImportGuard
    }
}

impl Drop for ImportGuard {
    fn drop(&mut self) {
        IMPORTS_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Why the app is too busy to delete everything, if it is.
fn busy_reason(state: &AppState) -> Option<&'static str> {
    if crate::scheduler::recording_active(state) {
        return Some("stop the recording first");
    }
    if IMPORTS_IN_FLIGHT.load(Ordering::SeqCst) > 0 {
        return Some("wait for the media import to finish");
    }
    if !state.pipeline_stage.lock().unwrap().is_empty() {
        return Some("wait for transcription to finish, or cancel it");
    }
    let queued = state
        .storage
        .lock()
        .unwrap()
        .queued_transcription_ids()
        .map(|ids| !ids.is_empty())
        .unwrap_or(true);
    if queued {
        return Some("cancel the queued transcriptions first");
    }
    None
}

// ---------------------------------------------------------------------------
// Delete all my data
// ---------------------------------------------------------------------------

/// Marker left in the data dir: the next launch wipes the folder before the
/// database (or logging) opens anything in it.
pub const WIPE_MARKER: &str = ".delete-all-pending.json";
/// The marker is renamed to this while the startup wipe runs, so a second
/// process can never pick the same marker up.
const WIPING_MARKER: &str = ".delete-all-wiping.json";
/// Held (OS file lock) by every running instance for its lifetime. The
/// startup wipe only runs once it owns this lock, i.e. after the previous
/// instance (the one that asked for the wipe) has exited.
const INSTANCE_LOCK: &str = ".instance.lock";
/// A marker older than this is stale (the user never restarted) and is
/// discarded instead of acted on: it must never delete data created later.
const MARKER_MAX_AGE_SECS: i64 = 10 * 60;
/// How long a launch that finds a marker waits for the old instance to exit.
const LOCK_WAIT: Duration = Duration::from_secs(20);
/// Locked files get a few retries (the old process may still be closing).
const WIPE_RETRIES: u32 = 5;
const WIPE_RETRY_DELAY: Duration = Duration::from_millis(800);
/// Internal setting holding what the restart wipe couldn't remove.
const WIPE_REPORT_SETTING: &str = "privacy.last_wipe_report";
/// Text the UI must send back. An accident guard only — on top of the UI's
/// two confirmations — not a security boundary: a compromised webview can
/// send it too (it can already delete notes one by one).
pub const WIPE_CONFIRMATION: &str = "delete everything";
/// Folders holding downloaded engines/models (re-downloadable, not personal).
const MODEL_DIRS: &[&str] = &["models", "bin"];
/// Never deleted by the wipe itself (removed/handled separately).
const WIPE_SKIP: &[&str] = &[WIPE_MARKER, WIPING_MARKER, INSTANCE_LOCK];
/// Leftover paths reported back at most (the log has the rest).
const MAX_REPORTED: usize = 50;

#[derive(Debug, Serialize, Deserialize)]
struct WipeMarker {
    keep_models: bool,
    /// Unix seconds when the user confirmed the wipe.
    #[serde(default)]
    created_at: i64,
    /// Process that wrote it (diagnostics).
    #[serde(default)]
    pid: u32,
    /// Set if the marker couldn't be deleted after a wipe: never act on it
    /// again.
    #[serde(default)]
    consumed: bool,
}

impl WipeMarker {
    /// Fresh, unconsumed, and not from the future (clock moved back).
    fn is_actionable(&self, now: i64) -> bool {
        let age = now - self.created_at;
        !self.consumed && self.created_at > 0 && (0..=MARKER_MAX_AGE_SECS).contains(&age)
    }
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
    /// Every table was emptied in place.
    #[serde(default)]
    pub db_erased: bool,
    /// The erased pages reached the database file (WAL checkpointed). False
    /// means old content is still on disk — usually another program (an MCP
    /// client such as Claude Desktop) has the database open.
    #[serde(default)]
    pub db_checkpointed: bool,
    /// Notes for the user (stale marker discarded, …).
    #[serde(default)]
    pub notes: Vec<String>,
}

impl WipeReport {
    fn fail(&mut self, root: &Path, path: &Path) {
        if self.failed.len() < MAX_REPORTED {
            let rel = path.strip_prefix(root).unwrap_or(path);
            self.failed.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }

    /// The in-place half fully worked: safe to restart straight away.
    pub fn clean(&self) -> bool {
        self.db_erased
            && self.db_checkpointed
            && self.failed.is_empty()
            && self.secrets_failed.is_empty()
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
                tracing::warn!(file = %path.file_name().unwrap_or_default().to_string_lossy(), error = %e, "wipe: could not delete");
                report.fail(root, path);
            }
        }
    }
}

/// Delete the contents of the data dir, keeping only the markers / instance
/// lock and, when `keep_models`, the downloaded engines/models.
pub fn wipe_data_dir(data_dir: &Path, keep_models: bool, only: Option<&[&str]>) -> WipeReport {
    let mut report = WipeReport::default();
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return report;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if WIPE_SKIP.contains(&name.as_str()) {
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

/// `wipe_data_dir`, retrying while files stay locked (a process that is
/// still exiting releases them within seconds).
fn wipe_with_retries(
    data_dir: &Path,
    keep_models: bool,
    retries: u32,
    delay: Duration,
) -> WipeReport {
    let mut total_removed = 0;
    for attempt in 1..=retries.max(1) {
        let report = wipe_data_dir(data_dir, keep_models, None);
        total_removed += report.removed;
        if report.failed.is_empty() || attempt == retries.max(1) {
            return WipeReport {
                removed: total_removed,
                ..report
            };
        }
        std::thread::sleep(delay);
    }
    unreachable!("the loop returns on its last attempt")
}

/// This process's hold on the instance lock (kept for its lifetime).
static INSTANCE_LOCK_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// Take the instance lock, waiting up to `wait` for another instance to
/// let go. Returns whether this process now holds it.
fn hold_instance_lock(data_dir: &Path, wait: Duration) -> bool {
    if INSTANCE_LOCK_FILE.lock().unwrap().is_some() {
        return true;
    }
    if std::fs::create_dir_all(data_dir).is_err() {
        return false;
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(data_dir.join(INSTANCE_LOCK))
    else {
        return false;
    };
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => {
                *INSTANCE_LOCK_FILE.lock().unwrap() = Some(file);
                return true;
            }
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(_) => return false,
        }
    }
}

/// Startup work that must run before logging and the database touch the
/// data dir: take the instance lock and finish a pending "Delete all my
/// data". A launch that finds a marker waits for the old instance to exit;
/// if it can't get the lock (another instance is still running), it leaves
/// the marker alone.
pub fn startup(data_dir: &Path) {
    let pending = data_dir.join(WIPE_MARKER).exists() || data_dir.join(WIPING_MARKER).exists();
    let wait = if pending { LOCK_WAIT } else { Duration::ZERO };
    if !hold_instance_lock(data_dir, wait) {
        if pending {
            note_startup("A pending delete waited for the other Fly on the Wall window to close; quit it and relaunch to finish.".into());
        }
        return;
    }
    run_pending_wipe(data_dir, chrono::Utc::now().timestamp());
}

/// Leftovers + notes of a restart wipe, recorded once the database is open.
static PENDING_WIPE_REPORT: Mutex<Option<WipeReport>> = Mutex::new(None);

fn note_startup(note: String) {
    PENDING_WIPE_REPORT
        .lock()
        .unwrap()
        .get_or_insert_with(WipeReport::default)
        .notes
        .push(note);
}

/// Startup half of "Delete all my data". The marker is renamed to the
/// "wiping" name first (atomic: no second process can act on it), checked
/// for freshness (≤10 minutes old — a stale marker never deletes data
/// created since), the folder is wiped with retries for locked files, and
/// the marker is removed. If removal fails it is rewritten as `consumed` so
/// it is never acted on again. Caller must hold the instance lock.
fn run_pending_wipe(data_dir: &Path, now: i64) {
    let marker = data_dir.join(WIPE_MARKER);
    let wiping = data_dir.join(WIPING_MARKER);
    if marker.exists() {
        if wiping.exists() {
            // an older interrupted wipe: the newer request supersedes it
            let _ = std::fs::remove_file(&wiping);
        }
        if let Err(e) = std::fs::rename(&marker, &wiping) {
            note_startup(format!("couldn't start the pending delete: {e}"));
            return;
        }
    }
    let Ok(raw) = std::fs::read_to_string(&wiping) else {
        return;
    };
    let parsed = serde_json::from_str::<WipeMarker>(&raw).ok();
    let Some(m) = parsed.filter(|m| m.is_actionable(now)) else {
        // stale, consumed, from the future, or unreadable: discard
        if std::fs::remove_file(&wiping).is_err() {
            consume_marker(&wiping);
        }
        note_startup(
            "A \"Delete all my data\" request expired before the app restarted, so nothing \
             was deleted at startup. Run it again from Settings → Privacy if you still want to."
                .into(),
        );
        return;
    };
    let mut report = wipe_with_retries(data_dir, m.keep_models, WIPE_RETRIES, WIPE_RETRY_DELAY);
    if std::fs::remove_file(&wiping).is_err() {
        consume_marker(&wiping);
        report.notes.push(
            "The delete marker couldn't be removed; it was disabled so it can't run again.".into(),
        );
    }
    let mut slot = PENDING_WIPE_REPORT.lock().unwrap();
    let notes = slot.take().map(|r| r.notes).unwrap_or_default();
    report.notes.splice(0..0, notes);
    *slot = Some(report);
}

/// Rewrite a marker as consumed so it can never trigger another wipe.
fn consume_marker(path: &Path) {
    let disabled = serde_json::to_string(&WipeMarker {
        keep_models: true,
        created_at: 0,
        pid: 0,
        consumed: true,
    })
    .unwrap_or_default();
    if std::fs::write(path, disabled).is_err() {
        // Nothing more we can do; the 10-minute expiry still bounds it.
        note_startup("The delete marker couldn't be disabled; it expires in 10 minutes.".into());
    }
}

/// Record (and log) what the startup wipe left behind, now that storage and
/// logging are up. A clean wipe records nothing.
pub fn record_pending_wipe(storage: &Storage) {
    let Some(report) = PENDING_WIPE_REPORT.lock().unwrap().take() else {
        return;
    };
    for note in &report.notes {
        tracing::warn!(note = %note, "startup wipe");
    }
    tracing::info!(
        removed = report.removed,
        leftovers = report.failed.len(),
        "finished deleting all data at startup"
    );
    if !report.failed.is_empty() || !report.notes.is_empty() {
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
/// app owns, and leaves a 10-minute marker so the restart removes everything
/// else (the database file itself, logs, and — unless kept — downloaded
/// models). Refused while anything is recording, importing or transcribing.
/// The UI then blocks further work until the app restarts.
#[tauri::command]
pub async fn delete_all_data(
    app: tauri::AppHandle,
    include_models: bool,
    confirmation: String,
) -> CmdResult<WipeReport> {
    // Accident guard, not authorization (see WIPE_CONFIRMATION).
    if confirmation != WIPE_CONFIRMATION {
        return Err("confirmation text doesn't match — nothing was deleted".into());
    }
    // Stop connector programs and drop connections before their keys go.
    crate::connectors::close_all(&app).await;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        if let Some(why) = busy_reason(&state) {
            return Err(format!("{why} — nothing was deleted"));
        }
        // Background compression works inside meeting folders: stop it (the
        // wipe restarts the app). Refused or failed → it picks up again.
        if !crate::audio_store::halt(&state, Duration::from_secs(20)) {
            crate::audio_store::resume(&state);
            return Err(
                "recording compression didn't stop in time — nothing was deleted; try again".into(),
            );
        }
        let result = perform_delete_all(
            &state,
            include_models,
            &fly_secrets::KeychainSecretStore::with_service(crate::state::LEGACY_KEYCHAIN_SERVICE),
        );
        if result.is_err() {
            crate::audio_store::resume(&state);
        }
        result
    })
    .await
    .map_err(err_str)?
}

fn perform_delete_all(
    state: &AppState,
    include_models: bool,
    legacy_secrets: &dyn SecretStore,
) -> CmdResult<WipeReport> {
    let data_dir = state.data_dir.clone();
    tracing::warn!(include_models, "deleting all user data");
    // Marker first: whatever happens next, a restart within 10 minutes
    // finishes the job (and an older marker can't fire later).
    let marker = serde_json::to_string(&WipeMarker {
        keep_models: !include_models,
        created_at: Utc::now().timestamp(),
        pid: std::process::id(),
        consumed: false,
    })
    .map_err(err_str)?;
    std::fs::write(data_dir.join(WIPE_MARKER), marker)
        .map_err(|e| format!("couldn't start deleting (data folder not writable): {e}"))?;

    let mut report = WipeReport::default();
    // Connector keychain entries are named after connector ids, which live in
    // the database: list them before the database is emptied.
    let connector_keys = crate::connectors::stored_secret_keys(
        &state.storage.lock().unwrap(),
        state.secrets.as_ref(),
    );
    // The storage lock covers only the database work, never file deletes.
    let erased = state
        .storage
        .lock()
        .unwrap()
        .erase_all_content()
        .map_err(err_str);
    apply_erase(&mut report, erased);
    let files = wipe_data_dir(&data_dir, true, Some(CONTENT_DIRS));
    report.removed = files.removed;
    report.failed = files.failed;
    report.secrets_failed = delete_owned_secrets(&[state.secrets.as_ref(), legacy_secrets]);
    for key in &connector_keys {
        if let Err(e) = state.secrets.delete(key) {
            tracing::warn!(key, error = %e, "could not delete a connector keychain entry");
            report.secrets_failed.push(key.clone());
        }
    }
    report.restart_required = true;
    Ok(report)
}

/// Fold the in-place erase result into the report. Anything short of an
/// erased AND checkpointed database is a failure the user must hear about
/// (the UI then doesn't restart on its own).
fn apply_erase(report: &mut WipeReport, erased: Result<fly_storage::EraseOutcome, String>) {
    match erased {
        Ok(outcome) => {
            report.db_erased = true;
            report.db_checkpointed = outcome.checkpointed;
        }
        Err(e) => tracing::warn!(error = %e, "could not empty the database in place"),
    }
    if !report.db_erased || !report.db_checkpointed {
        report.notes.push(
            "The database is still open in another program, so its old contents may remain on \
             disk. Quit Claude Desktop and any other app using Fly on the Wall's MCP server, \
             then choose Try again."
                .into(),
        );
    }
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

/// How long a probe process may run before it is killed.
#[cfg(any(target_os = "windows", target_os = "macos"))]
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `cmd`, returning its stdout if it exits successfully within
/// `timeout`; a hung process is killed.
#[cfg(target_os = "windows")]
fn output_with_timeout(mut cmd: std::process::Command, timeout: Duration) -> Option<String> {
    use std::io::Read;
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!("disk encryption probe timed out");
                return None;
            }
        }
    };
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    status.success().then_some(out)
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
    // Absolute path: never resolve `powershell` through PATH / the cwd.
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let exe = PathBuf::from(system_root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let script = format!(
        "(New-Object -ComObject Shell.Application).NameSpace('{drive}').Self.ExtendedProperty('System.Volume.BitLockerProtection')"
    );
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script])
        // CREATE_NO_WINDOW — no console flash from a GUI process
        .creation_flags(0x0800_0000);
    let state = output_with_timeout(cmd, PROBE_TIMEOUT)
        .map(|o| classify_bitlocker(&o))
        .unwrap_or(EncryptionState::Unknown);
    DiskEncryption {
        state,
        method: Some("BitLocker"),
        volume: Some(drive),
    }
}

#[cfg(target_os = "macos")]
fn probe(_data_dir: &Path) -> DiskEncryption {
    let mut cmd = std::process::Command::new("/usr/bin/fdesetup");
    cmd.arg("isactive");
    // `fdesetup isactive` exits 1 when inactive, so read stdout either way.
    let state = {
        use std::io::Read;
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        match cmd.spawn() {
            Ok(mut child) => {
                let deadline = Instant::now() + PROBE_TIMEOUT;
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => {
                            let mut out = String::new();
                            let _ = child.stdout.take().map(|mut s| s.read_to_string(&mut out));
                            break classify_fdesetup(&out);
                        }
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(100))
                        }
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break EncryptionState::Unknown;
                        }
                    }
                }
            }
            Err(_) => EncryptionState::Unknown,
        }
    };
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

/// Probe result, cached for the session — "unknown" included, so opening
/// Settings never spawns another probe. The mutex is held across the probe
/// so concurrent callers (startup check + Settings) share one process.
static DISK_ENCRYPTION: Mutex<Option<DiskEncryption>> = Mutex::new(None);

fn disk_encryption_cached(data_dir: PathBuf) -> DiskEncryption {
    let mut cached = DISK_ENCRYPTION.lock().unwrap();
    if let Some(d) = cached.clone() {
        return d;
    }
    let d = probe(&data_dir);
    tracing::info!(state = ?d.state, method = ?d.method, "disk encryption check");
    *cached = Some(d.clone());
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
    fn notice_length_counts_characters_not_bytes() {
        // 2000 multi-byte characters (6000 bytes) are fine; 2001 are not
        assert!(valid_notice(&"é".repeat(2000)));
        assert!(!valid_notice(&"é".repeat(2001)));
        assert!(!valid_notice("   "));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn hung_probe_is_killed_after_the_timeout() {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("ping");
        cmd.args(["-n", "30", "127.0.0.1"])
            .creation_flags(0x0800_0000);
        let t = Instant::now();
        assert_eq!(output_with_timeout(cmd, Duration::from_millis(300)), None);
        assert!(t.elapsed() < Duration::from_secs(10));
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

    fn names(dir: &Path) -> Vec<String> {
        let mut left: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        left
    }

    fn write_marker(dir: &Path, created_at: i64) {
        std::fs::write(
            dir.join(WIPE_MARKER),
            serde_json::to_string(&WipeMarker {
                keep_models: false,
                created_at,
                pid: 1,
                consumed: false,
            })
            .unwrap(),
        )
        .unwrap();
    }

    fn take_report() -> Option<WipeReport> {
        PENDING_WIPE_REPORT.lock().unwrap().take()
    }

    #[test]
    fn wipe_keeps_models_only_when_asked() {
        let dir = populated_data_dir();
        let r = wipe_data_dir(dir.path(), true, None);
        assert!(r.failed.is_empty());
        assert_eq!(names(dir.path()), vec!["bin", "models"]);
        assert!(dir.path().join("models/asr/ggml.bin").exists());

        let r = wipe_data_dir(dir.path(), false, None);
        assert!(r.failed.is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    // The report slot is process-global; the startup tests run serially.
    static STARTUP_TESTS: Mutex<()> = Mutex::new(());

    #[test]
    fn fresh_marker_wipes_once_and_is_removed() {
        let _g = STARTUP_TESTS.lock().unwrap();
        let dir = populated_data_dir();
        let now = Utc::now().timestamp();
        write_marker(dir.path(), now - 30);
        run_pending_wipe(dir.path(), now);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(take_report().is_some_and(|r| r.notes.is_empty()));
        // a later launch without the marker never deletes anything
        std::fs::write(dir.path().join("flyonthewall.db"), b"new").unwrap();
        run_pending_wipe(dir.path(), now + 5);
        assert!(dir.path().join("flyonthewall.db").exists());
    }

    /// M1: a marker the user never restarted on must not delete data that
    /// was created afterwards — it's discarded with a note instead.
    #[test]
    fn stale_or_future_markers_are_discarded_without_wiping() {
        let _g = STARTUP_TESTS.lock().unwrap();
        let now = Utc::now().timestamp();
        for created in [now - MARKER_MAX_AGE_SECS - 1, now + 3600, 0] {
            let dir = populated_data_dir();
            write_marker(dir.path(), created);
            run_pending_wipe(dir.path(), now);
            assert!(dir.path().join("flyonthewall.db").exists(), "{created}");
            assert!(!dir.path().join(WIPE_MARKER).exists());
            assert!(!dir.path().join(WIPING_MARKER).exists());
            let r = take_report().unwrap();
            assert!(r.notes[0].contains("expired"), "{:?}", r.notes);
        }
    }

    #[test]
    fn consumed_marker_never_wipes() {
        let _g = STARTUP_TESTS.lock().unwrap();
        let dir = populated_data_dir();
        consume_marker(&dir.path().join(WIPING_MARKER));
        run_pending_wipe(dir.path(), Utc::now().timestamp());
        assert!(dir.path().join("notes/a.md").exists());
        let _ = take_report();
    }

    /// M3: a launch only wipes once it holds the instance lock, which the
    /// running instance keeps until it exits.
    #[test]
    fn instance_lock_excludes_a_second_holder() {
        let dir = tempfile::tempdir().unwrap();
        let first = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.path().join(INSTANCE_LOCK))
            .unwrap();
        first.lock().unwrap();
        let second = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join(INSTANCE_LOCK))
            .unwrap();
        assert!(matches!(
            second.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        first.unlock().unwrap();
        assert!(second.try_lock().is_ok());
        // and the wipe never deletes the lock file itself
        let r = wipe_data_dir(dir.path(), false, None);
        assert!(r.failed.is_empty());
        assert!(dir.path().join(INSTANCE_LOCK).exists());
    }

    /// The command path: DB emptied in place (and checkpointed), content
    /// folders gone now, secrets removed from both keychain services, a
    /// fresh marker left for the restart.
    #[test]
    fn delete_all_empties_db_content_and_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = std::sync::Arc::new(MemorySecretStore::default());
        secrets.set(fly_secrets::keys::GROQ_API_KEY, "gsk").unwrap();
        let state = AppState::init_with(dir.path().to_path_buf(), secrets.clone()).unwrap();
        let legacy = MemorySecretStore::default();
        legacy.set(fly_secrets::keys::OPENAI_API_KEY, "sk").unwrap();
        // A connector with tokens and a stdio variable in the keychain.
        let mut connectors = fly_mcp_client::Registry::default();
        let connector = connectors
            .add_approved(
                fly_mcp_client::ServerConfig {
                    name: "Docs".into(),
                    transport: fly_mcp_client::TransportConfig::Stdio {
                        command: "docs-mcp".into(),
                        args: vec![],
                        env: vec![fly_mcp_client::EnvConfig {
                            name: "DOCS_TOKEN".into(),
                            value: None,
                        }],
                    },
                    auth: fly_mcp_client::AuthConfig::None,
                },
                0,
            )
            .unwrap();
        let connector_keys = [
            format!("connector.{connector}.access"),
            format!("connector.{connector}.access.0"),
            format!("connector.{connector}.refresh"),
            format!("connector.{connector}.env.DOCS_TOKEN"),
        ];
        for k in &connector_keys {
            secrets.set(k, "secret").unwrap();
        }
        {
            let s = state.storage.lock().unwrap();
            s.create_note("Confidential", None).unwrap();
            s.set_setting(crate::connectors::REGISTRY_KEY, &connectors.to_json())
                .unwrap();
        }
        std::fs::create_dir_all(dir.path().join("recordings/m")).unwrap();
        std::fs::write(dir.path().join("recordings/m/a.wav"), b"x").unwrap();

        let report = perform_delete_all(&state, false, &legacy).unwrap();

        assert!(report.restart_required);
        assert!(report.db_erased && report.db_checkpointed);
        assert!(report.clean(), "{report:?}");
        assert!(report.secrets_failed.is_empty());
        assert_eq!(secrets.get(fly_secrets::keys::GROQ_API_KEY).unwrap(), None);
        for k in &connector_keys {
            assert_eq!(secrets.get(k).unwrap(), None, "{k} survived delete-all");
        }
        assert_eq!(legacy.get(fly_secrets::keys::OPENAI_API_KEY).unwrap(), None);
        assert!(!dir.path().join("recordings").exists());
        let marker: WipeMarker =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(WIPE_MARKER)).unwrap())
                .unwrap();
        assert!(marker.is_actionable(Utc::now().timestamp()));
        assert!(state
            .storage
            .lock()
            .unwrap()
            .list_recent_notes(10)
            .unwrap()
            .is_empty());
    }

    /// H1: an MCP client holding the DB open blocks the checkpoint — the
    /// report must say so (no "Deleted. Restarting…").
    /// (fly-storage's `erase_reports_a_blocked_checkpoint` covers producing
    /// `checkpointed: false` from a real open reader.)
    #[test]
    fn delete_all_reports_a_db_held_open_elsewhere() {
        use fly_storage::EraseOutcome;
        let mut blocked = WipeReport::default();
        apply_erase(
            &mut blocked,
            Ok(EraseOutcome {
                checkpointed: false,
            }),
        );
        assert!(blocked.db_erased && !blocked.db_checkpointed);
        assert!(!blocked.clean());
        assert!(blocked.notes.iter().any(|n| n.contains("Claude Desktop")));

        let mut failed = WipeReport::default();
        apply_erase(&mut failed, Err("database is locked".into()));
        assert!(!failed.db_erased && !failed.clean());

        let mut ok = WipeReport::default();
        apply_erase(&mut ok, Ok(EraseOutcome { checkpointed: true }));
        assert!(ok.clean() && ok.notes.is_empty());
    }

    #[test]
    fn busy_app_refuses_delete_all() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::init_with(
            dir.path().to_path_buf(),
            std::sync::Arc::new(MemorySecretStore::default()),
        )
        .unwrap();
        assert_eq!(busy_reason(&state), None);
        {
            let _import = ImportGuard::enter();
            assert!(busy_reason(&state).unwrap().contains("import"));
        }
        state
            .pipeline_stage
            .lock()
            .unwrap()
            .insert("m".into(), "transcribing".into());
        assert!(busy_reason(&state).unwrap().contains("transcription"));
        state.pipeline_stage.lock().unwrap().clear();
        {
            let s = state.storage.lock().unwrap();
            let note = s.create_note("n", None).unwrap();
            let m = s.create_meeting("n", &note.id, &[]).unwrap();
            s.enqueue_transcription(&m.id).unwrap();
        }
        assert!(busy_reason(&state).unwrap().contains("queued"));
    }

    #[test]
    fn sweep_is_a_noop_when_retention_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        let r = sweep_retention(&s, Utc::now(), &|_| false, false);
        assert_eq!(r, SweepReport::default());
    }

    /// A finished meeting whose audio (files and dates) is `days` old.
    fn old_meeting(s: &Storage, dir: &Path, name: &str, days: i64) {
        let note = s.create_note(name, None).unwrap();
        let m = s.create_meeting(name, &note.id, &[]).unwrap();
        let rec = dir.join("recordings").join(name);
        std::fs::create_dir_all(&rec).unwrap();
        let wav = rec.join("recording.mic.wav");
        std::fs::write(&wav, b"RIFF").unwrap();
        let when = std::time::SystemTime::now() - Duration::from_secs(days as u64 * 86_400);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&wav)
            .unwrap()
            .set_modified(when)
            .unwrap();
        s.end_meeting(
            &m.id,
            &fly_core::RecordingRef {
                mic_path: Some(format!("recordings/{name}/recording.mic.wav")),
                system_path: None,
                mixed_path: None,
                playback_path: None,
                duration_ms: 1,
            },
        )
        .unwrap();
        // the date editor shifts started_at and ended_at (and renames the
        // folder; renames keep the files' modification times)
        s.set_meeting_started_at(&m.id, Utc::now() - chrono::Duration::days(days))
            .unwrap();
    }

    /// M5: a clock that jumped forward (or a long absence) makes most audio
    /// "old" at once — the background sweep holds back for confirmation; an
    /// explicit sweep from Settings goes ahead.
    #[test]
    fn background_sweep_holds_back_bulk_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        for i in 0..4 {
            old_meeting(&s, dir.path(), &format!("m{i}"), 100);
        }
        s.set_audio_retention_days(Some(30)).unwrap();
        let r = sweep_retention(&s, Utc::now(), &|_| false, false);
        assert_eq!(r.meetings, 0);
        assert!(r.held.is_some());
        assert_eq!(s.get_setting(HELD_SETTING).unwrap().as_deref(), Some("4"));

        let r = sweep_retention(&s, Utc::now(), &|_| false, true);
        assert_eq!(r.meetings, 4);
        assert_eq!(s.get_setting(HELD_SETTING).unwrap(), None);
    }

    /// M5: the clock moved backwards since the last sweep → skip.
    #[test]
    fn background_sweep_refuses_when_the_clock_went_back() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        old_meeting(&s, dir.path(), "a", 100);
        s.set_audio_retention_days(Some(30)).unwrap();
        let later = Utc::now() + chrono::Duration::days(3);
        s.set_setting(LAST_SWEEP_SETTING, &later.to_rfc3339())
            .unwrap();
        let r = sweep_retention(&s, Utc::now(), &|_| false, false);
        assert_eq!(r.meetings, 0);
        assert!(r.held.unwrap().contains("clock"));
        // busy meetings are skipped by an otherwise normal sweep
        s.delete_setting(LAST_SWEEP_SETTING).unwrap();
        let r = sweep_retention(&s, Utc::now(), &|_| true, false);
        assert_eq!(r.meetings, 0);
        assert!(r.held.is_none());
    }

    /// A meeting folder the compression job holds is skipped by the sweep,
    /// even before the job has said which meeting it's on.
    #[test]
    fn sweep_skips_a_folder_the_compression_job_holds() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::init_with(
            dir.path().to_path_buf(),
            std::sync::Arc::new(fly_secrets::MemorySecretStore::default()),
        )
        .unwrap();
        let id = {
            let s = state.storage.lock().unwrap();
            old_meeting(&s, dir.path(), "a", 100);
            s.set_audio_retention_days(Some(30)).unwrap();
            let id = s.meetings_with_stored_audio().unwrap().remove(0);
            assert!(s.lease_meeting_folder(&id));
            id
        };
        assert_eq!(sweep_now(&state, true).meetings, 0);
        state.storage.lock().unwrap().release_meeting_folder(&id);
        assert_eq!(sweep_now(&state, true).meetings, 1);
    }

    /// The review list for meetings heal kept local without a recorded
    /// setting: entries carry the note's title, unknown meetings are left
    /// out, repeats are ignored, and Review / Dismiss take entries off.
    #[test]
    fn recovered_meetings_list_for_the_review_notice() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::init_with(
            dir.path().to_path_buf(),
            std::sync::Arc::new(fly_secrets::MemorySecretStore::default()),
        )
        .unwrap();
        let (a, b) = {
            let storage = state.storage.lock().unwrap();
            let na = storage.create_note("Board prep", None).unwrap();
            let nb = storage.create_note("Vendor call", None).unwrap();
            (
                storage.create_meeting("Board prep", &na.id, &[]).unwrap(),
                storage.create_meeting("Vendor call", &nb.id, &[]).unwrap(),
            )
        };
        let ids = vec![a.id.clone(), "gone".to_string(), b.id.clone()];
        let entries = super::note_recovered_kept_local(&state, &ids);
        assert_eq!(
            entries,
            vec![
                super::RecoveredMeeting {
                    meeting_id: a.id.clone(),
                    note_id: a.note_id.clone(),
                    title: "Board prep".into(),
                },
                super::RecoveredMeeting {
                    meeting_id: b.id.clone(),
                    note_id: b.note_id.clone(),
                    title: "Vendor call".into(),
                },
            ]
        );
        // the same report again adds nothing
        assert_eq!(super::note_recovered_kept_local(&state, &ids).len(), 2);
        let left = super::dismiss_recovered(&state, Some(&a.id));
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].meeting_id, b.id);
        assert!(super::dismiss_recovered(&state, None).is_empty());
    }
}
