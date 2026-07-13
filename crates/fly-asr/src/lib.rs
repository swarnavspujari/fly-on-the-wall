//! fly-asr: the `TranscriptionEngine` trait and its backends.
//!
//! Backends (landing in M3):
//! - whisper.cpp — primary, fully local, every hardware tier
//! - parakeet — optional local engine for NVIDIA/Apple hardware
//! - groq — cloud FALLBACK only (audio leaves the device; the UI must say
//!   so). Returns no speaker labels; diarization always runs locally
//!   regardless (spec §6.3).

pub mod groq;
pub mod whisper_cpp;

use std::path::Path;

use fly_core::Word;
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
    /// Cloud ASR rejected the request (4xx) — retrying the identical payload
    /// can never succeed. Display starts with [`REJECTED_MARKER`] so
    /// string-typed layers (the app's job scheduler) can stop retrying.
    #[error("cloud ASR rejected the request: {0}")]
    Rejected(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, AsrError>;

/// Substring of [`AsrError::Rejected`]'s Display text. The pipeline reduces
/// errors to strings before they reach the scheduler, which matches on this
/// to mark 4xx request failures permanent (no retry).
pub const REJECTED_MARKER: &str = "cloud ASR rejected the request";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscribeOptions {
    /// BCP-47-ish language hint ("en", "de"); `None` = auto-detect.
    pub language: Option<String>,
    /// Initial prompt / vocabulary hint passed to the engine when supported.
    pub prompt: Option<String>,
    /// Cross-window text context cap for local whisper (`-mc`); `Some(0)`
    /// decodes every 30 s window independently, which stops one hallucinated
    /// window from poisoning the rest of the file. `None` = engine default.
    pub max_context: Option<i32>,
}

/// True for tokens ASR engines emit that no person said: whisper's silence /
/// noise annotations ("[BLANK_AUDIO]", "[ Silence]", "(coughing)", "♪") and
/// bare ">>" speaker-change markers. Seen in every real recording with quiet
/// stretches — they must never reach the aligner or the transcript.
pub fn is_non_speech_token(text: &str) -> bool {
    let t = text.trim();
    t.is_empty()
        || t == ">>"
        || (t.starts_with('[') && t.ends_with(']'))
        || (t.starts_with('(') && t.ends_with(')'))
        || t.chars().all(|c| c == '♪' || c.is_whitespace())
}

/// Strip engine markers that prefix real words (whisper glues ">>" onto the
/// first word after a speaker change).
pub fn clean_word_text(text: &str) -> String {
    text.trim().trim_start_matches(">>").trim().to_string()
}

#[cfg(test)]
mod token_tests {
    use super::*;

    #[test]
    fn non_speech_tokens_are_detected() {
        for t in [
            "[BLANK_AUDIO]",
            "[ Silence]",
            "[silence]",
            "(coughing)",
            "[Music]",
            "♪",
            ">>",
            "  ",
        ] {
            assert!(is_non_speech_token(t), "expected non-speech: {t:?}");
        }
        for t in ["Thanks", "SSO", "[unfinished", "friday."] {
            assert!(!is_non_speech_token(t), "expected speech: {t:?}");
        }
    }

    #[test]
    fn speaker_change_marker_is_stripped() {
        assert_eq!(clean_word_text(">> Sounds"), "Sounds");
        assert_eq!(clean_word_text("  hello "), "hello");
    }

    fn w(text: &str, at: u64) -> Word {
        Word {
            text: text.into(),
            start_ms: at,
            end_ms: at + 100,
        }
    }

    #[test]
    fn split_bracket_annotations_are_dropped() {
        let words = vec![
            w("[", 0),
            w(" Silence", 100),
            w("]", 200),
            w("Thanks", 300),
            w("for", 400),
            w("joining.", 500),
        ];
        let out = drop_non_speech_spans(words);
        let texts: Vec<_> = out.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(texts, vec!["Thanks", "for", "joining."]);
    }

    #[test]
    fn unmatched_bracket_keeps_real_speech() {
        let words = vec![
            w("[", 0),
            w("bracket", 100),
            w("but", 200),
            w("speech", 300),
        ];
        let out = drop_non_speech_spans(words.clone());
        assert_eq!(out.len(), words.len());
    }
}

/// Whisper's word-level mode (`-ml 1`) can split an annotation like
/// "[ Silence]" across several word entries ("[", " Silence", "]"), which the
/// per-token check can't see. Drop any short bracketed span; an unmatched
/// opening bracket keeps its words (never eat real speech on a stray "[").
pub fn drop_non_speech_spans(words: Vec<Word>) -> Vec<Word> {
    const MAX_SPAN: usize = 8;
    let mut out = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let t = words[i].text.trim();
        let closer = match t.chars().next() {
            Some('[') if !t.ends_with(']') => Some(']'),
            Some('(') if !t.ends_with(')') => Some(')'),
            _ => None,
        };
        if let Some(close) = closer {
            let end = (i + 1..words.len().min(i + 1 + MAX_SPAN))
                .find(|&j| words[j].text.trim_end().ends_with(close));
            if let Some(end) = end {
                i = end + 1; // whole annotation span dropped
                continue;
            }
        }
        out.push(words[i].clone());
        i += 1;
    }
    out
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
