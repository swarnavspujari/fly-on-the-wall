//! File import (M8): turn existing audio/video files into a normal note.
//!
//! The flow is staged, not immediate: `import_stage` picks files and creates
//! the note + meeting (copying every original into the meeting's folder), but
//! nothing transcribes until the user hits Transcribe in the import queue —
//! `import_transcribe` then normalizes each file IN THE USER'S ORDER to the
//! 16 kHz mono track the pipeline expects, concatenates them (1 s of silence
//! between files) into one `recording.mixed.wav`, and enqueues the same
//! transcribe → diarize → polish flow as a live recording. The staged state
//! lives in the settings table so a reopened note finds its queue again.

use std::path::{Path, PathBuf};

use fly_core::RecordingRef;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tauri_plugin_dialog::DialogExt;

use crate::state::AppState;
use crate::{models, scheduler};

type CmdResult<T> = Result<T, String>;

const AUDIO_EXTS: &[&str] = &["wav", "mp3", "m4a", "aac", "flac", "ogg"];
const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "webm"];
/// Silence inserted between concatenated files so ASR never merges the last
/// word of one recording with the first word of the next.
const GAP_MS: u64 = 1000;

fn manifest_key(meeting_id: &str) -> String {
    format!("import.staged.{meeting_id}")
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ImportFile {
    pub id: String,
    pub file_name: String,
    pub size: u64,
    /// "audio" | "video"
    pub kind: String,
    /// Source copy inside the data dir (None for unsupported files).
    pub rel_path: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ImportStaged {
    pub note_id: String,
    pub meeting_id: String,
    pub files: Vec<ImportFile>,
    /// Set at transcribe time: cumulative end (ms) of each supported file on
    /// the concatenated timeline, parallel to the (reordered) `files`. The
    /// frontend maps the pipeline's global transcription % onto per-file
    /// progress with these.
    pub boundaries_ms: Vec<u64>,
    pub started: bool,
}

/// Per-file conversion progress while `import_transcribe` normalizes the
/// queue (the ASR % rides the normal `pipeline:progress` stream afterwards).
#[derive(Clone, Serialize)]
struct ImportProgress {
    meeting_id: String,
    file_id: String,
    /// "converting" | "converted"
    stage: String,
}

fn load_staged(state: &AppState, meeting_id: &str) -> CmdResult<Option<ImportStaged>> {
    let raw = state
        .storage
        .lock()
        .unwrap()
        .get_setting(&manifest_key(meeting_id))
        .map_err(|e| e.to_string())?;
    match raw {
        Some(json) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

fn save_staged(state: &AppState, staged: &ImportStaged) -> CmdResult<()> {
    let json = serde_json::to_string(staged).map_err(|e| e.to_string())?;
    state
        .storage
        .lock()
        .unwrap()
        .set_setting(&manifest_key(&staged.meeting_id), &json)
        .map_err(|e| e.to_string())
}

/// Keep the original file name readable on disk but safe cross-platform,
/// and unique within the meeting folder.
fn copy_name(dir: &Path, original: &str) -> String {
    let safe: String = original
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let (stem, ext) = match safe.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (safe.clone(), String::new()),
    };
    let mut name = format!("{stem}{ext}");
    let mut n = 2;
    while dir.join(&name).exists() {
        name = format!("{stem} ({n}){ext}");
        n += 1;
    }
    name
}

/// Pick audio/video files and stage them as ONE new note. Returns None if the
/// user cancels. Nothing transcribes yet — the note opens on the import queue
/// and `import_transcribe` runs when the user confirms the order.
#[tauri::command]
pub async fn import_stage(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Option<ImportStaged>> {
    let exts: Vec<&str> = AUDIO_EXTS.iter().chain(VIDEO_EXTS).copied().collect();
    let Some(picked) = app
        .dialog()
        .file()
        .add_filter("Audio / video", &exts)
        .blocking_pick_files()
    else {
        return Ok(None);
    };
    let sources: Vec<PathBuf> = picked
        .into_iter()
        .filter_map(|f| f.into_path().ok())
        .collect();
    if sources.is_empty() {
        return Ok(None);
    }

    let title = sources[0]
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported media")
        .to_string();

    // note + meeting first, so everything lands in the meeting's folder
    let (note_id, meeting_id, rec_dir) = {
        let storage = state.storage.lock().unwrap();
        let note = storage
            .create_note(&title, None)
            .map_err(|e| e.to_string())?;
        let meeting = storage
            .create_meeting(&title, &note.id, &[])
            .map_err(|e| e.to_string())?;
        let rec_dir = storage
            .allocate_meeting_dir(&title, meeting.started_at)
            .map_err(|e| e.to_string())?;
        (note.id, meeting.id, rec_dir)
    };

    let mut files = Vec::with_capacity(sources.len());
    for src in &sources {
        let file_name = src
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("recording")
            .to_string();
        let size = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let kind = if VIDEO_EXTS.contains(&ext.as_str()) {
            "video"
        } else {
            "audio"
        };
        let supported = AUDIO_EXTS.contains(&ext.as_str()) || VIDEO_EXTS.contains(&ext.as_str());

        let (rel_path, error) = if !supported {
            (
                None,
                Some("Unsupported file type — audio or video only".to_string()),
            )
        } else {
            let copy = rec_dir.join(copy_name(&rec_dir, &file_name));
            match std::fs::copy(src, &copy) {
                Ok(_) => {
                    let rel = copy
                        .strip_prefix(&state.data_dir)
                        .map_err(|e| e.to_string())?
                        .to_string_lossy()
                        .replace('\\', "/");
                    (Some(rel), None)
                }
                Err(e) => (None, Some(format!("Couldn't copy file: {e}"))),
            }
        };
        files.push(ImportFile {
            id: fly_core::new_id(),
            file_name,
            size,
            kind: kind.to_string(),
            rel_path,
            error,
        });
    }

    let staged = ImportStaged {
        note_id,
        meeting_id,
        files,
        boundaries_ms: Vec::new(),
        started: false,
    };
    save_staged(&state, &staged)?;
    Ok(Some(staged))
}

/// The staged import for a meeting, if any. Lazily cleans up: once the
/// meeting has a transcript the queue is over, so the manifest is deleted
/// and None is returned (the note renders as a normal note).
#[tauri::command]
pub fn import_state(
    state: State<'_, AppState>,
    meeting_id: String,
) -> CmdResult<Option<ImportStaged>> {
    let Some(staged) = load_staged(&state, &meeting_id)? else {
        return Ok(None);
    };
    let done = state
        .storage
        .lock()
        .unwrap()
        .get_transcript(&meeting_id)
        .map_err(|e| e.to_string())?
        .is_some();
    if done {
        state
            .storage
            .lock()
            .unwrap()
            .delete_setting(&manifest_key(&meeting_id))
            .map_err(|e| e.to_string())?;
        return Ok(None);
    }
    Ok(Some(staged))
}

/// Concatenate 16 kHz mono WAVs into `dst` with `gap_ms` of silence between
/// files. Returns the total duration and the cumulative end (ms) of each
/// source on the joined timeline. Tauri-free so tests can drive it.
fn concat_16k(sources: &[PathBuf], dst: &Path, gap_ms: u64) -> CmdResult<(u64, Vec<u64>)> {
    const RATE: u64 = 16_000;
    let mut samples: Vec<f32> = Vec::new();
    let mut boundaries = Vec::with_capacity(sources.len());
    for src in sources {
        if !samples.is_empty() {
            samples.extend(std::iter::repeat(0f32).take((gap_ms * RATE / 1000) as usize));
        }
        let (chunk, rate) = fly_audio::mix::read_wav_mono(src).map_err(|e| e.to_string())?;
        let chunk = fly_audio::mix::resample_linear(&chunk, rate, RATE as u32);
        samples.extend(chunk);
        boundaries.push(samples.len() as u64 * 1000 / RATE);
    }
    fly_audio::mix::write_wav_mono_16(dst, &samples, RATE as u32).map_err(|e| e.to_string())?;
    Ok((samples.len() as u64 * 1000 / RATE, boundaries))
}

/// Normalize the staged files in `order`, join them into the meeting's mixed
/// track, and queue the standard pipeline. Files omitted from `order`
/// (removed rows, unsupported files) are dropped and their copies deleted.
#[tauri::command]
pub async fn import_transcribe(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    order: Vec<String>,
) -> CmdResult<ImportStaged> {
    let Some(staged) = load_staged(&state, &meeting_id)? else {
        return Err("no staged import for this meeting".into());
    };
    if staged.started {
        return Ok(staged); // double-click / re-invoke guard
    }

    // apply the user's order; drop (and clean up) everything else
    let files: Vec<ImportFile> = order
        .iter()
        .filter_map(|id| staged.files.iter().find(|f| &f.id == id).cloned())
        .filter(|f| f.rel_path.is_some() && f.error.is_none())
        .collect();
    for f in &staged.files {
        let kept = files.iter().any(|k| k.id == f.id);
        if !kept {
            if let Some(rel) = &f.rel_path {
                let _ = std::fs::remove_file(state.data_dir.join(rel));
            }
        }
    }
    if files.is_empty() {
        return Err("nothing to transcribe — no supported files in the queue".into());
    }

    let rec_dir = state
        .data_dir
        .join(files[0].rel_path.as_ref().unwrap())
        .parent()
        .map(Path::to_path_buf)
        .ok_or("staged file has no parent folder")?;

    let emit = |file_id: &str, stage: &str| {
        let _ = app.emit(
            "import:progress",
            ImportProgress {
                meeting_id: meeting_id.clone(),
                file_id: file_id.to_string(),
                stage: stage.to_string(),
            },
        );
    };

    // normalize each file to its own 16 kHz mono intermediate (pure Rust for
    // PCM wav; ffmpeg otherwise), in the user's order
    let mut intermediates: Vec<PathBuf> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        emit(&f.id, "converting");
        let src = state.data_dir.join(f.rel_path.as_ref().unwrap());
        let dst = rec_dir.join(format!("import-{i:02}.16k.wav"));
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let converted = if ext == "wav" {
            match fly_audio::mix::read_wav_mono(&src) {
                Ok((samples, rate)) => {
                    let resampled = fly_audio::mix::resample_linear(&samples, rate, 16_000);
                    fly_audio::mix::write_wav_mono_16(&dst, &resampled, 16_000)
                        .map_err(|e| e.to_string())?;
                    true
                }
                // exotic wav encodings (e.g. float64, ADPCM) fall through to ffmpeg
                Err(_) => false,
            }
        } else {
            false
        };
        if !converted {
            convert_with_ffmpeg(&app, &state, &src, &dst).await?;
        }
        intermediates.push(dst);
        emit(&f.id, "converted");
    }

    // join into the one mixed track the pipeline transcribes
    let mixed = rec_dir.join("recording.mixed.wav");
    let (duration_ms, boundaries_ms) = concat_16k(&intermediates, &mixed, GAP_MS)?;
    for p in &intermediates {
        let _ = std::fs::remove_file(p);
    }

    let mixed_rel = mixed
        .strip_prefix(&state.data_dir)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    {
        let storage = state.storage.lock().unwrap();
        storage
            .end_meeting(
                &meeting_id,
                &RecordingRef {
                    mic_path: None,
                    system_path: None,
                    mixed_path: Some(mixed_rel),
                    playback_path: None,
                    duration_ms,
                },
            )
            .map_err(|e| e.to_string())?;
        // imported videos double as note attachments so they play in-app
        for f in files.iter().filter(|f| f.kind == "video") {
            if let Some(rel) = &f.rel_path {
                if let Err(e) = storage.add_attachment_in_place(&staged.note_id, rel) {
                    tracing::warn!(error = %e, rel, "attaching imported video failed");
                }
            }
        }
    }

    let staged = ImportStaged {
        files,
        boundaries_ms,
        started: true,
        ..staged
    };
    save_staged(&state, &staged)?;

    // same pipeline as a live recording (single-track: diarize whole file),
    // queued so an active recording is never contended with
    scheduler::enqueue(&state, &scheduler::stage_emitter(&app), &meeting_id)?;
    Ok(staged)
}

async fn convert_with_ffmpeg(
    app: &tauri::AppHandle,
    state: &AppState,
    src: &std::path::Path,
    dst: &std::path::Path,
) -> CmdResult<()> {
    let on_model = {
        let app = app.clone();
        move |p: models::ModelProgress| {
            let _ = app.emit("model:progress", p);
        }
    };
    let ffmpeg = models::ensure_tool(
        &on_model,
        &state.data_dir,
        "ffmpeg",
        &["ffmpeg"],
        "install ffmpeg (macOS: brew install ffmpeg)",
    )
    .await?;

    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.arg("-i")
        .arg(src)
        .args(["-ac", "1", "-ar", "16000", "-vn", "-y"])
        .arg(dst);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "ffmpeg conversion failed: {}",
            stderr.chars().take(400).collect::<String>()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_16k_wav(path: &Path, secs: f32) {
        let n = (secs * 16_000.0) as usize;
        let samples: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.4).collect();
        fly_audio::mix::write_wav_mono_16(path, &samples, 16_000).unwrap();
    }

    #[test]
    fn concat_places_boundaries_after_each_file_with_gaps_between() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.wav");
        let b = dir.path().join("b.wav");
        let c = dir.path().join("c.wav");
        write_16k_wav(&a, 2.0);
        write_16k_wav(&b, 1.0);
        write_16k_wav(&c, 3.0);
        let dst = dir.path().join("mixed.wav");

        let (total, bounds) = concat_16k(&[a, b, c], &dst, 1000).unwrap();
        // 2s + gap + 1s + gap + 3s = 8s; boundaries at each file's end
        assert_eq!(bounds, vec![2000, 4000, 8000]);
        assert_eq!(total, 8000);
        let (samples, rate) = fly_audio::mix::read_wav_mono(&dst).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(samples.len(), 8 * 16_000);
    }

    #[test]
    fn concat_single_file_has_no_gap() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.wav");
        write_16k_wav(&a, 2.0);
        let dst = dir.path().join("mixed.wav");
        let (total, bounds) = concat_16k(&[a], &dst, 1000).unwrap();
        assert_eq!(total, 2000);
        assert_eq!(bounds, vec![2000]);
    }

    #[test]
    fn copy_name_sanitizes_and_uniquifies() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            copy_name(dir.path(), "Team sync: Q3.mp3"),
            "Team sync_ Q3.mp3"
        );
        std::fs::write(dir.path().join("a.mp3"), b"x").unwrap();
        assert_eq!(copy_name(dir.path(), "a.mp3"), "a (2).mp3");
    }
}
