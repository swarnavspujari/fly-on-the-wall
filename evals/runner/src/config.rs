//! `evals/thresholds.toml`: each suite's status, gates and settings.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::ask_contract::{AskBudget, Tier};

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
    /// Metrics with no absolute gate here that must not regress past the
    /// committed baseline.
    #[serde(default)]
    pub tracked: BTreeMap<String, Direction>,
    /// retrieval: passages per question.
    #[serde(default)]
    pub k: Option<usize>,
    /// retrieval: the CI embedder id.
    #[serde(default)]
    pub embedder: Option<String>,
    /// retrieval: the app's real embedder and its absolute gates.
    #[serde(default)]
    pub real_embedder: Option<RealEmbedder>,
    /// ask_contract: per-tier budgets and quality floors.
    #[serde(default)]
    pub tiers: BTreeMap<Tier, TierConfig>,
    /// ask_contract: the scenarios a quality group needs before it's gated.
    #[serde(default)]
    pub group_floor: Option<GroupFloor>,
    /// local_bench: allowed DER rise over the committed baseline.
    #[serde(default)]
    pub der_tolerance: Option<f64>,
}

/// Which way is better for a tracked metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Higher,
    Lower,
}

impl Direction {
    /// Worse than `baseline` by more than `tol`.
    pub fn regressed(self, v: f64, baseline: f64, tol: f64) -> bool {
        match self {
            Direction::Higher => v < baseline - tol,
            Direction::Lower => v > baseline + tol,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealEmbedder {
    /// `ollama:<model>`.
    pub embedder: String,
    pub gates: BTreeMap<String, Gate>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierConfig {
    pub budget: AskBudget,
    pub gates: BTreeMap<String, Gate>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupFloor {
    pub answerable: usize,
    pub unanswerable: usize,
}

/// The per-group quality metrics a tier may gate.
pub const GROUP_METRICS: &[&str] = &[
    "unsourced_sentence_rate",
    "grounded_answer_rate",
    "correct_refusal_rate",
    "false_refusal_rate",
    "must_cite_request_shortfall",
];

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
        if t.schema != 2 {
            bail!(
                "thresholds.toml schema {} is not supported (expected 2)",
                t.schema
            );
        }
        let mut all_gates: Vec<(String, &BTreeMap<String, Gate>)> = vec![
            ("retrieval".into(), &t.retrieval.gates),
            ("ask_contract".into(), &t.ask_contract.gates),
            ("local_bench".into(), &t.local_bench.gates),
        ];
        if let Some(r) = &t.retrieval.real_embedder {
            if !r.embedder.starts_with("ollama:") {
                bail!("retrieval.real_embedder.embedder must be ollama:<model>");
            }
            all_gates.push(("retrieval.real_embedder".into(), &r.gates));
        }
        for (tier, tc) in &t.ask_contract.tiers {
            for metric in tc.gates.keys() {
                if !GROUP_METRICS.contains(&metric.as_str()) {
                    bail!(
                        "ask_contract.tiers.{}.gates.{metric} is not a group metric ({})",
                        tier.as_str(),
                        GROUP_METRICS.join(", ")
                    );
                }
            }
            all_gates.push((format!("ask_contract.tiers.{}", tier.as_str()), &tc.gates));
        }
        if !Tier::ALL
            .iter()
            .all(|x| t.ask_contract.tiers.contains_key(x))
        {
            bail!("ask_contract needs a [ask_contract.tiers.<T>] table for S, M and C");
        }
        if t.ask_contract.group_floor.is_none() {
            bail!("ask_contract needs [ask_contract.group_floor]");
        }
        for (name, gates) in all_gates {
            for (metric, gate) in gates {
                if gate.min.is_none() && gate.max.is_none() {
                    bail!("{name}.gates.{metric} has neither min nor max");
                }
                // The evidence metrics are report-only by decision of the
                // user (README, "Note passages"): they never gate.
                if metric.starts_with("evidence_") {
                    bail!("{name}.gates.{metric}: evidence_* metrics are report-only and can't have a gate");
                }
            }
        }
        for metric in t.retrieval.tracked.keys() {
            if metric.starts_with("evidence_") {
                bail!("retrieval.tracked.{metric}: evidence_* metrics are report-only");
            }
        }
        if t.retrieval.k != Some(8) {
            bail!("retrieval.k must be 8 (the Ask prefetch size); the metric names say @8");
        }
        Ok(t)
    }
}
