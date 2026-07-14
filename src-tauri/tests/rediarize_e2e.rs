//! Re-diarize E2E (spec: attendee-informed diarization): runs the REAL
//! pipeline over the committed two-voice fixture, then exercises
//! `re_diarize_with` + revert and asserts the three contract points:
//!   (a) re-diarize (default AND with a confirmed attendee count) saves a
//!       transcript with updated speaker assignment and UNCHANGED text/ids;
//!   (b) revert restores the prior assignment byte-for-byte;
//!   (c) the polished (cleaned) variant's text is untouched throughout.
//!
//! Heavy + needs artifacts on disk, so it is `#[ignore]` for plain
//! `cargo test`. Run locally with:
//!   cargo test -p fly-app --test rediarize_e2e -- --ignored --nocapture

use fly_app_lib::pipeline;
use fly_app_lib::state::AppState;
use fly_core::{Attendee, RecordingRef, Transcript};

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

/// (segment id, text) pairs — the invariant re-diarize must never touch.
fn texts(t: &Transcript) -> Vec<(String, String)> {
    t.segments
        .iter()
        .map(|s| (s.id.clone(), s.text.clone()))
        .collect()
}

/// The speaker-assignment state as a canonical JSON string, for the
/// byte-for-byte revert assertion.
fn assignment_state(t: &Transcript) -> String {
    let keys: Vec<(&str, &str)> = t
        .segments
        .iter()
        .map(|s| (s.id.as_str(), s.speaker_key.as_str()))
        .collect();
    serde_json::to_string(&(keys, &t.speakers)).unwrap()
}

#[test]
#[ignore = "needs whisper/sherpa artifacts in %APPDATA%/FlyOnTheWall; run with --ignored"]
fn rediarize_revert_and_polish_contract() {
    let real_data = dirs::data_dir().unwrap().join("FlyOnTheWall");
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
        std::sync::Arc::new(fly_secrets::MemorySecretStore::default()),
    )
    .unwrap();

    // fixture as a single mixed track (the import path: diarize whole track)
    let meeting_id = {
        let storage = state.storage.lock().unwrap();
        storage.set_setting("asr.tier", "light").unwrap();
        storage
            .set_setting("asr.model_id", "ggml-small-q5_1")
            .unwrap();
        storage.set_setting("asr.use_gpu", "false").unwrap();
        let note = storage.create_note("Rediarize fixture", None).unwrap();
        let meeting = storage
            .create_meeting("Rediarize fixture", &note.id, &[])
            .unwrap();
        let rec_dir = data_dir.join("recordings").join(&meeting.id);
        std::fs::create_dir_all(&rec_dir).unwrap();
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/meeting-fixture.wav");
        assert!(fixture.exists(), "fixture wav missing from repo");
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

    let on_stage = |p: pipeline::PipelineProgress| eprintln!("stage: {}", p.stage);
    let on_model = |p: fly_app_lib::models::ModelProgress| eprintln!("model: {}", p.stage);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let raw = runtime
        .block_on(pipeline::run_with(
            &state,
            &on_stage,
            &on_model,
            &meeting_id,
        ))
        .expect("pipeline should succeed");
    let raw_texts = texts(&raw);

    // Simulate the polish pass: same ids/keys/timestamps, only text differs
    // (the storage layer's provenance contract, no LLM needed).
    let cleaned_texts: Vec<(String, String)> = {
        let storage = state.storage.lock().unwrap();
        let mut cleaned = raw.clone();
        for seg in &mut cleaned.segments {
            seg.text = format!("{} [polished]", seg.text.trim());
        }
        storage.save_cleaned_transcript(&cleaned).unwrap();
        texts(&cleaned)
    };

    // Manually assign an attendee to the second voice (label machinery).
    let zira_key = raw
        .segments
        .iter()
        .find(|s| s.text.to_lowercase().contains("engineering budget"))
        .map(|s| s.speaker_key.clone())
        .expect("second voice's line missing");
    {
        let storage = state.storage.lock().unwrap();
        storage
            .relabel_speaker(&meeting_id, &zira_key, "Zira")
            .unwrap();
    }

    // ---- (a) part 1: re-diarize with the DEFAULT engine options ----
    // (attendee list unconfirmed → no count is forced)
    let out_default = runtime
        .block_on(pipeline::re_diarize_with(
            &state,
            &on_stage,
            &on_model,
            &meeting_id,
        ))
        .expect("default re-diarize should succeed");
    eprintln!(
        "default re-diarize: {} segment(s) changed",
        out_default.changed_segments
    );
    assert_eq!(
        texts(&out_default.transcript),
        raw_texts,
        "re-diarize must never change segment ids or text"
    );

    // ---- (a) part 2: confirmed attendees → num_speakers = 2 ----
    {
        let storage = state.storage.lock().unwrap();
        storage
            .update_attendees(
                &meeting_id,
                &[Attendee {
                    name: "Zira".into(),
                    email: None,
                }],
            )
            .unwrap();
    }
    let out_forced = runtime
        .block_on(pipeline::re_diarize_with(
            &state,
            &on_stage,
            &on_model,
            &meeting_id,
        ))
        .expect("num_speakers=2 re-diarize should succeed");
    let saved = {
        let storage = state.storage.lock().unwrap();
        storage.get_transcript(&meeting_id).unwrap().unwrap()
    };
    assert_eq!(
        assignment_state(&out_forced.transcript),
        assignment_state(&saved),
        "the returned transcript must match what was persisted"
    );
    assert_eq!(texts(&saved), raw_texts, "saved text must be unchanged");
    let distinct: std::collections::HashSet<_> = saved
        .segments
        .iter()
        .map(|s| s.speaker_key.as_str())
        .filter(|k| *k != "spk_unknown")
        .collect();
    assert_eq!(
        distinct.len(),
        2,
        "forced 2-cluster diarization should yield 2 speakers, got {distinct:?}"
    );
    // stable cluster identity on this clean fixture → the manual attendee
    // assignment survives the re-diarize
    assert!(
        saved.speakers.iter().any(|s| s.label == "Zira"),
        "stable cluster should keep its attendee assignment; speakers: {:?}",
        saved.speakers
    );

    // ---- (b) setup: force a visible reassignment ("Just you"), then revert ----
    let state_before = assignment_state(&saved);
    {
        let storage = state.storage.lock().unwrap();
        storage.update_attendees(&meeting_id, &[]).unwrap(); // just you
    }
    let out_justyou = runtime
        .block_on(pipeline::re_diarize_with(
            &state,
            &on_stage,
            &on_model,
            &meeting_id,
        ))
        .expect("just-you re-diarize should succeed");
    assert!(
        out_justyou.changed_segments > 0,
        "just-you must re-attribute the diarized segments"
    );
    assert!(
        out_justyou
            .transcript
            .segments
            .iter()
            .all(|s| s.speaker_key == "mic"),
        "just-you attributes every line to You"
    );
    assert_eq!(texts(&out_justyou.transcript), raw_texts);

    // ---- (b) revert restores the prior assignment byte-for-byte ----
    let restored = {
        let storage = state.storage.lock().unwrap();
        storage.revert_speaker_assignment(&meeting_id).unwrap()
    };
    assert_eq!(
        assignment_state(&restored),
        state_before,
        "revert must restore per-segment keys + label map exactly"
    );
    assert_eq!(texts(&restored), raw_texts);
    {
        let storage = state.storage.lock().unwrap();
        // snapshot consumed (one level of undo)
        assert!(storage.get_speaker_snapshot(&meeting_id).unwrap().is_none());
    }

    // ---- (c) polish output untouched through all of the above ----
    let cleaned_after = {
        let storage = state.storage.lock().unwrap();
        storage
            .get_cleaned_transcript(&meeting_id)
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        texts(&cleaned_after),
        cleaned_texts,
        "the polished text must survive re-diarize + revert untouched"
    );
    // and its speaker keys mirror the restored raw assignment
    assert_eq!(
        assignment_state(&cleaned_after),
        assignment_state(&restored)
    );
}
