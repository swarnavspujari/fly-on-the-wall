//! `cargo run -p fly-evals -- --suite <retrieval|ask_contract|local_bench|ci|all>
//!  [--strict | --gate <strict|security|quality>] [--embedder <id>] [--json]`
//!
//! Gating (README, "Gating"): `strict` (the default, and what a release
//! requires) fails on any absolute miss or baseline regression of a
//! required suite; `security` only on the security gates; `quality` only on
//! regressions against the committed baseline. Integrity gates and scorer
//! errors fail every mode.
//!
//! Retrieval runs twice under `strict`: `retrieval` with the CI embedder and
//! `retrieval_real` with the app's embedder through Ollama, which carries
//! the absolute quality gates and is required for a release (it reports
//! "did not run" when Ollama or the model is missing). `--embedder
//! ollama:<model>` runs `retrieval_real` in any mode, with that model.
//!
//! Exit codes: 0 pass; 1 a suite blocks under the chosen gating; 2 bad
//! usage or unreadable thresholds. The last thing a run does is write
//! `target/evals/verdict.json` (deleted at the start), so CI can tell a
//! finished run from one that exited early.

use std::process::ExitCode;

use fly_evals::config::{Status, SuiteConfig, Thresholds};
use fly_evals::embedder::{Embedder, Unavailable};
use fly_evals::report::{judge_with, load_baseline, Gating, Judged, SuiteResult};
use fly_evals::{ask_contract, evals_dir, local_bench, results_dir, retrieval};
use serde_json::json;

const USAGE: &str = "usage: cargo run -p fly-evals -- --suite <retrieval|ask_contract|local_bench|ci|all> [--strict | --gate <strict|security|quality>] [--embedder <hashed-bow-v1|ollama:MODEL>] [--json]";

fn main() -> ExitCode {
    let mut suite: Option<String> = None;
    let mut gating = Gating::Strict;
    let mut print_json = false;
    let mut embedder: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let gate_value = |v: Option<String>| match v.as_deref() {
            Some("strict") => Some(Gating::Strict),
            Some("security") => Some(Gating::Security),
            Some("quality") => Some(Gating::Quality),
            _ => None,
        };
        match a.as_str() {
            "--suite" => suite = args.next(),
            s if s.starts_with("--suite=") => suite = Some(s["--suite=".len()..].to_string()),
            "--strict" => gating = Gating::Strict,
            "--gate" => match gate_value(args.next()) {
                Some(g) => gating = g,
                None => {
                    eprintln!("--gate takes strict, security or quality\n{USAGE}");
                    return ExitCode::from(2);
                }
            },
            s if s.starts_with("--gate=") => match gate_value(Some(s["--gate=".len()..].into())) {
                Some(g) => gating = g,
                None => {
                    eprintln!("--gate takes strict, security or quality\n{USAGE}");
                    return ExitCode::from(2);
                }
            },
            "--json" => print_json = true,
            "--embedder" => embedder = args.next(),
            s if s.starts_with("--embedder=") => embedder = Some(s["--embedder=".len()..].into()),
            "-h" | "--help" => {
                println!("{USAGE}\n\nSee evals/README.md.");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument {other:?}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(suite) = suite else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let mut names: Vec<&str> = match suite.as_str() {
        "retrieval" | "ask_contract" | "local_bench" => vec![suite.as_str()],
        "ci" => vec!["retrieval", "ask_contract"],
        "all" => vec!["retrieval", "ask_contract", "local_bench"],
        other => {
            eprintln!("unknown suite {other:?}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let real_requested = embedder
        .as_deref()
        .is_some_and(|e| e.starts_with("ollama:"));
    if names.contains(&"retrieval") && (gating == Gating::Strict || real_requested) {
        names.insert(1, "retrieval_real");
    }

    let out = results_dir();
    let verdict_path = out.join("verdict.json");
    let _ = std::fs::remove_file(&verdict_path);

    let dir = evals_dir();
    let thresholds = match Thresholds::load(&dir.join("thresholds.toml")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("fly-evals: {e:#}");
            return ExitCode::from(2);
        }
    };
    println!(
        "fly-evals {} ({} gating) — evals at {}",
        env!("CARGO_PKG_VERSION"),
        gating.as_str(),
        dir.display()
    );

    let mut judged: Vec<Judged> = Vec::new();
    for name in &names {
        let (cfg, result): (SuiteConfig, SuiteResult) = match *name {
            "retrieval" => {
                let spec = thresholds.retrieval.embedder.clone().unwrap_or_default();
                let result = match Embedder::from_spec(&spec) {
                    Ok(e) if !e.is_real() => retrieval::run(&dir, &e, "retrieval"),
                    Ok(_) => SuiteResult::failed(
                        "retrieval",
                        "retrieval.embedder must be the CI embedder",
                    ),
                    Err(Unavailable(why)) => SuiteResult::failed("retrieval", why),
                };
                (thresholds.retrieval.clone(), result)
            }
            "retrieval_real" => {
                let Some(real) = &thresholds.retrieval.real_embedder else {
                    eprintln!("fly-evals: thresholds.toml has no [retrieval.real_embedder]");
                    return ExitCode::from(2);
                };
                // Absolute quality gates, no CI-embedder baseline; required
                // only for a release (strict).
                let mut cfg = thresholds.retrieval.clone();
                cfg.status = if gating == Gating::Strict {
                    Status::Required
                } else {
                    Status::ReportOnly
                };
                cfg.gates = real.gates.clone();
                cfg.tracked.clear();
                let spec = embedder
                    .clone()
                    .filter(|_| real_requested)
                    .unwrap_or(real.embedder.clone());
                let result = match Embedder::from_spec(&spec) {
                    Ok(e) => retrieval::run(&dir, &e, "retrieval_real"),
                    Err(Unavailable(why)) => SuiteResult::not_run(
                        "retrieval_real",
                        format!("skipped: {why}. A release (--strict) needs this run."),
                    ),
                };
                (cfg, result)
            }
            "ask_contract" => (
                thresholds.ask_contract.clone(),
                ask_contract::run(&dir, &thresholds.ask_contract),
            ),
            _ => (
                thresholds.local_bench.clone(),
                local_bench::run(&dir, &thresholds.local_bench),
            ),
        };
        let mut result = result;
        // A required suite needs a usable committed baseline, or quality
        // gating would compare against nothing. (The real-embedder run can't
        // happen in CI; its baseline is optional.)
        let baseline = match load_baseline(&dir.join("baselines").join(format!("{name}.json"))) {
            Ok(Some(b)) => Some(b),
            Ok(None) => {
                if cfg.status == Status::Required && *name != "retrieval_real" {
                    result.errors.push(format!(
                        "required suite has no committed baseline (evals/baselines/{name}.json)"
                    ));
                }
                None
            }
            Err(e) => {
                if cfg.status == Status::Required {
                    result.errors.push(format!("baseline unusable: {e}"));
                }
                None
            }
        };
        let j = judge_with(result, &cfg, baseline.as_ref(), gating);
        print!("{}", j.table());
        judged.push(j);
    }

    let mut summary = format!("## System evals ({} gating)\n\n", gating.as_str());
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("fly-evals: can't create {}: {e}", out.display());
    }
    for j in &judged {
        summary.push_str(&j.markdown());
        let path = out.join(format!("{}.json", j.result.suite));
        let body = serde_json::to_string_pretty(&j.result).unwrap_or_default() + "\n";
        if let Err(e) = std::fs::write(&path, body) {
            eprintln!("fly-evals: can't write {}: {e}", path.display());
        }
    }
    let _ = std::fs::write(out.join("summary.md"), &summary);
    if print_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&judged).unwrap_or_default()
        );
    }

    let blocking: Vec<&str> = judged
        .iter()
        .filter(|j| j.blocks)
        .map(|j| j.result.suite.as_str())
        .collect();
    let pass = blocking.is_empty();
    // Written last: a run that exits before this point leaves no verdict.
    let verdict = json!({
        "complete": true,
        "gating": gating.as_str(),
        "suite": suite,
        "pass": pass,
        "suites": judged.iter().map(|j| json!({
            "suite": j.result.suite,
            "status": j.status.as_str(),
            "ran": j.result.ran,
            "outcome": j.outcome(),
            "blocks": j.blocks,
            "blocking": j.blocking,
        })).collect::<Vec<_>>(),
    });
    if let Err(e) = std::fs::write(
        &verdict_path,
        serde_json::to_string_pretty(&verdict).unwrap_or_default() + "\n",
    ) {
        eprintln!("fly-evals: can't write {}: {e}", verdict_path.display());
        return ExitCode::from(1);
    }

    let required = judged
        .iter()
        .filter(|j| j.status == Status::Required)
        .count();
    println!("\nresults: {}", out.display());
    if pass {
        println!(
            "fly-evals: OK ({required} required suite(s), {} gating)",
            gating.as_str()
        );
        ExitCode::SUCCESS
    } else {
        println!(
            "fly-evals: FAILED ({} gating): {}",
            gating.as_str(),
            blocking.join(", ")
        );
        ExitCode::from(1)
    }
}
