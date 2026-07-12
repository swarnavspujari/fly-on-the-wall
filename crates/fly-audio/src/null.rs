//! A no-op `AudioCapture` for platforms without a real backend yet and for
//! tests that need the trait but no hardware.

use crate::{AudioCapture, AudioDevice, AudioError, CaptureConfig, CaptureSession, Result};

pub struct NullAudioCapture;

impl AudioCapture for NullAudioCapture {
    fn list_mic_devices(&self) -> Result<Vec<AudioDevice>> {
        Ok(vec![])
    }

    fn supports_system_loopback(&self) -> bool {
        false
    }

    fn start(&self, _cfg: CaptureConfig) -> Result<Box<dyn CaptureSession>> {
        Err(AudioError::Backend(
            "no audio backend is available on this platform".into(),
        ))
    }
}
