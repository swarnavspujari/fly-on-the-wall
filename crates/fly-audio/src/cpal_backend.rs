//! cpal-based `AudioCapture`. On Windows/WASAPI, building an *input* stream
//! on an *output* device transparently enables loopback capture — that is
//! how the "them" (system audio) channel is recorded without any virtual
//! cable. Mic and system are written as separate mono WAVs at their native
//! rates; stop() then renders the 16 kHz mono mixdown for the ASR pipeline.
//!
//! cpal streams are !Send, so a dedicated audio thread owns them; the
//! session handle only talks to that thread over channels.
//!
//! The capture callbacks never touch the disk: they push mono samples into
//! a lock-free ring per channel and a writer thread does the file I/O
//! (`sink.rs`). Audio that goes missing anyway — the OS buffer overflowed
//! before the callback ran, or the ring filled behind a stalled disk — is
//! replaced by the same length of silence (the channels stay aligned),
//! counted per channel, reported live through `warnings()` and returned in
//! [`CaptureOutput::health`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::mix::{write_mixdown, write_playback_mix};
use crate::sink::{self, ChannelProducer, ChannelSink, ChannelStats, WriterThread};
use crate::{
    AudioCapture, AudioDevice, AudioError, CaptureConfig, CaptureHealthReport, CaptureOutput,
    CaptureSession, CaptureState, Result,
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
        #[allow(unused_mut)] // only the Windows volume check pushes
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
/// owns the channels: lost-audio counters for both channels, and on macOS
/// the tap's silence detection (an un-entitled build's tap delivers only
/// zeros — see coreaudio_tap.rs).
#[derive(Clone, Default)]
struct CaptureHealth {
    mic: Option<(Arc<ChannelStats>, u32)>,
    system: Option<(Arc<ChannelStats>, u32)>,
    #[cfg(target_os = "macos")]
    tap: Option<crate::coreaudio_tap::TapHealth>,
}

impl CaptureHealth {
    fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (stats, rate, mic) in [
            self.mic.as_ref().map(|(s, r)| (s, *r, true)),
            self.system.as_ref().map(|(s, r)| (s, *r, false)),
        ]
        .into_iter()
        .flatten()
        {
            let lost_ms = stats.lost_frames() * 1000 / rate.max(1) as u64;
            if lost_ms >= crate::LOST_AUDIO_WARN_MS {
                let at = stats
                    .last_loss_at_ms
                    .load(Ordering::Relaxed)
                    .saturating_sub(1);
                out.push(lost_audio_warning(mic, lost_ms, at));
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(w) = self.tap.as_ref().and_then(|t| t.silence_warning()) {
            out.push(w);
        }
        out
    }

    fn report(&self) -> CaptureHealthReport {
        CaptureHealthReport {
            mic: self.mic.as_ref().map(|(s, r)| s.report(*r)),
            system: self.system.as_ref().map(|(s, r)| s.report(*r)),
        }
    }
}

/// The live "audio was lost" sentence. The recording bar's classifier
/// (`frontend/src/recordingBar.ts`) keys on "was lost" and on "microphone"
/// for the side — keep them, or change the classifier with it.
pub(crate) fn lost_audio_warning(mic: bool, lost_ms: u64, last_at_ms: u64) -> String {
    let secs = (lost_ms as f64 / 1000.0).max(0.1);
    let at = last_at_ms / 1000;
    let when = if at >= 3600 {
        format!("{}:{:02}:{:02}", at / 3600, at / 60 % 60, at % 60)
    } else {
        format!("{}:{:02}", at / 60, at % 60)
    };
    let what = if mic {
        "Your microphone"
    } else {
        "The other participants' audio"
    };
    format!(
        "{what} has gaps: {secs:.1} s was lost because the computer was too busy (most \
         recently at {when}). The rest is still recording — closing heavy apps can help."
    )
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

    fn health(&self) -> CaptureHealthReport {
        self.health.report()
    }
}

/// Pause-aware wall clock shared with the stream callbacks. Lock-free: the
/// callbacks read it on every packet, and a mutex there could leave a
/// real-time thread waiting on a preempted normal-priority one.
pub(crate) struct Clock {
    base: Instant,
    /// Recorded time before the current run, µs.
    accum_us: AtomicU64,
    running: AtomicBool,
    /// When the clock last (re)started, µs since `base`.
    last_resume_us: AtomicU64,
    /// Bumped on every resume, so callbacks know a stream restarted and its
    /// capture timestamps jumped by the paused time (not a loss).
    epoch: AtomicU64,
}

impl Clock {
    pub(crate) fn new() -> Self {
        Self {
            base: Instant::now(),
            accum_us: AtomicU64::new(0),
            running: AtomicBool::new(true),
            last_resume_us: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
        }
    }

    fn now_us(&self) -> u64 {
        self.base.elapsed().as_micros() as u64
    }

    pub(crate) fn pause(&self) {
        if self.running.swap(false, Ordering::SeqCst) {
            let since = self
                .now_us()
                .saturating_sub(self.last_resume_us.load(Ordering::SeqCst));
            self.accum_us.fetch_add(since, Ordering::SeqCst);
        }
    }

    pub(crate) fn resume(&self) {
        if !self.running.load(Ordering::SeqCst) {
            self.last_resume_us.store(self.now_us(), Ordering::SeqCst);
            self.epoch.fetch_add(1, Ordering::SeqCst);
            self.running.store(true, Ordering::SeqCst);
        }
    }

    pub(crate) fn elapsed_ms(&self) -> u64 {
        let base = self.accum_us.load(Ordering::SeqCst);
        let run = if self.running.load(Ordering::SeqCst) {
            self.now_us()
                .saturating_sub(self.last_resume_us.load(Ordering::SeqCst))
        } else {
            0
        };
        (base + run) / 1000
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    pub(crate) fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Frames a channel at `rate` should hold right now.
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    pub(crate) fn expected_frames(&self, rate: u32) -> u64 {
        self.elapsed_ms() * rate as u64 / 1000
    }
}

/// Loopback timeline rule shared by every backend: when the system output
/// goes idle, loopback stops delivering, so silence is added to keep the
/// channel on the recording clock. A stretch shorter than this slack is left
/// alone (start-up and scheduling jitter), as it always has been.
pub(crate) fn idle_pad_frames(expected: u64, have_after_packet: u64, rate: u32) -> u64 {
    if expected > have_after_packet + rate as u64 / 5 {
        expected - have_after_packet
    } else {
        0
    }
}

struct ChannelRecorder {
    _stream: cpal::Stream,
    path: PathBuf,
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
    let (mic, mic_sink) = match build_mic_recorder(&host, &cfg, &clock) {
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
    let system: Option<(LoopbackChannel, ChannelSink)> = if cfg.capture_system {
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

    let health = CaptureHealth {
        mic: Some((mic_sink.stats(), mic_sink.rate())),
        system: system.as_ref().map(|(_, s)| (s.stats(), s.rate())),
        #[cfg(target_os = "macos")]
        tap: system.as_ref().and_then(|(s, _)| match s {
            LoopbackChannel::CoreAudioTap(r) => Some(r.health()),
            _ => None,
        }),
    };
    // Loopback stops delivering while the output is idle: top its file up
    // to the recording's length at the end.
    let system_tail = system.as_ref().map(|(_, s)| (s.tail_target(), s.rate()));
    let (system, system_sink) = match system {
        Some((ch, sink)) => (Some(ch), Some(sink)),
        None => (None, None),
    };
    let writer = match WriterThread::spawn(std::iter::once(mic_sink).chain(system_sink).collect()) {
        Ok(w) => w,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    let _ = ready_tx.send(Ok((warnings, health.clone())));

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
    if let Some((target, rate)) = &system_tail {
        target.store(duration_ms * *rate as u64 / 1000, Ordering::Relaxed);
    }

    // stop every producer (dropping a stream ends its callbacks), then let
    // the writer drain, top up and finalize the files
    let mic_path = mic.path.clone();
    let system_path = system.as_ref().map(|s| s.path().clone());
    drop(streams);
    drop(mic);
    drop(system);
    let fin = writer.finish();
    let report = health.report();
    for (name, ch) in [("mic", &report.mic), ("system", &report.system)] {
        if let Some(c) = ch {
            if c.audio_lost_ms() > 0 || c.write_errors > 0 {
                tracing::warn!(
                    channel = name,
                    lost_ms = c.lost_ms,
                    lost_events = c.lost_events,
                    overflow_ms = c.overflow_ms,
                    write_errors = c.write_errors,
                    xruns = c.xruns,
                    "capture lost audio (filled with silence)"
                );
            } else {
                tracing::info!(
                    channel = name,
                    captured_ms = c.captured_ms,
                    padded_ms = c.padded_ms,
                    xruns = c.xruns,
                    realtime = c.realtime_priority,
                    "capture channel finished with no lost audio"
                );
            }
        }
    }
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
            health: report,
        }
    });
    let _ = done_tx.send(result);
}

fn build_mic_recorder(
    host: &cpal::Host,
    cfg: &CaptureConfig,
    clock: &Arc<Clock>,
) -> Result<(ChannelRecorder, ChannelSink)> {
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
    build_recorder(&device, config, path, clock.clone(), ChannelKind::Mic)
}

fn build_loopback_channel(
    host: &cpal::Host,
    cfg: &CaptureConfig,
    clock: &Arc<Clock>,
) -> Result<(LoopbackChannel, ChannelSink)> {
    let path = cfg.out_dir.join(format!("{}.system.wav", cfg.base_name));
    if cfg!(target_os = "windows") {
        let device = host
            .default_output_device()
            .ok_or_else(|| AudioError::DeviceNotFound("default output".into()))?;
        // WASAPI loopback uses the OUTPUT device's render format for an input stream.
        let config = device
            .default_output_config()
            .map_err(|e| AudioError::Backend(e.to_string()))?;
        return build_recorder(&device, config, path, clock.clone(), ChannelKind::Loopback)
            .map(|(r, s)| (LoopbackChannel::Cpal(r), s));
    }
    #[cfg(target_os = "linux")]
    {
        return crate::pulse_loopback::PulseRecorder::start(path, clock.clone())
            .map(|(r, s)| (LoopbackChannel::Pulse(r), s));
    }
    #[cfg(target_os = "macos")]
    {
        // Core Audio process tap (14.2+). Any failure — older macOS, denied
        // system-audio permission, aggregate refusing to build — falls back
        // to mic-only with the existing degradation warning upstream.
        return crate::coreaudio_tap::TapRecorder::start(path, clock.clone())
            .map(|(r, s)| (LoopbackChannel::CoreAudioTap(r), s));
    }
    #[allow(unreachable_code)]
    Err(AudioError::LoopbackUnsupported)
}

#[derive(Clone, Copy, PartialEq)]
enum ChannelKind {
    Mic,
    Loopback,
}

/// WASAPI's own capture buffer. With the default (≈10 ms on common laptop
/// drivers) any callback delay past one period drops audio; this much
/// headroom rides out scheduler stalls. Shared mode keeps the callback
/// period at the device period regardless, so it costs no latency here.
#[cfg(target_os = "windows")]
const WASAPI_BUFFER_MS: u32 = 400;

/// Capture timestamps are only trusted for gap detection where they were
/// measured to be exact (WASAPI: the device position's QPC time). Elsewhere
/// a jittery timestamp would insert silence into good audio.
const TIMESTAMP_GAPS: bool = cfg!(target_os = "windows");
/// Smaller timestamp jumps are clock jitter, not a dropped packet (one
/// WASAPI period is 10 ms).
const GAP_MIN_NS: i128 = 5_000_000;

/// Per-callback state for capture-timestamp gap detection.
struct GapTracker {
    epoch: u64,
    /// Expected capture time of the next packet's first frame.
    next_capture_ns: Option<i128>,
    last_callback_ns: i128,
    /// Device time of the last packet that waited in our buffer for most of
    /// its length. Our buffer can only overflow after that; a loss with no
    /// such packet in the second before it happened upstream (audio engine
    /// or driver).
    last_backlog_high_ns: Option<i128>,
    /// Peak |sample| of the previous packet (mono i16).
    prev_peak: i16,
}

/// About -60 dBFS: a packet this quiet is silence for the idle rule.
const SILENT_PEAK: i16 = 33;

/// Now on the QPC clock, ns — the clock WASAPI capture timestamps use.
/// User-mode and wait-free, so fine in a callback.
#[cfg(target_os = "windows")]
fn qpc_now_ns(freq: i64) -> i128 {
    use windows::Win32::System::Performance::QueryPerformanceCounter;
    let mut count = 0i64;
    let _ = unsafe { QueryPerformanceCounter(&mut count) };
    count as i128 * 1_000_000_000 / freq.max(1) as i128
}

#[cfg(target_os = "windows")]
fn qpc_frequency() -> i64 {
    use windows::Win32::System::Performance::QueryPerformanceFrequency;
    let mut freq = 0i64;
    let _ = unsafe { QueryPerformanceFrequency(&mut freq) };
    freq
}

#[cfg(not(target_os = "windows"))]
fn qpc_now_ns(_freq: i64) -> i128 {
    0
}

#[cfg(not(target_os = "windows"))]
fn qpc_frequency() -> i64 {
    1
}

/// How long after a near-full buffer a loss is still blamed on this app.
const BACKLOG_BLAME_NS: i128 = 1_000_000_000;
/// A backlog above this is implausible (a bad timestamp), not measured.
const BACKLOG_SANE_NS: i128 = 10_000_000_000;

/// Build a capture stream for one channel. On Windows it first asks for a
/// deep WASAPI buffer; a driver that refuses it still gets a working stream
/// with the default one.
fn build_recorder(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    path: PathBuf,
    clock: Arc<Clock>,
    kind: ChannelKind,
) -> Result<(ChannelRecorder, ChannelSink)> {
    let base: cpal::StreamConfig = config.config();
    #[cfg(target_os = "windows")]
    {
        let mut deep = base;
        deep.buffer_size = cpal::BufferSize::Fixed(base.sample_rate * WASAPI_BUFFER_MS / 1000);
        let high_water_ns = WASAPI_BUFFER_MS as i128 * 1_000_000 * 3 / 4;
        match build_stream(
            device,
            config.sample_format(),
            deep,
            &path,
            &clock,
            kind,
            high_water_ns,
        ) {
            Ok(r) => return Ok(r),
            Err(e) => tracing::warn!(
                "capture with a {WASAPI_BUFFER_MS} ms buffer failed ({e}); using the default"
            ),
        }
    }
    // the default buffer is about one period: any backlog fills it
    build_stream(
        device,
        config.sample_format(),
        base,
        &path,
        &clock,
        kind,
        5_000_000,
    )
}

/// One stream that downmixes every callback buffer to mono i16 and hands it
/// to the channel's ring. Gaps become silence on the channel's timeline:
/// - capture-timestamp jumps (Windows) are lost audio — except, on
///   loopback, when the callback itself waited as long as the jump: the
///   output was idle and there was nothing to lose;
/// - loopback is also kept on the recording clock while the render pipeline
///   is idle (WASAPI stops delivering packets when nothing is playing);
/// - the mic's first packet is aligned to the clock, like loopback's always
///   was, so the two channels start together.
///
/// A failed build drops its channel (the WAV is recreated on the retry).
fn build_stream(
    device: &cpal::Device,
    format: cpal::SampleFormat,
    stream_config: cpal::StreamConfig,
    path: &std::path::Path,
    clock: &Arc<Clock>,
    kind: ChannelKind,
    high_water_ns: i128,
) -> Result<(ChannelRecorder, ChannelSink)> {
    let channels = stream_config.channels.max(1) as usize;
    let rate: u32 = stream_config.sample_rate;
    let (producer, sink) = sink::channel(path.to_path_buf(), rate)?;
    let stats = sink.stats();
    let qpf = qpc_frequency();

    macro_rules! build_for {
        ($t:ty) => {{
            let mut producer: ChannelProducer = producer;
            let clock = clock.clone();
            let err_stats = stats.clone();
            let err_path = path.to_path_buf();
            let err_fn = move |e: cpal::Error| {
                if matches!(e.kind(), cpal::ErrorKind::Xrun) {
                    // WASAPI flags loopback state changes this way too; real
                    // losses are measured from the timestamps instead
                    err_stats.xruns.fetch_add(1, Ordering::Relaxed);
                } else {
                    tracing::error!(
                        file = %err_path.file_name().unwrap_or_default().to_string_lossy(),
                        "audio stream error: {e}"
                    )
                }
            };
            let mut boosted = false;
            let mut started = false;
            let mut gaps = GapTracker {
                epoch: clock.epoch(),
                next_capture_ns: None,
                last_callback_ns: 0,
                last_backlog_high_ns: None,
                prev_peak: 0,
            };
            device.build_input_stream(
                stream_config,
                move |data: &[$t], info: &cpal::InputCallbackInfo| {
                    if !boosted {
                        boosted = true;
                        if sink::boost_capture_thread() {
                            producer.stats().boosted.store(true, Ordering::Relaxed);
                        }
                    }
                    let frames = (data.len() / channels) as u64;
                    let now_ms = clock.elapsed_ms();
                    let ts = info.timestamp();
                    let cap = ts.capture.as_nanos() as i128;
                    let cb = ts.callback.as_nanos() as i128;
                    // backlog: how long this packet sat in the OS buffer
                    let backlog = if TIMESTAMP_GAPS {
                        qpc_now_ns(qpf) - cap
                    } else {
                        0
                    };
                    let backlog_ok = (0..BACKLOG_SANE_NS).contains(&backlog);
                    // Judge the packet at the time it was captured, not now:
                    // after a stall the buffer hands over audio up to a
                    // buffer's length old, which isn't behind the clock.
                    let at_ms = if backlog_ok {
                        now_ms.saturating_sub((backlog / 1_000_000) as u64)
                    } else {
                        now_ms
                    };
                    let expected = at_ms * rate as u64 / 1000;

                    if TIMESTAMP_GAPS {
                        let epoch = clock.epoch();
                        if epoch != gaps.epoch {
                            // resumed after a pause: the jump is the pause
                            gaps.epoch = epoch;
                            gaps.next_capture_ns = None;
                        }
                        if backlog_ok {
                            producer
                                .stats()
                                .max_backlog_us
                                .fetch_max((backlog / 1000) as u64, Ordering::Relaxed);
                            if backlog >= high_water_ns {
                                gaps.last_backlog_high_ns = Some(cb);
                            }
                        }
                        if let Some(next) = gaps.next_capture_ns {
                            let gap_ns = cap - next;
                            let waited_ns = cb - gaps.last_callback_ns;
                            // our buffer was nearly full just before: this
                            // app fell behind, whatever else is true
                            let ours = gaps
                                .last_backlog_high_ns
                                .is_some_and(|at| cb - at < BACKLOG_BLAME_NS);
                            // Loopback stops delivering when nothing plays,
                            // and the callback then waits as long as the gap.
                            // Only a gap after silence can be that: meeting
                            // apps render continuous silence during a call,
                            // so a gap right after audible output is lost
                            // audio (an engine stall looks idle by timing
                            // alone). A gap after silence lost nothing
                            // audible either way.
                            let idle = kind == ChannelKind::Loopback
                                && gap_ns > GAP_MIN_NS
                                && waited_ns * 2 >= gap_ns
                                && gaps.prev_peak <= SILENT_PEAK
                                && !ours;
                            if gap_ns > GAP_MIN_NS {
                                // Every gap becomes the same length of silence,
                                // so the channel follows the device's timeline
                                // exactly; it counts as a loss unless idle.
                                let lost = !idle;
                                if lost && !ours {
                                    producer
                                        .stats()
                                        .upstream_lost_events
                                        .fetch_add(1, Ordering::Relaxed);
                                }
                                let gap = (gap_ns * rate as i128 / 1_000_000_000) as u64;
                                // never run more than half a second ahead of
                                // the clock, whatever a timestamp claims
                                let room = (expected + rate as u64 / 2)
                                    .saturating_sub(producer.timeline() + frames);
                                let fill = gap.min(room);
                                producer.silence(fill, lost, now_ms);
                                if lost && gap > fill {
                                    producer
                                        .stats()
                                        .lost
                                        .fetch_add(gap - fill, Ordering::Relaxed);
                                }
                            }
                        }
                        gaps.next_capture_ns =
                            Some(cap + frames as i128 * 1_000_000_000 / rate as i128);
                        gaps.last_callback_ns = cb;
                    }

                    if kind == ChannelKind::Loopback || !started {
                        let pad = idle_pad_frames(expected, producer.timeline() + frames, rate);
                        producer.silence(pad, false, now_ms);
                    }
                    started = true;

                    let mut peak: i16 = 0;
                    for frame in data.chunks(channels) {
                        let sum: f32 = frame
                            .iter()
                            .map(|s| <f32 as cpal::FromSample<$t>>::from_sample_(*s))
                            .sum();
                        let mono = (sum / channels as f32).clamp(-1.0, 1.0);
                        let sample = (mono * i16::MAX as f32) as i16;
                        peak = peak.max(sample.saturating_abs());
                        producer.push(sample);
                    }
                    gaps.prev_peak = peak;
                    producer.end_packet();
                    producer.note_time(now_ms);
                },
                err_fn,
                None,
            )
        }};
    }

    let stream = match format {
        cpal::SampleFormat::F32 => build_for!(f32),
        cpal::SampleFormat::I16 => build_for!(i16),
        cpal::SampleFormat::U16 => build_for!(u16),
        cpal::SampleFormat::I32 => build_for!(i32),
        other => {
            return Err(AudioError::Backend(format!(
                "unsupported sample format {other:?}"
            )))
        }
    }
    .map_err(|e| AudioError::Backend(e.to_string()))?;

    Ok((
        ChannelRecorder {
            _stream: stream,
            path: path.to_path_buf(),
        },
        sink,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_audio_warning_matches_the_recording_bar_classifier() {
        // recordingBar.ts: "was lost" → "Audio lost"; "microphone" → You side
        let mic = lost_audio_warning(true, 1_250, 754_000);
        assert!(
            mic.contains("was lost") && mic.contains("microphone"),
            "{mic}"
        );
        assert!(mic.contains("1.2 s") || mic.contains("1.3 s"), "{mic}");
        assert!(mic.contains("12:34"), "{mic}");
        let sys = lost_audio_warning(false, 40, 3_725_000);
        assert!(
            sys.contains("was lost") && !sys.contains("microphone"),
            "{sys}"
        );
        assert!(sys.contains("0.1 s"), "{sys}");
        assert!(sys.contains("1:02:05"), "{sys}");
    }

    #[test]
    fn idle_pad_keeps_the_existing_slack() {
        assert_eq!(idle_pad_frames(48_000, 48_000, 48_000), 0);
        // under 200 ms behind: left alone
        assert_eq!(idle_pad_frames(48_000, 48_000 - 9_000, 48_000), 0);
        // more: brought exactly back onto the clock
        assert_eq!(idle_pad_frames(48_000, 30_000, 48_000), 18_000);
    }

    #[test]
    fn health_warns_only_past_the_threshold() {
        let stats = Arc::new(ChannelStats::default());
        let h = CaptureHealth {
            mic: Some((stats.clone(), 48_000)),
            ..Default::default()
        };
        stats.lost.store(48 * 40, Ordering::Relaxed); // 40 ms
        assert!(h.warnings().is_empty());
        stats.overflow.store(48 * 20, Ordering::Relaxed); // +20 ms
        stats.last_loss_at_ms.store(65_001, Ordering::Relaxed);
        let w = h.warnings();
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("microphone") && w[0].contains("1:05"),
            "{}",
            w[0]
        );
        let r = h.report();
        assert_eq!(r.mic.unwrap().audio_lost_ms(), 60);
        assert!(r.system.is_none());
    }
}
