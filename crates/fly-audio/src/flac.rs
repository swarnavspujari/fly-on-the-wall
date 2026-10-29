//! Lossless FLAC storage for finished recordings (recording storage v2).
//!
//! Capture keeps writing 16-bit PCM WAV, which survives a crash mid-meeting.
//! Once a meeting's transcript is final the app re-stores each WAV as FLAC at
//! the same sample rate, channel count and bit depth. FLAC is lossless: every
//! sample decodes back exactly.
//!
//! This module never trusts that on faith:
//! - [`inspect_pcm16_wav`] only accepts a WAV whose layout is exactly what
//!   the app writes (`fmt ` + `data`, sizes consistent with the file length).
//!   A half-finalized or foreign WAV is refused and stays a WAV, so a header
//!   that under-reports its samples can never lead to deleting audio.
//! - [`encode_pcm16_wav_to_flac`] streams block by block (bounded memory for
//!   multi-hour recordings) and back-patches STREAMINFO (sample count, MD5).
//! - [`verify_flac_matches_wav`] decodes the FLAC with a second, independent
//!   decoder (claxon) and compares every sample with the WAV's raw PCM, plus
//!   the format, the sample count and the STREAMINFO MD5. Callers delete the
//!   WAV only after this returns `Ok`.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use flacenc::bitsink::MemSink;
use flacenc::component::{BitRepr, Stream, StreamInfo};
use flacenc::error::Verify;
use flacenc::source::{Context, Fill, FrameBuf};
use md5::{Digest, Md5};

/// FLAC block size (samples per channel per frame). 4096 is the reference
/// encoder's default for 44.1/48 kHz.
const BLOCK_SIZE: usize = 4096;

/// How many blocks pass between two `checkpoint` calls (~5 s of 48 kHz audio).
const CHECKPOINT_EVERY: usize = 64;

/// The four bytes every FLAC stream starts with.
pub const FLAC_MAGIC: &[u8; 4] = b"fLaC";

#[derive(Debug, thiserror::Error)]
pub enum FlacError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The WAV isn't in the exact layout the app writes; it stays a WAV.
    #[error("not a plain 16-bit PCM WAV: {0}")]
    UnsupportedWav(String),
    #[error("FLAC encoder: {0}")]
    Encode(String),
    #[error("FLAC decoder: {0}")]
    Decode(String),
    /// The FLAC doesn't decode to exactly the WAV's samples.
    #[error("FLAC does not match the original: {0}")]
    Mismatch(String),
    /// The caller's checkpoint asked to stop.
    #[error("stopped before finishing")]
    Interrupted,
}

pub type Result<T> = std::result::Result<T, FlacError>;

/// Layout of a WAV the app wrote: 16-bit integer PCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmWavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    /// Byte offset of the first sample.
    pub data_offset: u64,
    /// Length of the sample data in bytes.
    pub data_len: u64,
}

impl PcmWavInfo {
    /// Samples per channel.
    pub fn frames(&self) -> u64 {
        self.data_len / (u64::from(self.channels) * 2)
    }
}

fn read_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn read_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// `true` when the file starts with the FLAC stream marker.
pub fn is_flac(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok_and(|()| &magic == FLAC_MAGIC)
}

/// Parse and validate a WAV the app wrote. Accepts only: a `RIFF`/`WAVE`
/// header whose size matches the file, one `fmt ` chunk describing 16-bit
/// integer PCM (plain or `WAVE_FORMAT_EXTENSIBLE` with the PCM sub-format),
/// then one `data` chunk that runs exactly to the end of the file (plus the
/// RIFF pad byte for an odd length). Anything else (an unfinalized header, a
/// metadata chunk FLAC wouldn't carry, float or 24-bit audio, trailing bytes)
/// is refused, so nothing about the file can be lost by converting it.
pub fn inspect_pcm16_wav(path: &Path) -> Result<PcmWavInfo> {
    let unsupported = |why: &str| FlacError::UnsupportedWav(why.to_string());
    let mut f = File::open(path)?;
    let file_len = f.metadata()?.len();
    let mut head = [0u8; 12];
    f.read_exact(&mut head)
        .map_err(|_| unsupported("shorter than a RIFF header"))?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Err(unsupported("no RIFF/WAVE header"));
    }
    if u64::from(read_u32(&head[4..8])) + 8 != file_len {
        return Err(unsupported(
            "RIFF size doesn't match the file (unfinalized?)",
        ));
    }
    let mut pos: u64 = 12;
    let mut fmt: Option<(u32, u16)> = None;
    loop {
        let mut ch = [0u8; 8];
        f.read_exact(&mut ch)
            .map_err(|_| unsupported("no data chunk"))?;
        let id = [ch[0], ch[1], ch[2], ch[3]];
        let size = u64::from(read_u32(&ch[4..8]));
        let body = pos + 8;
        match &id {
            b"fmt " => {
                if fmt.is_some() || !(16..=40).contains(&size) {
                    return Err(unsupported("unexpected fmt chunk"));
                }
                let mut b = vec![0u8; size as usize];
                f.read_exact(&mut b)
                    .map_err(|_| unsupported("truncated fmt chunk"))?;
                let tag = read_u16(&b[0..2]);
                let channels = read_u16(&b[2..4]);
                let rate = read_u32(&b[4..8]);
                let byte_rate = read_u32(&b[8..12]);
                let block_align = read_u16(&b[12..14]);
                let bits = read_u16(&b[14..16]);
                let pcm = match tag {
                    1 => size == 16 || (size == 18 && read_u16(&b[16..18]) == 0),
                    // WAVE_FORMAT_EXTENSIBLE: cbSize 22, 16 valid bits, PCM GUID.
                    0xFFFE => {
                        size == 40
                            && read_u16(&b[16..18]) == 22
                            && read_u16(&b[18..20]) == 16
                            && b[24..40]
                                == [
                                    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00,
                                    0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
                                ]
                    }
                    _ => false,
                };
                if !pcm || bits != 16 {
                    return Err(unsupported("not 16-bit integer PCM"));
                }
                if !(1..=8).contains(&channels)
                    || rate == 0
                    || rate > 655_350
                    || block_align != channels * 2
                    || byte_rate != rate * u32::from(block_align)
                {
                    return Err(unsupported("inconsistent fmt chunk"));
                }
                fmt = Some((rate, channels));
            }
            b"data" => {
                let Some((sample_rate, channels)) = fmt else {
                    return Err(unsupported("data before fmt"));
                };
                let end = body + size + (size & 1);
                if end != file_len {
                    return Err(unsupported(
                        "data chunk doesn't end at the end of the file",
                    ));
                }
                if size % (u64::from(channels) * 2) != 0 {
                    return Err(unsupported("partial sample frame"));
                }
                return Ok(PcmWavInfo {
                    sample_rate,
                    channels,
                    data_offset: body,
                    data_len: size,
                });
            }
            _ => return Err(unsupported("extra chunk FLAC wouldn't keep")),
        }
        pos = body + size + (size & 1);
        f.seek(SeekFrom::Start(pos))?;
    }
}

/// Streams the raw interleaved samples of a validated 16-bit PCM WAV.
struct PcmWavReader {
    file: BufReader<File>,
    remaining: u64,
    channels: usize,
    bytes: Vec<u8>,
}

impl PcmWavReader {
    fn open(path: &Path, info: &PcmWavInfo) -> Result<Self> {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(info.data_offset))?;
        Ok(Self {
            file: BufReader::with_capacity(1 << 20, file),
            remaining: info.data_len,
            channels: usize::from(info.channels),
            bytes: Vec::new(),
        })
    }

    /// Read up to `frames` frames into `out` (interleaved, cleared first).
    /// Returns the number of frames read; 0 at the end.
    fn read_frames(&mut self, frames: usize, out: &mut Vec<i32>) -> Result<usize> {
        out.clear();
        let want = (frames as u64 * self.channels as u64 * 2).min(self.remaining) as usize;
        if want == 0 {
            return Ok(0);
        }
        self.bytes.resize(want, 0);
        self.file.read_exact(&mut self.bytes)?;
        self.remaining -= want as u64;
        out.extend(
            self.bytes
                .chunks_exact(2)
                .map(|b| i32::from(i16::from_le_bytes([b[0], b[1]]))),
        );
        Ok(want / (self.channels * 2))
    }
}

/// What one conversion produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeOutcome {
    /// Samples per channel.
    pub frames: u64,
    /// Size of the FLAC file.
    pub flac_bytes: u64,
}

fn header_bytes(info: &StreamInfo) -> Result<Vec<u8>> {
    let stream = Stream::with_stream_info(info.clone());
    let mut sink = MemSink::<u8>::new();
    stream
        .write(&mut sink)
        .map_err(|e| FlacError::Encode(e.to_string()))?;
    Ok(sink.into_inner())
}

/// Encode a validated 16-bit PCM WAV into a FLAC at `out` (created or
/// truncated), keeping the rate, channel count and bit depth. Streams one
/// block at a time; the STREAMINFO header is written first with placeholder
/// totals and patched once the sample count and MD5 are known. The file is
/// `fsync`ed before returning.
///
/// `checkpoint` runs every few seconds of audio; returning `false` stops with
/// [`FlacError::Interrupted`] (the partial `out` is the caller's to delete).
/// It may block (pause) for as long as it likes.
pub fn encode_pcm16_wav_to_flac(
    wav: &Path,
    out: &Path,
    checkpoint: &mut dyn FnMut() -> bool,
) -> Result<EncodeOutcome> {
    let wav_info = inspect_pcm16_wav(wav)?;
    let channels = usize::from(wav_info.channels);
    let enc = |e: &dyn std::fmt::Display| FlacError::Encode(e.to_string());
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| enc(&e))?;
    let mut info = StreamInfo::new(wav_info.sample_rate as usize, channels, 16)
        .map_err(|e| enc(&e))?;

    let mut reader = PcmWavReader::open(wav, &wav_info)?;
    let mut file = File::create(out)?;
    let placeholder = header_bytes(&info)?;
    let mut w = BufWriter::with_capacity(1 << 20, &mut file);
    w.write_all(&placeholder)?;

    let mut fb_ctx = (
        FrameBuf::with_size(channels, BLOCK_SIZE).map_err(|e| enc(&e))?,
        Context::new(16, channels),
    );
    let mut samples = Vec::with_capacity(BLOCK_SIZE * channels);
    let mut sink = MemSink::<u8>::new();
    let mut frames: u64 = 0;
    let (mut min_frame, mut max_frame) = (usize::MAX, 0usize);
    let mut blocks = 0usize;
    loop {
        if blocks % CHECKPOINT_EVERY == 0 && !checkpoint() {
            return Err(FlacError::Interrupted);
        }
        blocks += 1;
        let n = reader.read_frames(BLOCK_SIZE, &mut samples)?;
        if n == 0 {
            break;
        }
        fb_ctx.fill_interleaved(&samples).map_err(|e| enc(&e))?;
        let frame_no = fb_ctx.1.current_frame_number().unwrap_or(0);
        let frame = flacenc::encode_fixed_size_frame(&config, &fb_ctx.0, frame_no, &info)
            .map_err(|e| enc(&e))?;
        sink.clear();
        frame.write(&mut sink).map_err(|e| enc(&e))?;
        let bytes = sink.as_slice();
        min_frame = min_frame.min(bytes.len());
        max_frame = max_frame.max(bytes.len());
        w.write_all(bytes)?;
        frames += n as u64;
    }
    w.flush()?;
    drop(w);

    if frames != wav_info.frames() {
        return Err(FlacError::Encode(format!(
            "read {frames} of {} samples",
            wav_info.frames()
        )));
    }
    // A fixed-blocksize stream reports the block size as both min and max
    // (the reference encoder does the same, even for one short block).
    info.set_block_sizes(BLOCK_SIZE, BLOCK_SIZE)
        .map_err(|e| enc(&e))?;
    if frames == 0 {
        info.set_frame_sizes(0, 0).map_err(|e| enc(&e))?;
    } else {
        info.set_frame_sizes(min_frame, max_frame)
            .map_err(|e| enc(&e))?;
    }
    info.set_total_samples(frames as usize);
    info.set_md5_digest(&fb_ctx.1.md5_digest());
    let header = header_bytes(&info)?;
    if header.len() != placeholder.len() {
        return Err(FlacError::Encode("STREAMINFO changed size".into()));
    }
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header)?;
    file.sync_all()?;
    let flac_bytes = file.metadata()?.len();
    Ok(EncodeOutcome { frames, flac_bytes })
}

/// Decode `flac` with claxon (an implementation independent of the encoder)
/// and prove it holds exactly the samples of `wav`: same rate, channel count
/// and bit depth, the same number of samples, every sample equal, and the
/// STREAMINFO MD5 equal to the digest of the decoded audio.
pub fn verify_flac_matches_wav(
    flac: &Path,
    wav: &Path,
    checkpoint: &mut dyn FnMut() -> bool,
) -> Result<u64> {
    let wav_info = inspect_pcm16_wav(wav)?;
    let mismatch = |why: String| FlacError::Mismatch(why);
    let dec = |e: claxon::Error| FlacError::Decode(e.to_string());
    let mut reader = claxon::FlacReader::new(BufReader::with_capacity(
        1 << 20,
        File::open(flac)?,
    ))
    .map_err(dec)?;
    let si = reader.streaminfo();
    if si.sample_rate != wav_info.sample_rate
        || si.channels != u32::from(wav_info.channels)
        || si.bits_per_sample != 16
    {
        return Err(mismatch(format!(
            "format {} Hz/{} ch/{} bit vs {} Hz/{} ch/16 bit",
            si.sample_rate, si.channels, si.bits_per_sample, wav_info.sample_rate, wav_info.channels
        )));
    }
    // FLAC writes a total of 0 as "unknown"; only an empty file may say so.
    let declared = si.samples.unwrap_or(0);
    if declared != wav_info.frames() {
        return Err(mismatch(format!(
            "header says {:?} samples, the WAV has {}",
            si.samples,
            wav_info.frames()
        )));
    }
    let channels = usize::from(wav_info.channels);
    let mut wav_reader = PcmWavReader::open(wav, &wav_info)?;
    let mut expected = Vec::new();
    let mut md5 = Md5::new();
    let mut frames: u64 = 0;
    let mut blocks = 0usize;
    let mut frame_reader = reader.blocks();
    let mut buffer = Vec::new();
    loop {
        if blocks % CHECKPOINT_EVERY == 0 && !checkpoint() {
            return Err(FlacError::Interrupted);
        }
        blocks += 1;
        let Some(block) = frame_reader.read_next_or_eof(buffer).map_err(dec)? else {
            break;
        };
        let n = block.duration() as usize;
        if wav_reader.read_frames(n, &mut expected)? != n {
            return Err(mismatch("the FLAC is longer than the WAV".into()));
        }
        for i in 0..n {
            for ch in 0..channels {
                let got = block.sample(ch as u32, i as u32);
                if got != expected[i * channels + ch] {
                    return Err(mismatch(format!(
                        "sample {} (channel {ch}) differs",
                        frames + i as u64
                    )));
                }
                md5.update((got as i16).to_le_bytes());
            }
        }
        frames += n as u64;
        buffer = block.into_buffer();
    }
    if frames != wav_info.frames() {
        return Err(mismatch(format!(
            "decoded {frames} of {} samples",
            wav_info.frames()
        )));
    }
    let digest: [u8; 16] = md5.finalize().into();
    if digest != si.md5sum {
        return Err(mismatch("STREAMINFO MD5 doesn't match the audio".into()));
    }
    Ok(frames)
}

/// Stream of interleaved 16-bit samples decoded from a FLAC file.
pub(crate) struct FlacSamples {
    reader: claxon::FlacReader<BufReader<File>>,
    decode_buf: Vec<i32>,
    pending: Vec<i32>,
    pending_pos: usize,
    channels: usize,
    sample_rate: u32,
    frames: u64,
    done: bool,
}

impl FlacSamples {
    /// Open a 16-bit FLAC (the only depth this app writes).
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let reader = claxon::FlacReader::new(BufReader::with_capacity(1 << 20, File::open(path)?))
            .map_err(|e| FlacError::Decode(e.to_string()))?;
        let si = reader.streaminfo();
        if si.bits_per_sample != 16 {
            return Err(FlacError::Decode(format!(
                "unsupported FLAC bit depth {}",
                si.bits_per_sample
            )));
        }
        Ok(Self {
            channels: si.channels as usize,
            sample_rate: si.sample_rate,
            frames: si.samples.unwrap_or(0),
            reader,
            decode_buf: Vec::new(),
            pending: Vec::new(),
            pending_pos: 0,
            done: false,
        })
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    /// Samples per channel, from STREAMINFO.
    pub(crate) fn frames(&self) -> u64 {
        self.frames
    }

    /// Append up to `max_frames` interleaved frames to `out`. Returns the
    /// number appended; 0 at the end.
    pub(crate) fn read_frames(&mut self, max_frames: usize, out: &mut Vec<i32>) -> Result<usize> {
        let mut got = 0;
        while got < max_frames {
            if self.pending_pos >= self.pending.len() {
                if self.done {
                    break;
                }
                let buffer = std::mem::take(&mut self.decode_buf);
                match self
                    .reader
                    .blocks()
                    .read_next_or_eof(buffer)
                    .map_err(|e| FlacError::Decode(e.to_string()))?
                {
                    None => {
                        self.done = true;
                        break;
                    }
                    Some(block) => {
                        self.pending.clear();
                        self.pending_pos = 0;
                        if self.channels == 1 {
                            self.pending.extend_from_slice(block.channel(0));
                        } else {
                            for i in 0..block.duration() {
                                for ch in 0..self.channels {
                                    self.pending.push(block.sample(ch as u32, i));
                                }
                            }
                        }
                        self.decode_buf = block.into_buffer();
                    }
                }
                continue;
            }
            let avail = (self.pending.len() - self.pending_pos) / self.channels;
            let take = avail.min(max_frames - got);
            let end = self.pending_pos + take * self.channels;
            out.extend_from_slice(&self.pending[self.pending_pos..end]);
            self.pending_pos = end;
            got += take;
        }
        Ok(got)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A deterministic pseudo-random 16-bit signal (speech-ish: a few sines
    /// plus noise, with clipping excursions and digital silence).
    pub(crate) fn test_signal(frames: usize, channels: usize, seed: u64) -> Vec<i16> {
        let mut x = seed | 1;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        (0..frames * channels)
            .map(|i| {
                let t = (i / channels) as f64;
                if (i / channels) % 9000 < 700 {
                    return 0; // a stretch of digital silence
                }
                let tone = 9000.0 * (t * 0.031).sin() + 4000.0 * (t * 0.173).cos();
                let noise = (next() % 2001) as f64 - 1000.0;
                let v = tone + noise;
                if (i / channels) % 5000 == 17 {
                    return if i % 2 == 0 { i16::MAX } else { i16::MIN };
                }
                v.clamp(i16::MIN as f64, i16::MAX as f64) as i16
            })
            .collect()
    }

    pub(crate) fn write_wav(path: &Path, samples: &[i16], channels: u16, rate: u32) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }

    fn go() -> impl FnMut() -> bool {
        || true
    }

    #[test]
    fn mono_roundtrip_is_bit_exact_and_smaller() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        let flac = dir.path().join("a.flac");
        let s = test_signal(48_000 * 3 + 123, 1, 7);
        write_wav(&wav, &s, 1, 48_000);
        let out = encode_pcm16_wav_to_flac(&wav, &flac, &mut go()).unwrap();
        assert_eq!(out.frames, s.len() as u64);
        assert!(is_flac(&flac) && !is_flac(&wav));
        assert_eq!(verify_flac_matches_wav(&flac, &wav, &mut go()).unwrap(), s.len() as u64);
        assert!(out.flac_bytes < std::fs::metadata(&wav).unwrap().len());

        // An independent full decode agrees too.
        let mut r = claxon::FlacReader::open(&flac).unwrap();
        let decoded: Vec<i16> = r.samples().map(|v| v.unwrap() as i16).collect();
        assert_eq!(decoded, s);
    }

    #[test]
    fn stereo_and_odd_rates_roundtrip() {
        for (rate, ch, len) in [(44_100u32, 2u16, 10_000usize), (16_000, 1, 4096), (22_050, 2, 1), (8_000, 1, 0)] {
            let dir = tempfile::tempdir().unwrap();
            let wav = dir.path().join("a.wav");
            let flac = dir.path().join("a.flac");
            let s = test_signal(len, ch as usize, u64::from(rate));
            write_wav(&wav, &s, ch, rate);
            encode_pcm16_wav_to_flac(&wav, &flac, &mut go()).unwrap();
            assert_eq!(
                verify_flac_matches_wav(&flac, &wav, &mut go()).unwrap(),
                len as u64,
                "{rate} Hz / {ch} ch / {len}"
            );
        }
    }

    #[test]
    fn verify_catches_any_difference() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        let flac = dir.path().join("a.flac");
        let s = test_signal(20_000, 1, 3);
        write_wav(&wav, &s, 1, 48_000);
        encode_pcm16_wav_to_flac(&wav, &flac, &mut go()).unwrap();
        let good = std::fs::read(&flac).unwrap();

        // A flipped bit in the audio frames (CRC or sample mismatch).
        let mut bad = good.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0x10;
        std::fs::write(&flac, &bad).unwrap();
        assert!(verify_flac_matches_wav(&flac, &wav, &mut go()).is_err());

        // A truncated FLAC.
        std::fs::write(&flac, &good[..good.len() - 100]).unwrap();
        assert!(verify_flac_matches_wav(&flac, &wav, &mut go()).is_err());

        // One WAV sample changed by one LSB.
        std::fs::write(&flac, &good).unwrap();
        let mut s2 = s.clone();
        s2[12_345] = s2[12_345].wrapping_add(1);
        write_wav(&wav, &s2, 1, 48_000);
        assert!(matches!(
            verify_flac_matches_wav(&flac, &wav, &mut go()),
            Err(FlacError::Mismatch(_))
        ));

        // A WAV with one sample more.
        let mut s3 = s.clone();
        s3.push(0);
        write_wav(&wav, &s3, 1, 48_000);
        assert!(verify_flac_matches_wav(&flac, &wav, &mut go()).is_err());
    }

    #[test]
    fn interrupted_encode_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        write_wav(&wav, &test_signal(BLOCK_SIZE * CHECKPOINT_EVERY * 2, 1, 1), 1, 48_000);
        let mut calls = 0;
        let mut stop_second = || {
            calls += 1;
            calls < 2
        };
        let r = encode_pcm16_wav_to_flac(&wav, &dir.path().join("a.flac"), &mut stop_second);
        assert!(matches!(r, Err(FlacError::Interrupted)));
    }

    fn raw_wav(chunks: &[(&[u8; 4], Vec<u8>)], riff_size: Option<u32>, trailing: &[u8]) -> Vec<u8> {
        let mut body = b"WAVE".to_vec();
        for (id, data) in chunks {
            body.extend_from_slice(*id);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
            if data.len() % 2 == 1 {
                body.push(0);
            }
        }
        body.extend_from_slice(trailing);
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&riff_size.unwrap_or(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn fmt_pcm(tag: u16, ch: u16, rate: u32, bits: u16) -> Vec<u8> {
        let align = ch * bits / 8;
        let mut f = Vec::new();
        f.extend_from_slice(&tag.to_le_bytes());
        f.extend_from_slice(&ch.to_le_bytes());
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
        f.extend_from_slice(&align.to_le_bytes());
        f.extend_from_slice(&bits.to_le_bytes());
        f
    }

    #[test]
    fn inspect_accepts_only_the_layout_the_app_writes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.wav");
        let data = vec![1u8, 0, 2, 0, 3, 0, 4, 0];
        let check = |bytes: Vec<u8>| {
            std::fs::write(&p, bytes).unwrap();
            inspect_pcm16_wav(&p)
        };
        let ok = check(raw_wav(&[(b"fmt ", fmt_pcm(1, 1, 48_000, 16)), (b"data", data.clone())], None, &[]))
            .unwrap();
        assert_eq!(ok.frames(), 4);
        assert_eq!(ok.data_offset, 44);

        // Unfinalized: RIFF size still 0 (a crash before finalize).
        assert!(check(raw_wav(&[(b"fmt ", fmt_pcm(1, 1, 48_000, 16)), (b"data", data.clone())], Some(0), &[])).is_err());
        // Bytes after the data chunk the header doesn't account for.
        assert!(check(raw_wav(&[(b"fmt ", fmt_pcm(1, 1, 48_000, 16)), (b"data", data.clone())], None, &[9, 9])).is_err());
        // A metadata chunk FLAC wouldn't keep.
        assert!(check(raw_wav(&[(b"fmt ", fmt_pcm(1, 1, 48_000, 16)), (b"LIST", vec![0; 4]), (b"data", data.clone())], None, &[])).is_err());
        // 24-bit, float, and 8-bit audio.
        assert!(check(raw_wav(&[(b"fmt ", fmt_pcm(1, 1, 48_000, 24)), (b"data", vec![0; 6])], None, &[])).is_err());
        assert!(check(raw_wav(&[(b"fmt ", fmt_pcm(3, 1, 48_000, 32)), (b"data", vec![0; 8])], None, &[])).is_err());
        assert!(check(raw_wav(&[(b"fmt ", fmt_pcm(1, 1, 48_000, 8)), (b"data", vec![0; 8])], None, &[])).is_err());
        // Half a sample frame.
        assert!(check(raw_wav(&[(b"fmt ", fmt_pcm(1, 2, 48_000, 16)), (b"data", vec![0; 6])], None, &[])).is_err());
        // Not a WAV at all.
        assert!(check(b"fLaC\0\0\0\0\0\0\0\0".to_vec()).is_err());
    }

    #[test]
    fn hound_extensible_header_is_accepted() {
        // hound writes WAVE_FORMAT_EXTENSIBLE for more than two channels.
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        let flac = dir.path().join("a.flac");
        let s = test_signal(5000, 4, 9);
        write_wav(&wav, &s, 4, 48_000);
        assert_eq!(inspect_pcm16_wav(&wav).unwrap().channels, 4);
        encode_pcm16_wav_to_flac(&wav, &flac, &mut go()).unwrap();
        verify_flac_matches_wav(&flac, &wav, &mut go()).unwrap();
    }

    #[test]
    fn streaming_flac_reader_returns_every_sample() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        let flac = dir.path().join("a.flac");
        let s = test_signal(BLOCK_SIZE * 3 + 77, 2, 5);
        write_wav(&wav, &s, 2, 44_100);
        encode_pcm16_wav_to_flac(&wav, &flac, &mut go()).unwrap();
        let mut r = FlacSamples::open(&flac).unwrap();
        assert_eq!((r.sample_rate(), r.channels(), r.frames()), (44_100, 2, (s.len() / 2) as u64));
        let mut all = Vec::new();
        // odd read sizes cross block boundaries
        while r.read_frames(1000, &mut all).unwrap() > 0 {}
        assert_eq!(all, s.iter().map(|&v| i32::from(v)).collect::<Vec<_>>());
    }
}
