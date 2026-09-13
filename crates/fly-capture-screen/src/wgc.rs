//! Windows Graphics Capture `ScreenRecorder`.
//!
//! Why this exists: the ffmpeg path records a window with `gdigrab`, which
//! copies pixels through GDI. GPU-composited windows (Zoom, Teams, Chrome,
//! every Electron app) draw nothing GDI can see, so a whole meeting comes
//! out black. Windows Graphics Capture reads the composited surface the
//! same way the OS's own screen-share does, so those windows record
//! correctly. Frames are encoded in-process (H.264 in MP4 through Media
//! Foundation, hardware-accelerated when the GPU has an encoder) at 30 fps
//! with a keyframe cadence the player can scrub, so no ffmpeg is needed
//! while recording.
//!
//! Lifecycle: `start` spawns the capture thread (`start_free_threaded`) and
//! keeps the handler's `Arc` so `stop` can, after the thread is joined, take
//! the encoder out of the handler and `finish()` it — that is what writes
//! the MP4 index. A window closed mid-meeting fires `on_closed`, which
//! finishes the encoder early; `stop` then finds nothing left to do and
//! still returns the file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use std::time::{Duration, Instant};

use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::encoder::{
    AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
    VideoSettingsSubType,
};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl};
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    GraphicsCaptureItemType, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

use crate::window_list::find_window_hwnd;
use crate::{CaptureTarget, Result, ScreenError, ScreenRecorder, ScreenSession};

/// Encoder frame rate. WGC only delivers frames when something changed, so
/// this is a ceiling, not a cost — a static slide deck produces few frames.
pub const FRAME_RATE: u32 = 30;

/// How long `start` waits for proof of life (a frame, or the thread dying
/// with an error) before assuming the capture is running.
const START_PROBE: Duration = Duration::from_millis(1500);

/// Bitrate for a capture of `width`×`height` at [`FRAME_RATE`]: about 0.1
/// bit per pixel per frame, clamped to 2–8 Mbps. 1080p lands at ~6 Mbps,
/// which keeps a 2 h meeting around 5 GB and looks clean for slides + video
/// tiles; tiny windows stay above the floor where text gets smeary.
pub fn bitrate_for(width: u32, height: u32) -> u32 {
    let raw = (width as u64) * (height as u64) * (FRAME_RATE as u64) / 10;
    raw.clamp(2_000_000, 8_000_000) as u32
}

/// H.264 (4:2:0) needs even dimensions.
pub fn even(x: u32) -> u32 {
    (x & !1).max(2)
}

pub struct WgcScreenRecorder;

impl ScreenRecorder for WgcScreenRecorder {
    fn is_available(&self) -> bool {
        GraphicsCaptureApi::is_supported().unwrap_or(false)
    }

    fn start(&self, target: CaptureTarget, out_path: &Path) -> Result<Box<dyn ScreenSession>> {
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let progress = Progress::default();
        let flags = |crop| WgcFlags {
            out_path: out_path.to_path_buf(),
            crop,
            progress: progress.clone(),
        };
        let control = match target {
            CaptureTarget::FullScreen => start_item(primary_monitor()?, flags(None))?,
            CaptureTarget::Region {
                x,
                y,
                width,
                height,
            } => {
                let crop = Crop {
                    x: x.max(0) as u32,
                    y: y.max(0) as u32,
                    width: even(width),
                    height: even(height),
                };
                start_item(primary_monitor()?, flags(Some(crop)))?
            }
            CaptureTarget::Window { title } => {
                let hwnd = find_window_hwnd(&title).ok_or_else(|| {
                    ScreenError::Capture(format!(
                        "window \"{title}\" wasn't found — it may have been closed or renamed"
                    ))
                })?;
                let window = Window::from_raw_hwnd(hwnd as *mut std::ffi::c_void);
                if !window.is_valid() {
                    return Err(ScreenError::Capture(format!(
                        "window \"{title}\" is no longer valid"
                    )));
                }
                start_item(window, flags(None))?
            }
        };
        let callback = control.callback();
        let started = Instant::now();

        // Proof of life: WGC hands over the current contents right after
        // StartCapture, so a healthy session has a frame within a few
        // hundred ms. A capture that cannot work (protected content, item
        // gone) ends its thread with an error instead; surface that here so
        // the app never claims "recording" over a dead session.
        let deadline = started + START_PROBE;
        loop {
            if progress.frames.load(Ordering::Relaxed) > 0 {
                break;
            }
            if control.is_finished() {
                let outcome = control
                    .into_thread_handle()
                    .join()
                    .map_err(|_| ScreenError::Capture("capture thread panicked".into()))?;
                let msg = match outcome {
                    Ok(()) => "capture ended before delivering a frame".to_string(),
                    Err(e) => format!("capture failed to start: {e}"),
                };
                let _ = std::fs::remove_file(out_path);
                return Err(ScreenError::Capture(msg));
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    "screen capture delivered no frame within {:?}; assuming a static target",
                    START_PROBE
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }

        Ok(Box::new(WgcSession {
            control: Some(control),
            callback,
            out_path: out_path.to_path_buf(),
            started,
            progress,
        }))
    }
}

fn primary_monitor() -> Result<Monitor> {
    Monitor::primary().map_err(|e| ScreenError::Capture(format!("no primary monitor: {e}")))
}

fn start_item<T>(item: T, flags: WgcFlags) -> Result<CaptureControl<WgcHandler, HandlerError>>
where
    T: TryInto<GraphicsCaptureItemType> + Send + 'static,
{
    let settings = Settings::new(
        item,
        CursorCaptureSettings::WithCursor,
        // Default = whatever the OS does for this item (a thin border on
        // builds that draw one). Forcing it off needs a newer API that
        // fails on older Windows 10, and the border is on-screen only —
        // it never appears in the recording.
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );
    WgcHandler::start_free_threaded(settings)
        .map_err(|e| ScreenError::Capture(format!("screen capture could not start: {e}")))
}

/// Region of the source frame to keep (source pixels).
#[derive(Debug, Clone, Copy)]
struct Crop {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Counters the session reads while the capture thread runs.
#[derive(Default, Clone)]
struct Progress {
    frames: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct WgcFlags {
    out_path: PathBuf,
    crop: Option<Crop>,
    progress: Progress,
}

type HandlerError = Box<dyn std::error::Error + Send + Sync>;

pub struct WgcHandler {
    flags: WgcFlags,
    /// Created on the first frame so the encoder matches the real item
    /// size (a window's WGC size differs from its screen rect).
    encoder: Option<VideoEncoder>,
    scratch: Vec<u8>,
}

impl WgcHandler {
    fn open_encoder(
        &self,
        width: u32,
        height: u32,
    ) -> std::result::Result<VideoEncoder, HandlerError> {
        let (w, h) = (even(width), even(height));
        tracing::info!(
            width = w,
            height = h,
            bitrate = bitrate_for(w, h),
            "screen capture encoder opened"
        );
        Ok(VideoEncoder::new(
            VideoSettingsBuilder::new(w, h)
                .sub_type(VideoSettingsSubType::H264)
                .frame_rate(FRAME_RATE)
                .bitrate(bitrate_for(w, h)),
            AudioSettingsBuilder::default().disabled(true),
            ContainerSettingsBuilder::default(),
            &self.flags.out_path,
        )?)
    }
}

impl GraphicsCaptureApiHandler for WgcHandler {
    type Flags = WgcFlags;
    type Error = HandlerError;

    fn new(ctx: Context<Self::Flags>) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            flags: ctx.flags,
            encoder: None,
            scratch: Vec::new(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _control: InternalCaptureControl,
    ) -> std::result::Result<(), Self::Error> {
        if self.encoder.is_none() {
            let (w, h) = match self.flags.crop {
                Some(c) => (c.width, c.height),
                None => (frame.width(), frame.height()),
            };
            self.encoder = Some(self.open_encoder(w, h)?);
        }
        let encoder = self.encoder.as_mut().expect("encoder opened above");
        match self.flags.crop {
            None => encoder.send_frame(frame)?,
            Some(c) => {
                // Clamp to the frame so a region hanging off the edge still
                // records what is there instead of erroring every frame.
                let x1 = (c.x + c.width).min(frame.width());
                let y1 = (c.y + c.height).min(frame.height());
                if c.x >= x1 || c.y >= y1 {
                    return Ok(());
                }
                let timestamp = frame.timestamp()?.Duration;
                let buffer = frame.buffer_crop(c.x, c.y, x1, y1)?;
                let bytes = buffer.as_nopadding_buffer(&mut self.scratch);
                encoder.send_frame_buffer(bytes, timestamp)?;
            }
        }
        self.flags.progress.frames.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn on_closed(&mut self) -> std::result::Result<(), Self::Error> {
        self.flags.progress.closed.store(true, Ordering::Relaxed);
        if let Some(encoder) = self.encoder.take() {
            tracing::warn!("captured window closed mid-recording; finalizing the file");
            encoder.finish()?;
        }
        Ok(())
    }
}

struct WgcSession {
    control: Option<CaptureControl<WgcHandler, HandlerError>>,
    callback: Arc<Mutex<WgcHandler>>,
    out_path: PathBuf,
    started: Instant,
    progress: Progress,
}

impl ScreenSession for WgcSession {
    fn stop(mut self: Box<Self>) -> Result<PathBuf> {
        if let Some(control) = self.control.take() {
            control
                .stop()
                .map_err(|e| ScreenError::Capture(format!("stopping screen capture: {e}")))?;
        }
        let encoder = self.callback.lock().encoder.take();
        let frames = self.progress.frames.load(Ordering::Relaxed);
        match encoder {
            Some(encoder) => encoder
                .finish()
                .map_err(|e| ScreenError::Capture(format!("finalizing recording: {e}")))?,
            // Finished already by on_closed (window went away).
            None if self.progress.closed.load(Ordering::Relaxed) => {}
            None if frames == 0 => {
                let _ = std::fs::remove_file(&self.out_path);
                return Err(ScreenError::Capture(
                    "no frames were captured — the window never repainted".into(),
                ));
            }
            None => {}
        }
        tracing::info!(frames, path = %self.out_path.display(), "screen capture finalized");
        Ok(self.out_path.clone())
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_scales_with_area_and_clamps() {
        assert_eq!(bitrate_for(320, 200), 2_000_000); // floor
        assert_eq!(bitrate_for(1920, 1080), 6_220_800);
        assert_eq!(bitrate_for(3840, 2160), 8_000_000); // ceiling
    }

    #[test]
    fn even_rounds_down_and_never_hits_zero() {
        assert_eq!(even(1921), 1920);
        assert_eq!(even(1080), 1080);
        assert_eq!(even(1), 2);
    }

    /// Locate the ffmpeg the app manages, for decoding proof frames.
    fn local_ffmpeg() -> Option<PathBuf> {
        let root = PathBuf::from(std::env::var("APPDATA").ok()?)
            .join("FlyOnTheWall")
            .join("bin")
            .join("ffmpeg");
        fn walk(dir: &Path) -> Option<PathBuf> {
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    if let Some(hit) = walk(&p) {
                        return Some(hit);
                    }
                } else if p.file_name().is_some_and(|n| n == "ffmpeg.exe") {
                    return Some(p);
                }
            }
            None
        }
        walk(&root)
    }

    /// Mean luma of the frame `at_secs` into the video (0 = black).
    fn mean_luma(ffmpeg: &Path, video: &Path, at_secs: f32) -> f64 {
        let out = std::process::Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-ss"])
            .arg(at_secs.to_string())
            .arg("-i")
            .arg(video)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=160:-2",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
                "-",
            ])
            .output()
            .expect("run ffmpeg");
        assert!(
            out.status.success(),
            "ffmpeg decode failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.stdout.is_empty(), "ffmpeg decoded no pixels");
        out.stdout.iter().map(|&b| b as f64).sum::<f64>() / out.stdout.len() as f64
    }

    /// The bug this backend fixes: a GPU-composited window (the Claude
    /// desktop app is Electron) records as black through gdigrab. Record it
    /// for 3 s with WGC and prove the frames have content. #[ignore]: needs
    /// an interactive desktop with that window open plus the managed ffmpeg.
    #[test]
    #[ignore]
    fn wgc_records_gpu_window_e2e() {
        let ffmpeg = local_ffmpeg().expect("managed ffmpeg present");
        let title = crate::window_list::list_windows()
            .into_iter()
            .find(|t| t.contains("Claude"))
            .expect("a window titled *Claude* is open");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("wgc.mp4");
        let session = WgcScreenRecorder
            .start(CaptureTarget::Window { title }, &out)
            .expect("wgc start");
        std::thread::sleep(Duration::from_millis(3000));
        let path = session.stop().expect("wgc stop");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 10_000, "tiny file: {} bytes", bytes.len());
        assert!(
            bytes.windows(4).any(|w| w == b"moov"),
            "MP4 has no moov box"
        );
        let luma = mean_luma(&ffmpeg, &path, 1.0);
        eprintln!("mean luma of the 1 s frame: {luma:.1}");
        assert!(luma > 10.0, "frame is black (mean luma {luma:.1})");
        // FOTW_KEEP_CAPTURE=<file> copies the recording out for a human look.
        if let Ok(keep) = std::env::var("FOTW_KEEP_CAPTURE") {
            std::fs::copy(&path, &keep).unwrap();
        }
    }

    /// Full screen + region through the same backend, 2 s each.
    #[test]
    #[ignore]
    fn wgc_records_screen_and_region_e2e() {
        let dir = tempfile::tempdir().unwrap();
        for (name, target) in [
            ("full.mp4", CaptureTarget::FullScreen),
            (
                "region.mp4",
                CaptureTarget::Region {
                    x: 100,
                    y: 100,
                    width: 801,
                    height: 601,
                },
            ),
        ] {
            let out = dir.path().join(name);
            let session = WgcScreenRecorder.start(target, &out).expect("start");
            std::thread::sleep(Duration::from_millis(2000));
            let path = session.stop().expect("stop");
            let bytes = std::fs::read(&path).unwrap();
            assert!(bytes.windows(4).any(|w| w == b"moov"), "{name}: no moov");
            eprintln!("{name}: {} bytes", bytes.len());
        }
    }
}
