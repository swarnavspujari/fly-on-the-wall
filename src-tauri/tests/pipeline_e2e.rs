//! Golden transcription/diarization test (spec §11): runs the REAL pipeline
//! (whisper.cpp + sherpa-onnx sidecars) over the committed two-voice TTS
//! fixture and asserts WER and speaker attribution stay within tolerance.
//!
//! Heavy + needs artifacts on disk, so it is `#[ignore]` for plain
//! `cargo test`. Run locally with:
//!   cargo test -p looma-app --test pipeline_e2e -- --ignored --nocapture
//!
//! Artifacts are hardlinked from the real Looma data dir (%APPDATA%/Looma);
//! the test skips (passes with a notice) when they are not installed.

use looma_app_lib::pipeline;
use looma_app_lib::state::AppState;
use looma_core::RecordingRef;

const REFERENCE: &str = "good morning everyone let us start with the quarterly budget review \
thanks david the engineering budget is on track but marketing needs another fifty thousand dollars \
understood can you send me the updated forecast by friday \
yes i will share the forecast document tomorrow morning \
perfect then we all agree to approve the additional marketing spend";

fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Word error rate via Levenshtein distance over word tokens.
fn wer(reference: &str, hypothesis: &str) -> f64 {
    let r = normalize(reference);
    let h = normalize(hypothesis);
    let mut dp: Vec<Vec<usize>> = vec![vec![0; h.len() + 1]; r.len() + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=r.len() {
        for j in 1..=h.len() {
            let cost = usize::from(r[i - 1] != h[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[r.len()][h.len()] as f64 / r.len() as f64
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
fn golden_fixture_transcribes_and_diarizes() {
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

    // LOOMA_E2E_GPU=1 runs the golden fixture through the Vulkan GPU engine
    // instead of CPU (needs the whisper-bin-vulkan artifact installed; pick
    // the GPU with GGML_VK_VISIBLE_DEVICES). A pre-seeded "gpu" verdict
    // skips the in-pipeline benchmark so the test exercises decode, not the
    // gate.
    let gpu = std::env::var("LOOMA_E2E_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    if gpu
        && !real_data
            .join("bin/whisper-vulkan/Release/whisper-cli.exe")
            .exists()
    {
        eprintln!("SKIP: LOOMA_E2E_GPU set but whisper-bin-vulkan is not installed");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    for sub in ["bin/whisper", "bin/sherpa", "models/diarize"] {
        link_tree(&real_data.join(sub), &data_dir.join(sub)).unwrap();
    }
    if gpu {
        link_tree(
            &real_data.join("bin/whisper-vulkan"),
            &data_dir.join("bin/whisper-vulkan"),
        )
        .unwrap();
    }
    std::fs::create_dir_all(data_dir.join("models/asr")).unwrap();
    std::fs::hard_link(
        real_data.join("models/asr/ggml-small-q5_1.bin"),
        data_dir.join("models/asr/ggml-small-q5_1.bin"),
    )
    .unwrap();

    let state = AppState::init_with(
        data_dir.clone(),
        std::sync::Arc::new(looma_secrets::MemorySecretStore::default()),
    )
    .unwrap();

    // fixture as a single mixed track (the import path: diarize whole track)
    let (meeting_id, fixture_ok) = {
        let storage = state.storage.lock().unwrap();
        storage.set_setting("asr.tier", "light").unwrap();
        storage
            .set_setting("asr.model_id", "ggml-small-q5_1")
            .unwrap();
        // Deterministic engine selection: this test validates one specific
        // path, never whichever verdict this machine's benchmark would give.
        if gpu {
            storage.set_setting("asr.use_gpu", "true").unwrap();
            storage
                .set_setting(
                    "asr.gpu_bench",
                    r#"{"verdict":"gpu","reason":"forced by pipeline_e2e","gpu_secs":null,"cpu_secs":null,"model_id":"ggml-small-q5_1"}"#,
                )
                .unwrap();
        } else {
            storage.set_setting("asr.use_gpu", "false").unwrap();
        }
        let note = storage.create_note("Golden fixture", None).unwrap();
        let meeting = storage
            .create_meeting("Golden fixture", &note.id, &[])
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
                    duration_ms: 27_540,
                },
            )
            .unwrap();
        (meeting.id, fixture.exists())
    };
    assert!(fixture_ok, "fixture wav missing from repo");

    let on_stage = |p: pipeline::PipelineProgress| eprintln!("stage: {}", p.stage);
    let on_model = |p: looma_app_lib::models::ModelProgress| eprintln!("model: {}", p.stage);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let transcript = runtime
        .block_on(pipeline::run_with(
            &state,
            &on_stage,
            &on_model,
            &meeting_id,
        ))
        .expect("pipeline should succeed");

    // --- the requested engine actually ran (no silent CPU fallback) ---
    let expected_engine = if gpu {
        "whisper.cpp-vulkan"
    } else {
        "whisper.cpp"
    };
    assert_eq!(
        transcript.engine, expected_engine,
        "expected the {expected_engine} engine to produce this transcript"
    );

    // --- accuracy: WER within tolerance on clean TTS audio ---
    let hypothesis: String = transcript
        .segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let wer_val = wer(REFERENCE, &hypothesis);
    eprintln!("WER = {wer_val:.3}");
    eprintln!("hypothesis: {hypothesis}");
    assert!(
        wer_val < 0.25,
        "WER {wer_val:.3} exceeded tolerance; hypothesis: {hypothesis}"
    );

    // --- diarization: exactly two speakers, correctly attributed ---
    let speaker_keys: std::collections::HashSet<_> = transcript
        .segments
        .iter()
        .map(|s| s.speaker_key.clone())
        .filter(|k| k != "spk_unknown")
        .collect();
    assert_eq!(
        speaker_keys.len(),
        2,
        "expected 2 speakers, got {speaker_keys:?}"
    );

    let speaker_of = |needle: &str| -> Option<String> {
        transcript
            .segments
            .iter()
            .find(|s| s.text.to_lowercase().contains(needle))
            .map(|s| s.speaker_key.clone())
    };
    let david = speaker_of("quarterly budget").expect("David's opening line missing");
    let zira = speaker_of("engineering budget").expect("Zira's reply missing");
    assert_ne!(
        david, zira,
        "the two scripted voices must map to different speakers"
    );

    // --- persistence: markdown + json mirrors exist and are searchable ---
    {
        let storage = state.storage.lock().unwrap();
        let loaded = storage.get_transcript(&meeting_id).unwrap().unwrap();
        assert_eq!(loaded.segments.len(), transcript.segments.len());
        let hits = storage.search("forecast", 10).unwrap();
        assert!(!hits.is_empty(), "transcript should be searchable");
    }
    // mirrors live inside the meeting's folder; the 16 kHz intermediate is
    // cleaned up after success (only the real recording remains)
    let rec_dir = data_dir.join("recordings").join(&meeting_id);
    assert!(rec_dir.join("transcript.md").exists());
    assert!(rec_dir.join("transcript.json").exists());
    assert!(rec_dir.join("recording.mixed.wav").exists());
    assert!(
        !rec_dir.join("track.16k.wav").exists(),
        "16k intermediate should be removed after a successful run"
    );
}

/// Forced-fallback: the GPU verdict says "gpu" but the Vulkan exe is broken
/// (planted garbage). The pipeline must complete on the CPU engine, label the
/// transcript accordingly, and re-pin this "machine" to CPU so the failure
/// is not retried every meeting.
#[test]
#[ignore = "needs whisper/sherpa artifacts in %APPDATA%/Looma; run with --ignored"]
#[cfg(target_os = "windows")]
fn gpu_failure_falls_back_to_cpu() {
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
    // Plant a "Vulkan build" whose exe can't run: the registry probe exists,
    // so gpu::plan resolves it, and the launch fails at transcribe time.
    std::fs::create_dir_all(data_dir.join("bin/whisper-vulkan/Release")).unwrap();
    std::fs::write(
        data_dir.join("bin/whisper-vulkan/Release/whisper-cli.exe"),
        b"not an executable",
    )
    .unwrap();

    let state = AppState::init_with(
        data_dir.clone(),
        std::sync::Arc::new(looma_secrets::MemorySecretStore::default()),
    )
    .unwrap();

    let meeting_id = {
        let storage = state.storage.lock().unwrap();
        storage.set_setting("asr.tier", "light").unwrap();
        storage
            .set_setting("asr.model_id", "ggml-small-q5_1")
            .unwrap();
        storage.set_setting("asr.use_gpu", "true").unwrap();
        storage
            .set_setting(
                "asr.gpu_bench",
                r#"{"verdict":"gpu","reason":"forced by gpu_failure_falls_back_to_cpu","gpu_secs":null,"cpu_secs":null,"model_id":"ggml-small-q5_1"}"#,
            )
            .unwrap();
        let note = storage.create_note("Fallback test", None).unwrap();
        let meeting = storage
            .create_meeting("Fallback test", &note.id, &[])
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
                    duration_ms: 27_540,
                },
            )
            .unwrap();
        meeting.id
    };

    let on_stage = |p: pipeline::PipelineProgress| {
        eprintln!("stage: {} {}", p.stage, p.detail.unwrap_or_default())
    };
    let on_model = |p: looma_app_lib::models::ModelProgress| eprintln!("model: {}", p.stage);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let transcript = runtime
        .block_on(pipeline::run_with(
            &state,
            &on_stage,
            &on_model,
            &meeting_id,
        ))
        .expect("pipeline must survive a broken GPU build via CPU fallback");

    assert_eq!(
        transcript.engine, "whisper.cpp",
        "fallback transcript must be labeled with the CPU engine"
    );
    assert!(
        !transcript.segments.is_empty(),
        "fallback transcript should contain real segments"
    );

    // the machine is re-pinned to CPU for future meetings
    let bench = {
        let storage = state.storage.lock().unwrap();
        storage.get_setting("asr.gpu_bench").unwrap().unwrap()
    };
    let bench: serde_json::Value = serde_json::from_str(&bench).unwrap();
    assert_eq!(bench["verdict"], "cpu", "verdict should re-pin to cpu");
    assert!(
        bench["reason"]
            .as_str()
            .unwrap()
            .starts_with("runtime-failure"),
        "reason should record the runtime failure, got {}",
        bench["reason"]
    );
}
