//! Opt-in video storage (Settings › Your files and privacy). Videos are
//! never touched unless the user asks; nothing lossless shrinks them, so
//! the two choices trade the picture or bit-exactness for space:
//!
//! - **Imported videos → keep audio only.** Media import keeps a copy of
//!   each video in the meeting folder. This removes the picture: every audio
//!   track is copied out of the video WITHOUT re-encoding (`-c copy`), proven
//!   identical packet for packet (`framemd5` of the original's audio vs the
//!   new file's), attachments are repointed, then the app's video copy goes.
//!   The user's own original file (wherever they imported it from) is never
//!   touched. The transcription track (`recording.mixed.flac`) is kept too.
//! - **Screen recordings → visually lossless re-encode.** x264 (slow preset,
//!   CRF 18), audio copied as is. The original is replaced only once the new
//!   file decodes end to end, has the same number of video frames and
//!   duration, the same audio packets, an SSIM of at least
//!   [`SSIM_MIN`] against the original, and saves at least [`MIN_SAVING`].
//!
//! One job at a time, on its own background thread; ffmpeg runs at below-
//! normal priority. While anything records, transcribes or imports the job
//! kills the ffmpeg run in flight (its temp file is deleted, the original is
//! untouched), waits, and starts that file again.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

use crate::state::AppState;

/// Event carrying [`VideoJobStatus`] whenever it changes.
pub const VIDEO_EVENT: &str = "storage:videos";
/// `keep` (default) or `audio_only`: what happens to an imported video once
/// its import is transcribed. Internal (not webview-settable).
pub const IMPORT_VIDEO_POLICY_KEY: &str = "storage.import_video";
/// Container comment a shrunk screen recording carries, so it's never
/// re-encoded twice.
const SHRUNK_TAG: &str = "fotw:visually-lossless-v1";
/// Minimum SSIM (all planes, whole video) for "visually lossless".
pub const SSIM_MIN: f64 = 0.98;
/// A re-encode that saves less than this is discarded.
pub const MIN_SAVING: f64 = 0.10;
const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm", "avi"];
const POLL: Duration = Duration::from_millis(300);

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoJobKind {
    /// Imported videos → their audio, copied without re-encoding.
    AudioOnly,
    /// Screen recordings → visually lossless re-encode.
    ShrinkScreen,
}

/// What Settings shows about the video job.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct VideoJobStatus {
    pub kind: Option<VideoJobKind>,
    /// `idle`, `working`, `paused`, `done`, `stopped`.
    pub state: String,
    pub paused_for: Option<String>,
    pub files_total: usize,
    pub files_done: usize,
    /// Files left as they were (no audio track, already compact, a check
    /// failed); never deleted.
    pub files_skipped: usize,
    /// Freed by this run.
    pub bytes_saved: u64,
    pub last_error: Option<String>,
    /// `keep` or `audio_only` (see [`IMPORT_VIDEO_POLICY_KEY`]).
    pub import_policy: String,
}

/// Lives in [`AppState::video_store`].
#[derive(Default)]
pub struct VideoState {
    status: Mutex<VideoJobStatus>,
    running: AtomicBool,
    cancel: AtomicBool,
    /// The file in flight (and its meeting, for imported videos).
    current: Mutex<Option<Candidate>>,
}

impl VideoState {
    pub fn status(&self) -> VideoJobStatus {
        let mut s = self.status.lock().unwrap().clone();
        if s.state.is_empty() {
            s.state = "idle".into();
        }
        s
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// The meeting whose folder the job is working in, if any.
    pub fn current_meeting(&self) -> Option<String> {
        self.current
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|c| c.meeting_id.clone())
    }
}

/// Status for Settings, with the saved import policy.
pub fn status_for(state: &AppState) -> VideoJobStatus {
    let mut s = state.video_store.status();
    s.import_policy = import_policy(state);
    s
}

/// A note is being deleted: if the job is working on one of its files
/// (an imported video in one of `meeting_ids`' folders, or a screen
/// recording in its attachments), stop the job and wait for it to let go.
pub fn stop_for_note(state: &AppState, note_id: &str, meeting_ids: &[String], timeout: Duration) {
    let hit = state
        .video_store
        .current
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|c| {
            c.meeting_id
                .as_ref()
                .is_some_and(|m| meeting_ids.contains(m))
                || c.path
                    .starts_with(state.data_dir.join("attachments").join(note_id))
        });
    if hit && !halt(state, timeout) {
        tracing::warn!(note_id, "video job didn't stop before the note was deleted");
    }
}

fn emit<R: tauri::Runtime>(app: &tauri::AppHandle<R>, f: impl FnOnce(&mut VideoJobStatus)) {
    let state = app.state::<AppState>();
    let snap = {
        let mut s = state.video_store.status.lock().unwrap();
        f(&mut s);
        s.clone()
    };
    let _ = app.emit(VIDEO_EVENT, snap);
}

fn import_policy(state: &AppState) -> String {
    match state
        .storage
        .lock()
        .unwrap()
        .get_setting(IMPORT_VIDEO_POLICY_KEY)
        .ok()
        .flatten()
        .as_deref()
    {
        Some("audio_only") => "audio_only".into(),
        _ => "keep".into(),
    }
}

// ---------------------------------------------------------------------------
// ffmpeg, run so it can be stopped
// ---------------------------------------------------------------------------

/// The ffmpeg + ffprobe pair the app ships (fetched on first use), or the
/// ones on PATH.
#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Tools {
    /// ffprobe next to `ffmpeg` (both ship in the same archive / package).
    pub fn beside(ffmpeg: PathBuf) -> Self {
        let name = if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        };
        let ffprobe = ffmpeg.with_file_name(name);
        let ffprobe = if ffprobe.exists() {
            ffprobe
        } else {
            PathBuf::from(name) // PATH
        };
        Self { ffmpeg, ffprobe }
    }
}

#[derive(Debug, PartialEq)]
pub enum VideoError {
    /// Something needed the machine, or the user stopped the job.
    Stopped,
    /// Left as it is, with the reason (never an error for the user).
    Skipped(String),
    Failed(String),
}

impl std::fmt::Display for VideoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VideoError::Stopped => write!(f, "stopped"),
            VideoError::Skipped(w) => write!(f, "skipped: {w}"),
            VideoError::Failed(w) => write!(f, "{w}"),
        }
    }
}

fn failed(e: impl std::fmt::Display) -> VideoError {
    VideoError::Failed(e.to_string())
}

/// Run a tool to completion, polling `keep_going`; a `false` kills it.
/// Returns (stdout, stderr) of a successful run.
pub fn run_tool(
    exe: &Path,
    args: &[std::ffi::OsString],
    keep_going: &dyn Fn() -> bool,
) -> Result<(String, String), VideoError> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
        cmd.creation_flags(0x0800_0000 | 0x0000_4000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| failed(format!("couldn't run {}: {e}", exe.display())))?;
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut s);
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut err, &mut s);
        s
    });
    let status = loop {
        if let Some(st) = child.try_wait().map_err(failed)? {
            break st;
        }
        if !keep_going() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = (out_t.join(), err_t.join());
            return Err(VideoError::Stopped);
        }
        std::thread::sleep(POLL);
    };
    let (stdout, stderr) = (
        out_t.join().unwrap_or_default(),
        err_t.join().unwrap_or_default(),
    );
    if !status.success() {
        let tail: String = stderr
            .lines()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" / ");
        return Err(failed(format!("ffmpeg failed: {tail}")));
    }
    Ok((stdout, stderr))
}

fn os(args: &[&str]) -> Vec<std::ffi::OsString> {
    args.iter().map(std::ffi::OsString::from).collect()
}

fn with_path(mut v: Vec<std::ffi::OsString>, at: usize, p: &Path) -> Vec<std::ffi::OsString> {
    v.insert(at, p.as_os_str().to_os_string());
    v
}

// ---------------------------------------------------------------------------
// Probing and checks (pure parsing, unit-tested)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Probe {
    pub audio_codecs: Vec<String>,
    pub has_video: bool,
    pub duration_s: f64,
    pub comment: Option<String>,
}

/// Parse `ffprobe -show_entries stream=index,codec_type,codec_name:
/// format=duration:format_tags=comment -of json`.
pub fn parse_probe(json: &str) -> Option<Probe> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut p = Probe::default();
    for s in v.get("streams")?.as_array()? {
        match s.get("codec_type").and_then(|t| t.as_str()) {
            Some("audio") => p.audio_codecs.push(
                s.get("codec_name")
                    .and_then(|c| c.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
            ),
            Some("video") => p.has_video = true,
            _ => {}
        }
    }
    let f = v.get("format")?;
    p.duration_s = f
        .get("duration")
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);
    p.comment = f
        .get("tags")
        .and_then(|t| t.get("comment"))
        .and_then(|c| c.as_str())
        .map(str::to_string);
    Some(p)
}

/// The audio-only container that holds these codecs unchanged (and that
/// players open): AAC/ALAC → `.m4a`, a lone MP3 → `.mp3`, Opus/Vorbis →
/// `.ogg`, a lone FLAC → `.flac`, anything else (PCM, several mixed
/// codecs…) → Matroska audio `.mka`, which holds any codec.
pub fn container_for(codecs: &[String]) -> &'static str {
    let all = |f: &dyn Fn(&str) -> bool| !codecs.is_empty() && codecs.iter().all(|c| f(c));
    if all(&|c| c == "aac" || c == "alac") {
        "m4a"
    } else if codecs.len() == 1 && codecs[0] == "mp3" {
        "mp3"
    } else if all(&|c| c == "opus" || c == "vorbis") {
        "ogg"
    } else if codecs.len() == 1 && codecs[0] == "flac" {
        "flac"
    } else {
        "mka"
    }
}

/// `(stream, size, hash)` of every packet in `ffmpeg -f framemd5` output.
/// Timestamps are left out: a new container may use a different timebase,
/// but the packet bytes must be identical.
pub fn packet_hashes(framemd5: &str) -> Vec<(String, String, String)> {
    framemd5
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').map(str::trim).collect();
            (f.len() >= 6).then(|| (f[0].to_string(), f[4].to_string(), f[5].to_string()))
        })
        .collect()
}

/// The `All:` value from ffmpeg's `ssim` filter summary.
pub fn parse_ssim(stderr: &str) -> Option<f64> {
    let line = stderr
        .lines()
        .rev()
        .find(|l| l.contains("SSIM ") && l.contains("All:"))?;
    let rest = line.split("All:").nth(1)?;
    rest.split_whitespace().next()?.parse().ok()
}

fn probe(t: &Tools, file: &Path, keep_going: &dyn Fn() -> bool) -> Result<Probe, VideoError> {
    let args = with_path(
        os(&[
            "-v",
            "error",
            "-show_entries",
            "stream=index,codec_type,codec_name:format=duration:format_tags=comment",
            "-of",
            "json",
        ]),
        6,
        file,
    );
    let (out, _) = run_tool(&t.ffprobe, &args, keep_going)?;
    parse_probe(&out).ok_or_else(|| failed("couldn't read the video's streams"))
}

fn video_packets(t: &Tools, file: &Path, keep_going: &dyn Fn() -> bool) -> Result<u64, VideoError> {
    let args = with_path(
        os(&[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_packets",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "csv=p=0",
        ]),
        9,
        file,
    );
    let (out, _) = run_tool(&t.ffprobe, &args, keep_going)?;
    out.trim()
        .trim_end_matches(',')
        .parse()
        .map_err(|_| failed("couldn't count the video's frames"))
}

fn audio_hashes(
    t: &Tools,
    file: &Path,
    keep_going: &dyn Fn() -> bool,
) -> Result<Vec<(String, String, String)>, VideoError> {
    let mut args = os(&["-hide_banner", "-loglevel", "error", "-i"]);
    args.push(file.as_os_str().to_os_string());
    args.extend(os(&["-map", "0:a?", "-c", "copy", "-f", "framemd5", "-"]));
    let (out, _) = run_tool(&t.ffmpeg, &args, keep_going)?;
    Ok(packet_hashes(&out))
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn keep_mtime(from: &Path, to: &Path) {
    if let Some(t) = mtime(from) {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(to) {
            let _ = f.set_modified(t);
        }
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn rel_of(data_dir: &Path, p: &Path) -> Option<String> {
    p.strip_prefix(data_dir)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

// ---------------------------------------------------------------------------
// One file each
// ---------------------------------------------------------------------------

/// Copy a video's audio out without re-encoding and prove it. Returns the
/// audio file (in place next to the video; the video is NOT deleted here).
pub fn extract_audio(
    t: &Tools,
    video: &Path,
    keep_going: &dyn Fn() -> bool,
) -> Result<PathBuf, VideoError> {
    let p = probe(t, video, keep_going)?;
    if !p.has_video {
        return Err(VideoError::Skipped("it has no picture to remove".into()));
    }
    if p.audio_codecs.is_empty() {
        return Err(VideoError::Skipped("it has no audio track".into()));
    }
    let ext = container_for(&p.audio_codecs);
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".into());
    let dir = video.parent().ok_or_else(|| failed("no folder"))?;
    let want = audio_hashes(t, video, keep_going)?;
    if want.is_empty() {
        return Err(VideoError::Skipped("it has no audio packets".into()));
    }
    // An earlier run already made (and proved) it, then couldn't delete the
    // video: reuse it rather than making a second copy.
    let mut out = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while out.exists() {
        if audio_hashes(t, &out, keep_going)? == want {
            return Ok(out);
        }
        out = dir.join(format!("{stem} ({n}).{ext}"));
        n += 1;
    }
    let temp = dir.join(format!(".{stem}.audio-only.part.{ext}"));
    let _ = std::fs::remove_file(&temp);
    let result = (|| {
        let mut args = os(&["-hide_banner", "-loglevel", "error", "-i"]);
        args.push(video.as_os_str().to_os_string());
        args.extend(os(&[
            "-map",
            "0:a",
            "-map_metadata",
            "0",
            "-map_chapters",
            "0",
            "-c",
            "copy",
            "-vn",
            "-sn",
            "-dn",
            "-y",
        ]));
        args.push(temp.as_os_str().to_os_string());
        run_tool(&t.ffmpeg, &args, keep_going)?;
        let got = audio_hashes(t, &temp, keep_going)?;
        if want != got {
            return Err(failed(
                "the copied audio doesn't match the video's audio packet for packet",
            ));
        }
        keep_mtime(video, &temp);
        fly_storage::rename_with_retry(&temp, &out).map_err(failed)?;
        Ok(out.clone())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Re-encode a screen recording and prove it's a visually lossless,
/// smaller stand-in. Replaces `video` in place only once every check
/// passed; returns the bytes saved.
pub fn shrink_screen(
    t: &Tools,
    video: &Path,
    threads: usize,
    keep_going: &dyn Fn() -> bool,
) -> Result<u64, VideoError> {
    let src = probe(t, video, keep_going)?;
    if src.comment.as_deref() == Some(SHRUNK_TAG) {
        return Err(VideoError::Skipped("it was already shrunk".into()));
    }
    if !src.has_video {
        return Err(VideoError::Skipped("it has no video".into()));
    }
    let before = std::fs::metadata(video).map_err(failed)?.len();
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "screen".into());
    let temp = video.with_file_name(format!(".{stem}.shrinking.mp4"));
    let _ = std::fs::remove_file(&temp);
    let result = (|| {
        let mut args = os(&["-hide_banner", "-loglevel", "error", "-i"]);
        args.push(video.as_os_str().to_os_string());
        args.extend(os(&[
            "-map",
            "0:v:0",
            "-map",
            "0:a?",
            "-c:v",
            "libx264",
            "-preset",
            "slow",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            "-fps_mode",
            "passthrough",
            "-c:a",
            "copy",
            "-map_metadata",
            "0",
            "-metadata",
        ]));
        args.push(format!("comment={SHRUNK_TAG}").into());
        args.extend(os(&["-movflags", "+faststart", "-threads"]));
        args.push(threads.max(1).to_string().into());
        args.push("-y".into());
        args.push(temp.as_os_str().to_os_string());
        run_tool(&t.ffmpeg, &args, keep_going)?;

        let after = std::fs::metadata(&temp).map_err(failed)?.len();
        if (after as f64) > (before as f64) * (1.0 - MIN_SAVING) {
            return Err(VideoError::Skipped("it's already compact".into()));
        }
        // 1. decodes end to end without an error
        let mut args = os(&["-hide_banner", "-v", "error", "-i"]);
        args.push(temp.as_os_str().to_os_string());
        args.extend(os(&["-f", "null", "-"]));
        let (_, errs) = run_tool(&t.ffmpeg, &args, keep_going)?;
        if !errs.trim().is_empty() {
            return Err(failed("the new file has decode errors"));
        }
        // 2. same frames, same length
        let (a, b) = (
            video_packets(t, video, keep_going)?,
            video_packets(t, &temp, keep_going)?,
        );
        if a != b {
            return Err(failed(format!("frame count {b} vs {a}")));
        }
        let new = probe(t, &temp, keep_going)?;
        if (new.duration_s - src.duration_s).abs() > (src.duration_s * 0.01).max(0.5) {
            return Err(failed("the new file's length differs"));
        }
        // 3. audio untouched
        if audio_hashes(t, video, keep_going)? != audio_hashes(t, &temp, keep_going)? {
            return Err(failed("the audio changed"));
        }
        // 4. looks the same
        let mut args = os(&["-hide_banner", "-nostats", "-i"]);
        args.push(video.as_os_str().to_os_string());
        args.push("-i".into());
        args.push(temp.as_os_str().to_os_string());
        args.extend(os(&["-lavfi", "[0:v][1:v]ssim", "-f", "null", "-"]));
        let (_, log) = run_tool(&t.ffmpeg, &args, keep_going)?;
        let ssim = parse_ssim(&log).ok_or_else(|| failed("couldn't measure the quality"))?;
        if ssim < SSIM_MIN {
            return Err(VideoError::Skipped(format!(
                "the re-encode would look different (SSIM {ssim:.3})"
            )));
        }
        keep_mtime(video, &temp);
        fly_storage::rename_with_retry(&temp, video).map_err(failed)?;
        Ok(before.saturating_sub(after))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

// ---------------------------------------------------------------------------
// Finding the files
// ---------------------------------------------------------------------------

/// A video the job can work on.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub path: PathBuf,
    /// The meeting whose folder holds it (imported videos).
    pub meeting_id: Option<String>,
    pub bytes: u64,
}

fn is_video(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Imported video copies in the folders of FINISHED imports (transcript
/// saved, nothing queued or running for the meeting).
pub fn imported_videos(state: &AppState) -> Vec<Candidate> {
    let storage = state.storage.lock().unwrap();
    let Ok(ids) = storage.meetings_with_stored_audio() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for id in ids {
        let busy = storage
            .transcription_job(&id)
            .ok()
            .flatten()
            .is_some_and(|j| {
                j.status == fly_storage::JOB_QUEUED || j.status == fly_storage::JOB_RUNNING
            });
        if busy || !storage.get_transcript(&id).is_ok_and(|t| t.is_some()) {
            continue;
        }
        let Some(dir) = storage
            .get_meeting(&id)
            .ok()
            .and_then(|m| m.recording)
            .and_then(|r| fly_storage::recording_dir_rel(&r))
            .and_then(|rel| fly_storage::contained_recordings_dir(&state.data_dir, &rel))
        else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let plain = std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_file());
            if plain && is_video(&p) && !file_name(&p).starts_with('.') {
                let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                out.push(Candidate {
                    path: p,
                    meeting_id: Some(id.clone()),
                    bytes,
                });
            }
        }
    }
    out
}

/// Screen recordings (`attachments/<note>/screen-*.mp4`), except the one
/// being recorded right now.
pub fn screen_recordings(state: &AppState) -> Vec<Candidate> {
    let live = state
        .screen
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| state.data_dir.join(&s.rel_path));
    let mut out = Vec::new();
    let Ok(notes) = std::fs::read_dir(state.data_dir.join("attachments")) else {
        return out;
    };
    for d in notes.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            let name = file_name(&p);
            let plain = std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_file());
            if plain
                && name.starts_with("screen-")
                && name.ends_with(".mp4")
                && Some(&p) != live.as_ref()
            {
                let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                out.push(Candidate {
                    path: p,
                    meeting_id: None,
                    bytes,
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The job
// ---------------------------------------------------------------------------

/// The shared background rule (#112), named as Settings shows it. Asked
/// between files and on every ffmpeg poll (`POLL`) while one runs.
fn busy(state: &AppState) -> Option<&'static str> {
    crate::audio_store::paused_for(state)
}

async fn ensure_tools<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    data_dir: &Path,
) -> Result<Tools, String> {
    let on_model = {
        let app = app.clone();
        move |p: crate::models::ModelProgress| {
            let _ = app.emit("model:progress", p);
        }
    };
    let ffmpeg = crate::models::ensure_tool(
        &on_model,
        data_dir,
        "ffmpeg",
        &["ffmpeg"],
        "install ffmpeg (macOS: brew install ffmpeg)",
    )
    .await?;
    Ok(Tools::beside(ffmpeg))
}

/// Start a job (Settings). One at a time.
#[tauri::command]
pub async fn start_video_job(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    kind: VideoJobKind,
) -> Result<VideoJobStatus, String> {
    start(&app, &state, kind).await
}

async fn start<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    kind: VideoJobKind,
) -> Result<VideoJobStatus, String> {
    let vs = &state.video_store;
    if vs.running.swap(true, Ordering::SeqCst) {
        return Err("a video job is already running".into());
    }
    let tools = match ensure_tools(app, &state.data_dir).await {
        Ok(t) => t,
        Err(e) => {
            vs.running.store(false, Ordering::SeqCst);
            return Err(format!("ffmpeg isn't available: {e}"));
        }
    };
    vs.cancel.store(false, Ordering::SeqCst);
    let policy = import_policy(state);
    emit(app, |s| {
        *s = VideoJobStatus {
            kind: Some(kind),
            state: "working".into(),
            import_policy: policy,
            ..Default::default()
        }
    });
    let handle = app.clone();
    let spawned = std::thread::Builder::new()
        .name("fotw-videos".into())
        .spawn(move || {
            crate::audio_store::enter_background_priority();
            run_job(&handle, kind, &tools);
            handle
                .state::<AppState>()
                .video_store
                .running
                .store(false, Ordering::SeqCst);
        });
    if let Err(e) = spawned {
        vs.running.store(false, Ordering::SeqCst);
        return Err(e.to_string());
    }
    Ok(vs.status())
}

fn run_job<R: tauri::Runtime>(app: &tauri::AppHandle<R>, kind: VideoJobKind, tools: &Tools) {
    let state = app.state::<AppState>();
    let vs = &state.video_store;
    let files = match kind {
        VideoJobKind::AudioOnly => imported_videos(&state),
        VideoJobKind::ShrinkScreen => screen_recordings(&state),
    };
    emit(app, |s| s.files_total = files.len());
    let threads = std::thread::available_parallelism()
        .map(|n| (n.get() / 4).clamp(1, 4))
        .unwrap_or(2);
    for c in files {
        if vs.cancel.load(Ordering::SeqCst) {
            break;
        }
        loop {
            // wait out whatever needs the machine
            let mut paused = false;
            while let Some(why) = busy(&state) {
                if vs.cancel.load(Ordering::SeqCst) {
                    break;
                }
                if !paused {
                    paused = true;
                    emit(app, |s| {
                        s.state = "paused".into();
                        s.paused_for = Some(why.into());
                    });
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            if paused {
                emit(app, |s| {
                    s.state = "working".into();
                    s.paused_for = None;
                });
            }
            if vs.cancel.load(Ordering::SeqCst) {
                break;
            }
            let keep_going = || !vs.cancel.load(Ordering::SeqCst) && busy(&state).is_none();
            *vs.current.lock().unwrap() = Some(c.clone());
            let result = match kind {
                VideoJobKind::AudioOnly => audio_only_one(&state, tools, &c, &keep_going),
                VideoJobKind::ShrinkScreen => shrink_screen(tools, &c.path, threads, &keep_going),
            };
            *vs.current.lock().unwrap() = None;
            match result {
                Err(VideoError::Stopped) if !vs.cancel.load(Ordering::SeqCst) => continue,
                Err(VideoError::Stopped) => break,
                Ok(saved) => {
                    tracing::info!(saved, "videos: file done");
                    emit(app, |s| {
                        s.files_done += 1;
                        s.bytes_saved += saved;
                    });
                }
                Err(VideoError::Skipped(why)) => {
                    tracing::info!(why, "videos: file left as it is");
                    emit(app, |s| {
                        s.files_done += 1;
                        s.files_skipped += 1;
                    });
                }
                Err(VideoError::Failed(e)) => {
                    tracing::warn!(error = %e, "videos: file failed (original kept)");
                    emit(app, |s| {
                        s.files_done += 1;
                        s.files_skipped += 1;
                        s.last_error = Some(e.clone());
                    });
                }
            }
            break;
        }
    }
    let stopped = vs.cancel.load(Ordering::SeqCst);
    emit(app, |s| {
        s.state = if stopped { "stopped" } else { "done" }.into();
        s.paused_for = None;
    });
}

/// Audio-only for one imported video: lease its meeting folder, extract
/// and prove the audio, repoint attachments, then delete the video copy
/// (and its poster thumbnail).
pub fn audio_only_one(
    state: &AppState,
    tools: &Tools,
    c: &Candidate,
    keep_going: &dyn Fn() -> bool,
) -> Result<u64, VideoError> {
    let id = c.meeting_id.clone().unwrap_or_default();
    if !state.storage.lock().unwrap().lease_meeting_folder(&id) {
        // recording compression is in that folder; take it again later
        return Err(VideoError::Stopped);
    }
    struct Release<'a>(&'a AppState, &'a str);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            self.0
                .storage
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .release_meeting_folder(self.1);
        }
    }
    let _release = Release(state, &id);
    if !c.path.exists() {
        return Err(VideoError::Skipped("it's gone".into()));
    }
    let audio = extract_audio(tools, &c.path, keep_going)?;
    let (Some(old_rel), Some(new_rel)) = (
        rel_of(&state.data_dir, &c.path),
        rel_of(&state.data_dir, &audio),
    ) else {
        return Err(failed("file outside the data folder"));
    };
    let audio_bytes = std::fs::metadata(&audio).map(|m| m.len()).unwrap_or(0);
    {
        let storage = state.storage.lock().unwrap();
        storage
            .repoint_attachments(&old_rel, &new_rel)
            .map_err(|e| {
                let _ = std::fs::remove_file(&audio);
                failed(e)
            })?;
        // the audio file now carries everything that file did but the picture
        std::fs::remove_file(&c.path).map_err(failed)?;
        let _ = std::fs::remove_file(c.path.with_file_name(format!("{}.jpg", file_name(&c.path))));
    }
    Ok(c.bytes.saturating_sub(audio_bytes))
}

/// Stop the running job (it finishes the file in flight cleanly: the temp
/// is deleted, the original untouched).
#[tauri::command]
pub fn cancel_video_job(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.video_store.cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// Stop for good and wait (Delete all my data).
pub fn halt(state: &AppState, timeout: Duration) -> bool {
    state.video_store.cancel.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + timeout;
    while state.video_store.is_running() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// `keep` or `audio_only`. Choosing audio only starts the job for the
/// imported videos already there (the UI confirms first); later imports
/// follow once their transcript is saved.
#[tauri::command]
pub async fn set_import_video_policy(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    policy: String,
) -> Result<VideoJobStatus, String> {
    if policy != "keep" && policy != "audio_only" {
        return Err("unknown policy".into());
    }
    state
        .storage
        .lock()
        .unwrap()
        .set_setting(IMPORT_VIDEO_POLICY_KEY, &policy)
        .map_err(|e| e.to_string())?;
    emit(&app, |s| s.import_policy = policy.clone());
    if policy == "audio_only" && !state.video_store.is_running() {
        return start(&app, &state, VideoJobKind::AudioOnly).await;
    }
    Ok(state.video_store.status())
}

/// After a transcription finishes: with the audio-only policy, a new
/// import's video copy goes the same way (background, best-effort).
pub fn after_transcription<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) {
    let state = handle.state::<AppState>();
    if import_policy(&state) != "audio_only" || state.video_store.is_running() {
        return;
    }
    if imported_videos(&state).is_empty() {
        return;
    }
    let app = handle.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        if let Err(e) = start(&app, &state, VideoJobKind::AudioOnly).await {
            tracing::info!(error = %e, "videos: audio-only after import not started");
        }
    });
}

/// What shrinking the screen recordings would save, from a 30 s sample of
/// the largest one (encoded with the real settings into scratch).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ShrinkEstimate {
    pub files: usize,
    pub bytes: u64,
    /// Estimated size afterwards.
    pub bytes_after: u64,
}

#[tauri::command]
pub async fn estimate_screen_shrink(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Option<ShrinkEstimate>, String> {
    let files = screen_recordings(&state);
    let Some(largest) = files.iter().max_by_key(|c| c.bytes).cloned() else {
        return Ok(None);
    };
    let tools = ensure_tools(&app, &state.data_dir).await?;
    let scratch = fly_storage::scratch::scratch_root(&state.data_dir).join("_video-estimate");
    let total: u64 = files.iter().map(|c| c.bytes).sum();
    let count = files.len();
    let ratio = tauri::async_runtime::spawn_blocking(move || -> Result<f64, String> {
        std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
        let go = || true;
        let p = probe(&tools, &largest.path, &go).map_err(|e| e.to_string())?;
        if p.comment.as_deref() == Some(SHRUNK_TAG) || p.duration_s <= 0.0 {
            return Ok(1.0);
        }
        let len = p.duration_s.min(30.0);
        let start = ((p.duration_s - len) / 2.0).max(0.0);
        let out = scratch.join("sample.mp4");
        let mut args = os(&["-hide_banner", "-loglevel", "error", "-ss"]);
        args.push(format!("{start:.2}").into());
        args.push("-i".into());
        args.push(largest.path.as_os_str().to_os_string());
        args.push("-t".into());
        args.push(format!("{len:.2}").into());
        args.extend(os(&[
            "-map", "0:v:0", "-map", "0:a?", "-c:v", "libx264", "-preset", "slow", "-crf", "18",
            "-pix_fmt", "yuv420p", "-c:a", "copy", "-y",
        ]));
        args.push(out.as_os_str().to_os_string());
        let r = run_tool(&tools.ffmpeg, &args, &go);
        let sample = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_dir_all(&scratch);
        r.map_err(|e| e.to_string())?;
        let src_rate = largest.bytes as f64 / p.duration_s;
        let new_rate = sample as f64 / len;
        Ok((new_rate / src_rate).clamp(0.0, 1.0))
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(Some(ShrinkEstimate {
        files: count,
        bytes: total,
        bytes_after: (total as f64 * ratio) as u64,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_json_is_read() {
        let p = parse_probe(
            r#"{"streams":[{"index":0,"codec_name":"h264","codec_type":"video"},
                {"index":1,"codec_name":"aac","codec_type":"audio"},
                {"index":2,"codec_name":"opus","codec_type":"audio"}],
               "format":{"duration":"6.000000","tags":{"comment":"fotw:visually-lossless-v1"}}}"#,
        )
        .unwrap();
        assert_eq!(p.audio_codecs, vec!["aac", "opus"]);
        assert!(p.has_video);
        assert!((p.duration_s - 6.0).abs() < 1e-9);
        assert_eq!(p.comment.as_deref(), Some(SHRUNK_TAG));
        assert!(parse_probe("not json").is_none());
    }

    #[test]
    fn audio_keeps_its_codec_in_a_container_that_holds_it() {
        let c = |v: &[&str]| container_for(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(c(&["aac"]), "m4a");
        assert_eq!(c(&["aac", "alac"]), "m4a");
        assert_eq!(c(&["mp3"]), "mp3");
        assert_eq!(c(&["mp3", "mp3"]), "mka");
        assert_eq!(c(&["opus"]), "ogg");
        assert_eq!(c(&["flac"]), "flac");
        assert_eq!(c(&["pcm_s16le"]), "mka");
        assert_eq!(c(&["aac", "opus"]), "mka");
    }

    #[test]
    fn packet_hashes_ignore_timestamps() {
        let a = "#format: frame checksums\n#tb 0: 1/48000\n0,     0,     0,  1024,  288, aa\n0,  1024,  1024,  1024,  506, bb\n";
        let b =
            "#tb 0: 1/1000\n0,     0,     0,    21,  288, aa\n0,    21,    21,    21,  506, bb\n";
        assert_eq!(packet_hashes(a), packet_hashes(b));
        assert_eq!(packet_hashes(a).len(), 2);
        let c = "0, 0, 0, 1024, 288, aa\n0, 1024, 1024, 1024, 506, cc\n";
        assert_ne!(packet_hashes(a), packet_hashes(c));
    }

    #[test]
    fn ssim_summary_is_read() {
        let log = "[Parsed_ssim_0 @ 0x1] SSIM Y:0.997531 (26.07) U:0.998700 (28.85) V:0.998728 (28.95) All:0.997926 (26.831007)\n";
        assert_eq!(parse_ssim(log), Some(0.997926));
        assert_eq!(parse_ssim("nothing"), None);
    }
}
