//! sherpa-onnx sidecar diarization: pyannote segmentation + speaker
//! embedding + agglomerative clustering, all on CPU, all local — on every
//! tier (§6.3). When the speaker count is unknown, the sidecar's clustering
//! is only the INITIAL pass: its (deliberately over-split) clusters are
//! refined in-process by VBx (crate::refine), which merges the shattered
//! clusters that agglomerative thresholds cannot fix.

use std::path::{Path, PathBuf};

use fly_core::SpeakerTurn;

use crate::vbx::VbxParams;
use crate::{DiarizationEngine, DiarizeError, DiarizeOptions, Result};

/// Initial-clustering cut used when VBx refinement follows (measured
/// operating point, docs/BENCHMARKS.md Phase 3).
const VBX_INIT_THRESHOLD: f32 = 0.8;

pub struct SherpaDiarizeEngine {
    /// Path to sherpa-onnx-offline-speaker-diarization(.exe).
    pub exe: PathBuf,
    /// pyannote segmentation model (model.onnx).
    pub segmentation_model: PathBuf,
    /// Speaker embedding model (CAM++ ONNX) — used by the sidecar AND by
    /// the in-process VBx refinement.
    pub embedding_model: PathBuf,
    pub threads: usize,
    /// VBx refinement of the sidecar's clustering (the shipped path is
    /// `Some(VbxParams::default())`). `None` = raw sidecar output — kept for
    /// harness A/B comparisons. A user-provided speaker count bypasses
    /// refinement either way: the count is forced, and VBx can only merge.
    pub refine: Option<VbxParams>,
}

#[async_trait::async_trait]
impl DiarizationEngine for SherpaDiarizeEngine {
    fn id(&self) -> &'static str {
        "sherpa-onnx"
    }

    async fn diarize(&self, wav_path: &Path, opts: &DiarizeOptions) -> Result<Vec<SpeakerTurn>> {
        for (what, p) in [
            ("segmentation model", &self.segmentation_model),
            ("embedding model", &self.embedding_model),
        ] {
            if !p.exists() {
                return Err(DiarizeError::ModelMissing(format!(
                    "{what}: {}",
                    p.display()
                )));
            }
        }
        if !wav_path.exists() {
            return Err(DiarizeError::BadAudio(wav_path.display().to_string()));
        }

        // With VBx refinement on (and no forced speaker count), the sidecar's
        // clustering is only the init — run it deliberately over-split: VBx
        // merges shattered clusters but cannot split merged ones, so too many
        // init clusters is the safe side (docs/BENCHMARKS.md, Phase 3).
        let mut init_opts = opts.clone();
        if self.refine.is_some() && opts.num_speakers.is_none() {
            init_opts.cluster_threshold = Some(VBX_INIT_THRESHOLD);
        }
        let mut cmd = tokio::process::Command::new(&self.exe);
        cmd.args(self.cli_args(&init_opts));
        cmd.arg(wav_path);
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS: diarization is
            // background work — recording and foreground apps win the CPU.
            cmd.creation_flags(0x0800_0000 | 0x0000_4000);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| DiarizeError::Engine(format!("failed to launch sherpa-onnx: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DiarizeError::Engine(format!(
                "sherpa-onnx exited with {}: {}",
                output.status,
                stderr.chars().take(500).collect::<String>()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let turns = parse_diarization_output(&stdout, &opts.speaker_key_prefix);

        // VBx refinement: only when the speaker count is unknown (a forced
        // count already fixes the cluster structure) and refinement is on.
        let (Some(params), None) = (&self.refine, opts.num_speakers) else {
            return Ok(turns);
        };
        if turns.is_empty() {
            return Ok(turns);
        }
        ensure_ort_dylib(&self.exe)?;
        let samples = crate::refine::read_wav_mono_16k(wav_path)
            .map_err(|e| DiarizeError::BadAudio(format!("{}: {e}", wav_path.display())))?;
        let (model, threads, params, prefix) = (
            self.embedding_model.clone(),
            self.threads.max(1),
            params.clone(),
            opts.speaker_key_prefix.clone(),
        );
        // CPU-bound minute of work — off the async runtime.
        let refined = tokio::task::spawn_blocking(move || {
            crate::refine::refine_with_vbx(&samples, &turns, &model, threads, &params).map(
                |refined| {
                    refined
                        .into_iter()
                        .map(|mut t| {
                            // refine emits spk_N; honor the caller's prefix
                            if let Some(idx) = t.speaker_key.strip_prefix("spk_") {
                                t.speaker_key = format!("{prefix}_{idx}");
                            }
                            t
                        })
                        .collect::<Vec<_>>()
                },
            )
        })
        .await
        .map_err(|e| DiarizeError::Engine(format!("vbx refinement task failed: {e}")))?
        .map_err(DiarizeError::Engine)?;
        Ok(refined)
    }
}

impl SherpaDiarizeEngine {
    /// All CLI arguments except the trailing wav path. A user-provided
    /// speaker count wins over the clustering threshold (sherpa ignores the
    /// threshold when num-clusters is set; we don't pass both).
    fn cli_args(&self, opts: &DiarizeOptions) -> Vec<String> {
        let mut args = vec![
            format!(
                "--segmentation.pyannote-model={}",
                self.segmentation_model.display()
            ),
            format!("--embedding.model={}", self.embedding_model.display()),
            format!("--segmentation.num-threads={}", self.threads.max(1)),
            format!("--embedding.num-threads={}", self.threads.max(1)),
        ];
        match (opts.num_speakers, opts.cluster_threshold) {
            (Some(n), _) => args.push(format!("--clustering.num-clusters={n}")),
            (None, Some(threshold)) => {
                args.push(format!("--clustering.cluster-threshold={threshold}"))
            }
            (None, None) => {}
        }
        args
    }
}

/// Linux only: `ort` is built with load-dynamic there (its static prebuilts
/// need glibc ≥ 2.38; releases build on ubuntu-22.04 = glibc 2.35, the users'
/// floor), so before the first in-process session it must dlopen the
/// libonnxruntime.so that ships INSIDE the sherpa sidecar bundle — the exact
/// library already running on the user's machine. Windows/macOS statically
/// link and this is a no-op.
#[cfg(target_os = "linux")]
fn ensure_ort_dylib(sherpa_exe: &Path) -> Result<()> {
    static INIT: std::sync::OnceLock<std::result::Result<(), String>> = std::sync::OnceLock::new();
    let result = INIT.get_or_init(|| {
        let exe_dir = sherpa_exe.parent().unwrap_or(Path::new("."));
        let candidates = [exe_dir.join("../lib"), exe_dir.to_path_buf()];
        for dir in candidates {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with("libonnxruntime.so") {
                    return ort::init_from(entry.path().to_string_lossy().as_ref())
                        .map(|builder| {
                            builder.commit();
                        })
                        .map_err(|e| e.to_string());
                }
            }
        }
        Err(format!(
            "libonnxruntime.so not found next to the sherpa sidecar ({})",
            sherpa_exe.display()
        ))
    });
    result
        .clone()
        .map_err(|e| DiarizeError::Engine(format!("onnxruntime dylib init failed: {e}")))
}

#[cfg(not(target_os = "linux"))]
fn ensure_ort_dylib(_sherpa_exe: &Path) -> Result<()> {
    Ok(())
}

/// Parse lines shaped `0.318 -- 6.865 speaker_00` (sherpa prints config and
/// progress around them; everything non-matching is ignored).
pub fn parse_diarization_output(output: &str, key_prefix: &str) -> Vec<SpeakerTurn> {
    let mut turns = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        let Some((times, speaker)) = line.rsplit_once(' ') else {
            continue;
        };
        let Some(num) = speaker.strip_prefix("speaker_") else {
            continue;
        };
        let Ok(idx) = num.parse::<u32>() else {
            continue;
        };
        let Some((start, end)) = times.trim().split_once("--") else {
            continue;
        };
        let (Ok(start_s), Ok(end_s)) = (start.trim().parse::<f64>(), end.trim().parse::<f64>())
        else {
            continue;
        };
        turns.push(SpeakerTurn {
            speaker_key: format!("{key_prefix}_{idx}"),
            start_ms: (start_s * 1000.0) as u64,
            end_ms: (end_s * 1000.0) as u64,
        });
    }
    turns.sort_by_key(|t| t.start_ms);
    turns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_turn_lines_ignoring_noise() {
        let out = "\
progress 100.00%
Duration : 27.540 s
OfflineSpeakerDiarizationConfig(...)
Started
0.031 -- 1.347 speaker_00
5.465 -- 6.342 speaker_01
2.174 -- 4.655 speaker_00
";
        let turns = parse_diarization_output(out, "spk");
        assert_eq!(turns.len(), 3);
        // sorted by start
        assert_eq!(turns[0].speaker_key, "spk_0");
        assert_eq!(turns[0].start_ms, 31);
        assert_eq!(turns[1].start_ms, 2174);
        assert_eq!(turns[2].speaker_key, "spk_1");
        assert_eq!(turns[2].end_ms, 6342);
    }

    #[test]
    fn empty_output_gives_no_turns() {
        assert!(parse_diarization_output("no matches here", "spk").is_empty());
    }

    fn engine() -> SherpaDiarizeEngine {
        SherpaDiarizeEngine {
            exe: "sherpa.exe".into(),
            segmentation_model: "seg.onnx".into(),
            embedding_model: "emb.onnx".into(),
            threads: 4,
            refine: None,
        }
    }

    #[test]
    fn default_options_pass_the_cluster_threshold() {
        let args = engine().cli_args(&DiarizeOptions::default());
        assert!(args
            .iter()
            .any(|a| a == "--clustering.cluster-threshold=0.95"));
        assert!(!args
            .iter()
            .any(|a| a.starts_with("--clustering.num-clusters")));
    }

    #[test]
    fn known_speaker_count_wins_over_threshold() {
        let opts = DiarizeOptions {
            num_speakers: Some(2),
            ..Default::default()
        };
        let args = engine().cli_args(&opts);
        assert!(args.iter().any(|a| a == "--clustering.num-clusters=2"));
        assert!(!args
            .iter()
            .any(|a| a.starts_with("--clustering.cluster-threshold")));
    }

    #[test]
    fn no_threshold_means_engine_default() {
        let opts = DiarizeOptions {
            cluster_threshold: None,
            ..Default::default()
        };
        let args = engine().cli_args(&opts);
        assert!(!args.iter().any(|a| a.contains("cluster-threshold")));
    }
}
