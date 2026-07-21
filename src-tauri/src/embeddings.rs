//! Semantic-search embedding pipeline: a background worker that drains the
//! `embedding_chunks` backlog through Ollama's `/api/embed`, plus the
//! query-time embed used by hybrid search.
//!
//! Model: EmbeddingGemma 300M — small (~600 MB), multilingual (100+
//! languages), built for on-device retrieval. Pulled automatically through
//! the managed Ollama runtime the first time indexing runs, with progress on
//! the same `model:progress` channel as every other model download.
//!
//! Degradation contract: everything here is best-effort. No Ollama installed,
//! server down, model missing — the worker backs off and retries, chunk rows
//! simply stay pending, and search silently falls back to FTS-only. Nothing
//! in this module ever blocks the UI or fails a user action.

use std::time::Duration;

use serde::Deserialize;
use tauri::Manager;

use crate::ollama;
use crate::state::AppState;

/// Embedding model tag pulled into the managed Ollama runtime.
pub const EMBED_MODEL: &str = "embeddinggemma:300m";

/// Chunks per `/api/embed` call. Small enough to keep each request quick on
/// CPU, large enough to amortize HTTP overhead across a backfill.
const BATCH_SIZE: usize = 16;
/// Idle poll: a missed nudge only delays indexing by this much.
const IDLE_POLL: Duration = Duration::from_secs(30);
/// Backoff after an embed/availability failure (Ollama down, pull failed).
const RETRY_DELAY: Duration = Duration::from_secs(60);

/// EmbeddingGemma's document prompt. The owning note/meeting title rides in
/// the `title:` slot, so "the airline deal meeting" can match a chunk whose
/// text never says "airline". (Pub for the search-quality eval harness.)
pub fn doc_prompt(title: &str, text: &str) -> String {
    let title = title.trim();
    let title = if title.is_empty() { "none" } else { title };
    format!("title: {title} | text: {text}")
}

/// EmbeddingGemma's retrieval query prompt (asymmetric to `doc_prompt`).
pub fn query_prompt(query: &str) -> String {
    format!("task: search result | query: {query}")
}

/// POST /api/embed. Returns one vector per input, in order.
pub async fn embed_raw(root: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
    #[derive(Deserialize)]
    struct Resp {
        embeddings: Vec<Vec<f32>>,
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;
    let resp: Resp = client
        .post(format!("{root}/api/embed"))
        .json(&serde_json::json!({ "model": EMBED_MODEL, "input": inputs }))
        .send()
        .await
        .map_err(|e| format!("embed request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("embed request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("embed response unreadable: {e}"))?;
    if resp.embeddings.len() != inputs.len() {
        return Err(format!(
            "embed returned {} vectors for {} inputs",
            resp.embeddings.len(),
            inputs.len()
        ));
    }
    Ok(resp.embeddings)
}

/// Embed a search query IF the semantic path is currently available: the
/// Ollama server must already be answering (query time never spawns a server
/// or triggers a pull — that's the worker's job) and the model installed.
/// `None` means "FTS-only this time".
pub async fn embed_query(state: &AppState, query: &str) -> Option<Vec<f32>> {
    let root = ollama::root_url(state);
    if !ollama::server_alive(&root).await {
        return None;
    }
    match embed_raw(&root, &[query_prompt(query)]).await {
        Ok(mut v) if !v.is_empty() => Some(v.remove(0)),
        Ok(_) => None,
        Err(e) => {
            tracing::debug!(error = %e, "query embed unavailable, staying FTS-only");
            None
        }
    }
}

async fn model_installed(root: &str) -> bool {
    #[derive(Deserialize)]
    struct Tags {
        #[serde(default)]
        models: Vec<Tag>,
    }
    #[derive(Deserialize)]
    struct Tag {
        name: String,
    }
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    else {
        return false;
    };
    let Ok(resp) = client.get(format!("{root}/api/tags")).send().await else {
        return false;
    };
    let Ok(tags) = resp.json::<Tags>().await else {
        return false;
    };
    tags.models.iter().any(|m| m.name == EMBED_MODEL)
}

/// Make the embedding path fully ready: server answering (managed spawn if
/// installed), model pulled (streamed to `model:progress` like every other
/// model download). Errors are the caller's cue to back off.
async fn ensure_ready(app: &tauri::AppHandle) -> Result<String, String> {
    let state = app.state::<AppState>();
    ollama::ensure_running(&state).await?;
    let root = ollama::root_url(&state);
    if !model_installed(&root).await {
        tracing::info!(model = EMBED_MODEL, "pulling embedding model");
        let emit = {
            let app = app.clone();
            move |p: crate::models::ModelProgress| {
                use tauri::Emitter;
                let _ = app.emit("model:progress", p);
            }
        };
        ollama::pull_model(&root, EMBED_MODEL, &emit).await?;
    }
    Ok(root)
}

/// Spawn the background indexer (called once at app setup).
///
/// Flow: chunk-backfill existing content once (pure string work, off the UI
/// thread), then loop forever — embed pending chunks in small batches while
/// there's a backlog, otherwise sleep until nudged (content write) or the
/// idle poll fires. The storage lock is held only around the short DB reads/
/// writes, never across an HTTP call.
pub fn spawn(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        // One-time chunk backfill for pre-feature content.
        {
            let state = app.state::<AppState>();
            let backfilled = {
                let storage = state.storage.lock().unwrap();
                storage.backfill_embedding_chunks()
            };
            match backfilled {
                Ok(n) => tracing::debug!(owners = n, "embedding chunk backfill done"),
                Err(e) => tracing::warn!(error = %e, "embedding chunk backfill failed"),
            }
        }

        loop {
            let state = app.state::<AppState>();
            let backlog = state
                .storage
                .lock()
                .unwrap()
                .embedding_backlog(EMBED_MODEL)
                .unwrap_or(0);
            if backlog == 0 {
                let _ = tokio::time::timeout(IDLE_POLL, state.embed_notify.notified()).await;
                continue;
            }

            let root = match ensure_ready(&app).await {
                Ok(root) => root,
                Err(e) => {
                    tracing::debug!(error = %e, backlog, "embedding index paused (FTS-only until Ollama is available)");
                    let _ = tokio::time::timeout(RETRY_DELAY, state.embed_notify.notified()).await;
                    continue;
                }
            };

            let batch = state
                .storage
                .lock()
                .unwrap()
                .pending_embedding_chunks(EMBED_MODEL, BATCH_SIZE)
                .unwrap_or_default();
            if batch.is_empty() {
                continue;
            }
            let inputs: Vec<String> = batch
                .iter()
                .map(|c| doc_prompt(&c.title, &c.text))
                .collect();
            match embed_raw(&root, &inputs).await {
                Ok(vectors) => {
                    let rows: Vec<(i64, Vec<f32>)> =
                        batch.iter().map(|c| c.id).zip(vectors).collect();
                    let stored = {
                        let storage = state.storage.lock().unwrap();
                        storage.store_chunk_embeddings(EMBED_MODEL, &rows)
                    };
                    match stored {
                        Ok(()) => {
                            tracing::debug!(embedded = rows.len(), backlog, "embedded chunk batch")
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "storing embeddings failed");
                            tokio::time::sleep(RETRY_DELAY).await;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "embedding batch failed, backing off");
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
    });
}
