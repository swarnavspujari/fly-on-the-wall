//! Local benchmarks (never in CI). Filled in by a later commit.

use std::path::Path;

use crate::config::SuiteConfig;
use crate::report::SuiteResult;

pub fn run(_evals_dir: &Path, _cfg: &SuiteConfig) -> SuiteResult {
    SuiteResult::not_run("local_bench", "not implemented yet")
}
