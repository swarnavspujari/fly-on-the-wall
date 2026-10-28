//! The Ask contract suite. Filled in by a later commit.

use std::path::Path;

use crate::report::SuiteResult;

/// The adapter the Ask harness implements (defined in a later commit).
pub trait AskHarness: Send + Sync {}

pub fn run(_evals_dir: &Path) -> SuiteResult {
    SuiteResult::not_run("ask_contract", "not implemented yet")
}
