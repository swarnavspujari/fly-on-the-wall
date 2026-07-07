//! Screen-recording commands (M7): ffmpeg sidecar, capture linked to a note
//! as an in-place attachment.

use looma_capture_screen::{CaptureTarget, ScreenRecorder, ScreenSession};
use looma_core::Note;
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

#[tauri::command]
pub fn screen_status(state: State<'_, AppState>) -> ScreenStatus {
    match state.screen.lock().unwrap().as_ref() {
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
    }
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

    let recorder = looma_capture_screen::ffmpeg::FfmpegScreenRecorder::new(ffmpeg);
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
