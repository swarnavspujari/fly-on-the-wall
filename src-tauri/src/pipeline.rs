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

use looma_asr::{TranscribeOptions, TranscriptionEngine};
use looma_core::align::{
    align_words_to_speakers, merge_channel_segments, segments_from_single_speaker, AlignOptions,
};
use looma_core::{Speaker, Transcript};
use looma_diarize::{DiarizationEngine, DiarizeOptions};
use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::state::AppState;
use crate::{hw, models};

pub const MIC_SPEAKER_KEY: &str = "mic";

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

/// Tauri entrypoint used by the transcribe command and auto-run-after-stop:
/// bridges pipeline events onto the app's event bus.
pub async fn run<R: tauri::Runtime>(app: tauri::AppHandle<R>, meeting_id: String) {
    let on_stage = {
        let app = app.clone();
        move |p: PipelineProgress| {
            let _ = app.emit("pipeline:progress", p);
        }
    };
    let on_model = {
        let app = app.clone();
        move |p: models::ModelProgress| {
            let _ = app.emit("model:progress", p);
        }
    };
    let state = app.state::<AppState>();
    let result = run_with(&state, &on_stage, &on_model, &meeting_id).await;

    let error = match result {
        Ok(_) => None,
        Err(e) => {
            tracing::error!(meeting_id, error = %e, "transcription pipeline failed");
            Some(e)
        }
    };
    state.pipeline_stage.lock().unwrap().remove(&meeting_id);
    let _ = app.emit(
        "pipeline:progress",
        PipelineProgress {
            meeting_id: meeting_id.clone(),
            stage: if error.is_some() { "error" } else { "done" }.into(),
            detail: None,
            done: true,
            error,
        },
    );
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
    let mic_wav = abs(&recording.mic_path);
    let system_wav = abs(&recording.system_path);
    let mixed_wav = abs(&recording.mixed_path);
    if mic_wav.is_none() && system_wav.is_none() && mixed_wav.is_none() {
        return Err("no recording files found on disk".into());
    }

    // ---- settings → engines ----
    let (tier, use_groq, max_quality, model_override) = {
        let storage = state.storage.lock().unwrap();
        let get = |k: &str| storage.get_setting(k).ok().flatten();
        (
            get("asr.tier").unwrap_or_else(|| hw::detect().recommended_tier),
            get("asr.use_groq").as_deref() == Some("true"),
            get("asr.max_quality").as_deref() == Some("true"),
            get("asr.model_id").filter(|s| !s.is_empty()),
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

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let asr: Box<dyn TranscriptionEngine> = if use_groq || tier == "cloud" {
        let key = state
            .secrets
            .get(looma_secrets::keys::GROQ_API_KEY)
            .map_err(|e| e.to_string())?
            .ok_or("Groq transcription is enabled but no Groq API key is set")?;
        Box::new(looma_asr::groq::GroqEngine::new(key))
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
        Box::new(looma_asr::whisper_cpp::WhisperCppEngine {
            exe: whisper_exe,
            model: model_path,
            threads,
        })
    };
    let diarizer = looma_diarize::sherpa::SherpaDiarizeEngine {
        exe: sherpa_exe,
        segmentation_model: seg_model,
        embedding_model: emb_model,
        threads,
    };

    // ---- prepare 16 kHz mono inputs ----
    emit_stage(state, on_stage, meeting_id, "preparing-audio", None);
    let work_dir = data_dir.join("recordings").join(meeting_id);
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    let per_channel = mic_wav.is_some() && system_wav.is_some();
    let prep = |src: &Path, name: &str| -> Result<PathBuf, String> {
        let dst = work_dir.join(name);
        let (samples, rate) = looma_audio::mix::read_wav_mono(src).map_err(|e| e.to_string())?;
        let resampled = looma_audio::mix::resample_linear(&samples, rate, 16_000);
        looma_audio::mix::write_wav_mono_16(&dst, &resampled, 16_000).map_err(|e| e.to_string())?;
        Ok(dst)
    };

    let opts = TranscribeOptions::default();
    let align_opts = AlignOptions::default();
    let mut language = None;
    let mut channels: Vec<Vec<looma_core::TranscriptSegment>> = Vec::new();
    let mut speakers: Vec<Speaker> = Vec::new();

    if per_channel {
        let mic_16k = prep(mic_wav.as_ref().unwrap(), "mic.16k.wav")?;
        let sys_16k = prep(system_wav.as_ref().unwrap(), "system.16k.wav")?;

        emit_stage(
            state,
            on_stage,
            meeting_id,
            "transcribing",
            Some("your microphone".into()),
        );
        let mic_raw = asr
            .transcribe(&mic_16k, &opts)
            .await
            .map_err(|e| e.to_string())?;
        language = language.or(mic_raw.language.clone());

        emit_stage(
            state,
            on_stage,
            meeting_id,
            "transcribing",
            Some("other participants".into()),
        );
        let sys_raw = asr
            .transcribe(&sys_16k, &opts)
            .await
            .map_err(|e| e.to_string())?;
        language = language.or(sys_raw.language.clone());

        emit_stage(state, on_stage, meeting_id, "diarizing", None);
        let turns = diarizer
            .diarize(&sys_16k, &DiarizeOptions::default())
            .await
            .map_err(|e| e.to_string())?;

        emit_stage(state, on_stage, meeting_id, "aligning", None);
        channels.push(segments_from_single_speaker(
            &mic_raw.words,
            MIC_SPEAKER_KEY,
            &align_opts,
        ));
        channels.push(align_words_to_speakers(&sys_raw.words, &turns, &align_opts));

        speakers.push(Speaker {
            key: MIC_SPEAKER_KEY.into(),
            label: "You".into(),
        });
        collect_speakers(&mut speakers, &channels[1]);
    } else {
        // single-track path (imports, mic-only recordings)
        let src = mixed_wav.or(mic_wav).or(system_wav).expect("checked above");
        let track_16k = prep(&src, "track.16k.wav")?;

        emit_stage(state, on_stage, meeting_id, "transcribing", None);
        let raw = asr
            .transcribe(&track_16k, &opts)
            .await
            .map_err(|e| e.to_string())?;
        language = raw.language.clone();

        emit_stage(state, on_stage, meeting_id, "diarizing", None);
        let turns = diarizer
            .diarize(&track_16k, &DiarizeOptions::default())
            .await
            .map_err(|e| e.to_string())?;

        emit_stage(state, on_stage, meeting_id, "aligning", None);
        let segments = align_words_to_speakers(&raw.words, &turns, &align_opts);
        collect_speakers(&mut speakers, &segments);
        channels.push(segments);
    }

    let transcript = Transcript {
        meeting_id: meeting_id.to_string(),
        language,
        engine: asr.id().to_string(),
        segments: merge_channel_segments(channels),
        speakers,
    };

    emit_stage(state, on_stage, meeting_id, "saving", None);
    state
        .storage
        .lock()
        .unwrap()
        .save_transcript(&transcript)
        .map_err(|e| e.to_string())?;

    // release the per-meeting guard for direct callers (run() also clears it)
    state.pipeline_stage.lock().unwrap().remove(meeting_id);
    Ok(transcript)
}

/// Register display labels for every speaker key seen in the segments
/// ("spk_0" → "Speaker 1", fallback speaker → "Unknown").
fn collect_speakers(speakers: &mut Vec<Speaker>, segments: &[looma_core::TranscriptSegment]) {
    for seg in segments {
        if speakers.iter().any(|s| s.key == seg.speaker_key) {
            continue;
        }
        let label = match seg.speaker_key.strip_prefix("spk_") {
            Some("unknown") => "Unknown".to_string(),
            Some(n) => match n.parse::<u32>() {
                Ok(i) => format!("Speaker {}", i + 1),
                Err(_) => seg.speaker_key.clone(),
            },
            None => seg.speaker_key.clone(),
        };
        speakers.push(Speaker {
            key: seg.speaker_key.clone(),
            label,
        });
    }
}
