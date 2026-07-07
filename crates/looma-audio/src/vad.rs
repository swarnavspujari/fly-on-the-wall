//! Adaptive energy VAD + speech stitching for ASR preprocessing.
//!
//! Long silence reaching a Whisper decoder is how hallucination loops start
//! (a real meeting produced one phrase 1881×). The floor is estimated per
//! recording because "silence" differs wildly by channel: a mic channel's
//! noise floor sits around −41 dBFS while a system-loopback channel is
//! digitally silent (−80 dBFS and below). A fixed threshold would classify
//! an entire mic channel as speech.

use crate::{AudioError, Result};

#[derive(Debug, Clone)]
pub struct VadConfig {
    /// Analysis frame length.
    pub frame_ms: u32,
    /// Speech threshold sits this far above the estimated noise floor.
    pub margin_db: f32,
    /// Threshold clamp: never require quieter than this…
    pub threshold_min_db: f32,
    /// …and never louder than this.
    pub threshold_max_db: f32,
    /// Consecutive above-threshold frames required to enter speech
    /// (rejects sub-100 ms clicks).
    pub enter_frames: usize,
    /// Silence run that ends a speech span.
    pub min_silence_ms: u32,
    /// Context kept around each span so word onsets/offsets survive.
    pub pad_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            frame_ms: 100,
            margin_db: 8.0,
            threshold_min_db: -55.0,
            threshold_max_db: -25.0,
            enter_frames: 2,
            min_silence_ms: 1_200,
            pad_ms: 250,
        }
    }
}

/// A detected speech region, in milliseconds from the start of the audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechSpan {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Detect speech spans with a per-recording adaptive threshold: noise floor =
/// 10th-percentile frame energy, speech = floor + margin (clamped). Spans are
/// padded and overlapping pads merged.
pub fn detect_speech_spans(samples: &[f32], rate: u32, cfg: &VadConfig) -> Vec<SpeechSpan> {
    let frame_len = (rate as u64 * cfg.frame_ms as u64 / 1000).max(1) as usize;
    let frames: Vec<f32> = samples
        .chunks(frame_len)
        .map(|frame| {
            let mean_sq = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
            let rms = mean_sq.sqrt().max(1e-9);
            20.0 * rms.log10()
        })
        .collect();
    if frames.is_empty() {
        return Vec::new();
    }

    let mut sorted = frames.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let floor = sorted[sorted.len() / 10];
    let threshold = (floor + cfg.margin_db).clamp(cfg.threshold_min_db, cfg.threshold_max_db);

    let frame_ms = cfg.frame_ms as u64;
    let total_ms = samples.len() as u64 * 1000 / rate as u64;
    let silence_frames = (cfg.min_silence_ms / cfg.frame_ms).max(1) as usize;
    let enter = cfg.enter_frames.max(1);

    let mut spans: Vec<SpeechSpan> = Vec::new();
    let mut speech_since: Option<usize> = None;
    let mut silence_run = 0usize;
    for i in 0..frames.len() {
        let loud = frames[i] >= threshold;
        match speech_since {
            None => {
                let entering = loud
                    && (0..enter).all(|k| frames.get(i + k).is_some_and(|f| *f >= threshold));
                if entering {
                    speech_since = Some(i);
                    silence_run = 0;
                }
            }
            Some(start) => {
                if loud {
                    silence_run = 0;
                } else {
                    silence_run += 1;
                    if silence_run >= silence_frames {
                        let end = i + 1 - silence_run;
                        push_padded(&mut spans, start, end, frame_ms, cfg.pad_ms as u64, total_ms);
                        speech_since = None;
                    }
                }
            }
        }
    }
    if let Some(start) = speech_since {
        let end = frames.len() - silence_run;
        push_padded(&mut spans, start, end, frame_ms, cfg.pad_ms as u64, total_ms);
    }
    spans
}

/// Convert a frame range to padded milliseconds and append, merging with the
/// previous span when the pads touch.
fn push_padded(
    spans: &mut Vec<SpeechSpan>,
    start_frame: usize,
    end_frame: usize,
    frame_ms: u64,
    pad_ms: u64,
    total_ms: u64,
) {
    let start = (start_frame as u64 * frame_ms).saturating_sub(pad_ms);
    let end = (end_frame as u64 * frame_ms + pad_ms).min(total_ms);
    if end <= start {
        return;
    }
    match spans.last_mut() {
        Some(prev) if start <= prev.end_ms => prev.end_ms = prev.end_ms.max(end),
        _ => spans.push(SpeechSpan {
            start_ms: start,
            end_ms: end,
        }),
    }
}

/// One stitched region: where a span landed in the concatenated audio and
/// where it came from in the original timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StitchMap {
    pub concat_start_ms: u64,
    pub orig_start_ms: u64,
    pub len_ms: u64,
}

/// Concatenate the spans' samples (silence between spans is dropped) and
/// return the mapping needed to translate timestamps back.
pub fn stitch_spans(
    samples: &[f32],
    rate: u32,
    spans: &[SpeechSpan],
) -> Result<(Vec<f32>, Vec<StitchMap>)> {
    let mut stitched = Vec::new();
    let mut map = Vec::with_capacity(spans.len());
    let mut concat_ms = 0u64;
    for span in spans {
        let from = (span.start_ms * rate as u64 / 1000) as usize;
        let to = (span.end_ms * rate as u64 / 1000) as usize;
        if span.end_ms < span.start_ms || to > samples.len() {
            return Err(AudioError::Backend(format!(
                "speech span {}..{} ms outside audio ({} samples @ {rate} Hz)",
                span.start_ms,
                span.end_ms,
                samples.len()
            )));
        }
        let len_ms = span.end_ms - span.start_ms;
        stitched.extend_from_slice(&samples[from..to]);
        map.push(StitchMap {
            concat_start_ms: concat_ms,
            orig_start_ms: span.start_ms,
            len_ms,
        });
        concat_ms += len_ms;
    }
    Ok((stitched, map))
}

/// Translate a timestamp in stitched audio back to the original timeline.
/// Timestamps past the stitched end clamp to the last span's end.
pub fn map_to_original(concat_ms: u64, map: &[StitchMap]) -> u64 {
    for m in map {
        if concat_ms < m.concat_start_ms + m.len_ms {
            let into = concat_ms.saturating_sub(m.concat_start_ms);
            return m.orig_start_ms + into;
        }
    }
    map.last().map(|m| m.orig_start_ms + m.len_ms).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    /// Sine amplitude that yields the given RMS dBFS.
    fn amp_for_db(db: f32) -> f32 {
        10f32.powf(db / 20.0) * std::f32::consts::SQRT_2
    }

    /// Append `ms` of a 440 Hz tone at the given RMS level.
    fn push_tone(out: &mut Vec<f32>, ms: u64, rms_db: f32) {
        let amp = amp_for_db(rms_db);
        let n = (RATE as u64 * ms / 1000) as usize;
        let start = out.len();
        out.extend((0..n).map(|i| {
            let t = (start + i) as f32 / RATE as f32;
            (t * 440.0 * std::f32::consts::TAU).sin() * amp
        }));
    }

    fn push_silence(out: &mut Vec<f32>, ms: u64) {
        out.extend(std::iter::repeat(0.0).take((RATE as u64 * ms / 1000) as usize));
    }

    fn spans_of(samples: &[f32]) -> Vec<SpeechSpan> {
        detect_speech_spans(samples, RATE, &VadConfig::default())
    }

    #[test]
    fn pure_silence_has_no_spans() {
        let mut audio = Vec::new();
        push_silence(&mut audio, 10_000);
        assert!(spans_of(&audio).is_empty());
    }

    #[test]
    fn burst_in_digital_silence_is_found_with_padding() {
        let mut audio = Vec::new();
        push_silence(&mut audio, 5_000);
        push_tone(&mut audio, 2_000, -20.0);
        push_silence(&mut audio, 5_000);
        let spans = spans_of(&audio);
        assert_eq!(spans.len(), 1);
        let s = spans[0];
        // starts near 5000-pad, ends near 7000+pad (frame quantization slack)
        assert!(s.start_ms >= 4_600 && s.start_ms <= 4_900, "start={}", s.start_ms);
        assert!(s.end_ms >= 7_100 && s.end_ms <= 7_500, "end={}", s.end_ms);
    }

    #[test]
    fn threshold_adapts_to_loud_noise_floor() {
        // the mic-channel scenario: constant -40 dB floor, speech at -15 dB
        let mut audio = Vec::new();
        push_tone(&mut audio, 8_000, -40.0);
        push_tone(&mut audio, 2_000, -15.0);
        push_tone(&mut audio, 8_000, -40.0);
        let spans = spans_of(&audio);
        assert_eq!(spans.len(), 1, "floor must not read as speech: {spans:?}");
        let s = spans[0];
        assert!(s.start_ms >= 7_600 && s.start_ms <= 8_000, "start={}", s.start_ms);
        assert!(s.end_ms >= 10_000 && s.end_ms <= 10_400, "end={}", s.end_ms);
    }

    #[test]
    fn short_gap_merges_long_gap_splits() {
        let mut audio = Vec::new();
        push_silence(&mut audio, 3_000);
        push_tone(&mut audio, 1_000, -20.0);
        push_silence(&mut audio, 800); // < min_silence_ms
        push_tone(&mut audio, 1_000, -20.0);
        push_silence(&mut audio, 4_000); // > min_silence_ms
        push_tone(&mut audio, 1_000, -20.0);
        push_silence(&mut audio, 3_000);
        let spans = spans_of(&audio);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!(spans[0].end_ms < spans[1].start_ms);
    }

    #[test]
    fn sub_frame_click_is_ignored() {
        let mut audio = Vec::new();
        push_silence(&mut audio, 4_000);
        push_tone(&mut audio, 90, -10.0); // one loud frame at most
        push_silence(&mut audio, 4_000);
        assert!(spans_of(&audio).is_empty());
    }

    #[test]
    fn speech_from_sample_zero_starts_at_zero() {
        let mut audio = Vec::new();
        push_tone(&mut audio, 1_500, -20.0);
        push_silence(&mut audio, 4_000);
        let spans = spans_of(&audio);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start_ms, 0);
    }

    #[test]
    fn stitch_concatenates_and_maps_back() {
        let mut audio = Vec::new();
        push_silence(&mut audio, 10_000);
        push_tone(&mut audio, 2_000, -20.0); // span A: 10s..12s
        push_silence(&mut audio, 20_000);
        push_tone(&mut audio, 3_000, -20.0); // span B: 32s..35s
        push_silence(&mut audio, 5_000);

        let spans = vec![
            SpeechSpan { start_ms: 10_000, end_ms: 12_000 },
            SpeechSpan { start_ms: 32_000, end_ms: 35_000 },
        ];
        let (stitched, map) = stitch_spans(&audio, RATE, &spans).unwrap();
        assert_eq!(stitched.len(), (RATE as usize) * 5); // 2s + 3s
        assert_eq!(map.len(), 2);
        assert_eq!(map[0], StitchMap { concat_start_ms: 0, orig_start_ms: 10_000, len_ms: 2_000 });
        assert_eq!(map[1], StitchMap { concat_start_ms: 2_000, orig_start_ms: 32_000, len_ms: 3_000 });

        // timestamps map back into the right span
        assert_eq!(map_to_original(0, &map), 10_000);
        assert_eq!(map_to_original(1_500, &map), 11_500);
        assert_eq!(map_to_original(2_000, &map), 32_000);
        assert_eq!(map_to_original(4_999, &map), 34_999);
        // past the end clamps to the last span's end
        assert_eq!(map_to_original(6_000, &map), 35_000);
    }

    #[test]
    fn stitch_rejects_span_past_audio_end() {
        let mut audio = Vec::new();
        push_silence(&mut audio, 1_000);
        let spans = vec![SpeechSpan { start_ms: 0, end_ms: 5_000 }];
        assert!(stitch_spans(&audio, RATE, &spans).is_err());
    }
}
