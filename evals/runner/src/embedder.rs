//! The retrieval suite's vector channel: the CI embedder (`hashed-bow-v1`,
//! model-free) or the app's real embedder through Ollama
//! (`ollama:<model>`), fed the app's own document and query prompts
//! (`fly_core::embed_prompt`).

use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use fly_core::embed_prompt::{doc_prompt, query_prompt};
use fly_storage::Storage;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::embed;

pub enum Embedder {
    HashedBow,
    Ollama(Ollama),
}

/// Why a real-embedder run couldn't start (reported, never a crash).
#[derive(Debug)]
pub struct Unavailable(pub String);

impl Embedder {
    /// `hashed-bow-v1`, or `ollama:<model>` (server from `OLLAMA_HOST`,
    /// default `http://127.0.0.1:11434`).
    pub fn from_spec(spec: &str) -> std::result::Result<Self, Unavailable> {
        if spec == embed::MODEL {
            return Ok(Embedder::HashedBow);
        }
        match spec.strip_prefix("ollama:") {
            Some(model) if !model.is_empty() => Ollama::connect(model).map(Embedder::Ollama),
            _ => Err(Unavailable(format!(
                "unknown embedder {spec:?} (use {} or ollama:<model>)",
                embed::MODEL
            ))),
        }
    }

    /// The model id stored on chunks.
    pub fn model(&self) -> &str {
        match self {
            Embedder::HashedBow => embed::MODEL,
            Embedder::Ollama(o) => &o.model,
        }
    }

    pub fn is_real(&self) -> bool {
        matches!(self, Embedder::Ollama(_))
    }

    pub fn query(&self, q: &str) -> Result<Vec<f32>> {
        match self {
            Embedder::HashedBow => Ok(embed::embed(q)),
            Embedder::Ollama(o) => Ok(o.embed(&[query_prompt(q)])?.remove(0)),
        }
    }

    /// Embed every pending chunk of `storage`.
    pub fn index(&self, storage: &Storage) -> Result<usize> {
        match self {
            Embedder::HashedBow => embed::embed_pending(storage),
            Embedder::Ollama(o) => o.index(storage),
        }
    }

    pub fn describe(&self) -> Value {
        match self {
            Embedder::HashedBow => json!({ "embedder": embed::MODEL }),
            Embedder::Ollama(o) => json!({
                "embedder": format!("ollama:{}", o.model),
                "model": o.model,
                "digest": o.digest,
                "server": o.root,
                "document_prompt": doc_prompt("<title>", "<text>"),
                "query_prompt": query_prompt("<query>"),
            }),
        }
    }
}

pub struct Ollama {
    root: String,
    pub model: String,
    pub digest: String,
    client: reqwest::Client,
    rt: tokio::runtime::Runtime,
}

impl Ollama {
    fn connect(model: &str) -> std::result::Result<Self, Unavailable> {
        let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "127.0.0.1:11434".into());
        let root = if host.starts_with("http://") || host.starts_with("https://") {
            host.trim_end_matches('/').to_string()
        } else {
            format!("http://{}", host.trim_end_matches('/'))
        };
        let fail = |why: String| Unavailable(why);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| fail(e.to_string()))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| fail(e.to_string()))?;
        #[derive(Deserialize)]
        struct Tags {
            #[serde(default)]
            models: Vec<Tag>,
        }
        #[derive(Deserialize)]
        struct Tag {
            name: String,
            #[serde(default)]
            digest: String,
        }
        let tags: Tags = rt
            .block_on(async {
                client
                    .get(format!("{root}/api/tags"))
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await
            })
            .map_err(|_| {
                fail(format!(
                    "Ollama isn't reachable at {root}; start it to run the real-embedder suite"
                ))
            })?;
        let Some(tag) = tags.models.into_iter().find(|t| t.name == model) else {
            return Err(fail(format!(
                "Ollama at {root} doesn't have {model}; pull it (ollama pull {model})"
            )));
        };
        Ok(Ollama {
            root,
            model: model.into(),
            digest: tag.digest,
            client,
            rt,
        })
    }

    fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        #[derive(Deserialize)]
        struct Resp {
            embeddings: Vec<Vec<f32>>,
        }
        let resp: Resp = self
            .rt
            .block_on(async {
                self.client
                    .post(format!("{}/api/embed", self.root))
                    .json(&json!({ "model": self.model, "input": inputs }))
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await
            })
            .context("Ollama /api/embed")?;
        if resp.embeddings.len() != inputs.len() {
            bail!(
                "Ollama returned {} vectors for {} inputs",
                resp.embeddings.len(),
                inputs.len()
            );
        }
        Ok(resp.embeddings)
    }

    /// Embed every pending chunk with the app's document prompt, in the
    /// app's batch size.
    fn index(&self, storage: &Storage) -> Result<usize> {
        let mut n = 0;
        loop {
            let batch = storage.pending_embedding_chunks(&self.model, 16)?;
            if batch.is_empty() {
                return Ok(n);
            }
            let inputs: Vec<String> = batch
                .iter()
                .map(|c| doc_prompt(&c.title, &c.text))
                .collect();
            let vectors = self.embed(&inputs)?;
            let rows: Vec<(i64, Vec<f32>)> = batch.iter().map(|c| c.id).zip(vectors).collect();
            n += rows.len();
            storage.store_chunk_embeddings(&self.model, &rows)?;
        }
    }
}
