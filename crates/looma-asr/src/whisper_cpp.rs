//! whisper.cpp sidecar engine: shells out to `whisper-cli.exe` with
//! word-level output (`-ml 1 -sow -oj`) and parses the JSON it writes.
//! Fully local; works on every hardware tier.

use std::path::{Path, PathBuf};

use looma_core::Word;

use crate::{AsrError, RawTranscript, Result, TranscribeOptions, TranscriptionEngine};

pub struct WhisperCppEngine {
    /// Path to whisper-cli(.exe).
    pub exe: PathBuf,
    /// Path to the GGML/GGUF model file.
    pub model: PathBuf,
    pub threads: usize,
}

#[async_trait::async_trait]
impl TranscriptionEngine for WhisperCppEngine {
    fn id(&self) -> &'static str {
        "whisper.cpp"
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn transcribe(&self, wav_path: &Path, opts: &TranscribeOptions) -> Result<RawTranscript> {
        if !self.model.exists() {
            return Err(AsrError::ModelMissing(self.model.display().to_string()));
        }
        if !wav_path.exists() {
            return Err(AsrError::BadAudio(wav_path.display().to_string()));
        }
        let out_base = std::env::temp_dir().join(format!("looma-whisper-{}", uuid::Uuid::new_v4()));

        let mut cmd = tokio::process::Command::new(&self.exe);
        cmd.arg("-m")
            .arg(&self.model)
            .arg("-f")
            .arg(wav_path)
            .arg("-oj")
            .arg("-of")
            .arg(&out_base)
            // one word per JSON entry — the aligner needs word timestamps
            .arg("-ml")
            .arg("1")
            .arg("-sow")
            .arg("-t")
            .arg(self.threads.max(1).to_string())
            .arg("-l")
            .arg(opts.language.as_deref().unwrap_or("auto"));
        if let Some(prompt) = &opts.prompt {
            cmd.arg("--prompt").arg(prompt);
        }
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: no console flash from the sidecar
            cmd.creation_flags(0x0800_0000);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| AsrError::Engine(format!("failed to launch whisper-cli: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AsrError::Engine(format!(
                "whisper-cli exited with {}: {}",
                output.status,
                stderr.chars().take(500).collect::<String>()
            )));
        }

        let json_path = PathBuf::from(format!("{}.json", out_base.display()));
        let json = std::fs::read_to_string(&json_path)?;
        let _ = std::fs::remove_file(&json_path);
        parse_whisper_json(&json)
    }
}

/// Parse whisper-cli's `-oj` output (with `-ml 1 -sow`, each transcription
/// entry is one word).
pub fn parse_whisper_json(json: &str) -> Result<RawTranscript> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| AsrError::Engine(format!("bad JSON: {e}")))?;
    let language = v
        .pointer("/result/language")
        .and_then(|l| l.as_str())
        .map(str::to_string);

    let mut words = Vec::new();
    if let Some(entries) = v.get("transcription").and_then(|t| t.as_array()) {
        for entry in entries {
            let raw = entry.get("text").and_then(|t| t.as_str()).unwrap_or("");
            if crate::is_non_speech_token(raw) {
                continue;
            }
            let text = crate::clean_word_text(raw);
            if text.is_empty() {
                continue;
            }
            let from = entry.pointer("/offsets/from").and_then(|x| x.as_u64());
            let to = entry.pointer("/offsets/to").and_then(|x| x.as_u64());
            if let (Some(start_ms), Some(end_ms)) = (from, to) {
                words.push(Word {
                    text,
                    start_ms,
                    end_ms,
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
    fn parses_word_entries_and_skips_blanks() {
        let json = r#"{
            "result": {"language": "en"},
            "transcription": [
                {"offsets": {"from": 0, "to": 150}, "text": ""},
                {"offsets": {"from": 150, "to": 370}, "text": " Good"},
                {"offsets": {"from": 370, "to": 1000}, "text": " morning"},
                {"offsets": {"from": 1000, "to": 2040}, "text": " everyone."}
            ]
        }"#;
        let t = parse_whisper_json(json).unwrap();
        assert_eq!(t.language.as_deref(), Some("en"));
        assert_eq!(t.words.len(), 3);
        assert_eq!(t.words[0].text, "Good");
        assert_eq!(t.words[0].start_ms, 150);
        assert_eq!(t.words[2].end_ms, 2040);
    }

    #[test]
    fn bad_json_is_an_engine_error() {
        assert!(parse_whisper_json("not json").is_err());
    }
}
