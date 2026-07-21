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

/// Speaker-count hint for diarization, derived from the meeting's attendee
/// list ONLY when the user confirmed it in the attendee editor. Calendar
/// rosters are unreliable count proxies (MS Graph omits the organizer,
/// rosters carry rooms/declines — forcing a wrong count MERGES real voices,
/// see the E2 findings), so an unconfirmed list never drives the engine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpeakerHint {
    /// User said it's just them: no diarization needed, everything is "You".
    JustYou,
    /// Total speaker count INCLUDING the user.
    Total(usize),
    /// No trustworthy count — engine default (threshold clustering).
    Unknown,
}

pub fn speaker_hint(meeting: &fly_core::Meeting) -> SpeakerHint {
    if !meeting.attendees_confirmed {
        return SpeakerHint::Unknown;
    }
    match meeting.attendees.len() {
        0 => SpeakerHint::JustYou,
        n => SpeakerHint::Total(n + 1),
    }
}

impl SpeakerHint {
    fn just_you(self) -> bool {
        matches!(self, SpeakerHint::JustYou)
    }
    /// Cluster count for the system-only channel: the mic channel is already
    /// pre-labeled "You", so the far end is attendees-minus-one — the same
    /// arithmetic the E2 1:1 experiment validated (2 attendees → 1 cluster).
    fn system_clusters(self) -> Option<usize> {
        match self {
            SpeakerHint::Total(n) => Some(n.saturating_sub(1).max(1)),
            _ => None,
        }
    }
    /// Cluster count for a single mixed track (the user is in the mix).
    fn mixed_clusters(self) -> Option<usize> {
        match self {
            SpeakerHint::Total(n) => Some(n),
            _ => None,
        }
    }
}

fn diarize_opts(num_speakers: Option<usize>) -> DiarizeOptions {
    DiarizeOptions {
        num_speakers,
        ..DiarizeOptions::default()
    }
}

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
    // A cancel request left over from an earlier run of this meeting must
    // not kill this fresh one; requests arriving from here on stick.
    state.cancel_requests.lock().unwrap().remove(meeting_id);

    let (recording, hint, data_dir) = {
        let storage = state.storage.lock().unwrap();
        let meeting = storage.get_meeting(meeting_id).map_err(|e| e.to_string())?;
        let hint = speaker_hint(&meeting);
        let recording = meeting
            .recording
            .ok_or("meeting has no recording to transcribe")?;
        (recording, hint, state.data_dir.clone())
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
    // The GPU speed test cuts its sample from whichever recording exists
    // (same pick order as the pipeline's inputs below).
    let sample_src: PathBuf = mic_wav
        .clone()
        .or_else(|| system_wav.clone())
        .or_else(|| mixed_wav.clone())
        .expect("checked above: at least one recording file exists");

    let asr: GuardedAsr = if let Some(key) = groq_key {
        // A Groq failure that survives the engine's own per-chunk retries
        // (outage, exhausted quota budget, rejected payload) must not sink
        // the meeting — the SAME local chain the no-cloud path uses (GPU
        // included) backs it, so enabling cloud never forfeits the GPU.
        // Best-effort: if the local model can't be ensured (e.g. offline),
        // Groq still runs alone.
        let model_id = model_override
            .unwrap_or_else(|| hw::default_model_for_tier(&tier, max_quality).to_string());
        let local = async {
            let exe = models::ensure_tool(
                on_model,
                &data_dir,
                models::WHISPER_ENGINE_ID,
                models::WHISPER_CLI_NAMES,
                "install whisper.cpp so cloud transcription has a local fallback",
            )
            .await?;
            let model = models::ensure(on_model, &data_dir, &model_id).await?;
            Ok::<_, String>((exe, model))
        }
        .await;
        match local {
            Ok((cpu_exe, model_path)) => {
                let (locals, spillover) = local_tiers(
                    state,
                    on_stage,
                    on_model,
                    meeting_id,
                    cpu_exe,
                    model_path,
                    &model_id,
                    threads,
                    &opts,
                    &sample_src,
                    use_gpu,
                )
                .await;
                let mut tiers = vec![AsrTier {
                    engine: Box::new(fly_asr::groq::GroqEngine {
                        // finished (paced) cloud chunks survive a restart
                        resume: true,
                        // a full quota window must never idle the validated
                        // local engine below — long pacer waits decode that
                        // chunk locally, then return to the cloud
                        local_spillover: spillover,
                        ..fly_asr::groq::GroqEngine::new(key)
                    }) as Box<dyn TranscriptionEngine>,
                    label: Some("groq"),
                    gpu_model_id: None,
                }];
                tiers.extend(locals);
                GuardedAsr::chain(tiers)
            }
            Err(e) => {
                tracing::warn!(error = %e, "no local fallback for Groq available — cloud only");
                GuardedAsr::chain(vec![AsrTier {
                    engine: Box::new(fly_asr::groq::GroqEngine {
                        resume: true,
                        ..fly_asr::groq::GroqEngine::new(key)
                    }) as Box<dyn TranscriptionEngine>,
                    label: Some("groq"),
                    gpu_model_id: None,
                }])
            }
        }
    } else {
        let model_id = model_override
            .unwrap_or_else(|| hw::default_model_for_tier(&tier, max_quality).to_string());
        let whisper_exe = models::ensure_tool(
            on_model,
            &data_dir,
            models::WHISPER_ENGINE_ID,
            models::WHISPER_CLI_NAMES,
            "install whisper.cpp (macOS: brew install whisper-cpp; Linux: build from \
             source or use your package manager) or enable the Groq cloud fallback \
             in Settings",
        )
        .await?;
        // A wanted-but-absent model that can't be downloaded (offline, CDN
        // outage) must not sink the meeting when another model is already on
        // disk — fall back to the best installed one, visibly.
        let (model_id, model_path) = match models::ensure(on_model, &data_dir, &model_id).await {
            Ok(path) => (model_id, path),
            Err(e) => match models::best_installed_asr_model(&data_dir, &model_id) {
                Some((fallback_id, path)) => {
                    tracing::warn!(
                        wanted = %model_id, using = fallback_id, error = %e,
                        "model unavailable — using installed model instead"
                    );
                    // Human names, and no em-dash: the frontend already
                    // joins stage and detail with one.
                    let name = |id: &str| {
                        models::artifact(id)
                            .map(|a| a.display)
                            .unwrap_or(id)
                            .to_string()
                    };
                    emit_stage(
                        state,
                        on_stage,
                        meeting_id,
                        "ensuring-models",
                        Some(format!(
                            "{} unavailable, transcribing with installed {}",
                            name(&model_id),
                            name(fallback_id)
                        )),
                    );
                    (fallback_id.to_string(), path)
                }
                None => return Err(e),
            },
        };

        GuardedAsr::chain(
            local_tiers(
                state,
                on_stage,
                on_model,
                meeting_id,
                whisper_exe,
                model_path,
                &model_id,
                threads,
                &opts,
                &sample_src,
                use_gpu,
            )
            .await
            .0,
        )
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

    // Batch progress from the engine → a live "channel (engine 42%)" stage
    // detail naming the tier that is actually decoding. Percentages are
    // speech-time fractions (silence is never decoded), the honest measure
    // of remaining work on long recordings. Parentheses, not an em-dash:
    // the frontend already joins stage and detail with one.
    let progress_detail = |label: Option<&'static str>| {
        move |kind: TierKind, p: fly_asr::TranscribeProgress| {
            if let Some(detail) = transcribe_detail(kind, label, p) {
                emit_stage(state, on_stage, meeting_id, "transcribing", Some(detail));
            }
        }
    };

    // Cooperative cancellation: the delete/cancel commands drop this meeting
    // id into `cancel_requests` (cleared at the start of this run); engines
    // poll it between batches — checkpoints make the abort cheap.
    let cancelled = || state.cancel_requests.lock().unwrap().contains(meeting_id);

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
        let mic_raw = guard_loops(
            asr.transcribe(
                &mic_16k,
                &opts,
                &on_fallback,
                &progress_detail(Some("your microphone")),
                &cancelled,
            )
            .await?,
            "mic",
        );
        language = language.or(mic_raw.language.clone());

        emit_stage(
            state,
            on_stage,
            meeting_id,
            "transcribing",
            Some("other participants".into()),
        );
        let sys_raw = guard_loops(
            asr.transcribe(
                &sys_16k,
                &opts,
                &on_fallback,
                &progress_detail(Some("other participants")),
                &cancelled,
            )
            .await?,
            "system",
        );
        language = language.or(sys_raw.language.clone());

        // ASR checks between batches; catch a cancel that landed after the
        // last batch before spending minutes on diarization.
        if cancelled() {
            return Err(fly_asr::AsrError::Cancelled.to_string());
        }
        emit_stage(state, on_stage, meeting_id, "diarizing", None);
        // "Just you": no far-end speakers exist, so the engine run is skipped
        // entirely and any surviving far-end speech is attributed to You.
        let turns = if hint.just_you() {
            Vec::new()
        } else {
            fly_diarize::drop_dust_clusters(
                diarizer
                    .diarize(&sys_16k, &diarize_opts(hint.system_clusters()))
                    .await
                    .map_err(|e| e.to_string())?,
            )
        };

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
        channels.push(if hint.just_you() {
            segments_from_single_speaker(&split.far_words, MIC_SPEAKER_KEY, &align_opts)
        } else {
            align_words_to_speakers(&split.far_words, &turns, &align_opts)
        });

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
            asr.transcribe(
                &track_16k,
                &opts,
                &on_fallback,
                &progress_detail(None),
                &cancelled,
            )
            .await?,
            "mixed",
        );
        language = raw.language.clone();

        // ASR checks between batches; catch a cancel that landed after the
        // last batch before spending minutes on diarization.
        if cancelled() {
            return Err(fly_asr::AsrError::Cancelled.to_string());
        }
        emit_stage(state, on_stage, meeting_id, "diarizing", None);
        if hint.just_you() {
            // single track, just the user: everything is You, no engine run
            emit_stage(state, on_stage, meeting_id, "aligning", None);
            let segments = segments_from_single_speaker(&raw.words, MIC_SPEAKER_KEY, &align_opts);
            speakers.push(Speaker {
                key: MIC_SPEAKER_KEY.into(),
                label: "You".into(),
            });
            channels.push(segments);
        } else {
            let turns = fly_diarize::drop_dust_clusters(
                diarizer
                    .diarize(&track_16k, &diarize_opts(hint.mixed_clusters()))
                    .await
                    .map_err(|e| e.to_string())?,
            );

            emit_stage(state, on_stage, meeting_id, "aligning", None);
            let segments = align_words_to_speakers(&raw.words, &turns, &align_opts);
            collect_speakers(&mut speakers, &segments);
            channels.push(segments);
        }
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

    // Last cancel gate: a deleted note must not get a transcript saved over
    // its (about to be purged) meeting row.
    if cancelled() {
        return Err(fly_asr::AsrError::Cancelled.to_string());
    }
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

    // 16 kHz intermediates and their ASR checkpoints are pure derived data —
    // drop them once the transcript is saved so meeting folders hold only
    // the real recordings. (A failed run keeps both; the retry resumes from
    // the checkpoint and overwrites the intermediates.)
    for f in &intermediates {
        if let Err(e) = std::fs::remove_file(f) {
            tracing::warn!(path = %f.display(), error = %e, "could not remove 16k intermediate");
        }
        let _ = std::fs::remove_file(fly_asr::checkpoint::checkpoint_path_for(f));
    }

    // release the per-meeting guard (on failure the scheduler clears it)
    state.pipeline_stage.lock().unwrap().remove(meeting_id);
    Ok(transcript)
}

/// What a re-diarize did, for the UI's toast ("N lines re-attributed").
#[derive(Clone, Serialize)]
pub struct ReDiarizeOutcome {
    pub changed_segments: usize,
    pub transcript: Transcript,
}

/// Re-run ONLY diarize → align → save on the existing audio + existing
/// transcript. ASR output and the polished text are never touched: segments
/// keep their ids, boundaries, words, and text — only per-segment speaker
/// keys and the label map change (fly_core::rediarize), mirrored onto the
/// cleaned variant via the shared segment ids. The pre-existing assignment
/// is snapshotted first so `revert_speaker_assignment` can restore it.
/// The caller chains extraction (best-effort) after this returns.
pub async fn re_diarize_with(
    state: &AppState,
    on_stage: StageSink<'_>,
    on_model: models::ProgressSink<'_>,
    meeting_id: &str,
) -> Result<ReDiarizeOutcome, String> {
    // shares the per-meeting guard with the full pipeline
    {
        let stages = state.pipeline_stage.lock().unwrap();
        if stages.contains_key(meeting_id) {
            return Err("a pipeline is already running for this meeting".into());
        }
    }
    let result = re_diarize_inner(state, on_stage, on_model, meeting_id).await;
    state.pipeline_stage.lock().unwrap().remove(meeting_id);
    result
}

async fn re_diarize_inner(
    state: &AppState,
    on_stage: StageSink<'_>,
    on_model: models::ProgressSink<'_>,
    meeting_id: &str,
) -> Result<ReDiarizeOutcome, String> {
    let (meeting, mut raw) = {
        let storage = state.storage.lock().unwrap();
        let meeting = storage.get_meeting(meeting_id).map_err(|e| e.to_string())?;
        let raw = storage
            .get_transcript(meeting_id)
            .map_err(|e| e.to_string())?
            .ok_or("transcribe this meeting before re-analyzing speakers")?;
        (meeting, raw)
    };
    let hint = speaker_hint(&meeting);
    let snapshot = fly_storage::SpeakerSnapshot::capture(&raw);
    let data_dir = state.data_dir.clone();

    let changed: Vec<String> = if hint.just_you() {
        // No engine run: attribute every segment to You.
        emit_stage(state, on_stage, meeting_id, "aligning", None);
        let mut changed = Vec::new();
        for seg in &mut raw.segments {
            if seg.speaker_key != MIC_SPEAKER_KEY {
                changed.push(seg.id.clone());
                seg.speaker_key = MIC_SPEAKER_KEY.to_string();
            }
        }
        raw.speakers = vec![Speaker {
            key: MIC_SPEAKER_KEY.into(),
            label: "You".into(),
        }];
        changed
    } else {
        let recording = meeting
            .recording
            .clone()
            .ok_or("meeting has no recording to re-analyze")?;
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
        let diarizer = fly_diarize::sherpa::SherpaDiarizeEngine {
            exe: sherpa_exe,
            segmentation_model: seg_model,
            embedding_model: emb_model,
            threads: sidecar_threads(),
        };

        let abs = |rel: &Option<String>| -> Option<PathBuf> {
            rel.as_ref()
                .map(|r| data_dir.join(r))
                .filter(|p| p.exists())
        };
        let mut mic_wav = abs(&recording.mic_path);
        let mut system_wav = abs(&recording.system_path);
        let mut mixed_wav = abs(&recording.mixed_path);
        if mic_wav.is_none()
            && system_wav.is_none()
            && mixed_wav.is_none()
            && restore_parked_recording(&data_dir, &recording)
        {
            mic_wav = abs(&recording.mic_path);
            system_wav = abs(&recording.system_path);
            mixed_wav = abs(&recording.mixed_path);
        }
        // Same channel strategy as the full pipeline: with both channels the
        // mic is a known speaker and only the system channel is diarized.
        let per_channel = mic_wav.is_some() && system_wav.is_some();
        let (src, num_clusters) = if per_channel {
            (system_wav.clone().unwrap(), hint.system_clusters())
        } else {
            let src = mixed_wav
                .or(mic_wav)
                .or(system_wav)
                .ok_or_else(|| ERR_NO_RECORDING_FILES.to_string())?;
            (src, hint.mixed_clusters())
        };

        emit_stage(state, on_stage, meeting_id, "preparing-audio", None);
        let work_dir = src
            .parent()
            .map(Path::to_path_buf)
            .ok_or("recording has no parent folder")?;
        let track_16k = work_dir.join("rediarize.16k.wav");
        let (samples, rate) = fly_audio::mix::read_wav_mono(&src).map_err(|e| e.to_string())?;
        let resampled = fly_audio::mix::resample_linear(&samples, rate, 16_000);
        fly_audio::mix::write_wav_mono_16(&track_16k, &resampled, 16_000)
            .map_err(|e| e.to_string())?;

        emit_stage(state, on_stage, meeting_id, "diarizing", None);
        let diarized = diarizer
            .diarize(&track_16k, &diarize_opts(num_clusters))
            .await
            .map_err(|e| e.to_string());
        if let Err(e) = std::fs::remove_file(&track_16k) {
            tracing::warn!(path = %track_16k.display(), error = %e, "could not remove 16k intermediate");
        }
        let turns = fly_diarize::drop_dust_clusters(diarized?);

        emit_stage(state, on_stage, meeting_id, "aligning", None);
        let protected = per_channel.then_some(MIC_SPEAKER_KEY);
        fly_core::rediarize::apply_turns(&mut raw, &turns, protected).changed
    };

    emit_stage(state, on_stage, meeting_id, "saving", None);
    {
        let storage = state.storage.lock().unwrap();
        storage.save_transcript(&raw).map_err(|e| e.to_string())?;
        // Mirror the new assignment onto the polished variant: it shares the
        // raw's segment ids, so its text stays exactly as polished.
        if let Some(mut cleaned) = storage
            .get_cleaned_transcript(meeting_id)
            .map_err(|e| e.to_string())?
        {
            let keys: std::collections::HashMap<&str, &str> = raw
                .segments
                .iter()
                .map(|s| (s.id.as_str(), s.speaker_key.as_str()))
                .collect();
            for seg in &mut cleaned.segments {
                if let Some(k) = keys.get(seg.id.as_str()) {
                    seg.speaker_key = (*k).to_string();
                }
            }
            cleaned.speakers = raw.speakers.clone();
            storage
                .save_cleaned_transcript(&cleaned)
                .map_err(|e| e.to_string())?;
        }
        let mut snap = snapshot;
        snap.changed_segments = changed.len();
        storage
            .save_speaker_snapshot(meeting_id, &snap)
            .map_err(|e| e.to_string())?;
    }
    Ok(ReDiarizeOutcome {
        changed_segments: changed.len(),
        transcript: raw,
    })
}

/// User-facing family of an ASR tier, threaded through progress details so
/// the transcribing stage always says which engine is decoding ("cloud 42%",
/// "GPU 42%", "CPU 42%") — the incident UI showed nothing, and the user
/// assumed a stuck run was parallel cloud work.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TierKind {
    Cloud,
    Gpu,
    Cpu,
}

/// Classify a tier for progress labels: cloud engines by the trait, local
/// ones by the chain label (only GPU tiers carry one — "whisper.cpp-vulkan"
/// / "whisper.cpp-metal"; the trait id can't tell the builds apart).
fn tier_kind(tier: &AsrTier) -> TierKind {
    if !tier.engine.is_local() {
        TierKind::Cloud
    } else if tier
        .label
        .is_some_and(|l| l.contains("vulkan") || l.contains("metal"))
    {
        TierKind::Gpu
    } else {
        TierKind::Cpu
    }
}

/// The "transcribing" stage detail for one progress event: engine family +
/// percent, the channel label around it when one exists, and an explicit
/// quota-wait notice instead of a fake spinner while the cloud pacer sleeps.
/// `None` when there is no work to report a fraction of.
fn transcribe_detail(
    kind: TierKind,
    channel: Option<&str>,
    p: fly_asr::TranscribeProgress,
) -> Option<String> {
    if p.total_ms == 0 {
        return None;
    }
    let pct = (p.done_ms * 100 / p.total_ms).min(100);
    let engine = match kind {
        TierKind::Cloud => "cloud",
        TierKind::Gpu => "GPU",
        TierKind::Cpu => "CPU",
    };
    // partial minutes round up — never promise less waiting than reality
    let wait_m = p.quota_wait_ms.map(|w| w.div_ceil(60_000).max(1));
    Some(match (channel, wait_m) {
        (None, None) => format!("{engine} {pct}%"),
        (None, Some(m)) => format!("{engine} {pct}%, waiting for cloud quota (~{m}m)"),
        (Some(c), None) => format!("{c} ({engine} {pct}%)"),
        (Some(c), Some(m)) => {
            format!("{c} ({engine} {pct}%, waiting for cloud quota ~{m}m)")
        }
    })
}

/// One engine in the pipeline's fallback chain.
struct AsrTier {
    engine: Box<dyn TranscriptionEngine>,
    /// `Transcript::engine` label while this tier is active — the trait
    /// `id()` can't tell the Vulkan build from the CPU one (same engine).
    label: Option<&'static str>,
    /// Set when this tier decodes on the GPU: a runtime failure of THIS tier
    /// (never of a cloud tier above it) pins the machine back to CPU for
    /// this model.
    gpu_model_id: Option<String>,
}

/// The pipeline's ASR with automatic, visible fallback down a chain of
/// engines (cloud → GPU-local → CPU-local): a tier that fails to launch,
/// exits nonzero, OOMs, or exhausts its cloud retries must never sink the
/// pipeline while a tier below it can still decode. The failed batch is
/// retried on the next tier and the rest of the run stays there; a GPU
/// tier's failure is reported via `runtime_failure` so the machine gets
/// pinned back to CPU for future meetings.
struct GuardedAsr {
    tiers: Vec<AsrTier>,
    active: std::sync::Mutex<usize>,
    /// (model_id, error) recorded when a GPU tier failed during this run.
    gpu_failure: std::sync::Mutex<Option<(String, String)>>,
}

impl GuardedAsr {
    #[cfg(test)]
    fn single(engine: Box<dyn TranscriptionEngine>) -> Self {
        Self::chain(vec![AsrTier {
            engine,
            label: None,
            gpu_model_id: None,
        }])
    }

    #[cfg(test)]
    fn with_cpu_fallback(
        primary: Box<dyn TranscriptionEngine>,
        primary_label: &'static str,
        cpu: Box<dyn TranscriptionEngine>,
        gpu_model_id: Option<String>,
    ) -> Self {
        Self::chain(vec![
            AsrTier {
                engine: primary,
                label: Some(primary_label),
                gpu_model_id,
            },
            AsrTier {
                engine: cpu,
                label: None,
                gpu_model_id: None,
            },
        ])
    }

    fn chain(tiers: Vec<AsrTier>) -> Self {
        assert!(!tiers.is_empty(), "GuardedAsr needs at least one engine");
        Self {
            tiers,
            active: std::sync::Mutex::new(0),
            gpu_failure: std::sync::Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn failed_over(&self) -> bool {
        *self.active.lock().unwrap() > 0
    }

    /// What actually transcribed this meeting, for `Transcript::engine`.
    fn engine_id(&self) -> String {
        let tier = &self.tiers[*self.active.lock().unwrap()];
        tier.label.unwrap_or(tier.engine.id()).to_string()
    }

    /// (model_id, error) when a GPU tier failed during this run.
    fn runtime_failure(&self) -> Option<(String, String)> {
        self.gpu_failure.lock().unwrap().clone()
    }

    async fn transcribe(
        &self,
        wav: &Path,
        opts: &TranscribeOptions,
        notify: &(dyn Fn(String) + Send + Sync),
        on_progress: &(dyn Fn(TierKind, fly_asr::TranscribeProgress) + Send + Sync),
        cancel: fly_asr::CancelFn<'_>,
    ) -> Result<RawTranscript, String> {
        let mut fell_over_this_call = false;
        loop {
            let idx = *self.active.lock().unwrap();
            let tier = &self.tiers[idx];
            let kind = tier_kind(tier);
            // After a mid-call failover the next engine emits an immediate
            // 0% event; letting it through would clobber the failover notice
            // just shown. Progress resumes with the first batch.
            let after_first_batch = move |p: fly_asr::TranscribeProgress| {
                if !fell_over_this_call || p.done_ms > 0 {
                    on_progress(kind, p);
                }
            };
            let err = match tier
                .engine
                .transcribe_with_progress(wav, opts, &after_first_batch, cancel)
                .await
            {
                Ok(raw) => return Ok(raw),
                Err(e) => e,
            };
            let msg = err.to_string();
            // An abort the user asked for is not a tier failure — falling
            // over would restart the work the cancel was meant to stop.
            if matches!(err, fly_asr::AsrError::Cancelled) {
                return Err(msg);
            }
            if let Some(model_id) = &tier.gpu_model_id {
                *self.gpu_failure.lock().unwrap() = Some((model_id.clone(), msg.clone()));
            }
            if idx + 1 >= self.tiers.len() {
                return Err(msg);
            }
            let ui = if tier.engine.is_local() {
                "GPU transcription failed — continuing on CPU"
            } else {
                "Cloud transcription failed — continuing with local transcription"
            };
            tracing::warn!(error = %msg, "{ui}");
            notify(ui.into());
            *self.active.lock().unwrap() = idx + 1;
            fell_over_this_call = true;
        }
    }
}

/// Which OS shape the local-engine chain takes (parameterized so every
/// platform's plan is testable from any platform — only the current target's
/// variant is constructed outside tests).
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Debug)]
enum LocalOs {
    Windows,
    MacOs,
    Other,
}

const fn current_os() -> LocalOs {
    #[cfg(target_os = "windows")]
    {
        LocalOs::Windows
    }
    #[cfg(target_os = "macos")]
    {
        LocalOs::MacOs
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        LocalOs::Other
    }
}

/// Blueprint for one local whisper tier (pure data so the plan is testable
/// without touching disk or sidecars).
#[derive(Debug, PartialEq)]
struct LocalTierSpec {
    exe: PathBuf,
    force_cpu: bool,
    label: Option<&'static str>,
    /// Model to pin back to CPU if this (GPU) tier fails at runtime.
    gpu_pin: Option<String>,
}

/// The local whisper tiers for this machine, in fallback order. Used by BOTH
/// the local path and the Groq fallback, so enabling cloud transcription
/// never silently forfeits a GPU the benchmark already validated (the bug
/// behind the 11-hour CPU passes on a Vulkan-capable machine).
///
/// `gpu_exe` is the benchmark-approved Vulkan build (Windows only; `None`
/// when the switch is off, the verdict said CPU, or the build is missing).
fn local_tier_specs(
    os: LocalOs,
    gpu_exe: Option<PathBuf>,
    cpu_exe: &Path,
    model_id: &str,
    use_gpu: bool,
    pinned_cpu: bool,
) -> Vec<LocalTierSpec> {
    match os {
        LocalOs::Windows => match gpu_exe {
            Some(exe) => vec![
                LocalTierSpec {
                    exe,
                    force_cpu: false,
                    label: Some("whisper.cpp-vulkan"),
                    gpu_pin: Some(model_id.to_string()),
                },
                LocalTierSpec {
                    exe: cpu_exe.to_path_buf(),
                    force_cpu: false,
                    label: None,
                    gpu_pin: None,
                },
            ],
            // On a GPU-capable build honor the off switch; the Windows CPU
            // build has no GPU backend and ignores it.
            None => vec![LocalTierSpec {
                exe: cpu_exe.to_path_buf(),
                force_cpu: !use_gpu,
                label: None,
                gpu_pin: None,
            }],
        },
        // macOS: whisper.cpp defaults to Metal, and on GPUs that Metal can't
        // actually serve (e.g. Intel-era Macs) ggml's Metal init aborts with
        // SIGABRT — that crash must not sink the meeting. Run Metal as a
        // guarded tier with a forced-CPU tier below, honoring a prior
        // runtime-failure pin so later meetings skip the doomed attempt.
        LocalOs::MacOs => {
            if use_gpu && !pinned_cpu {
                vec![
                    LocalTierSpec {
                        exe: cpu_exe.to_path_buf(),
                        force_cpu: false,
                        label: Some("whisper.cpp-metal"),
                        gpu_pin: Some(model_id.to_string()),
                    },
                    LocalTierSpec {
                        exe: cpu_exe.to_path_buf(),
                        force_cpu: true,
                        label: None,
                        gpu_pin: None,
                    },
                ]
            } else {
                vec![LocalTierSpec {
                    exe: cpu_exe.to_path_buf(),
                    force_cpu: true,
                    label: None,
                    gpu_pin: None,
                }]
            }
        }
        LocalOs::Other => vec![LocalTierSpec {
            exe: cpu_exe.to_path_buf(),
            force_cpu: !use_gpu,
            label: None,
            gpu_pin: None,
        }],
    }
}

/// Resolve the local whisper fallback chain for this machine: on Windows the
/// benchmark-gated Vulkan build leads when approved (running the one-time
/// speed test if there is no verdict yet), macOS runs guarded Metal, and the
/// CPU build always anchors the chain. Shared by the local path and the Groq
/// fallback so both see the same hardware. Also returns a second instance of
/// the chain's best engine for the cloud tier's quota spillover (resume off:
/// those one-off chunk decodes are recorded in the cloud checkpoint, a
/// nested whisper checkpoint beside a temp wav would be junk).
#[allow(clippy::too_many_arguments)]
async fn local_tiers(
    state: &AppState,
    on_stage: StageSink<'_>,
    on_model: models::ProgressSink<'_>,
    meeting_id: &str,
    cpu_exe: PathBuf,
    model_path: PathBuf,
    model_id: &str,
    threads: usize,
    opts: &TranscribeOptions,
    sample_src: &Path,
    use_gpu: bool,
) -> (Vec<AsrTier>, Option<Box<dyn TranscriptionEngine>>) {
    let pinned_cpu = {
        let storage = state.storage.lock().unwrap();
        cpu_pinned_for_model(&storage, model_id)
    };
    #[cfg(target_os = "windows")]
    let gpu_exe: Option<PathBuf> = if use_gpu {
        emit_stage(state, on_stage, meeting_id, "benchmarking", None);
        let notify = |detail: String| {
            emit_stage(state, on_stage, meeting_id, "benchmarking", Some(detail));
        };
        gpu::plan(
            state,
            on_model,
            &notify,
            gpu::PlanRequest {
                cpu_exe: &cpu_exe,
                model_path: &model_path,
                model_id,
                threads,
                opts,
                sample_src,
            },
        )
        .await
    } else {
        None
    };
    #[cfg(not(target_os = "windows"))]
    let gpu_exe: Option<PathBuf> = {
        let _ = (on_stage, on_model, meeting_id, opts, sample_src);
        None
    };

    // Intel Macs never get the Metal tier: the PR #37 smoke test showed
    // Metal on an AMD GPU silently corrupting output (exit 0, one bogus
    // segment) — a failure mode no runtime crash guard can see. Runtime
    // sysctl, not cfg!(target_arch): the shipped binary is universal
    // (see gpu::is_apple_silicon). Apple Silicon keeps guarded Metal with
    // the same runtime-failure pin semantics as the Windows Vulkan tier.
    #[cfg(target_os = "macos")]
    let use_gpu = use_gpu && gpu::is_apple_silicon();
    let specs = local_tier_specs(
        current_os(),
        gpu_exe,
        &cpu_exe,
        model_id,
        use_gpu,
        pinned_cpu,
    );
    let spillover = specs.first().map(|spec| {
        Box::new(fly_asr::whisper_cpp::WhisperCppEngine {
            exe: spec.exe.clone(),
            model: model_path.clone(),
            threads,
            force_cpu: spec.force_cpu,
            resume: false,
        }) as Box<dyn TranscriptionEngine>
    });
    let tiers = specs
        .into_iter()
        .map(|spec| AsrTier {
            engine: Box::new(fly_asr::whisper_cpp::WhisperCppEngine {
                exe: spec.exe,
                model: model_path.clone(),
                threads,
                force_cpu: spec.force_cpu,
                // completed batches survive crashes, restarts, and failovers —
                // the CPU tier resumes exactly where a failed GPU tier stopped
                resume: true,
            }) as Box<dyn TranscriptionEngine>,
            label: spec.label,
            gpu_model_id: spec.gpu_pin,
        })
        .collect();
    (tiers, spillover)
}

/// Whether a stored GPU verdict pins this machine (and model) to CPU — a
/// benchmark that measured CPU faster, or a recorded GPU runtime failure
/// (e.g. the macOS Metal abort). Invalidated by a model change, like the
/// verdict itself.
fn cpu_pinned_for_model(storage: &fly_storage::Storage, model_id: &str) -> bool {
    gpu::stored(storage).is_some_and(|b| b.verdict == "cpu" && b.model_id == model_id)
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
            playback_path: None,
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

    /// Cloud engine whose every request is rejected (413-style).
    struct RejectingCloud;
    #[async_trait::async_trait]
    impl TranscriptionEngine for RejectingCloud {
        fn id(&self) -> &'static str {
            "groq"
        }
        fn is_local(&self) -> bool {
            false
        }
        async fn transcribe(
            &self,
            _wav: &Path,
            _opts: &TranscribeOptions,
        ) -> fly_asr::Result<RawTranscript> {
            Err(fly_asr::AsrError::Rejected(
                "groq returned 413 Payload Too Large: {}".into(),
            ))
        }
    }

    /// Local engine returning a fixed one-word transcript.
    struct FixedLocal;
    #[async_trait::async_trait]
    impl TranscriptionEngine for FixedLocal {
        fn id(&self) -> &'static str {
            "whisper.cpp"
        }
        fn is_local(&self) -> bool {
            true
        }
        async fn transcribe(
            &self,
            _wav: &Path,
            _opts: &TranscribeOptions,
        ) -> fly_asr::Result<RawTranscript> {
            Ok(RawTranscript {
                language: Some("en".into()),
                words: vec![fly_core::Word {
                    text: "local".into(),
                    start_ms: 0,
                    end_ms: 100,
                }],
                segments: vec![],
            })
        }
    }

    #[tokio::test]
    async fn rejected_cloud_request_falls_back_to_local_engine() {
        let asr = GuardedAsr::with_cpu_fallback(
            Box::new(RejectingCloud),
            "groq",
            Box::new(FixedLocal),
            None,
        );
        let notes = std::sync::Mutex::new(Vec::new());
        let notify = |d: String| notes.lock().unwrap().push(d);
        let raw = asr
            .transcribe(
                Path::new("unused.wav"),
                &TranscribeOptions::default(),
                &notify,
                &|_, _| {},
                &|| false,
            )
            .await
            .expect("fallback must rescue the meeting");
        assert_eq!(raw.words[0].text, "local");
        assert!(asr.failed_over());
        assert_eq!(asr.engine_id(), "whisper.cpp");
        // no GPU model involved — nothing to re-pin
        assert!(asr.runtime_failure().is_none());
        let notes = notes.lock().unwrap();
        assert!(
            notes[0].contains("Cloud transcription failed"),
            "user-visible detail should name the cloud, got: {notes:?}"
        );
    }

    /// Local engine that always fails (a Vulkan build whose init aborts).
    struct FailingLocalGpu;
    #[async_trait::async_trait]
    impl TranscriptionEngine for FailingLocalGpu {
        fn id(&self) -> &'static str {
            "whisper.cpp"
        }
        fn is_local(&self) -> bool {
            true
        }
        async fn transcribe(
            &self,
            _wav: &Path,
            _opts: &TranscribeOptions,
        ) -> fly_asr::Result<RawTranscript> {
            Err(fly_asr::AsrError::Engine(
                "whisper-cli exited with signal: vulkan init failed".into(),
            ))
        }
    }

    fn tier(
        engine: Box<dyn TranscriptionEngine>,
        label: Option<&'static str>,
        gpu_model_id: Option<&str>,
    ) -> AsrTier {
        AsrTier {
            engine,
            label,
            gpu_model_id: gpu_model_id.map(str::to_string),
        }
    }

    /// The incident fix: with Groq enabled, a cloud failure must land on the
    /// GPU-local tier (not a CPU-only engine), and a cloud failure alone
    /// must never record a GPU pin.
    #[tokio::test]
    async fn cloud_failure_lands_on_gpu_tier_without_pinning() {
        let asr = GuardedAsr::chain(vec![
            tier(Box::new(RejectingCloud), Some("groq"), None),
            tier(
                Box::new(FixedLocal),
                Some("whisper.cpp-vulkan"),
                Some("large-v3"),
            ),
            tier(Box::new(FixedLocal), None, None),
        ]);
        let notes = std::sync::Mutex::new(Vec::new());
        let notify = |d: String| notes.lock().unwrap().push(d);
        let raw = asr
            .transcribe(
                Path::new("unused.wav"),
                &TranscribeOptions::default(),
                &notify,
                &|_, _| {},
                &|| false,
            )
            .await
            .expect("the GPU tier must rescue the meeting");
        assert_eq!(raw.words[0].text, "local");
        assert!(asr.failed_over());
        assert_eq!(asr.engine_id(), "whisper.cpp-vulkan");
        assert!(
            asr.runtime_failure().is_none(),
            "a cloud failure must never pin the GPU to CPU"
        );
        let notes = notes.lock().unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("Cloud transcription failed"));
    }

    /// Chain walks all the way down: cloud fails, the GPU tier fails too,
    /// CPU decodes — and only the GPU tier's failure is recorded for the pin.
    #[tokio::test]
    async fn gpu_tier_failure_falls_to_cpu_and_records_the_pin() {
        let asr = GuardedAsr::chain(vec![
            tier(Box::new(RejectingCloud), Some("groq"), None),
            tier(
                Box::new(FailingLocalGpu),
                Some("whisper.cpp-vulkan"),
                Some("large-v3"),
            ),
            tier(Box::new(FixedLocal), None, None),
        ]);
        let notes = std::sync::Mutex::new(Vec::new());
        let notify = |d: String| notes.lock().unwrap().push(d);
        let raw = asr
            .transcribe(
                Path::new("unused.wav"),
                &TranscribeOptions::default(),
                &notify,
                &|_, _| {},
                &|| false,
            )
            .await
            .expect("the CPU tier must rescue the meeting");
        assert_eq!(raw.words[0].text, "local");
        assert_eq!(asr.engine_id(), "whisper.cpp");
        let (model, error) = asr.runtime_failure().expect("GPU failure must pin");
        assert_eq!(model, "large-v3");
        assert!(error.contains("vulkan init failed"));
        let notes = notes.lock().unwrap();
        assert!(notes[0].contains("Cloud transcription failed"));
        assert!(notes[1].contains("GPU transcription failed"));
    }

    /// The Windows plan: a benchmark-approved Vulkan build leads the chain
    /// (with its model pinned for runtime failures); without one the single
    /// CPU tier honors the GPU switch.
    #[test]
    fn windows_tier_specs_put_the_vulkan_build_first() {
        let specs = local_tier_specs(
            LocalOs::Windows,
            Some(PathBuf::from("vulkan/whisper-cli.exe")),
            Path::new("cpu/whisper-cli.exe"),
            "large-v3",
            true,
            false,
        );
        assert_eq!(
            specs,
            vec![
                LocalTierSpec {
                    exe: PathBuf::from("vulkan/whisper-cli.exe"),
                    force_cpu: false,
                    label: Some("whisper.cpp-vulkan"),
                    gpu_pin: Some("large-v3".into()),
                },
                LocalTierSpec {
                    exe: PathBuf::from("cpu/whisper-cli.exe"),
                    force_cpu: false,
                    label: None,
                    gpu_pin: None,
                },
            ]
        );

        let specs = local_tier_specs(
            LocalOs::Windows,
            None,
            Path::new("cpu/whisper-cli.exe"),
            "large-v3",
            false,
            false,
        );
        assert_eq!(
            specs,
            vec![LocalTierSpec {
                exe: PathBuf::from("cpu/whisper-cli.exe"),
                force_cpu: true,
                label: None,
                gpu_pin: None,
            }]
        );
    }

    /// The macOS plan mirrors the old inline logic: Metal guarded by a
    /// forced-CPU tier, collapsing to CPU-only once pinned (or switched off).
    #[test]
    fn macos_tier_specs_guard_metal_and_honor_the_pin() {
        let cpu = Path::new("whisper-cli");
        let metal = local_tier_specs(LocalOs::MacOs, None, cpu, "small-q5", true, false);
        assert_eq!(metal.len(), 2);
        assert_eq!(metal[0].label, Some("whisper.cpp-metal"));
        assert!(!metal[0].force_cpu);
        assert_eq!(metal[0].gpu_pin.as_deref(), Some("small-q5"));
        assert!(metal[1].force_cpu);

        for pinned_or_off in [
            local_tier_specs(LocalOs::MacOs, None, cpu, "small-q5", true, true),
            local_tier_specs(LocalOs::MacOs, None, cpu, "small-q5", false, false),
        ] {
            assert_eq!(pinned_or_off.len(), 1);
            assert!(pinned_or_off[0].force_cpu);
            assert!(pinned_or_off[0].gpu_pin.is_none());
        }
    }

    /// A stored CPU pin (benchmark verdict or recorded runtime failure) is
    /// per-model and feeds the tier plan, which then keeps a doomed GPU
    /// attempt out of every chain — including the Groq fallback.
    #[test]
    fn pinned_cpu_verdict_tracks_the_model_it_was_recorded_for() {
        let dir = tempfile::tempdir().unwrap();
        let storage = fly_storage::Storage::open(dir.path()).unwrap();

        assert!(!cpu_pinned_for_model(&storage, "small-q5"));
        gpu::record_runtime_failure(&storage, "small-q5", "ggml_metal_init: error");
        assert!(cpu_pinned_for_model(&storage, "small-q5"));
        // A different model invalidates the pin (same rule as gpu::plan).
        assert!(!cpu_pinned_for_model(&storage, "large-v3"));

        // ...and the pin collapses the macOS chain to forced CPU.
        let specs = local_tier_specs(
            LocalOs::MacOs,
            None,
            Path::new("whisper-cli"),
            "small-q5",
            true,
            cpu_pinned_for_model(&storage, "small-q5"),
        );
        assert_eq!(specs.len(), 1);
        assert!(specs[0].force_cpu);
    }

    #[test]
    fn tier_kinds_classify_cloud_gpu_cpu() {
        assert_eq!(
            tier_kind(&tier(Box::new(RejectingCloud), Some("groq"), None)),
            TierKind::Cloud
        );
        assert_eq!(
            tier_kind(&tier(
                Box::new(FixedLocal),
                Some("whisper.cpp-vulkan"),
                Some("large-v3")
            )),
            TierKind::Gpu
        );
        assert_eq!(
            tier_kind(&tier(
                Box::new(FixedLocal),
                Some("whisper.cpp-metal"),
                Some("small-q5")
            )),
            TierKind::Gpu
        );
        assert_eq!(
            tier_kind(&tier(Box::new(FixedLocal), None, None)),
            TierKind::Cpu
        );
    }

    /// The "transcribing" details the frontend parses: engine family +
    /// percent, wrapped in the channel label when one exists.
    #[test]
    fn transcribe_details_name_the_engine_and_percent() {
        let p = |done, total| fly_asr::TranscribeProgress {
            done_ms: done,
            total_ms: total,
            quota_wait_ms: None,
        };
        assert_eq!(
            transcribe_detail(TierKind::Cloud, None, p(42, 100)),
            Some("cloud 42%".into())
        );
        assert_eq!(
            transcribe_detail(TierKind::Gpu, None, p(1, 3)),
            Some("GPU 33%".into())
        );
        assert_eq!(
            transcribe_detail(TierKind::Cpu, Some("your microphone"), p(50, 100)),
            Some("your microphone (CPU 50%)".into())
        );
        assert_eq!(
            transcribe_detail(TierKind::Cloud, None, p(0, 0)),
            None,
            "no speech, no fraction to report"
        );
    }

    /// While the cloud pacer waits, the detail says so with an ETA — the UI
    /// must never show a fake spinner over a deliberate quota sleep.
    #[test]
    fn quota_wait_details_replace_the_spinner() {
        let w = |wait_ms| fly_asr::TranscribeProgress {
            done_ms: 40,
            total_ms: 100,
            quota_wait_ms: Some(wait_ms),
        };
        assert_eq!(
            transcribe_detail(TierKind::Cloud, None, w(180_000)),
            Some("cloud 40%, waiting for cloud quota (~3m)".into())
        );
        // partial minutes round up — never promise less waiting than reality
        assert_eq!(
            transcribe_detail(TierKind::Cloud, None, w(61_000)),
            Some("cloud 40%, waiting for cloud quota (~2m)".into())
        );
        assert_eq!(
            transcribe_detail(TierKind::Cloud, None, w(5_000)),
            Some("cloud 40%, waiting for cloud quota (~1m)".into())
        );
        assert_eq!(
            transcribe_detail(TierKind::Cloud, Some("other participants"), w(120_000)),
            Some("other participants (cloud 40%, waiting for cloud quota ~2m)".into())
        );
    }

    /// Progress events carry the tier that is ACTUALLY decoding: after a
    /// cloud failover the same run's events switch to the GPU tier's kind.
    #[tokio::test]
    async fn progress_events_carry_the_active_tier_kind() {
        struct ProgressLocal;
        #[async_trait::async_trait]
        impl TranscriptionEngine for ProgressLocal {
            fn id(&self) -> &'static str {
                "whisper.cpp"
            }
            fn is_local(&self) -> bool {
                true
            }
            async fn transcribe(
                &self,
                _wav: &Path,
                _opts: &TranscribeOptions,
            ) -> fly_asr::Result<RawTranscript> {
                unreachable!("transcribe_with_progress is overridden")
            }
            async fn transcribe_with_progress(
                &self,
                _wav: &Path,
                _opts: &TranscribeOptions,
                on_progress: fly_asr::TranscribeProgressFn<'_>,
                _cancel: fly_asr::CancelFn<'_>,
            ) -> fly_asr::Result<RawTranscript> {
                on_progress(fly_asr::TranscribeProgress {
                    done_ms: 50,
                    total_ms: 100,
                    quota_wait_ms: None,
                });
                Ok(RawTranscript {
                    language: None,
                    words: vec![],
                    segments: vec![],
                })
            }
        }

        let asr = GuardedAsr::chain(vec![
            tier(Box::new(RejectingCloud), Some("groq"), None),
            tier(
                Box::new(ProgressLocal),
                Some("whisper.cpp-vulkan"),
                Some("large-v3"),
            ),
        ]);
        let events = std::sync::Mutex::new(Vec::new());
        asr.transcribe(
            Path::new("unused.wav"),
            &TranscribeOptions::default(),
            &|_| {},
            &|kind, p| events.lock().unwrap().push((kind, p.done_ms)),
            &|| false,
        )
        .await
        .expect("the GPU tier must rescue the meeting");
        assert_eq!(
            events.into_inner().unwrap(),
            vec![(TierKind::Gpu, 50)],
            "events after the failover must be labeled with the ACTIVE (GPU) tier"
        );
    }

    /// A user-requested abort must not fail over to the next tier — that
    /// would restart the very work the cancel was meant to stop.
    #[tokio::test]
    async fn cancelled_run_does_not_fail_over() {
        struct CancelledCloud;
        #[async_trait::async_trait]
        impl TranscriptionEngine for CancelledCloud {
            fn id(&self) -> &'static str {
                "groq"
            }
            fn is_local(&self) -> bool {
                false
            }
            async fn transcribe(
                &self,
                _wav: &Path,
                _opts: &TranscribeOptions,
            ) -> fly_asr::Result<RawTranscript> {
                Err(fly_asr::AsrError::Cancelled)
            }
        }

        let asr = GuardedAsr::with_cpu_fallback(
            Box::new(CancelledCloud),
            "groq",
            Box::new(FixedLocal),
            None,
        );
        let notes = std::sync::Mutex::new(Vec::new());
        let notify = |d: String| notes.lock().unwrap().push(d);
        let err = asr
            .transcribe(
                Path::new("unused.wav"),
                &TranscribeOptions::default(),
                &notify,
                &|_, _| {},
                &|| true,
            )
            .await
            .unwrap_err();
        assert!(
            err.contains(fly_asr::CANCELLED_MARKER),
            "scheduler must see the cancel marker, got: {err}"
        );
        assert!(!asr.failed_over(), "a cancel is not a tier failure");
        assert!(
            notes.lock().unwrap().is_empty(),
            "no failover notice for a cancel"
        );
    }

    #[tokio::test]
    async fn rejected_cloud_without_fallback_surfaces_marker_for_scheduler() {
        let asr = GuardedAsr::single(Box::new(RejectingCloud));
        let err = asr
            .transcribe(
                Path::new("unused.wav"),
                &TranscribeOptions::default(),
                &|_| {},
                &|_, _| {},
                &|| false,
            )
            .await
            .unwrap_err();
        assert!(
            err.contains(fly_asr::REJECTED_MARKER),
            "scheduler must see the non-retryable marker, got: {err}"
        );
    }
}
