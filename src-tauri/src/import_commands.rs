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
    // fly-storage keeps a meeting folder's name while this key exists.
    format!("{}{meeting_id}", fly_storage::IMPORT_STAGED_PREFIX)
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
    /// Why this file's audio couldn't be converted on the last Transcribe.
    /// Unlike `error`, the file stays staged so Retry can try it again.
    #[serde(default)]
    pub convert_error: Option<String>,
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
    /// "converting" | "converted" | "failed"
    stage: String,
    /// With "failed": this file's cause. The rest of the batch carries on.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// "Not an audio or video file…", naming exactly what the file picker
/// accepts so the copy can't drift from the list (spec §3.6: the old copy
/// listed MOV as supported and then called it "not supported").
fn unsupported_file_message() -> String {
    let formats: Vec<String> = AUDIO_EXTS
        .iter()
        .chain(VIDEO_EXTS)
        .map(|e| match *e {
            "webm" => "WebM".to_string(),
            other => other.to_ascii_uppercase(),
        })
        .collect();
    format!(
        "Not an audio or video file this app imports ({}).",
        formats.join(", ")
    )
}

/// Where a staged file's 16 kHz intermediate lives in the meeting's scratch
/// dir. Keyed by file id, so a conversion finished on an earlier run is found
/// again whatever the order.
fn intermediate_path(scratch: &Path, file_id: &str) -> PathBuf {
    scratch.join(format!("import-{file_id}.16k.wav"))
}

/// Why ffmpeg couldn't convert a file.
#[derive(Debug)]
enum FfmpegError {
    /// No usable ffmpeg (it couldn't be fetched or started). Every file would
    /// fail the same way, so this fails the whole batch.
    Unavailable(String),
    /// ffmpeg ran and rejected this file: its stderr.
    Failed(String),
}

/// The ffmpeg step, behind a trait so tests can stand in for the sidecar.
trait Ffmpeg {
    fn convert(
        &mut self,
        src: &Path,
        dst: &Path,
    ) -> impl std::future::Future<Output = Result<(), FfmpegError>> + Send;
}

/// What one file's conversion came to, when it didn't work.
enum ConvertFailure {
    /// Nothing else in the batch can get past this either (no ffmpeg, an
    /// unwritable folder).
    Batch(String),
    /// This file only: its cause, in words for the import queue.
    File(String),
}

const MISSING_COPY: &str =
    "The imported copy of this file is missing. Remove it and import it again.";

/// ffmpeg's stderr → one sentence about this file. Never says the format is
/// "not supported": every extension in the picker is one ffmpeg reads, so a
/// failure is about this file's contents.
fn describe_ffmpeg_failure(stderr: &str) -> String {
    let s = stderr.to_ascii_lowercase();
    if s.contains("does not contain any stream") || s.contains("matches no streams") {
        "This file has no audio track.".into()
    } else if s.contains("moov atom not found")
        || s.contains("invalid data found")
        || s.contains("could not find codec parameters")
        || s.contains("end of file")
    {
        "This file looks damaged or incomplete, so its audio can't be read.".into()
    } else if (s.contains("decoder") && s.contains("not found"))
        || s.contains("unknown decoder")
        || s.contains("unsupported codec")
    {
        "Its audio uses a codec this app can't decode. Export it again as MP4 or M4A and import that file.".into()
    } else {
        let last = stderr
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty() && *l != "Conversion failed!");
        match last {
            Some(l) => format!(
                "Its audio couldn't be converted (ffmpeg: {}).",
                l.chars().take(160).collect::<String>()
            ),
            None => "Its audio couldn't be converted.".into(),
        }
    }
}

/// Convert one staged file to a 16 kHz mono WAV at `dst`: pure Rust for PCM
/// WAV, ffmpeg otherwise.
async fn convert_one(
    src: &Path,
    dst: &Path,
    ffmpeg: &mut impl Ffmpeg,
) -> Result<(), ConvertFailure> {
    if !src.exists() {
        return Err(ConvertFailure::File(MISSING_COPY.into()));
    }
    let is_wav = src
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"));
    if is_wav {
        // exotic wav encodings (e.g. float64, ADPCM) fall through to ffmpeg
        if let Ok((samples, rate)) = fly_audio::mix::read_wav_mono(src) {
            let resampled = fly_audio::mix::resample_linear(&samples, rate, 16_000);
            return fly_audio::mix::write_wav_mono_16(dst, &resampled, 16_000).map_err(|e| {
                ConvertFailure::Batch(format!("Couldn't write converted audio: {e}"))
            });
        }
    }
    match ffmpeg.convert(src, dst).await {
        Ok(()) => Ok(()),
        Err(FfmpegError::Unavailable(e)) => Err(ConvertFailure::Batch(e)),
        Err(FfmpegError::Failed(stderr)) => {
            let tail: String = {
                let chars: Vec<char> = stderr.chars().collect();
                chars[chars.len().saturating_sub(400)..].iter().collect()
            };
            tracing::warn!(stderr = %tail, "import: ffmpeg couldn't convert a file");
            Err(ConvertFailure::File(describe_ffmpeg_failure(&stderr)))
        }
    }
}

/// Normalize each staged file, in order, to its own 16 kHz mono intermediate
/// in `scratch` (the meeting's scratch dir, never the meeting folder). A file
/// that can't be converted, or whose conversion can't be saved, gets its
/// cause in `convert_error` and the others carry on (spec §3.6); a
/// conversion finished on an earlier run is reused, so Retry only redoes the
/// failures. Returns the ready intermediates in order. Fails the whole batch
/// only for what no file could get past: no ffmpeg, or a folder it can't
/// write to. Tauri-free so tests can drive it with a synthetic batch.
async fn normalize_files(
    files: &mut [ImportFile],
    data_dir: &Path,
    scratch: &Path,
    ffmpeg: &mut impl Ffmpeg,
    emit: &mut impl FnMut(&str, &str, Option<&str>),
) -> CmdResult<Vec<PathBuf>> {
    let mut ready = Vec::with_capacity(files.len());
    for f in files.iter_mut() {
        f.convert_error = None;
        emit(&f.id, "converting", None);
        let dst = intermediate_path(scratch, &f.id);
        if !dst.is_file() {
            let src = f
                .rel_path
                .as_ref()
                .map(|rel| data_dir.join(rel))
                .unwrap_or_default();
            // written under a temporary name and renamed when complete, so a
            // half-written file is never mistaken for a finished one
            let part = scratch.join(format!("import-{}.part.wav", f.id));
            let outcome = match convert_one(&src, &part, ffmpeg).await {
                // a fresh file is often held for a moment by a virus scanner
                // or the indexer: wait that out, and if it never clears it's
                // still only this file's problem
                Ok(()) => fly_storage::rename_with_retry(&part, &dst).map_err(|e| {
                    tracing::warn!(error = %e, "import: couldn't save a converted file");
                    ConvertFailure::File(format!(
                        "Couldn't save its converted audio ({e}). Retry in a moment."
                    ))
                }),
                Err(failure) => Err(failure),
            };
            if let Err(failure) = outcome {
                let _ = std::fs::remove_file(&part);
                match failure {
                    ConvertFailure::Batch(e) => return Err(e),
                    ConvertFailure::File(cause) => {
                        emit(&f.id, "failed", Some(&cause));
                        f.convert_error = Some(cause);
                        continue;
                    }
                }
            }
        }
        emit(&f.id, "converted", None);
        ready.push(dst);
    }
    Ok(ready)
}

/// The real ffmpeg: the bundled sidecar (fetched on first use) or one on PATH.
struct SidecarFfmpeg<'a> {
    app: &'a tauri::AppHandle,
    data_dir: &'a Path,
    exe: Option<PathBuf>,
}

impl Ffmpeg for SidecarFfmpeg<'_> {
    async fn convert(&mut self, src: &Path, dst: &Path) -> Result<(), FfmpegError> {
        let exe = match &self.exe {
            Some(exe) => exe.clone(),
            None => {
                let on_model = {
                    let app = self.app.clone();
                    move |p: models::ModelProgress| {
                        let _ = app.emit("model:progress", p);
                    }
                };
                let exe = models::ensure_tool(
                    &on_model,
                    self.data_dir,
                    "ffmpeg",
                    &["ffmpeg"],
                    "install ffmpeg (macOS: brew install ffmpeg)",
                )
                .await
                .map_err(FfmpegError::Unavailable)?;
                self.exe = Some(exe.clone());
                exe
            }
        };
        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("-i")
            .arg(src)
            .args(["-hide_banner", "-ac", "1", "-ar", "16000", "-vn", "-y"])
            .arg(dst);
        #[cfg(windows)]
        {
            cmd.creation_flags(0x0800_0000);
        }
        let out = cmd
            .output()
            .await
            .map_err(|e| FfmpegError::Unavailable(format!("Couldn't run ffmpeg: {e}")))?;
        if !out.status.success() {
            return Err(FfmpegError::Failed(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ));
        }
        Ok(())
    }
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

/// Un-stick a staged queue after a cancelled run so the user can reorder
/// and hit Transcribe again (identical re-runs resume from the ASR
/// checkpoints; a reorder changes the plan key and starts clean).
pub(crate) fn reset_staged_started(state: &AppState, meeting_id: &str) {
    if let Ok(Some(mut staged)) = load_staged(state, meeting_id) {
        if staged.started {
            staged.started = false;
            let _ = save_staged(state, &staged);
        }
    }
}

/// The import queue's Cancel (`cancel_transcription`), tauri-free. The queue
/// drops back to idle, then whatever is in flight stops: a running pipeline
/// unwinds at its next batch boundary and the worker emits "cancelled"; a
/// queued job is removed and "cancelled" is emitted on the spot. With
/// nothing queued or running (a queue left started with no job, e.g. an
/// enqueue that never happened) "cancelled" is emitted here, so the queue
/// never waits on an event that can't come.
///
/// Dropping a parked failure is defensive: the queue hides Cancel once a run
/// has failed (it offers Retry), but if Cancel arrives anyway, the failure
/// goes with the run it belongs to, so the list stops saying "Failed".
pub(crate) fn cancel_staged_transcription(
    state: &AppState,
    on_stage: crate::pipeline::StageSink<'_>,
    meeting_id: &str,
) -> CmdResult<()> {
    reset_staged_started(state, meeting_id);
    if scheduler::cancel(state, on_stage, meeting_id)? {
        return Ok(());
    }
    {
        // checked and deleted under one lock: only a job still parked as failed
        let storage = state.storage.lock().unwrap();
        let parked = storage
            .transcription_job(meeting_id)
            .map_err(|e| e.to_string())?
            .is_some_and(|j| j.status == fly_storage::JOB_FAILED);
        if parked {
            storage
                .delete_transcription_job(meeting_id)
                .map_err(|e| e.to_string())?;
        }
    }
    scheduler::emit_cancelled(on_stage, meeting_id);
    Ok(())
}

/// Delete a meeting's staged-import residue: the manifest row and — for an
/// import that never transcribed (no recording_json yet) — the folder of
/// staged copies. Safe no-op for meetings that were never imports.
pub(crate) fn purge_staged_import(state: &AppState, meeting_id: &str) {
    if let Ok(Some(staged)) = load_staged(state, meeting_id) {
        if let Some(rel) = staged.files.iter().find_map(|f| f.rel_path.clone()) {
            // Only ever the staged-copies folder under recordings/: the
            // manifest lives in a settings row, so don't trust it blindly.
            if let Some((dir, _)) = rel.rsplit_once('/') {
                if let Some(dir) = fly_storage::contained_recordings_dir(&state.data_dir, dir) {
                    let _ = std::fs::remove_dir_all(dir);
                }
            }
        }
    }
    let _ = state
        .storage
        .lock()
        .unwrap()
        .delete_setting(&manifest_key(meeting_id));
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
    // "Delete all my data" refuses to run while files are being imported
    let _busy = crate::privacy::ImportGuard::enter();
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
            (None, Some(unsupported_file_message()))
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
            convert_error: None,
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

/// After the join: point the meeting at its mixed track, and list the
/// imported videos as the note's attachments, in place in the meeting folder,
/// so they play in-app. A video that can't be listed is logged, not fatal.
fn record_joined_import(
    storage: &fly_storage::Storage,
    staged: &ImportStaged,
    files: &[ImportFile],
    mixed_rel: String,
    duration_ms: u64,
) -> CmdResult<()> {
    storage
        .end_meeting(
            &staged.meeting_id,
            &RecordingRef {
                mic_path: None,
                system_path: None,
                mixed_path: Some(mixed_rel),
                playback_path: None,
                duration_ms,
            },
        )
        .map_err(|e| e.to_string())?;
    for f in files.iter().filter(|f| f.kind == "video") {
        if let Some(rel) = &f.rel_path {
            if let Err(e) = storage.add_attachment_in_place(&staged.note_id, rel) {
                tracing::warn!(error = %e, note_id = %staged.note_id, "attaching imported video failed");
            }
        }
    }
    Ok(())
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
            samples.extend(std::iter::repeat_n(0f32, (gap_ms * RATE / 1000) as usize));
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
/// (removed rows, unsupported files) leave the queue, and their copies are
/// deleted once the new queue is saved. If any file can't be converted, the
/// others still convert, and the queue comes back idle with each cause on
/// its own file instead of transcribing.
#[tauri::command]
pub async fn import_transcribe(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    order: Vec<String>,
) -> CmdResult<ImportStaged> {
    let _busy = crate::privacy::ImportGuard::enter();
    let Some(staged) = load_staged(&state, &meeting_id)? else {
        return Err("No staged import for this meeting".into());
    };
    if staged.started {
        return Ok(staged); // double-click / re-invoke guard
    }

    // apply the user's order. A file whose conversion failed last time is
    // kept when it's in the order (Retry) and dropped like any other row
    // when it isn't.
    let mut files: Vec<ImportFile> = order
        .iter()
        .filter_map(|id| staged.files.iter().find(|f| &f.id == id).cloned())
        .filter(|f| f.rel_path.is_some() && f.error.is_none())
        .collect();
    // Everything else leaves the queue. Its copies are deleted only after
    // the new queue is saved, so a run that stops before then loses nothing.
    let dropped: Vec<ImportFile> = staged
        .files
        .iter()
        .filter(|f| !files.iter().any(|k| k.id == f.id))
        .cloned()
        .collect();
    if files.is_empty() {
        return Err("Nothing to transcribe: no supported files in the queue".into());
    }

    let rec_dir = state
        .data_dir
        .join(files[0].rel_path.as_ref().unwrap())
        .parent()
        .map(Path::to_path_buf)
        .ok_or("Staged file has no parent folder")?;
    // conversions go to the meeting's scratch dir (machine-local, keyed by
    // meeting id), never the meeting folder: the startup sweep keeps it while
    // the import is unfinished, and deleting the meeting removes it
    let scratch = fly_storage::scratch::prepare_meeting_scratch(&state.data_dir, &meeting_id, None)
        .map_err(|e| format!("Couldn't prepare a folder for the converted audio: {e}"))?;

    let mut emit = |file_id: &str, stage: &str, error: Option<&str>| {
        let _ = app.emit(
            "import:progress",
            ImportProgress {
                meeting_id: meeting_id.clone(),
                file_id: file_id.to_string(),
                stage: stage.to_string(),
                error: error.map(str::to_string),
            },
        );
    };

    // normalize each file to its own 16 kHz mono intermediate, in the
    // user's order; a file that fails is attributed and the rest carry on
    let mut ffmpeg = SidecarFfmpeg {
        app: &app,
        data_dir: &state.data_dir,
        exe: None,
    };
    let intermediates = normalize_files(
        &mut files,
        &state.data_dir,
        &scratch,
        &mut ffmpeg,
        &mut emit,
    )
    .await?;

    // Deleting the note while its files converted purges the manifest and
    // the staged copies: never write the queue back for a meeting that's gone.
    if load_staged(&state, &meeting_id)?.is_none() {
        let _ = std::fs::remove_dir_all(&scratch);
        return Err("This import was deleted while its files were converting".into());
    }

    // Stop before transcribing if any file failed: the batch becomes ONE
    // transcript, so a file skipped now couldn't be added to it later. The
    // queue goes back to idle with each cause on its file; finished
    // conversions stay in scratch, so Retry (or Transcribe without the
    // failed file) doesn't redo them.
    if files.iter().any(|f| f.convert_error.is_some()) {
        let paused = ImportStaged {
            files,
            boundaries_ms: Vec::new(),
            started: false,
            ..staged
        };
        save_staged(&state, &paused)?;
        drop_staged_files(&state.data_dir, &scratch, &dropped);
        return Ok(paused);
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
    record_joined_import(
        &state.storage.lock().unwrap(),
        &staged,
        &files,
        mixed_rel,
        duration_ms,
    )?;

    let staged = ImportStaged {
        files,
        boundaries_ms,
        started: true,
        ..staged
    };
    save_staged(&state, &staged)?;
    drop_staged_files(&state.data_dir, &scratch, &dropped);

    // same pipeline as a live recording (single-track: diarize whole file),
    // queued so an active recording is never contended with
    scheduler::enqueue(&state, &scheduler::stage_emitter(&app), &meeting_id)?;
    Ok(staged)
}

/// Delete what files that left the queue had on disk: the staged copy and
/// any finished conversion. Only after the queue without them is saved.
fn drop_staged_files(data_dir: &Path, scratch: &Path, dropped: &[ImportFile]) {
    for f in dropped {
        let _ = std::fs::remove_file(intermediate_path(scratch, &f.id));
        if let Some(rel) = &f.rel_path {
            let _ = std::fs::remove_file(data_dir.join(rel));
        }
    }
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

    /// Stand-in for the ffmpeg sidecar. A file whose name is in `fail_for`
    /// fails the way a real ffmpeg does: it starts writing the output, then
    /// exits with `stderr`. Any other file gets a short valid 16 kHz WAV.
    struct FakeFfmpeg {
        fail_for: Vec<&'static str>,
        stderr: &'static str,
        unavailable: bool,
        calls: Vec<String>,
    }

    impl FakeFfmpeg {
        fn failing_for(names: &[&'static str], stderr: &'static str) -> Self {
            Self {
                fail_for: names.to_vec(),
                stderr,
                unavailable: false,
                calls: Vec::new(),
            }
        }
        fn working() -> Self {
            Self::failing_for(&[], "")
        }
    }

    impl Ffmpeg for FakeFfmpeg {
        async fn convert(&mut self, src: &Path, dst: &Path) -> Result<(), FfmpegError> {
            let name = src.file_name().unwrap().to_string_lossy().into_owned();
            self.calls.push(name.clone());
            if self.unavailable {
                return Err(FfmpegError::Unavailable("ffmpeg isn't installed".into()));
            }
            if self.fail_for.contains(&name.as_str()) {
                std::fs::write(dst, b"RIFF....WAVEfmt half-written").unwrap();
                return Err(FfmpegError::Failed(self.stderr.to_string()));
            }
            write_16k_wav(dst, 1.0);
            Ok(())
        }
    }

    /// A staged batch in a temp data dir: real WAVs plus synthetic bad files
    /// (garbage bytes). No real media. Conversions go to the meeting's
    /// scratch dir, as in the app.
    struct Batch {
        dir: tempfile::TempDir,
        rec_dir: PathBuf,
        scratch: PathBuf,
        files: Vec<ImportFile>,
    }

    fn batch(entries: &[(&str, &str, Option<f32>)]) -> Batch {
        let dir = tempfile::tempdir().unwrap();
        let rel_dir = "recordings/2026/2026-10/imported";
        let rec_dir = dir.path().join(rel_dir);
        std::fs::create_dir_all(&rec_dir).unwrap();
        let scratch =
            fly_storage::scratch::prepare_meeting_scratch(dir.path(), "m-import", None).unwrap();
        let files = entries
            .iter()
            .map(|(id, name, secs)| {
                let path = rec_dir.join(name);
                match secs {
                    // a real recording at 48 kHz, so the WAV path resamples
                    Some(s) => {
                        let n = (s * 48_000.0) as usize;
                        let samples: Vec<f32> = (0..n).map(|i| (i as f32 * 0.01).sin()).collect();
                        fly_audio::mix::write_wav_mono_16(&path, &samples, 48_000).unwrap();
                    }
                    None => std::fs::write(&path, b"not media, just bytes").unwrap(),
                }
                ImportFile {
                    id: id.to_string(),
                    file_name: name.to_string(),
                    size: 0,
                    kind: if name.ends_with(".mov") {
                        "video"
                    } else {
                        "audio"
                    }
                    .to_string(),
                    rel_path: Some(format!("{rel_dir}/{name}")),
                    error: None,
                    convert_error: None,
                }
            })
            .collect();
        Batch {
            dir,
            rec_dir,
            scratch,
            files,
        }
    }

    fn part_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".part"))
            .collect()
    }

    type Events = Vec<(String, String, Option<String>)>;

    fn recorder(events: &mut Events) -> impl FnMut(&str, &str, Option<&str>) + '_ {
        |id, stage, err| events.push((id.into(), stage.into(), err.map(str::to_string)))
    }

    const MOOV_MISSING: &str = "[mov,mp4,m4a,3gp,3g2,mj2 @ 0x1] moov atom not found\n\
         clip.mov: Invalid data found when processing input\n";

    #[tokio::test]
    async fn a_bad_file_is_attributed_to_itself_and_the_rest_still_convert() {
        let mut b = batch(&[
            ("a", "intro.wav", Some(1.0)),
            ("b", "clip.mov", None),
            ("c", "outro.wav", Some(2.0)),
            ("d", "good.mov", None),
        ]);
        let mut ffmpeg = FakeFfmpeg::failing_for(&["clip.mov"], MOOV_MISSING);
        let mut events = Events::new();
        let ready = normalize_files(
            &mut b.files,
            b.dir.path(),
            &b.scratch,
            &mut ffmpeg,
            &mut recorder(&mut events),
        )
        .await
        .expect("one bad file must not fail the batch");

        // the good files converted, in order, to 16 kHz
        assert_eq!(
            ready,
            ["a", "c", "d"].map(|id| intermediate_path(&b.scratch, id))
        );
        for p in &ready {
            let (_, rate) = fly_audio::mix::read_wav_mono(p).unwrap();
            assert_eq!(rate, 16_000);
        }
        // the bad one carries its own cause; the others carry none
        let cause = b.files[1]
            .convert_error
            .clone()
            .expect("cause on the bad file");
        assert!(cause.contains("damaged or incomplete"), "{cause}");
        // a listed format never reads as "not supported"
        assert!(!cause.to_lowercase().contains("not supported"), "{cause}");
        for i in [0, 2, 3] {
            assert_eq!(b.files[i].convert_error, None);
        }
        // the bad file stays staged so Retry can try it again
        assert!(b.files[1].rel_path.is_some());
        assert!(b.files[1].error.is_none());
        // only the non-WAVs went to ffmpeg, and the bad one's half-written
        // output is gone, not mistaken for a finished conversion
        assert_eq!(ffmpeg.calls, vec!["clip.mov", "good.mov"]);
        assert!(!intermediate_path(&b.scratch, "b").exists());
        assert!(
            part_files(&b.scratch).is_empty(),
            "{:?}",
            part_files(&b.scratch)
        );
        // nothing derived lands in the meeting folder
        assert!(part_files(&b.rec_dir).is_empty());
        assert!(!intermediate_path(&b.rec_dir, "a").exists());
        // the queue hears about the failure as it happens
        assert!(events.contains(&("b".into(), "failed".into(), Some(cause.clone()))));
        assert!(events.contains(&("c".into(), "converted".into(), None)));
    }

    #[tokio::test]
    async fn a_garbage_wav_falls_back_to_ffmpeg_and_is_attributed() {
        let mut b = batch(&[("a", "broken.wav", None), ("b", "fine.wav", Some(1.0))]);
        let mut ffmpeg = FakeFfmpeg::failing_for(
            &["broken.wav"],
            "broken.wav: Invalid data found when processing input\nConversion failed!\n",
        );
        let mut events = Events::new();
        let ready = normalize_files(
            &mut b.files,
            b.dir.path(),
            &b.scratch,
            &mut ffmpeg,
            &mut recorder(&mut events),
        )
        .await
        .unwrap();
        assert_eq!(ready, vec![intermediate_path(&b.scratch, "b")]);
        assert_eq!(ffmpeg.calls, vec!["broken.wav"]);
        assert!(b.files[0].convert_error.is_some());
        assert!(b.files[1].convert_error.is_none());
    }

    #[tokio::test]
    async fn no_ffmpeg_at_all_is_the_batchs_problem_not_a_files() {
        let mut b = batch(&[("a", "clip.mov", None), ("b", "fine.wav", Some(1.0))]);
        let mut ffmpeg = FakeFfmpeg::working();
        ffmpeg.unavailable = true;
        let mut events = Events::new();
        let err = normalize_files(
            &mut b.files,
            b.dir.path(),
            &b.scratch,
            &mut ffmpeg,
            &mut recorder(&mut events),
        )
        .await
        .unwrap_err();
        assert!(err.contains("ffmpeg"), "{err}");
        assert!(b.files.iter().all(|f| f.convert_error.is_none()));
    }

    #[tokio::test]
    async fn retry_reuses_finished_conversions_and_redoes_only_the_failure() {
        let mut b = batch(&[
            ("a", "good.mov", None),
            ("b", "clip.mov", None),
            ("c", "intro.wav", Some(1.0)),
        ]);
        let mut events = Events::new();
        let mut run1 = FakeFfmpeg::failing_for(&["clip.mov"], MOOV_MISSING);
        normalize_files(
            &mut b.files,
            b.dir.path(),
            &b.scratch,
            &mut run1,
            &mut recorder(&mut events),
        )
        .await
        .unwrap();
        assert_eq!(run1.calls, vec!["good.mov", "clip.mov"]);
        assert!(b.files[1].convert_error.is_some());

        // the file is fixed (say ffmpeg was updated): run 2 converts only it;
        // good.mov's finished conversion is reused, not redone
        let mut run2 = FakeFfmpeg::working();
        let ready = normalize_files(
            &mut b.files,
            b.dir.path(),
            &b.scratch,
            &mut run2,
            &mut recorder(&mut events),
        )
        .await
        .unwrap();
        assert_eq!(run2.calls, vec!["clip.mov"]);
        assert_eq!(
            ready,
            ["a", "b", "c"].map(|id| intermediate_path(&b.scratch, id))
        );
        assert!(b.files.iter().all(|f| f.convert_error.is_none()));
    }

    #[tokio::test]
    async fn a_conversion_that_cant_be_saved_is_that_files_failure() {
        let mut b = batch(&[("a", "good.mov", None), ("b", "intro.wav", Some(1.0))]);
        // something in the way of the finished name (a folder): the rename
        // fails for good, which must not fail the batch
        std::fs::create_dir_all(intermediate_path(&b.scratch, "a")).unwrap();
        let mut ffmpeg = FakeFfmpeg::working();
        let mut events = Events::new();
        let ready = normalize_files(
            &mut b.files,
            b.dir.path(),
            &b.scratch,
            &mut ffmpeg,
            &mut recorder(&mut events),
        )
        .await
        .expect("a file that can't be saved fails alone");
        assert_eq!(ready, vec![intermediate_path(&b.scratch, "b")]);
        let cause = b.files[0].convert_error.clone().expect("cause on a");
        assert!(cause.contains("Couldn't save"), "{cause}");
        assert!(b.files[1].convert_error.is_none());
        assert!(part_files(&b.scratch).is_empty());
    }

    #[tokio::test]
    async fn a_missing_staged_copy_is_attributed_without_running_ffmpeg() {
        let mut b = batch(&[("a", "clip.mov", None), ("b", "fine.wav", Some(1.0))]);
        std::fs::remove_file(b.rec_dir.join("clip.mov")).unwrap();
        let mut ffmpeg = FakeFfmpeg::working();
        let mut events = Events::new();
        let ready = normalize_files(
            &mut b.files,
            b.dir.path(),
            &b.scratch,
            &mut ffmpeg,
            &mut recorder(&mut events),
        )
        .await
        .unwrap();
        assert_eq!(ready.len(), 1);
        assert!(ffmpeg.calls.is_empty());
        assert!(b.files[0]
            .convert_error
            .as_deref()
            .unwrap()
            .contains("missing"));
    }

    #[test]
    fn ffmpeg_failures_read_as_causes_not_as_unsupported_formats() {
        let no_audio = describe_ffmpeg_failure(
            "Output #0, wav, to 'x.wav':\nOutput file #0 does not contain any stream\n",
        );
        assert!(no_audio.contains("no audio track"), "{no_audio}");
        let no_audio_map = describe_ffmpeg_failure("Stream map '0:a' matches no streams.\n");
        assert!(no_audio_map.contains("no audio track"), "{no_audio_map}");
        let damaged = describe_ffmpeg_failure(MOOV_MISSING);
        assert!(damaged.contains("damaged or incomplete"), "{damaged}");
        let codec =
            describe_ffmpeg_failure("Decoder (codec none) not found for input stream #0:1\n");
        assert!(codec.contains("codec"), "{codec}");
        // unknown errors keep ffmpeg's own last line, minus the boilerplate
        let other = describe_ffmpeg_failure("something odd happened\nConversion failed!\n");
        assert!(other.contains("something odd happened"), "{other}");
        assert!(!other.contains("Conversion failed!"), "{other}");
        for cause in [no_audio, damaged, codec, other] {
            assert!(!cause.to_lowercase().contains("not supported"), "{cause}");
        }
    }

    #[test]
    fn the_unsupported_message_lists_the_formats_the_picker_accepts() {
        let msg = unsupported_file_message();
        for ext in AUDIO_EXTS.iter().chain(VIDEO_EXTS) {
            assert!(msg.to_lowercase().contains(ext), "{msg} should list {ext}");
        }
        assert!(msg.contains("MOV"));
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

    // ---- after Transcribe: a failed run, and Cancel with nothing running ----

    fn app_state(dir: &Path) -> AppState {
        AppState::init_with(
            dir.to_path_buf(),
            std::sync::Arc::new(fly_secrets::MemorySecretStore::default()),
        )
        .unwrap()
    }

    /// A staged import whose Transcribe already ran (`started`). Its joined
    /// recording is missing on disk, so the pipeline gives up on the first
    /// attempt (a permanent failure, never retried).
    fn started_import(state: &AppState) -> String {
        let (note_id, meeting_id) = {
            let storage = state.storage.lock().unwrap();
            let note = storage.create_note("Imported call", None).unwrap();
            let meeting = storage
                .create_meeting("Imported call", &note.id, &[])
                .unwrap();
            storage
                .end_meeting(
                    &meeting.id,
                    &RecordingRef {
                        mic_path: None,
                        system_path: None,
                        mixed_path: Some("recordings/gone/recording.mixed.wav".into()),
                        playback_path: None,
                        duration_ms: 1000,
                    },
                )
                .unwrap();
            (note.id, meeting.id)
        };
        let staged = ImportStaged {
            note_id,
            meeting_id: meeting_id.clone(),
            files: vec![ImportFile {
                id: "f1".into(),
                file_name: "call.wav".into(),
                size: 1,
                kind: "audio".into(),
                rel_path: Some("recordings/x/call.wav".into()),
                error: None,
                convert_error: None,
            }],
            boundaries_ms: vec![1000],
            started: true,
        };
        save_staged(state, &staged).unwrap();
        meeting_id
    }

    type Seen = std::sync::Mutex<Vec<(String, bool, Option<String>)>>;

    fn seen_by(seen: &Seen) -> impl Fn(crate::pipeline::PipelineProgress) + Send + Sync + '_ {
        move |p| seen.lock().unwrap().push((p.stage, p.done, p.error))
    }

    fn cancelled() -> (String, bool, Option<String>) {
        ("cancelled".into(), true, None)
    }

    /// A pipeline failure after Transcribe is the run's, not the queue's. The
    /// queue stays started, so Retry re-enqueues the joined recording (and
    /// resumes from its checkpoints). The cause stays readable after the live
    /// event is gone: on reopening the note, and after a restart.
    #[test]
    fn a_terminal_failure_leaves_the_staged_queue_retryable_with_its_cause() {
        let dir = tempfile::tempdir().unwrap();
        let state = app_state(dir.path());
        let meeting_id = started_import(&state);
        let quiet = |_: crate::pipeline::PipelineProgress| {};
        let model = |_: models::ModelProgress| {};
        scheduler::enqueue(&state, &quiet, &meeting_id).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tick = rt.block_on(scheduler::tick(&state, &quiet, &model));
        assert!(matches!(tick, scheduler::Tick::GaveUp { .. }));

        assert!(load_staged(&state, &meeting_id).unwrap().unwrap().started);
        let cause = scheduler::last_failure(&state, &meeting_id).expect("the failure is kept");
        assert!(
            cause.contains(crate::pipeline::ERR_NO_RECORDING_FILES),
            "{cause}"
        );

        drop(state);
        let state = app_state(dir.path());
        assert_eq!(
            scheduler::last_failure(&state, &meeting_id).as_deref(),
            Some(cause.as_str()),
            "a restart keeps it"
        );

        // Retry re-enqueues: the job is queued again and the failure clears
        scheduler::enqueue(&state, &quiet, &meeting_id).unwrap();
        assert_eq!(scheduler::last_failure(&state, &meeting_id), None);
    }

    /// An automatic retry is not a failure: while the job waits for its next
    /// attempt it's queued again (with the error noted), so nothing reads it
    /// as failed, and the queue keeps showing a run in progress.
    #[test]
    fn an_automatic_retry_never_reads_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let state = app_state(dir.path());
        let meeting_id = started_import(&state);
        let quiet = |_: crate::pipeline::PipelineProgress| {};
        scheduler::enqueue(&state, &quiet, &meeting_id).unwrap();
        state
            .storage
            .lock()
            .unwrap()
            .requeue_transcription(&meeting_id, 1, "boom")
            .unwrap();

        assert_eq!(scheduler::last_failure(&state, &meeting_id), None);
    }

    /// The stuck case: the queue says started but no job is queued or running
    /// (a failed run, or an enqueue that never happened). Cancel drops it to
    /// idle and emits the terminal "cancelled" the UI listens for.
    #[test]
    fn cancel_with_nothing_running_drops_the_queue_to_idle_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let state = app_state(dir.path());
        let meeting_id = started_import(&state);
        let seen = Seen::default();

        cancel_staged_transcription(&state, &seen_by(&seen), &meeting_id).unwrap();

        assert!(!load_staged(&state, &meeting_id).unwrap().unwrap().started);
        assert_eq!(*seen.lock().unwrap(), vec![cancelled()]);
    }

    /// Cancel after a run gave up abandons that run: the queue goes idle and
    /// the parked failure goes with it, so the list stops saying "Failed".
    #[test]
    fn cancel_after_a_failed_run_forgets_the_failure() {
        let dir = tempfile::tempdir().unwrap();
        let state = app_state(dir.path());
        let meeting_id = started_import(&state);
        let quiet = |_: crate::pipeline::PipelineProgress| {};
        let model = |_: models::ModelProgress| {};
        scheduler::enqueue(&state, &quiet, &meeting_id).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(scheduler::tick(&state, &quiet, &model));
        assert!(scheduler::last_failure(&state, &meeting_id).is_some());
        let seen = Seen::default();

        cancel_staged_transcription(&state, &seen_by(&seen), &meeting_id).unwrap();

        assert!(!load_staged(&state, &meeting_id).unwrap().unwrap().started);
        assert_eq!(scheduler::last_failure(&state, &meeting_id), None);
        assert_eq!(*seen.lock().unwrap(), vec![cancelled()]);
    }

    /// Unchanged: a queued job is removed on the spot with exactly one
    /// "cancelled" (the new no-job event must never double it).
    #[test]
    fn cancel_of_a_queued_job_emits_one_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let state = app_state(dir.path());
        let meeting_id = started_import(&state);
        let quiet = |_: crate::pipeline::PipelineProgress| {};
        scheduler::enqueue(&state, &quiet, &meeting_id).unwrap();
        let seen = Seen::default();

        cancel_staged_transcription(&state, &seen_by(&seen), &meeting_id).unwrap();

        let job = state
            .storage
            .lock()
            .unwrap()
            .transcription_job(&meeting_id)
            .unwrap();
        assert!(job.is_none());
        assert!(!load_staged(&state, &meeting_id).unwrap().unwrap().started);
        assert_eq!(*seen.lock().unwrap(), vec![cancelled()]);
    }

    /// Unchanged: a running pipeline gets a cooperative cancel request and
    /// emits nothing yet; the worker sends "cancelled" once it has unwound.
    #[test]
    fn cancel_of_a_running_job_waits_for_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let state = app_state(dir.path());
        let meeting_id = started_import(&state);
        state
            .pipeline_stage
            .lock()
            .unwrap()
            .insert(meeting_id.clone(), "transcribing".into());
        let seen = Seen::default();

        cancel_staged_transcription(&state, &seen_by(&seen), &meeting_id).unwrap();

        assert!(seen.lock().unwrap().is_empty());
        assert!(state.cancel_requests.lock().unwrap().contains(&meeting_id));
    }

    /// The join lists an imported video as the note's attachment, in place in
    /// the meeting folder (it plays in-app); audio files aren't listed.
    #[test]
    fn an_imported_video_is_listed_as_the_notes_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let state = app_state(dir.path());
        let storage = state.storage.lock().unwrap();
        // what import_stage leaves: note + meeting + copies in the meeting folder
        let note = storage.create_note("Design review", None).unwrap();
        let meeting = storage
            .create_meeting("Design review", &note.id, &[])
            .unwrap();
        let rec_dir = storage
            .allocate_meeting_dir("Design review", meeting.started_at)
            .unwrap();
        let folder = format!(
            "recordings/{}",
            rec_dir.file_name().unwrap().to_string_lossy()
        );
        std::fs::write(rec_dir.join("screen share.mp4"), b"ftyp").unwrap();
        std::fs::write(rec_dir.join("room mic.m4a"), b"ftyp").unwrap();
        std::fs::write(rec_dir.join("recording.mixed.wav"), b"RIFF").unwrap();
        let file = |id: &str, name: &str, kind: &str| ImportFile {
            id: id.into(),
            file_name: name.into(),
            size: 4,
            kind: kind.into(),
            rel_path: Some(format!("{folder}/{name}")),
            error: None,
            convert_error: None,
        };
        let files = vec![
            file("f1", "screen share.mp4", "video"),
            file("f2", "room mic.m4a", "audio"),
        ];
        let staged = ImportStaged {
            note_id: note.id.clone(),
            meeting_id: meeting.id.clone(),
            files: files.clone(),
            boundaries_ms: vec![],
            started: false,
        };

        record_joined_import(
            &storage,
            &staged,
            &files,
            format!("{folder}/recording.mixed.wav"),
            2_000,
        )
        .unwrap();

        let attachments = storage.get_note(&note.id).unwrap().attachments;
        assert_eq!(attachments.len(), 1, "{attachments:?}");
        assert_eq!(
            attachments[0].rel_path,
            format!("{folder}/screen share.mp4")
        );
        assert_eq!(attachments[0].mime.as_deref(), Some("video/mp4"));
        assert!(storage
            .attachment_abs_path(&attachments[0].rel_path)
            .unwrap()
            .is_file());
        let rec = storage.get_meeting(&meeting.id).unwrap().recording.unwrap();
        assert_eq!(rec.duration_ms, 2_000);
    }
}
