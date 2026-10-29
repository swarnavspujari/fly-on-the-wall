//! The registered Ask harness, if any.
//!
//! Wiring the real harness is a locked change (evals/README.md, "Wiring
//! the harness"): the user adds the harness crate as a dependency of
//! fly-evals and returns its adapter here, then flips `ask_contract` to
//! `required` in thresholds.toml.

use crate::ask_contract::AskHarness;

/// `None` until the Ask harness (spec v2.5 item 3.3) lands.
pub fn registered() -> Option<Box<dyn AskHarness>> {
    None
}
