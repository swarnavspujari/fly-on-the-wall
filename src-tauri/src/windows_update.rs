//! Windows in-app update install that survives a blocked installer launch.
//!
//! The updater plugin's own JS `install()` runs a before-exit hook that is
//! `AppHandle::cleanup_before_exit()`: it clears the resource table (which
//! holds single-instance's mutex, so the one-copy lock is released) and hides
//! every window, and only THEN calls `ShellExecuteW` on the installer. When
//! antivirus blocks that launch, `ShellExecuteW` fails, the plugin returns the
//! error instead of exiting, and the app is left running hidden with no
//! instance lock — a relaunch would start a second copy on the same data
//! directory. The plugin's `Builder` has no way to replace that hook for the
//! JS path, so Windows updates go through the two commands here instead:
//!
//! * `windows_update_download` checks again, downloads (minisign-verified by
//!   the plugin against the pubkey in `tauri.conf.json`) and keeps the bytes
//!   in memory. Nothing is installed, so a recording that started meanwhile
//!   can still park the update (the frontend decides).
//! * `windows_update_install` installs with a hook that only hides the main
//!   window, and on any failure brings it back, visible and focused.
//!
//! `ShellExecuteW` itself cannot be unit-tested; the hook and the recovery
//! decision are in the small pure piece at the top of this file.

use std::sync::Mutex;

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, Runtime, WebviewWindow};
use tauri_plugin_updater::{Update, UpdaterExt};

/// What the install flow needs from the main window. A trait so the hook and
/// the failure recovery can be tested without a real window.
pub(crate) trait MainWindow {
    fn hide(&self);
    /// Make the window visible and focused again (also from minimized).
    fn restore(&self);
}

impl<R: Runtime> MainWindow for WebviewWindow<R> {
    fn hide(&self) {
        let _ = WebviewWindow::hide(self);
    }
    fn restore(&self) {
        let _ = self.unminimize();
        let _ = self.show();
        let _ = self.set_focus();
    }
}

/// The before-exit hook: hide the main window only. Everything else the
/// default hook does (clearing the resource table) is exactly what drops the
/// single-instance lock, so it is deliberately not done.
pub(crate) fn before_exit(window: &impl MainWindow) {
    window.hide();
}

/// After the install attempt: on failure bring the window back and pass the
/// error on; on success there is nothing to do (the process is exiting).
pub(crate) fn recover_on_failure<T, E>(
    window: &impl MainWindow,
    result: Result<T, E>,
) -> Result<T, E> {
    if result.is_err() {
        window.restore();
    }
    result
}

/// The verified, downloaded update waiting for an explicit install.
#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<(Update, Vec<u8>)>>);

/// Same shape as the plugin's own channel events, so the frontend handles
/// both paths with one function.
#[derive(Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum DownloadEvent {
    #[serde(rename_all = "camelCase")]
    Started {
        content_length: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    Progress {
        chunk_length: usize,
    },
    Finished,
}

fn main_window<R: Runtime>(app: &AppHandle<R>) -> Result<WebviewWindow<R>, String> {
    app.get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())
}

/// Check, download and verify the update; hold it in memory for
/// `windows_update_install`.
#[tauri::command]
pub async fn windows_update_download<R: Runtime>(
    app: AppHandle<R>,
    pending: tauri::State<'_, PendingUpdate>,
    on_event: Channel<DownloadEvent>,
) -> Result<(), String> {
    let window = main_window(&app)?;
    // The builder keeps the config's pubkey, endpoints and install mode (an
    // unsigned or tampered bundle fails `download`'s verification). Its hook
    // replaces the plugin default and travels with the `Update` it returns.
    let update = app
        .updater_builder()
        .on_before_exit(move || before_exit(&window))
        .build()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no update is available".to_string())?;
    let mut started = false;
    let progress = on_event.clone();
    let finished = on_event.clone();
    let bytes = update
        .download(
            move |chunk_length, content_length| {
                if !started {
                    started = true;
                    let _ = progress.send(DownloadEvent::Started { content_length });
                }
                let _ = progress.send(DownloadEvent::Progress { chunk_length });
            },
            move || {
                let _ = finished.send(DownloadEvent::Finished);
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    *pending.0.lock().unwrap_or_else(|p| p.into_inner()) = Some((update, bytes));
    Ok(())
}

/// Run the installer. On Windows the plugin exits the process once the
/// installer has launched; on any failure the app stays up, visible and
/// single-instance, and the error goes to the frontend. The downloaded
/// update is consumed either way: "Try again" checks and downloads afresh.
#[tauri::command]
pub async fn windows_update_install<R: Runtime>(
    app: AppHandle<R>,
    pending: tauri::State<'_, PendingUpdate>,
) -> Result<(), String> {
    let window = main_window(&app)?;
    let taken = pending.0.lock().unwrap_or_else(|p| p.into_inner()).take();
    let result = match taken {
        None => Err("no downloaded update to install".to_string()),
        Some((update, bytes)) => {
            tauri::async_runtime::spawn_blocking(move || update.install(bytes))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r.map_err(|e| e.to_string()))
        }
    };
    recover_on_failure(&window, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeWindow(RefCell<Vec<&'static str>>);
    impl MainWindow for FakeWindow {
        fn hide(&self) {
            self.0.borrow_mut().push("hide");
        }
        fn restore(&self) {
            self.0.borrow_mut().push("restore");
        }
    }

    #[test]
    fn hook_only_hides_the_window() {
        let w = FakeWindow::default();
        before_exit(&w);
        assert_eq!(*w.0.borrow(), vec!["hide"]);
    }

    #[test]
    fn failed_install_restores_the_window_and_keeps_the_error() {
        let w = FakeWindow::default();
        before_exit(&w);
        let r: Result<(), String> = recover_on_failure(&w, Err("installer blocked".into()));
        assert_eq!(r, Err("installer blocked".to_string()));
        assert_eq!(*w.0.borrow(), vec!["hide", "restore"]);
    }

    #[test]
    fn successful_install_leaves_the_window_alone() {
        let w = FakeWindow::default();
        let r: Result<u8, String> = recover_on_failure(&w, Ok(1));
        assert_eq!(r, Ok(1));
        assert!(w.0.borrow().is_empty());
    }
}
