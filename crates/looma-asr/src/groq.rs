//! Groq cloud ASR — FALLBACK ONLY (spec §6.2 "Cloud" tier). Audio leaves the
//! device when this engine runs; the UI must show a privacy notice before
//! enabling it. Returns no speaker labels — diarization still runs locally
//! and is merged with these word timestamps (spec §6.3).

use std::path::Path;

use looma_core::Word;

use crate::{AsrError, RawTranscript, Result, TranscribeOptions, TranscriptionEngine};

pub const GROQ_DEFAULT_MODEL: &str = "whisper-large-v3-turbo";

pub struct GroqEngine {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
}

impl GroqEngine {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: GROQ_DEFAULT_MODEL.to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl TranscriptionEngine for GroqEngine {
    fn id(&self) -> &'static str {
        "groq"
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn transcribe(&self, wav_path: &Path, opts: &TranscribeOptions) -> Result<RawTranscript> {
        let bytes = std::fs::read(wav_path)?;
        let file_name = wav_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "audio.wav".into());

        let mut form = reqwest::multipart::Form::new()
            .part(
                "file",
                reqwest::multipart::Part::bytes(bytes)
                    .file_name(file_name)
                    .mime_str("audio/wav")
                    .map_err(|e| AsrError::Engine(e.to_string()))?,
            )
            .text("model", self.model.clone())
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "word");
        if let Some(lang) = &opts.language {
            form = form.text("language", lang.clone());
        }
        if let Some(prompt) = &opts.prompt {
            form = form.text("prompt", prompt.clone());
        }

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/audio/transcriptions", self.base_url))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| AsrError::Network(e.to_string()))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AsrError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(AsrError::Engine(format!(
                "groq returned {status}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        parse_groq_verbose_json(&body)
    }
}

/// Parse the OpenAI-compatible `verbose_json` transcription response.
pub fn parse_groq_verbose_json(json: &str) -> Result<RawTranscript> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| AsrError::Engine(format!("bad JSON: {e}")))?;
    let language = v
        .get("language")
        .and_then(|l| l.as_str())
        .map(str::to_string);

    let mut words = Vec::new();
    if let Some(entries) = v.get("words").and_then(|w| w.as_array()) {
        for entry in entries {
            let raw = entry.get("word").and_then(|t| t.as_str()).unwrap_or("");
            let start = entry.get("start").and_then(|x| x.as_f64());
            let end = entry.get("end").and_then(|x| x.as_f64());
            if crate::is_non_speech_token(raw) {
                continue;
            }
            let text = crate::clean_word_text(raw);
            if text.is_empty() {
                continue;
            }
            if let (Some(s), Some(e)) = (start, end) {
                words.push(Word {
                    text,
                    start_ms: (s * 1000.0) as u64,
                    end_ms: (e * 1000.0) as u64,
                });
            }
        }
    }

    Ok(RawTranscript {
        language,
        words: crate::drop_non_speech_spans(words),
        segments: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_verbose_json_words_in_seconds() {
        let json = r#"{
            "language": "english",
            "text": "Good morning",
            "words": [
                {"word": "Good", "start": 0.15, "end": 0.37},
                {"word": "morning", "start": 0.37, "end": 1.0}
            ]
        }"#;
        let t = parse_groq_verbose_json(json).unwrap();
        assert_eq!(t.words.len(), 2);
        assert_eq!(t.words[0].start_ms, 150);
        assert_eq!(t.words[1].end_ms, 1000);
    }
}
