//! fly-capture-screen: the `ScreenRecorder` trait.
//!
//! Windows records through Windows Graphics Capture (`wgc`, in-process
//! H.264) with the ffmpeg sidecar (`ffmpeg`, gdigrab) as the fallback;
//! Linux uses ffmpeg x11grab. macOS (ScreenCaptureKit) is future work —
//! see docs/PORTING.md.

pub mod ffmpeg;
#[cfg(windows)]
pub mod wgc;
pub mod window_list;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ScreenError {
    #[error("screen recorder backend unavailable: {0}")]
    Unavailable(String),
    #[error("capture failed: {0}")]
    Capture(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ScreenError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureTarget {
    FullScreen,
    Window {
        title: String,
    },
    Region {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
}

/// A live screen recording. Obtained from [`ScreenRecorder::start`].
pub trait ScreenSession: Send {
    /// Stop and finalize; returns the video file path.
    fn stop(self: Box<Self>) -> Result<PathBuf>;
    fn elapsed_ms(&self) -> u64;
}

pub trait ScreenRecorder: Send + Sync {
    fn is_available(&self) -> bool;
    fn start(&self, target: CaptureTarget, out_path: &Path) -> Result<Box<dyn ScreenSession>>;
}

/// Try `primary`, fall back to `secondary` when it is unavailable or fails
/// to start. The fallback is logged so a black recording from the
/// secondary path can be traced to why the primary one was skipped.
pub struct FallbackScreenRecorder {
    pub primary: Box<dyn ScreenRecorder>,
    pub secondary: Box<dyn ScreenRecorder>,
}

impl ScreenRecorder for FallbackScreenRecorder {
    fn is_available(&self) -> bool {
        self.primary.is_available() || self.secondary.is_available()
    }

    fn start(&self, target: CaptureTarget, out_path: &Path) -> Result<Box<dyn ScreenSession>> {
        if self.primary.is_available() {
            match self.primary.start(target.clone(), out_path) {
                Ok(session) => return Ok(session),
                Err(e) => tracing::warn!("primary screen recorder failed, falling back: {e}"),
            }
        } else {
            tracing::warn!("primary screen recorder unavailable on this system, falling back");
        }
        self.secondary.start(target, out_path)
    }
}

/// No-op recorder for platforms without an impl yet.
pub struct NullScreenRecorder;

impl ScreenRecorder for NullScreenRecorder {
    fn is_available(&self) -> bool {
        false
    }

    fn start(&self, _target: CaptureTarget, _out_path: &Path) -> Result<Box<dyn ScreenSession>> {
        Err(ScreenError::Unavailable(
            "no screen recorder on this platform".into(),
        ))
    }
}
