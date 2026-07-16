//! Screen-recording commands (M7): ffmpeg sidecar, capture linked to a note
//! as an in-place attachment.

use fly_capture_screen::{CaptureTarget, ScreenRecorder, ScreenSession};
use fly_core::Note;
use serde::Serialize;
use tauri::State;

use crate::models;
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

pub struct ActiveScreenRecording {
    pub session: Box<dyn ScreenSession>,
    pub note_id: String,
    pub rel_path: String,
}

#[derive(Serialize, Clone)]
pub struct ScreenStatus {
    pub active: bool,
    pub note_id: Option<String>,
    pub elapsed_ms: u64,
}

/// Async (like every startup/polling command) so it can't convoy behind a
/// slow synchronous command on the main thread.
#[tauri::command]
pub async fn screen_status(state: State<'_, AppState>) -> Result<ScreenStatus, String> {
    Ok(match state.screen.lock().unwrap().as_ref() {
        Some(s) => ScreenStatus {
            active: true,
            note_id: Some(s.note_id.clone()),
            elapsed_ms: s.session.elapsed_ms(),
        },
        None => ScreenStatus {
            active: false,
            note_id: None,
            elapsed_ms: 0,
        },
    })
}

/// Windows the user can pick for window capture (exact current titles,
/// front-to-back). Empty on platforms without window capture.
#[tauri::command]
pub async fn list_capture_windows() -> CmdResult<Vec<String>> {
    Ok(fly_capture_screen::window_list::list_windows())
}

/// Start capturing the screen (full / window / region) for a note. Downloads
/// the ffmpeg sidecar on first use.
#[tauri::command]
pub async fn start_screen_recording(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    note_id: String,
    target: CaptureTarget,
) -> CmdResult<ScreenStatus> {
    if state.screen.lock().unwrap().is_some() {
        return Err("a screen recording is already in progress".into());
    }
    // Window capture: gdigrab needs the EXACT current title, but titles
    // drift between picking and recording (VM/RDP clients decorate them —
    // the "IDB VDI IT" field failure). Re-resolve against the windows open
    // right now; a vanished window fails HERE with a readable message
    // instead of dying inside ffmpeg with an I/O error.
    #[cfg(windows)]
    let target = match target {
        CaptureTarget::Window { title } => {
            let resolved = fly_capture_screen::window_list::resolve_window_title(&title)
                .ok_or_else(|| {
                    format!(
                        "no open window matches \"{title}\" — it may have been closed or \
                         renamed. Pick the window again."
                    )
                })?;
            CaptureTarget::Window { title: resolved }
        }
        t => t,
    };
    // make sure the note exists before we spin anything up
    state
        .storage
        .lock()
        .unwrap()
        .get_note(&note_id)
        .map_err(|e| e.to_string())?;

    let on_model = {
        let app = app.clone();
        move |p: models::ModelProgress| {
            use tauri::Emitter;
            let _ = app.emit("model:progress", p);
        }
    };
    let ffmpeg = models::ensure_tool(
        &on_model,
        &state.data_dir,
        "ffmpeg",
        &["ffmpeg"],
        "install ffmpeg (macOS: brew install ffmpeg)",
    )
    .await?;

    let file_name = format!(
        "screen-{}.mp4",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let rel_path = format!("attachments/{note_id}/{file_name}");
    let out_path = state.data_dir.join(&rel_path);

    let recorder = fly_capture_screen::ffmpeg::FfmpegScreenRecorder::new(ffmpeg);
    let session = recorder
        .start(target, &out_path)
        .map_err(|e| e.to_string())?;

    let mut guard = state.screen.lock().unwrap();
    *guard = Some(ActiveScreenRecording {
        session,
        note_id: note_id.clone(),
        rel_path,
    });
    Ok(ScreenStatus {
        active: true,
        note_id: Some(note_id),
        elapsed_ms: 0,
    })
}

/// Poster frame for a video attachment: a `.jpg` next to the file
/// (`screen-….mp4` → `screen-….mp4.jpg`), generated lazily the first time the
/// note is opened and reused afterwards. Same ffmpeg sidecar as capture and
/// import — never a second bundle.
#[tauri::command]
pub async fn ensure_video_thumbnail(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    rel_path: String,
) -> CmdResult<String> {
    let video = state.storage.lock().unwrap().attachment_abs_path(&rel_path);
    if !video.is_file() {
        return Err(format!("video not found: {rel_path}"));
    }
    let thumb_rel = format!("{rel_path}.jpg");
    let thumb = state.data_dir.join(&thumb_rel);
    if thumb.is_file() {
        return Ok(thumb_rel);
    }
    let on_model = {
        let app = app.clone();
        move |p: models::ModelProgress| {
            use tauri::Emitter;
            let _ = app.emit("model:progress", p);
        }
    };
    let ffmpeg = models::ensure_tool(
        &on_model,
        &state.data_dir,
        "ffmpeg",
        &["ffmpeg"],
        "install ffmpeg (macOS: brew install ffmpeg)",
    )
    .await?;
    generate_thumbnail(&ffmpeg, &video, &thumb).await?;
    Ok(thumb_rel)
}

/// `thumbnail=n=100` scores the first ~100 decoded frames and emits the most
/// representative one — skips a black/blank lead-in without probing the
/// duration. The scale caps posters at 640px wide (never upscales).
async fn generate_thumbnail(
    ffmpeg: &std::path::Path,
    video: &std::path::Path,
    thumb: &std::path::Path,
) -> CmdResult<()> {
    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.arg("-y")
        .arg("-i")
        .arg(video)
        .args([
            "-vf",
            "thumbnail=n=100,scale=min(640\\,iw):-2",
            "-frames:v",
            "1",
            "-q:v",
            "4",
        ])
        .arg(thumb);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    if !out.status.success() || !thumb.is_file() {
        // never leave a partial jpg behind — it would be cached as the poster
        let _ = std::fs::remove_file(thumb);
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "thumbnail extraction failed: {}",
            stderr.chars().take(400).collect::<String>()
        ));
    }
    Ok(())
}

/// Stop, finalize the MP4, and attach it to the note.
#[tauri::command]
pub async fn stop_screen_recording(state: State<'_, AppState>) -> CmdResult<Note> {
    let active = state
        .screen
        .lock()
        .unwrap()
        .take()
        .ok_or("no screen recording in progress")?;
    let note_id = active.note_id;
    let rel_path = active.rel_path;
    let session = active.session;

    tauri::async_runtime::spawn_blocking(move || session.stop())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    let note = state
        .storage
        .lock()
        .unwrap()
        .add_attachment_in_place(&note_id, &rel_path)
        .map_err(|e| e.to_string())?;
    // capture over → the transcription queue may proceed
    state.jobs_notify.notify_one();
    Ok(note)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The installed sidecar (real data dir) or a PATH ffmpeg — mirrors what
    /// ensure_tool would resolve without triggering a download in tests.
    fn local_ffmpeg() -> Option<std::path::PathBuf> {
        dirs::data_dir()
            .map(|d| d.join("FlyOnTheWall"))
            .and_then(|data| {
                models::artifact("ffmpeg").and_then(|a| models::installed_path(&data, a))
            })
            .or_else(|| models::find_on_path(&["ffmpeg"]))
    }

    /// Real ffmpeg run: synthesize a 2 s clip, extract the poster, check a
    /// JPEG landed. Skips when no ffmpeg is available (like scheduling_e2e).
    #[tokio::test]
    async fn thumbnail_from_generated_clip() {
        let Some(ffmpeg) = local_ffmpeg() else {
            eprintln!("SKIP thumbnail_from_generated_clip: no ffmpeg on this machine");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.mp4");
        let out = tokio::process::Command::new(&ffmpeg)
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=320x240:rate=10",
            ])
            .args(["-pix_fmt", "yuv420p", "-y"])
            .arg(&clip)
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "test clip encode failed");

        let thumb = dir.path().join("clip.mp4.jpg");
        generate_thumbnail(&ffmpeg, &clip, &thumb).await.unwrap();
        let bytes = std::fs::read(&thumb).unwrap();
        assert!(bytes.starts_with(&[0xFF, 0xD8]), "not a JPEG");

        // missing input → error, and no partial jpg is left to be cached
        let missing_thumb = dir.path().join("nope.mp4.jpg");
        let res = generate_thumbnail(&ffmpeg, &dir.path().join("nope.mp4"), &missing_thumb).await;
        assert!(res.is_err());
        assert!(!missing_thumb.exists());
    }

    /// A doomed window capture must fail at START with a readable message —
    /// the old behavior reported "recording" and only surfaced ffmpeg's raw
    /// I/O-error dump at stop time, silently losing the session.
    #[cfg(windows)]
    #[test]
    fn missing_window_fails_at_start_with_readable_error() {
        let Some(ffmpeg) = local_ffmpeg() else {
            eprintln!("SKIP missing_window_fails_at_start: no ffmpeg on this machine");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let recorder = fly_capture_screen::ffmpeg::FfmpegScreenRecorder::new(ffmpeg);
        let err = recorder
            .start(
                CaptureTarget::Window {
                    title: "fotw-window-that-cannot-exist-4f9c1b".into(),
                },
                &dir.path().join("out.mp4"),
            )
            .err()
            .expect("start must fail when the window does not exist")
            .to_string();
        assert!(err.contains("wasn't found"), "unreadable error: {err}");
        assert!(!err.contains("libavdevice"), "banner leaked: {err}");
    }

    /// End-to-end proof of the fix path: spawn a real titled window, find it
    /// via enumeration, resolve a PARTIAL lowercase title (the field-failure
    /// shape), record it for ~2 s, and verify a playable MP4 landed.
    /// #[ignore]: needs an interactive desktop + ffmpeg, and briefly flashes
    /// a console window — run explicitly (cargo test -- --ignored).
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn window_capture_records_a_real_window_e2e() {
        use fly_capture_screen::window_list::{list_windows, pick_title};
        let Some(ffmpeg) = local_ffmpeg() else {
            eprintln!("SKIP window_capture_e2e: no ffmpeg on this machine");
            return;
        };
        let title = "FOTW capture proof 4f9c1b";
        let mut cmd = std::process::Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-Command",
            &format!("$Host.UI.RawUI.WindowTitle = '{title}'; Start-Sleep 30"),
        ]);
        {
            // CREATE_NEW_CONSOLE: without it the child INHERITS the test
            // runner's (hidden) console and no capturable window exists.
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0000_0010);
        }
        let mut helper = cmd.spawn().expect("spawn helper window");

        // The console window takes a beat to appear in enumeration.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let resolved = loop {
            if let Some(t) = pick_title("fotw capture proof", &list_windows()) {
                break t;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "helper window never appeared in list_windows()"
            );
            std::thread::sleep(std::time::Duration::from_millis(250));
        };
        assert_eq!(resolved, title, "partial lowercase title must resolve");

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("capture.mp4");
        let recorder = fly_capture_screen::ffmpeg::FfmpegScreenRecorder::new(ffmpeg);
        let session = recorder
            .start(CaptureTarget::Window { title: resolved }, &out)
            .expect("capture must start against a resolved live window");
        std::thread::sleep(std::time::Duration::from_millis(2500));
        let path = session.stop().expect("capture must finalize");
        let bytes = std::fs::metadata(&path).unwrap().len();
        let _ = helper.kill();
        let _ = helper.wait();
        assert!(
            bytes > 5_000,
            "expected a real MP4, got {bytes} bytes at {}",
            path.display()
        );
    }
}
