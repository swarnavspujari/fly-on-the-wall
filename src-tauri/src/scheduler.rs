//! Transcription scheduling: recording always wins.
//!
//! A dropped or degraded recording is unrecoverable; transcription can always
//! happen later. So the full pipeline never STARTS while any capture is
//! active — jobs queue up (persistently, see fly-storage jobs.rs) and a
//! single worker drains them one at a time once recording ends. Serializing
//! jobs also means at most one whisper/sherpa sidecar set runs at once.
//!
//! The queue is keyed by meeting id only; `pipeline::run_with` resolves the
//! recording files from the meeting row at execution time.
//!
//! The AI pass after a transcription (cleanup, then item extraction) is not
//! part of that queue's work. The worker only queues it (`ai_queue`) and
//! starts the next job; a second worker runs the passes one at a time. A slow
//! or runaway model therefore delays the pass behind it, never a transcription.
//!
//! `tick` and `handle_tick` are the tauri-free core (like
//! `pipeline::run_with`) so tests can drive scheduling decisions without a
//! webview runtime.

use std::time::Duration;

use tauri::{Emitter, Manager};

use crate::ai_queue::{AiJob, AiJobKind};
use crate::llm_commands::AiSkips;
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
#[derive(Debug)]
pub enum Tick {
    /// Nothing queued.
    Idle,
    /// Jobs may be queued but a capture is active — recording wins.
    RecordingActive,
    Completed(String),
    /// The run stopped because the user asked it to (note deleted, import
    /// cancelled): job row already removed, nothing to retry or surface.
    Cancelled(String),
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
        // A new job: a cancel left over from an earlier run of this meeting
        // must not stop it. (Only here: a retry of the same job keeps a
        // pending cancel, see `tick`.)
        state.cancel_requests.lock().unwrap().remove(meeting_id);
        // The earlier run's AI pass, if it hasn't started, would work on a
        // transcript this run replaces; this run queues its own when done.
        if state.ai_queue.remove_pending(meeting_id) {
            persist_ai_queue(state);
        }
        mark_waiting(state, on_stage, meeting_id, None);
    }
    state.jobs_notify.notify_one();
    Ok(())
}

/// True for failures that retrying the identical job can never fix:
/// missing recording files never come back on their own, a 4xx request
/// rejection (413 payload too large, 401 bad key) reproduces byte-for-byte
/// on every retry, and a missing engine or a failed model download needs a
/// person (install / go online / pick another model) — those already ran
/// their own multi-source retries inside `models::ensure`, so the job fails
/// once with its actionable notice instead of re-downloading three times.
fn permanent_failure(error: &str) -> bool {
    error.contains(pipeline::ERR_NO_RECORDING_FILES)
        || error.contains(fly_asr::REJECTED_MARKER)
        || error.contains(models::ENGINE_MISSING_MARKER)
        || error.contains(models::DOWNLOAD_FAILED_MARKER)
}

/// True when the pipeline stopped because the user asked it to (note
/// deleted, import cancelled) — not a failure at all: no retry, no error
/// surfaced, the job row is simply removed.
fn cancelled_failure(error: &str) -> bool {
    error.contains(fly_asr::CANCELLED_MARKER)
}

/// True while a cancel for `meeting_id` is waiting to be honoured. A run
/// that fails for another reason after the user cancelled (a network error
/// mid-ASR, say) ends as cancelled, not as a retry.
fn cancel_pending(state: &AppState, meeting_id: &str) -> bool {
    state.cancel_requests.lock().unwrap().contains(meeting_id)
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

    // A pass of this very meeting (transcribed a moment ago, now queued
    // again) is working on the transcript this run is about to replace, so
    // it is stopped, not waited out: a model can take minutes, and nothing
    // it would write is worth having. It drops its request and winds down at
    // once; the wait is for that, so nothing writes under the new run.
    // Passes of other meetings never hold a transcription up.
    if state.ai_queue.cancel_running(&meeting_id) {
        state
            .ai_queue
            .wait_for_pass_end(&meeting_id, IDLE_POLL)
            .await;
        // a capture may have started meanwhile; the job stays queued
        if recording_active(state) {
            return Tick::RecordingActive;
        }
    }

    set_job_state(state, |s| s.mark_transcription_running(&meeting_id));
    // clear the "waiting" marker so run_with's per-meeting guard can pass
    state.pipeline_stage.lock().unwrap().remove(&meeting_id);

    match pipeline::run_with(state, on_stage, on_model, &meeting_id).await {
        Ok(_) => {
            // a cancel that landed after the last batch poll is moot now
            state.cancel_requests.lock().unwrap().remove(&meeting_id);
            set_job_state(state, |s| s.mark_transcription_done(&meeting_id));
            Tick::Completed(meeting_id)
        }
        Err(error) if cancelled_failure(&error) || cancel_pending(state, &meeting_id) => {
            tracing::info!(meeting_id, "transcription cancelled");
            state.pipeline_stage.lock().unwrap().remove(&meeting_id);
            state.cancel_requests.lock().unwrap().remove(&meeting_id);
            set_job_state(state, |s| s.delete_transcription_job(&meeting_id));
            Tick::Cancelled(meeting_id)
        }
        Err(error) => {
            tracing::error!(meeting_id, error = %error, "transcription pipeline failed");
            state.pipeline_stage.lock().unwrap().remove(&meeting_id);
            let attempts = job.attempts + 1;
            let permanent = permanent_failure(&error);
            if attempts < MAX_ATTEMPTS && !permanent {
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

/// Spawn the workers (called once at app setup): the transcription queue's,
/// and the one that runs the AI passes it queues.
///
/// The transcription worker recovers jobs a previous process left 'running',
/// then loops: run a job when allowed, otherwise wait for a nudge (enqueue,
/// recording stop) or the poll tick.
pub fn spawn<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    spawn_ai_worker(app.clone());
    tauri::async_runtime::spawn(async move {
        let on_stage = stage_emitter(&app);
        let on_model = {
            let app = app.clone();
            move |p: models::ModelProgress| {
                let _ = app.emit("model:progress", p);
            }
        };
        recover(&app.state::<AppState>(), &on_stage);
        recover_ai_passes(&app.state::<AppState>(), &on_stage);
        loop {
            let state = app.state::<AppState>();
            let outcome = tick(&state, &on_stage, &on_model).await;
            handle_tick(&state, &on_stage, outcome).await;
        }
    });
}

/// What the transcription worker does with one tick's outcome. Waits at
/// most the retry back-off or one idle poll: a finished transcription only
/// QUEUES its AI pass, so the next job starts at once.
async fn handle_tick(state: &AppState, on_stage: StageSink<'_>, tick: Tick) {
    match tick {
        Tick::Completed(meeting_id) => {
            queue_ai_pass(state, on_stage, &meeting_id, AiJobKind::AfterTranscription)
        }
        Tick::Retrying {
            meeting_id,
            attempts,
            error,
        } => {
            tracing::warn!(meeting_id, attempts, error = %error, "transcription retry queued");
            mark_waiting(
                state,
                on_stage,
                &meeting_id,
                Some(format!("retrying (attempt {attempts} failed)")),
            );
            tokio::time::sleep(RETRY_DELAY).await;
        }
        Tick::Cancelled(meeting_id) => {
            finish_cancelled(state, on_stage, &meeting_id);
        }
        Tick::GaveUp { meeting_id, error } => emit_final(on_stage, &meeting_id, Some(error)),
        Tick::Idle | Tick::RecordingActive => {
            let _ = tokio::time::timeout(IDLE_POLL, state.jobs_notify.notified()).await;
        }
    }
}

/// Queue the AI pass for a meeting. The note is told at once ("polishing"),
/// so it shows the fresh raw transcript instead of a stale transcription
/// stage while an earlier pass finishes. That is an event only: the
/// `pipeline_stage` marker, which makes a meeting count as busy (background
/// work yields to it, re-analyze and "delete everything" refuse it), is set
/// when the pass really starts (`polish_after_transcribe`), not while it
/// waits its turn, or for as long as a recording blocks it.
/// Does nothing when the meeting is queued or running again (it was queued
/// afresh in the meantime: that run brings its own pass).
fn queue_ai_pass(state: &AppState, on_stage: StageSink<'_>, meeting_id: &str, kind: AiJobKind) {
    queue_ai_job(state, on_stage, AiJob::new(meeting_id, kind));
}

/// [`queue_ai_pass`] for an entry that already has a history (one recovered
/// at startup keeps its attempts).
fn queue_ai_job(state: &AppState, on_stage: StageSink<'_>, job: AiJob) {
    // "Delete all my data" is under way: nothing new runs
    if state.ai_queue.is_halted() {
        return;
    }
    if state
        .pipeline_stage
        .lock()
        .unwrap()
        .contains_key(&job.meeting_id)
    {
        return;
    }
    let kind = job.kind;
    let meeting_id = job.meeting_id.clone();
    state.ai_queue.push(job);
    persist_ai_queue(state);
    if kind != AiJobKind::ExtractOnly {
        on_stage(PipelineProgress {
            meeting_id,
            stage: "polishing".into(),
            detail: None,
            done: false,
            error: None,
        });
    }
    state.ai_queue.wake.notify_one();
}

/// Re-extract a meeting's items in the AI worker's turn (after re-analyze or
/// undo changed who said what): not under a pass of its own, not beside it.
pub fn queue_extraction(state: &AppState, meeting_id: &str) {
    queue_ai_pass(state, &|_| {}, meeting_id, AiJobKind::ExtractOnly);
}

/// Backend-only settings row holding the AI passes not yet finished.
pub(crate) const AI_QUEUE_KEY: &str = "ai.pending_passes";

/// Starts a pass may have had before `recover_ai_passes` stops requeuing it.
/// The count is saved when a pass starts, so a pass that takes the app down
/// each time (a crash in a decoder, a model server that kills the machine)
/// runs this many times across launches, not forever.
const MAX_PASS_ATTEMPTS: u32 = 2;

/// Write the queue (running and waiting passes) to its settings row, so a
/// quit or restart loses none: the app runs them again on the next launch.
/// The snapshot is taken under the storage lock the write holds, so two
/// callers can't land their rows out of order (an older queue over a newer).
fn persist_ai_queue(state: &AppState) {
    let storage = state.storage.lock().unwrap();
    let json = serde_json::to_string(&state.ai_queue.snapshot()).unwrap_or_else(|_| "[]".into());
    if let Err(e) = storage.set_setting(AI_QUEUE_KEY, &json) {
        tracing::warn!(error = %e, "saving the AI pass queue failed");
    }
}

/// Startup: queue again the passes a previous run left unfinished (a pass
/// that was running starts over). A meeting that is gone or has no
/// transcript is dropped, and so is a pass that has already started
/// [`MAX_PASS_ATTEMPTS`] times. Only what was already queued comes back:
/// meetings that never had a pass (history from before the pass existed,
/// cleanup that was refused) stay Settings → "Extract from N meetings" and the
/// consent notice, which are the user's to run.
pub fn recover_ai_passes(state: &AppState, on_stage: StageSink<'_>) {
    let jobs: Vec<AiJob> = {
        let storage = state.storage.lock().unwrap();
        let saved = match storage.get_setting(AI_QUEUE_KEY) {
            Ok(Some(json)) => serde_json::from_str::<Vec<AiJob>>(&json).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "the saved AI pass queue is unreadable; starting with none");
                Vec::new()
            }),
            Ok(None) => Vec::new(),
            Err(e) => {
                tracing::warn!(error = %e, "reading the saved AI pass queue failed; starting with none");
                Vec::new()
            }
        };
        saved
            .into_iter()
            .filter(|job| {
                if job.attempts >= MAX_PASS_ATTEMPTS {
                    tracing::warn!(
                        meeting_id = %job.meeting_id,
                        attempts = job.attempts,
                        "giving up on an AI pass that was interrupted every time it started \
                         (Settings can extract this meeting's items on demand)"
                    );
                    return false;
                }
                storage.get_meeting(&job.meeting_id).is_ok()
                    && storage
                        .get_transcript(&job.meeting_id)
                        .ok()
                        .flatten()
                        .is_some()
            })
            .collect()
    };
    if !jobs.is_empty() {
        tracing::info!(
            passes = jobs.len(),
            "requeued AI passes interrupted by shutdown"
        );
    }
    for job in jobs {
        queue_ai_job(state, on_stage, job);
    }
    persist_ai_queue(state);
}

fn spawn_ai_worker<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let on_stage = stage_emitter(&app);
        let on_skips = {
            let app = app.clone();
            move |skips: AiSkips| crate::llm_commands::emit_skips(&app, skips)
        };
        let on_transcript_final = {
            let app = app.clone();
            // with the audio-only policy, an import's video copy
            move || crate::video_store::after_transcription(&app)
        };
        loop {
            let state = app.state::<AppState>();
            if !run_next_ai_job(&state, &on_stage, &on_skips, &on_transcript_final).await {
                let _ = tokio::time::timeout(IDLE_POLL, state.ai_queue.wake.notified()).await;
            }
        }
    });
}

/// Run the oldest queued AI pass, if there is one, and say it's over. Returns
/// whether one ran. `on_skips` gets the meetings still waiting on cloud
/// consent or cut short; `on_transcript_final` is the one app-handle effect of
/// a pass that follows a transcription.
///
/// Recording wins here too: passes can now queue up behind a slow one, and a
/// model must not start eating CPU and GPU while a capture is running. They
/// wait for it to end, like the transcription jobs do. (A pass already
/// running carries on, as it always did.)
///
/// A pass can be cancelled while it runs (`AiQueue::cancel_running`: its
/// meeting is transcribed again or deleted, or all data is being erased). The
/// pass is then dropped where it stands, which closes its model request; it
/// ends without a "done", the nudges or its meeting's final event, because
/// what it was working on is gone or about to be replaced.
async fn run_next_ai_job(
    state: &AppState,
    on_stage: StageSink<'_>,
    on_skips: &(dyn Fn(AiSkips) + Sync),
    on_transcript_final: &(dyn Fn() + Sync),
) -> bool {
    if recording_active(state) {
        return false;
    }
    let Some((job, running)) = state.ai_queue.next() else {
        return false;
    };
    // The attempt is on record before the pass starts: an app that dies inside
    // it leaves the count behind (see `recover_ai_passes`).
    persist_ai_queue(state);
    // cleanup first, except after a speaker change: the text didn't change
    let cleanup = job.kind != AiJobKind::ExtractOnly;
    let finished = tokio::select! {
        _ = running.cancelled() => false,
        _ = ai_pass(state, on_stage, &job.meeting_id, on_skips, cleanup) => true,
    };
    // A note deleted while its pass worked leaves the meeting for the pass
    // to purge (deleting skips a meeting that is busy), whether the pass
    // was cancelled in time or ran to its end.
    if finished && purge_if_orphaned(state, &job.meeting_id) {
        tracing::info!(meeting_id = %job.meeting_id, "AI pass finished for a deleted note; meeting purged");
        crate::llm_commands::clear_skips(state, &job.meeting_id);
        on_skips(crate::llm_commands::current_skips(state));
    } else if finished {
        if job.kind == AiJobKind::AfterTranscription {
            // Fresh transcript → new chunks for the semantic index. After the
            // pass: nudged before it, the worker would start a batch only to
            // abandon it when the pass begins, and polish would re-chunk the
            // meeting anyway.
            state.embed_notify.notify_one();
            // Transcript final → its recording can be compressed.
            state.audio_store.nudge();
            on_transcript_final();
        }
        // whoever asked for an extraction alone has already said "done"
        if cleanup {
            emit_final(on_stage, &job.meeting_id, None);
        }
    } else {
        tracing::info!(meeting_id = %job.meeting_id, "AI pass cancelled");
        abandon_pass(state, &job.meeting_id);
        on_skips(crate::llm_commands::current_skips(state));
    }
    drop(running);
    persist_ai_queue(state);
    true
}

/// Tidy what a cancelled pass left standing: its busy marker (only ours: a
/// transcription run that has started since owns the entry), what it had
/// recorded about the meeting, and, when the cancel came from deleting the
/// note, the meeting itself, which deleting could not purge while the pass was
/// working.
fn abandon_pass(state: &AppState, meeting_id: &str) {
    {
        let mut stages = state.pipeline_stage.lock().unwrap();
        if stages.get(meeting_id).map(String::as_str) == Some("polishing") {
            stages.remove(meeting_id);
        }
    }
    crate::llm_commands::clear_skips(state, meeting_id);
    purge_if_orphaned(state, meeting_id);
}

/// Purge the meeting if its note is gone (a note deleted while the meeting was
/// busy leaves it behind for whoever was busy with it): row, transcript, job,
/// staged-import residue and the recordings folder. Whether it did.
fn purge_if_orphaned(state: &AppState, meeting_id: &str) -> bool {
    let orphaned = {
        let storage = state.storage.lock().unwrap();
        match storage.get_meeting(meeting_id) {
            Ok(m) => storage.get_note(&m.note_id).is_err(),
            Err(_) => false,
        }
    };
    if orphaned {
        crate::import_commands::purge_staged_import(state, meeting_id);
        let storage = state.storage.lock().unwrap();
        if let Err(e) = storage.purge_meeting(meeting_id) {
            tracing::warn!(meeting_id, error = %e, "purging an orphaned meeting failed");
        }
    }
    orphaned
}

/// Stop the AI worker for "Delete all my data": nothing new is accepted or
/// started, the waiting passes are taken off the queue, and the running pass is
/// cancelled and given `timeout` to wind down. False when it didn't end in
/// time. The passes are held, and the saved queue keeps them, until the wipe
/// is known to have worked ([`forget_halted_ai_passes`]) or not
/// ([`resume_ai_passes`]).
pub fn halt_ai_passes(state: &AppState, timeout: Duration) -> bool {
    state.ai_queue.halt();
    let deadline = std::time::Instant::now() + timeout;
    while state.ai_queue.is_busy() {
        if std::time::Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    true
}

/// Undo [`halt_ai_passes`]: the wipe was refused or failed, so the passes it
/// held go back on the queue (and stay saved) and the worker carries on.
/// Also how the worker starts again after a wipe that worked, once
/// [`forget_halted_ai_passes`] has let the held passes go.
pub fn resume_ai_passes(state: &AppState) {
    state.ai_queue.resume();
    persist_ai_queue(state);
}

/// The wipe worked: the passes [`halt_ai_passes`] held belonged to data that
/// is gone. (The erase emptied the saved queue with everything else.)
pub fn forget_halted_ai_passes(state: &AppState) {
    state.ai_queue.discard_held();
}

/// The app is quitting: save the queue once more, giving back the start of
/// the pass this quit interrupts (a quit isn't a crash; see
/// `recover_ai_passes`). Tries for a moment to get the storage lock and gives
/// up quietly, since quitting must not hang on it; the row then keeps the
/// count it has, which only errs towards giving up on a pass.
pub fn save_ai_queue_on_quit(state: &AppState) {
    for _ in 0..50 {
        if let Ok(storage) = state.storage.try_lock() {
            let json = serde_json::to_string(&state.ai_queue.snapshot_for_quit())
                .unwrap_or_else(|_| "[]".into());
            if let Err(e) = storage.set_setting(AI_QUEUE_KEY, &json) {
                tracing::warn!(error = %e, "saving the AI pass queue at quit failed");
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    tracing::warn!("the database stayed busy at quit; the AI pass queue was not updated");
}

/// The best-effort AI pass after a transcription: cleanup, then structured
/// item extraction. Enrichment only, it never blocks or fails the job. A
/// meeting either step skipped for missing cloud consent is remembered
/// (`AppState::consent_skipped`), one the model stopped short of too
/// (`AppState::ai_incomplete`); both lists go out (`llm:consent-skips`,
/// `llm:ai-cut-off`) so the app's notice can offer to run them.
async fn ai_pass(
    state: &AppState,
    on_stage: StageSink<'_>,
    meeting_id: &str,
    on_skips: &(dyn Fn(AiSkips) + Sync),
    cleanup: bool,
) {
    // This run decides it afresh; a refusal or a cut-off below puts it back.
    crate::llm_commands::clear_skips(state, meeting_id);
    if cleanup {
        polish_after_transcribe(state, on_stage, meeting_id).await;
    }
    crate::extraction::extract_after_transcribe(state, meeting_id).await;
    on_skips(crate::llm_commands::current_skips(state));
}

/// Run the AI pass again for every meeting it skipped for missing consent
/// (the user has just allowed the provider). Queued behind any pass already
/// waiting, one meeting at a time, with the same "polishing" marker and final
/// event as after a transcription, so an open note shows the cleaned
/// transcript when it lands. Returns how many meetings it took on.
pub fn retry_consent_skipped<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> usize {
    queue_consent_rerun(&app.state::<AppState>(), &stage_emitter(&app))
}

fn queue_consent_rerun(state: &AppState, on_stage: StageSink<'_>) -> usize {
    let ids = take_consent_rerun(state);
    for meeting_id in &ids {
        queue_ai_pass(state, on_stage, meeting_id, AiJobKind::ConsentRerun);
    }
    ids.len()
}

/// Empty the consent-skipped and cut-off lists and return the meetings the
/// re-run should take (all of them run together: "Run them now"). The rest are
/// dropped, not kept for later. A meeting being transcribed again gets its own
/// AI pass from that run. A meeting whose last transcription failed keeps its
/// failure notice: the re-run would end in a "done" event, which clears that
/// notice and the note's inline error.
fn take_consent_rerun(state: &AppState) -> Vec<String> {
    let mut taken = std::mem::take(&mut *state.consent_skipped.lock().unwrap());
    taken.append(&mut std::mem::take(
        &mut *state.ai_incomplete.lock().unwrap(),
    ));
    taken.append(&mut std::mem::take(
        &mut *state.cleanup_incomplete.lock().unwrap(),
    ));
    taken
        .into_iter()
        .filter(|id| consent_rerun_allowed(state, id))
        .collect()
}

/// See [`take_consent_rerun`]: not while it's being transcribed, and not
/// over a held transcription failure.
fn consent_rerun_allowed(state: &AppState, meeting_id: &str) -> bool {
    !state
        .pipeline_stage
        .lock()
        .unwrap()
        .contains_key(meeting_id)
        && last_failure(state, meeting_id).is_none()
}

/// Why this meeting's transcription last gave up: the error of a job parked
/// as failed. The terminal error event is fire-and-forget, so a note opened
/// later (another note was open, or the app restarted) reads it from here.
/// None while a job is queued or running, after success, or with no job.
pub fn last_failure(state: &AppState, meeting_id: &str) -> Option<String> {
    let job = state
        .storage
        .lock()
        .unwrap()
        .transcription_job(meeting_id)
        .ok()
        .flatten()?;
    (job.status == fly_storage::JOB_FAILED).then(|| {
        job.last_error
            .unwrap_or_else(|| "Transcription failed.".into())
    })
}

/// Stop transcription work for a meeting. A pipeline that is actually
/// running gets a cooperative cancel request and unwinds at its next batch
/// boundary (cheap — completed batches are checkpointed); the worker then
/// finishes up via [`Tick::Cancelled`]. A merely queued/waiting job is
/// removed on the spot and the terminal "cancelled" event emitted here.
/// Returns whether anything was in flight (so "cancelled" is out or coming).
///
/// An AI pass that hasn't started is dropped from its queue too, quietly
/// (the transcript is final: "cancelled" would tell the app it isn't), so
/// nothing runs on a meeting that is being deleted or cancelled. A pass
/// already running carries on, as it always did.
pub fn cancel(state: &AppState, on_stage: StageSink<'_>, meeting_id: &str) -> Result<bool, String> {
    if state.ai_queue.remove_pending(meeting_id) {
        persist_ai_queue(state);
    }
    let running = {
        let stages = state.pipeline_stage.lock().unwrap();
        matches!(stages.get(meeting_id).map(String::as_str), Some(s) if s != WAITING_STAGE)
    };
    if running {
        state
            .cancel_requests
            .lock()
            .unwrap()
            .insert(meeting_id.to_string());
        return Ok(true);
    }
    let had_job = {
        let storage = state.storage.lock().unwrap();
        let job = storage
            .transcription_job(meeting_id)
            .map_err(|e| e.to_string())?;
        let pending = job.is_some_and(|j| j.status == "queued" || j.status == "running");
        if pending {
            storage
                .delete_transcription_job(meeting_id)
                .map_err(|e| e.to_string())?;
        }
        pending
    };
    if had_job {
        // guard the pickup race: if the worker grabbed the job between the
        // check above and the delete, the request makes the run unwind too
        state
            .cancel_requests
            .lock()
            .unwrap()
            .insert(meeting_id.to_string());
        state.pipeline_stage.lock().unwrap().remove(meeting_id);
        emit_cancelled(on_stage, meeting_id);
    }
    Ok(had_job)
}

/// After a cancelled run winds down: if the owning note is gone (the cancel
/// came from a note deletion) purge the orphaned meeting — row, transcript,
/// job, staged-import residue, and the recordings folder — then tell the UI.
pub fn finish_cancelled(state: &AppState, on_stage: StageSink<'_>, meeting_id: &str) {
    purge_if_orphaned(state, meeting_id);
    emit_cancelled(on_stage, meeting_id);
}

/// Terminal event for a cancelled run: done, but neither success nor error —
/// the frontend drops back to the pre-transcribe state.
pub(crate) fn emit_cancelled(on_stage: StageSink<'_>, meeting_id: &str) {
    on_stage(PipelineProgress {
        meeting_id: meeting_id.to_string(),
        stage: "cancelled".into(),
        detail: None,
        done: true,
        error: None,
    });
}

/// How long the AI-cleanup step may run before the pass moves on to item
/// extraction (which has its own limits, per batch: `extraction::BatchLimits`).
/// LLM providers have no client-side timeout; without a cap here a hung
/// provider would hold up every AI pass behind it (never a transcription: the
/// passes have their own worker).
const POLISH_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Best-effort AI cleanup chained after every successful transcription, so a
/// (re)transcribe delivers the full pipeline: ASR → diarization → polish. It
/// must never fail the job — no provider running / no key configured just
/// means the raw transcript stands (exactly the pre-polish behavior);
/// re-running the transcription re-runs the cleanup too.
async fn polish_after_transcribe(state: &AppState, on_stage: StageSink<'_>, meeting_id: &str) {
    polish_within(state, on_stage, meeting_id, POLISH_TIMEOUT).await;
}

/// [`polish_after_transcribe`] with the limit as a parameter.
async fn polish_within(
    state: &AppState,
    on_stage: StageSink<'_>,
    meeting_id: &str,
    limit: Duration,
) {
    emit_stage_marker(state, on_stage, meeting_id, "polishing");
    let outcome =
        tokio::time::timeout(limit, crate::llm_commands::run_polish(state, meeting_id)).await;
    match outcome {
        Ok(Ok(r)) => tracing::info!(
            meeting_id,
            cleaned = r.segments_cleaned,
            kept_raw = r.segments_kept_raw,
            "transcript cleanup done"
        ),
        Ok(Err(e)) => {
            crate::llm_commands::record_consent_skip(state, meeting_id, &e);
            tracing::info!(meeting_id, error = %e, "transcript cleanup skipped (raw transcript stands)")
        }
        Err(_) => {
            // its own list: the notice words a cleanup that didn't finish
            // differently from action items that couldn't be read
            crate::llm_commands::record_cleanup_incomplete(state, meeting_id);
            tracing::warn!(
                meeting_id,
                "transcript cleanup timed out (raw transcript stands)"
            )
        }
    }
    // Clear only OUR marker: if the user re-enqueued this meeting while the
    // polish ran, the map now says "waiting" and that must survive.
    let mut stages = state.pipeline_stage.lock().unwrap();
    if stages.get(meeting_id).map(String::as_str) == Some("polishing") {
        stages.remove(meeting_id);
    }
}

/// Surface an intermediate stage owned by the scheduler (not the pipeline):
/// tracked in `pipeline_stage` for late joiners and emitted as progress.
fn emit_stage_marker(state: &AppState, on_stage: StageSink<'_>, meeting_id: &str, stage: &str) {
    state
        .pipeline_stage
        .lock()
        .unwrap()
        .insert(meeting_id.to_string(), stage.to_string());
    on_stage(PipelineProgress {
        meeting_id: meeting_id.to_string(),
        stage: stage.into(),
        detail: None,
        done: false,
        error: None,
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

pub(crate) fn recording_active(state: &AppState) -> bool {
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
    f: impl FnOnce(&fly_storage::Storage) -> fly_storage::Result<()>,
) {
    if let Err(e) = f(&state.storage.lock().unwrap()) {
        tracing::error!(error = %e, "updating transcription job state failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Review and allow" re-runs only meetings it can finish cleanly. One
    /// whose last transcription failed keeps its failure notice (a re-run's
    /// "done" event would clear it), and one being transcribed now gets the
    /// pass from that run. Both leave the list rather than wait in it.
    #[test]
    fn consent_rerun_skips_held_failures_and_running_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::init_with(
            dir.path().to_path_buf(),
            std::sync::Arc::new(fly_secrets::MemorySecretStore::default()),
        )
        .unwrap();
        {
            let storage = state.storage.lock().unwrap();
            storage.enqueue_transcription("m-failed").unwrap();
            storage
                .mark_transcription_failed("m-failed", 1, "whisper-cli crashed")
                .unwrap();
            // a finished job is fine: done, not failed
            storage.enqueue_transcription("m-done").unwrap();
            storage.mark_transcription_done("m-done").unwrap();
        }
        state
            .pipeline_stage
            .lock()
            .unwrap()
            .insert("m-running".into(), "transcribing".into());
        state
            .consent_skipped
            .lock()
            .unwrap()
            .extend(["m-ok", "m-done", "m-failed", "m-running"].map(String::from));

        assert!(last_failure(&state, "m-failed").is_some());
        assert_eq!(take_consent_rerun(&state), vec!["m-done", "m-ok"]);
        assert!(state.consent_skipped.lock().unwrap().is_empty());
        assert!(!consent_rerun_allowed(&state, "m-failed"));
        assert!(!consent_rerun_allowed(&state, "m-running"));
        assert!(consent_rerun_allowed(&state, "m-ok"));
    }

    #[test]
    fn rejected_cloud_errors_are_permanent() {
        // exactly what the pipeline hands the scheduler: AsrError stringified
        let e = fly_asr::AsrError::Rejected("groq returned 413 Payload Too Large: {}".into());
        assert!(permanent_failure(&e.to_string()));
        let e = fly_asr::AsrError::Rejected("groq returned 401 Unauthorized: {}".into());
        assert!(permanent_failure(&e.to_string()));
    }

    #[test]
    fn transient_errors_still_retry() {
        let e = fly_asr::AsrError::Network("connection reset by peer".into());
        assert!(!permanent_failure(&e.to_string()));
        let e = fly_asr::AsrError::Engine("groq returned 500 Internal Server Error".into());
        assert!(!permanent_failure(&e.to_string()));
        assert!(permanent_failure(pipeline::ERR_NO_RECORDING_FILES));
    }

    /// Human-fixable failures fail ONCE with their actionable notice: the
    /// exact messages `models::ensure_tool` / `models::ensure` produce
    /// (via the shared marker constants) must be recognized as permanent.
    #[test]
    fn engine_missing_and_download_failures_are_permanent() {
        // models.rs `ensure_tool` shape (no managed artifact, nothing on PATH)
        assert!(permanent_failure(&format!(
            "whisper-cli {} — install whisper.cpp (macOS: brew install whisper-cpp)",
            models::ENGINE_MISSING_MARKER
        )));
        // models.rs `ensure_with` shape (offline / CDN outage, all sources tried)
        assert!(permanent_failure(&format!(
            "{} Whisper small (Q5, ~190 MB) after trying 2 source(s) (last error: download \
             failed: connection reset)",
            models::DOWNLOAD_FAILED_MARKER
        )));
    }

    /// A cancel is neither retried (the user asked for the stop) nor a
    /// permanent failure (nothing is wrong) — it gets its own branch.
    #[test]
    fn cancelled_errors_are_neither_retried_nor_failures() {
        let e = fly_asr::AsrError::Cancelled.to_string();
        assert!(cancelled_failure(&e));
        assert!(!permanent_failure(&e));
        assert!(!cancelled_failure("network reset by peer"));
        assert!(!cancelled_failure(pipeline::ERR_NO_RECORDING_FILES));
    }

    fn test_state(dir: &std::path::Path) -> AppState {
        AppState::init_with(
            dir.to_path_buf(),
            std::sync::Arc::new(fly_secrets::MemorySecretStore::default()),
        )
        .unwrap()
    }

    /// The user cancels while a run is going, and the run then fails for
    /// another reason before it notices (here: the meeting has no recording,
    /// a retryable error). The cancel wins: the job is removed, not queued
    /// for another attempt that would carry on as if nothing was asked.
    #[test]
    fn a_cancel_survives_a_run_that_fails_before_noticing_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let meeting_id = {
            let storage = state.storage.lock().unwrap();
            let note = storage.create_note("Standup", None).unwrap();
            storage.create_meeting("Standup", &note.id, &[]).unwrap().id
        };
        let quiet = |_: pipeline::PipelineProgress| {};
        let model = |_: models::ModelProgress| {};
        enqueue(&state, &quiet, &meeting_id).unwrap();
        // the cancel lands while the worker is already on it
        state
            .cancel_requests
            .lock()
            .unwrap()
            .insert(meeting_id.clone());

        let rt = tokio::runtime::Runtime::new().unwrap();
        let tick = rt.block_on(tick(&state, &quiet, &model));
        assert!(
            matches!(&tick, Tick::Cancelled(id) if *id == meeting_id),
            "a pending cancel must not turn into a retry"
        );
        let job = state
            .storage
            .lock()
            .unwrap()
            .transcription_job(&meeting_id)
            .unwrap();
        assert!(job.is_none(), "{job:?}");
        assert!(!state.cancel_requests.lock().unwrap().contains(&meeting_id));
    }

    /// Without a cancel the same failure is retried, as before.
    #[test]
    fn the_same_failure_without_a_cancel_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let meeting_id = {
            let storage = state.storage.lock().unwrap();
            let note = storage.create_note("Standup", None).unwrap();
            storage.create_meeting("Standup", &note.id, &[]).unwrap().id
        };
        let quiet = |_: pipeline::PipelineProgress| {};
        let model = |_: models::ModelProgress| {};
        enqueue(&state, &quiet, &meeting_id).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tick = rt.block_on(tick(&state, &quiet, &model));
        assert!(matches!(tick, Tick::Retrying { .. }), "{tick:?}");
    }

    /// Queuing a meeting afresh drops a cancel left from an earlier run; an
    /// enqueue that finds the job still pending keeps it.
    #[test]
    fn only_a_new_job_drops_a_stale_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let quiet = |_: pipeline::PipelineProgress| {};
        state.cancel_requests.lock().unwrap().insert("m1".into());
        enqueue(&state, &quiet, "m1").unwrap();
        assert!(!state.cancel_requests.lock().unwrap().contains("m1"));
        state.cancel_requests.lock().unwrap().insert("m1".into());
        enqueue(&state, &quiet, "m1").unwrap(); // already queued
        assert!(state.cancel_requests.lock().unwrap().contains("m1"));
    }

    /// Progress events a test collects: `(meeting_id, stage)`.
    type Events = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

    fn collecting() -> (Events, impl Fn(pipeline::PipelineProgress) + Send + Sync) {
        let events = Events::default();
        let sink = {
            let events = events.clone();
            move |p: pipeline::PipelineProgress| {
                events.lock().unwrap().push((p.meeting_id, p.stage));
            }
        };
        (events, sink)
    }

    /// A model server that takes every request and never answers, and says
    /// when a request arrived and when its client hung up.
    struct HungModel {
        accepted: std::sync::mpsc::Receiver<()>,
        hung_up: std::sync::mpsc::Receiver<()>,
    }

    /// An app whose AI provider is a [`HungModel`]: OpenAI-compatible, on this
    /// machine (so a plain-HTTP address is allowed), with a key and consent,
    /// so a pass runs for as long as it is allowed to.
    fn state_with_hung_model(dir: &std::path::Path) -> (AppState, HungModel) {
        use fly_secrets::SecretStore;
        let secrets = std::sync::Arc::new(fly_secrets::MemorySecretStore::default());
        secrets
            .set(fly_secrets::keys::OPENAI_API_KEY, "sk-test")
            .unwrap();
        let state = AppState::init_with(dir.to_path_buf(), secrets).unwrap();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        {
            let storage = state.storage.lock().unwrap();
            storage.set_setting("llm.provider", "openai").unwrap();
            storage
                .set_setting(
                    "llm.openai.base_url",
                    &format!("http://127.0.0.1:{port}/v1"),
                )
                .unwrap();
            storage
                .set_setting("consent.cloud_llm.openai", "true")
                .unwrap();
        }
        let (accepted_tx, accepted) = std::sync::mpsc::channel();
        let (hung_up_tx, hung_up) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for conn in server.incoming() {
                let Ok(mut conn) = conn else { break };
                let (accepted_tx, hung_up_tx) = (accepted_tx.clone(), hung_up_tx.clone());
                std::thread::spawn(move || {
                    let _ = accepted_tx.send(());
                    let mut buf = [0u8; 4096];
                    while matches!(std::io::Read::read(&mut conn, &mut buf), Ok(n) if n > 0) {}
                    let _ = hung_up_tx.send(());
                });
            }
        });
        (state, HungModel { accepted, hung_up })
    }

    fn stage_of(state: &AppState, meeting_id: &str) -> Option<String> {
        state
            .pipeline_stage
            .lock()
            .unwrap()
            .get(meeting_id)
            .cloned()
    }

    fn real_meeting(state: &AppState) -> String {
        let storage = state.storage.lock().unwrap();
        let note = storage.create_note("Standup", None).unwrap();
        storage.create_meeting("Standup", &note.id, &[]).unwrap().id
    }

    /// A provider that refuses at once (a cloud provider with no consent: no
    /// network and no Ollama to start), so the pass ends fast.
    fn refusing_provider(state: &AppState) {
        state
            .storage
            .lock()
            .unwrap()
            .set_setting("llm.provider", "anthropic")
            .unwrap();
    }

    /// The incident: a model that ran for ten minutes held the next
    /// transcription back, because the AI pass ran inside the worker loop. A
    /// finished transcription now only queues its pass and returns, even
    /// while an earlier pass is still running.
    #[test]
    fn a_finished_transcription_queues_its_pass_and_the_worker_moves_on() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let (events, sink) = collecting();
        // an earlier meeting's pass is mid-flight (a slow model)
        state
            .ai_queue
            .push(AiJob::new("earlier", AiJobKind::AfterTranscription));
        let _earlier_pass = state.ai_queue.next().unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(2),
                handle_tick(&state, &sink, Tick::Completed("m1".into())),
            )
            .await
            .expect("the worker must not wait for the AI pass");
        });

        assert!(state.ai_queue.is_pending("m1"));
        // the note is told ("polishing"), so it shows the fresh raw
        // transcript instead of a stale transcription stage; no "done" yet:
        // that comes when the pass has run
        assert_eq!(
            *events.lock().unwrap(),
            vec![("m1".to_string(), "polishing".to_string())]
        );
    }

    /// A pass that is only waiting its turn makes its meeting no busier than
    /// any finished one: nothing for background work to yield to, nothing for
    /// re-analyze or "delete everything" to refuse, and nothing left standing
    /// for the length of a recording. (The marker is set when the pass starts.)
    #[test]
    fn a_queued_pass_does_not_mark_its_meeting_busy() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let (_events, sink) = collecting();
        queue_ai_pass(&state, &sink, "m1", AiJobKind::AfterTranscription);
        queue_ai_pass(&state, &sink, "m2", AiJobKind::ConsentRerun);
        queue_extraction(&state, "m3");
        assert!(state.ai_queue.is_pending("m1"));
        assert!(state.ai_queue.is_pending("m2"));
        assert!(state.ai_queue.is_pending("m3"));
        assert!(state.pipeline_stage.lock().unwrap().is_empty());
        assert!(!state.background_should_yield());
    }

    /// A transcription that completes while its meeting is queued again (the
    /// user hit re-transcribe) leaves the new run to queue its own pass.
    #[test]
    fn no_pass_is_queued_for_a_meeting_waiting_for_another_run() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let (events, sink) = collecting();
        state
            .pipeline_stage
            .lock()
            .unwrap()
            .insert("m1".into(), WAITING_STAGE.into());
        queue_ai_pass(&state, &sink, "m1", AiJobKind::AfterTranscription);
        assert!(!state.ai_queue.is_pending("m1"));
        assert_eq!(stage_of(&state, "m1").as_deref(), Some(WAITING_STAGE));
        assert!(events.lock().unwrap().is_empty());
    }

    /// The worker that drains the queue ends a pass the way the transcription
    /// worker used to: the semantic index and compression are nudged, the
    /// import's video copy is told, and the meeting gets its "done" event.
    #[test]
    fn a_queued_pass_ends_with_the_final_event_and_the_nudges() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        refusing_provider(&state);
        let (events, sink) = collecting();
        let m1 = real_meeting(&state);
        queue_ai_pass(&state, &sink, &m1, AiJobKind::AfterTranscription);
        let skips = std::sync::Mutex::new(Vec::<Vec<String>>::new());
        let finals = std::sync::atomic::AtomicUsize::new(0);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let ran = rt.block_on(run_next_ai_job(
            &state,
            &sink,
            &|now| skips.lock().unwrap().push(now.consent),
            &|| {
                finals.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        ));
        assert!(ran);
        assert_eq!(finals.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            events.lock().unwrap().last(),
            Some(&(m1.clone(), "done".to_string()))
        );
        assert_eq!(stage_of(&state, &m1), None, "the polishing marker is gone");
        assert!(!state.ai_queue.is_running(&m1));
        // the refusal was a missing-consent skip, reported at the end
        assert_eq!(skips.lock().unwrap().last().unwrap(), &vec![m1.clone()]);
        // nothing left to run
        assert!(!rt.block_on(run_next_ai_job(&state, &sink, &|_| {}, &|| {})));
    }

    struct FakeSession;

    impl fly_audio::CaptureSession for FakeSession {
        fn pause(&mut self) -> fly_audio::Result<()> {
            Ok(())
        }
        fn resume(&mut self) -> fly_audio::Result<()> {
            Ok(())
        }
        fn stop(self: Box<Self>) -> fly_audio::Result<fly_audio::CaptureOutput> {
            Ok(fly_audio::CaptureOutput {
                mic_path: None,
                system_path: None,
                mixed_path: None,
                playback_path: None,
                duration_ms: 0,
                health: Default::default(),
            })
        }
        fn state(&self) -> fly_audio::CaptureState {
            fly_audio::CaptureState::Recording
        }
        fn elapsed_ms(&self) -> u64 {
            0
        }
    }

    /// Recording wins over the model as well: passes that queued up wait for
    /// the capture to end instead of competing with it for the machine.
    #[test]
    fn a_queued_pass_waits_for_a_recording_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        refusing_provider(&state);
        let (_events, sink) = collecting();
        let m1 = real_meeting(&state);
        queue_ai_pass(&state, &sink, &m1, AiJobKind::AfterTranscription);
        *state.recording.lock().unwrap() = Some(crate::recording::ActiveRecording {
            session: Box::new(FakeSession),
            meeting_id: "live".into(),
            note_id: "n".into(),
            live_stop: None,
        });

        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(!rt.block_on(run_next_ai_job(&state, &sink, &|_| {}, &|| {})));
        assert!(state.ai_queue.is_pending(&m1), "still waiting its turn");

        *state.recording.lock().unwrap() = None;
        assert!(rt.block_on(run_next_ai_job(&state, &sink, &|_| {}, &|| {})));
        assert!(!state.ai_queue.is_pending(&m1));
    }

    /// "Review and allow" goes through the same queue and worker, so the
    /// re-runs and the passes after transcriptions never overlap; a re-run
    /// doesn't re-announce a transcript that was already final.
    #[test]
    fn consent_reruns_share_the_queue_and_skip_the_transcript_final_effects() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        refusing_provider(&state);
        let (events, sink) = collecting();
        let m2 = real_meeting(&state);
        state.consent_skipped.lock().unwrap().insert(m2.clone());
        assert_eq!(queue_consent_rerun(&state, &sink), 1);
        assert!(state.consent_skipped.lock().unwrap().is_empty());
        assert!(state.ai_queue.is_pending(&m2));

        let finals = std::sync::atomic::AtomicUsize::new(0);
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(rt.block_on(run_next_ai_job(&state, &sink, &|_| {}, &|| {
            finals.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })));
        assert_eq!(finals.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            events.lock().unwrap().last(),
            Some(&(m2.clone(), "done".to_string()))
        );
    }

    /// Transcribing a meeting again drops its pass if it hasn't started (the
    /// new run replaces the transcript and brings its own); deleting or
    /// cancelling drops it too, quietly: "cancelled" would tell the app a
    /// meeting whose transcript is final isn't.
    #[test]
    fn re_transcribing_or_cancelling_drops_a_pass_that_has_not_started() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let (events, sink) = collecting();

        queue_ai_pass(&state, &sink, "m1", AiJobKind::AfterTranscription);
        enqueue(&state, &sink, "m1").unwrap();
        assert!(!state.ai_queue.is_pending("m1"));
        assert_eq!(stage_of(&state, "m1").as_deref(), Some(WAITING_STAGE));

        queue_ai_pass(&state, &sink, "m3", AiJobKind::AfterTranscription);
        assert!(
            !cancel(&state, &sink, "m3").unwrap(),
            "no transcription was in flight"
        );
        assert!(!state.ai_queue.is_pending("m3"));
        assert_eq!(stage_of(&state, "m3"), None);
        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .all(|(_, stage)| stage != "cancelled"),
            "{:?}",
            events.lock().unwrap()
        );
    }

    /// Re-analyze and undo re-extract through the same worker: no cleanup
    /// (the text didn't change), no event of its own (the caller already said
    /// "done"), and not under or beside a pass of the same meeting.
    #[test]
    fn a_speaker_change_re_extracts_in_the_queue_without_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        refusing_provider(&state);
        let (events, sink) = collecting();
        let m1 = real_meeting(&state);
        queue_extraction(&state, &m1);
        assert!(state.ai_queue.is_pending(&m1));
        assert!(events.lock().unwrap().is_empty());

        let skips = std::sync::Mutex::new(Vec::<Vec<String>>::new());
        let finals = std::sync::atomic::AtomicUsize::new(0);
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(rt.block_on(run_next_ai_job(
            &state,
            &sink,
            &|now| skips.lock().unwrap().push(now.consent),
            &|| {
                finals.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        )));
        assert!(
            events.lock().unwrap().is_empty(),
            "no polishing, no done: {:?}",
            events.lock().unwrap()
        );
        assert_eq!(finals.load(std::sync::atomic::Ordering::SeqCst), 0);
        // extraction ran (and was refused for consent), the notice hears of it
        assert_eq!(skips.lock().unwrap().last().unwrap(), &vec![m1.clone()]);

        // a meeting already queued for the full pass keeps it
        queue_ai_pass(&state, &sink, &m1, AiJobKind::AfterTranscription);
        queue_extraction(&state, &m1);
        assert_eq!(state.ai_queue.snapshot().len(), 1);
        assert_eq!(
            state.ai_queue.snapshot()[0].kind,
            AiJobKind::AfterTranscription
        );
    }

    /// A transcript for a meeting, so a recovered pass has something to run on.
    fn meeting_with_transcript(state: &AppState) -> String {
        let id = real_meeting(state);
        let transcript = fly_core::Transcript {
            meeting_id: id.clone(),
            language: Some("en".into()),
            engine: "test".into(),
            segments: vec![fly_core::TranscriptSegment {
                id: "s1".into(),
                speaker_key: "mic".into(),
                start_ms: 0,
                end_ms: 1000,
                text: "we ship on Friday".into(),
                words: vec![],
            }],
            speakers: vec![],
        };
        state
            .storage
            .lock()
            .unwrap()
            .save_transcript(&transcript)
            .unwrap();
        id
    }

    /// The queue is in memory, but what is in it is saved: a quit or restart
    /// no longer loses every queued pass (a 20-file import would have lost 20
    /// cleanups). The pass that was running starts over; a meeting that is
    /// gone, or has no transcript, is dropped; meetings that never had a pass
    /// are not invented (history stays Settings' to extract).
    #[test]
    fn queued_passes_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (quiet_events, quiet) = collecting();
        let (running_id, waiting_id, no_transcript, history_id) = {
            let state = test_state(dir.path());
            let running_id = meeting_with_transcript(&state);
            let waiting_id = meeting_with_transcript(&state);
            let no_transcript = real_meeting(&state);
            let history_id = meeting_with_transcript(&state); // never queued
            queue_ai_pass(&state, &quiet, &running_id, AiJobKind::AfterTranscription);
            queue_ai_pass(&state, &quiet, &waiting_id, AiJobKind::ConsentRerun);
            queue_ai_pass(
                &state,
                &quiet,
                &no_transcript,
                AiJobKind::AfterTranscription,
            );
            queue_ai_pass(&state, &quiet, "deleted-meeting", AiJobKind::ExtractOnly);
            // the first one is mid-pass when the app quits
            let _running = state.ai_queue.next().unwrap();
            (running_id, waiting_id, no_transcript, history_id)
        };
        quiet_events.lock().unwrap().clear();

        let state = test_state(dir.path());
        assert!(state.ai_queue.snapshot().is_empty(), "memory starts empty");
        recover_ai_passes(&state, &quiet);
        let back: Vec<_> = state
            .ai_queue
            .snapshot()
            .into_iter()
            .map(|j| (j.meeting_id, j.kind))
            .collect();
        assert_eq!(
            back,
            vec![
                (running_id.clone(), AiJobKind::AfterTranscription),
                (waiting_id.clone(), AiJobKind::ConsentRerun),
            ]
        );
        assert!(!back
            .iter()
            .any(|(id, _)| *id == no_transcript || *id == history_id));

        // and what was dropped is gone from the saved row too
        let state = {
            drop(state);
            test_state(dir.path())
        };
        recover_ai_passes(&state, &quiet);
        assert_eq!(state.ai_queue.snapshot().len(), 2);
    }

    /// A pass that ran to the end leaves nothing to recover.
    #[test]
    fn a_finished_pass_is_not_requeued_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let (_events, sink) = collecting();
        {
            let state = test_state(dir.path());
            refusing_provider(&state);
            let id = meeting_with_transcript(&state);
            queue_ai_pass(&state, &sink, &id, AiJobKind::AfterTranscription);
            let rt = tokio::runtime::Runtime::new().unwrap();
            assert!(rt.block_on(run_next_ai_job(&state, &sink, &|_| {}, &|| {})));
        }
        let state = test_state(dir.path());
        recover_ai_passes(&state, &sink);
        assert!(state.ai_queue.snapshot().is_empty());
    }

    /// A transcription of a meeting whose own pass is running does not wait
    /// for that pass: the transcript the pass works on is about to be
    /// replaced, so the pass is cancelled and the job starts as soon as it
    /// has wound down. Another meeting's pass is none of its business.
    #[test]
    fn a_re_transcribe_stops_its_meetings_pass_instead_of_waiting_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let (state, model) = state_with_hung_model(dir.path());
        let state = std::sync::Arc::new(state);
        let meeting_id = meeting_with_transcript(&state);
        let (events, sink) = collecting();
        queue_ai_pass(&state, &sink, &meeting_id, AiJobKind::AfterTranscription);

        let rt = tokio::runtime::Runtime::new().unwrap();
        // the pass reaches the model, which never answers
        let worker = rt.spawn({
            let state = state.clone();
            async move { run_next_ai_job(&state, &sink, &|_| {}, &|| {}).await }
        });
        model
            .accepted
            .recv_timeout(Duration::from_secs(20))
            .unwrap();
        assert!(state.ai_queue.is_running(&meeting_id));

        // the meeting is transcribed again; its job (which fails fast, for
        // want of a recording) must not wait for the model
        let quiet = |_: pipeline::PipelineProgress| {};
        let model_progress = |_: models::ModelProgress| {};
        enqueue(&state, &quiet, &meeting_id).unwrap();
        let t = rt.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(10),
                tick(&state, &quiet, &model_progress),
            )
            .await
        });
        let t = t.expect("the job must not wait for a model that never answers");
        assert!(matches!(t, Tick::Retrying { .. }), "{t:?}");

        // the pass ended without the effects of a finished one
        let ran = rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(5), worker).await })
            .expect("the pass was cancelled")
            .unwrap();
        assert!(ran);
        assert!(!state.ai_queue.is_running(&meeting_id));
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .all(|(_, stage)| stage != "done"));
        model
            .hung_up
            .recv_timeout(Duration::from_secs(5))
            .expect("its request to the model was dropped");
    }

    /// Cancelling a running pass drops its model request, takes back the busy
    /// marker the pass set, forgets what it recorded, and says nothing of
    /// "done". The attempt is on record from the moment the pass starts.
    #[test]
    fn cancelling_a_running_pass_drops_its_request_and_leaves_nothing_standing() {
        let dir = tempfile::tempdir().unwrap();
        let (state, model) = state_with_hung_model(dir.path());
        let state = std::sync::Arc::new(state);
        let id = meeting_with_transcript(&state);
        let (events, sink) = collecting();
        queue_ai_pass(&state, &sink, &id, AiJobKind::AfterTranscription);
        let skips = std::sync::Arc::new(std::sync::Mutex::new(Vec::<AiSkips>::new()));

        let rt = tokio::runtime::Runtime::new().unwrap();
        let worker = rt.spawn({
            let (state, skips) = (state.clone(), skips.clone());
            async move {
                run_next_ai_job(
                    &state,
                    &sink,
                    &move |now| skips.lock().unwrap().push(now),
                    &|| {},
                )
                .await
            }
        });
        model
            .accepted
            .recv_timeout(Duration::from_secs(20))
            .unwrap();
        assert_eq!(stage_of(&state, &id).as_deref(), Some("polishing"));
        // a skip recorded earlier would be stale after a cancel
        state.ai_queue.cancel_running("someone else's meeting");
        assert!(
            state.ai_queue.is_running(&id),
            "only its own meeting's cancel counts"
        );
        let saved = state
            .storage
            .lock()
            .unwrap()
            .get_setting(AI_QUEUE_KEY)
            .unwrap()
            .unwrap();
        assert!(
            saved.contains(&id) && saved.contains("\"attempts\":1"),
            "the attempt is saved before the pass runs: {saved}"
        );

        crate::llm_commands::record_incomplete(&state, &id);
        assert!(state.ai_queue.cancel_running(&id));
        let ran = rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(5), worker).await })
            .expect("a cancelled pass ends at once")
            .unwrap();
        assert!(ran);
        model
            .hung_up
            .recv_timeout(Duration::from_secs(5))
            .expect("the request to the model was dropped, so the server stops generating");

        assert_eq!(stage_of(&state, &id), None, "its busy marker is gone");
        assert!(!state.background_should_yield());
        assert!(!state.ai_queue.is_running(&id));
        assert!(state.ai_queue.snapshot().is_empty());
        assert!(state.ai_incomplete.lock().unwrap().is_empty());
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .all(|(_, stage)| stage != "done"));
        // the notice is told the lists as they are now
        assert_eq!(skips.lock().unwrap().last(), Some(&AiSkips::default()));
        // the meeting itself is untouched: it has a note
        assert!(state.storage.lock().unwrap().get_meeting(&id).is_ok());
    }

    /// The privacy case: the note is deleted while the model is working on
    /// its meeting. Deleting can't purge a busy meeting, so the pass does it
    /// on its way out: nothing of the meeting stays in the database, and the
    /// notice stops counting it.
    #[test]
    fn deleting_a_note_whose_pass_is_running_purges_the_meeting_when_the_pass_ends() {
        let dir = tempfile::tempdir().unwrap();
        let (state, model) = state_with_hung_model(dir.path());
        let state = std::sync::Arc::new(state);
        let id = meeting_with_transcript(&state);
        let note_id = state
            .storage
            .lock()
            .unwrap()
            .get_meeting(&id)
            .unwrap()
            .note_id;
        let (_events, sink) = collecting();
        queue_ai_pass(&state, &sink, &id, AiJobKind::AfterTranscription);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let worker = rt.spawn({
            let state = state.clone();
            async move { run_next_ai_job(&state, &|_| {}, &|_| {}, &|| {}).await }
        });
        model
            .accepted
            .recv_timeout(Duration::from_secs(20))
            .unwrap();
        crate::llm_commands::record_incomplete(&state, &id);

        crate::commands::delete_note_with(&state, &|_| {}, &note_id).unwrap();
        rt.block_on(async { tokio::time::timeout(Duration::from_secs(10), worker).await })
            .expect("the pass was told to stop")
            .unwrap();

        let storage = state.storage.lock().unwrap();
        assert!(storage.get_meeting(&id).is_err(), "the meeting is purged");
        assert!(storage.get_transcript(&id).unwrap().is_none());
        assert!(storage.get_meeting_items(&id).unwrap().is_empty());
        assert!(!storage.has_meeting_items(&id).unwrap());
        drop(storage);
        assert!(state.ai_incomplete.lock().unwrap().is_empty());
        assert_eq!(stage_of(&state, &id), None);
    }

    /// A pass that runs to its end after its note was deleted (it was too late
    /// to cancel, or was cancelled and finished in the same breath) purges
    /// the meeting too, and none of the effects of a finished transcript.
    #[test]
    fn a_pass_that_ends_for_a_deleted_note_purges_the_meeting() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        refusing_provider(&state);
        let id = meeting_with_transcript(&state);
        let note_id = state
            .storage
            .lock()
            .unwrap()
            .get_meeting(&id)
            .unwrap()
            .note_id;
        let (events, sink) = collecting();
        queue_ai_pass(&state, &sink, &id, AiJobKind::AfterTranscription);
        // the note goes while the pass waits its turn; the meeting is left
        // for whoever is busy with it
        state.storage.lock().unwrap().delete_note(&note_id).unwrap();
        assert!(state.storage.lock().unwrap().get_meeting(&id).is_ok());

        let finals = std::sync::atomic::AtomicUsize::new(0);
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(rt.block_on(run_next_ai_job(&state, &sink, &|_| {}, &|| {
            finals.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })));
        assert!(state.storage.lock().unwrap().get_meeting(&id).is_err());
        assert!(state
            .storage
            .lock()
            .unwrap()
            .get_transcript(&id)
            .unwrap()
            .is_none());
        assert_eq!(finals.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .all(|(_, stage)| stage != "done"));
    }

    /// A cleanup that takes too long goes on its own list, apart from action
    /// items that couldn't be read: the notice says so in other words. The
    /// request is dropped, the busy marker taken back, and nothing is recorded
    /// as an extraction problem.
    #[test]
    fn a_cleanup_that_times_out_is_recorded_as_a_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let (state, model) = state_with_hung_model(dir.path());
        let id = meeting_with_transcript(&state);
        let (_events, sink) = collecting();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(polish_within(
            &state,
            &sink,
            &id,
            Duration::from_millis(1500),
        ));

        model
            .accepted
            .recv_timeout(Duration::from_secs(5))
            .expect("the cleanup reached the model");
        model
            .hung_up
            .recv_timeout(Duration::from_secs(5))
            .expect("and its request was dropped when time ran out");
        let skips = crate::llm_commands::current_skips(&state);
        assert_eq!(skips.cut_off.cleanup, std::slice::from_ref(&id));
        assert!(skips.cut_off.items.is_empty() && skips.consent.is_empty());
        assert_eq!(stage_of(&state, &id), None, "the marker is taken back");
    }

    /// A delete that fails leaves the note and its pass alone: the pass is
    /// stopped only once the delete has worked.
    #[test]
    fn a_delete_that_fails_does_not_stop_the_running_pass() {
        let dir = tempfile::tempdir().unwrap();
        let (state, model) = state_with_hung_model(dir.path());
        let state = std::sync::Arc::new(state);
        let id = meeting_with_transcript(&state);
        let note_id = state
            .storage
            .lock()
            .unwrap()
            .get_meeting(&id)
            .unwrap()
            .note_id;
        let (_events, sink) = collecting();
        queue_ai_pass(&state, &sink, &id, AiJobKind::AfterTranscription);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let worker = rt.spawn({
            let state = state.clone();
            async move { run_next_ai_job(&state, &|_| {}, &|_| {}, &|| {}).await }
        });
        model
            .accepted
            .recv_timeout(Duration::from_secs(20))
            .unwrap();

        // the note row is already gone (an earlier delete half-finished), so
        // deleting it fails
        state.storage.lock().unwrap().delete_note(&note_id).unwrap();
        assert!(crate::commands::delete_note_with(&state, &|_| {}, &note_id).is_err());
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            state.ai_queue.is_running(&id),
            "the failed delete left it running"
        );
        assert!(!worker.is_finished());

        // tidy up: stop it
        assert!(state.ai_queue.cancel_running(&id));
        rt.block_on(async { tokio::time::timeout(Duration::from_secs(10), worker).await })
            .expect("stopped")
            .unwrap();
    }

    // ------------------------------------------------ the saved queue

    /// A pass that takes the app down each time it runs is given up on: the
    /// count of starts is saved with the queue, and a pass that has started
    /// twice is not requeued a third time. One that only waited is.
    #[test]
    fn a_pass_that_keeps_crashing_the_app_is_given_up_on() {
        let dir = tempfile::tempdir().unwrap();
        let (_events, sink) = collecting();
        let (crashy, bystander) = {
            let state = test_state(dir.path());
            let crashy = meeting_with_transcript(&state);
            let bystander = meeting_with_transcript(&state);
            queue_ai_pass(&state, &sink, &crashy, AiJobKind::AfterTranscription);
            queue_ai_pass(&state, &sink, &bystander, AiJobKind::AfterTranscription);
            (crashy, bystander)
        };
        // Each launch recovers the saved queue; the crashy pass then starts
        // and the process dies inside it (run_next_ai_job saves the attempt
        // before the pass runs, and nothing is written after that).
        let launch = |expect_crashy: bool| -> Option<u32> {
            let state = test_state(dir.path());
            recover_ai_passes(&state, &sink);
            let queued: Vec<String> = state
                .ai_queue
                .snapshot()
                .into_iter()
                .map(|j| j.meeting_id)
                .collect();
            assert_eq!(queued.contains(&crashy), expect_crashy, "{queued:?}");
            assert!(queued.contains(&bystander), "{queued:?}");
            let started = expect_crashy.then(|| {
                let started = state.ai_queue.next().unwrap();
                assert_eq!(started.0.meeting_id, crashy);
                persist_ai_queue(&state);
                started
            });
            let attempts = job_attempts(&state, &crashy);
            drop(started);
            attempts
        };
        assert_eq!(launch(true), Some(1), "launch 2: first start on record");
        assert_eq!(launch(true), Some(2), "launch 3: second start on record");
        assert_eq!(launch(false), None, "launch 4: given up on");
    }

    fn job_attempts(state: &AppState, meeting_id: &str) -> Option<u32> {
        state
            .ai_queue
            .snapshot()
            .into_iter()
            .find(|j| j.meeting_id == meeting_id)
            .map(|j| j.attempts)
    }

    /// A quit is not a crash. The attempt counted when a pass starts is given
    /// back when the app quits normally, so a long local pass that is
    /// interrupted by quits (and by restarts for an update) is never given up
    /// on; only passes that were running when the process died are.
    #[test]
    fn quitting_during_a_pass_is_not_a_crash_of_the_pass() {
        let dir = tempfile::tempdir().unwrap();
        let (_events, sink) = collecting();
        let id = {
            let state = test_state(dir.path());
            let id = meeting_with_transcript(&state);
            queue_ai_pass(&state, &sink, &id, AiJobKind::AfterTranscription);
            id
        };
        for launch in 1..=5 {
            let state = test_state(dir.path());
            recover_ai_passes(&state, &sink);
            assert_eq!(
                job_attempts(&state, &id),
                Some(0),
                "launch {launch}: still queued, no failed attempt on record"
            );
            let started = state.ai_queue.next().unwrap();
            persist_ai_queue(&state); // as run_next_ai_job does before the pass
            assert_eq!(job_attempts(&state, &id), Some(1));
            save_ai_queue_on_quit(&state); // the app quits mid-pass
            let row = state
                .storage
                .lock()
                .unwrap()
                .get_setting(AI_QUEUE_KEY)
                .unwrap()
                .unwrap();
            assert!(row.contains("\"attempts\":0"), "{row}");
            drop(started);
        }
    }

    /// An unreadable saved queue (damaged, or from a newer build) is a queue
    /// of nothing, not a crash at startup, and it is overwritten.
    #[test]
    fn an_unreadable_saved_queue_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let (_events, sink) = collecting();
        let id = meeting_with_transcript(&state);
        for junk in ["{not json", "[{\"meeting_id\": 7}]", "\"[]\"", ""] {
            state
                .storage
                .lock()
                .unwrap()
                .set_setting(AI_QUEUE_KEY, junk)
                .unwrap();
            recover_ai_passes(&state, &sink);
            assert!(state.ai_queue.snapshot().is_empty(), "{junk:?}");
            assert_eq!(
                state
                    .storage
                    .lock()
                    .unwrap()
                    .get_setting(AI_QUEUE_KEY)
                    .unwrap()
                    .as_deref(),
                Some("[]"),
                "{junk:?} was replaced"
            );
        }
        // and a good row after the junk still comes back
        queue_ai_pass(&state, &sink, &id, AiJobKind::ConsentRerun);
        let state2 = {
            drop(state);
            test_state(dir.path())
        };
        recover_ai_passes(&state2, &sink);
        assert_eq!(state2.ai_queue.snapshot().len(), 1);
    }

    /// Writers that race leave the newest queue saved, never an older one
    /// written last: the snapshot is taken inside the lock the write holds.
    #[test]
    fn racing_writers_leave_the_newest_queue_saved() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        std::thread::scope(|scope| {
            for t in 0..6 {
                let state = &state;
                scope.spawn(move || {
                    for i in 0..40 {
                        let id = format!("m-{t}-{i}");
                        state
                            .ai_queue
                            .push(AiJob::new(&id, AiJobKind::AfterTranscription));
                        persist_ai_queue(state);
                        state.ai_queue.remove_pending(&id);
                        persist_ai_queue(state);
                    }
                });
            }
        });
        let saved = state
            .storage
            .lock()
            .unwrap()
            .get_setting(AI_QUEUE_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(saved, "[]", "everything was removed again");
    }

    /// The notice must not keep counting a meeting once it is deleted.
    #[test]
    fn deleting_a_note_clears_what_the_notice_counted_for_its_meeting() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let id = real_meeting(&state);
        let note_id = state
            .storage
            .lock()
            .unwrap()
            .get_meeting(&id)
            .unwrap()
            .note_id;
        state.consent_skipped.lock().unwrap().insert(id.clone());
        state.ai_incomplete.lock().unwrap().insert(id.clone());
        state.cleanup_incomplete.lock().unwrap().insert(id.clone());
        crate::commands::delete_note_with(&state, &|_| {}, &note_id).unwrap();
        assert_eq!(
            crate::llm_commands::current_skips(&state),
            AiSkips::default()
        );
    }

    /// "Run them now" runs both lists: the consent skips and the meetings the
    /// model stopped short of, once each.
    #[test]
    fn the_rerun_takes_the_cut_off_meetings_too() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let (_events, sink) = collecting();
        let refused = real_meeting(&state);
        let cut_off = real_meeting(&state);
        let uncleaned = real_meeting(&state);
        state
            .consent_skipped
            .lock()
            .unwrap()
            .insert(refused.clone());
        state.ai_incomplete.lock().unwrap().insert(cut_off.clone());
        state
            .cleanup_incomplete
            .lock()
            .unwrap()
            .insert(uncleaned.clone());
        // one meeting on several lists runs once
        state.ai_incomplete.lock().unwrap().insert(refused.clone());
        state
            .cleanup_incomplete
            .lock()
            .unwrap()
            .insert(cut_off.clone());

        assert_eq!(queue_consent_rerun(&state, &sink), 3);
        assert_eq!(
            crate::llm_commands::current_skips(&state),
            AiSkips::default()
        );
        assert!(state.ai_queue.is_pending(&refused) && state.ai_queue.is_pending(&cut_off));
        assert!(state.ai_queue.is_pending(&uncleaned));
    }

    /// A quota 429 is transient by definition (the window reopens) — it must
    /// never read as the permanent Rejected marker, or one busy hour would
    /// park the job as failed.
    #[test]
    fn rate_limited_cloud_errors_are_retryable() {
        let e = fly_asr::AsrError::RateLimited {
            message: "groq returned 429 Too Many Requests: rate limit reached".into(),
            retry_after: None,
        };
        assert!(!permanent_failure(&e.to_string()));
    }
}
