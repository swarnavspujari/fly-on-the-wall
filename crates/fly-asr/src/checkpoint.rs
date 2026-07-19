//! Per-batch ASR checkpoints: engines that decode a recording in batches
//! persist each batch's words as they complete, so a crash, an app restart,
//! or a mid-file engine failover resumes at the first undecoded batch
//! instead of throwing away hours of finished work (observed: a 586-minute
//! import re-decoded from zero three times).
//!
//! The checkpoint lives next to the input wav and is keyed by the decode
//! plan (engine, model, language, speech-span layout): anything that would
//! change the output — a different model, edited audio — changes the key and
//! the stale checkpoint is discarded wholesale. The file is derived data;
//! losing or corrupting it only costs a re-decode.

use std::path::{Path, PathBuf};

use fly_audio::vad::SpeechSpan;
use fly_core::Word;
use serde::{Deserialize, Serialize};

/// One completed decode batch, on the ORIGINAL recording timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    /// Language the engine detected while decoding this batch, if any.
    pub language: Option<String>,
    pub words: Vec<Word>,
    /// Which engine actually decoded this batch. A quota-paced cloud run may
    /// spill individual batches to a local engine, so this can differ from
    /// the plan's engine; `None` on checkpoints written before the field
    /// existed (serde default keeps them resumable).
    #[serde(default)]
    pub engine: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct CheckpointData {
    key: String,
    batches: Vec<BatchResult>,
}

/// The sidecar checkpoint file for one wav being transcribed.
pub struct CheckpointFile {
    path: PathBuf,
    key: String,
    batches: Vec<BatchResult>,
}

/// Where the checkpoint for `wav` lives (`track.16k.wav` →
/// `track.16k.asrpart.json`, in the same folder). Callers that clean up
/// intermediates use the same mapping.
pub fn checkpoint_path_for(wav: &Path) -> PathBuf {
    wav.with_extension("asrpart.json")
}

/// Stable key for a decode plan. Any change to the engine, model, language
/// setting, or the batched speech-span layout produces a different key.
pub fn plan_key(
    engine: &str,
    model_tag: &str,
    language: Option<&str>,
    batches: &[Vec<SpeechSpan>],
) -> String {
    // FNV-1a over the span layout: no crypto needed, only change detection.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
    };
    for batch in batches {
        mix(batch.len() as u64);
        for span in batch {
            mix(span.start_ms);
            mix(span.end_ms);
        }
    }
    format!(
        "{engine}|{model_tag}|{}|{}b|{h:016x}",
        language.unwrap_or("auto"),
        batches.len()
    )
}

impl CheckpointFile {
    /// Open the checkpoint at `path` for the given plan key. Batches from a
    /// previous run survive only when the stored key matches; a missing,
    /// corrupt, or differently-keyed file starts empty.
    pub fn open(path: PathBuf, key: String) -> Self {
        let batches = std::fs::read_to_string(&path)
            .ok()
            .and_then(|json| serde_json::from_str::<CheckpointData>(&json).ok())
            .filter(|data| data.key == key)
            .map(|data| data.batches)
            .unwrap_or_default();
        Self { path, key, batches }
    }

    /// Batches completed by previous runs of the same plan, in decode order.
    pub fn resumed(&self) -> Vec<BatchResult> {
        self.batches.clone()
    }

    /// Record one newly completed batch. Persisting is best-effort: a failed
    /// write only costs resumability, never the transcription.
    pub fn push(&mut self, batch: BatchResult) {
        self.batches.push(batch);
        let data = CheckpointData {
            key: self.key.clone(),
            batches: std::mem::take(&mut self.batches),
        };
        let json = serde_json::to_string(&data).expect("checkpoint serializes");
        self.batches = data.batches;
        // tmp + rename so a crash mid-write leaves the previous checkpoint
        // intact (a torn file would fail to parse and lose the whole resume)
        let tmp = self.path.with_extension("asrpart.json.tmp");
        let write = std::fs::write(&tmp, json).and_then(|()| {
            let _ = std::fs::remove_file(&self.path); // Windows: rename won't overwrite
            std::fs::rename(&tmp, &self.path)
        });
        if let Err(e) = write {
            tracing::warn!(path = %self.path.display(), error = %e, "could not persist ASR checkpoint");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start_s: u64, end_s: u64) -> SpeechSpan {
        SpeechSpan {
            start_ms: start_s * 1000,
            end_ms: end_s * 1000,
        }
    }

    fn word(text: &str, at: u64) -> Word {
        Word {
            text: text.into(),
            start_ms: at,
            end_ms: at + 100,
        }
    }

    #[test]
    fn plan_key_changes_with_engine_model_language_and_span_layout() {
        let plan = vec![vec![span(0, 10)], vec![span(20, 30)]];
        let base = plan_key("whisper.cpp", "large-v3.bin", Some("en"), &plan);
        assert_eq!(
            base,
            plan_key("whisper.cpp", "large-v3.bin", Some("en"), &plan),
            "same plan must produce the same key"
        );
        for other in [
            plan_key("groq", "large-v3.bin", Some("en"), &plan),
            plan_key("whisper.cpp", "small-q5.bin", Some("en"), &plan),
            plan_key("whisper.cpp", "large-v3.bin", None, &plan),
            plan_key(
                "whisper.cpp",
                "large-v3.bin",
                Some("en"),
                &[vec![span(0, 10)], vec![span(20, 31)]],
            ),
            plan_key(
                "whisper.cpp",
                "large-v3.bin",
                Some("en"),
                &[vec![span(0, 10), span(20, 30)]],
            ),
        ] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn completed_batches_survive_a_reopen_with_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = checkpoint_path_for(&dir.path().join("track.16k.wav"));
        let key = "whisper.cpp|m|en|2b|abc".to_string();

        let mut c = CheckpointFile::open(path.clone(), key.clone());
        assert!(c.resumed().is_empty(), "no file yet → nothing to resume");
        c.push(BatchResult {
            language: Some("en".into()),
            words: vec![word("hello", 100)],
            engine: Some("groq".into()),
        });
        c.push(BatchResult {
            language: None,
            words: vec![word("again", 20_100)],
            engine: Some("whisper.cpp".into()),
        });

        let reopened = CheckpointFile::open(path, key);
        let resumed = reopened.resumed();
        assert_eq!(resumed.len(), 2);
        assert_eq!(resumed[0].words[0].text, "hello");
        assert_eq!(resumed[0].language.as_deref(), Some("en"));
        assert_eq!(resumed[0].engine.as_deref(), Some("groq"));
        assert_eq!(resumed[1].words[0].start_ms, 20_100);
        assert_eq!(resumed[1].engine.as_deref(), Some("whisper.cpp"));
    }

    /// v1.5.0 wrote checkpoints without the per-batch `engine` field; a
    /// version upgrade mid-import must not discard those finished batches.
    #[test]
    fn pre_engine_field_checkpoints_still_resume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.16k.asrpart.json");
        std::fs::write(
            &path,
            r#"{"key":"k","batches":[{"language":"en","words":[{"text":"old","start_ms":0,"end_ms":100}]}]}"#,
        )
        .unwrap();
        let c = CheckpointFile::open(path, "k".into());
        let resumed = c.resumed();
        assert_eq!(resumed.len(), 1, "old-format batches must survive");
        assert_eq!(resumed[0].words[0].text, "old");
        assert_eq!(resumed[0].engine, None);
    }

    #[test]
    fn a_different_plan_key_discards_the_stale_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = checkpoint_path_for(&dir.path().join("track.16k.wav"));
        let mut c = CheckpointFile::open(path.clone(), "old-key".into());
        c.push(BatchResult {
            language: None,
            words: vec![word("stale", 0)],
            engine: None,
        });

        let c = CheckpointFile::open(path.clone(), "new-key".into());
        assert!(
            c.resumed().is_empty(),
            "a model/plan change must not resume stale batches"
        );
    }

    #[test]
    fn a_corrupt_checkpoint_file_starts_over_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.16k.asrpart.json");
        std::fs::write(&path, "not json {").unwrap();
        let c = CheckpointFile::open(path, "k".into());
        assert!(c.resumed().is_empty());
    }

    #[test]
    fn checkpoint_path_sits_next_to_the_wav() {
        assert_eq!(
            checkpoint_path_for(Path::new("dir/track.16k.wav")),
            Path::new("dir/track.16k.asrpart.json")
        );
    }
}
