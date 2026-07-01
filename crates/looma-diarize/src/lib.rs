//! looma-diarize: the `DiarizationEngine` trait.
//!
//! Diarization ALWAYS runs locally (sherpa-onnx backend, landing in M3) — on
//! every hardware tier including the Groq cloud-ASR tier. sherpa-onnx is
//! light enough for phones, so "who said what" never depends on the network
//! (spec §6.3). The word↔speaker aligner itself lives in looma-core.

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
    /// Prefix for generated speaker keys ("spk" → "spk_0", "spk_1", …).
    pub speaker_key_prefix: String,
}

impl Default for DiarizeOptions {
    fn default() -> Self {
        Self {
            num_speakers: None,
            speaker_key_prefix: "spk".to_string(),
        }
    }
}

#[async_trait::async_trait]
pub trait DiarizationEngine: Send + Sync {
    fn id(&self) -> &'static str;
    async fn diarize(&self, wav_path: &Path, opts: &DiarizeOptions) -> Result<Vec<SpeakerTurn>>;
}
