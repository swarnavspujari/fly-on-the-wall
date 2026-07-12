//! Scheduler behavior without sidecars: recording defers the queue, and a
//! job that keeps failing is retried a bounded number of times, then parked
//! as failed (never silently dropped). The full defer→record→transcribe
//! guarantee runs in scheduling_e2e.rs (needs artifacts).

use std::sync::Arc;

use fly_app_lib::recording::ActiveRecording;
use fly_app_lib::scheduler::{self, Tick};
use fly_app_lib::state::AppState;

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
            duration_ms: 0,
        })
    }
    fn state(&self) -> fly_audio::CaptureState {
        fly_audio::CaptureState::Recording
    }
    fn elapsed_ms(&self) -> u64 {
        0
    }
}

fn test_state() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::init_with(
        dir.path().to_path_buf(),
        Arc::new(fly_secrets::MemorySecretStore::default()),
    )
    .unwrap();
    (dir, state)
}

/// A meeting whose transcription will fail fast (no recording attached).
fn meeting_without_recording(state: &AppState) -> String {
    let storage = state.storage.lock().unwrap();
    let note = storage.create_note("t", None).unwrap();
    storage.create_meeting("t", &note.id, &[]).unwrap().id
}

fn fake_recording(meeting_id: &str) -> ActiveRecording {
    ActiveRecording {
        session: Box::new(FakeSession),
        meeting_id: meeting_id.into(),
        note_id: "n".into(),
        live_stop: None,
    }
}

fn on_stage() -> impl Fn(fly_app_lib::pipeline::PipelineProgress) + Send + Sync {
    |_| {}
}

fn on_model() -> impl Fn(fly_app_lib::models::ModelProgress) + Send + Sync {
    |_| {}
}

#[test]
fn queue_defers_while_recording_then_attempts_after() {
    let (_dir, state) = test_state();
    let meeting_id = meeting_without_recording(&state);
    let stage = on_stage();
    let model = on_model();

    scheduler::enqueue(&state, &stage, &meeting_id).unwrap();
    // queued state is visible immediately
    assert_eq!(
        state
            .pipeline_stage
            .lock()
            .unwrap()
            .get(&meeting_id)
            .map(String::as_str),
        Some(scheduler::WAITING_STAGE)
    );

    // recording active → the pipeline must not start
    *state.recording.lock().unwrap() = Some(fake_recording("other-meeting"));
    let rt = tokio::runtime::Runtime::new().unwrap();
    for _ in 0..3 {
        assert!(matches!(
            rt.block_on(scheduler::tick(&state, &stage, &model)),
            Tick::RecordingActive
        ));
    }
    // untouched: still queued, still marked waiting
    {
        let storage = state.storage.lock().unwrap();
        let job = storage.transcription_job(&meeting_id).unwrap().unwrap();
        assert_eq!(job.status, fly_storage::JOB_QUEUED);
        assert_eq!(job.attempts, 0);
    }
    assert_eq!(
        state
            .pipeline_stage
            .lock()
            .unwrap()
            .get(&meeting_id)
            .map(String::as_str),
        Some(scheduler::WAITING_STAGE)
    );

    // recording stops → the very next tick attempts the job (it fails fast
    // here because the meeting has no recording — the attempt is the point)
    *state.recording.lock().unwrap() = None;
    match rt.block_on(scheduler::tick(&state, &stage, &model)) {
        Tick::Retrying {
            meeting_id: id,
            attempts,
            error,
        } => {
            assert_eq!(id, meeting_id);
            assert_eq!(attempts, 1);
            assert!(error.contains("no recording"), "unexpected error: {error}");
        }
        _ => panic!("expected the deferred job to be attempted after recording stopped"),
    }
}

#[test]
fn failing_job_is_retried_then_parked_as_failed() {
    let (_dir, state) = test_state();
    let meeting_id = meeting_without_recording(&state);
    let stage = on_stage();
    let model = on_model();
    let rt = tokio::runtime::Runtime::new().unwrap();

    scheduler::enqueue(&state, &stage, &meeting_id).unwrap();
    for expected_attempt in 1..scheduler::MAX_ATTEMPTS {
        match rt.block_on(scheduler::tick(&state, &stage, &model)) {
            Tick::Retrying { attempts, .. } => assert_eq!(attempts, expected_attempt),
            _ => panic!("attempt {expected_attempt} should retry"),
        }
    }
    match rt.block_on(scheduler::tick(&state, &stage, &model)) {
        Tick::GaveUp {
            meeting_id: id,
            error,
        } => {
            assert_eq!(id, meeting_id);
            assert!(!error.is_empty());
        }
        _ => panic!("final attempt should give up"),
    }

    let storage = state.storage.lock().unwrap();
    let job = storage.transcription_job(&meeting_id).unwrap().unwrap();
    assert_eq!(job.status, fly_storage::JOB_FAILED);
    assert_eq!(job.attempts, scheduler::MAX_ATTEMPTS);
    assert!(job.last_error.is_some());
    // nothing schedulable left, and no ghost stage entry for the UI
    assert!(storage.next_transcription_job().unwrap().is_none());
    drop(storage);
    assert!(state
        .pipeline_stage
        .lock()
        .unwrap()
        .get(&meeting_id)
        .is_none());

    // the user asking again resets the failure
    scheduler::enqueue(&state, &stage, &meeting_id).unwrap();
    let storage = state.storage.lock().unwrap();
    let job = storage.transcription_job(&meeting_id).unwrap().unwrap();
    assert_eq!(job.status, fly_storage::JOB_QUEUED);
    assert_eq!(job.attempts, 0);
}

/// A job the app died on (or never got to) must come back schedulable and
/// visible after a restart — reopen the same data dir with a fresh state,
/// as app startup does, and run the worker's recovery step.
#[test]
fn queued_and_running_jobs_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(fly_secrets::MemorySecretStore::default());
    let (queued_id, running_id) = {
        let state = AppState::init_with(dir.path().to_path_buf(), secrets.clone()).unwrap();
        let queued_id = meeting_without_recording(&state);
        let running_id = meeting_without_recording(&state);
        let storage = state.storage.lock().unwrap();
        storage.enqueue_transcription(&queued_id).unwrap();
        storage.enqueue_transcription(&running_id).unwrap();
        storage.mark_transcription_running(&running_id).unwrap();
        (queued_id, running_id)
        // state dropped = app gone mid-transcription
    };

    let state = AppState::init_with(dir.path().to_path_buf(), secrets).unwrap();
    let stage = on_stage();
    scheduler::recover(&state, &stage);

    let stages = state.pipeline_stage.lock().unwrap();
    for id in [&queued_id, &running_id] {
        let storage = state.storage.lock().unwrap();
        let job = storage.transcription_job(id).unwrap().unwrap();
        assert_eq!(job.status, fly_storage::JOB_QUEUED, "job {id}");
        assert_eq!(
            stages.get(id.as_str()).map(String::as_str),
            Some(scheduler::WAITING_STAGE),
            "job {id} should be surfaced as waiting"
        );
    }
}
