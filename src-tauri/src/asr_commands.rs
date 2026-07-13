//! Commands for transcription, transcripts, ASR settings, and models.

use fly_core::Transcript;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::state::AppState;
use crate::{gpu, hw, models, scheduler};

type CmdResult<T> = Result<T, String>;

/// Queue the pipeline for a meeting — it runs as soon as no recording is
/// active (recording always wins). Progress arrives via `pipeline:progress`
/// events and `pipeline_stage` ("waiting" while queued).
#[tauri::command]
pub fn transcribe_meeting(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
) -> CmdResult<()> {
    scheduler::enqueue(&state, &scheduler::stage_emitter(&app), &meeting_id)
}

#[tauri::command]
pub fn get_transcript(
    state: State<'_, AppState>,
    meeting_id: String,
) -> CmdResult<Option<Transcript>> {
    state
        .storage
        .lock()
        .unwrap()
        .get_transcript(&meeting_id)
        .map_err(|e| e.to_string())
}

/// The LLM-polished transcript variant, if the polish pass has run for this
/// meeting (else `None`). The raw transcript is always available via
/// `get_transcript`; the UI toggles between the two.
#[tauri::command]
pub fn get_cleaned_transcript(
    state: State<'_, AppState>,
    meeting_id: String,
) -> CmdResult<Option<Transcript>> {
    state
        .storage
        .lock()
        .unwrap()
        .get_cleaned_transcript(&meeting_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn relabel_speaker(
    state: State<'_, AppState>,
    meeting_id: String,
    speaker_key: String,
    label: String,
) -> CmdResult<Transcript> {
    state
        .storage
        .lock()
        .unwrap()
        .relabel_speaker(&meeting_id, &speaker_key, &label)
        .map_err(|e| e.to_string())
}

/// Persist an edit to a transcript line's text (returns the updated transcript).
#[tauri::command]
pub fn edit_transcript_segment(
    state: State<'_, AppState>,
    meeting_id: String,
    segment_id: String,
    text: String,
) -> CmdResult<Transcript> {
    state
        .storage
        .lock()
        .unwrap()
        .edit_segment_text(&meeting_id, &segment_id, &text)
        .map_err(|e| e.to_string())
}

/// Current stage of a running pipeline (None = not running).
#[tauri::command]
pub fn pipeline_stage(state: State<'_, AppState>, meeting_id: String) -> Option<String> {
    state
        .pipeline_stage
        .lock()
        .unwrap()
        .get(&meeting_id)
        .cloned()
}

// ---------------------------------------------------------------------------
// Settings & models
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ModelStatus {
    pub id: String,
    pub display: String,
    pub bytes: u64,
    pub installed: bool,
}

#[derive(Serialize)]
pub struct AsrSettings {
    pub tier: String,
    pub model_id: Option<String>,
    pub use_groq: bool,
    pub max_quality: bool,
    pub has_groq_key: bool,
    pub auto_transcribe: bool,
    pub use_gpu: bool,
    /// This machine's one-time GPU-vs-CPU benchmark verdict, if it ran.
    pub gpu_bench: Option<gpu::GpuBench>,
    pub hw: hw::HwInfo,
    pub models: Vec<ModelStatus>,
}

/// Async so it never queues behind (or in front of) other startup commands
/// on the main thread; the hardware profile comes from the persisted cache,
/// so nvidia-smi only ever runs here on the very first launch — off the IPC
/// thread — before the background warm-up (lib.rs) has landed.
#[tauri::command]
pub async fn get_asr_settings(state: State<'_, AppState>) -> CmdResult<AsrSettings> {
    let cached = {
        let storage = state.storage.lock().unwrap();
        hw::cached(&storage)
    };
    let hw_info = match cached {
        Some(info) => info,
        None => {
            let info = tauri::async_runtime::spawn_blocking(hw::detect)
                .await
                .map_err(|e| e.to_string())?;
            if let Ok(json) = serde_json::to_string(&info) {
                let storage = state.storage.lock().unwrap();
                let _ = storage.set_setting(hw::CACHE_KEY, &json);
            }
            info
        }
    };
    let storage = state.storage.lock().unwrap();
    let get = |k: &str| storage.get_setting(k).ok().flatten();

    let models = models::registry()
        // The Ollama runtime belongs to the AI-provider section, not the
        // transcription models list.
        .filter(|a| a.id != crate::ollama::ARTIFACT_ID)
        .map(|a| ModelStatus {
            id: a.id.to_string(),
            display: a.display.to_string(),
            bytes: a.bytes,
            installed: models::installed_path(&state.data_dir, a).is_some(),
        })
        .collect();

    Ok(AsrSettings {
        tier: get("asr.tier").unwrap_or_else(|| hw_info.recommended_tier.clone()),
        model_id: get("asr.model_id").filter(|s| !s.is_empty()),
        use_groq: get("asr.use_groq").as_deref() == Some("true"),
        max_quality: get("asr.max_quality").as_deref() == Some("true"),
        has_groq_key: state
            .secrets
            .get(fly_secrets::keys::GROQ_API_KEY)
            .ok()
            .flatten()
            .is_some(),
        auto_transcribe: get("asr.auto_transcribe").as_deref() != Some("false"),
        use_gpu: gpu::enabled(&storage),
        gpu_bench: gpu::stored(&storage),
        hw: hw_info,
        models,
    })
}

#[derive(Deserialize)]
pub struct AsrSettingsUpdate {
    pub tier: String,
    pub model_id: Option<String>,
    pub use_groq: bool,
    pub max_quality: bool,
    pub auto_transcribe: bool,
    pub use_gpu: bool,
    /// Some("") clears the stored key; None leaves it untouched.
    pub groq_key: Option<String>,
}

#[tauri::command]
pub fn set_asr_settings(state: State<'_, AppState>, update: AsrSettingsUpdate) -> CmdResult<()> {
    {
        let storage = state.storage.lock().unwrap();
        storage
            .set_setting("asr.tier", &update.tier)
            .map_err(|e| e.to_string())?;
        match &update.model_id {
            Some(m) => storage
                .set_setting("asr.model_id", m)
                .map_err(|e| e.to_string())?,
            None => storage
                .set_setting("asr.model_id", "")
                .map(|_| ())
                .map_err(|e| e.to_string())?,
        }
        storage
            .set_setting(
                "asr.use_groq",
                if update.use_groq { "true" } else { "false" },
            )
            .map_err(|e| e.to_string())?;
        storage
            .set_setting(
                "asr.max_quality",
                if update.max_quality { "true" } else { "false" },
            )
            .map_err(|e| e.to_string())?;
        storage
            .set_setting(
                "asr.auto_transcribe",
                if update.auto_transcribe {
                    "true"
                } else {
                    "false"
                },
            )
            .map_err(|e| e.to_string())?;
        // Turning GPU off→on is the "try again" gesture: clear the stored
        // benchmark verdict so the next transcription re-measures (a failed
        // or slow GPU verdict would otherwise stick forever).
        let was_on = gpu::enabled(&storage);
        if update.use_gpu && !was_on {
            storage
                .set_setting(gpu::BENCH_KEY, "")
                .map_err(|e| e.to_string())?;
        }
        storage
            .set_setting(
                gpu::USE_GPU_KEY,
                if update.use_gpu { "true" } else { "false" },
            )
            .map_err(|e| e.to_string())?;
    }
    if let Some(key) = update.groq_key {
        if key.is_empty() {
            state
                .secrets
                .delete(fly_secrets::keys::GROQ_API_KEY)
                .map_err(|e| e.to_string())?;
        } else {
            state
                .secrets
                .set(fly_secrets::keys::GROQ_API_KEY, &key)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Pre-download an artifact from Settings (progress via `model:progress`).
#[tauri::command]
pub async fn download_model(app: tauri::AppHandle, id: String) -> CmdResult<String> {
    let data_dir = {
        let state = app.state::<AppState>();
        state.data_dir.clone()
    };
    let on_model = {
        let app = app.clone();
        move |p: models::ModelProgress| {
            use tauri::Emitter;
            let _ = app.emit("model:progress", p);
        }
    };
    let path = models::ensure(&on_model, &data_dir, &id).await?;
    Ok(path.display().to_string())
}
