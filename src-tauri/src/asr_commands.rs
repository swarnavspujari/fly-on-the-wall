//! Commands for transcription, transcripts, ASR settings, and models.

use looma_core::Transcript;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::state::AppState;
use crate::{hw, models, scheduler};

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
    pub hw: hw::HwInfo,
    pub models: Vec<ModelStatus>,
}

#[tauri::command]
pub fn get_asr_settings(state: State<'_, AppState>) -> CmdResult<AsrSettings> {
    let hw_info = hw::detect();
    let storage = state.storage.lock().unwrap();
    let get = |k: &str| storage.get_setting(k).ok().flatten();

    let models = models::registry()
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
            .get(looma_secrets::keys::GROQ_API_KEY)
            .ok()
            .flatten()
            .is_some(),
        auto_transcribe: get("asr.auto_transcribe").as_deref() != Some("false"),
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
    }
    if let Some(key) = update.groq_key {
        if key.is_empty() {
            state
                .secrets
                .delete(looma_secrets::keys::GROQ_API_KEY)
                .map_err(|e| e.to_string())?;
        } else {
            state
                .secrets
                .set(looma_secrets::keys::GROQ_API_KEY, &key)
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
