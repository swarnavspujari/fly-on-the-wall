//! Live Ask harness runs against REAL local models (ignored by default).
//!
//! Runs fly-ask on the system evals' synthetic fixture corpus with a model
//! served by the local Ollama, records every request and reply through the
//! evals' `RecordingProvider`, and writes replay files (format v2) plus
//! timing and resource numbers OUTSIDE the repository for review. Nothing
//! is pulled: a model that isn't installed is skipped with a message.
//!
//! The provider is built by the app's own `build_provider_with_reach` (the
//! one constructor) on a throwaway data dir, never the user's. Its Ollama
//! base URL points at a logging pass-through (`ask_live_support/wire.rs`)
//! that keeps Ollama's own prefill / decode counters and what the provider
//! sent (`think`, `format`, `tools`, `num_ctx`); `sampler.rs` watches CPU,
//! RAM, VRAM and how much of the model sits on the GPU.
//!
//! ```text
//! cargo test -p fly-app --features ask-live --test ask_live -- --ignored --nocapture
//! FOTW_ASK_LIVE_MODELS=llama3.1,qwen3.5:9b,gemma4:12b-it-qat   models to try
//! FOTW_ASK_LIVE_SAMPLE=32,10     evenly spaced answerable,unanswerable scenarios
//! FOTW_ASK_LIVE_LIMIT=20         at most this many scenarios per model
//! FOTW_ASK_LIVE_MATCH=q0         only ids containing this
//! FOTW_ASK_LIVE_KINDS=quality    quality (default) and/or red_team, guard
//! FOTW_ASK_LIVE_TIER=S           force a tier (default: the app's measured tier)
//! FOTW_ASK_LIVE_COLD=0           skip the unload before the first scenario
//! FOTW_ASK_LIVE_COLD_EVERY=1     unload before every scenario (cold p50 / p95)
//! FOTW_ASK_LIVE_RESUME=1         keep rows already in <out>/<model>/rows.jsonl
//! FOTW_ASK_LIVE_TIMEOUT_S=900    give up on one Ask after this long
//! FOTW_ASK_LIVE_QUIET_MAX_S=600  before each scenario wait up to this long for the machine
//!                                to go quiet (CPU below FOTW_ASK_LIVE_QUIET_PCT, default 30)
//! FOTW_ASK_LIVE_OUT=<dir outside the repo>   default: <temp>/fotw-ask-live
//! ```
//! Replays land in `<out>/<model>/<scenario>.json` (that folder is what the
//! evals read, so it holds nothing else); rows go to
//! `<out>/_bench/<model>/rows.jsonl` and the model's summary to `bench.json`
//! beside it. Copying a replay into `evals/suites/ask_contract/replays/` is
//! a locked change for the user. Other work on the machine skews every
//! number: `bench.json` records the non-Ollama CPU and whether builds were
//! running.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fly_app_lib::llm_commands::{ask_tier, build_provider_with_reach, DataScope};
use fly_app_lib::state::AppState;
use fly_evals::ask_contract::{self as c, Contract, Expect, RecordingProvider, ScenarioKind};
use serde_json::{json, Value};

#[path = "ask_live_support/sampler.rs"]
mod sampler;
#[path = "ask_live_support/wire.rs"]
mod wire;

const OLLAMA: &str = "http://127.0.0.1:11434";

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
    let tags: Value = reqwest::get(format!("{OLLAMA}/api/tags"))
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

/// `p` made absolute and canonical even when it doesn't exist yet: the
/// nearest existing ancestor is canonicalized and the rest appended, so both
/// sides of the "outside the repository" comparison use the same form
/// (Windows' `\\?\` prefix) and an uncreated folder inside the repo can't pass.
fn canonical_lenient(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut rest = Vec::new();
    let mut base = abs.as_path();
    loop {
        if let Ok(c) = std::fs::canonicalize(base) {
            return rest.iter().rev().fold(c, |acc, part| acc.join(part));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => return abs,
        }
    }
}

fn out_dir(repo: &Path) -> PathBuf {
    let out = std::env::var_os("FOTW_ASK_LIVE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("fotw-ask-live"));
    let abs = canonical_lenient(&out);
    let repo = canonical_lenient(repo);
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

fn env_num(name: &str) -> Option<usize> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// `n` evenly spaced items (all of them when there are no more than `n`).
fn spread<T: Clone>(items: &[T], n: usize) -> Vec<T> {
    if n >= items.len() {
        return items.to_vec();
    }
    (0..n).map(|i| items[i * items.len() / n].clone()).collect()
}

fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    (!sorted.is_empty()).then(|| sorted[((sorted.len() as f64 - 1.0) * p).round() as usize])
}

fn stats(mut v: Vec<u64>) -> Value {
    v.sort_unstable();
    json!({"n": v.len(), "p50": percentile(&v, 0.5), "p95": percentile(&v, 0.95), "max": v.last()})
}

/// Whether Ollama's `/api/ps` name `loaded` is the bench model `model`
/// (`llama3.1` is loaded as `llama3.1:latest`; two explicit tags must agree).
fn same_model(loaded: &str, model: &str) -> bool {
    if loaded == model {
        return true;
    }
    let base = |s: &str| s.split(':').next().unwrap_or(s).to_string();
    (!model.contains(':') && base(loaded) == model && loaded.ends_with(":latest"))
        || (!loaded.contains(':') && base(model) == loaded)
}

/// Models Ollama has loaded right now.
async fn loaded_models(client: &reqwest::Client) -> Option<Vec<String>> {
    let ps: Value = client
        .get(format!("{OLLAMA}/api/ps"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    Some(
        ps["models"]
            .as_array()?
            .iter()
            .filter_map(|x| x["name"].as_str().map(str::to_string))
            .collect(),
    )
}

/// Unload the bench model (and only that one: the rest of what a user's
/// Ollama holds isn't ours to evict), so the next request pays the load.
async fn unload_model(client: &reqwest::Client, model: &str) {
    for _ in 0..20 {
        let Some(loaded) = loaded_models(client).await else {
            return;
        };
        let ours: Vec<&String> = loaded.iter().filter(|n| same_model(n, model)).collect();
        if ours.is_empty() {
            return;
        }
        for n in ours {
            let _ = client
                .post(format!("{OLLAMA}/api/generate"))
                .json(&json!({"model": n, "keep_alive": 0}))
                .send()
                .await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Wait until the machine's CPU (other sessions' builds included) drops
/// below `below` percent, or `max` has passed. Returns the wait and the
/// last reading.
async fn wait_quiet(max: Duration, below: f32) -> (u64, f32) {
    let t0 = Instant::now();
    let mut sys = sysinfo::System::new();
    sys.refresh_cpu_usage();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        sys.refresh_cpu_usage();
        let cpu = sys.global_cpu_usage();
        if cpu < below || t0.elapsed() >= max {
            return (t0.elapsed().as_millis() as u64, cpu);
        }
    }
}

fn machine() -> Value {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
    json!({
        "cpu": sys.cpus().first().map(|c| c.brand().trim().to_string()),
        "logical_cpus": sys.cpus().len(),
        "ram_gb": (sys.total_memory() as f64 / 1024f64.powi(3) * 10.0).round() / 10.0,
        "os": sysinfo::System::long_os_version(),
    })
}

/// Prefill and decode rates from Ollama's own counters. Calls that
/// evaluated fewer than 64 new prompt tokens are cache hits, not prefill.
fn rates(rows: &[Value]) -> Value {
    let (mut pt, mut pns, mut dt, mut dns) = (0u64, 0u64, 0u64, 0u64);
    for w in rows
        .iter()
        .flat_map(|r| r["wires"].as_array().into_iter().flatten())
    {
        let d = &w["done"];
        let (pc, pd) = (
            d["prompt_eval_count"].as_u64().unwrap_or(0),
            d["prompt_eval_duration_ns"].as_u64().unwrap_or(0),
        );
        if pc >= 64 && pd > 0 {
            pt += pc;
            pns += pd;
        }
        let (ec, ed) = (
            d["eval_count"].as_u64().unwrap_or(0),
            d["eval_duration_ns"].as_u64().unwrap_or(0),
        );
        if ec > 0 && ed > 0 {
            dt += ec;
            dns += ed;
        }
    }
    let per_s =
        |n: u64, ns: u64| (ns > 0).then(|| (n as f64 / (ns as f64 / 1e9) * 10.0).round() / 10.0);
    json!({
        "prefill_tokens": pt,
        "prefill_tokens_per_s": per_s(pt, pns),
        "decode_tokens": dt,
        "decode_tokens_per_s": per_s(dt, dns),
    })
}

/// What the provider sent, over every logged call: the known-issue checks.
fn wire_checks(rows: &[Value]) -> Value {
    let wires: Vec<&Value> = rows
        .iter()
        .flat_map(|r| r["wires"].as_array().into_iter().flatten())
        .collect();
    let sent = |w: &&Value, k: &str| w["sent"][k].clone();
    let ctx: Vec<u64> = wires
        .iter()
        .filter_map(|w| sent(w, "num_ctx").as_u64())
        .collect();
    let mut ctx_sorted = ctx.clone();
    ctx_sorted.sort_unstable();
    json!({
        "chat_calls": wires.len(),
        // ollama#18944: `format` and `tools` must never travel together.
        "format_with_tools": wires.iter().filter(|w| sent(w, "format_sent") == true && sent(w, "tools").as_u64() > Some(0)).count(),
        "format_sent": wires.iter().filter(|w| sent(w, "format_sent") == true).count(),
        // ollama#10976: `think` always sent explicitly.
        "think_omitted": wires.iter().filter(|w| sent(w, "think_sent") == false).count(),
        "think_false": wires.iter().filter(|w| sent(w, "think") == false).count(),
        "calls_with_tools": wires.iter().filter(|w| sent(w, "tools").as_u64() > Some(0)).count(),
        "num_ctx": {"min": ctx_sorted.first(), "p50": percentile(&ctx_sorted, 0.5), "max": ctx_sorted.last(), "absent": wires.len() - ctx.len()},
        "http_errors": wires.iter().filter(|w| w["status"].as_u64() >= Some(400)).count(),
        // A retry without num_ctx follows a 5xx (the context fallback).
        "http_5xx": wires.iter().filter(|w| w["status"].as_u64() >= Some(500)).count(),
        "calls_without_num_ctx": wires.iter().filter(|w| sent(w, "num_ctx").is_null()).count(),
        "thinking_chars_total": wires.iter().filter_map(|w| w["thinking_chars"].as_u64()).sum::<u64>(),
        "empty_replies": wires.iter().filter(|w| w["status"] == 200 && w["content_chars"] == 0 && w["tool_calls"] == 0).count(),
        "done_reasons": wires.iter().fold(BTreeMap::<String, u32>::new(), |mut m, w| {
            *m.entry(w["done"]["done_reason"].as_str().unwrap_or("none").to_string()).or_default() += 1;
            m
        }),
    })
}

fn summarize(rows: &[Value]) -> Value {
    let warm: Vec<&Value> = rows
        .iter()
        .filter(|r| r["cold"] == false && r["timed_out"] == false)
        .collect();
    let cold: Vec<&Value> = rows
        .iter()
        .filter(|r| r["cold"] == true && r["timed_out"] == false)
        .collect();
    let pick = |set: &[&Value], k: &str| -> Vec<u64> {
        set.iter().filter_map(|r| r[k].as_u64()).collect()
    };
    let mut status: BTreeMap<String, u32> = BTreeMap::new();
    for r in rows {
        let key = format!(
            "{}:{}",
            r["kind"].as_str().unwrap_or("?"),
            r["status"].as_str().unwrap_or("?")
        );
        *status.entry(key).or_default() += 1;
    }
    let model_wire_ms = |set: &[&Value]| -> Vec<u64> {
        set.iter()
            .filter_map(|r| {
                let w = r["wires"].as_array()?;
                Some(w.iter().filter_map(|x| x["total_ms"].as_u64()).sum())
            })
            .collect()
    };
    let load_ms: Vec<u64> = cold
        .iter()
        .flat_map(|r| r["wires"].as_array().into_iter().flatten())
        .filter_map(|w| w["done"]["load_duration_ns"].as_u64())
        .map(|ns| ns / 1_000_000)
        .collect();
    let all: Vec<&Value> = rows.iter().collect();
    json!({
        "scenarios": rows.len(),
        "by_status": status,
        "timed_out": rows.iter().filter(|r| r["timed_out"] == true).count(),
        "warm": {
            "first_token_ms": stats(pick(&warm, "first_token_ms")),
            "total_ms": stats(pick(&warm, "total_ms")),
            "model_ms": stats(model_wire_ms(&warm)),
        },
        "cold": {
            "first_token_ms": stats(pick(&cold, "first_token_ms")),
            "total_ms": stats(pick(&cold, "total_ms")),
            "model_load_ms": load_ms,
        },
        "rates": rates(rows),
        "wire": wire_checks(rows),
        "context_fallback_asks": all.iter().filter(|r| r["context_fallback"] == true).count(),
    })
}

fn read_rows(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
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
    let limit = env_num("FOTW_ASK_LIVE_LIMIT").unwrap_or(usize::MAX);
    let only = std::env::var("FOTW_ASK_LIVE_MATCH").ok();
    let kinds: Vec<String> = std::env::var("FOTW_ASK_LIVE_KINDS")
        .unwrap_or_else(|_| "quality".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let sample = std::env::var("FOTW_ASK_LIVE_SAMPLE").ok().and_then(|v| {
        let (a, u) = v.split_once(',')?;
        Some((
            a.trim().parse::<usize>().ok()?,
            u.trim().parse::<usize>().ok()?,
        ))
    });
    let forced_tier = std::env::var("FOTW_ASK_LIVE_TIER")
        .ok()
        .map(|t| match t.as_str() {
            "S" => fly_ask::Tier::S,
            "M" => fly_ask::Tier::M,
            "C" => fly_ask::Tier::C,
            other => panic!("FOTW_ASK_LIVE_TIER must be S, M or C, not {other}"),
        });
    let cold_start = std::env::var("FOTW_ASK_LIVE_COLD").map_or(true, |v| v != "0");
    // Every Ask cold (the model unloaded before each): cold p50 / p95.
    let cold_every = std::env::var("FOTW_ASK_LIVE_COLD_EVERY").is_ok_and(|v| v == "1");
    let resume = std::env::var("FOTW_ASK_LIVE_RESUME").is_ok_and(|v| v == "1");
    let timeout = Duration::from_secs(env_num("FOTW_ASK_LIVE_TIMEOUT_S").unwrap_or(900) as u64);
    let quiet_max = Duration::from_secs(env_num("FOTW_ASK_LIVE_QUIET_MAX_S").unwrap_or(0) as u64);
    let quiet_below = env_num("FOTW_ASK_LIVE_QUIET_PCT").unwrap_or(30) as f32;
    let out = out_dir(&fly_evals::repo_root());

    let contract = Contract::load(&fly_evals::evals_dir()).expect("contract");
    let db = fly_evals::corpus::Db::build(&contract.corpus, true).expect("fixture db");
    let normalize = |text: &str| db.normalize_ids(text);
    let now = fly_evals::ask_contract::eval_now();
    let kind_name = |k: ScenarioKind| match k {
        ScenarioKind::Quality => "quality",
        ScenarioKind::Guard => "guard",
        ScenarioKind::RedTeam => "red_team",
        ScenarioKind::Robustness => "robustness",
    };
    // On-device scenarios of the wanted kinds; scripted replays excluded.
    let wanted: Vec<&c::Scenario> = contract
        .scenarios
        .iter()
        .filter(|s| s.provider == c::ProviderKind::Local && s.replay.is_none())
        .filter(|s| kinds.iter().any(|k| k == kind_name(s.kind)))
        .filter(|s| only.as_deref().is_none_or(|o| s.id.contains(o)))
        .collect();

    let http = reqwest::Client::new();
    let proxy = wire::Proxy::start(OLLAMA).await;
    let ollama_version: Value = match http.get(format!("{OLLAMA}/api/version")).send().await {
        Ok(r) => r.json().await.unwrap_or(Value::Null),
        Err(_) => {
            eprintln!("ask_live: Ollama isn't answering at {OLLAMA}; nothing run");
            return;
        }
    };

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
        unload_model(&http, model).await;
        // Models that aren't the bench's stay as they are, and can slow it.
        if let Some(others) = loaded_models(&http).await.filter(|l| !l.is_empty()) {
            eprintln!("ask_live: note: Ollama also has {others:?} loaded; numbers may be affected");
        }
        // A throwaway data dir: the app's provider path, never the user's data.
        let dir = tempfile::tempdir().expect("tempdir");
        let secrets = Arc::new(fly_secrets::MemorySecretStore::default());
        let state = AppState::init_with(dir.path().to_path_buf(), secrets).expect("state");
        {
            let s = state.storage.lock().unwrap();
            s.set_setting("llm.provider", "ollama").unwrap();
            s.set_setting("llm.ollama.model", model).unwrap();
            // The logging pass-through (loopback, so still on device).
            s.set_setting("llm.ollama.base_url", &proxy.url()).unwrap();
        }
        let idle = {
            let s = sampler::Sampler::start(OLLAMA);
            tokio::time::sleep(Duration::from_secs(5)).await;
            s.finish().await
        };
        let built = build_provider_with_reach(&state, DataScope::NoMeetingData).expect("provider");
        assert!(built.on_device, "the bench provider must be on device");
        let caps = built
            .provider
            .probe_capabilities()
            .await
            .unwrap_or_else(|_| built.provider.capabilities());
        let t_tier = Instant::now();
        let measured = ask_tier(&state, built.provider.as_ref(), built.endpoint.as_deref()).await;
        let tier_ms = t_tier.elapsed().as_millis() as u64;
        let tier = forced_tier.unwrap_or(measured);
        let tier_wires = proxy.take();
        // The smoke result itself (the tier cache keeps only pass / fail).
        let inputs = fly_ask::TierInputs {
            local_model: true,
            model: built.provider.model(),
            caps,
            tool_smoke: None,
        };
        let smoke = if fly_ask::tier::smoke_worth_running(inputs) {
            let t = Instant::now();
            let r = fly_ask::tool_smoke(built.provider.as_ref()).await;
            Some(json!({"result": r, "ms": t.elapsed().as_millis() as u64}))
        } else {
            None
        };
        let smoke_wires = proxy.take();

        let model_dir = out.join(model.replace([':', '/', '\\'], "_"));
        std::fs::create_dir_all(&model_dir).expect("out dir");
        let bench_dir = out
            .join("_bench")
            .join(model.replace([':', '/', '\\'], "_"));
        std::fs::create_dir_all(&bench_dir).expect("bench dir");
        let rows_path = bench_dir.join("rows.jsonl");
        let mut rows = if resume {
            read_rows(&rows_path)
        } else {
            Vec::new()
        };
        if !resume {
            let _ = std::fs::remove_file(&rows_path);
        }
        let done_ids: std::collections::BTreeSet<String> = rows
            .iter()
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect();

        // This tier's scenarios; quality ones stratified, then in file order.
        let at_tier: Vec<&&c::Scenario> =
            wanted.iter().filter(|s| s.tier == tier_of(tier)).collect();
        let position = |s: &&&c::Scenario| at_tier.iter().position(|x| x.id == s.id).unwrap_or(0);
        let answerable: Vec<&&&c::Scenario> = at_tier
            .iter()
            .filter(|s| s.kind == ScenarioKind::Quality && matches!(s.expect, Expect::Answer(_)))
            .collect();
        let unanswerable: Vec<&&&c::Scenario> = at_tier
            .iter()
            .filter(|s| s.kind == ScenarioKind::Quality && matches!(s.expect, Expect::NotFound))
            .collect();
        let mut chosen: Vec<&&&c::Scenario> = match sample {
            Some((a, u)) => spread(&answerable, a)
                .into_iter()
                .chain(spread(&unanswerable, u))
                .collect(),
            None => answerable
                .iter()
                .chain(unanswerable.iter())
                .copied()
                .collect(),
        };
        chosen.extend(at_tier.iter().filter(|s| s.kind != ScenarioKind::Quality));
        chosen.sort_by_key(|s| position(s));
        chosen.truncate(limit);
        eprintln!(
            "ask_live: {model}: tier {tier:?} (measured {measured:?}, {tier_ms} ms), {} scenarios ({} answerable, {} unanswerable in the contract) → {}",
            chosen.len(),
            answerable.len(),
            unanswerable.len(),
            model_dir.display()
        );

        let inner: Arc<dyn fly_llm::LLMProvider> = Arc::from(built.provider);
        if cold_start {
            unload_model(&http, model).await;
        }
        let samples = sampler::Sampler::start(OLLAMA);
        let t_all = Instant::now();
        for (i, s) in chosen.iter().enumerate() {
            if done_ids.contains(&s.id) {
                continue;
            }
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
            let history: Vec<fly_ask::Turn> = s
                .history
                .iter()
                .map(|t| fly_ask::Turn {
                    question: t.question.clone(),
                    answer: db.substitute_str(&t.answer).expect("history answer"),
                    citations: t.citations.iter().map(|c| db.seg_ids[c].clone()).collect(),
                    on_device: t.on_device,
                })
                .collect();
            let first = (i == 0 && cold_start && done_ids.is_empty()) || cold_every;
            // Wait for a quiet machine when asked to, then (cold runs) drop
            // the model, so the Ask pays the load.
            let (quiet_wait_ms, cpu_before) = if quiet_max.is_zero() {
                (0, f32::NAN)
            } else {
                wait_quiet(quiet_max, quiet_below).await
            };
            if cold_every {
                unload_model(&http, model).await;
            }
            let t0 = Instant::now();
            let asked = tokio::time::timeout(
                timeout,
                fly_ask::ask(
                    fly_ask::AskEnv {
                        store: &db.storage,
                        provider: &rec,
                        on_device: built.on_device,
                        tier,
                        limits: Some(tier.limits()),
                        embedder: Some(&Embedder),
                        events: None,
                        cancel: None,
                        external: None,
                    },
                    fly_ask::AskInput {
                        question: &s.question,
                        history: &history,
                        scope,
                        now,
                    },
                ),
            )
            .await;
            let wall = t0.elapsed().as_millis() as u64;
            let wires = proxy.take();
            let kind = match s.expect {
                Expect::Answer(_) => "answerable",
                Expect::NotFound => "unanswerable",
                _ => "other",
            };
            let mut row = json!({
                "id": s.id, "kind": kind, "scenario_kind": kind_name(s.kind), "cold": first,
                "wall_ms": wall, "wires": wires, "timed_out": asked.is_err(),
                "quiet_wait_ms": quiet_wait_ms,
                "cpu_before_pct": (!cpu_before.is_nan()).then_some(cpu_before),
                "status": "timed_out", "sentences": 0, "sourced": 0,
            });
            let line = match asked {
                Err(_) => format!(
                    "  {:<30} {:>7} ms  TIMED OUT after {} s",
                    s.id,
                    wall,
                    timeout.as_secs()
                ),
                Ok(a) => {
                    let sourced = a
                        .sentences
                        .iter()
                        .filter(|x| x.status == fly_ask::SentenceStatus::Sourced)
                        .count();
                    let run_log = serde_json::to_value(&a.run_log).unwrap_or(Value::Null);
                    row["status"] = serde_json::to_value(a.status).unwrap();
                    row["sentences"] = json!(a.sentences.len());
                    row["sourced"] = json!(sourced);
                    row["failure"] = a
                        .failure
                        .as_ref()
                        .map_or(Value::Null, |f| serde_json::to_value(f.kind).unwrap());
                    row["first_token_ms"] = json!(a.timing.first_token_ms);
                    row["total_ms"] = json!(a.timing.total_ms);
                    row["proposals"] = json!(a.proposals.len());
                    row["context_fallback"] =
                        json!(run_log.to_string().contains("context_fallback\":true"));
                    row["run_log"] = run_log;
                    let replay = rec.to_replay(&s.id, tier_of(tier), caps, &normalize);
                    std::fs::write(
                        model_dir.join(format!("{}.json", s.id)),
                        serde_json::to_string_pretty(&replay).unwrap(),
                    )
                    .expect("write replay");
                    format!(
                        "  {:<30} {:>7} ms  {:?}  {} sentence(s), {sourced} sourced",
                        s.id,
                        wall,
                        a.status,
                        a.sentences.len()
                    )
                }
            };
            eprintln!("{line}{}", if first { "  [cold]" } else { "" });
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&rows_path)
                .unwrap();
            writeln!(f, "{row}").unwrap();
            rows.push(row);
        }
        let resources = samples.finish().await;
        // Leave nothing of ours loaded for the next model's run.
        unload_model(&http, model).await;
        let mut entry = json!({
            "model": model,
            "tier": format!("{tier:?}"),
            "measured_tier": format!("{measured:?}"),
            "tier_decision_ms": tier_ms,
            "tool_smoke": smoke,
            "capabilities": serde_json::to_value(caps).unwrap(),
            "resumed": resume,
            "ollama": ollama_version,
            "machine": machine(),
            "idle": idle,
            "resources": resources,
            "run_wall_s": t_all.elapsed().as_secs(),
            "contract": {"answerable": answerable.len(), "unanswerable": unanswerable.len()},
            "tier_decision_wires": tier_wires,
            "smoke_wires": smoke_wires,
        });
        entry["results"] = summarize(&rows);
        std::fs::write(
            bench_dir.join("bench.json"),
            serde_json::to_string_pretty(&entry).unwrap(),
        )
        .expect("write bench.json");
        eprintln!("ask_live: {model}: {}", entry["results"]);
        summary.insert(model.clone(), entry["results"].clone());
    }
    std::fs::create_dir_all(&out).ok();
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_string_pretty(&summary).unwrap(),
    )
    .expect("write summary");
    eprintln!("ask_live: summary → {}", out.join("summary.json").display());
}
