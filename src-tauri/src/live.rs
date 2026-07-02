//! Live partial transcript (beta): while a recording runs, periodically
//! transcribe the NEW audio appended to the per-channel WAVs and stream the
//! text to the UI as `live:segment` events. Channel-level attribution only
//! ("you" = mic, "them" = system loopback) — full diarization still happens
//! in the real pipeline after Stop, which replaces these partials entirely.
//!
//! Always uses the small (Light-tier) model regardless of the ASR tier: live
//! chunks must transcribe faster than they accumulate on laptop CPUs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use looma_asr::TranscriptionEngine;
use tauri::{Emitter, Manager};

use crate::models;
use crate::state::AppState;

const TICK_MS: u64 = 5_000;
/// Don't bother transcribing less than this much new audio…
const MIN_CHUNK_SECS: u64 = 8;
/// …and never take more than this in one bite.
const MAX_CHUNK_SECS: u64 = 30;
const LIVE_MODEL: &str = "ggml-small-q5_1";

#[derive(Clone, serde::Serialize)]
struct LiveSegment {
    meeting_id: String,
    channel: &'static str, // "you" | "them"
    text: String,
    start_ms: u64,
}

#[derive(Clone, serde::Serialize)]
struct LiveStatus {
    meeting_id: String,
    state: &'static str, // "ready" | "unavailable"
    detail: String,
}

struct ChannelCursor {
    path: PathBuf,
    channel: &'static str,
    consumed_samples: u64,
    prompt_tail: String,
}

/// Spawn the live loop for an active recording. Returns the stop flag the
/// recording holds; setting it ends the loop after the current tick.
pub fn spawn(app: tauri::AppHandle, meeting_id: String, out_dir: PathBuf) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    tauri::async_runtime::spawn(async move {
        run(app, meeting_id, out_dir, flag).await;
    });
    stop
}

async fn run(app: tauri::AppHandle, meeting_id: String, out_dir: PathBuf, stop: Arc<AtomicBool>) {
    let data_dir = app.state::<AppState>().data_dir.clone();

    // Live is opt-out via setting; also silently absent if whisper isn't
    // resolvable (the post-meeting pipeline will download/report properly).
    let enabled = {
        let state = app.state::<AppState>();
        let storage = state.storage.lock().unwrap();
        storage
            .get_setting("live.enabled")
            .ok()
            .flatten()
            .map(|v| v != "false")
            .unwrap_or(true)
    };
    if !enabled {
        return;
    }

    let on_model = {
        let app = app.clone();
        move |p: models::ModelProgress| {
            let _ = app.emit("model:progress", p);
        }
    };
    let exe = match models::ensure_tool(
        &on_model,
        &data_dir,
        "whisper-bin",
        &["whisper-cli"],
        "live transcript needs whisper.cpp",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            let _ = app.emit(
                "live:status",
                LiveStatus {
                    meeting_id,
                    state: "unavailable",
                    detail: e,
                },
            );
            return;
        }
    };
    let model = match models::ensure(&on_model, &data_dir, LIVE_MODEL).await {
        Ok(p) => p,
        Err(e) => {
            let _ = app.emit(
                "live:status",
                LiveStatus {
                    meeting_id,
                    state: "unavailable",
                    detail: e,
                },
            );
            return;
        }
    };
    let threads = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(2))
        .unwrap_or(2);
    let engine = looma_asr::whisper_cpp::WhisperCppEngine {
        exe,
        model,
        threads,
    };
    let _ = app.emit(
        "live:status",
        LiveStatus {
            meeting_id: meeting_id.clone(),
            state: "ready",
            detail: String::new(),
        },
    );

    let mut cursors = [
        ChannelCursor {
            path: out_dir.join("recording.mic.wav"),
            channel: "you",
            consumed_samples: 0,
            prompt_tail: String::new(),
        },
        ChannelCursor {
            path: out_dir.join("recording.system.wav"),
            channel: "them",
            consumed_samples: 0,
            prompt_tail: String::new(),
        },
    ];
    let tmp_dir = out_dir.join("live-tmp");
    let _ = std::fs::create_dir_all(&tmp_dir);

    while !stop.load(Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // Paused → nothing new is being written; skip cheaply.
        let paused = {
            let state = app.state::<AppState>();
            let rec = state.recording.lock().unwrap();
            match rec.as_ref() {
                Some(r) if r.meeting_id == meeting_id => {
                    r.session.state() == looma_audio::CaptureState::Paused
                }
                _ => break, // recording ended
            }
        };
        if paused {
            continue;
        }

        for cur in cursors.iter_mut() {
            match take_chunk(&cur.path, cur.consumed_samples) {
                Some((samples, rate, start_sample)) => {
                    let start_ms = start_sample * 1000 / rate as u64;
                    let resampled = looma_audio::mix::resample_linear(&samples, rate, 16_000);
                    let chunk_path = tmp_dir.join(format!("{}-{}.wav", cur.channel, start_sample));
                    if looma_audio::mix::write_wav_mono_16(&chunk_path, &resampled, 16_000).is_err()
                    {
                        continue;
                    }
                    let opts = looma_asr::TranscribeOptions {
                        language: None,
                        prompt: (!cur.prompt_tail.is_empty()).then(|| cur.prompt_tail.clone()),
                    };
                    let text = match engine.transcribe(&chunk_path, &opts).await {
                        Ok(raw) => raw
                            .words
                            .iter()
                            .map(|w| w.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" "),
                        Err(e) => {
                            tracing::debug!("live chunk transcription failed: {e}");
                            String::new()
                        }
                    };
                    let _ = std::fs::remove_file(&chunk_path);
                    cur.consumed_samples = start_sample + samples.len() as u64;
                    if !text.trim().is_empty() {
                        cur.prompt_tail = text
                            .split_whitespace()
                            .rev()
                            .take(24)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect::<Vec<_>>()
                            .join(" ");
                        let _ = app.emit(
                            "live:segment",
                            LiveSegment {
                                meeting_id: meeting_id.clone(),
                                channel: cur.channel,
                                text: text.trim().to_string(),
                                start_ms,
                            },
                        );
                    }
                }
                None => continue,
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

/// Read the samples appended past `consumed` from a mono 16-bit WAV that is
/// still being written (header sizes are stale until finalize — go by file
/// length; our own writer emits the canonical 44-byte header). Returns
/// `(samples, rate, start_sample)` only when at least MIN_CHUNK_SECS of new
/// audio exist; bounded by MAX_CHUNK_SECS.
fn take_chunk(path: &Path, consumed: u64) -> Option<(Vec<f32>, u32, u64)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 44 {
        return None;
    }
    let rate = u32::from_le_bytes(bytes[24..28].try_into().ok()?);
    if rate == 0 {
        return None;
    }
    let total_samples = ((bytes.len() - 44) / 2) as u64;
    let new = total_samples.saturating_sub(consumed);
    if new < MIN_CHUNK_SECS * rate as u64 {
        return None;
    }
    let take = new.min(MAX_CHUNK_SECS * rate as u64);
    let from = 44 + (consumed as usize) * 2;
    let to = from + (take as usize) * 2;
    let samples: Vec<f32> = bytes[from..to]
        .chunks_exact(2)
        .map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / i16::MAX as f32)
        .collect();
    Some((samples, rate, consumed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_chunk_respects_min_and_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        // 10 s of 16 kHz audio
        let samples: Vec<f32> = (0..160_000).map(|i| ((i % 100) as f32) / 100.0).collect();
        looma_audio::mix::write_wav_mono_16(&path, &samples, 16_000).unwrap();

        let (chunk, rate, start) = take_chunk(&path, 0).expect("10s should clear the 8s minimum");
        assert_eq!(rate, 16_000);
        assert_eq!(start, 0);
        assert_eq!(chunk.len(), 160_000);

        // Everything consumed → nothing new
        assert!(take_chunk(&path, 160_000).is_none());
        // 2 s remaining < 8 s minimum
        assert!(take_chunk(&path, 128_000).is_none());
    }
}
