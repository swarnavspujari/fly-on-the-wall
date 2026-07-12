//! Commands for LLM enhancement, the Ask panel, templates, and provider
//! settings. Provider selection is composition-root logic: settings rows
//! (non-secret) + keychain (keys) → a boxed `LLMProvider`.

use std::collections::HashMap;

use fly_core::{enhance, Note, Template, Transcript};
use fly_llm::{ChatMessage, ChatRequest, LLMProvider, ThinkingMode};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

pub const PROVIDERS: &[(&str, &str, bool)] = &[
    // (id, default model, is_local)
    ("ollama", "llama3.1", true),
    ("openai", "gpt-4o-mini", false),
    ("anthropic", "claude-sonnet-5", false),
    ("nim", "meta/llama-3.1-70b-instruct", false),
];

fn secret_key_for(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some(fly_secrets::keys::OPENAI_API_KEY),
        "anthropic" => Some(fly_secrets::keys::ANTHROPIC_API_KEY),
        "nim" => Some(fly_secrets::keys::NIM_API_KEY),
        _ => None,
    }
}

/// Build the active provider from settings + keychain.
pub fn build_provider(state: &AppState) -> Result<Box<dyn LLMProvider>, String> {
    let storage = state.storage.lock().unwrap();
    let get = |k: &str| {
        storage
            .get_setting(k)
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
    };
    let provider = get("llm.provider").unwrap_or_else(|| "ollama".to_string());
    let default_model = PROVIDERS
        .iter()
        .find(|(id, _, _)| *id == provider)
        .map(|(_, m, _)| m.to_string())
        .unwrap_or_default();
    let model = get(&format!("llm.{provider}.model")).unwrap_or(default_model);
    let base_url = get(&format!("llm.{provider}.base_url"));
    drop(storage);

    let key = |name: &'static str| -> Result<String, String> {
        state
            .secrets
            .get(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no API key stored for {provider} — add one in Settings"))
    };

    Ok(match provider.as_str() {
        "openai" => {
            let mut p = fly_llm::openai_compat::OpenAiCompatProvider::openai(
                key(fly_secrets::keys::OPENAI_API_KEY)?,
                model,
            );
            if let Some(url) = base_url {
                p.base_url = url;
            }
            Box::new(p)
        }
        "nim" => {
            let mut p = fly_llm::openai_compat::OpenAiCompatProvider::nim(
                key(fly_secrets::keys::NIM_API_KEY)?,
                model,
            );
            if let Some(url) = base_url {
                p.base_url = url;
            }
            Box::new(p)
        }
        "anthropic" => Box::new(fly_llm::anthropic::AnthropicProvider::new(
            key(fly_secrets::keys::ANTHROPIC_API_KEY)?,
            model,
        )),
        _ => Box::new(fly_llm::openai_compat::OpenAiCompatProvider::ollama(
            base_url, model,
        )),
    })
}

// ---------------------------------------------------------------------------
// Enhance
// ---------------------------------------------------------------------------

/// Merge scratchpad + transcript into provenance-tagged blocks. Re-running
/// replaces the enhanced document (re-enhance).
#[tauri::command]
pub async fn enhance_note(
    state: State<'_, AppState>,
    note_id: String,
    template_id: String,
) -> CmdResult<Note> {
    let provider = build_provider(&state)?;
    let (prompt, template) = {
        let storage = state.storage.lock().unwrap();
        let note = storage.get_note(&note_id).map_err(|e| e.to_string())?;
        let template = storage
            .get_template(&template_id)
            .map_err(|e| e.to_string())?;
        let transcript = match &note.meeting_id {
            Some(mid) => storage.get_transcript(mid).map_err(|e| e.to_string())?,
            None => None,
        };
        if note.scratchpad.trim().is_empty() && transcript.is_none() {
            return Err("nothing to enhance yet — jot some notes or record a meeting".into());
        }
        (
            enhance::build_enhance_prompt(&note, transcript.as_ref(), &template),
            template,
        )
    };

    tracing::info!(note_id, template = %template.name, provider = provider.id(), "enhancing note");
    let output = provider
        .chat(ChatRequest {
            messages: vec![
                ChatMessage::system(prompt.system.clone()),
                ChatMessage::user(prompt.user.clone()),
            ],
            temperature: Some(0.2),
            max_tokens: Some(4096),
            thinking: ThinkingMode::Default,
        })
        .await
        .map_err(|e| e.to_string())?;

    let blocks = enhance::parse_enhanced_blocks(&output, &prompt.segment_ids);
    state
        .storage
        .lock()
        .unwrap()
        .update_note_blocks(&note_id, &blocks)
        .map_err(|e| e.to_string())
}

/// Edit one enhanced block (AI blocks are reclaimed as user text).
#[tauri::command]
pub fn edit_note_block(
    state: State<'_, AppState>,
    note_id: String,
    block_id: String,
    markdown: String,
) -> CmdResult<Note> {
    state
        .storage
        .lock()
        .unwrap()
        .edit_note_block(&note_id, &block_id, &markdown)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Polish transcript (LLM cleanup pass)
// ---------------------------------------------------------------------------

/// Batching bounds for the cleanup pass. A full meeting rarely fits one
/// non-streaming response, and real segments vary wildly in length (a phone
/// dial-in can produce 800-word run-on segments), so batches are bounded by
/// BOTH a word budget — keeping each response under the token cap — and a
/// segment count. The no-loss guard means a dropped or truncated batch only
/// leaves those segments un-cleaned (raw), never dropped.
const POLISH_MAX_BATCH_WORDS: usize = 1200;
const POLISH_MAX_BATCH_SEGMENTS: usize = 40;

#[derive(Serialize)]
pub struct PolishResult {
    /// The cleaned transcript — identical to the raw one in segment count,
    /// ids, speaker keys, and timestamps; only text differs. Provenance
    /// citations that reference segment ids keep resolving.
    pub transcript: Transcript,
    pub segments_total: usize,
    pub segments_cleaned: usize,
    pub segments_kept_raw: usize,
    /// Segments the drop-content guard refused to clean (kept raw), so the UI
    /// can show which lines couldn't be safely polished — never a silent loss.
    pub flags: Vec<enhance::PolishFlag>,
}

/// Run the stored RAW transcript through the chosen LLM provider to produce a
/// cleaned, readable variant, stored ALONGSIDE the raw one (never overwriting
/// it). Re-runnable from the UI: each run rebuilds the cleaned variant from the
/// raw source. Provider follows the user's selection — default Anthropic
/// (`claude-sonnet-5`); the local Ollama provider is used when the user has
/// selected it (privacy / offline). The cleaned text is mapped back strictly
/// by segment id, so ids / speakers / timestamps are preserved exactly and a
/// lossy or truncated response can only leave segments un-cleaned, not dropped.
#[tauri::command]
pub async fn polish_transcript(
    state: State<'_, AppState>,
    meeting_id: String,
) -> CmdResult<PolishResult> {
    let provider = build_provider(&state)?;
    let raw = {
        let storage = state.storage.lock().unwrap();
        storage
            .get_transcript(&meeting_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                "no transcript to polish yet — transcribe the meeting first".to_string()
            })?
    };
    if raw.segments.is_empty() {
        return Err("transcript has no segments to polish".into());
    }

    tracing::info!(
        meeting_id,
        provider = provider.id(),
        segments = raw.segments.len(),
        "polishing transcript"
    );

    // One provider call per batch; merge cleaned text by segment id.
    let mut cleaned_map: HashMap<String, String> = HashMap::new();
    for range in enhance::plan_cleanup_batches(
        &raw.segments,
        POLISH_MAX_BATCH_WORDS,
        POLISH_MAX_BATCH_SEGMENTS,
    ) {
        let prompt = enhance::build_cleanup_prompt(&raw.segments[range]);
        let output = provider
            .chat(ChatRequest {
                messages: vec![
                    ChatMessage::system(prompt.system),
                    ChatMessage::user(prompt.user),
                ],
                // No temperature: the cleanup contract (no paraphrase/invent)
                // lives in the system prompt, and the default model
                // (claude-sonnet-5) rejects an explicit `temperature` with a
                // 400. Omitting it works across every provider.
                temperature: None,
                max_tokens: Some(8192),
                // Cleanup is mechanical; disable thinking so adaptive reasoning
                // tokens don't eat the budget and truncate the JSON mid-array
                // (claude-sonnet-5 thinks by default — see AnthropicProvider).
                thinking: ThinkingMode::Disabled,
            })
            .await
            .map_err(|e| e.to_string())?;
        for (id, text) in enhance::parse_cleanup_response(&output).unwrap_or_default() {
            cleaned_map.insert(id, text);
        }
    }

    let outcome = enhance::apply_cleanup(&raw, &cleaned_map);
    // The provenance contract is load-bearing (the Enhanced doc cites segment
    // ids): refuse to persist a variant that drifted from the raw structure.
    // `apply_cleanup` guarantees this, so this is a belt-and-suspenders guard
    // against a future regression rather than an expected runtime path.
    if !enhance::preserves_provenance(&raw, &outcome.transcript) {
        return Err("polish produced a structurally different transcript; refusing to save".into());
    }

    state
        .storage
        .lock()
        .unwrap()
        .save_cleaned_transcript(&outcome.transcript)
        .map_err(|e| e.to_string())?;

    Ok(PolishResult {
        segments_total: raw.segments.len(),
        segments_cleaned: outcome.segments_cleaned,
        segments_kept_raw: outcome.segments_kept_raw,
        flags: outcome.flags,
        transcript: outcome.transcript,
    })
}

// ---------------------------------------------------------------------------
// Ask (ephemeral chat grounded in the meeting)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AskMessage {
    pub role: String, // "user" | "assistant"
    pub content: String,
}

#[tauri::command]
pub async fn ask_meeting(
    state: State<'_, AppState>,
    note_id: String,
    history: Vec<AskMessage>,
) -> CmdResult<String> {
    let provider = build_provider(&state)?;
    let context = {
        let storage = state.storage.lock().unwrap();
        let note = storage.get_note(&note_id).map_err(|e| e.to_string())?;
        let transcript = match &note.meeting_id {
            Some(mid) => storage.get_transcript(mid).map_err(|e| e.to_string())?,
            None => None,
        };
        let mut ctx = format!(
            "NOTE TITLE: {}\n\nUSER'S NOTES:\n{}\n",
            note.title, note.scratchpad
        );
        if !note.blocks.is_empty() {
            ctx.push_str("\nENHANCED NOTES:\n");
            for b in &note.blocks {
                ctx.push_str(&b.markdown);
                ctx.push('\n');
            }
        }
        if let Some(t) = &transcript {
            ctx.push_str("\nFULL TRANSCRIPT:\n");
            for seg in &t.segments {
                ctx.push_str(&format!(
                    "{}: {}\n",
                    t.label_for(&seg.speaker_key),
                    seg.text.trim()
                ));
            }
        }
        ctx
    };

    let mut messages = vec![ChatMessage::system(format!(
        "You are Fly on the Wall's meeting assistant. Answer questions about THIS meeting using only the \
         context below. Be concrete and cite who said what when relevant. If the answer is not \
         in the meeting, say so.\n\n{context}"
    ))];
    for m in history {
        messages.push(if m.role == "assistant" {
            ChatMessage::assistant(m.content)
        } else {
            ChatMessage::user(m.content)
        });
    }

    provider
        .chat(ChatRequest {
            messages,
            temperature: Some(0.3),
            max_tokens: Some(2048),
            thinking: ThinkingMode::Default,
        })
        .await
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_templates(state: State<'_, AppState>) -> CmdResult<Vec<Template>> {
    state
        .storage
        .lock()
        .unwrap()
        .list_templates()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_template(state: State<'_, AppState>, template: Template) -> CmdResult<()> {
    let template = if template.id.is_empty() {
        Template {
            id: fly_core::new_id(),
            built_in: false,
            ..template
        }
    } else {
        template
    };
    state
        .storage
        .lock()
        .unwrap()
        .upsert_template(&template)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_template(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state
        .storage
        .lock()
        .unwrap()
        .delete_template(&id)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Provider settings
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct LlmProviderInfo {
    pub id: String,
    pub default_model: String,
    pub is_local: bool,
    pub has_key: bool,
    pub model: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Serialize)]
pub struct LlmSettings {
    pub provider: String,
    pub providers: Vec<LlmProviderInfo>,
}

#[tauri::command]
pub fn get_llm_settings(state: State<'_, AppState>) -> CmdResult<LlmSettings> {
    let storage = state.storage.lock().unwrap();
    let get = |k: &str| {
        storage
            .get_setting(k)
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
    };
    let providers = PROVIDERS
        .iter()
        .map(|(id, default_model, is_local)| LlmProviderInfo {
            id: id.to_string(),
            default_model: default_model.to_string(),
            is_local: *is_local,
            has_key: secret_key_for(id)
                .and_then(|k| state.secrets.get(k).ok().flatten())
                .is_some(),
            model: get(&format!("llm.{id}.model")),
            base_url: get(&format!("llm.{id}.base_url")),
        })
        .collect();
    Ok(LlmSettings {
        provider: get("llm.provider").unwrap_or_else(|| "ollama".to_string()),
        providers,
    })
}

#[derive(Deserialize)]
pub struct LlmSettingsUpdate {
    pub provider: String,
    pub model: Option<String>,
    pub base_url: Option<String>,
    /// Some("") clears the stored key; None leaves it untouched.
    pub api_key: Option<String>,
}

#[tauri::command]
pub fn set_llm_settings(state: State<'_, AppState>, update: LlmSettingsUpdate) -> CmdResult<()> {
    {
        let storage = state.storage.lock().unwrap();
        storage
            .set_setting("llm.provider", &update.provider)
            .map_err(|e| e.to_string())?;
        storage
            .set_setting(
                &format!("llm.{}.model", update.provider),
                update.model.as_deref().unwrap_or(""),
            )
            .map_err(|e| e.to_string())?;
        storage
            .set_setting(
                &format!("llm.{}.base_url", update.provider),
                update.base_url.as_deref().unwrap_or(""),
            )
            .map_err(|e| e.to_string())?;
    }
    if let (Some(key), Some(secret_name)) = (update.api_key, secret_key_for(&update.provider)) {
        if key.is_empty() {
            state
                .secrets
                .delete(secret_name)
                .map_err(|e| e.to_string())?;
        } else {
            state
                .secrets
                .set(secret_name, &key)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn test_llm_connection(state: State<'_, AppState>) -> CmdResult<String> {
    let provider = build_provider(&state)?;
    let id = provider.id();
    provider
        .test_connection()
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("{id} connection OK"))
}
