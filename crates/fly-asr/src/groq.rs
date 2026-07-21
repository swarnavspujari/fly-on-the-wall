//! Groq cloud ASR — FALLBACK ONLY (spec §6.2 "Cloud" tier). Audio leaves the
//! device when this engine runs; the UI must show a privacy notice before
//! enabling it. Returns no speaker labels — diarization still runs locally
//! and is merged with these word timestamps (spec §6.3).
//!
//! Preprocessing mirrors the local whisper.cpp path: adaptive VAD strips long
//! non-speech before upload (anti-hallucination + smaller payloads), audio is
//! peak-normalized with the same constants, and returned word timestamps are
//! mapped back to the original timeline for the local aligner/diarizer.

use std::path::Path;

use fly_audio::vad::{detect_speech_spans, map_to_original, stitch_spans, VadConfig};
use fly_core::Word;

use crate::retry::{classify_http_failure, retry_delay, QuotaPacer};
use crate::whisper_cpp::{plan_batches, NORMALIZE_MAX_GAIN, NORMALIZE_TARGET_PEAK};
use crate::{AsrError, RawTranscript, Result, TranscribeOptions, TranscriptionEngine};

pub const GROQ_DEFAULT_MODEL: &str = "whisper-large-v3-turbo";

/// Per-upload payload cap (16-bit mono WAV data), with headroom under Groq's
/// free-tier 25 MB / 413 limit. ~9.8 min of 16 kHz audio.
const MAX_CHUNK_BYTES: u64 = 18 * 1024 * 1024;
/// Adjacent chunks share this much audio so no word is cut mid-utterance at a
/// boundary; the overlap is kept from exactly one side when stitching.
const OVERLAP_MS: u64 = 5_000;

pub struct GroqEngine {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    /// Persist each completed upload batch next to the input wav and skip
    /// batches a previous run of the same plan already transcribed
    /// (crate::checkpoint) — paced cloud runs are long, and finished chunks
    /// must survive a crash or restart.
    pub resume: bool,
    /// Local engine that decodes a chunk whenever the quota pacer would make
    /// it wait longer than [`crate::retry::SPILL_TO_LOCAL_THRESHOLD_MS`] —
    /// a benchmark-validated GPU must never idle under a full cloud budget
    /// (the incident: 100+ minutes of pacer sleep with a 24 s/batch GPU
    /// below it in the chain). `None` keeps the pure-wait behavior.
    pub local_spillover: Option<Box<dyn TranscriptionEngine>>,
}

impl GroqEngine {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: GROQ_DEFAULT_MODEL.to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            resume: false,
            local_spillover: None,
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
        // Same preprocessing as the local whisper.cpp path: VAD strips long
        // non-speech (silence reaching a Whisper decoder is how repetition
        // hallucination starts — and it's wasted upload bytes), each payload
        // is peak-normalized, and word timestamps are mapped back to the
        // original timeline so local diarization alignment stays correct.
        let (samples, rate) = fly_audio::mix::read_wav_mono(wav_path)
            .map_err(|e| AsrError::BadAudio(format!("{}: {e}", wav_path.display())))?;
        let spans = detect_speech_spans(&samples, rate, &VadConfig::default());
        let segment_ms = MAX_CHUNK_BYTES / 2 * 1000 / rate as u64;
        let total_speech_ms: u64 = spans.iter().map(|s| s.end_ms - s.start_ms).sum();
        tracing::debug!(
            file = %wav_path.display(),
            spans = spans.len(),
            speech_ms = total_speech_ms,
            total_ms = samples.len() as u64 * 1000 / rate.max(1) as u64,
            "vad segmentation (groq)"
        );

        let mut language = None;
        let mut words = Vec::new();
        // One pacer per job: uploads are proactively spaced so the whole run
        // stays inside the free-tier audio-per-hour quota instead of burning
        // it in minutes and losing the job to a 429.
        let started = std::time::Instant::now();
        let mut pacer = QuotaPacer::default();
        let batches = plan_batches(&spans, segment_ms);
        let progress_points = crate::whisper_cpp::cumulative_progress(&batches);
        let mut ckpt = self.resume.then(|| {
            crate::checkpoint::CheckpointFile::open(
                crate::checkpoint::checkpoint_path_for(wav_path),
                crate::checkpoint::plan_key(
                    "groq",
                    &self.model,
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
                "resuming cloud transcription from ASR checkpoint"
            );
        }
        // Initial 0% so the UI shows the cloud run is live before the first —
        // possibly paced — upload completes (progress parity with the local
        // engine). A silent recording emits nothing: no work to report.
        if total_speech_ms > 0 {
            on_progress(crate::TranscribeProgress {
                done_ms: 0,
                total_ms: total_speech_ms,
                quota_wait_ms: None,
            });
        }
        // Uploads are cut at speech-span gaps (plan_batches). Only a single
        // contiguous speech span longer than the payload cap falls back to
        // fixed overlapping chunks stitched at the overlap midpoint.
        for (bi, (batch, progress)) in batches.iter().zip(progress_points).enumerate() {
            if let Some(done) = resumed.get(bi) {
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
            let batch_speech_ms: u64 = batch.iter().map(|s| s.end_ms - s.start_ms).sum();
            let ctx = ChunkCtx {
                on_progress,
                cancel,
                done_ms: progress.done_ms - batch_speech_ms,
                total_ms: total_speech_ms,
            };
            let mut batch_language = None;
            // "groq" unless the quota pacer spilled decoding to the injected
            // local engine — recorded per batch in the checkpoint.
            let mut decoded_by: &'static str = "groq";
            let oversized = batch.len() == 1 && batch[0].end_ms - batch[0].start_ms > segment_ms;
            let batch_words: Vec<Word> = if oversized {
                let span = batch[0];
                let s0 = (span.start_ms * rate as u64 / 1000) as usize;
                let e0 = ((span.end_ms * rate as u64 / 1000) as usize).min(samples.len());
                let mut span_samples = samples[s0..e0].to_vec();
                fly_audio::mix::normalize_peak(
                    &mut span_samples,
                    NORMALIZE_TARGET_PEAK,
                    NORMALIZE_MAX_GAIN,
                );
                let chunks = plan_chunks(span.end_ms - span.start_ms, segment_ms, OVERLAP_MS);
                let mut per_chunk: Vec<Vec<Word>> = Vec::new();
                for (i, &(cs, ce)) in chunks.iter().enumerate() {
                    let s = (cs * rate as u64 / 1000) as usize;
                    let e = ((ce * rate as u64 / 1000) as usize).min(span_samples.len());
                    let (raw, by) = self
                        .upload_chunk(
                            &mut pacer,
                            &started,
                            wav_bytes_mono_16(&span_samples[s..e], rate),
                            (e - s) as u64 * 1000 / rate as u64,
                            format!("speech-{bi}-{i}.wav"),
                            opts,
                            &ctx,
                        )
                        .await?;
                    if by != "groq" {
                        decoded_by = by;
                    }
                    batch_language = batch_language.or(raw.language);
                    per_chunk.push(raw.words);
                }
                // stitched timestamps are relative to the span; shift to
                // absolute original-file time
                offset_words(stitch_chunk_words(&chunks, per_chunk), span.start_ms)
            } else {
                let (mut chunk, map) = stitch_spans(&samples, rate, batch)
                    .map_err(|e| AsrError::BadAudio(e.to_string()))?;
                fly_audio::mix::normalize_peak(
                    &mut chunk,
                    NORMALIZE_TARGET_PEAK,
                    NORMALIZE_MAX_GAIN,
                );
                let audio_ms = chunk.len() as u64 * 1000 / rate as u64;
                let (raw, by) = self
                    .upload_chunk(
                        &mut pacer,
                        &started,
                        wav_bytes_mono_16(&chunk, rate),
                        audio_ms,
                        format!("speech-{bi}.wav"),
                        opts,
                        &ctx,
                    )
                    .await?;
                if by != "groq" {
                    decoded_by = by;
                }
                batch_language = raw.language;
                raw.words
                    .into_iter()
                    .map(|mut w| {
                        w.start_ms = map_to_original(w.start_ms, &map);
                        w.end_ms = map_to_original(w.end_ms, &map);
                        w
                    })
                    .collect()
            };
            if let Some(c) = ckpt.as_mut() {
                c.push(crate::checkpoint::BatchResult {
                    language: batch_language.clone(),
                    words: batch_words.clone(),
                    engine: Some(decoded_by.to_string()),
                });
            }
            language = language.or(batch_language);
            words.extend(batch_words);
            on_progress(progress);
        }

        Ok(RawTranscript {
            language: language.or_else(|| opts.language.clone()),
            words,
            segments: vec![],
        })
    }
}

/// Progress/cancel context threaded into the per-chunk upload flow, so the
/// pacer can surface "waiting for quota" instead of sleeping silently and a
/// cancelled note aborts mid-wait.
struct ChunkCtx<'a> {
    on_progress: crate::TranscribeProgressFn<'a>,
    cancel: crate::CancelFn<'a>,
    /// Speech ms completed before this batch / job total — the baseline the
    /// quota-wait events report.
    done_ms: u64,
    total_ms: u64,
}

/// Sleep `ms`, polling `cancel` about once a second: a deleted note must
/// never wait out a (possibly near-hour) quota window or retry backoff.
async fn sleep_cancellable(ms: u64, cancel: crate::CancelFn<'_>) -> Result<()> {
    let mut remaining = ms;
    while remaining > 0 {
        if cancel() {
            return Err(AsrError::Cancelled);
        }
        let step = remaining.min(1_000);
        tokio::time::sleep(std::time::Duration::from_millis(step)).await;
        remaining -= step;
    }
    if cancel() {
        return Err(AsrError::Cancelled);
    }
    Ok(())
}

/// Shift word timestamps by a constant offset (span-relative → absolute).
fn offset_words(words: Vec<Word>, offset_ms: u64) -> Vec<Word> {
    words
        .into_iter()
        .map(|mut w| {
            w.start_ms += offset_ms;
            w.end_ms += offset_ms;
            w
        })
        .collect()
}

impl GroqEngine {
    /// One chunk, resiliently: when the free-tier pacer would wait longer
    /// than the spill threshold and a local engine is injected, decode THIS
    /// chunk locally and return to the cloud for the next one; shorter waits
    /// sleep (surfaced as a quota-wait status, never a silent spinner). Then
    /// retry transient failures per the policy in [`crate::retry`] — bounded
    /// backoff for network errors, honored `retry-after` for 429s. Only a
    /// hard rejection (4xx) or an exhausted retry budget surfaces to the
    /// caller, whose whole-job local fallback remains the last resort.
    /// Returns the transcript and the id of the engine that decoded it.
    #[allow(clippy::too_many_arguments)]
    async fn upload_chunk(
        &self,
        pacer: &mut QuotaPacer,
        job_started: &std::time::Instant,
        bytes: Vec<u8>,
        audio_ms: u64,
        file_name: String,
        opts: &TranscribeOptions,
        ctx: &ChunkCtx<'_>,
    ) -> Result<(RawTranscript, &'static str)> {
        if (ctx.cancel)() {
            return Err(AsrError::Cancelled);
        }
        let now_ms = || job_started.elapsed().as_millis() as u64;
        let pace = pacer.wait_ms(now_ms(), audio_ms);
        if pace > 0 {
            if crate::retry::spill_to_local(pace, self.local_spillover.is_some()) {
                tracing::info!(
                    wait_secs = pace / 1000,
                    "cloud quota is full — decoding this chunk locally instead of waiting"
                );
                match self.decode_chunk_locally(&bytes, opts).await {
                    Ok(raw) => {
                        return Ok((raw, self.local_spillover.as_ref().unwrap().id()));
                    }
                    Err(AsrError::Cancelled) => return Err(AsrError::Cancelled),
                    // spillover is an optimization — its failure falls back
                    // to the plain quota wait, never sinks the chunk
                    Err(e) => {
                        tracing::warn!(error = %e, "local spillover failed — waiting for the cloud window instead");
                    }
                }
            }
            tracing::info!(
                wait_secs = pace / 1000,
                "pacing cloud upload to stay inside the free-tier audio quota"
            );
            (ctx.on_progress)(crate::TranscribeProgress {
                done_ms: ctx.done_ms,
                total_ms: ctx.total_ms,
                quota_wait_ms: Some(pace),
            });
            sleep_cancellable(pace, ctx.cancel).await?;
            // wait over — swap the waiting status back to plain progress
            (ctx.on_progress)(crate::TranscribeProgress {
                done_ms: ctx.done_ms,
                total_ms: ctx.total_ms,
                quota_wait_ms: None,
            });
        }

        let mut network_attempts = 0u32;
        let mut rate_limited_waited = std::time::Duration::ZERO;
        loop {
            pacer.record(now_ms(), audio_ms);
            let err = match self
                .transcribe_bytes(bytes.clone(), file_name.clone(), opts)
                .await
            {
                Ok(raw) => return Ok((raw, "groq")),
                Err(e) => e,
            };
            let Some(delay) = retry_delay(&err, network_attempts, rate_limited_waited) else {
                return Err(err);
            };
            match &err {
                AsrError::Network(_) => network_attempts += 1,
                AsrError::RateLimited { .. } => rate_limited_waited += delay,
                _ => {}
            }
            tracing::warn!(
                error = %err,
                retry_in_secs = delay.as_secs(),
                "cloud chunk upload failed — retrying"
            );
            sleep_cancellable(delay.as_millis() as u64, ctx.cancel).await?;
        }
    }

    /// Decode one already-prepared chunk with the injected local engine
    /// (quota spillover): the wav bytes go to a temp file, the local engine
    /// transcribes it, and its words come back UNCHANGED — still on the
    /// chunk's own timeline, exactly like a cloud response, so the caller's
    /// existing timeline mapping applies to both paths. The temp file never
    /// outlives the call.
    async fn decode_chunk_locally(
        &self,
        bytes: &[u8],
        opts: &TranscribeOptions,
    ) -> Result<RawTranscript> {
        let local = self
            .local_spillover
            .as_ref()
            .ok_or_else(|| AsrError::Engine("no local spillover engine injected".into()))?;
        let tmp =
            std::env::temp_dir().join(format!("flyonthewall-spill-{}.wav", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, bytes)?;
        let result = local.transcribe(&tmp, opts).await;
        let _ = std::fs::remove_file(&tmp);
        result
    }

    /// Upload one WAV payload and parse the transcription response.
    async fn transcribe_bytes(
        &self,
        bytes: Vec<u8>,
        file_name: String,
        opts: &TranscribeOptions,
    ) -> Result<RawTranscript> {
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
            .text("timestamp_granularities[]", "word")
            // greedy decode — matches whisper.cpp's default; sampling adds
            // transcript variance and helps hallucination loops take hold
            .text("temperature", "0");
        if let Some(lang) = &opts.language {
            form = form.text("language", lang.clone());
        }
        // The pipeline deliberately passes prompt: None: the local path runs
        // context-free (-mc 0) because carried text context is the fuel for
        // repetition hallucination, and prompt biasing was measured inert.
        // Don't synthesize a prompt here.
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
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp
            .text()
            .await
            .map_err(|e| AsrError::Network(e.to_string()))?;
        if !status.is_success() {
            // 429 → retryable RateLimited; other 4xx → permanent Rejected;
            // 5xx → Engine (the scheduler may retry the whole job).
            return Err(classify_http_failure(
                status.as_u16(),
                retry_after.as_deref(),
                &body,
            ));
        }
        parse_groq_verbose_json(&body)
    }
}

/// Split `total_ms` of audio into spans of at most `segment_ms`, each
/// overlapping the previous by `overlap_ms`. Returns `(start_ms, end_ms)`
/// spans covering the whole input.
fn plan_chunks(total_ms: u64, segment_ms: u64, overlap_ms: u64) -> Vec<(u64, u64)> {
    if total_ms <= segment_ms {
        return vec![(0, total_ms)];
    }
    let stride = segment_ms.saturating_sub(overlap_ms).max(1);
    let mut chunks = Vec::new();
    let mut start = 0u64;
    loop {
        let end = (start + segment_ms).min(total_ms);
        chunks.push((start, end));
        if end >= total_ms {
            return chunks;
        }
        start += stride;
    }
}

/// Offset each chunk's words to absolute time, then keep each overlap region
/// from exactly one chunk by cutting at the overlap's midpoint — words the
/// two chunks both transcribed appear once, and nothing is dropped.
fn stitch_chunk_words(chunks: &[(u64, u64)], per_chunk: Vec<Vec<Word>>) -> Vec<Word> {
    let mut out = Vec::new();
    for (i, words) in per_chunk.into_iter().enumerate() {
        let (start, end) = chunks[i];
        let keep_lo = if i == 0 {
            0
        } else {
            // overlap with the previous chunk is [start, prev_end)
            start + (chunks[i - 1].1 - start) / 2
        };
        let keep_hi = if i + 1 < chunks.len() {
            let next_start = chunks[i + 1].0;
            next_start + (end - next_start) / 2
        } else {
            u64::MAX
        };
        for w in words {
            let s = start + w.start_ms;
            if s >= keep_lo && s < keep_hi {
                out.push(Word {
                    text: w.text,
                    start_ms: s,
                    end_ms: start + w.end_ms,
                });
            }
        }
    }
    out
}

/// Encode mono f32 samples as an in-memory 16-bit PCM WAV for upload.
fn wav_bytes_mono_16(samples: &[f32], rate: u32) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec).expect("in-memory wav");
        for &s in samples {
            writer
                .write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .expect("in-memory wav");
        }
        writer.finalize().expect("in-memory wav");
    }
    cursor.into_inner()
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

    #[test]
    fn short_audio_is_a_single_chunk() {
        assert_eq!(plan_chunks(3_000, 4_000, 1_000), vec![(0, 3_000)]);
    }

    #[test]
    fn chunks_cover_everything_and_overlap() {
        let chunks = plan_chunks(10_000, 4_000, 1_000);
        assert_eq!(chunks, vec![(0, 4_000), (3_000, 7_000), (6_000, 10_000)]);
        // full coverage, each boundary shared by exactly two chunks
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].1 - pair[1].0, 1_000, "overlap must be 1s");
        }
        assert_eq!(chunks.last().unwrap().1, 10_000);
    }

    #[test]
    fn chunked_wav_slices_parse_back_at_expected_lengths() {
        // 10s of 16 kHz audio split with a 4s segment / 1s overlap: the
        // sliced WAV bytes must decode to exactly the planned spans.
        let rate = 16_000u32;
        let samples: Vec<f32> = (0..(10 * rate) as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.3)
            .collect();
        let chunks = plan_chunks(10_000, 4_000, 1_000);
        for &(start_ms, end_ms) in &chunks {
            let s = (start_ms * rate as u64 / 1000) as usize;
            let e = ((end_ms * rate as u64 / 1000) as usize).min(samples.len());
            let bytes = wav_bytes_mono_16(&samples[s..e], rate);
            let reader = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
            assert_eq!(reader.spec().sample_rate, rate);
            assert_eq!(reader.spec().channels, 1);
            assert_eq!(reader.len() as usize, e - s);
        }
    }

    fn word(text: &str, start_ms: u64) -> Word {
        Word {
            text: text.into(),
            start_ms,
            end_ms: start_ms + 200,
        }
    }

    #[test]
    fn stitch_offsets_word_timestamps_by_chunk_start() {
        let chunks = vec![(0, 4_000), (3_000, 7_000)];
        let stitched = stitch_chunk_words(
            &chunks,
            vec![vec![word("early", 500)], vec![word("late", 2_000)]],
        );
        assert_eq!(stitched.len(), 2);
        assert_eq!(stitched[0].start_ms, 500);
        assert_eq!(stitched[1].start_ms, 5_000); // 3_000 + 2_000
        assert_eq!(stitched[1].end_ms, 5_200);
    }

    #[test]
    fn stitch_keeps_overlap_words_from_exactly_one_chunk() {
        // Overlap [3_000, 4_000); midpoint cut at 3_500. Both chunks
        // transcribed the same word at absolute 3_200 and 3_700.
        let chunks = vec![(0, 4_000), (3_000, 7_000)];
        let stitched = stitch_chunk_words(
            &chunks,
            vec![
                vec![word("before", 3_200), word("after", 3_700)], // abs 3_200, 3_700
                vec![word("before", 200), word("after", 700)],     // abs 3_200, 3_700
            ],
        );
        let texts: Vec<_> = stitched.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, vec!["before", "after"], "each overlap word once");
        // "before" (< 3_500) came from chunk 0; "after" (>= 3_500) from chunk 1
        assert_eq!(stitched[0].start_ms, 3_200);
        assert_eq!(stitched[1].start_ms, 3_700);
    }

    #[test]
    fn oversized_span_words_shift_to_absolute_time() {
        // A contiguous span starting at 60s, chunked span-relative: stitched
        // words must land back on the original timeline.
        let chunks = plan_chunks(10_000, 4_000, 1_000);
        let stitched = stitch_chunk_words(
            &chunks,
            vec![
                vec![word("a", 500)],
                vec![word("b", 2_000)],
                vec![word("c", 3_000)],
            ],
        );
        let abs = offset_words(stitched, 60_000);
        assert_eq!(abs[0].start_ms, 60_500);
        assert_eq!(abs[1].start_ms, 65_000); // chunk1 start 3_000 + 2_000 + 60_000
        assert_eq!(abs[2].start_ms, 69_000); // chunk2 start 6_000 + 3_000 + 60_000
        assert_eq!(abs[2].end_ms, 69_200);
    }

    #[test]
    fn vad_stitched_word_timestamps_round_trip_to_original_timeline() {
        // Synthetic WAV layout: 10s silence, 2s speech, 20s silence, 3s
        // speech, 5s silence. Words transcribed on the stitched (silence-
        // stripped) audio must map back inside the original speech windows.
        use fly_audio::vad::{detect_speech_spans, map_to_original, stitch_spans, VadConfig};
        let rate = 16_000u32;
        let mut audio = vec![0.0f32; (rate as usize) * 10];
        let tone = |out: &mut Vec<f32>, secs: usize| {
            let start = out.len();
            out.extend((0..rate as usize * secs).map(|i| {
                ((start + i) as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.3
            }));
        };
        tone(&mut audio, 2); // speech A: 10s..12s
        audio.resize(audio.len() + rate as usize * 20, 0.0);
        tone(&mut audio, 3); // speech B: 32s..35s
        audio.resize(audio.len() + rate as usize * 5, 0.0);

        let spans = detect_speech_spans(&audio, rate, &VadConfig::default());
        assert_eq!(spans.len(), 2, "{spans:?}");
        let (stitched, map) = stitch_spans(&audio, rate, &spans).unwrap();
        // stitched audio holds only speech (+pads): far shorter than 40s
        assert!(stitched.len() < rate as usize * 7, "{}", stitched.len());

        // a "word" 1s into stitched audio lies inside speech A's window
        let w1 = map_to_original(1_000, &map);
        assert!((9_500..=12_500).contains(&w1), "w1={w1}");
        // a "word" just after span A's stitched length falls in speech B
        let span_a_len = map[0].len_ms;
        let w2 = map_to_original(span_a_len + 1_000, &map);
        assert!((31_500..=35_500).contains(&w2), "w2={w2}");
        // exact interior offsets are preserved sample-accurately
        assert_eq!(
            map_to_original(map[1].concat_start_ms, &map),
            spans[1].start_ms
        );
    }

    /// Progress parity with the local engine: a fully-checkpointed cloud run
    /// must emit the initial 0% and then one monotonic point per resumed
    /// batch (resumed = instantly done), ending at exactly the speech total —
    /// all without any network (the base_url here is unroutable). This is
    /// what lets the import queue map real per-file progress for cloud runs.
    #[tokio::test]
    async fn resumed_batches_emit_progress_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let rate = 16_000u32;
        let samples: Vec<f32> = (0..(2 * rate) as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.4)
            .collect();
        let wav = dir.path().join("track.16k.wav");
        fly_audio::mix::write_wav_mono_16(&wav, &samples, rate).unwrap();

        // the same plan the engine will compute
        let spans = detect_speech_spans(&samples, rate, &VadConfig::default());
        let segment_ms = MAX_CHUNK_BYTES / 2 * 1000 / rate as u64;
        let batches = plan_batches(&spans, segment_ms);
        assert!(!batches.is_empty(), "test wav must contain speech");
        let key = crate::checkpoint::plan_key("groq", GROQ_DEFAULT_MODEL, Some("en"), &batches);
        let mut ckpt = crate::checkpoint::CheckpointFile::open(
            crate::checkpoint::checkpoint_path_for(&wav),
            key,
        );
        for _ in &batches {
            ckpt.push(crate::checkpoint::BatchResult {
                language: Some("en".into()),
                words: vec![word("resumed", 100)],
                engine: Some("groq".into()),
            });
        }

        let engine = GroqEngine {
            resume: true,
            base_url: "http://127.0.0.1:9/unroutable".into(),
            ..GroqEngine::new("test-key".into())
        };
        let opts = TranscribeOptions {
            language: Some("en".into()),
            ..Default::default()
        };
        let events = std::sync::Mutex::new(Vec::<crate::TranscribeProgress>::new());
        let raw = engine
            .transcribe_with_progress(&wav, &opts, &|p| events.lock().unwrap().push(p), &|| false)
            .await
            .expect("fully-checkpointed cloud run must not touch the network");
        assert_eq!(raw.words.len(), batches.len());

        let events = events.into_inner().unwrap();
        let total: u64 = spans.iter().map(|s| s.end_ms - s.start_ms).sum();
        assert_eq!(
            events.first().map(|p| p.done_ms),
            Some(0),
            "initial 0% event before the first batch"
        );
        for w in events.windows(2) {
            assert!(
                w[1].done_ms > w[0].done_ms,
                "done_ms must strictly increase"
            );
        }
        assert_eq!(events.last().unwrap().done_ms, total, "must end at 100%");
        assert!(events
            .iter()
            .all(|p| p.total_ms == total && p.quota_wait_ms.is_none()));
    }

    /// Cancellation is honored between chunks: with cancel already requested,
    /// the engine returns Cancelled before attempting any upload (the
    /// base_url is unroutable — an upload attempt would surface differently).
    #[tokio::test]
    async fn cancel_between_chunks_aborts_without_uploading() {
        let dir = tempfile::tempdir().unwrap();
        let rate = 16_000u32;
        let samples: Vec<f32> = (0..(2 * rate) as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.4)
            .collect();
        let wav = dir.path().join("track.16k.wav");
        fly_audio::mix::write_wav_mono_16(&wav, &samples, rate).unwrap();

        let engine = GroqEngine {
            base_url: "http://127.0.0.1:9/unroutable".into(),
            ..GroqEngine::new("test-key".into())
        };
        let err = engine
            .transcribe_with_progress(&wav, &TranscribeOptions::default(), &|_| {}, &|| true)
            .await
            .unwrap_err();
        assert!(matches!(err, AsrError::Cancelled), "{err:?}");
    }

    /// Local spillover decodes one already-prepared chunk: the wav bytes go
    /// to a real temp file the local engine can read, the words come back
    /// unchanged (chunk-relative — the caller maps timelines), and the temp
    /// file is cleaned up afterwards.
    #[tokio::test]
    async fn spillover_decodes_chunk_bytes_with_the_local_engine() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        struct Shared(std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>);
        #[async_trait::async_trait]
        impl TranscriptionEngine for Shared {
            fn id(&self) -> &'static str {
                "whisper.cpp"
            }
            fn is_local(&self) -> bool {
                true
            }
            async fn transcribe(
                &self,
                wav_path: &Path,
                _opts: &TranscribeOptions,
            ) -> Result<RawTranscript> {
                let (samples, rate) = fly_audio::mix::read_wav_mono(wav_path).unwrap();
                assert_eq!(rate, 16_000);
                assert!(!samples.is_empty());
                *self.0.lock().unwrap() = Some(wav_path.to_path_buf());
                Ok(RawTranscript {
                    language: Some("en".into()),
                    words: vec![word("local", 250)],
                    segments: vec![],
                })
            }
        }

        let engine = GroqEngine {
            local_spillover: Some(Box::new(Shared(seen.clone()))),
            ..GroqEngine::new("test-key".into())
        };
        let samples: Vec<f32> = (0..16_000).map(|i| (i as f32 * 0.05).sin() * 0.4).collect();
        let bytes = wav_bytes_mono_16(&samples, 16_000);
        let raw = engine
            .decode_chunk_locally(&bytes, &TranscribeOptions::default())
            .await
            .expect("spillover decode must succeed");
        assert_eq!(raw.words.len(), 1);
        assert_eq!(raw.words[0].text, "local");
        assert_eq!(raw.words[0].start_ms, 250, "words stay chunk-relative");
        let path = seen.lock().unwrap().clone().expect("local engine ran");
        assert!(!path.exists(), "temp chunk wav must be cleaned up");
    }

    #[test]
    fn uploads_prefer_cuts_at_speech_gaps() {
        use fly_audio::vad::SpeechSpan;
        let s = |a: u64, b: u64| SpeechSpan {
            start_ms: a * 1000,
            end_ms: b * 1000,
        };
        // three spans, 200s speech total, 300s cap per upload: the first two
        // (150s) share an upload; the third starts a new one at the gap
        let batches = plan_batches(&[s(0, 100), s(150, 200), s(400, 500)], 160_000);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0], vec![s(0, 100), s(150, 200)]);
        assert_eq!(batches[1], vec![s(400, 500)]);
    }
}
