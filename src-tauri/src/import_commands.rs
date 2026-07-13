//! File import (M8): pick an audio/video file, normalize it to the 16 kHz
//! mono track the pipeline expects, and run the same transcribe → diarize →
//! (enhanceable) flow as a live recording.

use fly_core::{Meeting, RecordingRef};
use serde::Serialize;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::state::AppState;
use crate::{models, scheduler};

type CmdResult<T> = Result<T, String>;

#[derive(Serialize)]
pub struct ImportResult {
    pub meeting: Meeting,
    pub note_id: String,
}

/// Pick a media file and import it. Returns None if the user cancels.
/// Transcription starts automatically in the background.
#[tauri::command]
pub async fn import_media(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Option<ImportResult>> {
    let Some(picked) = app
        .dialog()
        .file()
        .add_filter(
            "Audio / video",
            &[
                "wav", "mp3", "m4a", "aac", "flac", "ogg", "mp4", "mkv", "mov", "webm",
            ],
        )
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let src = picked.into_path().map_err(|e| e.to_string())?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported recording")
        .to_string();

    // note + meeting first, so everything lands in the meeting's folder
    let (note_id, meeting_id, rec_dir) = {
        let storage = state.storage.lock().unwrap();
        let note = storage
            .create_note(&stem, None)
            .map_err(|e| e.to_string())?;
        let meeting = storage
            .create_meeting(&stem, &note.id, &[])
            .map_err(|e| e.to_string())?;
        let rec_dir = storage
            .allocate_meeting_dir(&stem, meeting.started_at)
            .map_err(|e| e.to_string())?;
        (note.id, meeting.id, rec_dir)
    };

    // keep the original next to the derived track
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_ascii_lowercase();
    let source_copy = rec_dir.join(format!("source.{ext}"));
    std::fs::copy(&src, &source_copy).map_err(|e| e.to_string())?;

    // normalize to 16 kHz mono WAV (pure Rust for PCM wav; ffmpeg otherwise)
    let mixed = rec_dir.join("recording.mixed.wav");
    let duration_ms = if ext == "wav" {
        match fly_audio::mix::read_wav_mono(&source_copy) {
            Ok((samples, rate)) => {
                let resampled = fly_audio::mix::resample_linear(&samples, rate, 16_000);
                fly_audio::mix::write_wav_mono_16(&mixed, &resampled, 16_000)
                    .map_err(|e| e.to_string())?;
                (resampled.len() as u64) * 1000 / 16_000
            }
            // exotic wav encodings (e.g. float64, ADPCM) fall through to ffmpeg
            Err(_) => convert_with_ffmpeg(&app, &state, &source_copy, &mixed).await?,
        }
    } else {
        convert_with_ffmpeg(&app, &state, &source_copy, &mixed).await?
    };

    let mixed_rel = mixed
        .strip_prefix(&state.data_dir)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    let meeting = {
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
            .map_err(|e| e.to_string())?
    };

    // same pipeline as a live recording (single-track: diarize whole file),
    // queued so an active recording is never contended with
    scheduler::enqueue(&state, &scheduler::stage_emitter(&app), &meeting_id)?;

    Ok(Some(ImportResult { meeting, note_id }))
}

async fn convert_with_ffmpeg(
    app: &tauri::AppHandle,
    state: &AppState,
    src: &std::path::Path,
    dst: &std::path::Path,
) -> CmdResult<u64> {
    let on_model = {
        let app = app.clone();
        move |p: models::ModelProgress| {
            use tauri::Emitter;
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
    let (samples, rate) = fly_audio::mix::read_wav_mono(dst).map_err(|e| e.to_string())?;
    Ok((samples.len() as u64) * 1000 / rate.max(1) as u64)
}
