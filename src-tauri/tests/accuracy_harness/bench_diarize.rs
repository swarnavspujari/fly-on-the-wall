//! Diarizer comparison over the corpus (v2.5 diarizer choice). Words are
//! held fixed — taken from one kept ASR transcript per item — so the only
//! thing that changes between arms is who-spoke-when:
//!
//!   FLYONTHEWALL_HARNESS_DIARIZE_CORPUS=path\to\corpus.json \
//!   FLYONTHEWALL_HARNESS_DIARIZERS=sherpa,nemotron3,nemotron3-30s,sortformer \
//!   FLYONTHEWALL_HARNESS_BASE_VARIANT=parakeet-cpu      # whose kept words
//!   FLYONTHEWALL_HARNESS_BENCH_DIR=path\outside\repo    # NeMo-Speech.cpp + GGUFs
//!   FLYONTHEWALL_HARNESS_GPU=1                          # Vulkan for NeMo-Speech.cpp
//!     cargo test -p fly-app --release --test accuracy_harness -- --ignored --nocapture
//!
//! Arms:
//! - `sherpa`         the shipped engine: sherpa-onnx segmentation + CAM++ +
//!   VBx refinement + dust filter (exactly what `pipeline::run_with` does).
//! - `nemotron3`      NVIDIA Nemotron-3-Diarization q8_0 via NeMo-Speech.cpp
//!   `diarize`, its default streaming geometry (1.12 s chunks).
//! - `nemotron3-30s`  the same model with the model card's 30.4 s
//!   "offline" latency config (the AMI-SDM 11.14 % row).
//! - `sortformer`     diar_streaming_sortformer_4spk-v2 (previous gen).
//!
//! Every arm is scored twice against the reference: the raw turns
//! (`diarization_turns`: clustering quality alone) and the word-attributed
//! transcript (`reference`: SA-WER, attribution error and DER of what the
//! user would see). Results land in `<out_dir>/diarization/`.

use std::path::{Path, PathBuf};

use fly_core::{SpeakerTurn, Transcript};

use crate::bench_engines::BenchDir;
use crate::run_stats;

const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

/// One diarizer arm: turns for a 16 kHz mono wav.
fn diarize(arm: &str, wav: &Path, gpu: bool) -> Result<Vec<SpeakerTurn>, String> {
    match arm {
        "sherpa" => {
            let real_data = dirs::data_dir().unwrap().join("FlyOnTheWall");
            let paths = crate::diarizer_paths(&real_data).ok_or("diarizer artifacts missing")?;
            let engine = fly_diarize::sherpa::SherpaDiarizeEngine {
                exe: paths.exe,
                segmentation_model: paths.seg,
                embedding_model: paths.emb,
                threads: fly_app_lib::pipeline::sidecar_threads(),
                refine: Some(fly_diarize::vbx::VbxParams::default()),
            };
            use fly_diarize::DiarizationEngine as _;
            let rt = tokio::runtime::Runtime::new().unwrap();
            let turns = rt
                .block_on(engine.diarize(wav, &fly_diarize::DiarizeOptions::default()))
                .map_err(|e| e.to_string())?;
            Ok(fly_diarize::drop_dust_clusters(turns))
        }
        "nemotron3" | "nemotron3-30s" | "sortformer" => {
            let dir = BenchDir::from_env()?;
            let device = if gpu { "vulkan" } else { "cpu" };
            let exe = dir
                .root
                .join(format!("bin/nemo-speech-{device}/bin/nemo-speech{EXE}"));
            let gguf = if arm == "sortformer" {
                "diar_streaming_sortformer_4spk-v2.q8_0.gguf"
            } else {
                "Nemotron-3-Diarization.q8_0.gguf"
            };
            let model = dir.root.join("models/nemo-speech").join(gguf);
            for p in [&exe, &model] {
                if !p.exists() {
                    return Err(format!("bench artifact missing: {}", p.display()));
                }
            }
            let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
            let out = tmp.path().join("diar.json");
            let mut cmd = std::process::Command::new(&exe);
            cmd.arg("diarize")
                .arg(wav)
                .arg("--format")
                .arg("json")
                .arg("-o")
                .arg(&out)
                .arg("--device")
                .arg(device)
                .arg("--quiet");
            if arm == "nemotron3-30s" {
                // model card "30.4 s" config: chunk 340, rc 40, fifo 40,
                // spkcache 264, update period 300 (80 ms frames)
                let cfg = tmp.path().join("diar.yaml");
                std::fs::write(
                    &cfg,
                    format!(
                        "diar:\n  model_path: \"{}\"\n  chunk: 340\n  right_context: 40\n  left_context: 0\n  fifo: 40\n  spkcache: 264\n  update_period: 300\n",
                        model.to_string_lossy().replace('\\', "/")
                    ),
                )
                .map_err(|e| e.to_string())?;
                cmd.arg("--config").arg(&cfg);
            } else {
                cmd.arg("--model").arg(&model);
            }
            if let Some(d) = exe.parent() {
                cmd.current_dir(d);
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt as _;
                // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS, as the
                // shipped diarizer sidecar
                cmd.creation_flags(0x0800_0000 | 0x0000_4000);
            }
            let o = cmd
                .output()
                .map_err(|e| format!("launch nemo-speech: {e}"))?;
            if !o.status.success() {
                return Err(format!(
                    "nemo-speech diarize failed ({}): {}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr)
                        .lines()
                        .rev()
                        .take(6)
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
            }
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&out).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            Ok(nemo_segments(&v))
        }
        other => Err(format!("unknown diarizer {other}")),
    }
}

/// NeMo-Speech.cpp diarize JSON → turns (1-based speaker → spk_<n-1>).
pub fn nemo_segments(v: &serde_json::Value) -> Vec<SpeakerTurn> {
    let mut turns: Vec<SpeakerTurn> = v["segments"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let start = s["start"].as_f64()?;
                    let end = s["end"].as_f64()?;
                    let spk = s["speaker"].as_u64()?;
                    (end > start).then(|| SpeakerTurn {
                        speaker_key: format!("spk_{}", spk.saturating_sub(1)),
                        start_ms: (start * 1000.0).round() as u64,
                        end_ms: (end * 1000.0).round() as u64,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    turns.sort_by_key(|t| (t.start_ms, t.end_ms));
    turns
}

fn mixed_wav_16k(dir: &Path, tmp: &Path) -> Result<PathBuf, String> {
    let src = dir.join("recording.mixed.wav");
    let (samples, rate) = fly_audio::mix::read_wav_mono(&src).map_err(|e| e.to_string())?;
    if rate == 16_000 {
        return Ok(src);
    }
    let out = tmp.join("mixed.16k.wav");
    let s16 = fly_audio::mix::resample_linear(&samples, rate, 16_000);
    fly_audio::mix::write_wav_mono_16(&out, &s16, 16_000).map_err(|e| e.to_string())?;
    Ok(out)
}

pub fn run_diarize_corpus(manifest: &str) {
    let (out_dir, items) = crate::load_corpus(manifest);
    let arms: Vec<String> = std::env::var("FLYONTHEWALL_HARNESS_DIARIZERS")
        .unwrap_or_else(|_| "sherpa,nemotron3,nemotron3-30s,sortformer".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let base_variant = std::env::var("FLYONTHEWALL_HARNESS_BASE_VARIANT")
        .unwrap_or_else(|_| "parakeet-cpu".into());
    let gpu = std::env::var("FLYONTHEWALL_HARNESS_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    let only: Option<Vec<String>> = std::env::var("FLYONTHEWALL_HARNESS_ONLY")
        .ok()
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect());
    let res_dir = out_dir.join("diarization");
    std::fs::create_dir_all(&res_dir).unwrap();

    for item in &items {
        if item.skip || only.as_ref().is_some_and(|o| !o.contains(&item.name)) {
            continue;
        }
        let Some(ref_path) = &item.reference else {
            continue;
        };
        let reference = crate::parse_reference(ref_path);
        let kept = out_dir
            .join("transcripts")
            .join(format!("{}.{base_variant}.transcript.json", item.name));
        let Some(base) = std::fs::read_to_string(&kept)
            .ok()
            .and_then(|raw| serde_json::from_str::<Transcript>(&raw).ok())
        else {
            eprintln!("SKIP {}: no kept transcript {}", item.name, kept.display());
            continue;
        };
        let mut words: Vec<fly_core::Word> = base
            .segments
            .iter()
            .flat_map(|s| s.words.iter().cloned())
            .collect();
        words.sort_by_key(|w| (w.start_ms, w.end_ms));
        let tmp = tempfile::tempdir().unwrap();
        let wav = match mixed_wav_16k(Path::new(&item.dir), tmp.path()) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("SKIP {}: {e}", item.name);
                continue;
            }
        };
        let audio_secs = fly_audio::mix::read_wav_mono(&wav)
            .map(|(s, r)| s.len() as f64 / r as f64)
            .unwrap_or(f64::NAN);

        for arm in &arms {
            let nemo = arm != "sherpa";
            let variant = format!("{arm}-{}", if gpu && nemo { "vulkan" } else { "cpu" });
            eprintln!("\n===== {} × diarizer {variant} =====", item.name);
            let sampler = run_stats::Sampler::start();
            let started = std::time::Instant::now();
            let turns = match diarize(arm, &wav, gpu && nemo) {
                Ok(t) => t,
                Err(e) => {
                    let _ = sampler.finish();
                    eprintln!("SKIP {variant}: {e}");
                    continue;
                }
            };
            let secs = started.elapsed().as_secs_f64();
            let resources = sampler.finish();

            let segments = fly_core::align::align_words_to_speakers(
                &words,
                &turns,
                &fly_core::AlignOptions::default(),
            );
            let mut speakers: Vec<fly_core::Speaker> = Vec::new();
            for s in &segments {
                if !speakers.iter().any(|sp| sp.key == s.speaker_key) {
                    speakers.push(fly_core::Speaker {
                        key: s.speaker_key.clone(),
                        label: s.speaker_key.clone(),
                    });
                }
            }
            let t = Transcript {
                meeting_id: item.name.clone(),
                language: base.language.clone(),
                engine: format!("{}+{variant}", base.engine),
                segments,
                speakers,
            };
            let mut all = serde_json::Map::new();
            all.insert("fixture".into(), serde_json::json!(item.name));
            all.insert("kind".into(), serde_json::json!(item.kind));
            all.insert(
                "config".into(),
                serde_json::json!({
                    "mode": "diarize-corpus",
                    "diarizer": arm,
                    "variant": variant,
                    "gpu": gpu && nemo,
                    "words_from": base_variant,
                    "words_engine": base.engine,
                }),
            );
            all.insert("audio_secs".into(), serde_json::json!(audio_secs));
            all.insert("diarize_secs".into(), serde_json::json!(secs));
            all.insert(
                "speed_x_realtime".into(),
                serde_json::json!(audio_secs / secs.max(f64::EPSILON)),
            );
            all.insert(
                "resources".into(),
                serde_json::to_value(&resources).unwrap(),
            );
            all.insert(
                "reference".into(),
                crate::score_with_reference(&t, &reference, &variant, None),
            );
            if let Some(d) = crate::score_diarization(&reference, &turns, "turns") {
                all.insert("diarization_turns".into(), d);
            }
            let path = res_dir.join(format!("{}.{variant}.json", item.name));
            std::fs::write(
                &path,
                serde_json::to_string_pretty(&serde_json::Value::Object(all)).unwrap() + "\n",
            )
            .unwrap();
            eprintln!("diarizer {variant}: {secs:.1}s → {}", path.display());
        }
    }
    summarize(&res_dir);
}

/// Table over every diarizer result in `dir` (also via compare mode on it).
pub fn summarize(dir: &Path) {
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json")
            && p.file_name().is_some_and(|n| n != "summary.json")
        {
            if let Some(v) = std::fs::read_to_string(&p)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            {
                rows.push(v);
            }
        }
    }
    let key = |v: &serde_json::Value, p: &str| v.pointer(p).cloned().unwrap_or_default();
    rows.sort_by_key(|v| {
        (
            key(v, "/fixture").as_str().unwrap_or_default().to_string(),
            key(v, "/config/variant")
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
    });
    let pct = |v: &serde_json::Value, p: &str| {
        v.pointer(p)
            .and_then(|x| x.as_f64())
            .map(|x| format!("{:.1}", x * 100.0))
            .unwrap_or("-".into())
    };
    eprintln!("\n== Diarizer comparison ({}) ==", dir.display());
    eprintln!(
        "{:<20} {:<22} {:>8} {:>8} {:>8} {:>7} {:>6} {:>6} {:>7}",
        "fixture", "diarizer", "tDER250", "tDER0", "DER250", "SAWER%", "attr%", "clu/ref", "speed×"
    );
    let mut table = Vec::new();
    for v in &rows {
        let clusters = format!(
            "{}/{}",
            key(v, "/diarization_turns/hyp_clusters"),
            key(v, "/diarization_turns/ref_speakers")
        );
        eprintln!(
            "{:<20} {:<22} {:>8} {:>8} {:>8} {:>7} {:>6} {:>6} {:>7}",
            key(v, "/fixture").as_str().unwrap_or_default(),
            key(v, "/config/variant").as_str().unwrap_or_default(),
            pct(v, "/diarization_turns/der_collar_250/der"),
            pct(v, "/diarization_turns/der_collar_0/der"),
            pct(v, "/reference/diarization/der_collar_250/der"),
            pct(v, "/reference/sa_wer_merged"),
            pct(v, "/reference/attribution_error"),
            clusters,
            v.pointer("/speed_x_realtime")
                .and_then(|x| x.as_f64())
                .map(|x| format!("{x:.1}"))
                .unwrap_or("-".into()),
        );
        table.push(serde_json::json!({
            "fixture": key(v, "/fixture"),
            "variant": key(v, "/config/variant"),
            "turn_der_collar_250": key(v, "/diarization_turns/der_collar_250/der"),
            "turn_der_collar_0": key(v, "/diarization_turns/der_collar_0/der"),
            "der_collar_250": key(v, "/reference/diarization/der_collar_250/der"),
            "sa_wer": key(v, "/reference/sa_wer_merged"),
            "attribution_error": key(v, "/reference/attribution_error"),
            "hyp_clusters": key(v, "/diarization_turns/hyp_clusters"),
            "ref_speakers": key(v, "/diarization_turns/ref_speakers"),
            "speed_x_realtime": key(v, "/speed_x_realtime"),
            "machine_cpu_avg_pct": key(v, "/resources/machine_cpu_avg_pct"),
        }));
    }
    std::fs::write(
        dir.join("summary.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "rows": table })).unwrap() + "\n",
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    #[test]
    fn nemo_segments_are_zero_based_and_sorted() {
        let v = serde_json::json!({"segments": [
            {"start": 3.5, "end": 4.0, "speaker": 2},
            {"start": 0.25, "end": 1.5, "speaker": 1},
            {"start": 2.0, "end": 2.0, "speaker": 1}
        ]});
        let t = super::nemo_segments(&v);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].speaker_key, "spk_0");
        assert_eq!((t[0].start_ms, t[0].end_ms), (250, 1_500));
        assert_eq!(t[1].speaker_key, "spk_1");
    }
}
