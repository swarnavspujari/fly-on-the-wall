//! Recording storage v2 corpus check: the REAL pipeline (whisper.cpp +
//! sherpa-onnx, CPU) on the same meetings stored as WAV and after lossless
//! compression must produce identical transcripts (text, timings, words,
//! speakers) and identical re-diarization.
//!
//! Needs the same artifacts as pipeline_e2e (hardlinked, read-only, from
//! %APPDATA%/FlyOnTheWall), so it is `#[ignore]`d:
//!   cargo test -p fly-app --test compress_e2e -- --ignored --nocapture
//!
//! Corpus: three meetings built from the committed two-voice fixture
//! (two-channel at 48/44.1 kHz, mic-only, a media import). Extra meetings:
//! `FLYONTHEWALL_COMPRESS_E2E_DIRS` = `;`-separated folders holding
//! `recording.{mic,system}.wav` (use COPIES), cropped to
//! `FLYONTHEWALL_COMPRESS_E2E_SECS` (default 120) seconds. Nothing from them
//! is printed except pass/fail and segment counts.

use std::path::Path;
use std::sync::Arc;

use fly_app_lib::audio_store::{compress_meeting, is_storage_v2, MeetingCtx};
use fly_app_lib::pipeline;
use fly_app_lib::state::AppState;
use fly_audio::mix;
use fly_core::{RecordingRef, Transcript};

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

fn probe(id: &str) -> &'static str {
    fly_app_lib::models::artifact(id)
        .unwrap_or_else(|| panic!("no {id} artifact on this OS"))
        .probe_rel
}

/// A meeting's source files, as capture or import would leave them.
struct Source {
    name: String,
    mic: Option<Vec<f32>>,
    mic_rate: u32,
    system: Option<Vec<f32>>,
    system_rate: u32,
    /// Import: only a 16 kHz mixed track.
    import_mixed: Option<Vec<f32>>,
}

fn corpus() -> Vec<Source> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/meeting-fixture.wav");
    let (s, r) = mix::read_wav_mono(&fixture).unwrap();
    let half = s.len() / 2;
    let first: Vec<f32> = s
        .iter()
        .enumerate()
        .map(|(i, &v)| if i < half { v } else { 0.0 })
        .collect();
    let second: Vec<f32> = s
        .iter()
        .enumerate()
        .map(|(i, &v)| if i >= half { v } else { 0.0 })
        .collect();
    let mut out = vec![
        Source {
            name: "fixture two-channel".into(),
            mic: Some(mix::resample_linear(&first, r, 48_000)),
            mic_rate: 48_000,
            system: Some(mix::resample_linear(&second, r, 44_100)),
            system_rate: 44_100,
            import_mixed: None,
        },
        Source {
            name: "fixture mic-only".into(),
            mic: Some(mix::resample_linear(&s, r, 48_000)),
            mic_rate: 48_000,
            system: None,
            system_rate: 0,
            import_mixed: None,
        },
        Source {
            name: "fixture import".into(),
            mic: None,
            mic_rate: 0,
            system: None,
            system_rate: 0,
            import_mixed: Some(mix::resample_linear(&s, r, 16_000)),
        },
    ];
    let secs: f32 = std::env::var("FLYONTHEWALL_COMPRESS_E2E_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120.0);
    if let Ok(dirs) = std::env::var("FLYONTHEWALL_COMPRESS_E2E_DIRS") {
        for (i, d) in dirs.split(';').filter(|d| !d.is_empty()).enumerate() {
            let load = |n: &str| -> Option<(Vec<f32>, u32)> {
                let p = Path::new(d).join(n);
                p.exists().then(|| {
                    let (mut v, r) = mix::read_wav_mono(&p).unwrap();
                    v.truncate((secs * r as f32) as usize);
                    (v, r)
                })
            };
            let (mic, sys) = (load("recording.mic.wav"), load("recording.system.wav"));
            out.push(Source {
                name: format!("extra meeting {}", i + 1),
                mic_rate: mic.as_ref().map_or(0, |m| m.1),
                mic: mic.map(|m| m.0),
                system_rate: sys.as_ref().map_or(0, |s| s.1),
                system: sys.map(|s| s.0),
                import_mixed: None,
            });
        }
    }
    out
}

fn new_state(real_data: &Path) -> (tempfile::TempDir, AppState) {
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
        data_dir,
        Arc::new(fly_secrets::MemorySecretStore::default()),
    )
    .unwrap();
    {
        let s = state.storage.lock().unwrap();
        s.set_setting("asr.tier", "light").unwrap();
        s.set_setting("asr.model_id", "ggml-small-q5_1").unwrap();
        s.set_setting("asr.use_gpu", "false").unwrap(); // deterministic CPU
    }
    (tmp, state)
}

/// Write the source as capture would (WAVs + mixdown + playback mix) and
/// attach it to a new meeting.
fn add_meeting(state: &AppState, src: &Source) -> String {
    let storage = state.storage.lock().unwrap();
    let note = storage.create_note("Corpus meeting", None).unwrap();
    let meeting = storage
        .create_meeting("Corpus meeting", &note.id, &[])
        .unwrap();
    let folder = format!("recordings/{}", meeting.id);
    let dir = state.data_dir.join(&folder);
    std::fs::create_dir_all(&dir).unwrap();
    let rel = |n: &str| format!("{folder}/{n}");
    let mut rec = RecordingRef {
        mic_path: None,
        system_path: None,
        mixed_path: None,
        playback_path: None,
        duration_ms: 0,
    };
    if let Some(m) = &src.import_mixed {
        mix::write_wav_mono_16(&dir.join("recording.mixed.wav"), m, 16_000).unwrap();
        rec.mixed_path = Some(rel("recording.mixed.wav"));
    } else {
        let mic = dir.join("recording.mic.wav");
        let sys = dir.join("recording.system.wav");
        if let Some(m) = &src.mic {
            mix::write_wav_mono_16(&mic, m, src.mic_rate).unwrap();
            rec.mic_path = Some(rel("recording.mic.wav"));
        }
        if let Some(s) = &src.system {
            mix::write_wav_mono_16(&sys, s, src.system_rate).unwrap();
            rec.system_path = Some(rel("recording.system.wav"));
        }
        let (m, s) = (
            src.mic.as_ref().map(|_| mic.as_path()),
            src.system.as_ref().map(|_| sys.as_path()),
        );
        mix::write_mixdown(m, s, &dir.join("recording.mixed.wav")).unwrap();
        rec.mixed_path = Some(rel("recording.mixed.wav"));
        if m.is_some() && s.is_some() {
            mix::write_playback_mix(m, s, &dir.join("recording.playback.wav")).unwrap();
            rec.playback_path = Some(rel("recording.playback.wav"));
        } else {
            rec.playback_path = rec.mic_path.clone().or(rec.system_path.clone());
        }
    }
    storage.end_meeting(&meeting.id, &rec).unwrap();
    meeting.id
}

/// Everything about a transcript except the random segment ids.
fn content(t: &Transcript) -> serde_json::Value {
    let mut v = serde_json::to_value(t).unwrap();
    v["meeting_id"] = serde_json::Value::Null;
    for seg in v["segments"].as_array_mut().unwrap() {
        seg["id"] = serde_json::Value::Null;
    }
    v
}

#[test]
#[ignore = "needs whisper/sherpa artifacts in %APPDATA%/FlyOnTheWall; run with --ignored"]
fn transcripts_and_diarization_are_identical_from_flac_and_wav() {
    let real_data = dirs::data_dir().unwrap().join("FlyOnTheWall");
    let needed = [
        probe("whisper-bin"),
        probe("sherpa-bin"),
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
    let on_stage = |_: pipeline::PipelineProgress| {};
    let on_model = |_: fly_app_lib::models::ModelProgress| {};
    let rt = tokio::runtime::Runtime::new().unwrap();
    // FLYONTHEWALL_COMPRESS_E2E_ONLY=<substring of a corpus name> runs one case.
    let only = std::env::var("FLYONTHEWALL_COMPRESS_E2E_ONLY").unwrap_or_default();
    for src in corpus().into_iter().filter(|s| s.name.contains(&only)) {
        let (_a_dir, wav_state) = new_state(&real_data);
        let (_b_dir, flac_state) = new_state(&real_data);
        let wav_id = add_meeting(&wav_state, &src);
        let flac_id = add_meeting(&flac_state, &src);
        {
            let mut go = || true;
            let mut done = |_| {};
            let mut ctx = MeetingCtx {
                data_dir: &flac_state.data_dir,
                checkpoint: &mut go,
                may_commit: &|_| Ok(()),
                storage: &flac_state.storage,
                on_file_done: &mut done,
                skip_leftover: &|_| false,
                free_space: &fly_app_lib::audio_store::free_space,
            };
            compress_meeting(&mut ctx, &flac_id)
                .unwrap_or_else(|e| panic!("{}: compress failed: {e}", src.name));
        }
        let rec = flac_state
            .storage
            .lock()
            .unwrap()
            .get_meeting(&flac_id)
            .unwrap()
            .recording
            .unwrap();
        let v2 = is_storage_v2(&rec);
        let wav_t = rt
            .block_on(pipeline::run_with(
                &wav_state, &on_stage, &on_model, &wav_id,
            ))
            .unwrap();
        let flac_t = rt
            .block_on(pipeline::run_with(
                &flac_state,
                &on_stage,
                &on_model,
                &flac_id,
            ))
            .unwrap();
        assert_eq!(
            content(&flac_t),
            content(&wav_t),
            "{}: transcript differs",
            src.name
        );
        let wav_r = rt
            .block_on(pipeline::re_diarize_with(
                &wav_state, &on_stage, &on_model, &wav_id,
            ))
            .unwrap();
        let flac_r = rt
            .block_on(pipeline::re_diarize_with(
                &flac_state,
                &on_stage,
                &on_model,
                &flac_id,
            ))
            .unwrap();
        assert_eq!(
            flac_r.changed_segments, wav_r.changed_segments,
            "{}",
            src.name
        );
        assert_eq!(
            content(&flac_r.transcript),
            content(&wav_r.transcript),
            "{}: re-diarization differs",
            src.name
        );
        eprintln!(
            "{}: identical ({} segments, {} speakers; storage v2: {v2})",
            src.name,
            wav_t.segments.len(),
            wav_t.speakers.len()
        );
    }
}
