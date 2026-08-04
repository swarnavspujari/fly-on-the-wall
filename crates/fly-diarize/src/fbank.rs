//! Kaldi-style 80-dim log mel filterbank features for speaker-embedding
//! models (CAM++ / ERes2NetV2 / WeSpeaker ONNX exports all take (1, T, 80)).
//!
//! Parameters mirror kaldi-native-fbank as sherpa-onnx configures it for
//! speaker models: 16 kHz, 25 ms Povey window / 10 ms shift, snip-edges
//! framing, DC removal, pre-emphasis 0.97, power spectrum, 80 mel bins in
//! 20–7600 Hz, natural log with an epsilon floor, dither 0 (deterministic).
//! Absolute input scale does not matter downstream: it adds one constant to
//! every log-mel value and the per-dim mean subtraction (`apply_cmn`, the
//! models' "global-mean" normalize type) removes it.

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

pub const NUM_BINS: usize = 80;
const SAMPLE_RATE: f32 = 16_000.0;
const FRAME_LEN: usize = 400; // 25 ms
const FRAME_SHIFT: usize = 160; // 10 ms
const FFT_SIZE: usize = 512;
const PREEMPH: f32 = 0.97;
const LOW_FREQ: f32 = 20.0;
const HIGH_FREQ: f32 = 7_600.0;

fn mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

pub struct FbankComputer {
    fft: Arc<dyn Fft<f32>>,
    window: [f32; FRAME_LEN],
    /// Per mel bin: first FFT bin index + triangular weights.
    filters: Vec<(usize, Vec<f32>)>,
}

impl Default for FbankComputer {
    fn default() -> Self {
        Self::new()
    }
}

impl FbankComputer {
    pub fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);

        // Povey window: hann^0.85
        let mut window = [0.0f32; FRAME_LEN];
        for (i, w) in window.iter_mut().enumerate() {
            let hann =
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FRAME_LEN - 1) as f32).cos();
            *w = hann.powf(0.85);
        }

        // triangular mel filters over FFT bin center frequencies
        let n_fft_bins = FFT_SIZE / 2 + 1;
        let (mel_lo, mel_hi) = (mel(LOW_FREQ), mel(HIGH_FREQ));
        let mel_step = (mel_hi - mel_lo) / (NUM_BINS + 1) as f32;
        let bin_hz = SAMPLE_RATE / FFT_SIZE as f32;
        let mut filters = Vec::with_capacity(NUM_BINS);
        for b in 0..NUM_BINS {
            let left = mel_lo + b as f32 * mel_step;
            let center = left + mel_step;
            let right = center + mel_step;
            let mut first = None;
            let mut weights = Vec::new();
            for k in 0..n_fft_bins {
                let m = mel(k as f32 * bin_hz);
                let w = if m > left && m < right {
                    if m <= center {
                        (m - left) / mel_step
                    } else {
                        (right - m) / mel_step
                    }
                } else {
                    0.0
                };
                if w > 0.0 {
                    if first.is_none() {
                        first = Some(k);
                    }
                    weights.push(w);
                } else if first.is_some() {
                    break;
                }
            }
            filters.push((first.unwrap_or(0), weights));
        }

        Self {
            fft,
            window,
            filters,
        }
    }

    /// Log mel filterbank for 16 kHz mono samples: returns a flat
    /// `n_frames × NUM_BINS` matrix (row-major). Snip-edges framing: samples
    /// shorter than one frame yield zero frames.
    pub fn compute(&self, samples: &[f32]) -> (Vec<f32>, usize) {
        if samples.len() < FRAME_LEN {
            return (Vec::new(), 0);
        }
        let n_frames = (samples.len() - FRAME_LEN) / FRAME_SHIFT + 1;
        let mut out = Vec::with_capacity(n_frames * NUM_BINS);
        let mut frame = [0.0f32; FRAME_LEN];
        let mut fft_buf = [Complex32::new(0.0, 0.0); FFT_SIZE];
        let mut power = [0.0f32; FFT_SIZE / 2 + 1];
        for f in 0..n_frames {
            let start = f * FRAME_SHIFT;
            frame.copy_from_slice(&samples[start..start + FRAME_LEN]);

            // DC removal, then pre-emphasis (kaldi order), then window
            let mean = frame.iter().sum::<f32>() / FRAME_LEN as f32;
            for s in frame.iter_mut() {
                *s -= mean;
            }
            for i in (1..FRAME_LEN).rev() {
                frame[i] -= PREEMPH * frame[i - 1];
            }
            frame[0] -= PREEMPH * frame[0];

            for i in 0..FFT_SIZE {
                fft_buf[i] = if i < FRAME_LEN {
                    Complex32::new(frame[i] * self.window[i], 0.0)
                } else {
                    Complex32::new(0.0, 0.0)
                };
            }
            self.fft.process(&mut fft_buf);
            for (k, p) in power.iter_mut().enumerate() {
                *p = fft_buf[k].norm_sqr();
            }
            for (first, weights) in &self.filters {
                let e: f32 = weights
                    .iter()
                    .zip(&power[*first..])
                    .map(|(w, p)| w * p)
                    .sum();
                out.push(e.max(f32::EPSILON).ln());
            }
        }
        (out, n_frames)
    }
}

/// Subtract the per-dimension mean over frames — the "global-mean" feature
/// normalization the embedding ONNX models declare in their metadata.
pub fn apply_cmn(feats: &mut [f32], n_frames: usize) {
    if n_frames == 0 {
        return;
    }
    for d in 0..NUM_BINS {
        let mut mean = 0.0f32;
        for f in 0..n_frames {
            mean += feats[f * NUM_BINS + d];
        }
        mean /= n_frames as f32;
        for f in 0..n_frames {
            feats[f * NUM_BINS + d] -= mean;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_count_and_shape() {
        let fb = FbankComputer::new();
        // 1 s of a 440 Hz tone
        let samples: Vec<f32> = (0..16_000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin())
            .collect();
        let (feats, n) = fb.compute(&samples);
        assert_eq!(n, (16_000 - 400) / 160 + 1);
        assert_eq!(feats.len(), n * NUM_BINS);
    }

    #[test]
    fn tone_energy_lands_in_the_right_mel_bin() {
        let fb = FbankComputer::new();
        for hz in [300.0f32, 1000.0, 3000.0] {
            let samples: Vec<f32> = (0..16_000)
                .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / 16_000.0).sin())
                .collect();
            let (feats, n) = fb.compute(&samples);
            // average over frames, find the peak mel bin
            let mut avg = vec![0.0f32; NUM_BINS];
            for f in 0..n {
                for d in 0..NUM_BINS {
                    avg[d] += feats[f * NUM_BINS + d];
                }
            }
            let peak = (0..NUM_BINS)
                .max_by(|&a, &b| avg[a].total_cmp(&avg[b]))
                .unwrap();
            // expected bin: position of the tone on the mel axis
            let mel_lo = mel(LOW_FREQ);
            let mel_step = (mel(HIGH_FREQ) - mel_lo) / (NUM_BINS + 1) as f32;
            let expected = ((mel(hz) - mel_lo) / mel_step - 1.0).round() as isize;
            assert!(
                (peak as isize - expected).abs() <= 1,
                "{hz} Hz: peak bin {peak}, expected ~{expected}"
            );
        }
    }

    #[test]
    fn cmn_zeroes_the_mean_and_input_scale_cancels() {
        let fb = FbankComputer::new();
        // broadband pseudo-noise so no mel bin hits the epsilon floor at
        // either input scale (the floor is the one non-linear step)
        let mut state = 0x2545F491u32;
        let samples: Vec<f32> = (0..8_000)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect();
        let scaled: Vec<f32> = samples.iter().map(|s| s * 32_768.0).collect();
        let (mut a, n) = fb.compute(&samples);
        let (mut b, n2) = fb.compute(&scaled);
        assert_eq!(n, n2);
        apply_cmn(&mut a, n);
        apply_cmn(&mut b, n2);
        for d in 0..NUM_BINS {
            let mean: f32 = (0..n).map(|f| a[f * NUM_BINS + d]).sum::<f32>() / n as f32;
            assert!(mean.abs() < 1e-3);
        }
        for (x, y) in a.iter().zip(&b) {
            assert!(
                (x - y).abs() < 1e-2,
                "scale must cancel after CMN: {x} vs {y}"
            );
        }
    }
}
