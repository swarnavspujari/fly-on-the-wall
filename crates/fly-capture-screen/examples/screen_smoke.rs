//! Hardware smoke test: record ~4s of the full screen with the ffmpeg
//! recorder. Run manually:
//!   cargo run -p fly-capture-screen --example screen_smoke -- <ffmpeg.exe> <out.mp4>

use fly_capture_screen::ffmpeg::FfmpegScreenRecorder;
use fly_capture_screen::{CaptureTarget, ScreenRecorder};

fn main() {
    let mut args = std::env::args().skip(1);
    let ffmpeg = args.next().expect("arg 1: path to ffmpeg.exe");
    let out = args.next().expect("arg 2: output mp4 path");

    let recorder = FfmpegScreenRecorder::new(ffmpeg.into());
    let session = recorder
        .start(CaptureTarget::FullScreen, std::path::Path::new(&out))
        .expect("start screen capture");
    println!("recording 4s of the full screen…");
    std::thread::sleep(std::time::Duration::from_secs(4));
    let path = session.stop().expect("stop capture");
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!("done: {} ({bytes} bytes)", path.display());
    assert!(bytes > 10_000, "output suspiciously small");
}
