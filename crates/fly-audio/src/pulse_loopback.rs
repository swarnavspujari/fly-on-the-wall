//! Linux-only: system-audio loopback by recording the default sink's monitor
//! source over PulseAudio's simple API (PipeWire serves the same protocol via
//! pipewire-pulse, so this covers both stacks).
//!
//! Mirrors the WASAPI loopback discipline in `cpal_backend`: mono i16 WAV at
//! a fixed rate, pad-to-clock so the "them" channel timeline stays wall-clock
//! aligned, and pause implemented by discarding while the shared clock is
//! stopped (the paused stretch must not appear in the file). Like every
//! channel, it hands samples to a ring and the writer thread does the file
//! I/O (`sink.rs`), so a slow disk can't hold up the monitor reads.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

use crate::cpal_backend::{idle_pad_frames, Clock};
use crate::sink::{self, ChannelSink};
use crate::{AudioError, Result};

const RATE: u32 = 48_000;
/// 100 ms fragments: mono s16 at 48 kHz.
const FRAG_BYTES: usize = (RATE as usize / 10) * 2;

pub struct PulseRecorder {
    pub path: PathBuf,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl PulseRecorder {
    /// Connect to the default sink's monitor and start the reader thread.
    /// Fails cleanly (mic-only fallback upstream) when no Pulse/PipeWire
    /// server is available.
    pub fn start(path: PathBuf, clock: Arc<Clock>) -> Result<(Self, ChannelSink)> {
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
            "Fly on the Wall",
            Direction::Record,
            Some("@DEFAULT_MONITOR@"),
            "system audio",
            &spec,
            None,
            Some(&attr),
        )
        .map_err(|e| AudioError::Backend(format!("pulse monitor unavailable: {e}")))?;

        let (mut producer, sink) = sink::channel(path.clone(), RATE)?;
        let stop = Arc::new(AtomicBool::new(false));
        let t_stop = stop.clone();
        let worker = std::thread::Builder::new()
            .name("flyonthewall-pulse-loopback".into())
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
                    let samples = (buf.len() / 2) as u64;
                    let now_ms = clock.elapsed_ms();
                    let pad = idle_pad_frames(
                        clock.expected_frames(RATE),
                        producer.timeline() + samples,
                        RATE,
                    );
                    producer.silence(pad, false, now_ms);
                    let (pairs, _tail) = buf.as_chunks::<2>();
                    for pair in pairs {
                        producer.push(i16::from_le_bytes(*pair));
                    }
                    producer.end_packet();
                    producer.note_time(now_ms);
                }
            })
            .map_err(|e| AudioError::Backend(e.to_string()))?;

        Ok((
            Self {
                path,
                stop,
                worker: Some(worker),
            },
            sink,
        ))
    }

    /// Signal the reader to stop and wait for it (bounded by one fragment).
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

impl Drop for PulseRecorder {
    fn drop(&mut self) {
        self.shutdown();
    }
}
