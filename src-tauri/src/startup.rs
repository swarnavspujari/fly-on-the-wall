//! Startup: opening the library and everything that waits for it.
//!
//! Normally the database opens synchronously inside the Tauri `setup` hook,
//! as it always has. A first launch after a schema update can take tens of
//! seconds on a large library (the v3 step, see fly-storage `SCHEMA_V3.md`),
//! and `setup` runs on the main thread: the window would sit frozen. So when
//! [`fly_storage::Storage::pending_upgrade`] says the open will run that
//! step, it runs on a worker thread instead, reporting through
//! [`library_status`] and the [`UPGRADE_EVENT`] event; the frontend shows an
//! "Upgrading your library" screen and mounts the app once the phase is
//! `ready`. Until then `AppState` isn't managed: nothing may call
//! `app.state::<AppState>()` that can run before [`finish`] (the exit
//! handler uses `try_state`; commands taking `State<AppState>` return an
//! error instead of panicking).

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;
use crate::{audio_store, embeddings, hw, llm_commands, models, privacy, scheduler, state};

/// Event carrying a [`LibraryStatus`] whenever the startup upgrade moves.
pub const UPGRADE_EVENT: &str = "library:upgrade";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LibraryPhase {
    /// `AppState` is managed: every command works.
    Ready,
    /// The library is being upgraded in the background.
    Upgrading,
    /// The open failed; the error dialog is up and the app will exit.
    Failed,
}

/// `done` / `total` are progress units (a fraction for a determinate bar,
/// not meetings); both are 0 until the step's first report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LibraryStatus {
    pub phase: LibraryPhase,
    pub done: u64,
    pub total: u64,
}

impl LibraryStatus {
    const READY: Self = Self {
        phase: LibraryPhase::Ready,
        done: 0,
        total: 0,
    };
}

/// Managed from the start of `setup`, so [`library_status`] works before
/// `AppState` exists.
pub struct LibraryGate(Mutex<LibraryStatus>);

impl LibraryGate {
    pub fn ready() -> Self {
        Self(Mutex::new(LibraryStatus::READY))
    }

    fn set(&self, app: &AppHandle, status: LibraryStatus) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = status;
        let _ = app.emit(UPGRADE_EVENT, status);
    }
}

/// Where startup is. Callable before `AppState` exists (it doesn't take
/// `State<AppState>`), so the frontend asks it before mounting the app.
#[tauri::command]
pub fn library_status(app: AppHandle) -> LibraryStatus {
    match app.try_state::<LibraryGate>() {
        Some(gate) => *gate.0.lock().unwrap_or_else(|e| e.into_inner()),
        // Only before `setup` has run.
        None if app.try_state::<AppState>().is_some() => LibraryStatus::READY,
        None => LibraryStatus {
            phase: LibraryPhase::Upgrading,
            done: 0,
            total: 0,
        },
    }
}

/// True when opening the data dir will run a slow one-time schema step.
/// Any error (no data dir, unreadable file) says no: the synchronous open
/// then reports it through the startup dialog exactly as before.
pub fn upgrade_pending() -> bool {
    let Ok(dir) = state::prepared_data_dir() else {
        return false;
    };
    match fly_storage::Storage::pending_upgrade(&dir) {
        Ok(Some(p)) => {
            tracing::info!(
                from_version = p.from_version,
                transcripts = p.transcripts,
                "library upgrade pending: opening in the background"
            );
            true
        }
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(error = %e, "library upgrade check failed; opening normally");
            false
        }
    }
}

/// Open the library on a worker thread, reporting progress, then [`finish`]
/// startup. A failure reaches the same dialog and exit as the synchronous
/// path.
pub fn open_in_background(app: AppHandle) {
    let gate = LibraryGate(Mutex::new(LibraryStatus {
        phase: LibraryPhase::Upgrading,
        done: 0,
        total: 0,
    }));
    app.manage(gate);
    tauri::async_runtime::spawn_blocking(move || {
        let gate = app.state::<LibraryGate>();
        // A panic here (in the upgrade or the startup tail) must reach the
        // error dialog too, not leave "Upgrading" on screen forever: this
        // task's handle is dropped, so nobody else would see it.
        let opened = catch_panic(|| {
            let app_state = AppState::init_with_progress(&mut |p| {
                gate.set(
                    &app,
                    LibraryStatus {
                        phase: LibraryPhase::Upgrading,
                        done: p.done,
                        total: p.total,
                    },
                )
            })?;
            finish(&app, app_state);
            Ok(())
        });
        match opened {
            Ok(()) => gate.set(&app, LibraryStatus::READY),
            Err(e) => {
                gate.set(
                    &app,
                    LibraryStatus {
                        phase: LibraryPhase::Failed,
                        done: 0,
                        total: 0,
                    },
                );
                storage_failed(&app, &e);
            }
        }
    });
}

/// `f`'s result, with a panic turned into an error carrying its message.
fn catch_panic(f: impl FnOnce() -> anyhow::Result<()>) -> anyhow::Result<()> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(panic) => {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            Err(anyhow::anyhow!(
                "opening the library stopped unexpectedly: {msg}"
            ))
        }
    }
}

/// A failed storage open must NEVER be a silent flash-crash: the window
/// closes before the user can read anything and it looks like the app is
/// broken (seen in the wild with a database that was replaced under a live
/// connection). Tell them what happened and that their data folder is
/// intact, then exit cleanly.
pub fn storage_failed(app: &AppHandle, e: &anyhow::Error) -> ! {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
    tracing::error!(error = %e, "storage failed to open");
    app.dialog()
        .message(format!(
            "Fly on the Wall couldn't open its database:\n\n{e}\n\n\
             Your recordings, notes, and transcripts are separate \
             files and are intact in the app data folder. Close any \
             other program that may be using the database (or restore \
             a backup of flyonthewall.db) and start the app again."
        ))
        .kind(MessageDialogKind::Error)
        .title("Fly on the Wall can't start")
        .blocking_show();
    std::process::exit(1);
}

/// Everything startup does once the library is open, in this order, on
/// either path: webview file access, settings migrations, `AppState` itself,
/// then the background workers that use it.
pub fn finish(app: &AppHandle, app_state: AppState) {
    // Let the webview stream recordings and attachments (audio player,
    // video thumbnails) over the asset protocol — those two folders only.
    // The database, logs and settings stay unreadable from the webview even
    // if a script ever ran there.
    for sub in ["recordings", "attachments"] {
        let dir = app_state.data_dir.join(sub);
        let _ = std::fs::create_dir_all(&dir);
        let _ = app.asset_protocol_scope().allow_directory(&dir, true);
    }
    // Saved Claude Sonnet/Haiku picks from older releases → their 5.5
    // successors (a settings-row rewrite; Opus/Fable picks untouched).
    {
        let storage = app_state.storage.lock().unwrap();
        llm_commands::migrate_anthropic_model(&storage);
        // Consents recorded while the prompt couldn't show are void.
        llm_commands::reset_unseen_cloud_consent(&storage);
        privacy::record_pending_wipe(&storage);
    }
    privacy::spawn_disk_encryption_check(app_state.data_dir.clone());
    app.manage(app_state);
    // Warm the hardware cache off the startup path: detection shells out to
    // nvidia-smi and must never sit in front of first paint.
    {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = tauri::async_runtime::spawn_blocking(move || {
                let state = handle.state::<AppState>();
                hw::detect_and_cache(&state.storage.lock().unwrap());
            })
            .await;
        });
    }
    // Reclaim disk from download temp files a crashed run stranded
    // (per-attempt unique names — nothing else ever opens them).
    {
        let data_dir = app.state::<AppState>().data_dir.clone();
        tauri::async_runtime::spawn(async move {
            let _ = tauri::async_runtime::spawn_blocking(move || {
                models::sweep_stale_downloads(&data_dir, std::time::Duration::from_secs(60 * 60));
            })
            .await;
        });
    }
    // Recording self-heal: re-attach (or fully resurrect) finished
    // recordings whose database write was lost — the manifests written at
    // stop time make this possible even after the database itself was
    // replaced. Off the startup path; a healthy install no-ops.
    {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = tauri::async_runtime::spawn_blocking(move || {
                let state = handle.state::<AppState>();
                let healed = state.storage.lock().unwrap().self_heal_recordings();
                match healed {
                    Ok(r) if !r.is_empty() => {
                        tracing::info!(
                            attached = r.attached.len(),
                            resurrected = r.resurrected.len(),
                            kept_local_unconfirmed = r.kept_local_unconfirmed.len(),
                            "recording self-heal repaired meetings"
                        );
                        // Kept local only because the manifest didn't say:
                        // ask the user to review them.
                        if !r.kept_local_unconfirmed.is_empty() {
                            let entries = privacy::note_recovered_kept_local(
                                &state,
                                &r.kept_local_unconfirmed,
                            );
                            let _ = handle.emit(privacy::RECOVERED_KEPT_LOCAL_EVENT, entries);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "recording self-heal failed")
                    }
                }
            })
            .await;
        });
    }
    // Drain queued transcriptions (incl. jobs surviving a restart) whenever
    // no recording is active.
    scheduler::spawn(app.clone());
    // Semantic-search indexer: chunk-backfills existing content, then embeds
    // pending chunks through Ollama in the background. Fully best-effort —
    // without Ollama, search stays FTS-only.
    embeddings::spawn(app.clone());
    // Audio retention: shortly after startup, then daily.
    privacy::spawn_retention(app.clone());
    // Recording storage v2: losslessly compress finished recordings in the
    // background (low priority, steps aside for real work).
    audio_store::spawn(app.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `frontend/src/components/LibraryGate.tsx` reads, from the
    /// command and the event alike.
    #[test]
    fn a_panic_while_opening_becomes_an_error() {
        let e = catch_panic(|| panic!("boom in the v3 step")).unwrap_err();
        assert!(e.to_string().contains("boom in the v3 step"), "{e}");
        let e = catch_panic(|| panic!("{}", String::from("owned message"))).unwrap_err();
        assert!(e.to_string().contains("owned message"), "{e}");
        assert!(catch_panic(|| Ok(())).is_ok());
        assert!(catch_panic(|| Err(anyhow::anyhow!("plain error"))).is_err());
    }

    #[test]
    fn library_status_serializes_as_the_frontend_expects() {
        let upgrading = LibraryStatus {
            phase: LibraryPhase::Upgrading,
            done: 3,
            total: 9,
        };
        assert_eq!(
            serde_json::to_value(upgrading).unwrap(),
            serde_json::json!({ "phase": "upgrading", "done": 3, "total": 9 })
        );
        assert_eq!(
            serde_json::to_value(LibraryStatus::READY).unwrap()["phase"],
            "ready"
        );
        assert_eq!(
            serde_json::to_value(LibraryPhase::Failed).unwrap(),
            "failed"
        );
    }
}
