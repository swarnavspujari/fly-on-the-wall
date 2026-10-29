//! Derived audio, regenerated from the stored source channels.
//!
//! Capture stores two files that are pure functions of the mic and system
//! channels: the 16 kHz ASR mixdown ([`crate::mix::write_mixdown`]) and the
//! native-rate playback mix ([`crate::mix::write_playback_mix`]). Recording
//! storage v2 stops keeping them: they are rebuilt from the sources (WAV or
//! FLAC) when something needs them.
//!
//! Everything here streams with bounded memory (a multi-hour recording never
//! sits in RAM) yet repeats the in-memory code's arithmetic step for step, so
//! the output is bit-identical. The unit tests pin that equivalence, and
//! [`matches_stored`] lets the caller prove it for each real file before the
//! stored copy is deleted.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use crate::flac::FlacSamples;
use crate::mix::MIX_SAMPLE_RATE;
use crate::{AudioError, Result};

/// Which derived file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivedKind {
    /// 16 kHz mono mix: the single-track ASR / diarization input.
    Mixdown,
    /// Mix at the higher of the two native rates: what the player plays.
    Playback,
}

/// Frames pulled per step. Small enough to stay cheap, big enough to be fast.
const CHUNK: usize = 1 << 15;

fn backend(e: impl std::fmt::Display) -> AudioError {
    AudioError::Backend(e.to_string())
}

/// The averaging [`crate::mix::read_wav_mono`] applies to one frame. Shared so
/// the streaming and in-memory readers can't drift apart.
#[inline]
pub(crate) fn frame_to_mono(frame: &[f32], channels: usize) -> f32 {
    frame.iter().sum::<f32>() / channels as f32
}

enum Decoder {
    Flac(FlacSamples),
    WavI16(hound::WavIntoSamples<BufReader<File>, i16>),
    WavI32(hound::WavIntoSamples<BufReader<File>, i32>),
    WavF32(hound::WavIntoSamples<BufReader<File>, f32>),
}

/// A recording file (WAV or FLAC) read as mono f32, exactly as
/// [`crate::mix::read_wav_mono`] converts it, a chunk at a time.
pub struct MonoFile {
    dec: Decoder,
    channels: usize,
    rate: u32,
    /// Mono samples the file holds (a trailing partial frame counts, as in
    /// `read_wav_mono`'s `chunks`).
    len: u64,
    raw: Vec<i32>,
    frame: Vec<f32>,
}

impl MonoFile {
    /// Open a WAV or a FLAC (sniffed by content, not by extension).
    pub fn open(path: &Path) -> Result<Self> {
        if crate::flac::is_flac(path) {
            let f = FlacSamples::open(path).map_err(backend)?;
            let channels = f.channels().max(1);
            return Ok(Self {
                channels,
                rate: f.sample_rate(),
                len: f.frames(),
                dec: Decoder::Flac(f),
                raw: Vec::new(),
                frame: Vec::new(),
            });
        }
        let reader = hound::WavReader::open(path)
            .map_err(|e| AudioError::Backend(format!("open {}: {e}", path.display())))?;
        let spec = reader.spec();
        let channels = spec.channels.max(1) as usize;
        let samples = u64::from(reader.len());
        let dec = match (spec.sample_format, spec.bits_per_sample) {
            (hound::SampleFormat::Int, 16) => Decoder::WavI16(reader.into_samples()),
            (hound::SampleFormat::Int, 32) => Decoder::WavI32(reader.into_samples()),
            (hound::SampleFormat::Float, 32) => Decoder::WavF32(reader.into_samples()),
            (fmt, bits) => {
                return Err(AudioError::Backend(format!(
                    "unsupported WAV format {fmt:?}/{bits} in {}",
                    path.display()
                )))
            }
        };
        Ok(Self {
            dec,
            channels,
            rate: spec.sample_rate,
            len: samples.div_ceil(channels as u64),
            raw: Vec::new(),
            frame: Vec::new(),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// Number of mono samples.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append up to `max` mono samples to `out`; fewer only at the end.
    pub fn read(&mut self, out: &mut Vec<f32>, max: usize) -> Result<usize> {
        let ch = self.channels;
        let want = max * ch;
        self.frame.clear();
        match &mut self.dec {
            Decoder::Flac(f) => {
                self.raw.clear();
                f.read_frames(max, &mut self.raw).map_err(backend)?;
                self.frame
                    .extend(self.raw.iter().map(|&v| v as f32 / i16::MAX as f32));
            }
            Decoder::WavI16(it) => {
                for s in it.take(want) {
                    let v = s.map_err(backend)?;
                    self.frame.push(v as f32 / i16::MAX as f32);
                }
            }
            Decoder::WavI32(it) => {
                for s in it.take(want) {
                    let v = s.map_err(backend)?;
                    self.frame.push(v as f32 / i32::MAX as f32);
                }
            }
            Decoder::WavF32(it) => {
                for s in it.take(want) {
                    self.frame.push(s.map_err(backend)?);
                }
            }
        }
        let before = out.len();
        out.extend(self.frame.chunks(ch).map(|f| frame_to_mono(f, ch)));
        Ok(out.len() - before)
    }

    /// Read the whole file (the in-memory equivalent of `read_wav_mono`).
    pub fn read_all(mut self) -> Result<(Vec<f32>, u32)> {
        let mut out = Vec::with_capacity(self.len as usize);
        while self.read(&mut out, CHUNK)? > 0 {}
        Ok((out, self.rate))
    }
}

/// [`crate::mix::resample_linear`], one chunk at a time, same arithmetic.
struct Resampler {
    src: MonoFile,
    /// Input samples `win_start..win_start + win.len()`.
    win: Vec<f32>,
    win_start: u64,
    src_done: bool,
    n: u64,
    ratio: f64,
    identity: bool,
    out_len: u64,
    next: u64,
    rate: u32,
}

impl Resampler {
    fn new(src: MonoFile, to_rate: u32) -> Self {
        let n = src.len();
        let from = src.sample_rate();
        let identity = from == to_rate || n == 0;
        let ratio = from as f64 / to_rate as f64;
        let out_len = if identity {
            n
        } else {
            ((n as usize as f64) / ratio).floor() as usize as u64
        };
        Self {
            src,
            win: Vec::new(),
            win_start: 0,
            src_done: false,
            n,
            ratio,
            identity,
            out_len,
            next: 0,
            rate: to_rate,
        }
    }

    /// Make sure input sample `idx` is in the window (if it exists at all).
    fn fill_to(&mut self, idx: u64) -> Result<()> {
        while !self.src_done && idx >= self.win_start + self.win.len() as u64 {
            // Drop what no later output can reach (indices only grow).
            let keep_from = self.next_input_floor();
            if keep_from > self.win_start + (CHUNK as u64) * 4 {
                let drop = (keep_from - self.win_start) as usize;
                let drop = drop.min(self.win.len());
                self.win.drain(..drop);
                self.win_start += drop as u64;
            }
            if self.src.read(&mut self.win, CHUNK)? == 0 {
                self.src_done = true;
            }
        }
        Ok(())
    }

    fn next_input_floor(&self) -> u64 {
        ((self.next as f64) * self.ratio) as usize as u64
    }

    fn at(&self, idx: u64) -> f32 {
        self.win[(idx - self.win_start) as usize]
    }

    /// Append up to `max` output samples; fewer only at the end.
    fn read(&mut self, out: &mut Vec<f32>, max: usize) -> Result<usize> {
        if self.identity {
            let mut got = 0;
            while got < max {
                let n = self.src.read(out, max - got)?;
                if n == 0 {
                    break;
                }
                got += n;
            }
            return Ok(got);
        }
        let mut got = 0;
        while got < max && self.next < self.out_len {
            let i = self.next as usize;
            let pos = i as f64 * self.ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            self.fill_to(idx as u64 + 1)?;
            if (idx as u64) < self.win_start
                || (idx as u64) >= self.win_start + self.win.len() as u64
            {
                return Err(AudioError::Backend(
                    "source ended before its declared length".into(),
                ));
            }
            let a = self.at(idx as u64);
            let b = if (idx as u64 + 1) < self.n {
                self.at(idx as u64 + 1)
            } else {
                a
            };
            out.push(a + (b - a) * frac);
            self.next += 1;
            got += 1;
        }
        Ok(got)
    }
}

/// `write_wav_mono_16`'s quantizer.
#[inline]
fn quantize(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

fn open_existing(p: Option<&Path>) -> Result<Option<MonoFile>> {
    match p {
        Some(p) if p.exists() => Ok(Some(MonoFile::open(p)?)),
        _ => Ok(None),
    }
}

/// Generate a derived file's samples and hand them to `sink` in chunks.
/// Returns `(sample_rate, sample_count)`.
fn generate(
    kind: DerivedKind,
    mic: Option<&Path>,
    system: Option<&Path>,
    sink: &mut dyn FnMut(u32, &[i16]) -> Result<()>,
    checkpoint: &mut dyn FnMut() -> bool,
) -> Result<(u32, u64)> {
    let (m, s) = (open_existing(mic)?, open_existing(system)?);
    // Mirrors write_mixdown / write_playback_mix: a lone channel is used as
    // is (mixdown: resampled to 16 kHz; playback: at its own rate, untouched).
    let (mut a, mut b) = match (kind, m, s) {
        (_, None, None) => return Err(AudioError::Backend("no channel recordings to mix".into())),
        (DerivedKind::Mixdown, m, s) => (
            m.map(|m| Resampler::new(m, MIX_SAMPLE_RATE)),
            s.map(|s| Resampler::new(s, MIX_SAMPLE_RATE)),
        ),
        (DerivedKind::Playback, Some(m), Some(s)) => {
            let rate = m.sample_rate().max(s.sample_rate());
            (Some(Resampler::new(m, rate)), Some(Resampler::new(s, rate)))
        }
        (DerivedKind::Playback, m, s) => {
            let lone = m.or(s).expect("one channel exists");
            let rate = lone.sample_rate();
            (Some(Resampler::new(lone, rate)), None)
        }
    };
    if a.is_none() {
        std::mem::swap(&mut a, &mut b);
    }
    let rate = a.as_ref().map_or(MIX_SAMPLE_RATE, |r| r.rate);
    let (mut ca, mut cb, mut out) = (Vec::new(), Vec::new(), Vec::new());
    let mut total = 0u64;
    let mut steps = 0usize;
    loop {
        if steps.is_multiple_of(8) && !checkpoint() {
            return Err(AudioError::Backend("stopped before finishing".into()));
        }
        steps += 1;
        ca.clear();
        cb.clear();
        out.clear();
        let na = match a.as_mut() {
            Some(r) => r.read(&mut ca, CHUNK)?,
            None => 0,
        };
        match b.as_mut() {
            None => out.extend(ca.iter().map(|&v| quantize(v))),
            Some(rb) => {
                let nb = rb.read(&mut cb, CHUNK)?;
                // mix_tracks: the longer track sets the length; the missing
                // side counts as 0.0; the sum is clamped (then quantized).
                for i in 0..na.max(nb) {
                    let s = ca.get(i).copied().unwrap_or(0.0) + cb.get(i).copied().unwrap_or(0.0);
                    out.push(quantize(s.clamp(-1.0, 1.0)));
                }
            }
        }
        if out.is_empty() {
            break;
        }
        total += out.len() as u64;
        sink(rate, &out)?;
    }
    Ok((rate, total))
}

/// Write a derived file as a 16-bit mono WAV, exactly the file
/// `write_mixdown` / `write_playback_mix` would write from the same
/// sources (which may now be FLAC). Returns the duration in milliseconds.
pub fn regenerate(
    kind: DerivedKind,
    mic: Option<&Path>,
    system: Option<&Path>,
    out: &Path,
    checkpoint: &mut dyn FnMut() -> bool,
) -> Result<u64> {
    let mut writer: Option<hound::WavWriter<std::io::BufWriter<File>>> = None;
    let (rate, total) = generate(
        kind,
        mic,
        system,
        &mut |rate, chunk| {
            if writer.is_none() {
                let spec = hound::WavSpec {
                    channels: 1,
                    sample_rate: rate,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                };
                writer =
                    Some(hound::WavWriter::create(out, spec).map_err(|e| {
                        AudioError::Backend(format!("create {}: {e}", out.display()))
                    })?);
            }
            let w = writer.as_mut().expect("created above");
            let mut i16w = w.get_i16_writer(chunk.len() as u32);
            for &s in chunk {
                i16w.write_sample(s);
            }
            i16w.flush().map_err(backend)
        },
        checkpoint,
    )?;
    let writer = match writer {
        Some(w) => w,
        None => {
            // No samples at all: still write a valid empty WAV.
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            hound::WavWriter::create(out, spec).map_err(backend)?
        }
    };
    writer.finalize().map_err(backend)?;
    Ok(total * 1000 / u64::from(rate))
}

/// Stored samples of a derived file, as 16-bit integers.
enum StoredI16 {
    Flac(FlacSamples),
    Wav(hound::WavIntoSamples<BufReader<File>, i16>),
}

/// `true` when regenerating `kind` from the sources yields exactly the
/// samples in `stored` (a 16-bit mono WAV or FLAC at the same rate, same
/// length, every sample equal). This is the proof required before a stored
/// derived file may be dropped.
pub fn matches_stored(
    kind: DerivedKind,
    mic: Option<&Path>,
    system: Option<&Path>,
    stored: &Path,
    checkpoint: &mut dyn FnMut() -> bool,
) -> Result<bool> {
    let (mut src, stored_rate, stored_len) = if crate::flac::is_flac(stored) {
        let f = FlacSamples::open(stored).map_err(backend)?;
        if f.channels() != 1 {
            return Ok(false);
        }
        let (rate, len) = (f.sample_rate(), f.frames());
        (StoredI16::Flac(f), rate, len)
    } else {
        let r = hound::WavReader::open(stored).map_err(backend)?;
        let spec = r.spec();
        if spec.channels != 1
            || spec.sample_format != hound::SampleFormat::Int
            || spec.bits_per_sample != 16
        {
            return Ok(false);
        }
        let len = u64::from(r.len());
        (StoredI16::Wav(r.into_samples()), spec.sample_rate, len)
    };
    let mut equal = true;
    let mut raw = Vec::new();
    let mismatch = "derived file differs";
    let result = generate(
        kind,
        mic,
        system,
        &mut |rate, chunk| {
            if rate != stored_rate {
                return Err(AudioError::Backend(mismatch.into()));
            }
            let same = match &mut src {
                StoredI16::Flac(f) => {
                    raw.clear();
                    f.read_frames(chunk.len(), &mut raw).map_err(backend)?;
                    raw.len() == chunk.len()
                        && raw.iter().zip(chunk).all(|(&x, &y)| x == i32::from(y))
                }
                StoredI16::Wav(it) => {
                    let mut ok = true;
                    for &y in chunk {
                        match it.next() {
                            Some(Ok(x)) if x == y => {}
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    ok
                }
            };
            if same {
                Ok(())
            } else {
                Err(AudioError::Backend(mismatch.into()))
            }
        },
        checkpoint,
    );
    match result {
        Ok((rate, total)) => {
            equal &= rate == stored_rate && total == stored_len;
            Ok(equal)
        }
        Err(AudioError::Backend(m)) if m == mismatch => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flac::tests::{test_signal, write_wav};
    use crate::mix;

    fn go() -> impl FnMut() -> bool {
        || true
    }

    fn wav_samples(p: &Path) -> (Vec<i16>, u32) {
        let mut r = hound::WavReader::open(p).unwrap();
        let rate = r.spec().sample_rate;
        (r.samples::<i16>().map(|s| s.unwrap()).collect(), rate)
    }

    #[test]
    fn streaming_resample_matches_resample_linear_bit_for_bit() {
        let dir = tempfile::tempdir().unwrap();
        for (from, to, len) in [
            (48_000u32, 16_000u32, 200_001usize),
            (44_100, 16_000, 150_000),
            (16_000, 48_000, 40_000),
            (22_050, 48_000, 33_333),
            (96_000, 48_000, 100_000),
            (48_000, 44_100, 99_999),
            (16_000, 16_000, 70_000),
            (48_000, 16_000, 1),
            (48_000, 16_000, 2),
            (48_000, 16_000, 0),
        ] {
            let p = dir.path().join(format!("r{from}-{to}-{len}.wav"));
            write_wav(&p, &test_signal(len, 1, len as u64 + 1), 1, from);
            let (all, rate) = mix::read_wav_mono(&p).unwrap();
            let want = mix::resample_linear(&all, rate, to);
            let mut r = Resampler::new(MonoFile::open(&p).unwrap(), to);
            let mut got = Vec::new();
            // uneven chunk sizes exercise the window bookkeeping
            let mut step = 1;
            while r.read(&mut got, step).unwrap() > 0 {
                step = (step * 7 + 3) % 50_000 + 1;
            }
            assert_eq!(got.len(), want.len(), "{from}->{to} len {len}");
            assert!(
                got.iter()
                    .zip(&want)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "{from}->{to} len {len}: samples differ"
            );
        }
    }

    #[test]
    fn mono_file_reads_like_read_wav_mono_for_wav_and_flac() {
        let dir = tempfile::tempdir().unwrap();
        for ch in [1u16, 2] {
            let wav = dir.path().join(format!("s{ch}.wav"));
            let flac = dir.path().join(format!("s{ch}.flac"));
            write_wav(&wav, &test_signal(50_001, ch as usize, 11), ch, 48_000);
            crate::flac::encode_pcm16_wav_to_flac(&wav, &flac, &mut go()).unwrap();
            let (want, wr) = mix::read_wav_mono(&wav).unwrap();
            for p in [&wav, &flac] {
                let (got, gr) = MonoFile::open(p).unwrap().read_all().unwrap();
                assert_eq!(gr, wr);
                assert_eq!(got.len(), want.len());
                assert!(got
                    .iter()
                    .zip(&want)
                    .all(|(a, b)| a.to_bits() == b.to_bits()));
            }
            let (via, _) = mix::read_audio_mono(&flac).unwrap();
            assert!(via
                .iter()
                .zip(&want)
                .all(|(a, b)| a.to_bits() == b.to_bits()));
        }
    }

    /// Every channel layout the app can store: both channels at equal or
    /// different rates and lengths, one channel only, loud (clipping) mixes.
    fn layouts(dir: &Path) -> Vec<(Option<std::path::PathBuf>, Option<std::path::PathBuf>)> {
        let mk = |name: &str, len: usize, rate: u32, seed: u64, gain: f32| {
            let p = dir.join(name);
            let s: Vec<i16> = test_signal(len, 1, seed)
                .into_iter()
                .map(|v| ((v as f32) * gain).clamp(-32768.0, 32767.0) as i16)
                .collect();
            write_wav(&p, &s, 1, rate);
            p
        };
        let mic = mk("mic.wav", 96_000, 48_000, 1, 1.0);
        let sys = mk("sys.wav", 95_000, 48_000, 2, 1.0);
        let mic44 = mk("mic44.wav", 88_200, 44_100, 3, 1.0);
        let loud_a = mk("la.wav", 48_000, 48_000, 4, 3.0);
        let loud_b = mk("lb.wav", 48_100, 48_000, 5, 3.0);
        let sys16 = mk("sys16.wav", 30_000, 16_000, 6, 1.0);
        vec![
            (Some(mic.clone()), Some(sys.clone())),
            (Some(mic44.clone()), Some(sys.clone())),
            (Some(sys.clone()), Some(mic44)),
            (Some(loud_a), Some(loud_b)),
            (Some(mic.clone()), None),
            (None, Some(sys)),
            (Some(mic), Some(sys16)),
            (Some(dir.join("missing.wav")), Some(dir.join("sys.wav"))),
        ]
    }

    #[test]
    fn regenerated_mixes_are_byte_identical_to_capture_output() {
        let dir = tempfile::tempdir().unwrap();
        for (i, (m, s)) in layouts(dir.path()).into_iter().enumerate() {
            for kind in [DerivedKind::Mixdown, DerivedKind::Playback] {
                let want = dir.path().join(format!("want-{i}-{kind:?}.wav"));
                let got = dir.path().join(format!("got-{i}-{kind:?}.wav"));
                let want_ms = match kind {
                    DerivedKind::Mixdown => mix::write_mixdown(m.as_deref(), s.as_deref(), &want),
                    DerivedKind::Playback => {
                        mix::write_playback_mix(m.as_deref(), s.as_deref(), &want)
                    }
                }
                .unwrap();
                let got_ms = regenerate(kind, m.as_deref(), s.as_deref(), &got, &mut go()).unwrap();
                assert_eq!(got_ms, want_ms, "layout {i} {kind:?}");
                assert_eq!(
                    std::fs::read(&got).unwrap(),
                    std::fs::read(&want).unwrap(),
                    "layout {i} {kind:?}: files differ"
                );
                assert!(
                    matches_stored(kind, m.as_deref(), s.as_deref(), &want, &mut go()).unwrap()
                );
            }
        }
    }

    #[test]
    fn regeneration_from_flac_sources_matches_wav_sources() {
        let dir = tempfile::tempdir().unwrap();
        let mic = dir.path().join("mic.wav");
        let sys = dir.path().join("sys.wav");
        write_wav(&mic, &test_signal(70_000, 1, 21), 1, 48_000);
        write_wav(&sys, &test_signal(71_234, 1, 22), 1, 44_100);
        let (micf, sysf) = (dir.path().join("mic.flac"), dir.path().join("sys.flac"));
        crate::flac::encode_pcm16_wav_to_flac(&mic, &micf, &mut go()).unwrap();
        crate::flac::encode_pcm16_wav_to_flac(&sys, &sysf, &mut go()).unwrap();
        for kind in [DerivedKind::Mixdown, DerivedKind::Playback] {
            let want = dir.path().join(format!("want-{kind:?}.wav"));
            match kind {
                DerivedKind::Mixdown => mix::write_mixdown(Some(&mic), Some(&sys), &want),
                DerivedKind::Playback => mix::write_playback_mix(Some(&mic), Some(&sys), &want),
            }
            .unwrap();
            let got = dir.path().join(format!("got-{kind:?}.wav"));
            regenerate(kind, Some(&micf), Some(&sysf), &got, &mut go()).unwrap();
            assert_eq!(std::fs::read(&got).unwrap(), std::fs::read(&want).unwrap());
            // The stored copy can itself be FLAC.
            let wantf = dir.path().join(format!("want-{kind:?}.flac"));
            crate::flac::encode_pcm16_wav_to_flac(&want, &wantf, &mut go()).unwrap();
            assert!(matches_stored(kind, Some(&micf), Some(&sysf), &wantf, &mut go()).unwrap());
        }
    }

    #[test]
    fn a_stored_mix_that_differs_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mic = dir.path().join("mic.wav");
        let sys = dir.path().join("sys.wav");
        write_wav(&mic, &test_signal(40_000, 1, 1), 1, 48_000);
        write_wav(&sys, &test_signal(40_000, 1, 2), 1, 48_000);
        let stored = dir.path().join("mixed.wav");
        mix::write_mixdown(Some(&mic), Some(&sys), &stored).unwrap();
        let (orig, rate) = wav_samples(&stored);
        let check = |s: &[i16], rate: u32| {
            write_wav(&stored, s, 1, rate);
            matches_stored(
                DerivedKind::Mixdown,
                Some(&mic),
                Some(&sys),
                &stored,
                &mut go(),
            )
            .unwrap()
        };
        // one sample, one LSB
        let mut s = orig.clone();
        s[4321] = s[4321].wrapping_sub(1);
        assert!(!check(&s, rate));
        // one sample short, one sample long
        assert!(!check(&orig[..orig.len() - 1], rate));
        let mut long = orig.clone();
        long.push(0);
        assert!(!check(&long, rate));
        // wrong rate
        assert!(!check(&orig, 16_001));
        // the mic-only mix is not the two-channel mix
        assert!(check(&orig, rate));
        assert!(matches_stored(
            DerivedKind::Mixdown,
            Some(&mic),
            Some(&sys),
            &stored,
            &mut go()
        )
        .unwrap());
        assert!(
            !matches_stored(DerivedKind::Mixdown, Some(&mic), None, &stored, &mut go()).unwrap()
        );
    }
}
