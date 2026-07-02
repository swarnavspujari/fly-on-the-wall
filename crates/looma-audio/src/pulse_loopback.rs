//! Linux-only: system-audio loopback by recording the default sink's monitor
//! source over PulseAudio's simple API (PipeWire serves the same protocol via
//! pipewire-pulse, so this covers both stacks).
//!
//! Mirrors the WASAPI loopback discipline in `cpal_backend`: mono i16 WAV at
//! a fixed rate, pad-to-clock so the "them" channel timeline stays wall-clock
//! aligned, and pause implemented by discarding while the shared clock is
//! stopped (the paused stretch must not appear in the file).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

use crate::cpal_backend::{Clock, SharedWriter};
use crate::{AudioError, Result};

const RATE: u32 = 48_000;
/// 100 ms fragments: mono s16 at 48 kHz.
const FRAG_BYTES: usize = (RATE as usize / 10) * 2;

pub struct PulseRecorder {
    pub writer: SharedWriter,
    pub path: PathBuf,
    pub rate: u32,
    pub written: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl PulseRecorder {
    /// Connect to the default sink's monitor and start the reader thread.
    /// Fails cleanly (mic-only fallback upstream) when no Pulse/PipeWire
    /// server is available.
    pub fn start(path: PathBuf, clock: Arc<Clock>) -> Result<Self> {
        let spec = Spec {
            format: Format::S16le,
            channels: 1,
            rate: RATE,
        };
        debug_assert!(spec.is_valid());
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: FRAG_BYTES as u32,
        };
        let simple = Simple::new(
            None,
            "Looma",
            Direction::Record,
            Some("@DEFAULT_MONITOR@"),
            "system audio",
            &spec,
            None,
            Some(&attr),
        )
        .map_err(|e| AudioError::Backend(format!("pulse monitor unavailable: {e}")))?;

        let wav_spec = hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer: SharedWriter = Arc::new(Mutex::new(Some(
            hound::WavWriter::create(&path, wav_spec)
                .map_err(|e| AudioError::Backend(e.to_string()))?,
        )));
        let written = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let t_writer = writer.clone();
        let t_written = written.clone();
        let t_stop = stop.clone();
        let worker = std::thread::Builder::new()
            .name("looma-pulse-loopback".into())
            .spawn(move || {
                let mut buf = vec![0u8; FRAG_BYTES];
                while !t_stop.load(Ordering::Relaxed) {
                    if let Err(e) = simple.read(&mut buf) {
                        tracing::warn!("pulse loopback read failed, stopping: {e}");
                        break;
                    }
                    // Paused: keep draining the server but drop the audio so
                    // the paused stretch never reaches the file.
                    if !clock.is_running() {
                        continue;
                    }
                    let mut guard = t_writer.lock().unwrap();
                    let Some(w) = guard.as_mut() else { break };
                    let samples = buf.len() / 2;
                    let expected = clock.elapsed_ms() * RATE as u64 / 1000;
                    let have = t_written.load(Ordering::Relaxed) + samples as u64;
                    if expected > have + RATE as u64 / 5 {
                        let pad = expected - have;
                        for _ in 0..pad {
                            let _ = w.write_sample(0i16);
                        }
                        t_written.fetch_add(pad, Ordering::Relaxed);
                    }
                    for pair in buf.chunks_exact(2) {
                        let _ = w.write_sample(i16::from_le_bytes([pair[0], pair[1]]));
                    }
                    t_written.fetch_add(samples as u64, Ordering::Relaxed);
                }
            })
            .map_err(|e| AudioError::Backend(e.to_string()))?;

        Ok(Self {
            writer,
            path,
            rate: RATE,
            written,
            stop,
            worker: Some(worker),
        })
    }

    /// Signal the reader to stop and wait for it (bounded by one fragment).
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }

    pub fn pad_tail_to(&self, target_ms: u64) {
        let expected = target_ms * self.rate as u64 / 1000;
        let have = self.written.load(Ordering::Relaxed);
        if expected > have {
            if let Some(w) = self.writer.lock().unwrap().as_mut() {
                for _ in 0..(expected - have) {
                    let _ = w.write_sample(0i16);
                }
            }
            self.written.fetch_add(expected - have, Ordering::Relaxed);
        }
    }
}

impl Drop for PulseRecorder {
    fn drop(&mut self) {
        self.shutdown();
    }
}
