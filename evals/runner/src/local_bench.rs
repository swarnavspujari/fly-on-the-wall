//! Local benchmarks: never in CI, report-only (README, "local_bench").
//!
//! * Retrieval latency over the synthetic corpus: cold (first search on a
//!   freshly opened database) and warm p50 / p95. Always runs.
//! * ASR WER / SA-WER / speed / loops gates and diarization DER, by
//!   wrapping the accuracy harness (`src-tauri/tests/accuracy_harness.rs`):
//!   `FLYONTHEWALL_HARNESS_CORPUS=<manifest>` runs its corpus mode, or
//!   `FLY_EVALS_ASR_RESULTS=<dir>` re-scores existing results. Skips when
//!   neither is set. The corpus never lives in the repo.
//! * Ask latency / tokens / cost: pending until a harness is registered.
//! * Contention (spec §4.2a #3): not implemented yet.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context as _, Result};
use fly_context::{Context, QueryVector, Visibility};
use serde_json::{json, Value};

use crate::config::SuiteConfig;
use crate::corpus::{Corpus, Db};
use crate::report::SuiteResult;
use crate::retrieval::{scoped_args, GoldSet, K};
use crate::{embed, repo_root};

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

fn retrieval_latency(evals_dir: &Path) -> Result<(Vec<f64>, Vec<f64>)> {
    let corpus = Corpus::load(&evals_dir.join("fixtures"))?;
    let gold = GoldSet::load(&evals_dir.join("suites/retrieval/questions.toml"))?;
    let db = Db::build(&corpus, true)?;
    let search = |storage: &fly_storage::Storage, q: &str| -> Result<f64> {
        let ctx = Context::new(storage, Visibility::All);
        let v = embed::embed(q);
        let t = Instant::now();
        ctx.search_passages(
            &scoped_args(&db, q, None, K),
            Some(QueryVector {
                vector: &v,
                model: embed::MODEL,
            }),
        )?;
        Ok(t.elapsed().as_secs_f64() * 1000.0)
    };
    let mut cold = Vec::new();
    for q in gold.answerable.iter().take(10) {
        let fresh = db.reopen()?;
        cold.push(search(&fresh, &q.question)?);
    }
    for q in &gold.answerable {
        search(&db.storage, &q.question)?;
    }
    let mut warm = Vec::new();
    for _ in 0..3 {
        for q in &gold.answerable {
            warm.push(search(&db.storage, &q.question)?);
        }
    }
    Ok((cold, warm))
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

    // DER against the committed diarization baselines, by fixture name.
    let mut der = Vec::new();
    let base_dir = repo_root().join("docs").join("data").join("diarization");
    for row in summary["rows"].as_array().cloned().unwrap_or_default() {
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
    }))
}

pub fn run(evals_dir: &Path, cfg: &SuiteConfig) -> SuiteResult {
    let mut res = SuiteResult::new("local_bench");
    match retrieval_latency(evals_dir) {
        Ok((cold, warm)) => {
            for (name, s) in [("cold", &cold), ("warm", &warm)] {
                res.metrics.insert(
                    format!("retrieval_{name}_p50_ms"),
                    (percentile(s, 50.0) * 100.0).round() / 100.0,
                );
                res.metrics.insert(
                    format!("retrieval_{name}_p95_ms"),
                    (percentile(s, 95.0) * 100.0).round() / 100.0,
                );
            }
            res.info.insert(
                "retrieval_latency".into(),
                json!({ "cold_samples": cold.len(), "warm_samples": warm.len(), "build": if cfg!(debug_assertions) { "debug" } else { "release" } }),
            );
        }
        Err(e) => res.errors.push(format!("retrieval latency: {e:#}")),
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
            res.info.insert("asr".into(), v);
        }
        Err(e) => res.errors.push(format!("asr: {e:#}")),
    }
    res.info.insert(
        "ask_latency".into(),
        json!(if crate::harnesses::registered().is_some() {
            "not wired yet: needs a real provider from the app (build_provider), recorded from an app-side test"
        } else {
            "pending: no Ask harness registered"
        }),
    );
    res.info.insert(
        "contention".into(),
        json!("not implemented yet (spec §4.2a #3: Ask p95 while recording, captions and backfill run)"),
    );
    res.note = Some("Report-only: never blocks, never runs in CI.".into());
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
