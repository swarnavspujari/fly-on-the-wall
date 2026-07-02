//! looma-audio: the `AudioCapture` trait and platform backends.
//!
//! Windows backend (cpal/WASAPI, incl. system loopback) lands in M2.
//! macOS (Core Audio process taps) and mobile impls are future work — see
//! docs/PORTING.md. UI and domain code must only ever see the trait.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod cpal_backend;
pub mod mix;
pub mod null;
#[cfg(target_os = "linux")]
mod pulse_loopback;
#[cfg(target_os = "windows")]
mod win_volume;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("audio device not found: {0}")]
    DeviceNotFound(String),
    #[error("system loopback capture is not supported on this platform/backend")]
    LoopbackUnsupported,
    #[error("capture is not in a state that allows this operation: {0}")]
    InvalidState(String),
    #[error("audio backend error: {0}")]
    Backend(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, AudioError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureConfig {
    /// `None` = system default microphone.
    pub mic_device_id: Option<String>,
    /// Capture system output (the other meeting participants) as its own channel.
    pub capture_system: bool,
    /// Directory the WAV files are written into.
    pub out_dir: PathBuf,
    /// File stem; the backend appends `.mic.wav`, `.system.wav`, `.mixed.wav`.
    pub base_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Recording,
    Paused,
    Stopped,
}

/// What a finished capture produced. Paths are absolute; `mixed_path` is the
/// mono mixdown used for playback and single-track pipelines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureOutput {
    pub mic_path: Option<PathBuf>,
    pub system_path: Option<PathBuf>,
    pub mixed_path: Option<PathBuf>,
    pub duration_ms: u64,
}

/// A live recording. Obtained from [`AudioCapture::start`].
pub trait CaptureSession: Send {
    fn pause(&mut self) -> Result<()>;
    fn resume(&mut self) -> Result<()>;
    fn stop(self: Box<Self>) -> Result<CaptureOutput>;
    fn state(&self) -> CaptureState;
    /// Recorded time, excluding paused stretches.
    fn elapsed_ms(&self) -> u64;
}

/// Platform audio capture. One impl per OS; selected in src-tauri at
/// composition time.
pub trait AudioCapture: Send + Sync {
    fn list_mic_devices(&self) -> Result<Vec<AudioDevice>>;
    /// Whether this backend can capture system output audio at all.
    fn supports_system_loopback(&self) -> bool;
    fn start(&self, cfg: CaptureConfig) -> Result<Box<dyn CaptureSession>>;
    /// Human-readable conditions that will silently degrade a capture (e.g.
    /// the system output is muted, so loopback records silence). Cheap —
    /// polled while recording so a mid-meeting mute surfaces immediately.
    fn capture_warnings(&self) -> Vec<String> {
        Vec::new()
    }
}
