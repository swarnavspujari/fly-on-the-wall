//! Live Ask harness runs against REAL local models (ignored by default).
//!
//! Runs fly-ask on the system evals' synthetic fixture corpus with a model
//! served by the local Ollama, records every request and reply through the
//! evals' `RecordingProvider`, and writes replay files (format v2) plus a
//! summary OUTSIDE the repository for review. Nothing is pulled: a model
//! that isn't installed is skipped with a message.
//!
//! The provider is built by the app's own `build_provider_with_reach` (the
//! one constructor) on a throwaway data dir, never the user's.
//!
//! ```text
//! cargo test -p fly-app --test ask_live -- --ignored --nocapture
//! FOTW_ASK_LIVE_MODELS=llama3.1,qwen3.5:9b,gemma4:12b   models to try
//! FOTW_ASK_LIVE_LIMIT=20                                 scenarios per model
//! FOTW_ASK_LIVE_MATCH=q0                                 only ids containing this
//! FOTW_ASK_LIVE_OUT=<dir outside the repo>               default: <temp>/fotw-ask-live
//! ```
//! Replays land in `<out>/<model>/<scenario>.json`; copying one into
//! `evals/suites/ask_contract/replays/` is a locked change for the user.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fly_app_lib::llm_commands::{ask_tier, build_provider_with_reach, DataScope};
use fly_app_lib::state::AppState;
use fly_evals::ask_contract::{self as c, Contract, RecordingProvider, ScenarioKind};
use serde_json::json;

const OLLAMA: &str = "http://localhost:11434";

/// The evals' query embedder, as fly-ask's (the fixture vectors are its).
struct Embedder;

impl fly_ask::QueryEmbedder for Embedder {
    fn model(&self) -> &str {
        fly_evals::embed::MODEL
    }
    async fn embed(&self, text: &str) -> Option<Vec<f32>> {
        Some(fly_evals::embed::embed(text))
    }
}

async fn installed(model: &str) -> Option<bool> {
    let tags: serde_json::Value = reqwest::get(format!("{OLLAMA}/api/tags"))
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let want = if model.contains(':') {
        model.to_string()
    } else {
        format!("{model}:latest")
    };
    Some(
        tags["models"]
            .as_array()?
            .iter()
            .filter_map(|m| m["name"].as_str())
            .any(|n| n == want || n == model),
    )
}

fn out_dir(repo: &Path) -> PathBuf {
    let out = std::env::var_os("FOTW_ASK_LIVE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("fotw-ask-live"));
    let abs = std::fs::canonicalize(&out).unwrap_or_else(|_| out.clone());
    let repo = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    assert!(
        !abs.starts_with(&repo),
        "FOTW_ASK_LIVE_OUT must be outside the repository (replays are reviewed before they're committed)"
    );
    out
}

fn tier_of(t: fly_ask::Tier) -> c::Tier {
    match t {
        fly_ask::Tier::S => c::Tier::S,
        fly_ask::Tier::M => c::Tier::M,
        fly_ask::Tier::C => c::Tier::C,
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a local Ollama with the models installed; run by hand"]
async fn ask_live() {
    let models: Vec<String> = std::env::var("FOTW_ASK_LIVE_MODELS")
        .unwrap_or_else(|_| "llama3.1".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let limit: usize = std::env::var("FOTW_ASK_LIVE_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);
    let only = std::env::var("FOTW_ASK_LIVE_MATCH").ok();
    let out = out_dir(&fly_evals::repo_root());

    let contract = Contract::load(&fly_evals::evals_dir()).expect("contract");
    let db = fly_evals::corpus::Db::build(&contract.corpus, true).expect("fixture db");
    // Database ids back to fixture ids (the evals' own mapping), so a
    // replay's request fingerprint matches what CI computes.
    let normalize = |s: &str| db.normalize_ids(s);
    let now = fly_evals::ask_contract::eval_now();
    // Quality scenarios on a local provider: the reference questions.
    let scenarios: Vec<&c::Scenario> = contract
        .scenarios
        .iter()
        .filter(|s| s.kind == ScenarioKind::Quality && s.provider == c::ProviderKind::Local)
        .filter(|s| !s.id.starts_with("replay:"))
        .filter(|s| only.as_deref().is_none_or(|o| s.id.contains(o)))
        .take(limit)
        .collect();

    let mut summary = BTreeMap::new();
    for model in &models {
        match installed(model).await {
            None => {
                eprintln!("ask_live: Ollama isn't answering at {OLLAMA}; nothing run");
                return;
            }
            Some(false) => {
                eprintln!("ask_live: {model} isn't installed; skipped (nothing is pulled)");
                continue;
            }
            Some(true) => {}
        }
        // A throwaway data dir: the app's provider path, never the user's data.
        let dir = tempfile::tempdir().expect("tempdir");
        let secrets = Arc::new(fly_secrets::MemorySecretStore::default());
        let state = AppState::init_with(dir.path().to_path_buf(), secrets).expect("state");
        {
            let s = state.storage.lock().unwrap();
            s.set_setting("llm.provider", "ollama").unwrap();
            s.set_setting("llm.ollama.model", model).unwrap();
        }
        let built = build_provider_with_reach(&state, DataScope::NoMeetingData).expect("provider");
        let caps = built
            .provider
            .probe_capabilities()
            .await
            .unwrap_or_else(|_| built.provider.capabilities());
        let tier = ask_tier(&state, built.provider.as_ref()).await;
        let inner: Arc<dyn fly_llm::LLMProvider> = Arc::from(built.provider);
        let model_dir = out.join(model.replace([':', '/', '\\'], "_"));
        std::fs::create_dir_all(&model_dir).expect("out dir");
        eprintln!(
            "ask_live: {model}: tier {tier:?}, {} scenarios → {}",
            scenarios.len(),
            model_dir.display()
        );

        let mut totals: BTreeMap<String, u32> = BTreeMap::new();
        let mut latency = Vec::new();
        let mut first_token = Vec::new();
        for s in &scenarios {
            let rec = RecordingProvider::wrap(inner.clone(), built.on_device);
            let scope = match &s.scope {
                Some(sc) if sc.meeting.is_some() => {
                    fly_ask::AskScope::Meeting(db.meeting_ids[sc.meeting.as_ref().unwrap()].clone())
                }
                Some(sc) if sc.folder.is_some() => {
                    fly_ask::AskScope::Folder(db.folder_ids[sc.folder.as_ref().unwrap()].clone())
                }
                _ => fly_ask::AskScope::All,
            };
            let t0 = Instant::now();
            let a = fly_ask::ask(
                fly_ask::AskEnv {
                    store: &db.storage,
                    provider: &rec,
                    on_device: built.on_device,
                    tier,
                    limits: None,
                    embedder: Some(&Embedder),
                    events: None,
                    cancel: None,
                },
                fly_ask::AskInput {
                    question: &s.question,
                    history: &[],
                    scope,
                    now,
                },
            )
            .await;
            latency.push(t0.elapsed().as_millis() as u64);
            if let Some(ft) = a.timing.first_token_ms {
                first_token.push(ft);
            }
            let status = serde_json::to_value(a.status).unwrap();
            *totals
                .entry(status.as_str().unwrap_or("?").to_string())
                .or_default() += 1;
            let sourced = a
                .sentences
                .iter()
                .filter(|x| x.status == fly_ask::SentenceStatus::Sourced)
                .count() as u32;
            *totals.entry("sentences".into()).or_default() += a.sentences.len() as u32;
            *totals.entry("sourced_sentences".into()).or_default() += sourced;
            if let Some(f) = &a.failure {
                let k = serde_json::to_value(f.kind).unwrap();
                *totals
                    .entry(format!("failure:{}", k.as_str().unwrap_or("?")))
                    .or_default() += 1;
            }
            let replay = rec.to_replay(&s.id, tier_of(tier), caps, &normalize);
            std::fs::write(
                model_dir.join(format!("{}.json", s.id)),
                serde_json::to_string_pretty(&replay).unwrap(),
            )
            .expect("write replay");
            eprintln!(
                "  {:<28} {:>6} ms  {:?}  {} sentence(s), {sourced} sourced",
                s.id,
                t0.elapsed().as_millis(),
                a.status,
                a.sentences.len()
            );
        }
        latency.sort_unstable();
        first_token.sort_unstable();
        let entry = json!({
            "tier": format!("{tier:?}"),
            "scenarios": scenarios.len(),
            "totals": totals,
            "latency_ms": {"p50": percentile(&latency, 0.5), "p95": percentile(&latency, 0.95)},
            "first_token_ms": {"p50": percentile(&first_token, 0.5), "p95": percentile(&first_token, 0.95)},
        });
        eprintln!("ask_live: {model}: {entry}");
        summary.insert(model.clone(), entry);
    }
    std::fs::create_dir_all(&out).ok();
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_string_pretty(&summary).unwrap(),
    )
    .expect("write summary");
    eprintln!("ask_live: summary → {}", out.join("summary.json").display());
}
