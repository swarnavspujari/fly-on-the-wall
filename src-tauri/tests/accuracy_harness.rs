//! Offline accuracy harness for real recordings: runs the REAL per-channel
//! pipeline (whisper.cpp + sherpa-onnx) over a meeting's WAVs and reports the
//! trust metrics — consecutive-repetition runs (hallucination loops), distinct
//! speaker count, and word counts. `#[ignore]`d: heavy, needs artifacts and a
//! recording on disk.
//!
//! Score an already-exported transcript JSON (no pipeline run):
//!   LOOMA_HARNESS_SCORE_JSON=path\to\meeting.json \
//!     cargo test -p looma-app --test accuracy_harness -- --ignored --nocapture
//!
//! Run the pipeline over a recording folder (recording.mic.wav +
//! recording.system.wav), optionally trimmed for fast iteration:
//!   LOOMA_HARNESS_DIR=path\to\recording-folder \
//!   LOOMA_HARNESS_MODEL=ggml-large-v3-turbo-q5_0 \
//!   LOOMA_HARNESS_MAX_SECS=300 \
//!     cargo test -p looma-app --test accuracy_harness -- --ignored --nocapture

use std::path::Path;

use looma_core::repeat::loop_token;
use looma_core::{RecordingRef, Transcript};

/// One reportable repetition run: `reps` consecutive occurrences of an
/// `n`-word phrase starting at `start_ms`.
#[derive(Debug, serde::Serialize)]
struct RunReport {
    n: usize,
    reps: usize,
    phrase: String,
    start_ms: u64,
}

fn channel_words(t: &Transcript, mic: bool) -> Vec<(String, u64)> {
    let mut words: Vec<(String, u64)> = t
        .segments
        .iter()
        .filter(|s| (s.speaker_key == "mic") == mic)
        .flat_map(|s| s.words.iter().map(|w| (loop_token(&w.text), w.start_ms)))
        .collect();
    words.sort_by_key(|(_, at)| *at);
    words
}

/// Worst consecutive run per n-gram size (only n with 3+ reps are reported).
fn worst_runs(words: &[(String, u64)]) -> Vec<RunReport> {
    let tokens: Vec<&String> = words.iter().map(|(t, _)| t).collect();
    let mut out = Vec::new();
    for n in 1..=10usize {
        let (mut best, mut best_i) = (1usize, 0usize);
        let mut i = 0;
        while i + n <= tokens.len() {
            let mut reps = 1;
            let mut j = i + n;
            while j + n <= tokens.len() && tokens[j..j + n] == tokens[i..i + n] {
                reps += 1;
                j += n;
            }
            if reps > best {
                (best, best_i) = (reps, i);
            }
            i += if reps > 1 { (reps - 1) * n } else { 1 };
        }
        if best >= 3 {
            out.push(RunReport {
                n,
                reps: best,
                phrase: tokens[best_i..best_i + n]
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                start_ms: words[best_i].1,
            });
        }
    }
    out
}

fn mmss(ms: u64) -> String {
    format!("{:02}:{:02}", ms / 60_000, ms % 60_000 / 1000)
}

fn report(t: &Transcript) {
    let keys: std::collections::BTreeSet<&str> =
        t.segments.iter().map(|s| s.speaker_key.as_str()).collect();
    let non_unknown = keys
        .iter()
        .filter(|k| **k != "spk_unknown" && **k != "mic")
        .count();
    let mic = channel_words(t, true);
    let system = channel_words(t, false);

    eprintln!("== transcript metrics ==");
    eprintln!(
        "segments={} speakers_listed={} speaker_keys={} system_speakers(non-mic, non-unknown)={}",
        t.segments.len(),
        t.speakers.len(),
        keys.len(),
        non_unknown
    );
    eprintln!(
        "words: total={} mic={} system={}",
        mic.len() + system.len(),
        mic.len(),
        system.len()
    );
    let mut worst = serde_json::Map::new();
    for (label, words) in [("mic", &mic), ("system", &system)] {
        let runs = worst_runs(words);
        eprintln!("worst consecutive runs ({label}):");
        if runs.is_empty() {
            eprintln!("  none with 3+ reps");
        }
        for r in &runs {
            eprintln!(
                "  n={} x{} [{}] '{}'",
                r.n,
                r.reps,
                mmss(r.start_ms),
                &r.phrase[..r.phrase.len().min(60)]
            );
        }
        let max_reps = runs.iter().map(|r| r.reps).max().unwrap_or(1);
        worst.insert(format!("worst_reps_{label}"), serde_json::json!(max_reps));
    }

    let metrics = serde_json::json!({
        "segments": t.segments.len(),
        "speakers_listed": t.speakers.len(),
        "speaker_keys": keys.len(),
        "system_speakers": non_unknown,
        "words_total": mic.len() + system.len(),
        "words_mic": mic.len(),
        "words_system": system.len(),
        "worst_reps_mic": worst["worst_reps_mic"],
        "worst_reps_system": worst["worst_reps_system"],
    });
    eprintln!("HARNESS_METRICS_JSON: {metrics}");
}

/// Recursively hardlink a directory tree (same-volume, instant, no copies).
fn link_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
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

/// Copy a recording channel into the meeting dir, trimmed to `max_secs`.
fn stage_channel(src: &Path, dst: &Path, max_secs: Option<u64>) -> Result<u64, String> {
    match max_secs {
        None => {
            std::fs::copy(src, dst).map_err(|e| e.to_string())?;
            let (samples, rate) =
                looma_audio::mix::read_wav_mono(dst).map_err(|e| e.to_string())?;
            Ok(samples.len() as u64 * 1000 / rate as u64)
        }
        Some(secs) => {
            let (samples, rate) =
                looma_audio::mix::read_wav_mono(src).map_err(|e| e.to_string())?;
            let take = (secs * rate as u64).min(samples.len() as u64) as usize;
            looma_audio::mix::write_wav_mono_16(dst, &samples[..take], rate)
                .map_err(|e| e.to_string())?;
            Ok(take as u64 * 1000 / rate as u64)
        }
    }
}

#[test]
#[ignore = "offline accuracy harness; needs artifacts + a recording, see file docs"]
fn accuracy_harness() {
    // surface pipeline logs (e.g. collapsed-loop warnings) on stderr
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "looma_app_lib=debug,looma_asr=debug".into()),
        )
        .try_init();

    // ---- score-only mode: metrics for an exported transcript JSON ----
    if let Ok(json_path) = std::env::var("LOOMA_HARNESS_SCORE_JSON") {
        let raw = std::fs::read_to_string(&json_path).expect("read score json");
        let t: Transcript = serde_json::from_str(&raw).expect("parse transcript json");
        report(&t);
        return;
    }

    let Ok(rec_dir) = std::env::var("LOOMA_HARNESS_DIR") else {
        eprintln!("SKIP: set LOOMA_HARNESS_DIR or LOOMA_HARNESS_SCORE_JSON");
        return;
    };
    let rec_dir = std::path::PathBuf::from(rec_dir);
    let mic_src = rec_dir.join("recording.mic.wav");
    let sys_src = rec_dir.join("recording.system.wav");
    assert!(mic_src.exists(), "missing {}", mic_src.display());
    assert!(sys_src.exists(), "missing {}", sys_src.display());

    let model =
        std::env::var("LOOMA_HARNESS_MODEL").unwrap_or_else(|_| "ggml-large-v3-turbo-q5_0".into());
    let max_secs = std::env::var("LOOMA_HARNESS_MAX_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());

    // ---- artifacts, hardlinked from the real data dir like the golden E2E ----
    let real_data = dirs::data_dir().unwrap().join("Looma");
    let needed = [
        "bin/whisper/Release/whisper-cli.exe".to_string(),
        "bin/sherpa/sherpa-onnx-v1.13.3-win-x64-shared-MD-Release/bin/sherpa-onnx-offline-speaker-diarization.exe".to_string(),
        "models/diarize/sherpa-onnx-pyannote-segmentation-3-0/model.onnx".to_string(),
        "models/diarize/campplus.onnx".to_string(),
        format!("models/asr/{model}.bin"),
    ];
    if let Some(missing) = needed.iter().find(|p| !real_data.join(p).exists()) {
        panic!(
            "artifact not installed: {}",
            real_data.join(missing).display()
        );
    }

    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    for sub in ["bin/whisper", "bin/sherpa", "models/diarize"] {
        link_tree(&real_data.join(sub), &data_dir.join(sub)).unwrap();
    }
    // GPU modes need the Vulkan build in place (no download inside the test)
    if std::env::var("LOOMA_HARNESS_GPU").is_ok_and(|v| !v.is_empty() && v != "0") {
        let vulkan = real_data.join("bin/whisper-vulkan");
        assert!(
            vulkan.join("Release/whisper-cli.exe").exists(),
            "LOOMA_HARNESS_GPU set but whisper-bin-vulkan is not installed"
        );
        link_tree(&vulkan, &data_dir.join("bin/whisper-vulkan")).unwrap();
    }
    std::fs::create_dir_all(data_dir.join("models/asr")).unwrap();
    std::fs::hard_link(
        real_data.join(format!("models/asr/{model}.bin")),
        data_dir.join(format!("models/asr/{model}.bin")),
    )
    .unwrap();

    let state = looma_app_lib::state::AppState::init_with(
        data_dir.clone(),
        std::sync::Arc::new(looma_secrets::MemorySecretStore::default()),
    )
    .unwrap();

    let meeting_id = {
        let storage = state.storage.lock().unwrap();
        storage.set_setting("asr.tier", "light").unwrap();
        storage.set_setting("asr.model_id", &model).unwrap();
        // Deterministic engine selection: CPU by default. LOOMA_HARNESS_GPU=1
        // forces the Vulkan build (verdict pre-seeded so no benchmark runs;
        // pick the GPU with GGML_VK_VISIBLE_DEVICES). LOOMA_HARNESS_GPU=bench
        // enables GPU with NO verdict, exercising the real in-pipeline
        // benchmark + gate exactly as a user's machine would.
        match std::env::var("LOOMA_HARNESS_GPU").ok().as_deref() {
            Some("bench") => {
                storage.set_setting("asr.use_gpu", "true").unwrap();
            }
            Some(v) if !v.is_empty() && v != "0" => {
                storage.set_setting("asr.use_gpu", "true").unwrap();
                storage
                    .set_setting(
                        "asr.gpu_bench",
                        &format!(
                            r#"{{"verdict":"gpu","reason":"forced by accuracy_harness","gpu_secs":null,"cpu_secs":null,"model_id":"{model}"}}"#
                        ),
                    )
                    .unwrap();
            }
            _ => {
                storage.set_setting("asr.use_gpu", "false").unwrap();
            }
        }
        let note = storage.create_note("Accuracy harness", None).unwrap();
        let meeting = storage
            .create_meeting("Accuracy harness", &note.id, &[])
            .unwrap();
        let meet_dir = data_dir.join("recordings").join(&meeting.id);
        std::fs::create_dir_all(&meet_dir).unwrap();
        let dur_mic =
            stage_channel(&mic_src, &meet_dir.join("recording.mic.wav"), max_secs).unwrap();
        let dur_sys =
            stage_channel(&sys_src, &meet_dir.join("recording.system.wav"), max_secs).unwrap();
        storage
            .end_meeting(
                &meeting.id,
                &RecordingRef {
                    mic_path: Some(format!("recordings/{}/recording.mic.wav", meeting.id)),
                    system_path: Some(format!("recordings/{}/recording.system.wav", meeting.id)),
                    mixed_path: None,
                    duration_ms: dur_mic.max(dur_sys),
                },
            )
            .unwrap();
        meeting.id
    };

    let started = std::time::Instant::now();
    let on_stage = |p: looma_app_lib::pipeline::PipelineProgress| {
        eprintln!(
            "[{:>6.1}s] stage: {} {}",
            started.elapsed().as_secs_f32(),
            p.stage,
            p.detail.unwrap_or_default()
        )
    };
    let on_model = |p: looma_app_lib::models::ModelProgress| eprintln!("model: {}", p.stage);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let transcript = runtime
        .block_on(looma_app_lib::pipeline::run_with(
            &state,
            &on_stage,
            &on_model,
            &meeting_id,
        ))
        .expect("pipeline should succeed");
    eprintln!("pipeline took {:.1}s", started.elapsed().as_secs_f32());

    // keep the produced transcript for spot-checking against the audio
    if let Ok(out) = std::env::var("LOOMA_HARNESS_OUT_JSON") {
        std::fs::write(&out, serde_json::to_string_pretty(&transcript).unwrap()).unwrap();
        eprintln!("transcript written to {out}");
    }

    report(&transcript);
}
