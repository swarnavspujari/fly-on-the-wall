//! looma-diarize: the `DiarizationEngine` trait.
//!
//! Diarization ALWAYS runs locally (sherpa-onnx backend, landing in M3) — on
//! every hardware tier including the Groq cloud-ASR tier. sherpa-onnx is
//! light enough for phones, so "who said what" never depends on the network
//! (spec §6.3). The word↔speaker aligner itself lives in looma-core.

pub mod sherpa;

use std::path::Path;

use looma_core::SpeakerTurn;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum DiarizeError {
    #[error("diarization model is not downloaded yet: {0}")]
    ModelMissing(String),
    #[error("audio file not found or unreadable: {0}")]
    BadAudio(String),
    #[error("diarization engine failed: {0}")]
    Engine(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DiarizeError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiarizeOptions {
    /// Known speaker count if the user provided one; `None` = auto.
    pub num_speakers: Option<usize>,
    /// Agglomerative clustering distance threshold when the speaker count is
    /// unknown: larger = fewer speakers. sherpa's own default (0.5) shattered
    /// a one-hour single-speaker channel into 75+ clusters; upstream docs
    /// recommend ~0.9 for unknown counts. `None` = engine default.
    pub cluster_threshold: Option<f32>,
    /// Prefix for generated speaker keys ("spk" → "spk_0", "spk_1", …).
    pub speaker_key_prefix: String,
}

impl Default for DiarizeOptions {
    fn default() -> Self {
        Self {
            num_speakers: None,
            cluster_threshold: Some(0.9),
            speaker_key_prefix: "spk".to_string(),
        }
    }
}

#[async_trait::async_trait]
pub trait DiarizationEngine: Send + Sync {
    fn id(&self) -> &'static str;
    async fn diarize(&self, wav_path: &Path, opts: &DiarizeOptions) -> Result<Vec<SpeakerTurn>>;
}

/// Drop turns belonging to "dust" clusters — speakers whose total time is
/// negligible. Hour-long single-speaker audio still shatters into a dozen
/// sub-10-second clusters even at a sane clustering threshold; discarding
/// their turns lets the word aligner fall back to the nearest real speaker
/// instead of inventing phantom ones. The floor scales down for short
/// recordings so brief-but-real speakers survive.
pub fn drop_dust_clusters(mut turns: Vec<SpeakerTurn>) -> Vec<SpeakerTurn> {
    /// A cluster totalling less than this is dust…
    const DUST_FLOOR_MS: u64 = 15_000;
    /// …unless the whole recording is short: the floor never exceeds this
    /// fraction of all attributed speech.
    const DUST_FRACTION: f64 = 0.05;

    let mut totals: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for t in &turns {
        *totals.entry(t.speaker_key.clone()).or_default() +=
            t.end_ms.saturating_sub(t.start_ms);
    }
    let all: u64 = totals.values().sum();
    let floor = DUST_FLOOR_MS.min((all as f64 * DUST_FRACTION) as u64);
    turns.retain(|t| totals[&t.speaker_key] >= floor);
    turns
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(key: &str, start_s: u64, end_s: u64) -> SpeakerTurn {
        SpeakerTurn {
            speaker_key: key.into(),
            start_ms: start_s * 1000,
            end_ms: end_s * 1000,
        }
    }

    #[test]
    fn dust_clusters_are_dropped_in_long_audio() {
        // one dominant hour-scale speaker, one real 4-minute speaker, dust
        let mut turns = vec![turn("spk_0", 0, 1500), turn("spk_1", 1500, 1740)];
        for i in 0..10 {
            let at = 1800 + i * 20;
            turns.push(turn(&format!("spk_{}", 2 + i), at, at + 8));
        }
        let kept = drop_dust_clusters(turns);
        let keys: std::collections::BTreeSet<_> =
            kept.iter().map(|t| t.speaker_key.as_str()).collect();
        assert_eq!(
            keys.into_iter().collect::<Vec<_>>(),
            vec!["spk_0", "spk_1"]
        );
    }

    #[test]
    fn short_recordings_keep_brief_real_speakers() {
        // fixture-scale: two ~13 s speakers must both survive
        let turns = vec![
            turn("spk_0", 0, 7),
            turn("spk_1", 7, 20),
            turn("spk_0", 20, 27),
        ];
        let kept = drop_dust_clusters(turns.clone());
        assert_eq!(kept, turns);
    }

    #[test]
    fn single_cluster_survives() {
        let turns = vec![turn("spk_0", 0, 5)];
        assert_eq!(drop_dust_clusters(turns.clone()), turns);
    }

    #[test]
    fn empty_input_is_fine() {
        assert!(drop_dust_clusters(Vec::new()).is_empty());
    }
}
