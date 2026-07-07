//! Transcription scheduling: recording always wins.
//!
//! A dropped or degraded recording is unrecoverable; transcription can always
//! happen later. So the full pipeline never STARTS while any capture is
//! active — jobs queue up (persistently, see looma-storage jobs.rs) and a
//! single worker drains them one at a time once recording ends. Serializing
//! jobs also means at most one whisper/sherpa sidecar set runs at once.
//!
//! The queue is keyed by meeting id only; `pipeline::run_with` resolves the
//! recording files from the meeting row at execution time.
//!
//! `tick` is the tauri-free core (like `pipeline::run_with`) so tests can
//! drive scheduling decisions without a webview runtime.

use std::time::Duration;

use tauri::{Emitter, Manager};

use crate::models;
use crate::pipeline::{self, PipelineProgress, StageSink};
use crate::state::AppState;

/// Attempts per job before it is parked as failed (user can re-trigger).
pub const MAX_ATTEMPTS: u32 = 3;
/// Queue stage surfaced to the UI while a job waits its turn.
pub const WAITING_STAGE: &str = "waiting";
/// Pause after a failed attempt so a transient fault (e.g. a download
/// hiccup) gets breathing room before the retry.
const RETRY_DELAY: Duration = Duration::from_secs(10);
/// Poll fallback: a missed notify may delay the queue, never stall it.
const IDLE_POLL: Duration = Duration::from_secs(5);

/// What one scheduling step did.
pub enum Tick {
    /// Nothing queued.
    Idle,
    /// Jobs may be queued but a capture is active — recording wins.
    RecordingActive,
    Completed(String),
    Retrying {
        meeting_id: String,
        attempts: u32,
        error: String,
    },
    GaveUp {
        meeting_id: String,
        error: String,
    },
}

/// Queue a meeting for transcription (idempotent) and surface it as
/// "waiting" right away. The worker picks it up when no recording is active.
pub fn enqueue(state: &AppState, on_stage: StageSink<'_>, meeting_id: &str) -> Result<(), String> {
    let queued = state
        .storage
        .lock()
        .unwrap()
        .enqueue_transcription(meeting_id)
        .map_err(|e| e.to_string())?;
    if queued {
        mark_waiting(state, on_stage, meeting_id, None);
    }
    state.jobs_notify.notify_one();
    Ok(())
}

/// Run at most one queued job to completion. Never starts a pipeline while
/// audio or screen capture is active.
pub async fn tick(
    state: &AppState,
    on_stage: StageSink<'_>,
    on_model: models::ProgressSink<'_>,
) -> Tick {
    if recording_active(state) {
        return Tick::RecordingActive;
    }
    let job = state
        .storage
        .lock()
        .unwrap()
        .next_transcription_job()
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "reading transcription queue failed");
            None
        });
    let Some(job) = job else { return Tick::Idle };
    let meeting_id = job.meeting_id.clone();

    set_job_state(state, |s| s.mark_transcription_running(&meeting_id));
    // clear the "waiting" marker so run_with's per-meeting guard can pass
    state.pipeline_stage.lock().unwrap().remove(&meeting_id);

    match pipeline::run_with(state, on_stage, on_model, &meeting_id).await {
        Ok(_) => {
            set_job_state(state, |s| s.mark_transcription_done(&meeting_id));
            Tick::Completed(meeting_id)
        }
        Err(error) => {
            tracing::error!(meeting_id, error = %error, "transcription pipeline failed");
            state.pipeline_stage.lock().unwrap().remove(&meeting_id);
            let attempts = job.attempts + 1;
            if attempts < MAX_ATTEMPTS {
                set_job_state(state, |s| {
                    s.requeue_transcription(&meeting_id, attempts, &error)
                });
                Tick::Retrying {
                    meeting_id,
                    attempts,
                    error,
                }
            } else {
                set_job_state(state, |s| {
                    s.mark_transcription_failed(&meeting_id, attempts, &error)
                });
                Tick::GaveUp { meeting_id, error }
            }
        }
    }
}

/// Spawn the queue worker (called once at app setup). Recovers jobs a
/// previous process left 'running', then loops: run a job when allowed,
/// otherwise wait for a nudge (enqueue, recording stop) or the poll tick.
pub fn spawn<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let on_stage = stage_emitter(&app);
        let on_model = {
            let app = app.clone();
            move |p: models::ModelProgress| {
                let _ = app.emit("model:progress", p);
            }
        };
        recover(&app.state::<AppState>(), &on_stage);
        loop {
            let state = app.state::<AppState>();
            match tick(&state, &on_stage, &on_model).await {
                Tick::Completed(meeting_id) => emit_final(&on_stage, &meeting_id, None),
                Tick::Retrying {
                    meeting_id,
                    attempts,
                    error,
                } => {
                    tracing::warn!(meeting_id, attempts, error = %error, "transcription retry queued");
                    mark_waiting(
                        &state,
                        &on_stage,
                        &meeting_id,
                        Some(format!("retrying (attempt {attempts} failed)")),
                    );
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                Tick::GaveUp { meeting_id, error } => {
                    emit_final(&on_stage, &meeting_id, Some(error))
                }
                Tick::Idle | Tick::RecordingActive => {
                    let _ = tokio::time::timeout(IDLE_POLL, state.jobs_notify.notified()).await;
                }
            }
        }
    });
}

/// Startup recovery: jobs a previous process left 'running' died with it —
/// re-queue them, then resurface every queued job as "waiting" in the UI.
pub fn recover(state: &AppState, on_stage: StageSink<'_>) {
    let (reset, queued) = {
        let storage = state.storage.lock().unwrap();
        (
            storage.reset_running_transcriptions().unwrap_or(0),
            storage.queued_transcription_ids().unwrap_or_default(),
        )
    };
    if reset > 0 {
        tracing::info!(reset, "requeued transcriptions interrupted by shutdown");
    }
    for id in queued {
        mark_waiting(state, on_stage, &id, None);
    }
}

/// The `pipeline:progress` bridge shared by the worker and the enqueue
/// commands.
pub fn stage_emitter<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> impl Fn(PipelineProgress) + Send + Sync {
    let app = app.clone();
    move |p: PipelineProgress| {
        let _ = app.emit("pipeline:progress", p);
    }
}

fn recording_active(state: &AppState) -> bool {
    state.recording.lock().unwrap().is_some() || state.screen.lock().unwrap().is_some()
}

fn mark_waiting(
    state: &AppState,
    on_stage: StageSink<'_>,
    meeting_id: &str,
    detail: Option<String>,
) {
    state
        .pipeline_stage
        .lock()
        .unwrap()
        .insert(meeting_id.to_string(), WAITING_STAGE.to_string());
    on_stage(PipelineProgress {
        meeting_id: meeting_id.to_string(),
        stage: WAITING_STAGE.into(),
        detail,
        done: false,
        error: None,
    });
}

/// Terminal `pipeline:progress` event (mirrors what `pipeline::run` emitted
/// before scheduling existed).
fn emit_final(on_stage: StageSink<'_>, meeting_id: &str, error: Option<String>) {
    on_stage(PipelineProgress {
        meeting_id: meeting_id.to_string(),
        stage: if error.is_some() { "error" } else { "done" }.into(),
        detail: None,
        done: true,
        error,
    });
}

/// Job bookkeeping must never take the pipeline down; log and continue.
fn set_job_state(
    state: &AppState,
    f: impl FnOnce(&looma_storage::Storage) -> looma_storage::Result<()>,
) {
    if let Err(e) = f(&state.storage.lock().unwrap()) {
        tracing::error!(error = %e, "updating transcription job state failed");
    }
}
