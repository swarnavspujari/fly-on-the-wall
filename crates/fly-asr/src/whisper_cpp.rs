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
    /// Persist each completed batch next to the input wav and skip batches a
    /// previous run of the same plan already decoded (crate::checkpoint) —
    /// hours of finished ASR must survive a crash, restart, or failover.
    /// Off for short-lived callers (benchmarks, live captions).
    pub resume: bool,
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
        self.transcribe_with_progress(wav_path, opts, &|_| {}, &|| false)
            .await
    }

    async fn transcribe_with_progress(
        &self,
        wav_path: &Path,
        opts: &TranscribeOptions,
        on_progress: crate::TranscribeProgressFn<'_>,
        cancel: crate::CancelFn<'_>,
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
        let mut ckpt = self.resume.then(|| {
            let model_tag = self
                .model
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            crate::checkpoint::CheckpointFile::open(
                crate::checkpoint::checkpoint_path_for(wav_path),
                crate::checkpoint::plan_key(
                    "whisper.cpp",
                    &model_tag,
                    opts.language.as_deref(),
                    &batches,
                ),
            )
        });
        let resumed = ckpt.as_ref().map(|c| c.resumed()).unwrap_or_default();
        if !resumed.is_empty() {
            tracing::info!(
                resumed = resumed.len(),
                total = batches.len(),
                "resuming transcription from ASR checkpoint"
            );
        }
        // Initial 0% so the UI shows progress before the first — possibly
        // minutes-long — batch completes. A silent recording (no speech)
        // emits nothing: there is no work to report a fraction of.
        if total_speech_ms > 0 {
            on_progress(crate::TranscribeProgress {
                done_ms: 0,
                total_ms: total_speech_ms,
                quota_wait_ms: None,
            });
        }
        for (i, (batch, progress)) in batches
            .iter()
            .zip(cumulative_progress(&batches))
            .enumerate()
        {
            if let Some(done) = resumed.get(i) {
                language = language.or_else(|| done.language.clone());
                words.extend(done.words.iter().cloned());
                on_progress(progress);
                continue;
            }
            // between batches is the cheap place to abort: everything decoded
            // so far is already checkpointed
            if cancel() {
                return Err(AsrError::Cancelled);
            }
            let (mut chunk, map) = stitch_spans(&samples, rate, batch)
                .map_err(|e| AsrError::BadAudio(e.to_string()))?;
            fly_audio::mix::normalize_peak(&mut chunk, NORMALIZE_TARGET_PEAK, NORMALIZE_MAX_GAIN);
            let raw = self.run_whisper_cli(&chunk, rate, opts).await?;
            let batch_language = raw.language;
            let mapped: Vec<Word> = raw
                .words
                .into_iter()
                .map(|mut w| {
                    w.start_ms = map_to_original(w.start_ms, &map);
                    w.end_ms = map_to_original(w.end_ms, &map);
                    w
                })
                .collect();
            if let Some(c) = ckpt.as_mut() {
                c.push(crate::checkpoint::BatchResult {
                    language: batch_language.clone(),
                    words: mapped.clone(),
                    engine: Some(self.id().to_string()),
                });
            }
            language = language.or(batch_language);
            words.extend(mapped);
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
            crate::TranscribeProgress {
                done_ms,
                total_ms,
                quota_wait_ms: None,
            }
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

    /// Resume proof: with a checkpoint covering every batch of the plan, the
    /// engine must return the checkpointed words WITHOUT launching
    /// whisper-cli at all (the exe here doesn't exist — any decode attempt
    /// would error). This is the property that saves an 11-hour job from a
    /// post-ASR failure: the retry re-reads finished batches instead of
    /// re-decoding them.
    #[tokio::test]
    async fn completed_checkpoint_batches_resume_without_decoding() {
        let dir = tempfile::tempdir().unwrap();
        // 2s of clear tone at 16 kHz → exactly one speech span / one batch
        let rate = 16_000u32;
        let samples: Vec<f32> = (0..(2 * rate) as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.4)
            .collect();
        let wav = dir.path().join("track.16k.wav");
        fly_audio::mix::write_wav_mono_16(&wav, &samples, rate).unwrap();
        let model = dir.path().join("model.bin");
        std::fs::write(&model, b"fake model").unwrap();

        // the same plan the engine will compute
        let spans = detect_speech_spans(&samples, rate, &VadConfig::default());
        let batches = plan_batches(&spans, MAX_BATCH_SPEECH_MS);
        assert!(!batches.is_empty(), "test wav must contain speech");
        let opts = TranscribeOptions {
            language: Some("en".into()),
            ..Default::default()
        };
        let key = crate::checkpoint::plan_key("whisper.cpp", "model.bin", Some("en"), &batches);
        let mut ckpt = crate::checkpoint::CheckpointFile::open(
            crate::checkpoint::checkpoint_path_for(&wav),
            key,
        );
        for _ in &batches {
            ckpt.push(crate::checkpoint::BatchResult {
                language: Some("en".into()),
                words: vec![Word {
                    text: "resumed".into(),
                    start_ms: 100,
                    end_ms: 300,
                }],
                engine: Some("whisper.cpp".into()),
            });
        }

        let engine = WhisperCppEngine {
            exe: dir.path().join("does-not-exist.exe"),
            model,
            threads: 1,
            force_cpu: true,
            resume: true,
        };
        let raw = engine
            .transcribe(&wav, &opts)
            .await
            .expect("fully-checkpointed run must not decode");
        assert_eq!(raw.words.len(), batches.len());
        assert_eq!(raw.words[0].text, "resumed");
        assert_eq!(raw.language.as_deref(), Some("en"));

        // ...and with resume off, the missing exe surfaces immediately —
        // proof the checkpoint is what rescued the run above.
        let engine = WhisperCppEngine {
            resume: false,
            ..engine
        };
        assert!(engine.transcribe(&wav, &opts).await.is_err());
    }

    /// Cancellation is checked before each batch decode: with cancel already
    /// requested the engine returns Cancelled without launching whisper-cli
    /// at all (the exe here doesn't exist — a decode attempt would surface a
    /// different error).
    #[tokio::test]
    async fn cancel_between_batches_aborts_without_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let rate = 16_000u32;
        let samples: Vec<f32> = (0..(2 * rate) as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.4)
            .collect();
        let wav = dir.path().join("track.16k.wav");
        fly_audio::mix::write_wav_mono_16(&wav, &samples, rate).unwrap();
        let model = dir.path().join("model.bin");
        std::fs::write(&model, b"fake model").unwrap();

        let engine = WhisperCppEngine {
            exe: dir.path().join("does-not-exist.exe"),
            model,
            threads: 1,
            force_cpu: true,
            resume: false,
        };
        let err = engine
            .transcribe_with_progress(&wav, &TranscribeOptions::default(), &|_| {}, &|| true)
            .await
            .unwrap_err();
        assert!(matches!(err, AsrError::Cancelled), "{err:?}");
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
