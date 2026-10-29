//! A marker tone for measuring capture gaps end to end (the contention
//! bench, `src-tauri/tests/contention_bench.rs`): play it through the system
//! output while recording, then check the captured system channel against it.
//!
//! The tone is a sine whose amplitude alternates between two levels every
//! `step_s`. Each level change is a timing marker, so the captured file can
//! be checked two ways:
//! - **padded gaps:** runs of exact digital zeros inside the tone (the
//!   recorder fills lost audio with silence) — exact to the sample;
//! - **removed / inserted audio:** two consecutive markers that are not
//!   exactly `step_s` apart (audio dropped without padding, or extra
//!   silence) — accurate to about a millisecond.
//!
//! At 1 kHz and 48 kHz a period is exactly 48 samples, and level changes land
//! on zero crossings, so the tone has no clicks and a one-period RMS window has
//! no ripple.

use std::f64::consts::TAU;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::Serialize;

use crate::{AudioError, Result};

/// What the player plays and the analyzer expects.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ToneSpec {
    pub freq_hz: u32,
    /// Peak amplitude (0..1 full scale) on even steps…
    pub hi: f32,
    /// …and on odd steps.
    pub lo: f32,
    pub step_s: u32,
}

impl Default for ToneSpec {
    /// Quiet on purpose: about -30 dBFS / -36 dBFS.
    fn default() -> Self {
        Self {
            freq_hz: 1000,
            hi: 0.03,
            lo: 0.015,
            step_s: 1,
        }
    }
}

impl ToneSpec {
    /// Sample `n` (frame index from the tone's start) at `rate`.
    pub fn sample(&self, n: u64, rate: u32) -> f32 {
        let step = (n / (self.step_s as u64 * rate as u64)) % 2;
        let amp = if step == 0 { self.hi } else { self.lo };
        // integer phase keeps hours-long tones exact
        let phase = (self.freq_hz as u64 * n) % rate as u64;
        amp * (TAU * phase as f64 / rate as f64).sin() as f32
    }
}

/// Plays a [`ToneSpec`] on the default output device until dropped.
pub struct TonePlayer {
    _stream: cpal::Stream,
    frames: Arc<AtomicU64>,
    max_gap_us: Arc<AtomicU64>,
    rate: u32,
}

impl TonePlayer {
    pub fn start(spec: ToneSpec) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| AudioError::DeviceNotFound("default output".into()))?;
        let config = device
            .default_output_config()
            .map_err(|e| AudioError::Backend(e.to_string()))?;
        let mut stream_config: cpal::StreamConfig = config.config();
        let rate = stream_config.sample_rate;
        let channels = stream_config.channels.max(1) as usize;
        // A deep render buffer: under load the player itself must not glitch,
        // or its gaps would be blamed on the recorder.
        stream_config.buffer_size = cpal::BufferSize::Fixed(rate / 2);
        let frames = Arc::new(AtomicU64::new(0));
        let max_gap_us = Arc::new(AtomicU64::new(0));

        macro_rules! build_for {
            ($t:ty) => {{
                let frames = frames.clone();
                let max_gap_us = max_gap_us.clone();
                let mut last: Option<Instant> = None;
                device
                    .build_output_stream(
                        stream_config.clone(),
                        move |data: &mut [$t], _| {
                            let now = Instant::now();
                            if let Some(prev) = last {
                                max_gap_us.fetch_max(
                                    now.duration_since(prev).as_micros() as u64,
                                    Ordering::Relaxed,
                                );
                            }
                            last = Some(now);
                            let mut n = frames.load(Ordering::Relaxed);
                            for frame in data.chunks_mut(channels) {
                                let v = spec.sample(n, rate);
                                for s in frame.iter_mut() {
                                    *s = <$t as cpal::FromSample<f32>>::from_sample_(v);
                                }
                                n += 1;
                            }
                            frames.store(n, Ordering::Relaxed);
                        },
                        |e| tracing::warn!("tone player: {e}"),
                        None,
                    )
                    .map_err(|e| AudioError::Backend(e.to_string()))?
            }};
        }
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => build_for!(f32),
            cpal::SampleFormat::I16 => build_for!(i16),
            other => {
                return Err(AudioError::Backend(format!(
                    "tone player: unsupported sample format {other:?}"
                )))
            }
        };
        stream
            .play()
            .map_err(|e| AudioError::Backend(e.to_string()))?;
        Ok(Self {
            _stream: stream,
            frames,
            max_gap_us,
            rate,
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn frames_played(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// Longest pause between two render callbacks — a value near the
    /// half-second render buffer means the player itself may have glitched.
    pub fn max_callback_gap_ms(&self) -> f64 {
        self.max_gap_us.load(Ordering::Relaxed) as f64 / 1000.0
    }
}

/// One discontinuity found in a captured tone.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToneGap {
    /// Position in the captured file, seconds.
    pub at_s: f64,
    /// Length, milliseconds.
    pub ms: f64,
    /// `silence` (digital zeros inside the tone), `removed` (markers closer
    /// than `step_s`: audio dropped without padding) or `inserted` (markers
    /// further apart than `step_s`, with no zeros to explain it).
    pub kind: &'static str,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ToneReport {
    /// First non-zero sample (the head is the recorder's start-up padding).
    pub tone_start_s: f64,
    /// Length of the analysed region.
    pub analysed_s: f64,
    /// Level changes found.
    pub markers: usize,
    /// Largest marker-spacing error that stayed under the gap threshold, ms
    /// (measurement noise; should be well under 1 ms).
    pub max_marker_jitter_ms: f64,
    /// Captured hi/lo level ratio (the player's is `hi / lo`; much lower
    /// means the tone wasn't found and `markers` stays 0).
    pub level_ratio: f32,
    pub gaps: Vec<ToneGap>,
}

impl ToneReport {
    pub fn gaps_longer_than(&self, ms: f64) -> usize {
        self.gaps.iter().filter(|g| g.ms > ms).count()
    }
}

/// Shortest zero run counted as padding (1 ms): a sine has at most a couple
/// of consecutive zero samples.
const MIN_ZERO_RUN_MS: f64 = 1.0;
/// Marker spacing errors above this are gaps.
const SPACING_TOLERANCE_MS: f64 = 2.0;

/// Find gaps in a captured [`ToneSpec`] (mono i16 at `rate`, the recorder's
/// system-channel format).
pub fn analyze(samples: &[i16], rate: u32, spec: &ToneSpec) -> ToneReport {
    let Some(first) = samples.iter().position(|&s| s != 0) else {
        return ToneReport::default();
    };
    let last = samples.iter().rposition(|&s| s != 0).unwrap_or(first);
    let region = &samples[first..=last];
    let to_s = |i: usize| (first + i) as f64 / rate as f64;
    let mut report = ToneReport {
        tone_start_s: first as f64 / rate as f64,
        analysed_s: region.len() as f64 / rate as f64,
        ..Default::default()
    };

    // --- padded gaps: zero runs strictly inside the tone ---
    let min_run = ((rate as f64 * MIN_ZERO_RUN_MS / 1000.0) as usize).max(2);
    let mut zero_runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < region.len() {
        if region[i] == 0 {
            let start = i;
            while i < region.len() && region[i] == 0 {
                i += 1;
            }
            if i - start >= min_run {
                zero_runs.push((start, i - start));
            }
        } else {
            i += 1;
        }
    }
    for &(start, len) in &zero_runs {
        report.gaps.push(ToneGap {
            at_s: to_s(start),
            ms: len as f64 * 1000.0 / rate as f64,
            kind: "silence",
        });
    }

    // --- markers: one-period RMS crossing the midpoint between levels ---
    let period = (rate as f64 / spec.freq_hz as f64).round().max(1.0) as usize;
    let in_zero_run = |i: usize| zero_runs.iter().any(|&(s, l)| i + period > s && i < s + l);
    // The capture's gain isn't the player's (system volume, loopback
    // scaling), so learn the two levels from the recording: the 10th and
    // 90th percentile of whole-period RMS.
    let mut levels: Vec<f32> = region
        .chunks_exact(period)
        .map(|c| {
            let e: f64 = c.iter().map(|&s| (s as f64) * (s as f64)).sum();
            (e / period as f64).sqrt() as f32
        })
        .filter(|&r| r > 0.0)
        .collect();
    if levels.len() < 4 {
        return report;
    }
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lo_level = levels[levels.len() / 10];
    let hi_level = levels[levels.len() * 9 / 10];
    report.level_ratio = hi_level / lo_level.max(1e-6);
    let expected_ratio = spec.hi / spec.lo;
    if report.level_ratio < 1.0 + (expected_ratio - 1.0) / 2.0 {
        // no two-level tone in here (wrong signal, or heavy processing)
        return report;
    }
    let mid = (hi_level + lo_level) / 2.0;
    let floor = lo_level / 4.0;
    // running sum of squares over one period
    let mut markers: Vec<usize> = Vec::new();
    let mut state: Option<bool> = None; // Some(true) = hi
                                        // a level change first seen right after a skipped stretch could have
                                        // happened anywhere inside it: not usable as a timing marker
    let mut after_skip = false;
    let mut acc: f64 = 0.0;
    for (i, &s) in region.iter().enumerate() {
        acc += (s as f64) * (s as f64);
        if i >= period {
            let old = region[i - period] as f64;
            acc -= old * old;
        }
        if i + 1 < period {
            continue;
        }
        let start = i + 1 - period;
        let rms = (acc.max(0.0) / period as f64).sqrt() as f32;
        if rms < floor || in_zero_run(start) {
            after_skip = true;
            continue;
        }
        let hi = rms > mid;
        if state.is_some_and(|prev| prev != hi) && !after_skip {
            // the window [start, i] straddles the step; the crossing sits a
            // fixed offset from it for both directions, so spacing is exact
            markers.push(start + period / 2);
        }
        after_skip = false;
        state = Some(hi);
    }
    report.markers = markers.len();

    let step = (spec.step_s as u64 * rate as u64) as f64;
    let tol = rate as f64 * SPACING_TOLERANCE_MS / 1000.0;
    for pair in markers.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let spacing = (b - a) as f64;
        // markers swallowed by a long gap: compare with the nearest whole
        // number of steps
        let steps = (spacing / step).round().max(1.0);
        let err = spacing - steps * step;
        // zeros inside this interval already account for their own length
        let zeros: usize = zero_runs
            .iter()
            .filter(|&&(s, _)| s >= a && s < b)
            .map(|&(_, l)| l)
            .sum();
        if err.abs() <= tol {
            report.max_marker_jitter_ms = report
                .max_marker_jitter_ms
                .max(err.abs() * 1000.0 / rate as f64);
            continue;
        }
        let ms = err.abs() * 1000.0 / rate as f64;
        let kind = if err < 0.0 {
            "removed"
        } else if zeros as f64 >= err - tol {
            continue; // the extra length is the padded silence, already listed
        } else {
            "inserted"
        };
        report.gaps.push(ToneGap {
            at_s: to_s(a),
            ms,
            kind,
        });
    }
    report.gaps.sort_by(|x, y| {
        x.at_s
            .partial_cmp(&y.at_s)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(secs: f64, from: u64) -> Vec<i16> {
        let spec = ToneSpec::default();
        (0..(secs * RATE as f64) as u64)
            .map(|n| (spec.sample(from + n, RATE) * i16::MAX as f32) as i16)
            .collect()
    }

    #[test]
    fn clean_tone_has_no_gaps() {
        let mut s = vec![0i16; RATE as usize / 2]; // start-up padding
        s.extend(tone(10.3, 7_777));
        let r = analyze(&s, RATE, &ToneSpec::default());
        assert!(r.gaps.is_empty(), "{:?}", r.gaps);
        assert_eq!(r.markers, 10);
        assert!(r.max_marker_jitter_ms < 0.5, "{}", r.max_marker_jitter_ms);
        assert!((r.tone_start_s - 0.5).abs() < 0.01);
    }

    #[test]
    fn padded_gap_is_found_exactly() {
        let mut s = tone(10.0, 0);
        // lose 25 ms at 4.2 s, padded with silence (timeline intact)
        let at = (4.2 * RATE as f64) as usize;
        for v in &mut s[at..at + 1200] {
            *v = 0;
        }
        let r = analyze(&s, RATE, &ToneSpec::default());
        assert_eq!(r.gaps.len(), 1, "{:?}", r.gaps);
        assert_eq!(r.gaps[0].kind, "silence");
        assert!((r.gaps[0].ms - 25.0).abs() < 0.1);
        assert!((r.gaps[0].at_s - 4.2).abs() < 0.001);
    }

    #[test]
    fn removed_audio_is_found_from_marker_spacing() {
        let mut s = tone(10.0, 0);
        // drop 138 ms at 6.5 s without padding (the pre-fix mic behaviour)
        let at = (6.5 * RATE as f64) as usize;
        s.drain(at..at + 6_624);
        let r = analyze(&s, RATE, &ToneSpec::default());
        assert_eq!(r.gaps.len(), 1, "{:?}", r.gaps);
        assert_eq!(r.gaps[0].kind, "removed");
        assert!((r.gaps[0].ms - 138.0).abs() < 1.0, "{}", r.gaps[0].ms);
        assert!(r.gaps_longer_than(10.0) == 1);
    }

    #[test]
    fn removal_spanning_a_marker_is_still_found() {
        let mut s = tone(10.0, 0);
        let at = (2.9 * RATE as f64) as usize; // swallows the 3 s marker
        s.drain(at..at + 9_600); // 200 ms
        let r = analyze(&s, RATE, &ToneSpec::default());
        // the level change moves to the cut, so the loss shows up split
        // across the two intervals around it — the total is what's exact
        assert!(r.gaps.iter().all(|g| g.kind == "removed"), "{:?}", r.gaps);
        let total: f64 = r.gaps.iter().map(|g| g.ms).sum();
        assert!((total - 200.0).abs() < 1.5, "{:?}", r.gaps);
    }

    #[test]
    fn padding_across_a_marker_is_not_double_counted() {
        let mut s = tone(10.0, 0);
        let at = (2.95 * RATE as f64) as usize; // zeros swallow the 3 s marker
        for v in &mut s[at..at + 4_800] {
            *v = 0;
        }
        let r = analyze(&s, RATE, &ToneSpec::default());
        assert_eq!(r.gaps.len(), 1, "{:?}", r.gaps);
        assert_eq!(r.gaps[0].kind, "silence");
        assert!((r.gaps[0].ms - 100.0).abs() < 0.1);
    }

    #[test]
    fn small_jitter_is_not_a_gap() {
        let mut s = tone(6.0, 0);
        // a single dropped sample is below the 2 ms tolerance
        s.remove(3 * RATE as usize + 100);
        let r = analyze(&s, RATE, &ToneSpec::default());
        assert!(r.gaps.is_empty(), "{:?}", r.gaps);
    }

    #[test]
    fn capture_gain_does_not_matter() {
        // loopback at a lower system volume: same tone, a third of the level
        let mut s: Vec<i16> = tone(8.0, 0).iter().map(|v| v / 3).collect();
        let at = (5.5 * RATE as f64) as usize;
        s.drain(at..at + 960); // 20 ms removed
        let r = analyze(&s, RATE, &ToneSpec::default());
        assert_eq!(r.markers, 7);
        assert_eq!(r.gaps.len(), 1, "{:?}", r.gaps);
        assert!((r.gaps[0].ms - 20.0).abs() < 1.0);
    }

    #[test]
    fn silence_only_capture_reports_nothing() {
        let r = analyze(&vec![0i16; 48_000], RATE, &ToneSpec::default());
        assert_eq!(r.markers, 0);
        assert!(r.gaps.is_empty());
    }
}
