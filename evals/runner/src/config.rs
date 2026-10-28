//! `evals/thresholds.toml`: each suite's status, gates and settings.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    pub schema: u32,
    pub retrieval: SuiteConfig,
    pub ask_contract: SuiteConfig,
    pub local_bench: SuiteConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A miss fails the run.
    Required,
    /// Runs and reports; never fails the run.
    Pending,
    /// Numbers only.
    ReportOnly,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Required => "required",
            Status::Pending => "pending",
            Status::ReportOnly => "report_only",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteConfig {
    pub status: Status,
    #[serde(default)]
    pub baseline_tolerance: Option<f64>,
    #[serde(default)]
    pub gates: BTreeMap<String, Gate>,
    /// retrieval: passages per question.
    #[serde(default)]
    pub k: Option<usize>,
    /// retrieval: the CI embedder id.
    #[serde(default)]
    pub embedder: Option<String>,
    /// local_bench: allowed DER rise over the committed baseline.
    #[serde(default)]
    pub der_tolerance: Option<f64>,
}

/// `{ min = x }` or `{ max = x }`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gate {
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

impl Gate {
    pub fn passes(&self, v: f64) -> bool {
        self.min.is_none_or(|m| v >= m) && self.max.is_none_or(|m| v <= m)
    }

    /// Worse than `baseline` by more than `tol`, in the gate's direction.
    pub fn regressed(&self, v: f64, baseline: f64, tol: f64) -> bool {
        (self.min.is_some() && v < baseline - tol) || (self.max.is_some() && v > baseline + tol)
    }

    pub fn describe(&self) -> String {
        match (self.min, self.max) {
            (Some(a), Some(b)) => format!("{a} to {b}"),
            (Some(a), None) => format!(">= {a}"),
            (None, Some(b)) => format!("<= {b}"),
            (None, None) => "-".into(),
        }
    }
}

impl Thresholds {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let t: Thresholds =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        if t.schema != 1 {
            bail!(
                "thresholds.toml schema {} is not supported (expected 1)",
                t.schema
            );
        }
        for (name, suite) in [
            ("retrieval", &t.retrieval),
            ("ask_contract", &t.ask_contract),
            ("local_bench", &t.local_bench),
        ] {
            for (metric, gate) in &suite.gates {
                if gate.min.is_none() && gate.max.is_none() {
                    bail!("{name}.gates.{metric} has neither min nor max");
                }
            }
        }
        if t.retrieval.k != Some(8) {
            bail!("retrieval.k must be 8 (the Ask prefetch size); the metric names say @8");
        }
        Ok(t)
    }
}
