//! The transcription pipeline: recording → 16 kHz prep → ASR per channel →
//! local diarization → word↔speaker alignment → merged, persisted transcript.
//!
//! Channel strategy (spec §6.4): the mic channel is a known speaker ("You"),
//! so only the system channel is diarized. When only a single mixed track
//! exists (file import), the whole track is diarized instead.
//! Diarization ALWAYS runs locally, even when ASR is Groq (spec §6.3).
//!
//! `run_with` is tauri-free (events go through sinks) so the golden E2E test
//! can drive the real pipeline without a webview runtime.

use std::path::{Path, PathBuf};

use fly_asr::{RawTranscript, TranscribeOptions, TranscriptionEngine};
use fly_core::align::{
    align_words_to_speakers, merge_channel_segments, segments_from_single_speaker, AlignOptions,
};
use fly_core::repeat::collapse_loops;
use fly_core::{Speaker, Transcript};
use fly_diarize::{DiarizationEngine, DiarizeOptions};
use serde::Serialize;

use crate::state::AppState;
use crate::{gpu, hw, models};

pub const MIC_SPEAKER_KEY: &str = "mic";

/// Marker for the one failure retrying can never fix: the recording files are
/// gone from disk. The scheduler matches on this prefix to skip its retries.
pub const ERR_NO_RECORDING_FILES: &str = "recording files not found on disk";

#[derive(Clone, Serialize)]
pub struct PipelineProgress {
    pub meeting_id: String,
    pub stage: String,
    pub detail: Option<String>,
    pub done: bool,
    pub error: Option<String>,
}

pub type StageSink<'a> = &'a (dyn Fn(PipelineProgress) + Send + Sync);

fn emit_stage(
    state: &AppState,
    sink: StageSink<'_>,
    meeting_id: &str,
    stage: &str,
    detail: Option<String>,
) {
    state
        .pipeline_stage
        .lock()
        .unwrap()
        .insert(meeting_id.to_string(), stage.to_string());
    sink(PipelineProgress {
        meeting_id: meeting_id.into(),
        stage: stage.into(),
        detail,
        done: false,
        error: None,
    });
}

/// Sidecar thread budget. Transcription is background work: recording and
/// whatever the user is actively doing must stay responsive while it runs,
/// so it never gets more than half the logical CPUs, capped at 8 (whisper.cpp
/// scales poorly beyond that anyway). Pipelines are additionally serialized
/// behind the queue worker (scheduler.rs), so this is the whole ASR budget.
pub fn sidecar_threads() -> usize {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (n / 2).clamp(2, 8)
}

pub async fn run_with(
    state: &AppState,
    on_stage: StageSink<'_>,
    on_model: models::ProgressSink<'_>,
    meeting_id: &str,
) -> Result<Transcript, String> {
    // one pipeline per meeting at a time
    {
        let stages = state.pipeline_stage.lock().unwrap();
        if stages.contains_key(meeting_id) {
            return Err("transcription already running for this meeting".into());
        }
    }
    emit_stage(state, on_stage, meeting_id, "starting", None);

    let (recording, data_dir) = {
        let storage = state.storage.lock().unwrap();
        let meeting = storage.get_meeting(meeting_id).map_err(|e| e.to_string())?;
        let recording = meeting
            .recording
            .ok_or("meeting has no recording to transcribe")?;
        (recording, state.data_dir.clone())
    };

    let abs = |rel: &Option<String>| -> Option<PathBuf> {
        rel.as_ref()
            .map(|r| data_dir.join(r))
            .filter(|p| p.exists())
    };
    let mut mic_wav = abs(&recording.mic_path);
    let mut system_wav = abs(&recording.system_path);
    let mut mixed_wav = abs(&recording.mixed_path);
    if mic_wav.is_none() && system_wav.is_none() && mixed_wav.is_none() {
        // Self-heal: a referenced folder can end up parked under
        // recordings/_unlinked/ (the pre-1.0.2 multi-instance launch could
        // race the v2 migration's orphan sweep). If the parked folder is
        // still there under the same name, move it back and carry on.
        if restore_parked_recording(&data_dir, &recording) {
            mic_wav = abs(&recording.mic_path);
            system_wav = abs(&recording.system_path);
            mixed_wav = abs(&recording.mixed_path);
        }
    }
    if mic_wav.is_none() && system_wav.is_none() && mixed_wav.is_none() {
        let dir = fly_storage::recording_dir_rel(&recording).unwrap_or_default();
        return Err(format!(
            "{ERR_NO_RECORDING_FILES} — expected them under \"{dir}\" in the app data folder. \
             If the audio files were moved or deleted, this meeting can't be re-transcribed."
        ));
    }

    // ---- settings → engines ----
    let (tier, use_groq, max_quality, model_override, language_setting, use_gpu) = {
        let storage = state.storage.lock().unwrap();
        let get = |k: &str| storage.get_setting(k).ok().flatten();
        (
            get("asr.tier")
                .or_else(|| hw::cached(&storage).map(|h| h.recommended_tier))
                .unwrap_or_else(|| hw::detect_and_cache(&storage).recommended_tier),
            get("asr.use_groq").as_deref() == Some("true"),
            get("asr.max_quality").as_deref() == Some("true"),
            get("asr.model_id").filter(|s| !s.is_empty()),
            get("asr.language").filter(|s| !s.is_empty()),
            gpu::enabled(&storage),
        )
    };

    // Diarization models are ALWAYS ensured — local on every tier (§6.3).
    emit_stage(state, on_stage, meeting_id, "ensuring-models", None);
    let sherpa_exe = models::ensure_tool(
        on_model,
        &data_dir,
        "sherpa-bin",
        &["sherpa-onnx-offline-speaker-diarization"],
        "install sherpa-onnx or report this platform in an issue",
    )
    .await?;
    let seg_model = models::ensure(on_model, &data_dir, "pyannote-seg").await?;
    let emb_model = models::ensure(on_model, &data_dir, "campplus-embedding").await?;

    let threads = sidecar_threads();

    // Meetings are transcribed in a fixed language ("asr.language" setting,
    // default English, "auto" opts back into detection). Auto-detect on a
    // language-less window (silence, noise) destabilizes whisper decoding —
    // it was part of how a real meeting collapsed into a hallucination loop.
    let language = match language_setting.as_deref() {
        Some("auto") => None,
        Some(lang) => Some(lang.to_string()),
        None => Some("en".to_string()),
    };
    let opts = TranscribeOptions {
        language,
        prompt: None,
        // decode 30 s windows independently: one bad window must never poison
        // the rest of the recording (observed: a loop consumed 63 minutes)
        max_context: Some(0),
    };

    // Cloud ASR only with a stored key. A Groq toggle left on with no key
    // (e.g. the key never survived a settings change) must not sink the
    // pipeline — fall back to fully-local transcription, visibly.
    let groq_key = if use_groq || tier == "cloud" {
        let key = state
            .secrets
            .get(fly_secrets::keys::GROQ_API_KEY)
            .ok()
            .flatten();
        if key.is_none() {
            tracing::warn!("Groq transcription enabled but no API key stored — using local ASR");
            emit_stage(
                state,
                on_stage,
                meeting_id,
                "ensuring-models",
                Some(
                    "Groq is enabled but no API key is saved — transcribing locally instead".into(),
                ),
            );
        }
        key
    } else {
        None
    };
    let asr: GuardedAsr = if let Some(key) = groq_key {
        GuardedAsr::single(Box::new(fly_asr::groq::GroqEngine::new(key)))
    } else {
        let model_id = model_override
            .unwrap_or_else(|| hw::default_model_for_tier(&tier, max_quality).to_string());
        let whisper_exe = models::ensure_tool(
            on_model,
            &data_dir,
            "whisper-bin",
            &["whisper-cli"],
            "install whisper.cpp (macOS: brew install whisper-cpp; Linux: build from \
             source or use your package manager) or enable the Groq cloud fallback \
             in Settings",
        )
        .await?;
        let model_path = models::ensure(on_model, &data_dir, &model_id).await?;

        // GPU offload. Windows: the pinned Vulkan build, but only when a
        // one-time benchmark measured it faster on this machine (gpu.rs);
        // any failure falls back to the CPU engine below. macOS/Linux:
        // whisper.cpp builds there default to Metal/GPU on their own, so
        // asr.use_gpu only gates a force-to-CPU flag and nothing else about
        // the shipped path changes.
        #[cfg(target_os = "windows")]
        let gpu_exe: Option<PathBuf> = if use_gpu {
            let sample_src = mic_wav
                .as_ref()
                .or(system_wav.as_ref())
                .or(mixed_wav.as_ref())
                .expect("checked above: at least one recording file exists");
            emit_stage(state, on_stage, meeting_id, "benchmarking", None);
            let notify = |detail: String| {
                emit_stage(state, on_stage, meeting_id, "benchmarking", Some(detail));
            };
            gpu::plan(
                state,
                on_model,
                &notify,
                gpu::PlanRequest {
                    cpu_exe: &whisper_exe,
                    model_path: &model_path,
                    model_id: &model_id,
                    threads,
                    opts: &opts,
                    sample_src,
                },
            )
            .await
        } else {
            None
        };
        #[cfg(not(target_os = "windows"))]
        let gpu_exe: Option<PathBuf> = None;

        match gpu_exe {
            Some(exe) => GuardedAsr::with_cpu_fallback(
                Box::new(fly_asr::whisper_cpp::WhisperCppEngine {
                    exe,
                    model: model_path.clone(),
                    threads,
                    force_cpu: false,
                }),
                "whisper.cpp-vulkan",
                Box::new(fly_asr::whisper_cpp::WhisperCppEngine {
                    exe: whisper_exe,
                    model: model_path,
                    threads,
                    force_cpu: false,
                }),
                model_id,
            ),
            None => GuardedAsr::single(Box::new(fly_asr::whisper_cpp::WhisperCppEngine {
                exe: whisper_exe,
                model: model_path,
                threads,
                // On a GPU-capable build (Metal on macOS) honor the off
                // switch; the Windows CPU build has no GPU backend and
                // ignores it.
                force_cpu: !use_gpu,
            })),
        }
    };
    let diarizer = fly_diarize::sherpa::SherpaDiarizeEngine {
        exe: sherpa_exe,
        segmentation_model: seg_model,
        embedding_model: emb_model,
        threads,
    };

    // ---- prepare 16 kHz mono inputs ----
    emit_stage(state, on_stage, meeting_id, "preparing-audio", None);
    // intermediates live next to the recordings (the meeting's folder)
    let work_dir = mic_wav
        .as_ref()
        .or(system_wav.as_ref())
        .or(mixed_wav.as_ref())
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .expect("checked above: at least one recording file exists");
    let per_channel = mic_wav.is_some() && system_wav.is_some();
    let mut intermediates: Vec<PathBuf> = Vec::new();
    let prep = |src: &Path, name: &str| -> Result<PathBuf, String> {
        let dst = work_dir.join(name);
        let (samples, rate) = fly_audio::mix::read_wav_mono(src).map_err(|e| e.to_string())?;
        let resampled = fly_audio::mix::resample_linear(&samples, rate, 16_000);
        fly_audio::mix::write_wav_mono_16(&dst, &resampled, 16_000).map_err(|e| e.to_string())?;
        Ok(dst)
    };

    let align_opts = AlignOptions::default();
    let mut language = None;
    let mut channels: Vec<Vec<fly_core::TranscriptSegment>> = Vec::new();
    let mut speakers: Vec<Speaker> = Vec::new();

    // A GPU failure mid-transcription surfaces as a one-line detail and the
    // batch retries on CPU (GuardedAsr) — the meeting always gets its
    // transcript.
    let on_fallback =
        |detail: String| emit_stage(state, on_stage, meeting_id, "transcribing", Some(detail));

    if per_channel {
        let mic_16k = prep(mic_wav.as_ref().unwrap(), "mic.16k.wav")?;
        let sys_16k = prep(system_wav.as_ref().unwrap(), "system.16k.wav")?;
        intermediates.extend([mic_16k.clone(), sys_16k.clone()]);

        emit_stage(
            state,
            on_stage,
            meeting_id,
            "transcribing",
            Some("your microphone".into()),
        );
        let mic_raw = guard_loops(asr.transcribe(&mic_16k, &opts, &on_fallback).await?, "mic");
        language = language.or(mic_raw.language.clone());

        emit_stage(
            state,
            on_stage,
            meeting_id,
            "transcribing",
            Some("other participants".into()),
        );
        let sys_raw = guard_loops(
            asr.transcribe(&sys_16k, &opts, &on_fallback).await?,
            "system",
        );
        language = language.or(sys_raw.language.clone());

        emit_stage(state, on_stage, meeting_id, "diarizing", None);
        let turns = fly_diarize::drop_dust_clusters(
            diarizer
                .diarize(&sys_16k, &DiarizeOptions::default())
                .await
                .map_err(|e| e.to_string())?,
        );

        // Cross-talk de-dup (§6.4, E1). Without a headset the built-in mic
        // re-captures the far-end played through the speakers, so the far-end
        // is transcribed on BOTH channels — double-counted and mislabelled
        // "You" on merge. Split into a clean near ("you") stream and one
        // far-end stream, keeping each duplicated run once from the better
        // source (measured: the system loopback). Genuine talk-over — DIFFERENT
        // words on the two channels at the same instant — has no cross-channel
        // token match and survives.
        let split = fly_core::crosstalk::split_crosstalk(
            &mic_raw.words,
            &sys_raw.words,
            &fly_core::crosstalk::CrosstalkOptions::default(),
        );
        let echo_dropped = mic_raw.words.len().saturating_sub(split.you_words.len());
        if echo_dropped > 0 {
            tracing::info!(
                echo_dropped,
                mic_words = mic_raw.words.len(),
                "removed far-end cross-talk from the mic channel before alignment"
            );
        }

        emit_stage(state, on_stage, meeting_id, "aligning", None);
        channels.push(segments_from_single_speaker(
            &split.you_words,
            MIC_SPEAKER_KEY,
            &align_opts,
        ));
        channels.push(align_words_to_speakers(
            &split.far_words,
            &turns,
            &align_opts,
        ));

        speakers.push(Speaker {
            key: MIC_SPEAKER_KEY.into(),
            label: "You".into(),
        });
        collect_speakers(&mut speakers, &channels[1]);
    } else {
        // single-track path (imports, mic-only recordings)
        let src = mixed_wav.or(mic_wav).or(system_wav).expect("checked above");
        let track_16k = prep(&src, "track.16k.wav")?;
        intermediates.push(track_16k.clone());

        emit_stage(state, on_stage, meeting_id, "transcribing", None);
        let raw = guard_loops(
            asr.transcribe(&track_16k, &opts, &on_fallback).await?,
            "mixed",
        );
        language = raw.language.clone();

        emit_stage(state, on_stage, meeting_id, "diarizing", None);
        let turns = fly_diarize::drop_dust_clusters(
            diarizer
                .diarize(&track_16k, &DiarizeOptions::default())
                .await
                .map_err(|e| e.to_string())?,
        );

        emit_stage(state, on_stage, meeting_id, "aligning", None);
        let segments = align_words_to_speakers(&raw.words, &turns, &align_opts);
        collect_speakers(&mut speakers, &segments);
        channels.push(segments);
    }

    // A GPU that failed mid-run gets pinned back to CPU so the next meeting
    // doesn't hit the same failure (toggling the setting re-tests).
    if let Some((model_id, error)) = asr.runtime_failure() {
        let storage = state.storage.lock().unwrap();
        gpu::record_runtime_failure(&storage, &model_id, &error);
    }

    let transcript = Transcript {
        meeting_id: meeting_id.to_string(),
        language,
        engine: asr.engine_id(),
        segments: merge_channel_segments(channels),
        speakers,
    };

    emit_stage(state, on_stage, meeting_id, "saving", None);
    {
        let storage = state.storage.lock().unwrap();
        storage
            .save_transcript(&transcript)
            .map_err(|e| e.to_string())?;
        // A previous run's polished variant references segment ids that no
        // longer exist — drop it (the chained polish pass rebuilds it).
        storage
            .clear_cleaned_transcript(meeting_id)
            .map_err(|e| e.to_string())?;
    }

    // 16 kHz intermediates are pure derived data — drop them once the
    // transcript is saved so meeting folders hold only the real recordings.
    // (A failed run keeps them; the retry overwrites them anyway.)
    for f in &intermediates {
        if let Err(e) = std::fs::remove_file(f) {
            tracing::warn!(path = %f.display(), error = %e, "could not remove 16k intermediate");
        }
    }

    // release the per-meeting guard (on failure the scheduler clears it)
    state.pipeline_stage.lock().unwrap().remove(meeting_id);
    Ok(transcript)
}

/// The pipeline's ASR with an automatic, visible CPU fallback: a GPU engine
/// that fails to launch, exits nonzero, or OOMs must never sink the pipeline.
/// The failed batch is retried on the validated CPU engine and the rest of
/// the run stays there; the failure is reported via `runtime_failure` so the
/// machine gets pinned back to CPU for future meetings.
struct GuardedAsr {
    primary: Box<dyn TranscriptionEngine>,
    /// `Transcript::engine` label while the primary is healthy — the trait
    /// `id()` can't tell the Vulkan build from the CPU one (same engine).
    primary_label: Option<&'static str>,
    cpu_fallback: Option<Box<dyn TranscriptionEngine>>,
    /// Model the GPU decision was made for (verdict re-pinning on failure).
    gpu_model_id: Option<String>,
    failure: std::sync::Mutex<Option<String>>,
}

impl GuardedAsr {
    fn single(engine: Box<dyn TranscriptionEngine>) -> Self {
        Self {
            primary: engine,
            primary_label: None,
            cpu_fallback: None,
            gpu_model_id: None,
            failure: std::sync::Mutex::new(None),
        }
    }

    fn with_cpu_fallback(
        primary: Box<dyn TranscriptionEngine>,
        primary_label: &'static str,
        cpu: Box<dyn TranscriptionEngine>,
        gpu_model_id: String,
    ) -> Self {
        Self {
            primary,
            primary_label: Some(primary_label),
            cpu_fallback: Some(cpu),
            gpu_model_id: Some(gpu_model_id),
            failure: std::sync::Mutex::new(None),
        }
    }

    fn failed_over(&self) -> bool {
        self.failure.lock().unwrap().is_some()
    }

    /// What actually transcribed this meeting, for `Transcript::engine`.
    fn engine_id(&self) -> String {
        if self.failed_over() {
            if let Some(cpu) = &self.cpu_fallback {
                return cpu.id().to_string();
            }
        }
        self.primary_label.unwrap_or(self.primary.id()).to_string()
    }

    /// (model_id, error) when the GPU primary failed during this run.
    fn runtime_failure(&self) -> Option<(String, String)> {
        let failure = self.failure.lock().unwrap().clone()?;
        Some((self.gpu_model_id.clone()?, failure))
    }

    async fn transcribe(
        &self,
        wav: &Path,
        opts: &TranscribeOptions,
        notify: &(dyn Fn(String) + Send + Sync),
    ) -> Result<RawTranscript, String> {
        if !self.failed_over() {
            return match self.primary.transcribe(wav, opts).await {
                Ok(raw) => Ok(raw),
                Err(e) => {
                    let Some(cpu) = &self.cpu_fallback else {
                        return Err(e.to_string());
                    };
                    let msg = e.to_string();
                    tracing::warn!(error = %msg, "GPU transcription failed — falling back to CPU");
                    notify("GPU transcription failed — continuing on CPU".into());
                    *self.failure.lock().unwrap() = Some(msg);
                    cpu.transcribe(wav, opts).await.map_err(|e| e.to_string())
                }
            };
        }
        self.cpu_fallback
            .as_ref()
            .expect("failed_over implies a fallback engine")
            .transcribe(wav, opts)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Recovery for a meeting folder wrongly parked under `recordings/_unlinked/`:
/// if the folder `recording_json` points at is missing but a folder with the
/// same name sits in `_unlinked/`, move it back. Returns whether a restore
/// happened (the caller then re-resolves paths). Restoring is safe: parking
/// was a plain rename, and the relative paths in `recording_json` are
/// unchanged by the round trip.
fn restore_parked_recording(data_dir: &Path, rec: &fly_core::RecordingRef) -> bool {
    let Some(rel) = fly_storage::recording_dir_rel(rec) else {
        return false;
    };
    let expected = data_dir.join(&rel);
    let Some(name) = expected.file_name() else {
        return false;
    };
    let parked = data_dir.join("recordings").join("_unlinked").join(name);
    if expected.exists() || !parked.is_dir() {
        return false;
    }
    match std::fs::rename(&parked, &expected) {
        Ok(()) => {
            tracing::info!(
                dir = %rel,
                "restored meeting folder from recordings/_unlinked (was parked by a raced migration)"
            );
            true
        }
        Err(e) => {
            tracing::warn!(dir = %rel, error = %e, "could not restore parked meeting folder");
            false
        }
    }
}

/// Engine-agnostic hallucination guard: collapse consecutive-repetition loops
/// in the raw word stream (whisper and cloud engines both produce them) so a
/// stuck decoder can never flood a transcript. Collapses are logged — they
/// are evidence of an upstream decoding problem worth seeing in the logs.
fn guard_loops(raw: RawTranscript, channel: &str) -> RawTranscript {
    let (words, runs) = collapse_loops(raw.words);
    for run in &runs {
        tracing::warn!(
            channel,
            phrase = %run.phrase,
            reps = run.reps,
            start_ms = run.start_ms,
            end_ms = run.end_ms,
            "collapsed ASR repetition loop"
        );
    }
    RawTranscript { words, ..raw }
}

/// Register display labels for every speaker key seen in the segments.
/// Diarized speakers are numbered by FIRST APPEARANCE ("Speaker 1, 2, …"),
/// not by raw cluster id: dropping dust clusters leaves sparse ids (spk_1,
/// spk_3), and "Speaker 2 / Speaker 4" for two people reads as a bug. The
/// fallback cluster becomes "Unknown"; "mic" is pre-registered as "You".
fn collect_speakers(speakers: &mut Vec<Speaker>, segments: &[fly_core::TranscriptSegment]) {
    let mut next = 1 + speakers
        .iter()
        .filter(|s| s.label.starts_with("Speaker "))
        .count();
    for seg in segments {
        if speakers.iter().any(|s| s.key == seg.speaker_key) {
            continue;
        }
        let label = match seg.speaker_key.strip_prefix("spk_") {
            Some("unknown") => "Unknown".to_string(),
            Some(_) => {
                let l = format!("Speaker {next}");
                next += 1;
                l
            }
            None => seg.speaker_key.clone(),
        };
        speakers.push(Speaker {
            key: seg.speaker_key.clone(),
            label,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_core::TranscriptSegment;

    fn seg(key: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: "x".into(),
            speaker_key: key.into(),
            start_ms: 0,
            end_ms: 0,
            text: String::new(),
            words: vec![],
        }
    }

    /// A meeting folder wrongly parked under `recordings/_unlinked/` (raced
    /// migration) is moved back so the recording paths resolve again; a
    /// second call (or a genuinely missing folder) is a no-op.
    #[test]
    fn parked_recording_folder_is_restored() {
        let dir = tempfile::tempdir().unwrap();
        let rec = fly_core::RecordingRef {
            mic_path: Some("recordings/2026-07-02 TB 1 1/recording.mic.wav".into()),
            system_path: None,
            mixed_path: None,
            duration_ms: 1000,
        };
        let parked = dir.path().join("recordings/_unlinked/2026-07-02 TB 1 1");
        std::fs::create_dir_all(&parked).unwrap();
        std::fs::write(parked.join("recording.mic.wav"), b"RIFF").unwrap();

        assert!(restore_parked_recording(dir.path(), &rec));
        assert!(dir
            .path()
            .join("recordings/2026-07-02 TB 1 1/recording.mic.wav")
            .exists());
        assert!(!parked.exists());
        // already restored → nothing left to do
        assert!(!restore_parked_recording(dir.path(), &rec));
    }

    #[test]
    fn speakers_are_numbered_contiguously_by_appearance() {
        // Dust-dropping leaves sparse cluster ids (spk_1, spk_3); labelling by
        // id would show "Speaker 2 / Speaker 4". Number by first appearance.
        let mut speakers = vec![Speaker {
            key: MIC_SPEAKER_KEY.into(),
            label: "You".into(),
        }];
        let segs = [seg("spk_1"), seg("spk_1"), seg("spk_3"), seg("spk_unknown")];
        collect_speakers(&mut speakers, &segs);
        let label = |k: &str| speakers.iter().find(|s| s.key == k).unwrap().label.clone();
        assert_eq!(label(MIC_SPEAKER_KEY), "You");
        assert_eq!(label("spk_1"), "Speaker 1");
        assert_eq!(label("spk_3"), "Speaker 2");
        assert_eq!(label("spk_unknown"), "Unknown");
    }
}
