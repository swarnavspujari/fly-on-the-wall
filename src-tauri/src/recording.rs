//! Recording commands: one active capture session at a time, tied to a
//! meeting row and its note.

use looma_audio::{CaptureConfig, CaptureSession, CaptureState};
use looma_core::{Meeting, RecordingRef};
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

pub struct ActiveRecording {
    pub session: Box<dyn CaptureSession>,
    pub meeting_id: String,
    pub note_id: String,
}

#[derive(Serialize, Clone)]
pub struct RecordingStatus {
    pub active: bool,
    pub state: Option<CaptureState>,
    pub elapsed_ms: u64,
    pub meeting_id: Option<String>,
    pub note_id: Option<String>,
}

impl RecordingStatus {
    fn idle() -> Self {
        Self {
            active: false,
            state: None,
            elapsed_ms: 0,
            meeting_id: None,
            note_id: None,
        }
    }

    fn from_active(rec: &ActiveRecording) -> Self {
        Self {
            active: true,
            state: Some(rec.session.state()),
            elapsed_ms: rec.session.elapsed_ms(),
            meeting_id: Some(rec.meeting_id.clone()),
            note_id: Some(rec.note_id.clone()),
        }
    }
}

#[tauri::command]
pub fn recording_status(state: State<'_, AppState>) -> RecordingStatus {
    match state.recording.lock().unwrap().as_ref() {
        Some(rec) => RecordingStatus::from_active(rec),
        None => RecordingStatus::idle(),
    }
}

/// Start recording for a note (created on the fly when none is given).
/// Captures mic + system loopback as separate channels.
#[tauri::command]
pub fn start_recording(
    state: State<'_, AppState>,
    note_id: Option<String>,
) -> CmdResult<RecordingStatus> {
    start_recording_impl(&state, note_id, None, &[])
}

/// Shared start path — also used by the calendar one-click start, which
/// prefills the note title and meeting attendees (spec item 9).
pub fn start_recording_impl(
    state: &AppState,
    note_id: Option<String>,
    title: Option<String>,
    attendees: &[String],
) -> CmdResult<RecordingStatus> {
    let mut recording = state.recording.lock().unwrap();
    if recording.is_some() {
        return Err("a recording is already in progress".into());
    }

    let (note, meeting, mic_device_id) = {
        let storage = state.storage.lock().unwrap();
        let note = match note_id {
            Some(id) => storage.get_note(&id).map_err(|e| e.to_string())?,
            None => {
                let title = title.unwrap_or_else(|| {
                    format!("Meeting {}", chrono::Local::now().format("%Y-%m-%d %H:%M"))
                });
                storage
                    .create_note(&title, None)
                    .map_err(|e| e.to_string())?
            }
        };
        let meeting = storage
            .create_meeting(&note.title, &note.id, attendees)
            .map_err(|e| e.to_string())?;
        let mic_device_id = storage
            .get_setting("capture.mic_device_id")
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        (note, meeting, mic_device_id)
    };

    let out_dir = state.data_dir.join("recordings").join(&meeting.id);
    let session = state
        .audio
        .start(CaptureConfig {
            mic_device_id,
            capture_system: true,
            out_dir,
            base_name: "recording".into(),
        })
        .map_err(|e| e.to_string())?;

    let active = ActiveRecording {
        session,
        meeting_id: meeting.id,
        note_id: note.id,
    };
    let status = RecordingStatus::from_active(&active);
    *recording = Some(active);
    Ok(status)
}

#[tauri::command]
pub fn pause_recording(state: State<'_, AppState>) -> CmdResult<RecordingStatus> {
    let mut guard = state.recording.lock().unwrap();
    let rec = guard.as_mut().ok_or("no active recording")?;
    rec.session.pause().map_err(|e| e.to_string())?;
    Ok(RecordingStatus::from_active(rec))
}

#[tauri::command]
pub fn resume_recording(state: State<'_, AppState>) -> CmdResult<RecordingStatus> {
    let mut guard = state.recording.lock().unwrap();
    let rec = guard.as_mut().ok_or("no active recording")?;
    rec.session.resume().map_err(|e| e.to_string())?;
    Ok(RecordingStatus::from_active(rec))
}

/// Stop, finalize WAVs + mixdown (blocking work off the IPC thread), and
/// attach the recording to its meeting.
#[tauri::command]
pub async fn stop_recording(state: State<'_, AppState>) -> CmdResult<Meeting> {
    let rec = state
        .recording
        .lock()
        .unwrap()
        .take()
        .ok_or("no active recording")?;
    let meeting_id = rec.meeting_id.clone();
    let session = rec.session;

    let output = tauri::async_runtime::spawn_blocking(move || session.stop())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    let to_rel = |p: &std::path::PathBuf| -> Option<String> {
        p.strip_prefix(&state.data_dir)
            .ok()
            .map(|r| r.to_string_lossy().replace('\\', "/"))
    };
    let recording_ref = RecordingRef {
        mic_path: output.mic_path.as_ref().and_then(to_rel),
        system_path: output.system_path.as_ref().and_then(to_rel),
        mixed_path: output.mixed_path.as_ref().and_then(to_rel),
        duration_ms: output.duration_ms,
    };

    state
        .storage
        .lock()
        .unwrap()
        .end_meeting(&meeting_id, &recording_ref)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_meeting_for_note(
    state: State<'_, AppState>,
    note_id: String,
) -> CmdResult<Option<Meeting>> {
    state
        .storage
        .lock()
        .unwrap()
        .get_meeting_for_note(&note_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_mic_devices(state: State<'_, AppState>) -> CmdResult<Vec<looma_audio::AudioDevice>> {
    state.audio.list_mic_devices().map_err(|e| e.to_string())
}
