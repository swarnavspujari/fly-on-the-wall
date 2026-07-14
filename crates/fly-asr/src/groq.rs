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
        // Same preprocessing as the local whisper.cpp path: VAD strips long
        // non-speech (silence reaching a Whisper decoder is how repetition
        // hallucination starts — and it's wasted upload bytes), each payload
        // is peak-normalized, and word timestamps are mapped back to the
        // original timeline so local diarization alignment stays correct.
        let (samples, rate) = fly_audio::mix::read_wav_mono(wav_path)
            .map_err(|e| AsrError::BadAudio(format!("{}: {e}", wav_path.display())))?;
        let spans = detect_speech_spans(&samples, rate, &VadConfig::default());
        let segment_ms = MAX_CHUNK_BYTES / 2 * 1000 / rate as u64;
        tracing::debug!(
            file = %wav_path.display(),
            spans = spans.len(),
            speech_ms = spans.iter().map(|s| s.end_ms - s.start_ms).sum::<u64>(),
            total_ms = samples.len() as u64 * 1000 / rate.max(1) as u64,
            "vad segmentation (groq)"
        );

        let mut language = None;
        let mut words = Vec::new();
        // Uploads are cut at speech-span gaps (plan_batches). Only a single
        // contiguous speech span longer than the payload cap falls back to
        // fixed overlapping chunks stitched at the overlap midpoint.
        for (bi, batch) in plan_batches(&spans, segment_ms).into_iter().enumerate() {
            let oversized = batch.len() == 1 && batch[0].end_ms - batch[0].start_ms > segment_ms;
            if oversized {
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
                    let raw = self
                        .transcribe_bytes(
                            wav_bytes_mono_16(&span_samples[s..e], rate),
                            format!("speech-{bi}-{i}.wav"),
                            opts,
                        )
                        .await?;
                    language = language.or(raw.language);
                    per_chunk.push(raw.words);
                }
                // stitched timestamps are relative to the span; shift to
                // absolute original-file time
                words.extend(offset_words(
                    stitch_chunk_words(&chunks, per_chunk),
                    span.start_ms,
                ));
            } else {
                let (mut chunk, map) = stitch_spans(&samples, rate, &batch)
                    .map_err(|e| AsrError::BadAudio(e.to_string()))?;
                fly_audio::mix::normalize_peak(
                    &mut chunk,
                    NORMALIZE_TARGET_PEAK,
                    NORMALIZE_MAX_GAIN,
                );
                let raw = self
                    .transcribe_bytes(
                        wav_bytes_mono_16(&chunk, rate),
                        format!("speech-{bi}.wav"),
                        opts,
                    )
                    .await?;
                language = language.or(raw.language);
                words.extend(raw.words.into_iter().map(|mut w| {
                    w.start_ms = map_to_original(w.start_ms, &map);
                    w.end_ms = map_to_original(w.end_ms, &map);
                    w
                }));
            }
        }

        Ok(RawTranscript {
            language: language.or_else(|| opts.language.clone()),
            words,
            segments: vec![],
        })
    }
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
        let body = resp
            .text()
            .await
            .map_err(|e| AsrError::Network(e.to_string()))?;
        if status.is_client_error() {
            // 4xx (413 payload too large, 401 bad key, …): the identical
            // request can never succeed — callers must not retry it.
            return Err(AsrError::Rejected(format!(
                "groq returned {status}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        if !status.is_success() {
            return Err(AsrError::Engine(format!(
                "groq returned {status}: {}",
                body.chars().take(300).collect::<String>()
            )));
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
