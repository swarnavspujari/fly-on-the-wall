//! sherpa-onnx sidecar diarization: pyannote segmentation + speaker
//! embedding + clustering, all on CPU, all local — on every tier (§6.3).

use std::path::{Path, PathBuf};

use fly_core::SpeakerTurn;

use crate::{DiarizationEngine, DiarizeError, DiarizeOptions, Result};

pub struct SherpaDiarizeEngine {
    /// Path to sherpa-onnx-offline-speaker-diarization(.exe).
    pub exe: PathBuf,
    /// pyannote segmentation model (model.onnx).
    pub segmentation_model: PathBuf,
    /// Speaker embedding model (CAM++ ONNX).
    pub embedding_model: PathBuf,
    pub threads: usize,
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

        let mut cmd = tokio::process::Command::new(&self.exe);
        cmd.args(self.cli_args(opts));
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
        Ok(parse_diarization_output(&stdout, &opts.speaker_key_prefix))
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
        }
    }

    #[test]
    fn default_options_pass_the_cluster_threshold() {
        let args = engine().cli_args(&DiarizeOptions::default());
        assert!(args
            .iter()
            .any(|a| a == "--clustering.cluster-threshold=0.9"));
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
