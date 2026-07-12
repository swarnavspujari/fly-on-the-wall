//! Hardware smoke test: records ~4s from the default mic + system loopback,
//! then prints per-channel RMS so a human (or CI-less agent) can verify
//! signal actually landed. Run manually: cargo run -p fly-audio --example capture_smoke -- <out_dir>

use fly_audio::cpal_backend::CpalAudioCapture;
use fly_audio::mix::read_wav_mono;
use fly_audio::{AudioCapture, CaptureConfig};

fn rms(path: &std::path::Path) -> f32 {
    match read_wav_mono(path) {
        Ok((samples, _)) if !samples.is_empty() => {
            (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
        }
        _ => -1.0,
    }
}

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "capture-smoke-out".to_string());
    let capture = CpalAudioCapture::new();

    println!("mic devices:");
    for d in capture.list_mic_devices().expect("list devices") {
        println!(
            "  - {}{}",
            d.name,
            if d.is_default { " (default)" } else { "" }
        );
    }

    let mut session = capture
        .start(CaptureConfig {
            mic_device_id: None,
            capture_system: true,
            out_dir: out_dir.clone().into(),
            base_name: "smoke".into(),
        })
        .expect("start capture");

    println!("recording 4s…");
    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("elapsed at 2s mark: {}ms", session.elapsed_ms());
    session.pause().expect("pause");
    std::thread::sleep(std::time::Duration::from_millis(600));
    session.resume().expect("resume");
    std::thread::sleep(std::time::Duration::from_secs(2));

    let out = Box::new(session).stop().expect("stop capture");
    println!("duration_ms: {}", out.duration_ms);
    for (label, path) in [
        ("mic", out.mic_path.as_ref()),
        ("system", out.system_path.as_ref()),
        ("mixed", out.mixed_path.as_ref()),
    ] {
        match path {
            Some(p) => println!(
                "{label}: {} ({} bytes, rms {:.5})",
                p.display(),
                std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
                rms(p)
            ),
            None => println!("{label}: <none>"),
        }
    }
}
