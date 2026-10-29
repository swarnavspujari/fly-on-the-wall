//! fly-ask: the Ask harness (spec §4.2 / §4.2a; v2.5 PR 3.3).
//!
//! Agentic RAG that works on small local models and on cloud models, with
//! citations the user can trust because code — not the model — checks them.
//! One [`ask`] call:
//!
//! 1. **Scope** ([`scope`]): this meeting / a note / a folder / a person /
//!    everything. Visibility comes from the caller's `on_device` (the app's
//!    `build_provider` result): off-device models never see Keep-local
//!    meetings, and the user's trace counts how many weren't searched.
//! 2. **Route** ([`router`]): dates, people, a folder and list intents
//!    pulled from the question and validated against known values.
//! 3. **Prefetch** ([`prefetch`]): fly-context passages (the plain top 8
//!    whole, a deeper pool clipped, notes demoted), extracted items and the
//!    user's notes, as `[seg:ID]` lines sized to the tier.
//! 4. **Floor**: a wide-scope question whose topic words aren't in what was
//!    found is "not found" without a model call.
//! 5. **Synthesize**, streamed as sanitized frames: tier S writes from the
//!    prefetch alone; tiers M/C may call the read tools and propose
//!    changes, inside budgets ([`tier`], fly-llm's tool loop,
//!    [`provider::Metered`]).
//! 6. **Validate** ([`validate`], [`claims`]): only shown ids count;
//!    unsourced and flagged sentences; numbers, dates, names and quotes
//!    checked; links removed. Zero valid citations → one retry → otherwise
//!    shown fully flagged. An empty or truncated reply is a model failure,
//!    not "not found".
//! 7. **Proposals** ([`proposals`]): validated, returned, never executed.
//! 8. **Run log** ([`runlog`]): typed and allowlisted, returned for the app
//!    to persist.
//!
//! The harness never constructs a provider, reads a secret or writes to
//! storage.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use chrono::{DateTime, SecondsFormat, Utc};
use fly_context::Visibility;
use fly_llm::external_tools::ExternalTools;
use fly_llm::tool_loop::{run_tool_loop_async, LoopEnd, ToolCallRecord};
use fly_llm::{
    ContentBlock, ConverseRequest, LLMProvider, LlmError, Message, StopReason, ThinkingMode,
    ToolChoice,
};
use serde::{Deserialize, Serialize};

pub mod agent;
pub mod claims;
pub mod events;
pub mod evidence;
pub mod prefetch;
pub mod prompt;
pub mod proposals;
pub mod provider;
pub mod router;
pub mod runlog;
pub mod scope;
pub mod store;
pub mod text;
pub mod tier;
pub mod validate;

pub use events::{AskEvent, EventSink, Stage};
pub use evidence::{SegmentInfo, SourceLine, SourceView};
pub use proposals::{Proposal, ProposalRefusal, ProposalTool};
pub use runlog::{AnswerStatus, FailureKind, RunLog};
pub use scope::{AskScope, ScopeKind};
pub use store::StoreAccess;
pub use tier::{measure_tier, tool_smoke, Limits, Tier, TierInputs};
pub use validate::{AnswerSentence, Flag, FlagKind, SentenceStatus};

use prefetch::{PrefetchInput, QueryEmbedding};
use prompt::RetryReason;
use provider::Metered;
use runlog::{FloorLog, Reach, RouterLog, Stop, ToolCallLog, ToolName};

/// A wide-scope question below this share of topic words found is "not
/// found" without asking the model.
pub const FLOOR_COVERAGE: f64 = 0.34;
/// Earlier chat turns sent with a question.
const HISTORY_TURNS: usize = 3;
const HISTORY_TURN_CHARS: usize = 600;

/// Embeds a question for semantic search (the app's embeds through the
/// local Ollama). Takes the raw query; applies its own query prompt.
/// `None` = no vector this time (search falls back to keywords).
pub trait QueryEmbedder {
    /// The model id stored on chunks (e.g. `embeddinggemma:300m`).
    fn model(&self) -> &str;
    fn embed(&self, text: &str) -> impl Future<Output = Option<Vec<f32>>>;
}

/// Keyword search only.
pub struct NoEmbedder;

impl QueryEmbedder for NoEmbedder {
    fn model(&self) -> &str {
        ""
    }
    async fn embed(&self, _: &str) -> Option<Vec<f32>> {
        None
    }
}

/// An earlier turn of this chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub question: String,
    /// The answer as displayed.
    pub answer: String,
    /// The segment ids it cited.
    #[serde(default)]
    pub citations: Vec<String>,
    /// It ran on this device, so its answer may hold Keep-local content: it
    /// is never sent to a provider that isn't on this device.
    pub on_device: bool,
}

/// Who answers, and with what.
pub struct AskEnv<'a, S, E> {
    pub store: &'a S,
    /// Built by the app's `build_provider` (consent and Keep local already
    /// enforced there).
    pub provider: &'a dyn LLMProvider,
    /// `build_provider`'s verdict that the model runs on this machine.
    /// Decides Keep-local visibility. Not `provider.is_local()`: a remote
    /// Ollama is local in kind but not on this device.
    pub on_device: bool,
    /// Measured with [`measure_tier`] (the evals pin it).
    pub tier: Tier,
    /// Declared limits; `None` = the tier's own ([`Tier::limits`]).
    pub limits: Option<Limits>,
    pub embedder: Option<&'a E>,
    pub events: Option<EventSink<'a>>,
    pub cancel: Option<&'a AtomicBool>,
    /// Tools from outside the harness (connectors), per turn. Offered only
    /// on tiers that call tools and when the explicit scope holds no
    /// Keep-local note; the whole turn then reads Keep-local content
    /// excluded (the fly-llm `ExternalTools` contract).
    pub external: Option<&'a dyn ExternalTools>,
}

/// The question.
pub struct AskInput<'a> {
    pub question: &'a str,
    /// Earlier turns of this chat, oldest first.
    pub history: &'a [Turn],
    pub scope: AskScope,
    /// The clock for "last week".
    pub now: DateTime<Utc>,
}

/// What the user's trace shows (not the run log).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Trace {
    pub tier: Option<Tier>,
    pub scope: Option<ScopeKind>,
    /// "N kept-local meetings not searched" — for the user only.
    pub kept_local_not_searched: usize,
    pub meetings_in_scope: usize,
    pub passages: u32,
    pub items: u32,
    pub tool_calls: Vec<ToolCallLog>,
    pub retried: bool,
    /// The server seems to have dropped the start of the prompt.
    pub context_truncated: bool,
    /// "Not found" came from the score floor, before any model call.
    pub floor_not_found: bool,
    /// A budget forced the final answer.
    pub budget_forced: bool,
    /// Connectors were available but off for this question: its explicit
    /// scope holds a Keep-local meeting.
    pub connectors_off_keep_local: bool,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl Trace {
    /// The trace as shown, one step per line.
    pub fn lines(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(scope) = self.scope {
            v.push(match scope {
                ScopeKind::Meeting | ScopeKind::Note => "Searched this meeting".to_string(),
                _ => format!(
                    "Searched {}",
                    plural(self.meetings_in_scope, "meeting", "meetings")
                ),
            });
        }
        if self.kept_local_not_searched > 0 {
            v.push(format!(
                "{} not searched",
                plural(
                    self.kept_local_not_searched,
                    "kept-local meeting",
                    "kept-local meetings"
                )
            ));
        }
        if self.passages > 0 || self.items > 0 {
            v.push(format!(
                "Read {} and {}",
                plural(self.passages as usize, "passage", "passages"),
                plural(self.items as usize, "extracted item", "extracted items")
            ));
        }
        if !self.tool_calls.is_empty() {
            v.push(format!(
                "Looked further {}",
                plural(self.tool_calls.len(), "time", "times")
            ));
        }
        if self.connectors_off_keep_local {
            v.push("Connectors were off: this includes a Keep-local meeting".into());
        }
        if self.floor_not_found {
            v.push("Nothing in your meetings matched the question".into());
        }
        if self.retried {
            v.push("Asked the model again".into());
        }
        if self.budget_forced {
            v.push("Stopped looking to stay within the answer's budget".into());
        }
        if self.context_truncated {
            v.push("The model may not have read every excerpt".into());
        }
        v
    }
}

/// A model failure as the user sees it. `message` may quote the provider's
/// error (shown in the UI only; never logged).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timing {
    pub first_token_ms: Option<u64>,
    pub total_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AskAnswer {
    pub status: AnswerStatus,
    /// Display Markdown for a plain renderer (markers and links removed).
    pub text: String,
    pub sentences: Vec<AnswerSentence>,
    /// The meetings and lines the answer cites, first-cited first.
    pub sources: Vec<SourceView>,
    pub proposals: Vec<Proposal>,
    pub refusals: Vec<ProposalRefusal>,
    pub trace: Trace,
    pub failure: Option<Failure>,
    pub timing: Timing,
    /// Where the answering model ran (the next turn's `Turn::on_device`).
    pub on_device: bool,
    pub run_log: RunLog,
}

impl AskAnswer {
    pub fn not_found(&self) -> bool {
        self.status == AnswerStatus::NotFound
    }
}

fn reach(on_device: bool, provider: &dyn LLMProvider) -> Reach {
    if on_device {
        Reach::OnDevice
    } else if provider.is_local() {
        Reach::LocalRemote
    } else {
        Reach::Cloud
    }
}

/// Display text: sentences joined, bullets on their own lines.
fn join(sentences: &[AnswerSentence]) -> String {
    let mut out = String::new();
    for s in sentences {
        if !out.is_empty() {
            out.push(if s.text.starts_with("- ") { '\n' } else { ' ' });
        }
        out.push_str(&s.text);
    }
    out
}

fn stop_of(r: StopReason) -> Stop {
    match r {
        StopReason::EndTurn => Stop::EndTurn,
        StopReason::ToolUse => Stop::ToolUse,
        StopReason::MaxTokens => Stop::MaxTokens,
    }
}

fn failure_of(e: &LlmError) -> Failure {
    let kind = FailureKind::from_error(e);
    let message = match kind {
        FailureKind::ToolProtocol => "The model's tool calls couldn't be understood, so there's no answer. Try again, or switch to another model.".into(),
        FailureKind::BudgetExhausted => "Ask used up its budget before the model answered.".into(),
        _ => e.to_string(),
    };
    Failure { kind, message }
}

struct Run<'a> {
    log: RunLog,
    trace: Trace,
    started: Instant,
    on_device: bool,
    events: Option<EventSink<'a>>,
}

struct Outcome {
    status: AnswerStatus,
    sentences: Vec<AnswerSentence>,
    sources: Vec<SourceView>,
    proposals: Vec<Proposal>,
    refusals: Vec<ProposalRefusal>,
    failure: Option<Failure>,
    cited: Vec<String>,
}

impl Outcome {
    fn bare(status: AnswerStatus, failure: Option<Failure>) -> Self {
        Outcome {
            status,
            sentences: vec![],
            sources: vec![],
            proposals: vec![],
            refusals: vec![],
            failure,
            cited: vec![],
        }
    }
}

impl Run<'_> {
    fn done(mut self, o: Outcome) -> AskAnswer {
        self.log.status = o.status;
        self.log.failure = o.failure.as_ref().map(|f| f.kind);
        self.log.timings.total_ms = self.started.elapsed().as_millis() as u64;
        self.log.proposals = o.proposals.len() as u32;
        self.log.proposal_refusals = o.refusals.iter().map(|r| r.reason).collect();
        self.log.cited_segment_ids = o.cited;
        let text = match o.status {
            // The kept-local count stays in `trace` (the user's view): the
            // answer text becomes chat history, which can go to a model.
            AnswerStatus::NotFound => prompt::NOT_FOUND_REPLY.to_string(),
            AnswerStatus::Refused => match self.log.scope_refusal {
                Some(scope::ScopeRefusal::KeepLocal) => "This is marked Keep local, so it isn't sent to a model that runs off this computer.".into(),
                Some(scope::ScopeRefusal::Unchecked) => "Couldn't check which meetings are kept local, so nothing was sent to the model. Try again in a moment.".into(),
                _ => "There's nothing to search here.".into(),
            },
            AnswerStatus::Cancelled | AnswerStatus::ModelFailed => String::new(),
            AnswerStatus::Answered | AnswerStatus::Unverified => join(&o.sentences),
        };
        // The last frame is the answer as displayed (turn 0: validated, with
        // citations in `sentences`), never the model's raw text.
        if !text.is_empty() {
            events::emit(
                self.events,
                AskEvent::Frame {
                    turn: 0,
                    text: text.clone(),
                },
            );
        }
        AskAnswer {
            status: o.status,
            text,
            sentences: o.sentences,
            sources: o.sources,
            proposals: o.proposals,
            refusals: o.refusals,
            timing: Timing {
                first_token_ms: self.log.timings.first_token_ms,
                total_ms: self.log.timings.total_ms,
            },
            trace: self.trace,
            failure: o.failure,
            on_device: self.on_device,
            run_log: self.log,
        }
    }
}

/// Earlier turns to send, as alternating user/assistant messages: the
/// newest few, never one that ran on this device when this provider isn't
/// on this device (its answer — or even its question — may hold Keep-local
/// content).
fn history_messages(history: &[Turn], on_device: bool) -> Vec<Message> {
    let clip = |t: &str| prefetch::truncate(t.trim(), HISTORY_TURN_CHARS);
    let allowed: Vec<&Turn> = history
        .iter()
        .filter(|t| on_device || !t.on_device)
        .filter(|t| !t.question.trim().is_empty() && !t.answer.trim().is_empty())
        .collect();
    let skip = allowed.len().saturating_sub(HISTORY_TURNS);
    let mut out = Vec::new();
    for t in allowed.into_iter().skip(skip) {
        let (answer, _) = text::take_markers(&without_kept_local_note(&t.answer));
        out.push(Message::user(evidence::safe(&clip(&t.question))));
        out.push(Message::assistant(evidence::safe(&clip(&answer))));
    }
    out
}

/// Tell the connector turn when any of `meetings` is Keep local.
pub fn mark_if_keep_local<S: StoreAccess>(x: &dyn ExternalTools, meetings: &[String], store: &S) {
    if store.with(|s| meetings.iter().any(|m| s.meeting_keep_local(m))) {
        x.mark_keep_local();
    }
}

/// Each source with the note that holds its meeting.
fn with_notes<S: StoreAccess>(
    mut views: Vec<SourceView>,
    resolved: &scope::Resolved,
    store: &S,
) -> Vec<SourceView> {
    for v in &mut views {
        v.note_id = match resolved.meeting(&v.meeting_id) {
            Some(m) => m.note_id.clone(),
            None => store.with(|s| {
                s.get_meeting(&v.meeting_id)
                    .map(|m| m.note_id)
                    .unwrap_or_default()
            }),
        };
    }
    views
}

/// An earlier answer without a "(N kept-local meetings were not searched.)"
/// note (answers from builds that appended it to the text): the count
/// never reaches a model.
fn without_kept_local_note(answer: &str) -> String {
    let mut out = answer.to_string();
    // ASCII lowercasing keeps byte offsets: `k` indexes `out` itself.
    while let Some(k) = out.to_ascii_lowercase().find("kept-local") {
        let open = out[..k].rfind('(');
        let close = out[k..].find(')').map(|j| k + j + 1);
        match (open, close) {
            (Some(o), Some(c)) => out.replace_range(o..c, ""),
            // Not the note's shape: drop the word itself.
            _ => out.replace_range(k..k + "kept-local".len(), ""),
        }
    }
    out.trim().to_string()
}

/// What one synthesis phase produced.
struct Phase {
    text: String,
    convo: Vec<Message>,
    rounds: u32,
    calls: Vec<ToolCallRecord>,
    end: Option<LoopEnd>,
}

async fn synthesize<S: StoreAccess, E: QueryEmbedder>(
    metered: &Metered<'_>,
    req: &ConverseRequest,
    budget: tier::Budget,
    state: &agent::ToolState<'_, S, E>,
) -> Result<Phase, LlmError> {
    if budget.uses_tools() && req.has_tools() {
        let out = run_tool_loop_async(
            metered,
            req.clone(),
            budget.loop_limits(),
            |name, args| async move { state.call(name, args).await },
        )
        .await?;
        return Ok(Phase {
            text: out.answer,
            convo: out.messages,
            rounds: out.rounds,
            calls: out.calls,
            end: Some(out.end),
        });
    }
    let r = metered.converse(req.clone()).await?;
    let text = r.text();
    let mut convo = req.messages.clone();
    convo.push(Message::assistant(text.clone()));
    Ok(Phase {
        text,
        convo,
        rounds: 0,
        calls: vec![],
        end: None,
    })
}

/// Answer one question. Never panics on model output; a model or storage
/// failure is an [`AnswerStatus`], not an `Err`.
pub async fn ask<S: StoreAccess, E: QueryEmbedder>(
    env: AskEnv<'_, S, E>,
    input: AskInput<'_>,
) -> AskAnswer {
    let started = Instant::now();
    let tier = env.tier;
    let budget = tier.budget_with(env.limits.unwrap_or(tier.limits()));
    // Connectors: only on tool tiers, only when the explicit scope holds
    // no Keep-local note (a failed lookup counts as Keep local). When they
    // are offered, the whole turn reads with Keep-local content excluded,
    // as for an off-device model.
    let offered = env
        .external
        .filter(|x| tier.uses_tools() && !x.specs().is_empty());
    let connectors_off_keep_local = offered.is_some()
        && env
            .store
            .with(|s| scope::explicit_keep_local(s, &input.scope));
    let external = offered.filter(|_| !connectors_off_keep_local);
    let vis = if env.on_device && external.is_none() {
        Visibility::All
    } else {
        Visibility::ShareableOnly
    };
    let mut run = Run {
        log: RunLog::new(
            fly_core::new_id(),
            Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            tier,
            reach(env.on_device, env.provider),
            runlog::ProviderKind::from_id(env.provider.id()),
        ),
        trace: Trace {
            tier: Some(tier),
            connectors_off_keep_local,
            ..Default::default()
        },
        started,
        on_device: env.on_device,
        events: env.events,
    };
    run.log.history_turns = input.history.len() as u32;
    let cancelled = || env.cancel.is_some_and(|c| c.load(Ordering::Relaxed));

    // 1. Scope.
    events::emit(
        env.events,
        AskEvent::Stage {
            stage: Stage::Scoping,
        },
    );
    let t = Instant::now();
    let resolved = env.store.with(|s| scope::resolve(s, &input.scope, vis));
    run.log.timings.scope_ms = t.elapsed().as_millis() as u64;
    let resolved = match resolved {
        Ok(r) => r,
        Err(refusal) => {
            run.log.scope_refusal = Some(refusal);
            return run.done(Outcome::bare(AnswerStatus::Refused, None));
        }
    };
    run.log.scope = resolved.kind;
    run.log.scope_meetings = resolved.meetings.len() as u32;
    run.log.kept_local_excluded = resolved.kept_local_excluded as u32;
    run.trace.scope = Some(resolved.kind);
    run.trace.kept_local_not_searched = resolved.kept_local_excluded;
    run.trace.meetings_in_scope = resolved.meetings.len();

    // 2. Route.
    let hints = router::route(input.question, input.now, &resolved.known, resolved.wide);
    run.log.router = RouterLog {
        date_rule: hints.date_rule,
        people: hints.people.len() as u32,
        folder: hints.folder_id.is_some(),
        items_intent: hints.items.map(|i| {
            i.kind
                .map(|k| k.as_str().to_string())
                .unwrap_or_else(|| "any".into())
        }),
    };
    let salient = prefetch::salient_terms(input.question);
    // A short follow-up searches with the previous question too (a local
    // search; the previous question itself is only sent if its turn is).
    let follow_up = input
        .history
        .iter()
        .rev()
        .find(|t| !t.question.trim().is_empty())
        .filter(|_| salient.len() < 3)
        .map(|t| {
            format!(
                "{} {}",
                prefetch::truncate(t.question.trim(), 300),
                input.question
            )
        });

    // 3. Embed the queries (outside any storage lock).
    events::emit(
        env.events,
        AskEvent::Stage {
            stage: Stage::Searching,
        },
    );
    let t = Instant::now();
    let mut embeddings: Vec<QueryEmbedding> = Vec::new();
    if let Some(e) = env.embedder {
        let mut queries: Vec<String> = vec![input.question.to_string()];
        queries.extend(follow_up.clone());
        if !resolved.wide && resolved.meeting_ids.is_some() {
            queries.extend(salient.iter().cloned());
        }
        for q in queries {
            if let Some(vector) = e.embed(&q).await {
                embeddings.push(QueryEmbedding { query: q, vector });
            }
        }
    }
    run.log.timings.embed_ms = t.elapsed().as_millis() as u64;
    if cancelled() {
        return run.done(Outcome::bare(AnswerStatus::Cancelled, None));
    }

    // 4. Prefetch.
    let t = Instant::now();
    let embed_model = env.embedder.map(|e| e.model().to_string());
    let pf = env.store.with(|s| {
        prefetch::prefetch(
            s,
            &PrefetchInput {
                question: input.question,
                follow_up_query: follow_up.as_deref(),
                resolved: &resolved,
                hints: &hints,
                budget,
                embeddings: &embeddings,
                embed_model: embed_model.as_deref(),
            },
        )
    });
    run.log.timings.prefetch_ms = t.elapsed().as_millis() as u64;
    let pf = match pf {
        Ok(p) => p,
        // fly-context couldn't read the Keep-local sets, so it returned
        // nothing: a refusal (fail closed), not a storage failure.
        Err(fly_context::ContextError::KeepLocalCheck(_)) => {
            run.log.scope_refusal = Some(scope::ScopeRefusal::Unchecked);
            return run.done(Outcome::bare(AnswerStatus::Refused, None));
        }
        Err(_) => {
            return run.done(Outcome::bare(
                AnswerStatus::ModelFailed,
                Some(Failure {
                    kind: FailureKind::Storage,
                    message: "Couldn't read your meetings to answer that.".into(),
                }),
            ))
        }
    };
    // Backstop (the ExternalTools contract): the turn reads without
    // Keep-local content when connectors are offered, but if a Keep-local
    // meeting's line reached the context anyway, every connector call is
    // refused from here on.
    if let Some(x) = external {
        mark_if_keep_local(x, pf.evidence.meetings(), env.store);
    }
    run.log.prefetch = pf.stats.clone();
    run.trace.passages = pf.stats.transcript_passages;
    run.trace.items = pf.stats.items;
    events::emit(
        env.events,
        AskEvent::Sources {
            sources: with_notes(
                evidence::source_views(&pf.evidence, pf.evidence.ids()),
                &resolved,
                env.store,
            ),
            kept_local_not_searched: resolved.kept_local_excluded,
        },
    );

    // 5. Score floor (wide scopes): nothing about the topic → not found.
    //    The share of the question's topic words in the context decides;
    //    an unscoped question the search also measured semantically (a
    //    floor for its embedder, 4+ content words) must clear its cosine
    //    floor too. On the contract's wide questions with the app's
    //    embedder the cosine alone caught 1 of 23 unanswerables (their
    //    best cosines overlap the answerable ones'), coverage 4, either
    //    one 4, with no answerable question refused by any rule.
    let floor_applies = resolved.wide;
    let semantic = pf.similarity.filter(|_| resolved.kind == ScopeKind::All);
    let passed = !floor_applies
        || (!pf.evidence.is_empty()
            && (pf.terms == 0 || pf.coverage >= FLOOR_COVERAGE)
            && semantic.is_none_or(|sim| sim.passes()));
    let pct = |c: f32| (c.max(0.0) * 100.0).round() as u32;
    run.log.floor = FloorLog {
        applied: floor_applies,
        terms: pf.terms as u32,
        coverage_pct: (pf.coverage * 100.0).round() as u32,
        semantic: floor_applies && semantic.is_some(),
        top_similarity_pct: semantic.and_then(|s| s.top).map(pct),
        similarity_floor_pct: semantic.map(|s| pct(s.floor)),
        passed,
    };
    if !passed {
        run.trace.floor_not_found = true;
        return run.done(Outcome::bare(AnswerStatus::NotFound, None));
    }

    // 6. Synthesize.
    events::emit(
        env.events,
        AskEvent::Stage {
            stage: Stage::Answering,
        },
    );
    let metered = Metered::new(env.provider, env.events, env.cancel, budget, started);
    let local_model = env.provider.is_local();
    let profile = fly_core::prompt_profile::profile_for(env.provider.model());
    let uses_tools = budget.uses_tools() && tier.uses_tools();
    let mut messages = vec![Message::system(
        profile.apply_preamble(prompt::system(tier)),
    )];
    // With connectors offered, an earlier on-device turn stays out too.
    messages.extend(history_messages(
        input.history,
        env.on_device && external.is_none(),
    ));
    messages.push(Message::user(prompt::user(
        &pf.context,
        &[],
        input.question,
    )));
    let req = ConverseRequest {
        messages,
        tools: if uses_tools {
            let mut t = agent::tool_specs(&env.provider.capabilities());
            if let Some(x) = external {
                // Never one that shadows the harness's own tools.
                let own: Vec<String> = t.iter().map(|s| s.name.clone()).collect();
                t.extend(x.specs().into_iter().filter(|s| !own.contains(&s.name)));
            }
            t
        } else {
            vec![]
        },
        temperature: Some(0.2),
        max_tokens: Some(if local_model {
            budget.max_output_tokens
        } else {
            profile.max_tokens.ask.unwrap_or(budget.max_output_tokens)
        }),
        // Local models always get thinking off, explicitly (Ollama turns it
        // on when `think` is omitted with tools, ollama#10976).
        thinking: if local_model || profile.thinking_disabled() {
            ThinkingMode::Disabled
        } else {
            ThinkingMode::Default
        },
        effort: profile.effort.ask.map(String::from),
        ..Default::default()
    };
    let state = agent::ToolState::new(
        env.store,
        &resolved,
        env.embedder,
        env.events,
        budget.observation_chars,
        input.now,
        pf.evidence.clone(),
    )
    .with_external(external)
    .with_cancel(env.cancel);

    let mut first = synthesize(&metered, &req, budget, &state).await;
    // One bounded retry of a transient provider failure.
    if let Err(e) = &first {
        if provider::is_transient(e)
            && !cancelled()
            && metered.affords(provider::request_tokens(&req))
        {
            run.log.retries.push(RetryReason::ProviderError);
            run.trace.retried = true;
            first = synthesize(&metered, &req, budget, &state).await;
        }
    }

    let finish_meter = |run: &mut Run| {
        let m = metered.meter();
        run.log.model_calls = m.calls;
        run.log.input_tokens_est = m.input_tokens;
        run.log.input_tokens = m.usage.input_tokens;
        run.log.output_tokens = m.usage.output_tokens;
        run.log.truncation_suspected = m.truncation_suspected;
        run.log.timings.first_token_ms = m.first_token_ms;
        run.log.timings.model_ms = m.model_ms;
        let mut stops: Vec<Stop> = m.stops.iter().map(|s| stop_of(*s)).collect();
        stops.append(&mut run.log.stops);
        if m.budget_forced {
            stops.push(Stop::TokenBudget);
        }
        run.log.stops = stops;
        run.trace.context_truncated = m.truncation_suspected;
        run.trace.budget_forced = m.budget_forced;
        run.trace.tool_calls = run.log.tool_calls.clone();
    };
    let tool_results = |state: &agent::ToolState<'_, S, E>| {
        (
            state
                .proposals
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
            state
                .refusals
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
        )
    };
    let failed = |run: Run, state: &agent::ToolState<'_, S, E>, status, failure| {
        let (proposals, refusals) = tool_results(state);
        run.done(Outcome {
            proposals,
            refusals,
            ..Outcome::bare(status, failure)
        })
    };

    let phase = match first {
        Ok(p) => p,
        Err(e) => {
            if matches!(e, LlmError::ToolProtocol(_)) {
                run.log.stops.push(Stop::ToolProtocol);
            }
            finish_meter(&mut run);
            if provider::is_cancel(&e) || cancelled() {
                // A stopped answer proposes nothing.
                return run.done(Outcome::bare(AnswerStatus::Cancelled, None));
            }
            return failed(run, &state, AnswerStatus::ModelFailed, Some(failure_of(&e)));
        }
    };
    run.log.rounds = phase.rounds;
    run.log.tool_calls = phase
        .calls
        .iter()
        .map(|c| ToolCallLog {
            tool: ToolName::from_name(&c.name),
            ok: c.ok,
        })
        .collect();
    match phase.end {
        Some(LoopEnd::CallBudget) => run.log.stops.push(Stop::CallBudget),
        Some(LoopEnd::RoundBudget) => run.log.stops.push(Stop::RoundBudget),
        _ => {}
    }

    // 7. Validate, retry once if needed.
    events::emit(
        env.events,
        AskEvent::Stage {
            stage: Stage::Checking,
        },
    );
    let t = Instant::now();
    let mut evidence = state
        .evidence
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let last_stop = metered.meter().stops.last().copied();
    let truncated =
        last_stop == Some(StopReason::MaxTokens) || phase.end == Some(LoopEnd::MaxTokens);
    let mut v = validate::validate(&phase.text, &evidence);
    let retry_reason = if truncated {
        None // the same budget would cut it off again
    } else if v.empty {
        Some(RetryReason::Empty)
    } else if !v.refusal && !v.any_valid_citation() {
        Some(RetryReason::NoValidCitations)
    } else if v.refusal && uses_tools && !state.searched.load(Ordering::Relaxed) {
        Some(RetryReason::Requery)
    } else {
        None
    };
    run.log.timings.validate_ms = t.elapsed().as_millis() as u64;
    let mut truncated = truncated;

    if let Some(reason) = retry_reason.filter(|_| !cancelled()) {
        // A re-query only helps if a second, keyword-led search finds
        // lines the model hasn't seen.
        let extra = if reason == RetryReason::Requery {
            requery(&env, &resolved, &hints, budget, &salient, &evidence)
        } else {
            None
        };
        if reason != RetryReason::Requery || extra.is_some() {
            let note = prompt::retry(reason, extra.as_ref().map(|(c, _)| c.as_str()));
            let mut retry_req = ConverseRequest {
                tool_choice: ToolChoice::None,
                ..req.clone()
            };
            if reason == RetryReason::Empty {
                // Never send an empty assistant turn: ask again in the user
                // turn.
                let mut msgs = req.messages.clone();
                if let Some(last) = msgs.last_mut() {
                    last.content.push(ContentBlock::text(format!("\n\n{note}")));
                }
                retry_req.messages = msgs;
            } else {
                let mut convo = phase.convo.clone();
                convo.push(Message::user(note));
                retry_req.messages = convo;
            }
            if !uses_tools {
                retry_req.tools.clear();
            }
            if metered.affords(provider::request_tokens(&retry_req)) {
                if let Some((_, extra_ev)) = &extra {
                    for id in extra_ev.ids() {
                        if let Some(info) = extra_ev.get(id) {
                            evidence.insert(info.clone());
                        }
                    }
                }
                run.log.retries.push(reason);
                run.trace.retried = true;
                events::emit(
                    env.events,
                    AskEvent::Stage {
                        stage: Stage::Retrying,
                    },
                );
                match metered.converse(retry_req).await {
                    Ok(resp) => {
                        let v2 = validate::validate(&resp.text(), &evidence);
                        let cut = resp.stop_reason == StopReason::MaxTokens;
                        // Keep the retry unless it's worse: empty, or citing
                        // nothing when the first did.
                        if !v2.empty && (v2.any_valid_citation() || !v.any_valid_citation()) {
                            v = v2;
                            truncated = cut;
                        }
                    }
                    Err(e) if provider::is_cancel(&e) || cancelled() => {
                        finish_meter(&mut run);
                        return run.done(Outcome::bare(AnswerStatus::Cancelled, None));
                    }
                    Err(_) => {} // keep the first answer
                }
            }
        }
    }
    finish_meter(&mut run);
    run.log.validation = v.stats;
    // Stopped after the last call: nothing is shown or proposed.
    if cancelled() {
        return run.done(Outcome::bare(AnswerStatus::Cancelled, None));
    }
    let (proposals, refusals) = tool_results(&state);

    // The server read less of the prompt than was sent: whatever came back
    // was written without part of the evidence or the rules.
    if metered.meter().truncation_suspected {
        return run.done(Outcome {
            proposals,
            refusals,
            ..Outcome::bare(
                AnswerStatus::ModelFailed,
                Some(Failure {
                    kind: FailureKind::Truncated,
                    message: "The model's context was too small for this question, so part of it was cut off before the model read it. Try a narrower question, or a model with a larger context window.".into(),
                }),
            )
        });
    }
    if truncated {
        return run.done(Outcome {
            proposals,
            refusals,
            ..Outcome::bare(
                AnswerStatus::ModelFailed,
                Some(Failure {
                    kind: FailureKind::Truncated,
                    message: "The model's answer was cut off before it finished. Try again, or ask a narrower question.".into(),
                }),
            )
        });
    }
    if v.empty {
        return run.done(Outcome {
            proposals,
            refusals,
            ..Outcome::bare(
                AnswerStatus::ModelFailed,
                Some(Failure {
                    kind: FailureKind::EmptyReply,
                    message: "The model returned an empty answer. Try again, or switch to another model in Settings.".into(),
                }),
            )
        });
    }
    if v.refusal {
        run.log.validation = validate::ValidationStats {
            links_removed: v.stats.links_removed,
            ..Default::default()
        };
        return run.done(Outcome {
            proposals,
            refusals,
            ..Outcome::bare(AnswerStatus::NotFound, None)
        });
    }
    let status = if v.any_valid_citation() {
        AnswerStatus::Answered
    } else {
        validate::flag_all(&mut v);
        run.log.validation = v.stats;
        AnswerStatus::Unverified
    };
    let mut cited: Vec<String> = Vec::new();
    for s in &v.sentences {
        for id in &s.citations {
            if !cited.contains(id) {
                cited.push(id.clone());
            }
        }
    }
    run.done(Outcome {
        status,
        sources: with_notes(
            evidence::source_views(&evidence, &cited),
            &resolved,
            env.store,
        ),
        sentences: v.sentences,
        proposals,
        refusals,
        failure: None,
        cited,
    })
}

/// One deterministic re-query before accepting "not discussed" (tiers M/C,
/// §4.2a #12): the topic words alone, without the router's date filter.
/// Returns the new lines (only if some weren't shown yet) as extra context.
fn requery<S: StoreAccess, E: QueryEmbedder>(
    env: &AskEnv<'_, S, E>,
    resolved: &scope::Resolved,
    hints: &router::Hints,
    budget: tier::Budget,
    salient: &[String],
    shown: &evidence::Evidence,
) -> Option<(String, evidence::Evidence)> {
    if salient.is_empty() {
        return None;
    }
    let q = salient.join(" ");
    let plain = router::Hints {
        since: None,
        until: None,
        date_rule: None,
        ..hints.clone()
    };
    let pf = env
        .store
        .with(|s| {
            prefetch::prefetch(
                s,
                &PrefetchInput {
                    question: &q,
                    follow_up_query: None,
                    resolved,
                    hints: &plain,
                    budget: tier::Budget {
                        context_chars: budget.context_chars / 2,
                        item_chars: 0,
                        memory_chars: 0,
                        note_chars: 0,
                        max_note_passages: 0,
                        ..budget
                    },
                    embeddings: &[],
                    embed_model: None,
                },
            )
        })
        .ok()?;
    pf.evidence
        .ids()
        .iter()
        .any(|id| !shown.contains(id))
        .then_some((pf.context, pf.evidence))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_local_notes_are_removed_byte_safely() {
        assert_eq!(
            without_kept_local_note(
                "Not found in your meetings. (1 kept-local meeting was not searched.)"
            ),
            "Not found in your meetings."
        );
        // Multibyte text whose lowercase form is longer than itself.
        let t = without_kept_local_note("İİ İstanbul — (2 Kept-Local meetings were not searched.)");
        assert_eq!(t, "İİ İstanbul —");
        let t = without_kept_local_note("İ a KEPT-LOCAL b (no close");
        assert!(!t.to_ascii_lowercase().contains("kept-local"), "{t}");
        assert_eq!(without_kept_local_note("Plain answer."), "Plain answer.");
    }
}
