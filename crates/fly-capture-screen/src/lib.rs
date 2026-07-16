//! fly-capture-screen: the `ScreenRecorder` trait.
//!
//! Windows impl (ffmpeg sidecar, gdigrab/ddagrab) lands in M7. macOS
//! (ScreenCaptureKit) is future work — see docs/PORTING.md.

pub mod ffmpeg;
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
