//! `cargo run -p fly-evals -- --suite <retrieval|ask_contract|local_bench|ci|all> [--json]`
//!
//! Exit codes: 0 every required suite passed; 1 a required suite missed a
//! gate, regressed past its baseline, or couldn't run; 2 bad usage or
//! unreadable thresholds.

use std::process::ExitCode;

use fly_evals::config::{Status, Thresholds};
use fly_evals::report::{judge, load_baseline, Judged, SuiteResult};
use fly_evals::{ask_contract, evals_dir, local_bench, results_dir, retrieval};

const USAGE: &str =
    "usage: cargo run -p fly-evals -- --suite <retrieval|ask_contract|local_bench|ci|all> [--json]";

fn main() -> ExitCode {
    let mut suite: Option<String> = None;
    let mut print_json = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--suite" => suite = args.next(),
            s if s.starts_with("--suite=") => suite = Some(s["--suite=".len()..].to_string()),
            "--json" => print_json = true,
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
    let names: Vec<&str> = match suite.as_str() {
        "retrieval" | "ask_contract" | "local_bench" => vec![suite.as_str()],
        "ci" => vec!["retrieval", "ask_contract"],
        "all" => vec!["retrieval", "ask_contract", "local_bench"],
        other => {
            eprintln!("unknown suite {other:?}\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let dir = evals_dir();
    let thresholds = match Thresholds::load(&dir.join("thresholds.toml")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("fly-evals: {e:#}");
            return ExitCode::from(2);
        }
    };
    println!(
        "fly-evals {} — evals at {}",
        env!("CARGO_PKG_VERSION"),
        dir.display()
    );

    let mut judged: Vec<Judged> = Vec::new();
    for name in names {
        let (cfg, result): (_, SuiteResult) = match name {
            "retrieval" => (
                &thresholds.retrieval,
                retrieval::run(
                    &dir,
                    thresholds.retrieval.embedder.as_deref().unwrap_or_default(),
                ),
            ),
            "ask_contract" => (&thresholds.ask_contract, ask_contract::run(&dir)),
            _ => (
                &thresholds.local_bench,
                local_bench::run(&dir, &thresholds.local_bench),
            ),
        };
        let baseline = load_baseline(&dir.join("baselines").join(format!("{name}.json")));
        let j = judge(result, cfg, baseline.as_ref());
        print!("{}", j.table());
        judged.push(j);
    }

    let out = results_dir();
    let mut summary = String::from("## System evals\n\n");
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
    let required = judged
        .iter()
        .filter(|j| j.status == Status::Required)
        .count();
    println!("\nresults: {}", out.display());
    if blocking.is_empty() {
        println!("fly-evals: OK ({required} required suite(s) passed)");
        ExitCode::SUCCESS
    } else {
        println!("fly-evals: FAILED: {}", blocking.join(", "));
        ExitCode::from(1)
    }
}
