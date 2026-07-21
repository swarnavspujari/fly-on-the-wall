//! Recording commands: one active capture session at a time, tied to a
//! meeting row and its note.

use fly_audio::{CaptureConfig, CaptureSession, CaptureState};
use fly_core::{Meeting, RecordingRef};
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

pub struct ActiveRecording {
    pub session: Box<dyn CaptureSession>,
    pub meeting_id: String,
    pub note_id: String,
    /// Signals the live-transcript loop (if any) to end.
    pub live_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

#[derive(Serialize, Clone)]
pub struct RecordingStatus {
    pub active: bool,
    pub state: Option<CaptureState>,
    pub elapsed_ms: u64,
    pub meeting_id: Option<String>,
    pub note_id: Option<String>,
    /// Conditions that will degrade this capture (e.g. muted system output →
    /// silent loopback). Re-checked on every status poll so mid-meeting mutes
    /// surface while there is still time to fix them.
    pub warnings: Vec<String>,
}

impl RecordingStatus {
    fn idle() -> Self {
        Self {
            active: false,
            state: None,
            elapsed_ms: 0,
            meeting_id: None,
            note_id: None,
            warnings: Vec::new(),
        }
    }

    fn from_active(rec: &ActiveRecording, warnings: Vec<String>) -> Self {
        // session warnings first: "loopback never started" outranks
        // "system volume is low" when both apply
        let mut all = rec.session.warnings();
        all.extend(warnings);
        Self {
            active: true,
            state: Some(rec.session.state()),
            elapsed_ms: rec.session.elapsed_ms(),
            meeting_id: Some(rec.meeting_id.clone()),
            note_id: Some(rec.note_id.clone()),
            warnings: all,
        }
    }
}

/// Async (like every startup/polling command) so it can't convoy behind a
/// slow synchronous command on the main thread.
#[tauri::command]
pub async fn recording_status(state: State<'_, AppState>) -> CmdResult<RecordingStatus> {
    Ok(match state.recording.lock().unwrap().as_ref() {
        Some(rec) => RecordingStatus::from_active(rec, state.audio.capture_warnings()),
        None => RecordingStatus::idle(),
    })
}

/// Start recording for a note (created on the fly when none is given).
/// Captures mic + system loopback as separate channels.
#[tauri::command]
pub fn start_recording(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    note_id: Option<String>,
) -> CmdResult<RecordingStatus> {
    start_recording_impl(&app, &state, note_id, None, &[])
}

/// Shared start path — also used by the calendar one-click start, which
/// prefills the note title and meeting attendees (spec item 9).
pub fn start_recording_impl(
    app: &tauri::AppHandle,
    state: &AppState,
    note_id: Option<String>,
    title: Option<String>,
    attendees: &[String],
) -> CmdResult<RecordingStatus> {
    let mut recording = state.recording.lock().unwrap();
    if recording.is_some() {
        return Err("a recording is already in progress".into());
    }

    let (note, meeting, mic_device_id, out_dir) = {
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
        // Calendar attendees arrive as raw emails; the struct form carries
        // them as name = email until the user renames them in the editor.
        let attendees: Vec<fly_core::Attendee> = attendees
            .iter()
            .map(|s| fly_core::Attendee::from_legacy(s))
            .collect();
        let meeting = storage
            .create_meeting(&note.title, &note.id, &attendees)
            .map_err(|e| e.to_string())?;
        // human-readable meeting folder ("recordings/<date> <title>/");
        // the relative paths stored at stop_recording tie it to the meeting
        let out_dir = storage
            .allocate_meeting_dir(&note.title, meeting.started_at)
            .map_err(|e| e.to_string())?;
        let mic_device_id = storage
            .get_setting("capture.mic_device_id")
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        (note, meeting, mic_device_id, out_dir)
    };

    let session = state
        .audio
        .start(CaptureConfig {
            mic_device_id,
            capture_system: true,
            out_dir: out_dir.clone(),
            base_name: "recording".into(),
        })
        .map_err(|e| e.to_string())?;

    let live_stop = Some(crate::live::spawn(app.clone(), meeting.id.clone(), out_dir));
    let active = ActiveRecording {
        session,
        meeting_id: meeting.id,
        note_id: note.id,
        live_stop,
    };
    let status = RecordingStatus::from_active(&active, state.audio.capture_warnings());
    *recording = Some(active);
    Ok(status)
}

#[tauri::command]
pub fn pause_recording(state: State<'_, AppState>) -> CmdResult<RecordingStatus> {
    let mut guard = state.recording.lock().unwrap();
    let rec = guard.as_mut().ok_or("no active recording")?;
    rec.session.pause().map_err(|e| e.to_string())?;
    Ok(RecordingStatus::from_active(
        rec,
        state.audio.capture_warnings(),
    ))
}

#[tauri::command]
pub fn resume_recording(state: State<'_, AppState>) -> CmdResult<RecordingStatus> {
    let mut guard = state.recording.lock().unwrap();
    let rec = guard.as_mut().ok_or("no active recording")?;
    rec.session.resume().map_err(|e| e.to_string())?;
    Ok(RecordingStatus::from_active(
        rec,
        state.audio.capture_warnings(),
    ))
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
    if let Some(stop) = &rec.live_stop {
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let meeting_id = rec.meeting_id.clone();
    let note_id = rec.note_id.clone();
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
        playback_path: output.playback_path.as_ref().and_then(to_rel),
        duration_ms: output.duration_ms,
    };

    // Stash a filesystem-only manifest BEFORE the database write: if that
    // write fails (corrupted/replaced database — seen in the wild after a
    // 2.8-hour recording), the startup self-heal re-attaches the recording
    // from this file. The audio must never depend on SQLite being healthy.
    let meeting = {
        let storage = state.storage.lock().unwrap();
        if let Err(e) = storage.stash_recording_manifest(&meeting_id, &note_id, &recording_ref) {
            tracing::warn!(meeting_id, error = %e, "could not write recording manifest");
        }
        storage
            .end_meeting(&meeting_id, &recording_ref)
            .map_err(|e| {
                format!(
                    "{e} — the recording files are safe in the meeting folder and \
                     will be re-attached automatically on the next launch"
                )
            })?
    };
    // recording over → the transcription queue may proceed
    state.jobs_notify.notify_one();
    Ok(meeting)
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

/// ALL meetings attached to a note, newest first. Re-recording into an open
/// note repoints `notes.meeting_id` at the new meeting, and
/// `get_meeting_for_note` returns only the newest — without this list the
/// earlier meetings (and their transcripts) are intact in storage but
/// unreachable from the UI.
#[tauri::command]
pub fn get_meetings_for_note(
    state: State<'_, AppState>,
    note_id: String,
) -> CmdResult<Vec<Meeting>> {
    let mut meetings = state
        .storage
        .lock()
        .unwrap()
        .meetings_for_note(&note_id)
        .map_err(|e| e.to_string())?;
    meetings.reverse(); // storage returns oldest first
    Ok(meetings)
}

#[tauri::command]
pub fn list_mic_devices(state: State<'_, AppState>) -> CmdResult<Vec<fly_audio::AudioDevice>> {
    state.audio.list_mic_devices().map_err(|e| e.to_string())
}
