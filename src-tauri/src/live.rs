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

use fly_asr::TranscriptionEngine;
use tauri::{Emitter, Manager};

use crate::models;
use crate::state::AppState;

const TICK_MS: u64 = 5_000;
/// Don't bother transcribing less than this much new audio…
const MIN_CHUNK_SECS: u64 = 8;
/// …and never take more than this in one bite.
const MAX_CHUNK_SECS: u64 = 30;
pub(crate) const LIVE_MODEL: &str = "ggml-small-q5_1";
/// Consecutive failed chunks before the live pane is told "unavailable".
const FAILURES_BEFORE_UNAVAILABLE: u32 = 3;

#[derive(Clone, serde::Serialize)]
struct LiveSegment {
    meeting_id: String,
    channel: &'static str, // "you" | "them"
    text: String,
    start_ms: u64,
}

#[derive(Clone, serde::Serialize)]
pub struct LiveStatus {
    meeting_id: String,
    state: &'static str, // "ready" | "unavailable"
    detail: String,
}

/// Emit a `live:status` event AND remember it in AppState: the pane mounts
/// on its own schedule (it appears when the recording-status poll notices
/// the capture), so a status emitted before its listener exists would be
/// lost — `live_status` (the command below) lets it catch up.
fn set_status(app: &tauri::AppHandle, status: LiveStatus) {
    *app.state::<AppState>().live_status.lock().unwrap() = Some(status.clone());
    let _ = app.emit("live:status", status);
}

/// Latest live-caption status for a meeting (None: nothing reported yet —
/// the pane keeps its optimistic "Listening…" line).
#[tauri::command]
pub fn live_status(state: tauri::State<'_, AppState>, meeting_id: String) -> Option<LiveStatus> {
    state
        .live_status
        .lock()
        .unwrap()
        .clone()
        .filter(|s| s.meeting_id == meeting_id)
}

struct ChannelCursor {
    path: PathBuf,
    channel: &'static str,
    consumed_samples: u64,
    prompt_tail: String,
}

/// What one channel's pass of a [`LiveCaptioner::tick`] produced.
pub enum ChunkOutcome {
    /// A chunk transcribed (text may be empty: silence).
    Text {
        channel: &'static str,
        text: String,
        /// Chunk start / end on the channel's recording timeline.
        start_ms: u64,
        end_ms: u64,
    },
    /// The engine failed on this chunk (the cursor still moved past it).
    Failed(String),
}

/// The engine-agnostic core of the live loop: per-channel cursors over the
/// WAVs being recorded, chunking, and transcription. The app loop below
/// turns its outcomes into `live:*` events; the contention bench
/// (`src-tauri/tests/contention_bench.rs`) drives the same code to measure
/// caption lag under load.
pub struct LiveCaptioner {
    cursors: [ChannelCursor; 2],
    tmp_dir: PathBuf,
}

impl LiveCaptioner {
    /// Follow `recording.mic.wav` ("you") and `recording.system.wav`
    /// ("them") in `out_dir`; chunk files go to `tmp_dir`.
    pub fn new(out_dir: &Path, tmp_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&tmp_dir);
        Self {
            cursors: [
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
            ],
            tmp_dir,
        }
    }

    pub fn tmp_dir(&self) -> &Path {
        &self.tmp_dir
    }

    /// Transcribe the new audio of each channel that has at least
    /// MIN_CHUNK_SECS of it. `stop` is checked between channels.
    pub async fn tick(
        &mut self,
        engine: &dyn TranscriptionEngine,
        stop: &AtomicBool,
    ) -> Vec<ChunkOutcome> {
        let mut out = Vec::new();
        for cur in self.cursors.iter_mut() {
            if let Some((samples, rate, start_sample)) = take_chunk(&cur.path, cur.consumed_samples)
            {
                let start_ms = start_sample * 1000 / rate as u64;
                let end_ms = (start_sample + samples.len() as u64) * 1000 / rate as u64;
                let resampled = fly_audio::mix::resample_linear(&samples, rate, 16_000);
                let chunk_path = self
                    .tmp_dir
                    .join(format!("{}-{}.wav", cur.channel, start_sample));
                if fly_audio::mix::write_wav_mono_16(&chunk_path, &resampled, 16_000).is_err() {
                    continue;
                }
                let opts = fly_asr::TranscribeOptions {
                    language: None,
                    prompt: (!cur.prompt_tail.is_empty()).then(|| cur.prompt_tail.clone()),
                    ..Default::default()
                };
                let outcome = match engine.transcribe(&chunk_path, &opts).await {
                    Ok(raw) => {
                        let text = raw
                            .words
                            .iter()
                            .map(|w| w.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" ");
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
                        }
                        ChunkOutcome::Text {
                            channel: cur.channel,
                            text,
                            start_ms,
                            end_ms,
                        }
                    }
                    Err(e) => ChunkOutcome::Failed(e.to_string()),
                };
                let _ = std::fs::remove_file(&chunk_path);
                cur.consumed_samples = start_sample + samples.len() as u64;
                out.push(outcome);
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
        out
    }
}

/// The live-caption engine: the small model on a strict CPU budget — a
/// quarter of the logical CPUs, 2..=4 threads (it stays well ahead of real
/// time on that; partials are cosmetic and the capture must never starve).
///
/// It deliberately stays off the GPU offload path (gpu.rs is post-meeting
/// only): it runs DURING capture, exactly when the GPU is busy with the video
/// call / screen share, and mid-meeting contention is the one regression this
/// app can't afford. On Windows that means the CPU whisper build; on macOS
/// whisper.cpp defaults to Metal, so `-ng` keeps live decoding on CPU (this
/// also sidesteps the Metal init abort on GPUs Metal can't serve — see the
/// guarded fallback in pipeline.rs).
pub fn live_engine(exe: PathBuf, model: PathBuf) -> fly_asr::whisper_cpp::WhisperCppEngine {
    let threads = std::thread::available_parallelism()
        .map(|n| (n.get() / 4).clamp(2, 4))
        .unwrap_or(2);
    fly_asr::whisper_cpp::WhisperCppEngine {
        exe,
        model,
        threads,
        force_cpu: cfg!(target_os = "macos"),
        // live windows are throwaway snippets — nothing to checkpoint
        resume: false,
    }
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
    // Single-attempt downloads: when the machine is offline, the loop must
    // settle on "unavailable" promptly, not after ~12 s of mirror retries —
    // post-meeting transcription (DownloadEffort::Full) rescues the models
    // later anyway.
    let exe = match models::ensure_tool_with(
        &on_model,
        &data_dir,
        models::WHISPER_ENGINE_ID,
        models::WHISPER_CLI_NAMES,
        "live transcript needs whisper.cpp",
        models::DownloadEffort::SingleAttempt,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            set_status(
                &app,
                LiveStatus {
                    meeting_id,
                    state: "unavailable",
                    detail: e,
                },
            );
            return;
        }
    };
    let model = match models::ensure_with(
        &on_model,
        &data_dir,
        LIVE_MODEL,
        models::DownloadEffort::SingleAttempt,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            set_status(
                &app,
                LiveStatus {
                    meeting_id,
                    state: "unavailable",
                    detail: e,
                },
            );
            return;
        }
    };
    let engine = live_engine(exe, model);
    set_status(
        &app,
        LiveStatus {
            meeting_id: meeting_id.clone(),
            state: "ready",
            detail: String::new(),
        },
    );

    // Chunk files go to the machine-local scratch dir keyed by meeting id,
    // not the meeting folder (which a rename can move mid-recording).
    let tmp_dir = fly_storage::scratch::meeting_scratch_dir(&data_dir, &meeting_id)
        .join(fly_storage::scratch::LIVE_TMP_DIR);
    let mut captioner = LiveCaptioner::new(&out_dir, tmp_dir);

    // Chunk failures across both channels, reset by any success. Engine
    // resolution succeeding does not mean the engine RUNS (missing dylib,
    // deleted model file mid-meeting): after a few consecutive failures the
    // pane must say "unavailable" instead of listening optimistically —
    // errors here were previously swallowed at debug level and the status
    // never downgraded. The loop keeps trying (a later chunk may recover,
    // which flips the status back to ready).
    let mut consecutive_failures: u32 = 0;

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
                    r.session.state() == fly_audio::CaptureState::Paused
                }
                _ => break, // recording ended
            }
        };
        if paused {
            continue;
        }

        for outcome in captioner.tick(&engine, &stop).await {
            match outcome {
                ChunkOutcome::Text {
                    channel,
                    text,
                    start_ms,
                    ..
                } => {
                    if consecutive_failures >= FAILURES_BEFORE_UNAVAILABLE {
                        set_status(
                            &app,
                            LiveStatus {
                                meeting_id: meeting_id.clone(),
                                state: "ready",
                                detail: String::new(),
                            },
                        );
                    }
                    consecutive_failures = 0;
                    if !text.trim().is_empty() {
                        let _ = app.emit(
                            "live:segment",
                            LiveSegment {
                                meeting_id: meeting_id.clone(),
                                channel,
                                text: text.trim().to_string(),
                                start_ms,
                            },
                        );
                    }
                }
                ChunkOutcome::Failed(e) => {
                    consecutive_failures += 1;
                    if consecutive_failures == FAILURES_BEFORE_UNAVAILABLE {
                        tracing::warn!(
                            failures = consecutive_failures,
                            "live chunk transcription keeps failing — captions \
                             unavailable: {e}"
                        );
                        set_status(
                            &app,
                            LiveStatus {
                                meeting_id: meeting_id.clone(),
                                state: "unavailable",
                                detail: format!(
                                    "Live captions are unavailable ({e}) — the full \
                                     transcript still arrives after Stop."
                                ),
                            },
                        );
                    } else {
                        tracing::debug!("live chunk transcription failed: {e}");
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(captioner.tmp_dir());
}

/// Read the samples appended past `consumed` from a mono 16-bit WAV that is
/// still being written (go by file length, not the header's sizes, which
/// lag the data; our own writer emits the canonical 44-byte header). Returns
/// `(samples, rate, start_sample)` only when at least MIN_CHUNK_SECS of new
/// audio exist; bounded by MAX_CHUNK_SECS. Reads only the header and the new
/// stretch — never the whole growing file (≈345 MB per channel per hour).
fn take_chunk(path: &Path, consumed: u64) -> Option<(Vec<f32>, u32, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len < 44 {
        return None;
    }
    let mut header = [0u8; 44];
    file.read_exact(&mut header).ok()?;
    let rate = u32::from_le_bytes(header[24..28].try_into().ok()?);
    if rate == 0 {
        return None;
    }
    let total_samples = (len - 44) / 2;
    let new = total_samples.saturating_sub(consumed);
    if new < MIN_CHUNK_SECS * rate as u64 {
        return None;
    }
    let take = new.min(MAX_CHUNK_SECS * rate as u64);
    file.seek(SeekFrom::Start(44 + consumed * 2)).ok()?;
    let mut bytes = vec![0u8; (take * 2) as usize];
    file.read_exact(&mut bytes).ok()?;
    let (pairs, _tail) = bytes.as_chunks::<2>();
    let samples: Vec<f32> = pairs
        .iter()
        .map(|p| i16::from_le_bytes(*p) as f32 / i16::MAX as f32)
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
        fly_audio::mix::write_wav_mono_16(&path, &samples, 16_000).unwrap();

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
