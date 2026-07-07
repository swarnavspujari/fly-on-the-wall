//! Recording-is-sacred E2E (heavy, `#[ignore]`): with a recording ACTIVE,
//! a queued transcription must defer; the recording finishes clean; the
//! queued job then runs the REAL pipeline (whisper.cpp + sherpa-onnx) and
//! the transcript persists. Uses the committed golden fixture as the queued
//! meeting and a real cpal capture (default devices) as the active one,
//! falling back to a stub session on machines without audio devices.
//!
//! Run locally with:
//!   cargo test -p looma-app --test scheduling_e2e -- --ignored --nocapture

use std::sync::Arc;

use looma_app_lib::recording::ActiveRecording;
use looma_app_lib::scheduler::{self, Tick};
use looma_app_lib::state::AppState;
use looma_core::RecordingRef;

struct StubSession;

impl looma_audio::CaptureSession for StubSession {
    fn pause(&mut self) -> looma_audio::Result<()> {
        Ok(())
    }
    fn resume(&mut self) -> looma_audio::Result<()> {
        Ok(())
    }
    fn stop(self: Box<Self>) -> looma_audio::Result<looma_audio::CaptureOutput> {
        Ok(looma_audio::CaptureOutput {
            mic_path: None,
            system_path: None,
            mixed_path: None,
            duration_ms: 0,
        })
    }
    fn state(&self) -> looma_audio::CaptureState {
        looma_audio::CaptureState::Recording
    }
    fn elapsed_ms(&self) -> u64 {
        0
    }
}

/// Recursively hardlink a directory tree (same-volume, instant, no copies).
fn link_tree(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            link_tree(&entry.path(), &target)?;
        } else {
            std::fs::hard_link(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[test]
#[ignore = "needs whisper/sherpa artifacts in %APPDATA%/Looma; run with --ignored"]
fn transcription_defers_to_recording_then_completes() {
    let real_data = dirs::data_dir().unwrap().join("Looma");
    let needed = [
        "bin/whisper/Release/whisper-cli.exe",
        "bin/sherpa/sherpa-onnx-v1.13.3-win-x64-shared-MD-Release/bin/sherpa-onnx-offline-speaker-diarization.exe",
        "models/diarize/sherpa-onnx-pyannote-segmentation-3-0/model.onnx",
        "models/diarize/campplus.onnx",
        "models/asr/ggml-small-q5_1.bin",
    ];
    if needed.iter().any(|p| !real_data.join(p).exists()) {
        eprintln!(
            "SKIP: artifacts not installed under {}",
            real_data.display()
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    for sub in ["bin/whisper", "bin/sherpa", "models/diarize"] {
        link_tree(&real_data.join(sub), &data_dir.join(sub)).unwrap();
    }
    std::fs::create_dir_all(data_dir.join("models/asr")).unwrap();
    std::fs::hard_link(
        real_data.join("models/asr/ggml-small-q5_1.bin"),
        data_dir.join("models/asr/ggml-small-q5_1.bin"),
    )
    .unwrap();

    let state = AppState::init_with(
        data_dir.clone(),
        Arc::new(looma_secrets::MemorySecretStore::default()),
    )
    .unwrap();

    // ---- meeting A: finished earlier, waiting to be transcribed ----
    let meeting_a = {
        let storage = state.storage.lock().unwrap();
        storage.set_setting("asr.tier", "light").unwrap();
        storage
            .set_setting("asr.model_id", "ggml-small-q5_1")
            .unwrap();
        let note = storage.create_note("Meeting A", None).unwrap();
        let meeting = storage.create_meeting("Meeting A", &note.id, &[]).unwrap();
        let rec_dir = data_dir.join("recordings").join(&meeting.id);
        std::fs::create_dir_all(&rec_dir).unwrap();
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/meeting-fixture.wav");
        std::fs::copy(&fixture, rec_dir.join("recording.mixed.wav")).unwrap();
        storage
            .end_meeting(
                &meeting.id,
                &RecordingRef {
                    mic_path: None,
                    system_path: None,
                    mixed_path: Some(format!("recordings/{}/recording.mixed.wav", meeting.id)),
                    duration_ms: 27_540,
                },
            )
            .unwrap();
        meeting.id
    };

    // ---- meeting B: recording RIGHT NOW (real capture when possible) ----
    let meeting_b = {
        let storage = state.storage.lock().unwrap();
        let note = storage.create_note("Meeting B", None).unwrap();
        storage
            .create_meeting("Meeting B", &note.id, &[])
            .unwrap()
            .id
    };
    let out_dir = data_dir.join("recordings").join(&meeting_b);
    let (session, real_capture) = match state.audio.start(looma_audio::CaptureConfig {
        mic_device_id: None,
        capture_system: true,
        out_dir: out_dir.clone(),
        base_name: "recording".into(),
    }) {
        Ok(s) => {
            eprintln!("recording meeting B with the real cpal backend");
            (s, true)
        }
        Err(e) => {
            eprintln!("no audio devices ({e}); recording meeting B with a stub session");
            (
                Box::new(StubSession) as Box<dyn looma_audio::CaptureSession>,
                false,
            )
        }
    };
    *state.recording.lock().unwrap() = Some(ActiveRecording {
        session,
        meeting_id: meeting_b.clone(),
        note_id: "n".into(),
        live_stop: None,
    });

    let on_stage = |p: looma_app_lib::pipeline::PipelineProgress| {
        eprintln!("stage[{}]: {}", p.meeting_id, p.stage)
    };
    let on_model = |p: looma_app_lib::models::ModelProgress| eprintln!("model: {}", p.stage);
    let rt = tokio::runtime::Runtime::new().unwrap();

    // 1. queue A while B records → must defer, however often we ask
    scheduler::enqueue(&state, &on_stage, &meeting_a).unwrap();
    for _ in 0..3 {
        assert!(matches!(
            rt.block_on(scheduler::tick(&state, &on_stage, &on_model)),
            Tick::RecordingActive
        ));
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
    {
        let storage = state.storage.lock().unwrap();
        assert!(storage.get_transcript(&meeting_a).unwrap().is_none());
        assert_eq!(
            storage
                .transcription_job(&meeting_a)
                .unwrap()
                .unwrap()
                .status,
            looma_storage::JOB_QUEUED
        );
    }

    // 2. B stops → must be a clean recording, then it queues up too
    let rec_b = state.recording.lock().unwrap().take().unwrap();
    let output = rec_b.session.stop().expect("recording B must stop clean");
    if real_capture {
        let mic = output.mic_path.as_ref().expect("mic wav missing");
        assert!(mic.exists(), "mic wav not on disk: {}", mic.display());
        assert!(output.duration_ms > 0, "captured no audio");
        eprintln!(
            "meeting B recorded clean: {} ms at {}",
            output.duration_ms,
            mic.display()
        );
        let to_rel = |p: &std::path::PathBuf| -> Option<String> {
            p.strip_prefix(&data_dir)
                .ok()
                .map(|r| r.to_string_lossy().replace('\\', "/"))
        };
        let storage = state.storage.lock().unwrap();
        storage
            .end_meeting(
                &meeting_b,
                &RecordingRef {
                    mic_path: output.mic_path.as_ref().and_then(to_rel),
                    system_path: output.system_path.as_ref().and_then(to_rel),
                    mixed_path: output.mixed_path.as_ref().and_then(to_rel),
                    duration_ms: output.duration_ms,
                },
            )
            .unwrap();
        drop(storage);
        scheduler::enqueue(&state, &on_stage, &meeting_b).unwrap();
    }

    // 3. the queue drains in order: A first, then (if recorded) B
    match rt.block_on(scheduler::tick(&state, &on_stage, &on_model)) {
        Tick::Completed(id) => assert_eq!(id, meeting_a),
        _ => panic!("meeting A should transcribe once recording ended"),
    }
    if real_capture {
        match rt.block_on(scheduler::tick(&state, &on_stage, &on_model)) {
            Tick::Completed(id) => assert_eq!(id, meeting_b),
            _ => panic!("meeting B should transcribe after A"),
        }
    }
    assert!(matches!(
        rt.block_on(scheduler::tick(&state, &on_stage, &on_model)),
        Tick::Idle
    ));

    // 4. the transcript persisted, and jobs are done
    let storage = state.storage.lock().unwrap();
    let transcript = storage.get_transcript(&meeting_a).unwrap().unwrap();
    assert!(
        !transcript.segments.is_empty(),
        "meeting A transcript should have segments"
    );
    assert_eq!(
        storage
            .transcription_job(&meeting_a)
            .unwrap()
            .unwrap()
            .status,
        looma_storage::JOB_DONE
    );
    if real_capture {
        assert_eq!(
            storage
                .transcription_job(&meeting_b)
                .unwrap()
                .unwrap()
                .status,
            looma_storage::JOB_DONE
        );
    }
    eprintln!(
        "meeting A transcribed after deferral: {} segments",
        transcript.segments.len()
    );
}
