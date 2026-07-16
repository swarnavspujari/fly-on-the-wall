//! ffmpeg-sidecar `ScreenRecorder` (Windows gdigrab). Full screen, a single
//! window by title, or a fixed region. Stopped gracefully by sending `q` on
//! stdin so ffmpeg finalizes the MP4 moov atom.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use crate::{CaptureTarget, Result, ScreenError, ScreenRecorder, ScreenSession};

pub struct FfmpegScreenRecorder {
    pub exe: PathBuf,
    pub framerate: u32,
}

impl FfmpegScreenRecorder {
    pub fn new(exe: PathBuf) -> Self {
        Self { exe, framerate: 10 }
    }
}

/// Which ffmpeg capture input this OS uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabBackend {
    /// Windows.
    Gdigrab,
    /// Linux/X11 (Wayland sessions need a portal-based recorder — x11grab
    /// will fail there with an ffmpeg error the UI surfaces).
    X11grab,
    /// macOS.
    AvFoundation,
}

pub fn host_backend() -> GrabBackend {
    if cfg!(target_os = "windows") {
        GrabBackend::Gdigrab
    } else if cfg!(target_os = "macos") {
        GrabBackend::AvFoundation
    } else {
        GrabBackend::X11grab
    }
}

/// Build the capture-input arguments for a target on a backend. Pure and
/// OS-independent so every mapping is unit-tested on every platform.
pub fn grab_args(
    backend: GrabBackend,
    target: &CaptureTarget,
    framerate: u32,
    display: &str,
) -> Result<Vec<String>> {
    let mut args: Vec<String> = vec![
        "-f".into(),
        match backend {
            GrabBackend::Gdigrab => "gdigrab".into(),
            GrabBackend::X11grab => "x11grab".into(),
            GrabBackend::AvFoundation => "avfoundation".into(),
        },
        "-framerate".into(),
        framerate.to_string(),
    ];
    match (backend, target) {
        (GrabBackend::Gdigrab, CaptureTarget::FullScreen) => {
            args.extend(["-i".into(), "desktop".into()]);
        }
        (GrabBackend::Gdigrab, CaptureTarget::Window { title }) => {
            args.extend(["-i".into(), format!("title={title}")]);
        }
        (
            GrabBackend::Gdigrab,
            CaptureTarget::Region {
                x,
                y,
                width,
                height,
            },
        ) => {
            // gdigrab requires even dimensions for yuv420p; round down
            let w = width & !1;
            let h = height & !1;
            args.extend([
                "-offset_x".into(),
                x.to_string(),
                "-offset_y".into(),
                y.to_string(),
                "-video_size".into(),
                format!("{w}x{h}"),
                "-i".into(),
                "desktop".into(),
            ]);
        }
        (GrabBackend::X11grab, CaptureTarget::FullScreen) => {
            args.extend(["-i".into(), display.to_string()]);
        }
        (
            GrabBackend::X11grab,
            CaptureTarget::Region {
                x,
                y,
                width,
                height,
            },
        ) => {
            let w = width & !1;
            let h = height & !1;
            args.extend([
                "-video_size".into(),
                format!("{w}x{h}"),
                "-i".into(),
                format!("{display}+{x},{y}"),
            ]);
        }
        (GrabBackend::X11grab, CaptureTarget::Window { .. }) => {
            return Err(ScreenError::Unavailable(
                "window capture is not supported on Linux yet — record the full screen or a region"
                    .into(),
            ));
        }
        (GrabBackend::AvFoundation, CaptureTarget::FullScreen) => {
            // avfoundation matches devices by name; ":none" = no audio.
            args.extend([
                "-capture_cursor".into(),
                "1".into(),
                "-i".into(),
                "Capture screen 0:none".into(),
            ]);
        }
        (GrabBackend::AvFoundation, _) => {
            return Err(ScreenError::Unavailable(
                "only full-screen capture is supported on macOS yet".into(),
            ));
        }
    }
    Ok(args)
}

impl ScreenRecorder for FfmpegScreenRecorder {
    fn is_available(&self) -> bool {
        self.exe.exists()
    }

    fn start(&self, target: CaptureTarget, out_path: &Path) -> Result<Box<dyn ScreenSession>> {
        if !self.exe.exists() {
            return Err(ScreenError::Unavailable(format!(
                "ffmpeg not found at {}",
                self.exe.display()
            )));
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into());
        let mut cmd = Command::new(&self.exe);
        // stderr is piped but nobody drains it while recording — ffmpeg's
        // periodic stats lines would eventually fill the OS pipe buffer and
        // block encoding mid-meeting. Errors still come through (and are all
        // brief_stderr needs); the banner noise disappears as a bonus.
        cmd.args(["-hide_banner", "-nostats", "-loglevel", "error"]);
        cmd.args(grab_args(
            host_backend(),
            &target,
            self.framerate,
            &display,
        )?)
        .args([
            "-c:v",
            "libx264",
            // ultrafast + a 1080p cap keep encoding realtime even for
            // high-DPI screens on laptop CPUs; x264 falling behind would
            // silently compress the recording's timeline.
            "-preset",
            "ultrafast",
            "-crf",
            "28",
            "-pix_fmt",
            "yuv420p",
            // cap width at 1920 and force even dimensions for yuv420p
            "-vf",
            "scale='trunc(min(1920,iw)/2)*2':-2",
            "-y",
        ])
        .arg(out_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| ScreenError::Capture(format!("failed to launch ffmpeg: {e}")))?;

        // A doomed capture (window vanished, bad input) makes ffmpeg exit
        // within a few hundred ms — but the old code only noticed at STOP
        // time, so the app claimed "recording" while nothing was captured
        // and the user's meeting was silently lost. Give the process a short
        // beat and fail the START with a readable error instead.
        let deadline = Instant::now() + std::time::Duration::from_millis(600);
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(status)) if !status.success() => {
                    let mut stderr = String::new();
                    if let Some(mut e) = child.stderr.take() {
                        use std::io::Read;
                        let _ = e.read_to_string(&mut stderr);
                    }
                    tracing::warn!(%status, stderr, "ffmpeg died at capture start");
                    return Err(ScreenError::Capture(format!(
                        "{} (ffmpeg {status})",
                        brief_stderr(&stderr)
                    )));
                }
                Ok(Some(_)) => break, // exited cleanly?! stop() will sort it out
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                Err(_) => break,
            }
        }

        Ok(Box::new(FfmpegSession {
            child,
            out_path: out_path.to_path_buf(),
            started: Instant::now(),
        }))
    }
}

/// The human end of an ffmpeg stderr dump: the version/config banner is
/// noise, the real error is in the last lines. Special-case gdigrab's
/// window-not-found, the one users actually hit.
fn brief_stderr(stderr: &str) -> String {
    if stderr.contains("Can't find window") {
        return "the selected window wasn't found — it may have been closed or renamed. \
                Pick the window again."
            .into();
    }
    let mut tail: Vec<&str> = stderr
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(3)
        .collect();
    tail.reverse();
    if tail.is_empty() {
        // No claim about WHEN it died — stop() uses this too, and a capture
        // can fail hours in (disk full at finalize) with drained stderr.
        "ffmpeg exited without diagnostics".into()
    } else {
        tail.join(" · ")
    }
}

struct FfmpegSession {
    child: Child,
    out_path: PathBuf,
    started: Instant,
}

impl ScreenSession for FfmpegSession {
    fn stop(mut self: Box<Self>) -> Result<PathBuf> {
        // graceful: 'q' lets ffmpeg finalize the container
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = stdin.write_all(b"q");
            let _ = stdin.flush();
        }
        drop(self.child.stdin.take());

        // give it a few seconds, then force-kill as a last resort
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() && !self.out_path.exists() {
                        let mut stderr = String::new();
                        if let Some(mut e) = self.child.stderr.take() {
                            use std::io::Read;
                            let _ = e.read_to_string(&mut stderr);
                        }
                        tracing::warn!(%status, stderr, "screen capture ffmpeg failed");
                        return Err(ScreenError::Capture(format!(
                            "{} (ffmpeg {status})",
                            brief_stderr(&stderr)
                        )));
                    }
                    break;
                }
                Ok(None) if Instant::now() > deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                Err(e) => return Err(ScreenError::Capture(e.to_string())),
            }
        }

        if !self.out_path.exists() {
            return Err(ScreenError::Capture(
                "ffmpeg produced no output file".into(),
            ));
        }
        Ok(self.out_path)
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact stderr from the 2026-07-16 field failure must brief to the
    /// actionable window message, not a 400-char banner dump.
    #[test]
    fn brief_stderr_names_the_missing_window_case() {
        let field = "ffmpeg version n8.1.2 Copyright (c)\n  libavdevice 62. 3.102 / 62. 3.102\n\
                     [in#0 @ 00000222dd7de940] Can't find window 'IDB VDI IT', aborting.\n\
                     [in#0 @ 00000222dd7dc7c0] Error opening input: I/O error\n\
                     Error opening input files: I/O error\n";
        let msg = brief_stderr(field);
        assert!(msg.contains("wasn't found"), "{msg}");
        assert!(!msg.contains("libavdevice"), "banner must not leak: {msg}");
    }

    #[test]
    fn brief_stderr_keeps_the_last_real_lines_otherwise() {
        let msg = brief_stderr("ffmpeg version n8.1.2\n\nUnknown encoder 'libx26'\n");
        assert!(msg.contains("Unknown encoder"), "{msg}");
        // No when-claim: stop() uses this too, hours into a recording.
        assert_eq!(brief_stderr(""), "ffmpeg exited without diagnostics");
    }

    #[test]
    fn gdigrab_args_for_each_target() {
        let full = grab_args(GrabBackend::Gdigrab, &CaptureTarget::FullScreen, 15, ":0").unwrap();
        assert!(full.windows(2).any(|w| w == ["-i", "desktop"]));

        let win = grab_args(
            GrabBackend::Gdigrab,
            &CaptureTarget::Window {
                title: "Budget – Zoom".into(),
            },
            15,
            ":0",
        )
        .unwrap();
        assert!(win.iter().any(|a| a == "title=Budget – Zoom"));

        let region = grab_args(
            GrabBackend::Gdigrab,
            &CaptureTarget::Region {
                x: 10,
                y: 20,
                width: 801, // odd → rounded down
                height: 600,
            },
            30,
            ":0",
        )
        .unwrap();
        assert!(region.windows(2).any(|w| w == ["-video_size", "800x600"]));
        assert!(region.windows(2).any(|w| w == ["-offset_x", "10"]));
        assert!(region.contains(&"30".to_string()));
    }

    #[test]
    fn x11grab_args_full_and_region() {
        let full = grab_args(GrabBackend::X11grab, &CaptureTarget::FullScreen, 10, ":1").unwrap();
        assert!(full.windows(2).any(|w| w == ["-f", "x11grab"]));
        assert!(full.windows(2).any(|w| w == ["-i", ":1"]));

        let region = grab_args(
            GrabBackend::X11grab,
            &CaptureTarget::Region {
                x: 100,
                y: 50,
                width: 1281,
                height: 720,
            },
            10,
            ":0",
        )
        .unwrap();
        assert!(region.windows(2).any(|w| w == ["-video_size", "1280x720"]));
        assert!(region.windows(2).any(|w| w == ["-i", ":0+100,50"]));

        let win = grab_args(
            GrabBackend::X11grab,
            &CaptureTarget::Window { title: "x".into() },
            10,
            ":0",
        );
        assert!(win.is_err());
    }

    #[test]
    fn avfoundation_args_full_screen_only() {
        let full = grab_args(
            GrabBackend::AvFoundation,
            &CaptureTarget::FullScreen,
            10,
            "",
        )
        .unwrap();
        assert!(full.windows(2).any(|w| w == ["-f", "avfoundation"]));
        assert!(full.iter().any(|a| a == "Capture screen 0:none"));

        let region = grab_args(
            GrabBackend::AvFoundation,
            &CaptureTarget::Region {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
            10,
            "",
        );
        assert!(region.is_err());
    }
}
