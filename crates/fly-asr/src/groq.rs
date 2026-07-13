//! Groq cloud ASR — FALLBACK ONLY (spec §6.2 "Cloud" tier). Audio leaves the
//! device when this engine runs; the UI must show a privacy notice before
//! enabling it. Returns no speaker labels — diarization still runs locally
//! and is merged with these word timestamps (spec §6.3).

use std::path::Path;

use fly_core::Word;

use crate::{AsrError, RawTranscript, Result, TranscribeOptions, TranscriptionEngine};

pub const GROQ_DEFAULT_MODEL: &str = "whisper-large-v3-turbo";

/// Groq rejects uploads over 25 MB with 413 on the free tier. Anything over
/// this threshold is transcribed in overlapping chunks and stitched.
const MAX_UPLOAD_BYTES: u64 = 20 * 1024 * 1024;
/// Per-chunk payload cap when splitting (16-bit mono WAV data), with headroom
/// under the 25 MB limit. ~9.8 min of 16 kHz audio.
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
        let size = std::fs::metadata(wav_path)?.len();
        if size <= MAX_UPLOAD_BYTES {
            let bytes = std::fs::read(wav_path)?;
            let file_name = wav_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "audio.wav".into());
            return self.transcribe_bytes(bytes, file_name, opts).await;
        }

        // Oversized recording (~13+ min at 16 kHz): transcribe in overlapping
        // chunks, offset each chunk's word timestamps by its start, and keep
        // every overlap region from exactly one chunk.
        let (samples, rate) = fly_audio::mix::read_wav_mono(wav_path)
            .map_err(|e| AsrError::BadAudio(e.to_string()))?;
        let total_ms = samples.len() as u64 * 1000 / rate as u64;
        let segment_ms = MAX_CHUNK_BYTES / 2 * 1000 / rate as u64;
        let chunks = plan_chunks(total_ms, segment_ms, OVERLAP_MS);
        tracing::info!(
            bytes = size,
            chunks = chunks.len(),
            total_ms,
            "recording exceeds groq upload limit — transcribing in chunks"
        );

        let mut language = None;
        let mut per_chunk: Vec<Vec<Word>> = Vec::new();
        for (i, &(start_ms, end_ms)) in chunks.iter().enumerate() {
            let s = (start_ms * rate as u64 / 1000) as usize;
            let e = ((end_ms * rate as u64 / 1000) as usize).min(samples.len());
            let bytes = wav_bytes_mono_16(&samples[s..e], rate);
            let raw = self
                .transcribe_bytes(bytes, format!("chunk-{i}.wav"), opts)
                .await?;
            language = language.or(raw.language);
            per_chunk.push(raw.words);
        }

        Ok(RawTranscript {
            language,
            words: stitch_chunk_words(&chunks, per_chunk),
            segments: vec![],
        })
    }
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
}
