//! The opt-in video jobs against a real ffmpeg on synthetic videos (made
//! here with ffmpeg's test sources). Needs ffmpeg + ffprobe: set
//! `FLYONTHEWALL_FFMPEG` to an ffmpeg binary, or have one on PATH;
//! otherwise each test passes with a SKIP notice.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use fly_app_lib::state::AppState;
use fly_app_lib::video_store::{
    audio_only_one, extract_audio, imported_videos, screen_recordings, shrink_screen, Tools,
    VideoError,
};
use fly_core::RecordingRef;

fn tools() -> Option<Tools> {
    let ffmpeg = std::env::var("FLYONTHEWALL_FFMPEG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("ffmpeg"));
    let ok = Command::new(&ffmpeg)
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("SKIP: no ffmpeg (set FLYONTHEWALL_FFMPEG)");
        return None;
    }
    let t = Tools::beside(ffmpeg);
    let probe_ok = Command::new(&t.ffprobe)
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !probe_ok {
        eprintln!("SKIP: no ffprobe next to ffmpeg");
        return None;
    }
    Some(t)
}

fn ff(t: &Tools, args: &[&str]) {
    let st = Command::new(&t.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error"])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

/// A meeting-style video: picture + speech-band tone, AAC audio.
fn make_video(t: &Tools, out: &Path, secs: u32, audio: Option<&str>, vcodec_args: &[&str]) {
    let d = secs.to_string();
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", "testsrc2=size=640x360:rate=10"];
    if audio.is_some() {
        args.extend(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"]);
    }
    args.extend(["-t", d.as_str()]);
    args.extend(vcodec_args);
    if let Some(a) = audio {
        args.extend(["-c:a", a]);
    }
    args.extend(["-y", out.to_str().unwrap()]);
    ff(t, &args);
}

fn go() -> impl Fn() -> bool {
    || true
}

fn hashes(t: &Tools, p: &Path) -> String {
    let out = Command::new(&t.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(p)
        .args(["-map", "0:a", "-c", "copy", "-f", "framemd5", "-"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let f: Vec<&str> = l.split(',').map(str::trim).collect();
            format!("{} {}", f[4], f[5])
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn audio_comes_out_of_a_video_packet_for_packet() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let v = dir.path().join("Board call.mp4");
    make_video(
        &t,
        &v,
        6,
        Some("aac"),
        &["-c:v", "libx264", "-preset", "ultrafast"],
    );
    let mtime = std::fs::metadata(&v).unwrap().modified().unwrap();
    let audio = extract_audio(&t, &v, &go()).unwrap();
    assert_eq!(audio.file_name().unwrap(), "Board call.m4a");
    assert!(
        v.exists(),
        "the video itself is only deleted by the job, after the commit"
    );
    assert_eq!(hashes(&t, &audio), hashes(&t, &v));
    assert!(!hashes(&t, &v).is_empty());
    assert_eq!(
        std::fs::metadata(&audio).unwrap().modified().unwrap(),
        mtime
    );
    // run again: the proven audio is reused, no second copy
    assert_eq!(extract_audio(&t, &v, &go()).unwrap(), audio);
    assert!(!dir.path().join("Board call (2).m4a").exists());

    // Opus in Matroska keeps Opus, in Ogg
    let mkv = dir.path().join("talk.mkv");
    make_video(
        &t,
        &mkv,
        4,
        Some("libopus"),
        &["-c:v", "libx264", "-preset", "ultrafast"],
    );
    let ogg = extract_audio(&t, &mkv, &go()).unwrap();
    assert_eq!(ogg.extension().unwrap(), "ogg");
    assert_eq!(hashes(&t, &ogg), hashes(&t, &mkv));

    // nothing to keep
    let silent = dir.path().join("silent.mp4");
    make_video(
        &t,
        &silent,
        3,
        None,
        &["-c:v", "libx264", "-preset", "ultrafast"],
    );
    assert!(matches!(
        extract_audio(&t, &silent, &go()),
        Err(VideoError::Skipped(_))
    ));

    // stopped mid-way: nothing left behind
    let stop = || false;
    let v2 = dir.path().join("other.mp4");
    make_video(
        &t,
        &v2,
        3,
        Some("aac"),
        &["-c:v", "libx264", "-preset", "ultrafast"],
    );
    assert_eq!(extract_audio(&t, &v2, &stop), Err(VideoError::Stopped));
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_screen_recording_shrinks_only_when_it_stays_visually_identical() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    // like a WGC capture: hardware-style constant bitrate, far more than
    // screen content needs
    let v = dir.path().join("screen-20260105-101500.mp4");
    make_video(
        &t,
        &v,
        8,
        Some("aac"),
        &[
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-b:v",
            "6M",
            "-minrate",
            "6M",
            "-maxrate",
            "6M",
            "-bufsize",
            "2M",
        ],
    );
    let before = std::fs::metadata(&v).unwrap().len();
    let audio_before = hashes(&t, &v);
    let mtime = std::fs::metadata(&v).unwrap().modified().unwrap();
    let saved = shrink_screen(&t, &v, 2, &go()).unwrap();
    let after = std::fs::metadata(&v).unwrap().len();
    assert_eq!(before - after, saved);
    assert!((after as f64) < before as f64 * 0.9, "{before} -> {after}");
    assert_eq!(hashes(&t, &v), audio_before, "audio packets untouched");
    assert_eq!(std::fs::metadata(&v).unwrap().modified().unwrap(), mtime);
    // never twice
    assert!(matches!(
        shrink_screen(&t, &v, 2, &go()),
        Err(VideoError::Skipped(_))
    ));

    // already small: left as it is
    let small = dir.path().join("screen-20260105-111500.mp4");
    make_video(
        &t,
        &small,
        4,
        Some("aac"),
        &["-c:v", "libx264", "-preset", "veryslow", "-crf", "45"],
    );
    let bytes = std::fs::read(&small).unwrap();
    assert!(matches!(
        shrink_screen(&t, &small, 2, &go()),
        Err(VideoError::Skipped(_))
    ));
    assert_eq!(std::fs::read(&small).unwrap(), bytes, "untouched");

    // stopped mid-way: the original is untouched and no temp remains
    let v3 = dir.path().join("screen-20260105-121500.mp4");
    make_video(
        &t,
        &v3,
        4,
        Some("aac"),
        &["-c:v", "libx264", "-preset", "ultrafast", "-b:v", "6M"],
    );
    let bytes = std::fs::read(&v3).unwrap();
    assert_eq!(
        shrink_screen(&t, &v3, 2, &|| false),
        Err(VideoError::Stopped)
    );
    assert_eq!(std::fs::read(&v3).unwrap(), bytes);
    assert!(!dir
        .path()
        .join(".screen-20260105-121500.shrinking.mp4")
        .exists());
}

#[test]
fn audio_only_keeps_the_audio_and_deletes_only_the_app_copy() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::init_with(
        dir.path().to_path_buf(),
        Arc::new(fly_secrets::MemorySecretStore::default()),
    )
    .unwrap();
    let (note_id, meeting_id, video) = {
        let s = state.storage.lock().unwrap();
        let note = s.create_note("Quarterly review", None).unwrap();
        let m = s.create_meeting("Quarterly review", &note.id, &[]).unwrap();
        let folder = dir.path().join("recordings/2026-01-05 Quarterly review");
        std::fs::create_dir_all(&folder).unwrap();
        let video = folder.join("Board call.mp4");
        make_video(
            &t,
            &video,
            5,
            Some("aac"),
            &["-c:v", "libx264", "-preset", "ultrafast"],
        );
        std::fs::write(folder.join("Board call.mp4.jpg"), b"poster").unwrap();
        fly_audio::mix::write_wav_mono_16(
            &folder.join("recording.mixed.wav"),
            &[0.0; 1600],
            16_000,
        )
        .unwrap();
        s.end_meeting(
            &m.id,
            &RecordingRef {
                mic_path: None,
                system_path: None,
                mixed_path: Some(
                    "recordings/2026-01-05 Quarterly review/recording.mixed.wav".into(),
                ),
                playback_path: None,
                duration_ms: 100,
            },
        )
        .unwrap();
        s.save_transcript(&fly_core::Transcript {
            meeting_id: m.id.clone(),
            language: None,
            engine: "test".into(),
            segments: vec![],
            speakers: vec![],
        })
        .unwrap();
        (note.id, m.id, video)
    };
    // (repointing an old attachment of the copy is unit-tested in
    // fly-storage's recording_store.rs)
    let _ = note_id;
    let found = imported_videos(&state);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].meeting_id.as_deref(), Some(meeting_id.as_str()));
    assert!(screen_recordings(&state).is_empty());
    let saved = audio_only_one(&state, &t, &found[0], &go()).unwrap();
    assert!(saved > 0);
    let folder = video.parent().unwrap();
    assert!(!video.exists(), "the app's video copy is gone");
    assert!(
        !folder.join("Board call.mp4.jpg").exists(),
        "its poster too"
    );
    assert!(folder.join("Board call.m4a").exists());
    assert!(
        folder.join("recording.mixed.wav").exists(),
        "the transcription track stays"
    );
    assert!(imported_videos(&state).is_empty(), "nothing left to do");
}
