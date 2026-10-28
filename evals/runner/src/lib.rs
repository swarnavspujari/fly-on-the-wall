//! fly-evals: Fly on the Wall's system evals (see `evals/README.md`).
//!
//! * [`retrieval`]: model-free retrieval over the synthetic corpus, through
//!   `fly-context`'s public API. Required, runs in CI.
//! * [`ask_contract`]: the contract the Ask harness must meet, scored from
//!   what the harness actually sent to a request-recording mock provider.
//!   Pending until the harness lands.
//! * [`local_bench`]: ASR WER/DER gates and latency on this machine. Never
//!   in CI; report-only.
//!
//! Everything under `evals/` (this crate included) is immutable to AI
//! agents. The runner never writes under `evals/`.

pub mod ask_contract;
pub mod claims;
pub mod config;
pub mod corpus;
pub mod embed;
pub mod harnesses;
pub mod local_bench;
pub mod report;
pub mod retrieval;

use std::path::{Path, PathBuf};

/// The `evals/` folder this crate lives in (fixed at compile time, so a
/// run can't be pointed at a different, laxer copy).
pub fn evals_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("evals/runner has a parent")
        .to_path_buf()
}

/// The repository root (the parent of `evals/`).
pub fn repo_root() -> PathBuf {
    evals_dir()
        .parent()
        .expect("evals/ has a parent")
        .to_path_buf()
}

/// Where results go: `<target>/evals/`, never under `evals/`.
pub fn results_dir() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|p| {
            if p.is_absolute() {
                p
            } else {
                repo_root().join(p)
            }
        })
        .unwrap_or_else(|| repo_root().join("target"));
    target.join("evals")
}
