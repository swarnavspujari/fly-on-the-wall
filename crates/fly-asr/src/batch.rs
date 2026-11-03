//! The VAD-batched decode loop shared by the local sidecar engines
//! (whisper.cpp, parakeet): adaptive VAD splits the recording into speech
//! spans (long silence/noise never reaches a decoder), spans are grouped
//! into ≤ [`MAX_BATCH_SPEECH_MS`] batches, each batch is stitched,
//! peak-normalized and handed to the engine's `decode`, and word timestamps
//! are mapped back to the original timeline. Checkpointing, progress and
//! cooperative cancellation live here so every engine gets them identically.
//! Public so the accuracy harness's bench-only engines chunk audio exactly
//! like the shipped ones.

use std::future::Future;
use std::path::Path;

use fly_audio::vad::{detect_speech_spans, map_to_original, stitch_spans, SpeechSpan, VadConfig};
use fly_core::Word;

use crate::whisper_cpp::{
    cumulative_progress, plan_batches, NORMALIZE_MAX_GAIN, NORMALIZE_TARGET_PEAK,
};
use crate::{AsrError, CancelFn, RawTranscript, Result, TranscribeOptions, TranscribeProgressFn};

/// Cap on the speech content of one sidecar invocation. Batching many short
/// spans into one call amortizes model load; the cap keeps single
/// invocations bounded (and inside Parakeet's ~400 s encoder context).
pub const MAX_BATCH_SPEECH_MS: u64 = 120_000;

/// Run `decode` over every speech batch of `wav_path`. `decode` gets the
/// normalized batch samples and their rate and returns words timed relative
/// to those samples. `engine_id` + the model's file name key the checkpoint,
/// so a resumed run never mixes batches from a different engine or model.
/// `max_span_ms` additionally cuts any single speech span longer than that
/// at its quietest point (engines whose decoder has a hard context limit);
/// `None` keeps contiguous speech whole, as whisper always has.
#[allow(clippy::too_many_arguments)]
pub async fn transcribe_batched<F, Fut>(
    engine_id: &str,
    model: &Path,
    wav_path: &Path,
    opts: &TranscribeOptions,
    resume: bool,
    max_span_ms: Option<u64>,
    on_progress: TranscribeProgressFn<'_>,
    cancel: CancelFn<'_>,
    decode: F,
) -> Result<RawTranscript>
where
    F: Fn(Vec<f32>, u32) -> Fut,
    Fut: Future<Output = Result<RawTranscript>>,
{
    if !model.exists() {
        return Err(AsrError::ModelMissing(model.display().to_string()));
    }
    if !wav_path.exists() {
        return Err(AsrError::BadAudio(wav_path.display().to_string()));
    }

    let (samples, rate) = fly_audio::mix::read_wav_mono(wav_path)
        .map_err(|e| AsrError::BadAudio(format!("{}: {e}", wav_path.display())))?;
    let mut spans = detect_speech_spans(&samples, rate, &VadConfig::default());
    if let Some(max_ms) = max_span_ms {
        spans = split_long_spans(&spans, &samples, rate, max_ms);
    }
    let total_speech_ms: u64 = spans.iter().map(|s| s.end_ms - s.start_ms).sum();
    tracing::debug!(
        engine = engine_id,
        file = %wav_path.file_name().unwrap_or_default().to_string_lossy(),
        spans = spans.len(),
        speech_ms = total_speech_ms,
        total_ms = samples.len() as u64 * 1000 / rate.max(1) as u64,
        "vad segmentation"
    );

    let mut words = Vec::new();
    let mut language = None;
    let batches = plan_batches(&spans, MAX_BATCH_SPEECH_MS);
    let mut ckpt = resume.then(|| {
        let model_tag = model
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        crate::checkpoint::CheckpointFile::open(
            crate::checkpoint::checkpoint_path_for(wav_path),
            crate::checkpoint::plan_key(engine_id, &model_tag, opts.language.as_deref(), &batches),
        )
    });
    let resumed = ckpt.as_ref().map(|c| c.resumed()).unwrap_or_default();
    if !resumed.is_empty() {
        tracing::info!(
            engine = engine_id,
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
        let (mut chunk, map) =
            stitch_spans(&samples, rate, batch).map_err(|e| AsrError::BadAudio(e.to_string()))?;
        fly_audio::mix::normalize_peak(&mut chunk, NORMALIZE_TARGET_PEAK, NORMALIZE_MAX_GAIN);
        let raw = decode(chunk, rate).await?;
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
                engine: Some(engine_id.to_string()),
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

/// Analysis frame for picking a cut point inside an over-long span.
const CUT_FRAME_MS: u64 = 20;
/// Shortest piece a split may leave behind.
const MIN_PIECE_MS: u64 = 1_000;

/// Cut every span longer than `max_ms` into pieces of at most `max_ms`,
/// each cut placed at the lowest-energy 20 ms frame in the last quarter of
/// the allowed length — inside a breath or pause wherever the speech has
/// one, so a cut rarely lands mid-word. Shorter spans pass through as-is.
pub fn split_long_spans(
    spans: &[SpeechSpan],
    samples: &[f32],
    rate: u32,
    max_ms: u64,
) -> Vec<SpeechSpan> {
    let per_ms = rate as u64 / 1000;
    let energy = |from_ms: u64| -> f64 {
        let a = ((from_ms * per_ms) as usize).min(samples.len());
        let b = (((from_ms + CUT_FRAME_MS) * per_ms) as usize).min(samples.len());
        samples[a..b]
            .iter()
            .map(|x| (*x as f64) * (*x as f64))
            .sum()
    };
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let mut start = span.start_ms;
        while span.end_ms - start > max_ms {
            let lo = start + max_ms * 3 / 4;
            // never leave a sliver: the piece after the cut keeps at least
            // MIN_PIECE_MS (a ~15 ms tail would otherwise reach the engine
            // on its own)
            let hi = (start + max_ms - CUT_FRAME_MS).min(span.end_ms.saturating_sub(MIN_PIECE_MS));
            let hi = hi.max(lo);
            let mut best = (hi, f64::INFINITY);
            let mut at = lo;
            while at <= hi {
                let e = energy(at);
                if e < best.1 {
                    best = (at, e);
                }
                at += CUT_FRAME_MS;
            }
            let cut = best.0 + CUT_FRAME_MS / 2;
            out.push(SpeechSpan {
                start_ms: start,
                end_ms: cut,
            });
            start = cut;
        }
        out.push(SpeechSpan {
            start_ms: start,
            end_ms: span.end_ms,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start_ms: u64, end_ms: u64) -> SpeechSpan {
        SpeechSpan { start_ms, end_ms }
    }

    #[test]
    fn short_spans_are_untouched() {
        let spans = vec![span(0, 5_000), span(9_000, 19_000)];
        assert_eq!(split_long_spans(&spans, &[], 16_000, 10_000), spans);
    }

    /// A 25 s span with a quiet dip at 8.5 s is cut there (inside the last
    /// quarter of the 10 s cap), and every piece respects the cap and the
    /// pieces tile the original span exactly.
    #[test]
    fn long_span_is_cut_at_the_quiet_point() {
        let rate = 16_000u32;
        let mut samples = vec![0.5f32; 25 * rate as usize];
        let dip = (8_500 * 16) as usize;
        samples[dip..dip + 320].fill(0.0);
        let pieces = split_long_spans(&[span(0, 25_000)], &samples, rate, 10_000);
        assert_eq!(pieces[0], span(0, 8_510));
        assert!(pieces.iter().all(|p| p.end_ms - p.start_ms <= 10_000));
        assert_eq!(pieces.first().unwrap().start_ms, 0);
        assert_eq!(pieces.last().unwrap().end_ms, 25_000);
        assert!(pieces.windows(2).all(|w| w[0].end_ms == w[1].start_ms));
    }

    /// A span just over the cap (cap + 15 ms) splits into two real pieces,
    /// not a full piece plus a 15 ms sliver.
    #[test]
    fn no_sliver_tail() {
        let rate = 16_000u32;
        let samples = vec![0.5f32; 11 * rate as usize];
        let pieces = split_long_spans(&[span(0, 10_015)], &samples, rate, 10_000);
        assert_eq!(pieces.len(), 2);
        assert!(pieces
            .iter()
            .all(|p| p.end_ms - p.start_ms >= MIN_PIECE_MS - CUT_FRAME_MS));
        assert!(pieces.iter().all(|p| p.end_ms - p.start_ms <= 10_000));
    }
}
