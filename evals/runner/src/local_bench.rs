//! Local benchmarks: never in CI, report-only (README, "local_bench").
//! Run them with `--release`; a debug build's timings mean little.
//!
//! * **Retrieval latency** over the fixture corpus: the same questions cold
//!   (the first search on a freshly opened connection, in this process:
//!   "in-process cold", not a new process) and warm (three passes after a
//!   warm-up), every answerable question, so p95 has 100+ samples. The
//!   query-embedding call is timed separately (hashed, and the real
//!   embedder when Ollama has it).
//! * **Scale**: seeded synthetic libraries of 200 and 1,000 meetings with
//!   768-dimension vectors (`synth`). Timings are reported; the per-query
//!   work counts (vector rows scanned, candidate meetings whose transcripts
//!   a search loads) are deterministic for a given seed.
//! * **ASR** WER / SA-WER / speed / loops gates and diarization DER, by
//!   wrapping the accuracy harness (`src-tauri/tests/accuracy_harness.rs`):
//!   `FLYONTHEWALL_HARNESS_CORPUS=<manifest>` runs its corpus mode, or
//!   `FLY_EVALS_ASR_RESULTS=<dir>` re-scores existing results. Speed
//!   (×realtime), peak sidecar RSS and machine CPU pass through, with a
//!   machine fingerprint. Skips when neither is set; the corpus never
//!   lives in the repo.
//! * **Placeholders** (reported as pending, with what they'll read):
//!   contention, backfill / time-to-searchable with the real embedder,
//!   first-launch migration time, Ask latency per tier.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context as _, Result};
use fly_context::{Context, QueryVector, Visibility};
use serde_json::{json, Value};

use crate::config::SuiteConfig;
use crate::corpus::{Corpus, Db};
use crate::embedder::Embedder;
use crate::report::SuiteResult;
use crate::retrieval::{scoped_args, GoldSet, K};
use crate::{embed, repo_root, synth};

/// Nearest-rank percentile of samples (milliseconds).
pub fn percentile(samples: &[f64], p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * s.len() as f64).ceil().max(1.0) as usize;
    s[rank.min(s.len()) - 1]
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// p50 / p95 of `samples` as `<prefix>_p50_ms` / `<prefix>_p95_ms`.
fn put_percentiles(res: &mut SuiteResult, prefix: &str, samples: &[f64]) {
    res.metrics.insert(
        format!("{prefix}_p50_ms"),
        round2(percentile(samples, 50.0)),
    );
    res.metrics.insert(
        format!("{prefix}_p95_ms"),
        round2(percentile(samples, 95.0)),
    );
}

fn retrieval_latency(evals_dir: &Path, res: &mut SuiteResult) -> Result<()> {
    let corpus = Corpus::load(&evals_dir.join("fixtures"))?;
    let gold = GoldSet::load(&evals_dir.join("suites/retrieval/questions.toml"))?;
    let db = Db::build(&corpus, true)?;
    let questions: Vec<&str> = gold
        .answerable
        .iter()
        .map(|q| q.question.as_str())
        .collect();

    // The query embedding, timed on its own.
    let mut embed_ms = Vec::new();
    let mut vectors = Vec::new();
    for q in &questions {
        let t = Instant::now();
        vectors.push(embed::embed(q));
        embed_ms.push(ms(t));
    }
    put_percentiles(res, "query_embed_hashed", &embed_ms);
    match Embedder::from_spec(&format!("ollama:{}", fly_core::embed_prompt::EMBED_MODEL)) {
        Ok(real) => {
            let mut real_ms = Vec::new();
            for q in &questions {
                let t = Instant::now();
                real.query(q)?;
                real_ms.push(ms(t));
            }
            put_percentiles(res, "query_embed_ollama", &real_ms);
        }
        Err(e) => {
            res.info.insert(
                "query_embed_ollama".into(),
                json!(format!("skipped: {}", e.0)),
            );
        }
    }

    let search = |storage: &fly_storage::Storage, q: &str, v: &[f32]| -> Result<f64> {
        let ctx = Context::new(storage, Visibility::All);
        let t = Instant::now();
        ctx.search_passages(
            &scoped_args(&db, q, None, K),
            Some(QueryVector {
                vector: v,
                model: embed::MODEL,
            }),
        )?;
        Ok(ms(t))
    };
    // Cold, in-process: each question is the first search on a fresh
    // connection (no page cache in SQLite's own cache; the OS cache stays).
    let mut cold = Vec::new();
    for (q, v) in questions.iter().zip(&vectors) {
        let fresh = db.reopen()?;
        cold.push(search(&fresh, q, v)?);
    }
    for (q, v) in questions.iter().zip(&vectors) {
        search(&db.storage, q, v)?;
    }
    let mut warm = Vec::new();
    for _ in 0..3 {
        for (q, v) in questions.iter().zip(&vectors) {
            warm.push(search(&db.storage, q, v)?);
        }
    }
    put_percentiles(res, "retrieval_cold_inprocess", &cold);
    put_percentiles(res, "retrieval_warm", &warm);
    res.info.insert(
        "retrieval_latency".into(),
        json!({
            "questions": questions.len(),
            "cold_samples": cold.len(),
            "warm_samples": warm.len(),
            "cold": "in-process: the first search on a freshly opened connection",
            "search_excludes_query_embedding": true,
        }),
    );
    Ok(())
}

/// One synthetic library size: build and index time, query timings, and
/// the deterministic work counts per query.
fn scale(evals_dir: &Path, meetings: usize, res: &mut SuiteResult) -> Result<()> {
    const SEED: u64 = 42;
    // fly-context's candidate pools (tools.rs FTS_CANDIDATES,
    // VECTOR_CANDIDATES): a search loads the transcript of every meeting in
    // them.
    const FTS_POOL: usize = 40;
    const VECTOR_POOL: usize = 200;
    let fixtures = Corpus::load(&evals_dir.join("fixtures"))?;
    let gold = GoldSet::load(&evals_dir.join("suites/retrieval/questions.toml"))?;
    let lib = synth::library(&fixtures, meetings, SEED)?;
    let t = Instant::now();
    let db = Db::build(&lib, false)?;
    let build_ms = ms(t);
    let t = Instant::now();
    let chunks = synth::index(&db.storage, SEED)?;
    let index_ms = ms(t);

    let ctx = Context::new(&db.storage, Visibility::All);
    let filter = |limit| fly_storage::SearchFilter {
        folder_id: None,
        since: None,
        until: None,
        limit,
        offset: 0,
        exclude_keep_local: false,
    };
    let mut hybrid = Vec::new();
    let mut keyword = Vec::new();
    let mut loaded = Vec::new();
    let mut rows_scanned = 0usize;
    for (i, q) in gold.answerable.iter().take(30).enumerate() {
        let v = synth::vector(SEED ^ 0xABCD, i as u64);
        let qv = QueryVector {
            vector: &v,
            model: synth::MODEL,
        };
        let args = scoped_args(&db, &q.question, None, K);
        ctx.search_passages(&args, Some(qv))?; // warm-up
        let t = Instant::now();
        ctx.search_passages(&args, Some(qv))?;
        hybrid.push(ms(t));
        let t = Instant::now();
        ctx.search_passages(&args, None)?;
        keyword.push(ms(t));
        // Work: every embedded row is scanned (no vector index); the
        // candidate meetings are loaded and parsed.
        let all = db.storage.vector_search(
            &v,
            synth::MODEL,
            &filter(usize::MAX >> 1),
            usize::MAX >> 1,
        )?;
        rows_scanned = all.len();
        let mut meetings_hit: BTreeSet<String> = all
            .iter()
            .take(VECTOR_POOL)
            .filter_map(|h| h.meeting_id.clone())
            .collect();
        let (_, t_hits) = db.storage.search_split(&q.question, &filter(FTS_POOL))?;
        meetings_hit.extend(t_hits.into_iter().filter_map(|h| h.meeting_id));
        loaded.push(meetings_hit.len() as f64);
    }
    let p = format!("scale_{meetings}");
    put_percentiles(res, &format!("{p}_hybrid"), &hybrid);
    put_percentiles(res, &format!("{p}_keyword_only"), &keyword);
    res.metrics
        .insert(format!("{p}_build_ms"), round2(build_ms));
    res.metrics
        .insert(format!("{p}_index_ms"), round2(index_ms));
    res.metrics
        .insert(format!("{p}_vector_rows_scanned"), rows_scanned as f64);
    res.metrics.insert(
        format!("{p}_transcripts_loaded_mean"),
        round2(loaded.iter().sum::<f64>() / loaded.len().max(1) as f64),
    );
    res.metrics.insert(
        format!("{p}_transcripts_loaded_max"),
        loaded.iter().copied().fold(0.0, f64::max),
    );
    res.info.insert(
        p,
        json!({
            "meetings": lib.meetings.len(),
            "chunks": chunks,
            "dims": synth::DIMS,
            "seed": SEED,
            "queries": hybrid.len(),
            "vectors": "random (synthetic text): cost is realistic, ranking is not",
        }),
    );
    Ok(())
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

fn accuracy_harness(envs: &[(&str, String)]) -> Result<()> {
    let mut cmd = Command::new(cargo());
    cmd.current_dir(repo_root()).args([
        "test",
        "-p",
        "fly-app",
        "--release",
        "--test",
        "accuracy_harness",
        "--",
        "--ignored",
        "--nocapture",
    ]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let status = cmd.status().context("run the accuracy harness")?;
    anyhow::ensure!(
        status.success(),
        "the accuracy harness exited with {status}"
    );
    Ok(())
}

/// Where corpus mode writes results (mirrors the harness's own rule).
fn corpus_out_dir(manifest: &Path) -> PathBuf {
    if let Ok(d) = std::env::var("FLYONTHEWALL_HARNESS_OUT_DIR") {
        return PathBuf::from(d);
    }
    let from_manifest = std::fs::read_to_string(manifest)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("out_dir").and_then(Value::as_str).map(PathBuf::from));
    from_manifest.unwrap_or_else(|| manifest.parent().unwrap_or(Path::new(".")).join("results"))
}

/// What machine produced these numbers (timings mean little without it).
fn machine() -> Value {
    let cpu = std::env::var("PROCESSOR_IDENTIFIER").ok().or_else(|| {
        std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
    });
    let cpu = cpu.or_else(|| {
        Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    });
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "cpu": cpu,
        "build": if cfg!(debug_assertions) { "debug" } else { "release" },
    })
}

fn asr(der_tolerance: f64) -> Result<Value> {
    let results = if let Ok(d) = std::env::var("FLY_EVALS_ASR_RESULTS") {
        PathBuf::from(d)
    } else if let Ok(m) = std::env::var("FLYONTHEWALL_HARNESS_CORPUS") {
        let engines = std::env::var("FLYONTHEWALL_HARNESS_ENGINES")
            .unwrap_or_else(|_| "whisper,parakeet".into());
        accuracy_harness(&[
            ("FLYONTHEWALL_HARNESS_CORPUS", m.clone()),
            ("FLYONTHEWALL_HARNESS_ENGINES", engines),
        ])?;
        corpus_out_dir(Path::new(&m))
    } else {
        return Ok(json!({
            "skipped": "set FLYONTHEWALL_HARNESS_CORPUS (run the corpus) or FLY_EVALS_ASR_RESULTS (re-score a results folder)"
        }));
    };
    // Compare mode prints the table and (re)writes summary.json.
    accuracy_harness(&[(
        "FLYONTHEWALL_HARNESS_COMPARE",
        results.display().to_string(),
    )])?;
    let summary: Value = serde_json::from_str(
        &std::fs::read_to_string(results.join("summary.json")).context("read summary.json")?,
    )?;
    let gates = summary["gates"].as_array().cloned().unwrap_or_default();
    let count = |p: Option<bool>| gates.iter().filter(|g| g["pass"].as_bool() == p).count();
    let failures: Vec<Value> = gates
        .iter()
        .filter(|g| g["pass"].as_bool() == Some(false))
        .cloned()
        .collect();
    let rows = summary["rows"].as_array().cloned().unwrap_or_default();

    // Speed and resources the harness measured, CPU runs only for speed
    // (the spec §4.1 gate is "≥ 5× realtime on CPU").
    let cpu_rows: Vec<&Value> = rows
        .iter()
        .filter(|r| r["gpu"].as_bool() == Some(false))
        .collect();
    let min = |vals: Vec<f64>| vals.into_iter().reduce(f64::min);
    let max = |vals: Vec<f64>| vals.into_iter().reduce(f64::max);
    let speed = min(cpu_rows
        .iter()
        .filter_map(|r| r["speed_x_realtime"].as_f64())
        .collect());
    let rss = max(rows
        .iter()
        .filter_map(|r| r["peak_sidecar_rss_mb"].as_f64())
        .collect());
    let cpu = max(rows
        .iter()
        .filter_map(|r| r["machine_cpu_avg_pct"].as_f64())
        .collect());

    // DER against the committed diarization baselines, by fixture name.
    let mut der = Vec::new();
    let base_dir = repo_root().join("docs").join("data").join("diarization");
    for row in &rows {
        let (Some(fixture), Some(v)) = (row["fixture"].as_str(), row["der_250"].as_f64()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(base_dir.join(format!("{fixture}.json"))) else {
            continue;
        };
        let base: Value = serde_json::from_str(&text)?;
        if let Some(b) = base["reference"]["diarization"]["der_collar_250"]["der"].as_f64() {
            der.push(json!({
                "fixture": fixture,
                "variant": row["variant"],
                "der_250": v,
                "baseline": b,
                "regressed": v > b + der_tolerance,
            }));
        }
    }
    Ok(json!({
        "results_dir": results,
        "gates_pass": count(Some(true)),
        "gates_fail": count(Some(false)),
        "gates_not_measured": count(None),
        "failures": failures,
        "der_vs_baseline": der,
        "speed_x_realtime_cpu_min": speed,
        "peak_sidecar_rss_mb_max": rss,
        "machine_cpu_avg_pct_max": cpu,
        "speed_by_run": rows.iter().map(|r| json!({
            "fixture": r["fixture"],
            "variant": r["variant"],
            "gpu": r["gpu"],
            "speed_x_realtime": r["speed_x_realtime"],
        })).collect::<Vec<_>>(),
    }))
}

/// A result file another benchmark wrote, passed through when its
/// environment variable names it.
fn external(var: &str, pending: &str) -> Value {
    match std::env::var(var) {
        Ok(path) => match std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            Some(v) => json!({ "source": path, "results": v }),
            None => json!({ "error": format!("{var}={path} is not readable JSON") }),
        },
        Err(_) => json!({ "pending": pending, "reads": var }),
    }
}

/// Library sizes for the scale benchmark (`FLY_EVALS_SCALE=200,1000`;
/// `0` skips it).
fn scale_sizes() -> Vec<usize> {
    std::env::var("FLY_EVALS_SCALE")
        .unwrap_or_else(|_| "200,1000".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .filter(|n| *n > 0)
        .collect()
}

pub fn run(evals_dir: &Path, cfg: &SuiteConfig) -> SuiteResult {
    let mut res = SuiteResult::new("local_bench");
    res.info.insert("machine".into(), machine());
    if let Err(e) = retrieval_latency(evals_dir, &mut res) {
        res.errors.push(format!("retrieval latency: {e:#}"));
    }
    for n in scale_sizes() {
        if let Err(e) = scale(evals_dir, n, &mut res) {
            res.errors.push(format!("scale {n}: {e:#}"));
        }
    }
    match asr(cfg.der_tolerance.unwrap_or(0.0)) {
        Ok(v) => {
            if let Some(n) = v["gates_fail"].as_u64() {
                res.metrics.insert("asr_gates_failed".into(), n as f64);
                res.metrics.insert(
                    "asr_gates_passed".into(),
                    v["gates_pass"].as_u64().unwrap_or(0) as f64,
                );
            }
            for (metric, key) in [
                ("asr_speed_x_realtime", "speed_x_realtime_cpu_min"),
                ("asr_peak_rss_mb", "peak_sidecar_rss_mb_max"),
                ("asr_machine_cpu_avg_pct", "machine_cpu_avg_pct_max"),
            ] {
                if let Some(x) = v[key].as_f64() {
                    res.metrics.insert(metric.into(), round2(x));
                }
            }
            res.info.insert("asr".into(), v);
        }
        Err(e) => res.errors.push(format!("asr: {e:#}")),
    }
    // Pending: each names what it will read once it exists.
    res.info.insert(
        "contention".into(),
        external(
            "FLY_EVALS_CONTENTION_RESULTS",
            "spec §4.2a #3: Ask p95 (retrieval, first token) while recording, live captions and an embedding backfill run; caption lag p95 during an Ask. The recording-resilience work adds the bench in src-tauri; point this at its results JSON.",
        ),
    );
    res.info.insert(
        "backfill_time_to_searchable".into(),
        external(
            "FLY_EVALS_BACKFILL_RESULTS",
            "time from a finished meeting to searchable with the real embedder (the scale benchmark's index time uses synthetic vectors)",
        ),
    );
    res.info.insert(
        "first_launch_migration".into(),
        external(
            "FLY_EVALS_MIGRATION_RESULTS",
            "first-launch migration time on a 1,000-meeting schema-v2 database",
        ),
    );
    res.info.insert(
        "ask_latency".into(),
        external(
            "FLY_EVALS_ASK_LATENCY_RESULTS",
            "Ask latency per tier: time to first token and total (AskAnswer.timing), prefill and decode tokens/s, num_ctx fallbacks, peak RAM / VRAM; needs the registered harness and a real provider built by the app (build_provider)",
        ),
    );
    res.note = Some("Report-only: never blocks, never runs in CI. Use --release.".into());
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let s = [5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(percentile(&s, 50.0), 3.0);
        assert_eq!(percentile(&s, 95.0), 5.0);
        assert_eq!(percentile(&[], 50.0), 0.0);
    }
}
