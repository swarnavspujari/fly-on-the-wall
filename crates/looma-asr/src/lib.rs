//! looma-asr: the `TranscriptionEngine` trait and its backends.
//!
//! Backends (landing in M3):
//! - whisper.cpp — primary, fully local, every hardware tier
//! - parakeet — optional local engine for NVIDIA/Apple hardware
//! - groq — cloud FALLBACK only (audio leaves the device; the UI must say
//!   so). Returns no speaker labels; diarization always runs locally
//!   regardless (spec §6.3).

use std::path::Path;

use looma_core::Word;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum AsrError {
    #[error("ASR model is not downloaded yet: {0}")]
    ModelMissing(String),
    #[error("audio file not found or unreadable: {0}")]
    BadAudio(String),
    #[error("transcription engine failed: {0}")]
    Engine(String),
    #[error("network error talking to cloud ASR: {0}")]
    Network(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, AsrError>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscribeOptions {
    /// BCP-47-ish language hint ("en", "de"); `None` = auto-detect.
    pub language: Option<String>,
    /// Initial prompt / vocabulary hint passed to the engine when supported.
    pub prompt: Option<String>,
}

/// Sentence/phrase-level chunk as emitted by the engine, before diarization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Engine output: word-level timestamps are required — they are what the
/// aligner merges with diarization turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawTranscript {
    pub language: Option<String>,
    pub words: Vec<Word>,
    pub segments: Vec<RawSegment>,
}

#[async_trait::async_trait]
pub trait TranscriptionEngine: Send + Sync {
    /// Stable id: "whisper.cpp", "parakeet", "groq".
    fn id(&self) -> &'static str;
    /// False only for cloud engines; the UI shows a privacy notice for those.
    fn is_local(&self) -> bool;
    async fn transcribe(&self, wav_path: &Path, opts: &TranscribeOptions) -> Result<RawTranscript>;
}
