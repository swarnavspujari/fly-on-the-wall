//! The Ask contract (spec §4.2, §4.2a #1, #5, #7, #10, #12, #13, #14).
//!
//! A scenario runs one question through an [`AskHarness`] against the
//! fixture database, with a [`RecordingProvider`] that plays a fixed script
//! (or a recorded replay) and records every request. The scorer
//! ([`score`]) trusts nothing the harness says about itself:
//! * the **retrieved set** is the `[seg:ID]` markers the harness put in the
//!   system, user and tool-result content it sent (never assistant turns);
//! * **claims** are checked against the fixture text ([`crate::claims`]);
//! * **writes** are found by diffing the database, and the consent and
//!   provider settings, before and after;
//! * **keep-local leaks** are found by scanning every request to a provider
//!   that isn't on this device, assistant turns the harness wrote included;
//! * **budgets** are counted from the requests, and **links** from
//!   everything displayed (answer, sources, trace, proposal previews,
//!   streamed frames).
//!
//! The harness-facing types are in [`api`]; the provider and replay files
//! in [`provider`].

pub mod api;
pub mod probes;
pub mod provider;
pub mod score;

pub use api::*;
pub use provider::{
    fingerprint, strip_thinking, ProviderKind, RecordingProvider, ReplayFile, ReplayProfile,
    ScriptStep, REPLAY_FORMAT,
};
pub use score::{budgets, free_text_leaves, run, run_harness, self_check, Row, Tally};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context as _, Result};
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::corpus::Corpus;
use crate::retrieval::{Answerable, GoldSet, Requirement, Scope};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioKind {
    /// A well-behaved (auto) or recorded model: quality metrics apply.
    Quality,
    /// An adversarial model the harness must contain (fake citation,
    /// corrupted number, confident answer to an unanswerable question).
    Guard,
    /// Injected content the model obeys.
    RedTeam,
    /// A failure mode or citation-syntax variant with an expected outcome.
    Robustness,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioFile {
    auto: AutoConfig,
    #[serde(default)]
    scenario: Vec<ScenarioDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AutoConfig {
    /// Every gold question runs on an on-device provider at each tier.
    tiers: Vec<Tier>,
    /// Gold ids also run, at tier S, on a cloud provider…
    cloud: Vec<String>,
    /// …and on an Ollama at a remote URL.
    remote_local: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioDef {
    id: String,
    /// A gold question id: its question, scope and expectation…
    #[serde(default)]
    question: Option<String>,
    /// …or free text (with `question`, a follow-up's phrasing).
    #[serde(default)]
    question_text: Option<String>,
    #[serde(default)]
    scope: Option<Scope>,
    /// Earlier turns; answers and citations use fixture segment ids.
    #[serde(default)]
    history: Vec<AskTurn>,
    script: String,
    provider: ProviderKind,
    tier: Tier,
    kind: ScenarioKind,
    /// Robustness: the outcome expected instead of the gold one.
    #[serde(default)]
    expect: Option<ExpectDef>,
    /// With `expect = "model_failed"`: which failure.
    #[serde(default)]
    failure: Option<FailureKind>,
}

/// The kind of model failure a robustness script expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    EmptyReply,
    Truncated,
    /// Any provider error kind.
    Provider,
    BudgetExhausted,
    ToolProtocol,
}

impl FailureKind {
    pub fn matches(self, f: ModelFailure) -> bool {
        matches!(
            (self, f),
            (FailureKind::EmptyReply, ModelFailure::EmptyReply)
                | (FailureKind::Truncated, ModelFailure::Truncated)
                | (FailureKind::Provider, ModelFailure::Provider(_))
                | (FailureKind::BudgetExhausted, ModelFailure::BudgetExhausted)
                | (FailureKind::ToolProtocol, ModelFailure::ToolProtocol)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExpectDef {
    Answer,
    NotFound,
    ModelFailed,
    Contained,
}

/// What a correct outcome looks like.
#[derive(Debug, Clone)]
pub enum Expect {
    /// Answered, citing at least one must-cite requirement.
    Answer(Vec<Requirement>),
    /// "Not found", with zero citations.
    NotFound,
    /// A typed model failure (of this kind, when given), with nothing
    /// shown as sourced.
    ModelFailed(Option<FailureKind>),
    /// Anything but a crash or a sourced invalid citation.
    Contained,
    /// Red team: only the guards apply.
    Ungraded,
}

#[derive(Debug, Clone)]
pub struct Scenario {
    pub id: String,
    pub question: String,
    pub scope: Option<Scope>,
    /// Earlier turns (fixture ids; mapped at run time).
    pub history: Vec<AskTurn>,
    pub provider: ProviderKind,
    pub tier: Tier,
    pub kind: ScenarioKind,
    pub expect: Expect,
    /// Script steps, placeholders unresolved (`{meeting:m10}`, `m13-045`).
    pub responses: Vec<Value>,
    pub source: String,
    /// The quality group: `auto@<tier>` or `replay:<model>@<tier>`.
    pub group: Option<String>,
    /// A replay's file (its identity and request fingerprint).
    pub replay: Option<ReplayFile>,
}

/// Everything the suite loads.
pub struct Contract {
    pub corpus: Corpus,
    pub gold: GoldSet,
    pub scenarios: Vec<Scenario>,
    pub scripts: usize,
    pub replays: usize,
    /// Dated list prices for the cost report (`prices.toml`).
    pub prices: Prices,
}

/// The reply a well-behaved model gives an unanswerable question.
pub const NOT_FOUND_REPLY: &str = "I couldn't find that in your meetings.";

fn tool_use(id: &str, name: &str, input: Value) -> Value {
    json!({ "type": "tool_use", "id": id, "name": name, "input": input })
}

fn reply(blocks: Vec<Value>, stop: &str) -> Value {
    json!({ "blocks": blocks, "stop_reason": stop })
}

fn text_reply(text: &str) -> Value {
    reply(vec![json!({ "type": "text", "text": text })], "end_turn")
}

fn search_input(question: &str, scope: Option<&Scope>) -> Value {
    let mut a = json!({ "query": question, "limit": 8 });
    if let Some(s) = scope {
        if let Some(m) = &s.meeting {
            a["meeting_ids"] = json!([format!("{{meeting:{m}}}")]);
        }
        if let Some(f) = &s.folder {
            a["folder_id"] = json!(format!("{{folder:{f}}}"));
        }
    }
    a
}

/// The `auto` reference model for one gold question at one tier: S answers
/// from the prefetch; M reads the must-cite segments in one tool round; C
/// searches, then reads, then answers. Unanswerable: searches (once on M,
/// twice on C, the deterministic re-query of §4.2a #12), then "not found".
fn auto_responses(
    tier: Tier,
    expect: &Expect,
    answer: Option<&Answerable>,
    question: &str,
    scope: Option<&Scope>,
    corpus: &Corpus,
) -> Vec<Value> {
    let final_text = match (expect, answer) {
        (Expect::Answer(reqs), Some(a)) => {
            let markers: Vec<String> = reqs
                .iter()
                .map(|r| format!("[seg:{}]", r.ids()[0]))
                .collect();
            format!("{} {}.", a.answer.trim_end_matches('.'), markers.join(" "))
        }
        _ => NOT_FOUND_REPLY.to_string(),
    };
    let reads = |reqs: &[Requirement]| {
        let blocks = reqs
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let seg = r.ids()[0];
                let meeting = &corpus.segment(seg).expect("validated").meeting;
                tool_use(
                    &format!("call_r{i}"),
                    "read_segments",
                    json!({
                        "meeting_id": format!("{{meeting:{meeting}}}"),
                        "around_segment_id": seg,
                        "before": 1,
                        "after": 1,
                    }),
                )
            })
            .collect();
        reply(blocks, "tool_use")
    };
    let search = |n: usize| {
        reply(
            vec![tool_use(
                &format!("call_s{n}"),
                "search_passages",
                search_input(question, scope),
            )],
            "tool_use",
        )
    };
    let mut out = Vec::new();
    match (tier, expect) {
        (Tier::S, _) => {}
        (Tier::M, Expect::Answer(reqs)) => out.push(reads(reqs)),
        (Tier::M, _) => out.push(search(0)),
        (Tier::C, Expect::Answer(reqs)) => {
            out.push(search(0));
            out.push(reads(reqs));
        }
        (Tier::C, _) => {
            out.push(search(0));
            out.push(search(1));
        }
    }
    out.push(text_reply(&final_text));
    out
}

impl Contract {
    pub fn load(evals_dir: &Path) -> Result<Self> {
        let corpus = Corpus::load(&evals_dir.join("fixtures"))?;
        let gold = GoldSet::load(&evals_dir.join("suites/retrieval/questions.toml"))?;
        gold.validate(&corpus)?;
        let dir = evals_dir.join("suites").join("ask_contract");
        let file: ScenarioFile = toml::from_str(
            &std::fs::read_to_string(dir.join("scenarios.toml")).context("read scenarios.toml")?,
        )
        .context("parse scenarios.toml")?;
        let prices = Prices::load(&dir.join("prices.toml"))?;

        let answerable: BTreeMap<&str, &Answerable> =
            gold.answerable.iter().map(|q| (q.id.as_str(), q)).collect();
        let unanswerable: BTreeMap<&str, _> = gold
            .unanswerable
            .iter()
            .map(|u| (u.id.as_str(), u))
            .collect();
        let expect_for =
            |gold_id: &str, provider: ProviderKind| -> Result<(String, Option<Scope>, Expect)> {
                if let Some(q) = answerable.get(gold_id) {
                    let expect = if q.keep_local && !provider.on_device() {
                        Expect::NotFound
                    } else {
                        Expect::Answer(q.must_cite.clone())
                    };
                    return Ok((q.question.clone(), q.scope.clone(), expect));
                }
                if let Some(u) = unanswerable.get(gold_id) {
                    ensure!(
                        !(u.shareable_only() && provider.on_device()),
                        "{gold_id} is unanswerable only off device"
                    );
                    return Ok((u.question.clone(), None, Expect::NotFound));
                }
                bail!("unknown gold question {gold_id}")
            };

        let mut scenarios = Vec::new();
        // Auto: every gold question on device at each tier (except the
        // off-device-only unanswerables), plus the listed ones off device.
        let mut auto_ids: Vec<(String, ProviderKind, Tier)> = Vec::new();
        for tier in &file.auto.tiers {
            auto_ids.extend(
                gold.answerable
                    .iter()
                    .map(|q| q.id.clone())
                    .chain(
                        gold.unanswerable
                            .iter()
                            .filter(|u| !u.shareable_only())
                            .map(|u| u.id.clone()),
                    )
                    .map(|id| (id, ProviderKind::Local, *tier)),
            );
        }
        for (ids, kind) in [
            (&file.auto.cloud, ProviderKind::Cloud),
            (&file.auto.remote_local, ProviderKind::RemoteLocal),
        ] {
            auto_ids.extend(ids.iter().map(|id| (id.clone(), kind, Tier::S)));
        }
        for (gold_id, provider, tier) in auto_ids {
            let (question, scope, expect) = expect_for(&gold_id, provider)?;
            let responses = auto_responses(
                tier,
                &expect,
                answerable.get(gold_id.as_str()).copied(),
                &question,
                scope.as_ref(),
                &corpus,
            );
            scenarios.push(Scenario {
                id: format!(
                    "auto-{}-{}-{gold_id}",
                    tier.as_str().to_lowercase(),
                    provider.as_str()
                ),
                question,
                scope,
                history: vec![],
                provider,
                tier,
                kind: ScenarioKind::Quality,
                expect,
                responses,
                source: "auto".into(),
                group: Some(format!("auto@{}", tier.as_str())),
                replay: None,
            });
        }

        let mut scripts = 0;
        for def in &file.scenario {
            let (question, scope, gold_expect) = match (&def.question, &def.question_text) {
                (Some(g), text) => {
                    let (q, s, e) = expect_for(g, def.provider)?;
                    (text.clone().unwrap_or(q), def.scope.clone().or(s), e)
                }
                (None, Some(t)) => (t.clone(), def.scope.clone(), Expect::Ungraded),
                (None, None) => bail!("{}: set question, question_text or both", def.id),
            };
            let expect = match def.expect {
                None => gold_expect,
                Some(ExpectDef::Answer) => match gold_expect {
                    Expect::Answer(r) => Expect::Answer(r),
                    _ => bail!("{}: expect = answer needs an answerable gold id", def.id),
                },
                Some(ExpectDef::NotFound) => Expect::NotFound,
                Some(ExpectDef::ModelFailed) => Expect::ModelFailed(def.failure),
                Some(ExpectDef::Contained) => Expect::Contained,
            };
            ensure!(
                def.failure.is_none() || def.expect == Some(ExpectDef::ModelFailed),
                "{}: failure needs expect = \"model_failed\"",
                def.id
            );
            match def.kind {
                ScenarioKind::RedTeam => ensure!(
                    matches!(expect, Expect::Ungraded),
                    "{}: red-team scenarios use question_text only",
                    def.id
                ),
                ScenarioKind::Robustness => ensure!(
                    !matches!(expect, Expect::Ungraded),
                    "{}: a robustness scenario needs an expected outcome",
                    def.id
                ),
                ScenarioKind::Quality => bail!(
                    "{}: quality scenarios come from auto and replays only",
                    def.id
                ),
                ScenarioKind::Guard => {}
            }
            for t in &def.history {
                for c in &t.citations {
                    ensure!(
                        corpus.segment(c).is_some(),
                        "{}: history cites unknown segment {c}",
                        def.id
                    );
                }
            }
            let path = dir.join("scripts").join(format!("{}.json", def.script));
            let script: Value = serde_json::from_str(
                &std::fs::read_to_string(&path)
                    .with_context(|| format!("{}: read {}", def.id, path.display()))?,
            )
            .with_context(|| format!("parse {}", path.display()))?;
            let responses = script
                .get("responses")
                .and_then(Value::as_array)
                .with_context(|| format!("{}: no responses array", path.display()))?
                .clone();
            for r in &responses {
                ScriptStep::from_json(r).with_context(|| {
                    format!(
                        "{}: a step is neither a ChatResponse nor an error",
                        path.display()
                    )
                })?;
            }
            ensure!(!responses.is_empty(), "{}: empty script", def.id);
            scripts += 1;
            scenarios.push(Scenario {
                id: def.id.clone(),
                question,
                scope,
                history: def.history.clone(),
                provider: def.provider,
                tier: def.tier,
                kind: def.kind,
                expect,
                responses,
                source: format!("scripts/{}.json", def.script),
                group: None,
                replay: None,
            });
        }

        // Replays: recorded real-model replies for an existing scenario.
        let mut replays = 0;
        let replay_dir = dir.join("replays");
        let mut files: Vec<PathBuf> = Vec::new();
        if replay_dir.is_dir() {
            for model in std::fs::read_dir(&replay_dir)? {
                let model = model?.path();
                if model.is_dir() {
                    for f in std::fs::read_dir(&model)? {
                        let f = f?.path();
                        if f.extension().is_some_and(|e| e == "json") {
                            files.push(f);
                        }
                    }
                }
            }
        }
        files.sort();
        for f in files {
            let mut r: ReplayFile = serde_json::from_str(&std::fs::read_to_string(&f)?)
                .with_context(|| format!("parse {}", f.display()))?;
            ensure!(
                r.format == REPLAY_FORMAT,
                "{}: replay format {} (expected {REPLAY_FORMAT}); re-record it",
                f.display(),
                r.format
            );
            let base = scenarios
                .iter()
                .find(|s| s.id == r.scenario)
                .with_context(|| format!("{}: unknown scenario {}", f.display(), r.scenario))?
                .clone();
            ensure!(
                r.tier == base.tier,
                "{}: recorded at tier {} but {} runs at {}",
                f.display(),
                r.tier.as_str(),
                base.id,
                base.tier.as_str()
            );
            replays += 1;
            let responses = r
                .responses
                .iter()
                .map(|x| serde_json::to_value(x).expect("serializable"))
                .collect();
            let group = (base.kind == ScenarioKind::Quality).then(|| r.group());
            r.responses.clear();
            scenarios.push(Scenario {
                id: format!("replay:{}:{}", r.model, r.scenario),
                responses,
                source: f
                    .strip_prefix(evals_dir)
                    .unwrap_or(&f)
                    .display()
                    .to_string(),
                group,
                replay: Some(r),
                ..base
            });
        }

        let mut seen = BTreeSet::new();
        for s in &scenarios {
            ensure!(
                seen.insert(s.id.clone()),
                "scenario id {} appears twice",
                s.id
            );
            if let Some(sc) = &s.scope {
                ensure!(
                    sc.meeting.is_some() != sc.folder.is_some(),
                    "{}: scope needs exactly one of meeting or folder",
                    s.id
                );
                if let Some(m) = &sc.meeting {
                    ensure!(corpus.meeting(m).is_some(), "{}: unknown meeting {m}", s.id);
                }
                if let Some(f) = &sc.folder {
                    ensure!(
                        corpus.folder_paths().contains(f),
                        "{}: unknown folder {f}",
                        s.id
                    );
                }
            }
        }
        Ok(Contract {
            corpus,
            gold,
            scenarios,
            scripts,
            replays,
            prices,
        })
    }

    pub fn count(&self, kind: ScenarioKind) -> usize {
        self.scenarios.iter().filter(|s| s.kind == kind).count()
    }
}

/// Dated list prices, USD per million tokens, for the cost report only.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prices {
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, Price>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub input: f64,
    pub output: f64,
}

impl Prices {
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let p: Prices = toml::from_str(&std::fs::read_to_string(path)?)
            .with_context(|| format!("parse {}", path.display()))?;
        ensure!(
            p.models.is_empty() || p.date.is_some(),
            "{}: a price table needs its date",
            path.display()
        );
        Ok(p)
    }

    /// USD for one Ask on `model`.
    pub fn cost(&self, model: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
        self.models
            .get(model)
            .map(|p| (input_tokens as f64 * p.input + output_tokens as f64 * p.output) / 1e6)
    }
}

/// The fixed clock every scenario runs at (the day after the last meeting).
pub fn eval_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 10, 17, 0, 0)
        .single()
        .expect("valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_contract_loads() {
        let c = Contract::load(&crate::evals_dir()).unwrap();
        assert!(c.count(ScenarioKind::RedTeam) >= 10);
        assert!(c.count(ScenarioKind::Robustness) >= 10);
        assert!(c.count(ScenarioKind::Guard) >= 1);
        // The cost table is dated and prices the scripted cloud model.
        assert!(c.prices.date.is_some());
        assert_eq!(
            c.prices
                .cost(fly_llm::anthropic::ANTHROPIC_DEFAULT_MODEL, 1_000_000, 0)
                .map(|x| x > 0.0),
            Some(true)
        );
        // Keep-local gold off device expects "not found".
        for id in ["auto-s-cloud-q46", "auto-s-remote_local-q46"] {
            let s = c
                .scenarios
                .iter()
                .find(|s| s.id == id)
                .unwrap_or_else(|| panic!("{id} runs"));
            assert!(matches!(s.expect, Expect::NotFound), "{id}");
        }
        // Every tier has an auto group of the whole gold set.
        for t in Tier::ALL {
            let g = format!("auto@{}", t.as_str());
            let n = c
                .scenarios
                .iter()
                .filter(|s| s.group.as_deref() == Some(g.as_str()))
                .count();
            assert!(n >= c.gold.answerable.len(), "{g}: {n}");
        }
    }

    #[test]
    fn auto_scripts_follow_the_tier() {
        let c = Contract::load(&crate::evals_dir()).unwrap();
        let steps = |id: &str| {
            c.scenarios
                .iter()
                .find(|s| s.id == id)
                .unwrap()
                .responses
                .len()
        };
        assert_eq!(steps("auto-s-local-q02"), 1);
        assert_eq!(steps("auto-m-local-q02"), 2);
        assert_eq!(steps("auto-c-local-q02"), 3);
        assert_eq!(steps("auto-c-local-u01"), 3);
    }
}
