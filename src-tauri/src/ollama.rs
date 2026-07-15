//! Managed Ollama: opt-in install through the artifact manager, an app-owned
//! `ollama serve` child process when nothing is already listening, and model
//! pulls with streaming progress. A server the USER runs (desktop app,
//! service, custom port) is always respected — we only spawn when the
//! configured address is silent, and we only ever kill the child we spawned.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::models;
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Artifact id in the models.rs registry (Windows-only; other platforms use
/// an Ollama found on PATH or a user-run server).
pub const ARTIFACT_ID: &str = "ollama-bin";
/// Where a managed server keeps its pulled models — inside the app data dir,
/// so "everything the app stores lives under the data dir" stays true.
const MODELS_SUBDIR: &str = "models/ollama";
/// The only address we manage a server for. A custom base URL means the user
/// runs their own server (remote box, custom port) — never touch it.
const DEFAULT_ROOT: &str = "http://localhost:11434";
/// How long a freshly spawned server gets to answer /api/version.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Root server URL (no `/v1`): the chat provider appends /v1, settings may
/// store either form.
fn root_url(state: &AppState) -> String {
    let configured = state
        .storage
        .lock()
        .unwrap()
        .get_setting("llm.ollama.base_url")
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty());
    match configured {
        Some(url) => normalize_root(&url),
        None => DEFAULT_ROOT.to_string(),
    }
}

fn normalize_root(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix("/v1").unwrap_or(u);
    u.trim_end_matches('/').to_string()
}

/// The Ollama executable: a managed install wins, then PATH.
fn installed_exe(data_dir: &Path) -> Option<PathBuf> {
    models::artifact(ARTIFACT_ID)
        .and_then(|a| models::installed_path(data_dir, a))
        .or_else(|| models::find_on_path(&["ollama"]))
}

async fn server_alive(root: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(1500))
        .build()
    else {
        return false;
    };
    client
        .get(format!("{root}/api/version"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Make sure an Ollama server answers at the configured address. Reuses a
/// server that is already listening (user-run wins); otherwise spawns a
/// managed `ollama serve` that `shutdown` kills on app exit. Called lazily by
/// every LLM entry point when the selected provider is "ollama".
pub async fn ensure_running(state: &AppState) -> Result<(), String> {
    let root = root_url(state);
    if server_alive(&root).await {
        return Ok(());
    }
    if root != DEFAULT_ROOT {
        return Err(format!(
            "Ollama isn't reachable at {root}. Start your server there, or clear the \
             custom base URL in Settings to let the app manage one."
        ));
    }
    let exe = installed_exe(&state.data_dir).ok_or(
        "Ollama isn't installed — use the Install button in Settings → AI provider, \
         or install it yourself from ollama.com",
    )?;

    {
        let mut guard = state.ollama.lock().unwrap();
        // Reap a child that exited (crash, manual kill) so we can respawn.
        if let Some(child) = guard.as_mut() {
            if matches!(child.try_wait(), Ok(Some(_))) {
                *guard = None;
            }
        }
        if guard.is_none() {
            let mut cmd = std::process::Command::new(&exe);
            cmd.arg("serve")
                .env("OLLAMA_MODELS", state.data_dir.join(MODELS_SUBDIR))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            let child = cmd
                .spawn()
                .map_err(|e| format!("could not start Ollama ({}): {e}", exe.display()))?;
            tracing::info!(exe = %exe.display(), "started managed `ollama serve`");
            *guard = Some(child);
        }
    } // guard dropped before any await

    let deadline = std::time::Instant::now() + STARTUP_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if server_alive(&root).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    Err("Ollama was started but isn't answering yet — try again in a few seconds".into())
}

/// Kill the managed server if we spawned one (app exit). A user-run server is
/// never ours to stop.
pub fn shutdown(state: &AppState) {
    if let Some(mut child) = state.ollama.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
        tracing::info!("stopped managed `ollama serve`");
    }
}

// ---------------------------------------------------------------------------
// Commands (status + model pull)
// ---------------------------------------------------------------------------

/// One installed model as reported by `/api/tags`.
#[derive(Serialize, Clone)]
pub struct OllamaModel {
    /// Full name with tag (`llama3.1:latest`, `qwen3.5:4b`, …).
    pub name: String,
    /// On-disk size in bytes.
    pub size: u64,
}

#[derive(Serialize)]
pub struct OllamaStatus {
    /// An executable is available (managed install or PATH).
    pub installed: bool,
    /// This OS has a managed download (the Install button makes sense).
    pub can_install: bool,
    pub running: bool,
    /// The running server is a child this app spawned.
    pub managed: bool,
    pub base_url: String,
    /// Installed models (name + size) when the server is running.
    pub models: Vec<OllamaModel>,
}

#[tauri::command]
pub async fn ollama_status(state: State<'_, AppState>) -> CmdResult<OllamaStatus> {
    let root = root_url(&state);
    let running = server_alive(&root).await;
    let models = if running {
        list_models(&root).await.unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(OllamaStatus {
        installed: installed_exe(&state.data_dir).is_some(),
        can_install: models::artifact(ARTIFACT_ID).is_some(),
        running,
        managed: state.ollama.lock().unwrap().is_some(),
        base_url: root,
        models,
    })
}

async fn list_models(root: &str) -> Result<Vec<OllamaModel>, String> {
    #[derive(Deserialize)]
    struct Tags {
        #[serde(default)]
        models: Vec<Tag>,
    }
    #[derive(Deserialize)]
    struct Tag {
        name: String,
        #[serde(default)]
        size: u64,
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let tags: Tags = client
        .get(format!("{root}/api/tags"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(tags
        .models
        .into_iter()
        .map(|m| OllamaModel {
            name: m.name,
            size: m.size,
        })
        .collect())
}

/// Tag-insensitive model-name match: `llama3.1` names the same model as
/// `llama3.1:latest` (Ollama reports the tagged form; settings usually store
/// the bare form). Distinct tags of the same family (`qwen3.5:4b` vs
/// `qwen3.5:9b`) do NOT match unless one side is bare.
fn same_model(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let base = |s: &str| s.split(':').next().unwrap_or(s).trim().to_string();
    let (ab, bb) = (base(a), base(b));
    // A bare name matches any tag of its family; two explicit tags must agree.
    (a == ab || b == bb) && ab == bb
}

/// Delete a local model (`DELETE /api/delete`). Refuses to remove the model
/// the app is currently configured to use — switch models first, then delete.
/// Server handling matches `ollama_pull`: the managed server is started if
/// needed; a custom (user-run) base URL is never touched when unreachable.
#[tauri::command]
pub async fn ollama_delete(state: State<'_, AppState>, model: String) -> CmdResult<()> {
    let (provider, selected) = {
        let storage = state.storage.lock().unwrap();
        let get = |k: &str| {
            storage
                .get_setting(k)
                .ok()
                .flatten()
                .filter(|v| !v.is_empty())
        };
        let default_model = crate::llm_commands::PROVIDERS
            .iter()
            .find(|(id, _, _)| *id == "ollama")
            .map(|(_, m, _)| m.to_string())
            .unwrap_or_default();
        (
            get("llm.provider").unwrap_or_else(|| "ollama".to_string()),
            get("llm.ollama.model").unwrap_or(default_model),
        )
    };
    if provider == "ollama" && same_model(&model, &selected) {
        return Err(format!(
            "{model} is the model the app is currently using — switch to another model in Settings first, then delete it."
        ));
    }

    ensure_running(&state).await?;
    let root = root_url(&state);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .delete(format!("{root}/api/delete"))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(format!("model {model} is not installed"));
    }
    resp.error_for_status().map_err(|e| e.to_string())?;
    tracing::info!(model, "deleted ollama model");
    Ok(())
}

/// Pull a model into the (started-if-needed) server, streaming progress as
/// `model:progress` events with id `ollama:<model>` — the same channel the
/// whisper downloads use, so Settings renders it with the existing plumbing.
#[tauri::command]
pub async fn ollama_pull(app: tauri::AppHandle, model: String) -> CmdResult<()> {
    let state = app.state::<AppState>();
    ensure_running(&state).await?;
    let root = root_url(&state);
    let emit = {
        let app = app.clone();
        move |p: models::ModelProgress| {
            use tauri::Emitter;
            let _ = app.emit("model:progress", p);
        }
    };
    pull_model(&root, &model, &emit).await
}

/// `POST /api/pull` with streaming JSON lines ({status, total?, completed?,
/// error?}) mapped onto ModelProgress. The HTTP API beats parsing the CLI's
/// ANSI progress output. Public so tests can drive it without a webview.
pub async fn pull_model(
    root: &str,
    model: &str,
    progress: models::ProgressSink<'_>,
) -> Result<(), String> {
    use futures_util::StreamExt;

    #[derive(Deserialize, Default)]
    struct Line {
        #[serde(default)]
        status: String,
        error: Option<String>,
        total: Option<u64>,
        completed: Option<u64>,
    }

    let id = format!("ollama:{model}");
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{root}/api/pull"))
        .json(&serde_json::json!({ "model": model, "stream": true }))
        .send()
        .await
        .map_err(|e| format!("model download failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("model download failed: {e}"))?;

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let (mut downloaded, mut total) = (0u64, 0u64);
    let mut last_emit = std::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("model download interrupted: {e}"))?;
        buf.extend_from_slice(&chunk);
        while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            let Ok(l) = serde_json::from_slice::<Line>(&line) else {
                continue;
            };
            if let Some(err) = l.error {
                progress(models::ModelProgress {
                    id: id.clone(),
                    downloaded,
                    total,
                    stage: "error".into(),
                    error: Some(err.clone()),
                });
                return Err(format!("ollama could not pull {model}: {err}"));
            }
            if let (Some(t), Some(c)) = (l.total, l.completed) {
                // The pull downloads several layers; report the biggest one
                // (the weights) rather than flickering between digests.
                if t >= total {
                    (downloaded, total) = (c, t);
                }
            }
            let done = l.status == "success";
            if done || last_emit.elapsed().as_millis() > 200 {
                progress(models::ModelProgress {
                    id: id.clone(),
                    downloaded,
                    total,
                    stage: if done { "done" } else { "downloading" }.into(),
                    error: None,
                });
                last_emit = std::time::Instant::now();
            }
            if done {
                return Ok(());
            }
        }
    }
    // Stream ended without an explicit success line — treat as done; the
    // status refresh will show whether the model is actually present.
    progress(models::ModelProgress {
        id,
        downloaded,
        total,
        stage: "done".into(),
        error: None,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{normalize_root, same_model};

    #[test]
    fn same_model_is_tag_insensitive_for_bare_names() {
        assert!(same_model("llama3.1", "llama3.1:latest"));
        assert!(same_model("llama3.1:latest", "llama3.1"));
        assert!(same_model("qwen3.5:4b", "qwen3.5:4b"));
        assert!(same_model("qwen3.5", "qwen3.5:4b"));
        // Two explicit, different tags are different models.
        assert!(!same_model("qwen3.5:4b", "qwen3.5:9b"));
        assert!(!same_model("llama3.1", "llama3.2:3b"));
        assert!(!same_model("gemma3", "gemma3n:e4b"));
    }

    #[test]
    fn root_urls_normalize_to_no_v1_no_slash() {
        for (input, want) in [
            ("http://localhost:11434", "http://localhost:11434"),
            ("http://localhost:11434/", "http://localhost:11434"),
            ("http://localhost:11434/v1", "http://localhost:11434"),
            ("http://localhost:11434/v1/", "http://localhost:11434"),
            (" http://box:8080/v1 ", "http://box:8080"),
        ] {
            assert_eq!(normalize_root(input), want, "input: {input}");
        }
    }
}
