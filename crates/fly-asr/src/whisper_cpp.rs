//! whisper.cpp sidecar engine: adaptive VAD splits the recording into speech
//! batches (long silence/noise never reaches the decoder — that is what
//! seeded 1881×-repetition hallucination loops in real meetings), each batch
//! is peak-normalized and transcribed by `whisper-cli` with word-level output
//! (`-ml 1 -sow -oj`), and word timestamps are mapped back to the original
//! timeline. Fully local; works on every hardware tier.

use std::path::{Path, PathBuf};

use fly_audio::vad::{detect_speech_spans, map_to_original, stitch_spans, SpeechSpan, VadConfig};
use fly_core::Word;

use crate::{AsrError, RawTranscript, Result, TranscribeOptions, TranscriptionEngine};

/// Cap on the speech content of one whisper-cli invocation. Batching many
/// short spans into one call amortizes model load; the cap keeps single
/// invocations bounded.
const MAX_BATCH_SPEECH_MS: u64 = 120_000;
/// Batches are boosted so their peak reaches this (never attenuated) —
/// system-loopback audio routinely peaks below −12 dBFS. Shared with the
/// Groq engine so both tiers apply identical preprocessing.
pub(crate) const NORMALIZE_TARGET_PEAK: f32 = 0.85;
/// …but a nearly-silent batch is never boosted more than this (~30 dB).
pub(crate) const NORMALIZE_MAX_GAIN: f32 = 31.6;

pub struct WhisperCppEngine {
    /// Path to whisper-cli(.exe).
    pub exe: PathBuf,
    /// Path to the GGML/GGUF model file.
    pub model: PathBuf,
    pub threads: usize,
    /// Pass `-ng` (--no-gpu) so a GPU-capable build (Metal on macOS, Vulkan
    /// on the pinned Windows build) decodes on CPU. False leaves the build's
    /// default untouched — identical invocation to before this flag existed.
    pub force_cpu: bool,
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
        self.transcribe_with_progress(wav_path, opts, &|_| {}).await
    }

    async fn transcribe_with_progress(
        &self,
        wav_path: &Path,
        opts: &TranscribeOptions,
        on_progress: crate::TranscribeProgressFn<'_>,
    ) -> Result<RawTranscript> {
        if !self.model.exists() {
            return Err(AsrError::ModelMissing(self.model.display().to_string()));
        }
        if !wav_path.exists() {
            return Err(AsrError::BadAudio(wav_path.display().to_string()));
        }

        let (samples, rate) = fly_audio::mix::read_wav_mono(wav_path)
            .map_err(|e| AsrError::BadAudio(format!("{}: {e}", wav_path.display())))?;
        let spans = detect_speech_spans(&samples, rate, &VadConfig::default());
        let total_speech_ms: u64 = spans.iter().map(|s| s.end_ms - s.start_ms).sum();
        tracing::debug!(
            file = %wav_path.display(),
            spans = spans.len(),
            speech_ms = total_speech_ms,
            total_ms = samples.len() as u64 * 1000 / rate.max(1) as u64,
            "vad segmentation"
        );

        let mut words = Vec::new();
        let mut language = None;
        let batches = plan_batches(&spans, MAX_BATCH_SPEECH_MS);
        // Initial 0% so the UI shows progress before the first — possibly
        // minutes-long — batch completes. A silent recording (no speech)
        // emits nothing: there is no work to report a fraction of.
        if total_speech_ms > 0 {
            on_progress(crate::TranscribeProgress {
                done_ms: 0,
                total_ms: total_speech_ms,
            });
        }
        for (batch, progress) in batches.iter().zip(cumulative_progress(&batches)) {
            let (mut chunk, map) = stitch_spans(&samples, rate, batch)
                .map_err(|e| AsrError::BadAudio(e.to_string()))?;
            fly_audio::mix::normalize_peak(&mut chunk, NORMALIZE_TARGET_PEAK, NORMALIZE_MAX_GAIN);
            let raw = self.run_whisper_cli(&chunk, rate, opts).await?;
            language = language.or(raw.language);
            words.extend(raw.words.into_iter().map(|mut w| {
                w.start_ms = map_to_original(w.start_ms, &map);
                w.end_ms = map_to_original(w.end_ms, &map);
                w
            }));
            on_progress(progress);
        }

        Ok(RawTranscript {
            language: language.or_else(|| opts.language.clone()),
            words,
            segments: vec![],
        })
    }
}

impl WhisperCppEngine {
    /// One whisper-cli invocation over prepared samples; returns parsed words
    /// with timestamps relative to the given samples.
    async fn run_whisper_cli(
        &self,
        samples: &[f32],
        rate: u32,
        opts: &TranscribeOptions,
    ) -> Result<RawTranscript> {
        let out_base =
            std::env::temp_dir().join(format!("flyonthewall-whisper-{}", uuid::Uuid::new_v4()));
        let wav_path = PathBuf::from(format!("{}.wav", out_base.display()));
        fly_audio::mix::write_wav_mono_16(&wav_path, samples, rate)
            .map_err(|e| AsrError::Engine(format!("write chunk wav: {e}")))?;

        let mut cmd = tokio::process::Command::new(&self.exe);
        cmd.arg("-m")
            .arg(&self.model)
            .arg("-f")
            .arg(&wav_path)
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
        if let Some(max_context) = opts.max_context {
            // 0 disables cross-window text carryover, the fuel that lets one
            // hallucinated window poison the rest of a recording
            cmd.arg("-mc").arg(max_context.to_string());
        }
        if let Some(prompt) = &opts.prompt {
            cmd.arg("--prompt").arg(prompt);
        }
        if self.force_cpu {
            cmd.arg("-ng");
        }
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW (no console flash from the sidecar) |
            // BELOW_NORMAL_PRIORITY_CLASS: transcription is background work —
            // audio capture and the user's foreground apps must win the CPU,
            // never the decoder.
            cmd.creation_flags(0x0800_0000 | 0x0000_4000);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| AsrError::Engine(format!("failed to launch whisper-cli: {e}")));
        let _ = std::fs::remove_file(&wav_path);
        let output = output?;
        log_device_lines(&String::from_utf8_lossy(&output.stderr));
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

/// Surface whisper-cli's compute-device lines (Metal/Vulkan/CUDA init,
/// fallbacks) in our logs — the only evidence of which device actually
/// decoded. Debug level: one whisper-cli run per ≤2 min speech batch.
fn log_device_lines(stderr: &str) {
    for line in stderr.lines() {
        let l = line.trim();
        let lower = l.to_ascii_lowercase();
        if ["ggml_vulkan", "ggml_metal", "ggml_cuda"]
            .iter()
            .any(|m| lower.starts_with(m))
            || lower.contains("use gpu")
            || lower.contains("backends")
        {
            tracing::debug!("whisper device: {l}");
        }
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

/// Group speech spans into transcription batches: consecutive spans join a
/// batch until its total speech reaches `max_speech_ms`; a single span longer
/// than the cap gets its own batch (contiguous speech is never cut). Also
/// used by the Groq engine to prefer cutting uploads at speech gaps.
pub(crate) fn plan_batches(spans: &[SpeechSpan], max_speech_ms: u64) -> Vec<Vec<SpeechSpan>> {
    let mut batches: Vec<Vec<SpeechSpan>> = Vec::new();
    let mut current: Vec<SpeechSpan> = Vec::new();
    let mut current_ms = 0u64;
    for span in spans {
        let len = span.end_ms.saturating_sub(span.start_ms);
        if !current.is_empty() && current_ms + len > max_speech_ms {
            batches.push(std::mem::take(&mut current));
            current_ms = 0;
        }
        current.push(*span);
        current_ms += len;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

/// After-each-batch progress points for a batch plan. The guarantees
/// consumers rely on: `done_ms` strictly increases, `total_ms` is constant,
/// and the final point is exactly `total_ms` — the UI reaches 100% with no
/// rounding drift. Empty for a silent recording (no batches). Extracted from
/// the decode loop so the accounting is testable without a whisper-cli.
pub(crate) fn cumulative_progress(batches: &[Vec<SpeechSpan>]) -> Vec<crate::TranscribeProgress> {
    let total_ms: u64 = batches
        .iter()
        .flatten()
        .map(|s| s.end_ms - s.start_ms)
        .sum();
    let mut done_ms = 0u64;
    batches
        .iter()
        .map(|batch| {
            done_ms += batch.iter().map(|s| s.end_ms - s.start_ms).sum::<u64>();
            crate::TranscribeProgress { done_ms, total_ms }
        })
        .collect()
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

    #[test]
    fn no_spans_no_batches() {
        assert!(plan_batches(&[], 120_000).is_empty());
    }

    #[test]
    fn small_spans_share_one_batch() {
        let spans = vec![span(0, 10), span(20, 35), span(50, 70)];
        let batches = plan_batches(&spans, 120_000);
        assert_eq!(batches, vec![spans]);
    }

    #[test]
    fn batch_splits_before_overflow() {
        let spans = vec![span(0, 60), span(100, 150), span(200, 230)];
        let batches = plan_batches(&spans, 120_000);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0], vec![span(0, 60), span(100, 150)]); // 110s speech
        assert_eq!(batches[1], vec![span(200, 230)]);
    }

    #[test]
    fn oversized_span_gets_its_own_batch() {
        let spans = vec![span(0, 200), span(300, 310)];
        let batches = plan_batches(&spans, 120_000);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0], vec![span(0, 200)]);
        assert_eq!(batches[1], vec![span(300, 310)]);
    }

    /// Multi-batch input: progress must rise strictly, keep a constant
    /// total, and land on exactly 100% (no rounding drift at the end).
    #[test]
    fn progress_is_monotonic_and_ends_at_exactly_total() {
        let spans = vec![span(0, 60), span(100, 150), span(200, 230), span(400, 520)];
        let batches = plan_batches(&spans, 120_000);
        assert!(batches.len() >= 3, "test needs genuinely multi-batch input");
        let points = cumulative_progress(&batches);
        assert_eq!(points.len(), batches.len());
        let total: u64 = spans.iter().map(|s| s.end_ms - s.start_ms).sum();
        let mut prev = 0u64;
        for p in &points {
            assert_eq!(p.total_ms, total);
            assert!(p.done_ms > prev, "done_ms must strictly increase");
            prev = p.done_ms;
        }
        assert_eq!(points.last().unwrap().done_ms, total, "must end at 100%");
    }

    /// A silent recording (VAD finds no speech) plans no batches and yields
    /// no progress points; combined with the engine skipping the initial
    /// event when total is 0, consumers never see a 0/0 fraction.
    #[test]
    fn silent_recording_yields_no_progress_points() {
        assert!(cumulative_progress(&plan_batches(&[], 120_000)).is_empty());
    }

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
