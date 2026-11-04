//! Library backups in the app: the pieces around `fly_storage::backup` that
//! need the running app. The launch-time backup itself runs in
//! [`crate::state::open_library`], before the database opens; this module
//! adds the compression job's own backup, and the commands behind Settings ›
//! Your files and privacy ("Backups: N · Open folder") and the one-time
//! "couldn't save a backup" notice.
//!
//! Lock order: the storage lock is never held while a backup copies the
//! database (the copy reads through its own connection), and nothing here
//! takes `recording` or `screen`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use fly_storage::backup::{self, Outcome};
use serde::Serialize;
use tauri::{Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Event carrying a [`BackupStatus`] when it changes while the app runs (the
/// compression job's backup failed or skipped).
pub const BACKUP_EVENT: &str = "backup:status";

/// What Settings and the notice show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupStatus {
    /// Backups on disk: the newest [`backup::KEEP_BACKUPS`] plus the last
    /// copy from before each recent schema change.
    pub count: usize,
    /// While the one-time notice is due, why (a [`backup::NoticeKind`]'s
    /// string: `space` / `failed` before an upgrade, `compression_space` /
    /// `compression_failed` before the compression job's first pass);
    /// otherwise none.
    pub notice: Option<String>,
}

pub fn status(state: &AppState) -> BackupStatus {
    let notice = backup::pending_notice(&state.storage.lock().unwrap());
    BackupStatus {
        count: backup::list(&state.data_dir).len(),
        notice: notice.map(|k| k.as_str().to_string()),
    }
}

#[tauri::command]
pub fn backup_status(state: State<'_, AppState>) -> CmdResult<BackupStatus> {
    Ok(status(&state))
}

/// The user dismissed the notice; it does not come back.
#[tauri::command]
pub fn dismiss_backup_notice(state: State<'_, AppState>) -> CmdResult<()> {
    backup::dismiss_notice(&state.storage.lock().unwrap());
    Ok(())
}

/// "Open folder": the backups folder, created if there is none yet so the
/// button always has somewhere to go.
#[tauri::command]
pub fn reveal_backups_dir(app: tauri::AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let dir = state.data_dir.join(backup::BACKUP_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .reveal_item_in_dir(&dir)
        .map_err(|e| e.to_string())
}

/// The compression job's pre-first-pass backup, when this install still owes
/// one (`None` when it doesn't). Reads through its own connection, so the
/// app keeps writing meanwhile; records the result and raises the notice if
/// it couldn't be taken.
pub fn backup_before_compression(
    state: &AppState,
    version: &str,
    free_space: &dyn Fn(&Path) -> Option<u64>,
) -> Option<Outcome> {
    let due = backup::compression_backup_due(&state.storage.lock().unwrap());
    if !due {
        return None;
    }
    let outcome = backup::before_compression(&state.data_dir, version, free_space);
    backup::after_compression_backup(&state.storage.lock().unwrap(), version, &outcome);
    Some(outcome)
}

/// Called by the compression job before a pass that has work. Tries at most
/// once per run of the app: a backup that couldn't be taken is retried next
/// launch, not every half hour.
pub fn before_first_compression_pass(app: &tauri::AppHandle) {
    static TRIED: AtomicBool = AtomicBool::new(false);
    if TRIED.swap(true, Ordering::SeqCst) {
        return;
    }
    let state = app.state::<AppState>();
    let attempted = backup_before_compression(
        &state,
        crate::state::APP_VERSION,
        &crate::audio_store::free_space,
    );
    if attempted.is_some() {
        let _ = app.emit(BACKUP_EVENT, status(&state));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_secrets::MemorySecretStore;
    use std::sync::Arc;

    fn app_state(dir: &Path) -> AppState {
        AppState::init_with(dir.to_path_buf(), Arc::new(MemorySecretStore::default())).unwrap()
    }

    fn roomy(_: &Path) -> Option<u64> {
        Some(u64::MAX)
    }

    #[test]
    fn status_counts_backups_and_reports_the_notice() {
        let dir = tempfile::tempdir().unwrap();
        let state = app_state(dir.path());
        assert_eq!(
            status(&state),
            BackupStatus {
                count: 0,
                notice: None
            }
        );
        let outcome = backup::before_compression(dir.path(), "2.5.0", &roomy);
        assert!(matches!(outcome, Outcome::Saved(_)), "{outcome:?}");
        backup::after_compression_backup(
            &state.storage.lock().unwrap(),
            "2.5.0",
            &Outcome::Failed("disk full".into()),
        );
        let s = status(&state);
        assert_eq!(s.count, 1);
        assert_eq!(s.notice.as_deref(), Some("compression_failed"));
        // Serialized the way the frontend reads it.
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::json!({ "count": 1, "notice": "compression_failed" })
        );

        backup::dismiss_notice(&state.storage.lock().unwrap());
        assert_eq!(status(&state).notice, None);
    }

    /// The compression job's backup: owed once per install, never while the
    /// storage lock is held (the real `Mutex<Storage>` is in play here, and
    /// the copy runs while another thread uses it).
    #[test]
    fn the_compression_backup_is_taken_once_and_does_not_block_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(app_state(dir.path()));
        state
            .storage
            .lock()
            .unwrap()
            .create_note("a note", None)
            .unwrap();

        // A writer keeps using the app's own storage while the backup runs.
        let writer = {
            let state = state.clone();
            std::thread::spawn(move || {
                for i in 0..20 {
                    state
                        .storage
                        .lock()
                        .unwrap()
                        .create_note(&format!("note {i}"), None)
                        .unwrap();
                }
            })
        };
        let first = backup_before_compression(&state, "2.5.0", &roomy);
        writer.join().unwrap();
        assert!(matches!(first, Some(Outcome::Saved(_))), "{first:?}");
        assert_eq!(backup::list(dir.path()).len(), 1);

        // Done once: later passes (and launches) owe nothing.
        assert!(backup_before_compression(&state, "2.5.0", &roomy).is_none());
        assert_eq!(backup::list(dir.path()).len(), 1);
    }

    #[test]
    fn a_skipped_compression_backup_raises_the_notice_and_is_owed_again() {
        let dir = tempfile::tempdir().unwrap();
        let state = app_state(dir.path());
        let outcome = backup_before_compression(&state, "2.5.0", &|_| Some(1024));
        assert!(
            matches!(outcome, Some(Outcome::NoSpace { .. })),
            "{outcome:?}"
        );
        assert_eq!(status(&state).notice.as_deref(), Some("compression_space"));
        assert_eq!(status(&state).count, 0);
        // Still owed: the next launch tries again.
        assert!(backup::compression_backup_due(
            &state.storage.lock().unwrap()
        ));
    }

    /// The user dismisses the notice and the next launch's compression
    /// backup is skipped again: the same app version does not tell them twice.
    #[test]
    fn a_dismissed_notice_stays_dismissed_when_the_compression_backup_is_skipped_again() {
        let dir = tempfile::tempdir().unwrap();
        let state = app_state(dir.path());
        let tight = |_: &Path| Some(1024);
        assert!(backup_before_compression(&state, "2.5.0", &tight).is_some());
        assert_eq!(status(&state).notice.as_deref(), Some("compression_space"));
        backup::dismiss_notice(&state.storage.lock().unwrap());

        // Launch after launch: still owed, still skipped, never raised again.
        for _ in 0..3 {
            let again = backup_before_compression(&state, "2.5.0", &tight);
            assert!(matches!(again, Some(Outcome::NoSpace { .. })), "{again:?}");
            assert_eq!(status(&state).notice, None);
        }
        // A new app version is a new upgrade and tells again.
        assert!(backup_before_compression(&state, "2.5.1", &tight).is_some());
        assert_eq!(status(&state).notice.as_deref(), Some("compression_space"));
    }
}
