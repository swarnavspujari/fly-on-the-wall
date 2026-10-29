//! Semantic-search embedding pipeline: a background worker that drains the
//! `embedding_chunks` backlog through Ollama's `/api/embed`, plus the
//! query-time embed used by hybrid search.
//!
//! Model: EmbeddingGemma 300M — small (~600 MB), multilingual (100+
//! languages), built for on-device retrieval. Pulled automatically through
//! the managed Ollama runtime the first time indexing runs, with progress on
//! the same `model:progress` channel as every other model download.
//!
//! Scheduling: the worker steps aside while a recording (live captions), a
//! transcription or its polish, or a foreground AI request (Ask, Enhance)
//! runs; embeds the most recently written chunks first; and rests after each
//! batch for as long as it took, so a large backlog never hogs the machine
//! (see [`yield_reason`], [`rest_after`]).
//!
//! Degradation contract: everything here is best-effort. No Ollama installed,
//! server down, model missing — the worker backs off and retries, chunk rows
//! simply stay pending, and search silently falls back to FTS-only. Nothing
//! in this module ever blocks the UI or fails a user action.

use std::sync::Mutex;
use std::time::Duration;

use fly_storage::Storage;
use serde::Deserialize;
use tauri::Manager;

use crate::ollama;
use crate::state::AppState;

/// Embedding model tag pulled into the managed Ollama runtime.
pub const EMBED_MODEL: &str = "embeddinggemma:300m";

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

/// Chunks per `/api/embed` call. Small enough to keep each request quick on
/// CPU (and to let a search's query embedding in soon), large enough to
/// amortize HTTP overhead across a backfill.
const BATCH_SIZE: usize = 16;
/// Idle poll: a missed nudge only delays indexing by this much.
const IDLE_POLL: Duration = Duration::from_secs(30);
/// Backoff after an embed/availability failure (Ollama down, pull failed).
const RETRY_DELAY: Duration = Duration::from_secs(60);
/// While the worker yields (recording, transcription, a foreground AI
/// request), it checks back this often.
const YIELD_POLL: Duration = Duration::from_secs(5);
/// Pacing: after a batch that took `t`, rest `t × REST_RATIO` (at least
/// `MIN_REST`), so a large backlog — the one-time re-embed after the v3
/// upgrade, or a model change — takes at most about half of what the
/// embedding runtime could do, and never runs back to back.
const REST_RATIO: u32 = 1;
const MIN_REST: Duration = Duration::from_millis(250);
/// Notes/transcripts re-chunked per storage-lock hold in the startup pass.
const BACKFILL_OWNERS_PER_LOCK: usize = 8;

/// What the worker steps aside for. Priority: live captions (recording) >
/// transcription and its polish > a foreground AI request (Ask, Enhance) >
/// the embedding backlog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum YieldTo {
    Recording,
    Transcription,
    Foreground,
}

/// Why the worker shouldn't embed right now, if it shouldn't.
pub(crate) fn yield_reason(state: &AppState) -> Option<YieldTo> {
    if crate::scheduler::recording_active(state) {
        return Some(YieldTo::Recording);
    }
    // A queued job only says "waiting"; a running one (ASR, diarization,
    // polish) names its stage.
    let transcribing = state
        .pipeline_stage
        .lock()
        .unwrap()
        .values()
        .any(|stage| stage != crate::scheduler::WAITING_STAGE);
    if transcribing {
        return Some(YieldTo::Transcription);
    }
    if state.foreground_ai_active() {
        return Some(YieldTo::Foreground);
    }
    None
}

/// How long to rest after a batch that took `took`.
pub(crate) fn rest_after(took: Duration) -> Duration {
    (took * REST_RATIO).max(MIN_REST)
}

/// Something that turns document prompts into vectors (Ollama in the app,
/// a fake in tests).
pub(crate) trait Embedder {
    fn embed(
        &self,
        inputs: Vec<String>,
    ) -> impl std::future::Future<Output = Result<Vec<Vec<f32>>, String>> + Send;
}

struct Ollama {
    root: String,
}

impl Embedder for Ollama {
    async fn embed(&self, inputs: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        embed_raw(&self.root, &inputs).await
    }
}

/// One batch: read the newest pending chunks (lock held only for the read),
/// embed them with no lock held, store what still matches (lock held only
/// for the write). `shareable_only`: the embedder is off this machine, so
/// keep-local chunks never go to it. Returns how many vectors were stored
/// (`Ok(0)` with nothing pending).
pub(crate) async fn embed_next_batch<E: Embedder>(
    storage: &Mutex<Storage>,
    embedder: &E,
    shareable_only: bool,
    size: usize,
) -> Result<usize, String> {
    let batch = {
        let storage = storage.lock().unwrap();
        if shareable_only {
            storage.pending_embedding_chunks_shareable(EMBED_MODEL, size)
        } else {
            storage.pending_embedding_chunks(EMBED_MODEL, size)
        }
    }
    .map_err(|e| e.to_string())?;
    if batch.is_empty() {
        return Ok(0);
    }
    let inputs: Vec<String> = batch
        .iter()
        .map(|c| doc_prompt(&c.title, &c.text))
        .collect();
    let vectors = embedder.embed(inputs).await?;
    if vectors.len() != batch.len() {
        return Err(format!(
            "embedder returned {} vectors for {} chunks",
            vectors.len(),
            batch.len()
        ));
    }
    let rows: Vec<(fly_storage::PendingChunk, Vec<f32>)> = batch.into_iter().zip(vectors).collect();
    storage
        .lock()
        .unwrap()
        .store_pending_embeddings(EMBED_MODEL, &rows)
        .map_err(|e| e.to_string())
}

/// The startup chunk pass, a few owners per storage-lock hold, so commands
/// and the recording path never wait behind a whole library's re-chunk.
fn backfill_chunks(storage: &Mutex<Storage>) -> Result<usize, String> {
    let owners = storage
        .lock()
        .unwrap()
        .chunk_owners()
        .map_err(|e| e.to_string())?;
    for group in owners.chunks(BACKFILL_OWNERS_PER_LOCK) {
        let storage = storage.lock().unwrap();
        for owner in group {
            if let Err(e) = storage.sync_owner_chunks(owner) {
                tracing::warn!(?owner, error = %e, "chunk backfill skipped an owner");
            }
        }
        drop(storage);
        std::thread::yield_now();
    }
    Ok(owners.len())
}

/// Spawn the background indexer (called once at app setup).
///
/// Flow: chunk-backfill existing content once (pure string work, on a
/// blocking thread, a few owners per lock hold), then loop forever: step
/// aside while recording, transcribing or serving a foreground AI request;
/// otherwise embed pending chunks newest first, in small batches with a
/// rest after each; with no backlog, sleep until nudged (content write) or
/// the idle poll fires. The storage lock is held only around short DB
/// reads/writes, never across an HTTP call.
pub fn spawn(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        {
            let handle = app.clone();
            let done = tauri::async_runtime::spawn_blocking(move || {
                backfill_chunks(&handle.state::<AppState>().storage)
            })
            .await;
            match done {
                Ok(Ok(n)) => tracing::debug!(owners = n, "embedding chunk backfill done"),
                Ok(Err(e)) => tracing::warn!(error = %e, "embedding chunk backfill failed"),
                Err(e) => tracing::warn!(error = %e, "embedding chunk backfill panicked"),
            }
        }

        let mut yielded: Option<YieldTo> = None;
        loop {
            let state = app.state::<AppState>();
            if let Some(reason) = yield_reason(&state) {
                if yielded != Some(reason) {
                    tracing::info!(?reason, "embedding index paused");
                    yielded = Some(reason);
                }
                let _ = tokio::time::timeout(YIELD_POLL, state.embed_notify.notified()).await;
                continue;
            }
            if yielded.take().is_some() {
                tracing::info!("embedding index resumed");
            }
            // An Ollama on another machine must never receive keep-local
            // notes or transcripts: index only the shareable queue then.
            // (Backlog only decides whether to wake up; the batch below
            // re-derives the filter from the URL it actually sends to.)
            let remote = !crate::llm_commands::is_loopback_url(&ollama::root_url(&state));
            let backlog = {
                let storage = state.storage.lock().unwrap();
                if remote {
                    storage.embedding_backlog_shareable(EMBED_MODEL)
                } else {
                    storage.embedding_backlog(EMBED_MODEL)
                }
                .unwrap_or(0)
            };
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

            // Decide the filter from the SAME url the batch is sent to: the
            // setting may have changed since the backlog check above.
            let send_remote = !crate::llm_commands::is_loopback_url(&root);
            let started = std::time::Instant::now();
            match embed_next_batch(&state.storage, &Ollama { root }, send_remote, BATCH_SIZE).await
            {
                Ok(0) => {}
                Ok(n) => {
                    tracing::debug!(embedded = n, backlog, "embedded chunk batch");
                    tokio::time::sleep(rest_after(started.elapsed())).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "embedding batch failed, backing off");
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_core::{Speaker, Transcript, TranscriptSegment};

    /// A write the fake runs while it "embeds".
    type During<'a> = Box<dyn Fn(&Storage) + Send + Sync + 'a>;

    /// Records what it was asked to embed; asserts the storage lock is free
    /// while it works; can run a write mid-embed.
    struct Fake<'a> {
        storage: &'a Mutex<Storage>,
        seen: Mutex<Vec<String>>,
        during: Option<During<'a>>,
    }

    impl<'a> Fake<'a> {
        fn new(storage: &'a Mutex<Storage>) -> Self {
            Self {
                storage,
                seen: Mutex::new(Vec::new()),
                during: None,
            }
        }
    }

    impl Embedder for Fake<'_> {
        async fn embed(&self, inputs: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
            let storage = self
                .storage
                .try_lock()
                .expect("the storage lock is never held across an embed");
            if let Some(f) = &self.during {
                f(&storage);
            }
            drop(storage);
            self.seen.lock().unwrap().extend(inputs.iter().cloned());
            Ok(inputs.iter().map(|_| vec![1.0, 0.0]).collect())
        }
    }

    fn storage() -> (tempfile::TempDir, Mutex<Storage>) {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        (dir, Mutex::new(s))
    }

    fn meeting(s: &Storage, title: &str, line: &str) {
        let note = s.create_note(title, None).unwrap();
        let m = s.create_meeting(title, &note.id, &[]).unwrap();
        s.save_transcript(&Transcript {
            meeting_id: m.id.clone(),
            language: Some("en".into()),
            engine: "test".into(),
            segments: vec![TranscriptSegment {
                id: format!("{title}-0"),
                speaker_key: "spk_0".into(),
                start_ms: 0,
                end_ms: 1000,
                text: line.into(),
                words: vec![],
            }],
            speakers: vec![Speaker {
                key: "spk_0".into(),
                label: "Speaker 1".into(),
            }],
        })
        .unwrap();
    }

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tauri::async_runtime::block_on(f)
    }

    #[test]
    fn new_content_is_embedded_before_the_backlog_and_the_lock_is_free() {
        let (_dir, storage) = storage();
        {
            let s = storage.lock().unwrap();
            for i in 0..5 {
                meeting(&s, &format!("Backlog {i}"), "an older discussion");
            }
            meeting(&s, "Just recorded", "the meeting that just ended");
        }
        let fake = Fake::new(&storage);
        assert_eq!(run(embed_next_batch(&storage, &fake, false, 1)), Ok(1));
        assert!(
            fake.seen.lock().unwrap()[0].contains("Just recorded"),
            "{:?}",
            fake.seen.lock().unwrap()
        );
        while run(embed_next_batch(&storage, &fake, false, 2)).unwrap() > 0 {}
        assert_eq!(
            storage
                .lock()
                .unwrap()
                .embedding_backlog(EMBED_MODEL)
                .unwrap(),
            0
        );
        assert_eq!(fake.seen.lock().unwrap().len(), 6);
    }

    #[test]
    fn a_chunk_rewritten_while_embedding_stays_pending() {
        let (_dir, storage) = storage();
        let note_id = {
            let s = storage.lock().unwrap();
            let n = s.create_note("Draft", None).unwrap();
            s.update_note_scratchpad(&n.id, "first version").unwrap();
            n.id
        };
        let mut fake = Fake::new(&storage);
        let id = note_id.clone();
        fake.during = Some(Box::new(move |s: &Storage| {
            s.update_note_scratchpad(&id, "second version").unwrap();
        }));
        // The vector of "first version" must not land on the new row.
        assert_eq!(run(embed_next_batch(&storage, &fake, false, 8)), Ok(0));
        let s = storage.lock().unwrap();
        let pending = s.pending_embedding_chunks(EMBED_MODEL, 8).unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].text.contains("second version"));
    }

    #[test]
    fn startup_pass_chunks_every_owner() {
        let (_dir, storage) = storage();
        {
            let s = storage.lock().unwrap();
            for i in 0..(BACKFILL_OWNERS_PER_LOCK * 2 + 1) {
                meeting(&s, &format!("M{i}"), "words to index");
            }
            let pending = s.pending_embedding_chunks(EMBED_MODEL, 1000).unwrap();
            assert!(!pending.is_empty());
        }
        // Owners listed: 17 notes (empty, no chunks) + 17 transcripts.
        assert_eq!(
            backfill_chunks(&storage),
            Ok((BACKFILL_OWNERS_PER_LOCK * 2 + 1) * 2)
        );
        // Idempotent: nothing becomes pending twice.
        let before = storage
            .lock()
            .unwrap()
            .embedding_backlog(EMBED_MODEL)
            .unwrap();
        backfill_chunks(&storage).unwrap();
        assert_eq!(
            storage
                .lock()
                .unwrap()
                .embedding_backlog(EMBED_MODEL)
                .unwrap(),
            before
        );
    }

    #[test]
    fn yields_to_transcription_and_foreground_ai() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::init_with(
            dir.path().to_path_buf(),
            std::sync::Arc::new(fly_secrets::MemorySecretStore::default()),
        )
        .unwrap();
        assert_eq!(yield_reason(&state), None);
        // A queued job is only waiting.
        state
            .pipeline_stage
            .lock()
            .unwrap()
            .insert("m1".into(), crate::scheduler::WAITING_STAGE.into());
        assert_eq!(yield_reason(&state), None);
        state
            .pipeline_stage
            .lock()
            .unwrap()
            .insert("m1".into(), "polishing".into());
        assert_eq!(yield_reason(&state), Some(YieldTo::Transcription));
        state.pipeline_stage.lock().unwrap().clear();
        {
            let _ask = state.foreground_ai();
            let _enhance = state.foreground_ai();
            assert_eq!(yield_reason(&state), Some(YieldTo::Foreground));
        }
        assert_eq!(yield_reason(&state), None, "guards release on drop");
    }

    #[test]
    fn rest_scales_with_the_batch() {
        assert_eq!(rest_after(Duration::from_millis(10)), MIN_REST);
        assert_eq!(rest_after(Duration::from_secs(2)), Duration::from_secs(2));
    }
}
