//! A suite's result: metrics, gate verdicts, the baseline comparison, and
//! the table / JSON / Markdown renderings.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Gate, Status, SuiteConfig};

/// What a suite measured. `metrics` are the gated numbers; `info` is
/// everything reported but not gated.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuiteResult {
    pub suite: String,
    /// False when the suite couldn't measure (e.g. no harness registered,
    /// or a local input is missing). `note` says why.
    pub ran: bool,
    #[serde(default)]
    pub note: Option<String>,
    pub metrics: BTreeMap<String, f64>,
    #[serde(default)]
    pub info: BTreeMap<String, Value>,
    /// Problems that make the run untrustworthy (bad fixtures, a scorer
    /// self-check failure). Any error fails a required suite.
    #[serde(default)]
    pub errors: Vec<String>,
}

impl SuiteResult {
    pub fn new(suite: &str) -> Self {
        Self {
            suite: suite.into(),
            ran: true,
            ..Default::default()
        }
    }

    pub fn not_run(suite: &str, note: impl Into<String>) -> Self {
        Self {
            suite: suite.into(),
            ran: false,
            note: Some(note.into()),
            ..Default::default()
        }
    }

    pub fn failed(suite: &str, error: impl std::fmt::Display) -> Self {
        Self {
            suite: suite.into(),
            ran: false,
            errors: vec![format!("{error:#}")],
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    /// Meets the gate but is worse than the baseline beyond tolerance.
    Regressed,
    /// The suite didn't produce this metric.
    Missing,
    /// Not gated.
    Info,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Regressed => "REGRESSED",
            Verdict::Missing => "MISSING",
            Verdict::Info => "info",
        }
    }
}

/// Which misses fail the run (README, "Gating").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gating {
    /// Every absolute gate and every baseline regression. The default, and
    /// what a release requires (`--strict`).
    Strict,
    /// Only the security gates, absolutely (CI job evals-security).
    Security,
    /// Only regressions against the committed baseline (CI job
    /// evals-quality); absolute misses are reported, not blocking.
    Quality,
}

impl Gating {
    pub fn as_str(self) -> &'static str {
        match self {
            Gating::Strict => "strict",
            Gating::Security => "security",
            Gating::Quality => "quality",
        }
    }
}

/// Privacy and injection gates: a miss is a security failure whatever the
/// quality numbers say.
pub const SECURITY_GATES: &[&str] = &[
    "keep_local_leaks",
    "keep_local_in_cloud_requests",
    "executed_proposals",
    "rendered_links",
    "run_log_free_text",
];

/// Gates that say whether the eval itself means anything (gold-set and
/// scenario floors, scenarios that crashed). Absolute in every mode.
pub const INTEGRITY_GATES: &[&str] = &[
    "answerable_questions",
    "unanswerable_questions",
    "red_team_scenarios",
    "scenarios_errored",
];

/// Does this row's verdict block under `gating`?
fn blocking(gating: Gating, metric: &str, verdict: Verdict) -> bool {
    let absolute = INTEGRITY_GATES.contains(&metric)
        || match gating {
            Gating::Strict => true,
            Gating::Security => SECURITY_GATES.contains(&metric),
            Gating::Quality => false,
        };
    match verdict {
        Verdict::Missing => true,
        Verdict::Fail => absolute,
        Verdict::Regressed => gating != Gating::Security || SECURITY_GATES.contains(&metric),
        Verdict::Pass | Verdict::Info => false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub metric: String,
    pub value: Option<f64>,
    pub gate: Option<Gate>,
    pub baseline: Option<f64>,
    pub verdict: Verdict,
}

/// A result judged against its config and baseline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Judged {
    pub status: Status,
    pub gating: Gating,
    pub rows: Vec<Row>,
    /// True when this suite makes the run fail.
    pub blocks: bool,
    /// Every miss, regression and error (blocking or not).
    pub problems: Vec<String>,
    /// The problems that block under `gating`.
    pub blocking: Vec<String>,
    #[serde(flatten)]
    pub result: SuiteResult,
}

pub fn load_baseline(path: &Path) -> Option<BTreeMap<String, f64>> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let m = v.get("metrics")?.as_object()?;
    Some(
        m.iter()
            .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
            .collect(),
    )
}

pub fn judge(
    result: SuiteResult,
    cfg: &SuiteConfig,
    baseline: Option<&BTreeMap<String, f64>>,
) -> Judged {
    judge_with(result, cfg, baseline, Gating::Strict)
}

pub fn judge_with(
    result: SuiteResult,
    cfg: &SuiteConfig,
    baseline: Option<&BTreeMap<String, f64>>,
    gating: Gating,
) -> Judged {
    let tol = cfg.baseline_tolerance.unwrap_or(0.0);
    let mut rows = Vec::new();
    let mut problems: Vec<String> = result.errors.clone();
    // Scorer errors make the run untrustworthy: they block in every mode.
    let mut blocks_list: Vec<String> = result.errors.clone();
    if result.ran {
        for (metric, gate) in &cfg.gates {
            let value = result.metrics.get(metric).copied();
            let base = baseline.and_then(|b| b.get(metric)).copied();
            let verdict = match value {
                None => Verdict::Missing,
                Some(v) if !gate.passes(v) => Verdict::Fail,
                Some(v) if base.is_some_and(|b| gate.regressed(v, b, tol)) => Verdict::Regressed,
                Some(_) => Verdict::Pass,
            };
            // A metric can miss its gate AND regress: quality gating must
            // still see the regression.
            let also_regressed = verdict == Verdict::Fail
                && value
                    .zip(base)
                    .is_some_and(|(v, b)| gate.regressed(v, b, tol));
            let problem = match verdict {
                Verdict::Fail if also_regressed => Some(format!(
                    "{metric} = {} misses its gate ({}) and regressed past baseline {} (tolerance {tol})",
                    fmt(value),
                    gate.describe(),
                    fmt(base)
                )),
                Verdict::Fail => Some(format!(
                    "{metric} = {} misses its gate ({})",
                    fmt(value),
                    gate.describe()
                )),
                Verdict::Regressed => Some(format!(
                    "{metric} = {} regressed past baseline {} (tolerance {tol})",
                    fmt(value),
                    fmt(base)
                )),
                Verdict::Missing => Some(format!(
                    "{metric} has a gate but the suite didn't measure it"
                )),
                _ => None,
            };
            if let Some(p) = problem {
                if blocking(gating, metric, verdict)
                    || (also_regressed && blocking(gating, metric, Verdict::Regressed))
                {
                    blocks_list.push(p.clone());
                }
                problems.push(p);
            }
            rows.push(Row {
                metric: metric.clone(),
                value,
                gate: Some(*gate),
                baseline: base,
                verdict,
            });
        }
        for (metric, v) in &result.metrics {
            if !cfg.gates.contains_key(metric) {
                rows.push(Row {
                    metric: metric.clone(),
                    value: Some(*v),
                    gate: None,
                    baseline: baseline.and_then(|b| b.get(metric)).copied(),
                    verdict: Verdict::Info,
                });
            }
        }
    } else if result.errors.is_empty() && cfg.status == Status::Required {
        let p = format!(
            "required suite did not run: {}",
            result.note.clone().unwrap_or_default()
        );
        blocks_list.push(p.clone());
        problems.push(p);
    }
    // Pending suites block only on scorer errors; report-only never.
    let blocks = match cfg.status {
        Status::Required => !blocks_list.is_empty(),
        Status::Pending => !result.errors.is_empty(),
        Status::ReportOnly => false,
    };
    if !blocks {
        blocks_list.clear();
    }
    Judged {
        status: cfg.status,
        gating,
        rows,
        blocks,
        problems,
        blocking: blocks_list,
        result,
    }
}

pub fn fmt(v: Option<f64>) -> String {
    match v {
        None => "-".into(),
        Some(v) if v.fract() == 0.0 && v.abs() < 1e12 => format!("{v:.0}"),
        Some(v) => format!("{v:.4}"),
    }
}

impl Judged {
    pub fn outcome(&self) -> &'static str {
        if self.blocks {
            return "FAIL";
        }
        match (self.status, self.problems.is_empty(), self.result.ran) {
            (Status::Required, true, true) => "PASS",
            (Status::Required, false, true) => "PASS (its misses don't block in this mode)",
            (_, _, false) => "NOT RUN",
            (Status::Pending, true, true) => "PASS (pending, not blocking)",
            (Status::Pending, false, true) => "MISSES (pending, not blocking)",
            (Status::ReportOnly, _, true) => "REPORTED",
        }
    }

    /// The plain-text table printed to the terminal.
    pub fn table(&self) -> String {
        let mut out = format!(
            "\n== {} ({}, {} gating) == {}\n",
            self.result.suite,
            self.status.as_str(),
            self.gating.as_str(),
            self.outcome()
        );
        if let Some(note) = &self.result.note {
            out.push_str(&format!("   {note}\n"));
        }
        if !self.rows.is_empty() {
            out.push_str(&format!(
                "   {:<34} {:>12} {:>12} {:>12}  {}\n",
                "metric", "value", "gate", "baseline", "verdict"
            ));
            for r in &self.rows {
                out.push_str(&format!(
                    "   {:<34} {:>12} {:>12} {:>12}  {}\n",
                    r.metric,
                    fmt(r.value),
                    r.gate.map(|g| g.describe()).unwrap_or_else(|| "-".into()),
                    fmt(r.baseline),
                    r.verdict.label()
                ));
            }
        }
        for p in &self.problems {
            let mark = if self.blocking.contains(p) { "!" } else { "-" };
            out.push_str(&format!("   {mark} {p}\n"));
        }
        out
    }

    /// The same, as Markdown (for the CI job summary).
    pub fn markdown(&self) -> String {
        let mut out = format!(
            "### {} ({}, {} gating): {}\n\n",
            self.result.suite,
            self.status.as_str(),
            self.gating.as_str(),
            self.outcome()
        );
        if let Some(note) = &self.result.note {
            out.push_str(&format!("{note}\n\n"));
        }
        if !self.rows.is_empty() {
            out.push_str(
                "| metric | value | gate | baseline | verdict |\n|---|---:|---:|---:|---|\n",
            );
            for r in &self.rows {
                out.push_str(&format!(
                    "| `{}` | {} | {} | {} | {} |\n",
                    r.metric,
                    fmt(r.value),
                    r.gate.map(|g| g.describe()).unwrap_or_else(|| "-".into()),
                    fmt(r.baseline),
                    r.verdict.label()
                ));
            }
            out.push('\n');
        }
        for p in &self.problems {
            out.push_str(&format!("- {p}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(status: Status) -> SuiteConfig {
        let mut gates = BTreeMap::new();
        gates.insert(
            "recall".into(),
            Gate {
                min: Some(0.85),
                max: None,
            },
        );
        gates.insert(
            "leaks".into(),
            Gate {
                min: None,
                max: Some(0.0),
            },
        );
        SuiteConfig {
            status,
            baseline_tolerance: Some(0.02),
            gates,
            k: None,
            embedder: None,
            der_tolerance: None,
        }
    }

    fn result(recall: f64, leaks: f64) -> SuiteResult {
        let mut r = SuiteResult::new("t");
        r.metrics.insert("recall".into(), recall);
        r.metrics.insert("leaks".into(), leaks);
        r
    }

    #[test]
    fn gates_and_baseline() {
        let c = cfg(Status::Required);
        assert!(!judge(result(0.9, 0.0), &c, None).blocks);
        assert!(judge(result(0.84, 0.0), &c, None).blocks);
        assert!(judge(result(0.9, 1.0), &c, None).blocks);
        let base: BTreeMap<String, f64> = [("recall".to_string(), 0.95)].into();
        // Within tolerance, then past it.
        assert!(!judge(result(0.94, 0.0), &c, Some(&base)).blocks);
        let j = judge(result(0.92, 0.0), &c, Some(&base));
        assert!(j.blocks);
        assert_eq!(j.rows[1].verdict, Verdict::Regressed);
        // Improvements never fail.
        assert!(!judge(result(0.99, 0.0), &c, Some(&base)).blocks);
    }

    #[test]
    fn security_and_quality_gating() {
        let c = cfg(Status::Required);
        let base: BTreeMap<String, f64> = [("recall".to_string(), 0.95)].into();
        // An absolute quality miss: strict fails, security and quality pass.
        let r = || result(0.5, 0.0);
        assert!(judge_with(r(), &c, None, Gating::Strict).blocks);
        assert!(!judge_with(r(), &c, None, Gating::Security).blocks);
        assert!(!judge_with(r(), &c, None, Gating::Quality).blocks);
        // A regression: quality fails, security doesn't.
        assert!(judge_with(r(), &c, Some(&base), Gating::Quality).blocks);
        assert!(!judge_with(r(), &c, Some(&base), Gating::Security).blocks);
        // A security miss: security and strict fail, quality doesn't
        // (no baseline to regress from).
        let mut sc = c.clone();
        sc.gates.insert(
            "keep_local_leaks".into(),
            Gate {
                min: None,
                max: Some(0.0),
            },
        );
        let mut leak = result(0.9, 0.0);
        leak.metrics.insert("keep_local_leaks".into(), 1.0);
        assert!(judge_with(leak.clone(), &sc, None, Gating::Security).blocks);
        assert!(judge_with(leak.clone(), &sc, None, Gating::Strict).blocks);
        assert!(!judge_with(leak, &sc, None, Gating::Quality).blocks);
        // Scorer errors block in every mode, even for a pending suite.
        let mut broken = result(0.9, 0.0);
        broken.errors.push("bad fixture".into());
        for g in [Gating::Strict, Gating::Security, Gating::Quality] {
            assert!(judge_with(broken.clone(), &cfg(Status::Pending), None, g).blocks);
        }
    }

    #[test]
    fn pending_never_blocks_and_missing_metrics_count() {
        let p = cfg(Status::Pending);
        let j = judge(result(0.1, 5.0), &p, None);
        assert!(!j.blocks && !j.problems.is_empty());
        let mut r = SuiteResult::new("t");
        r.metrics.insert("recall".into(), 0.9);
        let j = judge(r, &cfg(Status::Required), None);
        assert!(j.blocks, "a gated metric that wasn't measured fails");
        let j = judge(
            SuiteResult::not_run("t", "why"),
            &cfg(Status::Required),
            None,
        );
        assert!(j.blocks, "a required suite that didn't run fails");
        let j = judge(SuiteResult::not_run("t", "why"), &p, None);
        assert!(!j.blocks);
    }
}
