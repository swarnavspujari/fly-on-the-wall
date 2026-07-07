//! Pure audio helpers: WAV read, linear resampling, and the post-capture
//! mixdown that produces the 16 kHz mono track the ASR pipeline consumes.

use std::path::Path;

use crate::{AudioError, Result};

/// Target rate for the mixed track — what whisper.cpp expects natively.
pub const MIX_SAMPLE_RATE: u32 = 16_000;

/// Read a WAV into mono f32 samples plus its sample rate. Multi-channel
/// input is averaged down to mono.
pub fn read_wav_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| AudioError::Backend(format!("open {}: {e}", path.display())))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;

    let interleaved: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / i16::MAX as f32))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| AudioError::Backend(e.to_string()))?,
        (hound::SampleFormat::Int, 32) => reader
            .samples::<i32>()
            .map(|s| s.map(|v| v as f32 / i32::MAX as f32))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| AudioError::Backend(e.to_string()))?,
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| AudioError::Backend(e.to_string()))?,
        (fmt, bits) => {
            return Err(AudioError::Backend(format!(
                "unsupported WAV format {fmt:?}/{bits} in {}",
                path.display()
            )))
        }
    };

    let mono: Vec<f32> = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

/// Linear-interpolation resample. Fine for speech; avoids a DSP dependency.
pub fn resample_linear(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = ((samples.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let idx = pos as usize;
        let frac = (pos - idx as f64) as f32;
        let a = samples[idx];
        let b = *samples.get(idx + 1).unwrap_or(&a);
        out.push(a + (b - a) * frac);
    }
    out
}

/// Boost quiet audio so its peak hits `target_peak` (never attenuates, never
/// boosts more than `max_gain`). System-loopback recordings routinely peak at
/// −12 dBFS or lower; ASR models behave better on healthy levels.
pub fn normalize_peak(samples: &mut [f32], target_peak: f32, max_gain: f32) {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak <= 0.0 || peak >= target_peak {
        return;
    }
    let gain = (target_peak / peak).min(max_gain);
    for s in samples.iter_mut() {
        *s *= gain;
    }
}

/// Sum two mono tracks (already at the same rate); the longer one defines
/// the length. Soft-clipped to [-1, 1].
pub fn mix_tracks(a: &[f32], b: &[f32]) -> Vec<f32> {
    let len = a.len().max(b.len());
    (0..len)
        .map(|i| {
            let s = a.get(i).copied().unwrap_or(0.0) + b.get(i).copied().unwrap_or(0.0);
            s.clamp(-1.0, 1.0)
        })
        .collect()
}

/// Write mono f32 samples as a 16-bit PCM WAV.
pub fn write_wav_mono_16(path: &Path, samples: &[f32], rate: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|e| AudioError::Backend(format!("create {}: {e}", path.display())))?;
    for &s in samples {
        writer
            .write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .map_err(|e| AudioError::Backend(e.to_string()))?;
    }
    writer
        .finalize()
        .map_err(|e| AudioError::Backend(e.to_string()))?;
    Ok(())
}

/// Build the 16 kHz mono mixed track from whichever per-channel recordings
/// exist. Returns the duration of the mixed track in milliseconds.
pub fn write_mixdown(mic: Option<&Path>, system: Option<&Path>, out: &Path) -> Result<u64> {
    let load = |p: Option<&Path>| -> Result<Option<Vec<f32>>> {
        match p {
            Some(p) if p.exists() => {
                let (samples, rate) = read_wav_mono(p)?;
                Ok(Some(resample_linear(&samples, rate, MIX_SAMPLE_RATE)))
            }
            _ => Ok(None),
        }
    };
    let mixed = match (load(mic)?, load(system)?) {
        (Some(m), Some(s)) => mix_tracks(&m, &s),
        (Some(m), None) => m,
        (None, Some(s)) => s,
        (None, None) => return Err(AudioError::Backend("no channel recordings to mix".into())),
    };
    write_wav_mono_16(out, &mixed, MIX_SAMPLE_RATE)?;
    Ok(mixed.len() as u64 * 1000 / MIX_SAMPLE_RATE as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_halves_length() {
        let samples: Vec<f32> = (0..1000).map(|i| (i as f32 / 100.0).sin()).collect();
        let out = resample_linear(&samples, 32_000, 16_000);
        assert!((out.len() as i64 - 500).abs() <= 1);
    }

    #[test]
    fn resample_same_rate_is_identity() {
        let samples = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_linear(&samples, 16_000, 16_000), samples);
    }

    #[test]
    fn normalize_boosts_quiet_audio_to_target() {
        let mut s = vec![0.0, 0.1, -0.2, 0.05];
        normalize_peak(&mut s, 0.8, 40.0);
        assert!((s[2] + 0.8).abs() < 1e-6, "peak should hit target: {s:?}");
        assert!((s[1] - 0.4).abs() < 1e-6);
    }

    #[test]
    fn normalize_caps_gain_and_never_attenuates() {
        let mut quiet = vec![0.001, -0.001];
        normalize_peak(&mut quiet, 0.8, 10.0);
        assert!(
            (quiet[0] - 0.01).abs() < 1e-6,
            "gain must cap at 10x: {quiet:?}"
        );

        let mut loud = vec![0.9, -0.95];
        normalize_peak(&mut loud, 0.8, 10.0);
        assert_eq!(loud, vec![0.9, -0.95], "already-loud audio is untouched");
    }

    #[test]
    fn normalize_leaves_silence_alone() {
        let mut silence = vec![0.0; 8];
        normalize_peak(&mut silence, 0.8, 10.0);
        assert_eq!(silence, vec![0.0; 8]);
    }

    #[test]
    fn mix_pads_shorter_track_and_clips() {
        let a = vec![0.9, 0.9];
        let b = vec![0.9, 0.9, 0.5];
        let m = mix_tracks(&a, &b);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0], 1.0); // clipped
        assert_eq!(m[2], 0.5); // padded from a
    }

    #[test]
    fn wav_roundtrip_and_mixdown() {
        let dir = tempfile::tempdir().unwrap();
        let mic = dir.path().join("mic.wav");
        let sys = dir.path().join("sys.wav");
        let out = dir.path().join("mixed.wav");

        // 1s of 440Hz at 48k, 0.5s of 220Hz at 44.1k
        let mic_samples: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.4)
            .collect();
        let sys_samples: Vec<f32> = (0..22_050)
            .map(|i| (i as f32 * 220.0 * std::f32::consts::TAU / 44_100.0).sin() * 0.4)
            .collect();
        write_wav_mono_16(&mic, &mic_samples, 48_000).unwrap();
        write_wav_mono_16(&sys, &sys_samples, 44_100).unwrap();

        let dur = write_mixdown(Some(&mic), Some(&sys), &out).unwrap();
        assert!((dur as i64 - 1000).abs() <= 5, "duration was {dur}ms");

        let (mixed, rate) = read_wav_mono(&out).unwrap();
        assert_eq!(rate, MIX_SAMPLE_RATE);
        assert!((mixed.len() as i64 - 16_000).abs() <= 5);
        // energy present in the overlap region
        assert!(mixed[..8000].iter().any(|s| s.abs() > 0.1));
    }
}
