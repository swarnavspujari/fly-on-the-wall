//! cpal-based `AudioCapture`. On Windows/WASAPI, building an *input* stream
//! on an *output* device transparently enables loopback capture — that is
//! how the "them" (system audio) channel is recorded without any virtual
//! cable. Mic and system are written as separate mono WAVs at their native
//! rates; stop() then renders the 16 kHz mono mixdown for the ASR pipeline.
//!
//! cpal streams are !Send, so a dedicated audio thread owns them; the
//! session handle only talks to that thread over channels.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::mix::{write_mixdown, write_playback_mix};
use crate::{
    AudioCapture, AudioDevice, AudioError, CaptureConfig, CaptureOutput, CaptureSession,
    CaptureState, Result,
};

pub struct CpalAudioCapture;

impl CpalAudioCapture {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CpalAudioCapture {
    fn default() -> Self {
        Self::new()
    }
}

fn device_name(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|desc| desc.name().to_string())
}

impl AudioCapture for CpalAudioCapture {
    fn list_mic_devices(&self) -> Result<Vec<AudioDevice>> {
        let host = cpal::default_host();
        let default_name = host
            .default_input_device()
            .and_then(|d| device_name(&d))
            .unwrap_or_default();
        let mut out = Vec::new();
        let devices = host
            .input_devices()
            .map_err(|e| AudioError::Backend(e.to_string()))?;
        for d in devices {
            if let Some(name) = device_name(&d) {
                out.push(AudioDevice {
                    id: name.clone(),
                    is_default: name == default_name,
                    name,
                });
            }
        }
        Ok(out)
    }

    fn supports_system_loopback(&self) -> bool {
        #[cfg(target_os = "macos")]
        return crate::coreaudio_tap::supported(); // process taps, 14.2+
        #[cfg(not(target_os = "macos"))]
        cfg!(any(target_os = "windows", target_os = "linux"))
    }

    fn capture_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        #[cfg(target_os = "windows")]
        if let Some((volume, muted)) = crate::win_volume::default_output_volume() {
            if muted {
                warnings.push(
                    "System output is muted — the other participants' audio will record as \
                     silence. Unmute your speakers."
                        .to_string(),
                );
            } else if volume < 0.01 {
                warnings.push(
                    "System volume is at 0% — the other participants' audio will record as \
                     silence. Raise your system volume."
                        .to_string(),
                );
            }
        }
        warnings
    }

    fn start(&self, cfg: CaptureConfig) -> Result<Box<dyn CaptureSession>> {
        std::fs::create_dir_all(&cfg.out_dir)?;
        let clock = Arc::new(Clock::new());
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Command>();
        let (ready_tx, ready_rx) =
            std::sync::mpsc::channel::<Result<(Vec<String>, CaptureHealth)>>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<Result<CaptureOutput>>();

        let thread_clock = clock.clone();
        std::thread::Builder::new()
            .name("fly-audio-capture".into())
            .spawn(move || audio_thread(cfg, thread_clock, cmd_rx, ready_tx, done_tx))
            .map_err(|e| AudioError::Backend(e.to_string()))?;

        // Surface stream-construction errors synchronously to the caller;
        // a degraded start (mic ok, loopback failed) comes back as warnings.
        let (warnings, health) = ready_rx
            .recv()
            .map_err(|_| AudioError::Backend("audio thread died during startup".into()))??;

        Ok(Box::new(CpalSession {
            cmd_tx,
            done_rx,
            clock,
            state: CaptureState::Recording,
            warnings,
            health,
        }))
    }
}

enum Command {
    Pause,
    Resume,
    Stop,
}

struct CpalSession {
    cmd_tx: Sender<Command>,
    done_rx: Receiver<Result<CaptureOutput>>,
    clock: Arc<Clock>,
    state: CaptureState,
    /// Startup degradations (loopback failed → mic-only), set once.
    warnings: Vec<String>,
    /// Dynamic capture health, re-evaluated on every `warnings()` poll.
    health: CaptureHealth,
}

/// Dynamic capture health readable from the session while the audio thread
/// owns the channels: today, the macOS tap's silence detection (an
/// un-entitled build's tap delivers only zeros — see coreaudio_tap.rs);
/// inert on the other platforms.
#[derive(Clone, Default)]
struct CaptureHealth {
    #[cfg(target_os = "macos")]
    tap: Option<crate::coreaudio_tap::TapHealth>,
}

impl CaptureHealth {
    fn warnings(&self) -> Vec<String> {
        #[cfg(target_os = "macos")]
        if let Some(w) = self.tap.as_ref().and_then(|t| t.silence_warning()) {
            return vec![w];
        }
        Vec::new()
    }
}

impl CaptureSession for CpalSession {
    fn pause(&mut self) -> Result<()> {
        if self.state != CaptureState::Recording {
            return Err(AudioError::InvalidState("not recording".into()));
        }
        self.cmd_tx
            .send(Command::Pause)
            .map_err(|_| AudioError::Backend("audio thread gone".into()))?;
        self.state = CaptureState::Paused;
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if self.state != CaptureState::Paused {
            return Err(AudioError::InvalidState("not paused".into()));
        }
        self.cmd_tx
            .send(Command::Resume)
            .map_err(|_| AudioError::Backend("audio thread gone".into()))?;
        self.state = CaptureState::Recording;
        Ok(())
    }

    fn stop(self: Box<Self>) -> Result<CaptureOutput> {
        self.cmd_tx
            .send(Command::Stop)
            .map_err(|_| AudioError::Backend("audio thread gone".into()))?;
        self.done_rx
            .recv()
            .map_err(|_| AudioError::Backend("audio thread died before finishing".into()))?
    }

    fn state(&self) -> CaptureState {
        self.state
    }

    fn elapsed_ms(&self) -> u64 {
        self.clock.elapsed_ms()
    }

    fn warnings(&self) -> Vec<String> {
        let mut all = self.warnings.clone();
        all.extend(self.health.warnings());
        all
    }
}

/// Pause-aware wall clock shared with the stream callbacks.
pub(crate) struct Clock {
    accum_ms: AtomicU64,
    running: AtomicBool,
    last_resume: Mutex<Instant>,
}

impl Clock {
    pub(crate) fn new() -> Self {
        Self {
            accum_ms: AtomicU64::new(0),
            running: AtomicBool::new(true),
            last_resume: Mutex::new(Instant::now()),
        }
    }

    pub(crate) fn pause(&self) {
        if self.running.swap(false, Ordering::SeqCst) {
            let since = self.last_resume.lock().unwrap().elapsed().as_millis() as u64;
            self.accum_ms.fetch_add(since, Ordering::SeqCst);
        }
    }

    pub(crate) fn resume(&self) {
        if !self.running.swap(true, Ordering::SeqCst) {
            *self.last_resume.lock().unwrap() = Instant::now();
        }
    }

    pub(crate) fn elapsed_ms(&self) -> u64 {
        let base = self.accum_ms.load(Ordering::SeqCst);
        if self.running.load(Ordering::SeqCst) {
            base + self.last_resume.lock().unwrap().elapsed().as_millis() as u64
        } else {
            base
        }
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

pub(crate) type SharedWriter =
    Arc<Mutex<Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>>>;

struct ChannelRecorder {
    _stream: cpal::Stream,
    writer: SharedWriter,
    path: PathBuf,
    rate: u32,
    written: Arc<AtomicU64>,
}

impl ChannelRecorder {
    /// Top the channel up with silence to `target_ms` — a loopback stream
    /// delivers nothing while the system is idle, so without this the
    /// channel's timeline would end early.
    fn pad_tail_to(&self, target_ms: u64) {
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

/// The system-audio channel differs per OS: WASAPI loopback rides a cpal
/// stream; Linux records the Pulse/PipeWire monitor on its own thread;
/// macOS taps every process's output via a Core Audio process tap.
enum LoopbackChannel {
    Cpal(ChannelRecorder),
    #[cfg(target_os = "linux")]
    Pulse(crate::pulse_loopback::PulseRecorder),
    #[cfg(target_os = "macos")]
    CoreAudioTap(crate::coreaudio_tap::TapRecorder),
}

impl LoopbackChannel {
    fn pad_tail_to(&self, target_ms: u64) {
        match self {
            LoopbackChannel::Cpal(r) => r.pad_tail_to(target_ms),
            #[cfg(target_os = "linux")]
            LoopbackChannel::Pulse(r) => r.pad_tail_to(target_ms),
            #[cfg(target_os = "macos")]
            LoopbackChannel::CoreAudioTap(r) => r.pad_tail_to(target_ms),
        }
    }
    fn writer(&self) -> &SharedWriter {
        match self {
            LoopbackChannel::Cpal(r) => &r.writer,
            #[cfg(target_os = "linux")]
            LoopbackChannel::Pulse(r) => &r.writer,
            #[cfg(target_os = "macos")]
            LoopbackChannel::CoreAudioTap(r) => &r.writer,
        }
    }
    fn path(&self) -> &PathBuf {
        match self {
            LoopbackChannel::Cpal(r) => &r.path,
            #[cfg(target_os = "linux")]
            LoopbackChannel::Pulse(r) => &r.path,
            #[cfg(target_os = "macos")]
            LoopbackChannel::CoreAudioTap(r) => &r.path,
        }
    }
    fn cpal_stream(&self) -> Option<&cpal::Stream> {
        match self {
            LoopbackChannel::Cpal(r) => Some(&r._stream),
            #[cfg(target_os = "linux")]
            LoopbackChannel::Pulse(_) => None,
            #[cfg(target_os = "macos")]
            LoopbackChannel::CoreAudioTap(_) => None,
        }
    }
}

fn audio_thread(
    cfg: CaptureConfig,
    clock: Arc<Clock>,
    cmd_rx: Receiver<Command>,
    ready_tx: Sender<Result<(Vec<String>, CaptureHealth)>>,
    done_tx: Sender<Result<CaptureOutput>>,
) {
    let host = cpal::default_host();

    // --- mic channel ---
    let mic = match build_mic_recorder(&host, &cfg, &clock) {
        Ok(r) => r,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    // --- system loopback channel (best effort: recording proceeds mic-only
    //     if loopback can't be built, e.g. non-Windows or exotic devices —
    //     but the degradation must be visible to the person recording) ---
    let mut warnings = Vec::new();
    let system: Option<LoopbackChannel> = if cfg.capture_system {
        match build_loopback_channel(&host, &cfg, &clock) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!("system loopback unavailable, recording mic only: {e}");
                warnings.push(format!(
                    "System audio capture couldn't start ({e}) — only your microphone is \
                     being recorded, so the other participants won't be in this recording."
                ));
                None
            }
        }
    } else {
        None
    };

    #[cfg(target_os = "macos")]
    let health = CaptureHealth {
        tap: system.as_ref().and_then(|s| match s {
            LoopbackChannel::CoreAudioTap(r) => Some(r.health()),
            _ => None,
        }),
    };
    #[cfg(not(target_os = "macos"))]
    let health = CaptureHealth::default();
    let _ = ready_tx.send(Ok((warnings, health)));

    let streams: Vec<&cpal::Stream> = std::iter::once(&mic._stream)
        .chain(system.iter().filter_map(|s| s.cpal_stream()))
        .collect();
    for s in &streams {
        if let Err(e) = s.play() {
            let _ = done_tx.send(Err(AudioError::Backend(e.to_string())));
            return;
        }
    }

    // command loop
    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Command::Pause => {
                for s in &streams {
                    let _ = s.pause();
                }
                clock.pause();
            }
            Command::Resume => {
                clock.resume();
                for s in &streams {
                    let _ = s.play();
                }
            }
            Command::Stop => break,
        }
    }
    clock.pause();
    let duration_ms = clock.elapsed_ms();
    if let Some(s) = &system {
        s.pad_tail_to(duration_ms);
    }

    // finalize writers, then drop streams
    let finalize = |w: &SharedWriter| -> Result<()> {
        if let Some(writer) = w.lock().unwrap().take() {
            writer
                .finalize()
                .map_err(|e| AudioError::Backend(e.to_string()))?;
        }
        Ok(())
    };
    let mic_path = mic.path.clone();
    let system_path = system.as_ref().map(|s| s.path().clone());
    let fin = finalize(&mic.writer).and_then(|_| match &system {
        Some(s) => finalize(s.writer()),
        None => Ok(()),
    });
    drop(mic);
    drop(system);
    if let Err(e) = fin {
        let _ = done_tx.send(Err(e));
        return;
    }

    // 16 kHz mono mixdown for the ASR pipeline (untouched — ASR depends on it)
    let mixed_path = cfg.out_dir.join(format!("{}.mixed.wav", cfg.base_name));
    let result = write_mixdown(Some(&mic_path), system_path.as_deref(), &mixed_path).map(|_| {
        // Full-quality listening copy: only worth a second file when there
        // are two tracks to combine; a mic-only recording's best listening
        // copy IS the native-rate mic WAV.
        let playback_path = match &system_path {
            Some(sys) => {
                let out = cfg.out_dir.join(format!("{}.playback.wav", cfg.base_name));
                match write_playback_mix(Some(&mic_path), Some(sys), &out) {
                    Ok(_) => Some(out),
                    Err(e) => {
                        tracing::warn!("playback mix failed (falling back to ASR mix): {e}");
                        None
                    }
                }
            }
            None => Some(mic_path.clone()),
        };
        CaptureOutput {
            mic_path: Some(mic_path),
            system_path,
            mixed_path: Some(mixed_path),
            playback_path,
            duration_ms,
        }
    });
    let _ = done_tx.send(result);
}

fn build_mic_recorder(
    host: &cpal::Host,
    cfg: &CaptureConfig,
    clock: &Arc<Clock>,
) -> Result<ChannelRecorder> {
    let device = match &cfg.mic_device_id {
        Some(id) => host
            .input_devices()
            .map_err(|e| AudioError::Backend(e.to_string()))?
            .find(|d| device_name(d).as_deref() == Some(id))
            .ok_or_else(|| AudioError::DeviceNotFound(id.clone()))?,
        None => host
            .default_input_device()
            .ok_or_else(|| AudioError::DeviceNotFound("default microphone".into()))?,
    };
    let config = device
        .default_input_config()
        .map_err(|e| AudioError::Backend(e.to_string()))?;
    let path = cfg.out_dir.join(format!("{}.mic.wav", cfg.base_name));
    build_recorder(&device, config, path, clock.clone(), false)
}

fn build_loopback_channel(
    host: &cpal::Host,
    cfg: &CaptureConfig,
    clock: &Arc<Clock>,
) -> Result<LoopbackChannel> {
    let path = cfg.out_dir.join(format!("{}.system.wav", cfg.base_name));
    if cfg!(target_os = "windows") {
        let device = host
            .default_output_device()
            .ok_or_else(|| AudioError::DeviceNotFound("default output".into()))?;
        // WASAPI loopback uses the OUTPUT device's render format for an input stream.
        let config = device
            .default_output_config()
            .map_err(|e| AudioError::Backend(e.to_string()))?;
        return build_recorder(&device, config, path, clock.clone(), true)
            .map(LoopbackChannel::Cpal);
    }
    #[cfg(target_os = "linux")]
    {
        return crate::pulse_loopback::PulseRecorder::start(path, clock.clone())
            .map(LoopbackChannel::Pulse);
    }
    #[cfg(target_os = "macos")]
    {
        // Core Audio process tap (14.2+). Any failure — older macOS, denied
        // system-audio permission, aggregate refusing to build — falls back
        // to mic-only with the existing degradation warning upstream.
        return crate::coreaudio_tap::TapRecorder::start(path, clock.clone())
            .map(LoopbackChannel::CoreAudioTap);
    }
    #[allow(unreachable_code)]
    Err(AudioError::LoopbackUnsupported)
}

/// Build a stream that downmixes every callback buffer to mono i16 and
/// appends it to a WAV. For loopback (`pad_to_clock`), silence is inserted
/// when the render pipeline goes idle so the channel's timeline stays
/// aligned with wall clock (WASAPI stops delivering packets when nothing
/// is playing).
fn build_recorder(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    path: PathBuf,
    clock: Arc<Clock>,
    pad_to_clock: bool,
) -> Result<ChannelRecorder> {
    let stream_config: cpal::StreamConfig = config.config();
    let channels = stream_config.channels.max(1) as usize;
    let rate: u32 = stream_config.sample_rate;

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let writer: SharedWriter = Arc::new(Mutex::new(Some(
        hound::WavWriter::create(&path, spec).map_err(|e| AudioError::Backend(e.to_string()))?,
    )));
    let written = Arc::new(AtomicU64::new(0));

    macro_rules! build_for {
        ($t:ty) => {{
            let writer = writer.clone();
            let written = written.clone();
            let clock = clock.clone();
            let err_path = path.clone();
            let err_fn = move |e: cpal::Error| {
                tracing::error!("stream error on {}: {e}", err_path.display())
            };
            device
                .build_input_stream(
                    stream_config.clone(),
                    move |data: &[$t], _| {
                        let mut guard = writer.lock().unwrap();
                        let Some(w) = guard.as_mut() else { return };
                        if pad_to_clock {
                            let expected = clock.elapsed_ms() * rate as u64 / 1000;
                            let have =
                                written.load(Ordering::Relaxed) + (data.len() / channels) as u64;
                            if expected > have + rate as u64 / 5 {
                                let pad = expected - have;
                                for _ in 0..pad {
                                    let _ = w.write_sample(0i16);
                                }
                                written.fetch_add(pad, Ordering::Relaxed);
                            }
                        }
                        for frame in data.chunks(channels) {
                            let sum: f32 = frame
                                .iter()
                                .map(|s| <f32 as cpal::FromSample<$t>>::from_sample_(*s))
                                .sum();
                            let mono = (sum / channels as f32).clamp(-1.0, 1.0);
                            let _ = w.write_sample((mono * i16::MAX as f32) as i16);
                        }
                        written.fetch_add((data.len() / channels) as u64, Ordering::Relaxed);
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| AudioError::Backend(e.to_string()))?
        }};
    }

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build_for!(f32),
        cpal::SampleFormat::I16 => build_for!(i16),
        cpal::SampleFormat::U16 => build_for!(u16),
        cpal::SampleFormat::I32 => build_for!(i32),
        other => {
            return Err(AudioError::Backend(format!(
                "unsupported sample format {other:?}"
            )))
        }
    };

    Ok(ChannelRecorder {
        _stream: stream,
        writer,
        path,
        rate,
        written,
    })
}
