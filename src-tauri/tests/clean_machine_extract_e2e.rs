//! Clean-machine artifact E2E (heavy, `#[ignore]`, needs network): with the
//! sherpa CLI and pyannote model ABSENT from the data dir — the state every
//! fresh Windows install is in — a transcribe must lazily download the
//! .tar.bz2 artifacts and extract them fully in-process. The old extractor
//! shelled out to Windows' bsdtar, which delegates bzip2 to an external
//! binary most machines lack ("Can't initialize filter; unable to run
//! program bzip2 -d"). Only whisper (bin + small model) is linked in, so a
//! pass proves the download → in-process extract → diarize path end to end.
//!
//! Run locally with:
//!   cargo test -p fly-app --test clean_machine_extract_e2e -- --ignored --nocapture

use std::sync::Arc;

use fly_app_lib::scheduler::{self, Tick};
use fly_app_lib::state::AppState;
use fly_core::RecordingRef;

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
#[ignore = "needs whisper artifacts in %APPDATA%/FlyOnTheWall + network; run with --ignored"]
fn transcribe_downloads_and_extracts_diarization_artifacts_in_process() {
    let real_data = dirs::data_dir().unwrap().join("FlyOnTheWall");
    let needed = [
        "bin/whisper/Release/whisper-cli.exe",
        "models/asr/ggml-small-q5_1.bin",
    ];
    if needed.iter().any(|p| !real_data.join(p).exists()) {
        eprintln!(
            "SKIP: whisper artifacts not installed under {}",
            real_data.display()
        );
        return;
    }

    // A data dir with ONLY whisper — sherpa + pyannote deliberately absent,
    // exactly like a clean machine after the first model download.
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    link_tree(
        &real_data.join("bin/whisper"),
        &data_dir.join("bin/whisper"),
    )
    .unwrap();
    std::fs::create_dir_all(data_dir.join("models/asr")).unwrap();
    std::fs::hard_link(
        real_data.join("models/asr/ggml-small-q5_1.bin"),
        data_dir.join("models/asr/ggml-small-q5_1.bin"),
    )
    .unwrap();
    // campplus is a plain-file artifact (no extraction involved) — link it
    // when available so the test only downloads the two archives under test.
    if real_data.join("models/diarize/campplus.onnx").exists() {
        std::fs::create_dir_all(data_dir.join("models/diarize")).unwrap();
        std::fs::hard_link(
            real_data.join("models/diarize/campplus.onnx"),
            data_dir.join("models/diarize/campplus.onnx"),
        )
        .unwrap();
    }
    assert!(!data_dir.join("bin/sherpa").exists());
    assert!(!data_dir
        .join("models/diarize/sherpa-onnx-pyannote-segmentation-3-0")
        .exists());

    let state = AppState::init_with(
        data_dir.clone(),
        Arc::new(fly_secrets::MemorySecretStore::default()),
    )
    .unwrap();

    let meeting_id = {
        let storage = state.storage.lock().unwrap();
        storage.set_setting("asr.tier", "light").unwrap();
        storage.set_setting("asr.use_gpu", "false").unwrap();
        storage
            .set_setting("asr.model_id", "ggml-small-q5_1")
            .unwrap();
        let note = storage.create_note("Clean machine", None).unwrap();
        let meeting = storage
            .create_meeting("Clean machine", &note.id, &[])
            .unwrap();
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
                    playback_path: None,
                    duration_ms: 27_540,
                },
            )
            .unwrap();
        meeting.id
    };

    let on_stage = |p: fly_app_lib::pipeline::PipelineProgress| {
        eprintln!("stage[{}]: {}", p.meeting_id, p.stage)
    };
    let on_model = |p: fly_app_lib::models::ModelProgress| {
        eprintln!("model[{}]: {} {}/{}", p.id, p.stage, p.downloaded, p.total)
    };
    let rt = tokio::runtime::Runtime::new().unwrap();

    scheduler::enqueue(&state, &on_stage, &meeting_id).unwrap();
    match rt.block_on(scheduler::tick(&state, &on_stage, &on_model)) {
        Tick::Completed(id) => assert_eq!(id, meeting_id),
        Tick::Retrying { error, .. } | Tick::GaveUp { error, .. } => {
            panic!("pipeline failed on the clean data dir: {error}")
        }
        _ => panic!("expected the queued meeting to transcribe"),
    }

    // the archives were downloaded and extracted in-process into the clean dir
    assert!(data_dir
        .join("bin/sherpa/sherpa-onnx-v1.13.3-win-x64-shared-MD-Release/bin/sherpa-onnx-offline-speaker-diarization.exe")
        .exists());
    assert!(data_dir
        .join("models/diarize/sherpa-onnx-pyannote-segmentation-3-0/model.onnx")
        .exists());

    let transcript = state
        .storage
        .lock()
        .unwrap()
        .get_transcript(&meeting_id)
        .unwrap()
        .expect("transcript must persist");
    assert!(
        !transcript.segments.is_empty(),
        "transcript should have segments"
    );
    eprintln!(
        "clean-machine transcribe OK: {} segments, engine {}",
        transcript.segments.len(),
        transcript.engine
    );
}
