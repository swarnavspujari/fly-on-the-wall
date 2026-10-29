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
//! 3. **Prefetch** ([`prefetch`]): fly-context passages (deep pool, notes
//!    demoted, clipped), extracted items and the user's notes, as
//!    `[seg:ID]` lines sized to the tier.
//! 4. **Floor**: a wide-scope question whose topic words aren't in what was
//!    found is "not found" without a model call.
//! 5. **Synthesize** (streamed): tier S writes from the prefetch alone;
//!    tiers M/C may call the read tools and propose changes, inside
//!    budgets ([`tier`], fly-llm's tool loop, [`provider::Metered`]).
//! 6. **Validate** ([`validate`], [`claims`]): only shown ids count;
//!    unsourced and flagged sentences; numbers, dates, names and quotes
//!    checked; links removed. Zero valid citations → one retry → otherwise
//!    shown fully flagged. An empty reply is a model failure, not "not
//!    found".
//! 7. **Proposals** ([`proposals`]): validated, returned, never executed.
//! 8. **Run log** ([`runlog`]): typed and allowlisted, returned for the app
//!    to persist.
//!
//! The harness never constructs a provider, reads a secret or writes to
//! storage.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use chrono::{DateTime, SecondsFormat, Utc};
use fly_context::Visibility;
use fly_llm::tool_loop::{run_tool_loop_async, LoopEnd};
use fly_llm::{ContentBlock, ConverseRequest, LLMProvider, LlmError, Message, Role, ThinkingMode};
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
pub use evidence::SegmentInfo;
pub use proposals::{Proposal, ProposalRefusal, ProposalTool};
pub use runlog::{AnswerStatus, FailureKind, RunLog};
pub use scope::{AskScope, ScopeKind};
pub use store::StoreAccess;
pub use tier::{measure_tier, tool_smoke, Tier, TierInputs};
pub use validate::{AnswerSentence, Flag, FlagKind, SentenceStatus};

use prefetch::{PrefetchInput, QueryEmbedding};
use prompt::RetryReason;
use provider::Metered;
use runlog::{FloorLog, Reach, RouterLog, Stop, ToolCallLog, ToolName};

/// A wide-scope question below this share of topic words found is "not
/// found" without asking the model.
pub const FLOOR_COVERAGE: f64 = 0.34;
/// Earlier chat turns sent with a question.
const HISTORY_TURNS: usize = 6;
const HISTORY_TURN_CHARS: usize = 1_200;

/// Embeds a question for semantic search. The app's embeds through the
/// local Ollama (`embeddinggemma`); `None` = semantic search off.
#[async_trait::async_trait]
pub trait QueryEmbedder: Send + Sync {
    /// The model id stored on chunks (e.g. `embeddinggemma:300m`).
    fn model(&self) -> &str;
    async fn embed(&self, text: &str) -> Option<Vec<f32>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    User,
    Assistant,
}

/// One earlier chat turn (the answer's display text for the assistant).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub role: TurnRole,
    pub text: String,
}

/// Who answers, and with what.
pub struct AskEnv<'a, S> {
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
    pub embedder: Option<&'a dyn QueryEmbedder>,
    pub events: Option<EventSink<'a>>,
    pub cancel: Option<&'a AtomicBool>,
}

/// The question.
pub struct AskInput<'a> {
    pub question: &'a str,
    /// Earlier turns of this chat, oldest first (not including `question`).
    pub history: &'a [Turn],
    pub scope: AskScope,
    /// The clock for "last week".
    pub now: DateTime<Utc>,
}

/// What the user's trace shows (not the run log: this may be richer).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// A model failure as the user sees it. `message` may quote the provider's
/// error (shown in the UI only; never logged).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskAnswer {
    pub status: AnswerStatus,
    /// Display Markdown for a plain renderer (markers removed).
    pub text: String,
    pub sentences: Vec<AnswerSentence>,
    /// The lines the answer cites, first-cited first.
    pub sources: Vec<SegmentInfo>,
    pub proposals: Vec<Proposal>,
    pub refusals: Vec<ProposalRefusal>,
    pub trace: Trace,
    pub failure: Option<Failure>,
    pub run_log: RunLog,
}

impl AskAnswer {
    pub fn not_found(&self) -> bool {
        self.status == AnswerStatus::NotFound
    }
}

fn reach(env_on_device: bool, provider: &dyn LLMProvider) -> Reach {
    if env_on_device {
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

fn stop_of(r: fly_llm::StopReason) -> Stop {
    match r {
        fly_llm::StopReason::EndTurn => Stop::EndTurn,
        fly_llm::StopReason::ToolUse => Stop::ToolUse,
        fly_llm::StopReason::MaxTokens => Stop::MaxTokens,
    }
}

fn failure_message(e: &LlmError) -> String {
    match e {
        LlmError::ToolProtocol(_) => "The model's tool calls couldn't be understood, so there's no answer. Try again, or switch to another model.".into(),
        e if provider::is_over_budget(e) => "Ask used up its model-call budget before the model answered.".into(),
        e => e.to_string(),
    }
}

struct Run<'a> {
    log: RunLog,
    trace: Trace,
    started: Instant,
    on_device: bool,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl Run<'_> {
    fn done(
        mut self,
        status: AnswerStatus,
        sentences: Vec<AnswerSentence>,
        sources: Vec<SegmentInfo>,
        proposals: Vec<Proposal>,
        refusals: Vec<ProposalRefusal>,
        failure: Option<Failure>,
    ) -> AskAnswer {
        self.log.status = status;
        self.log.failure = failure.as_ref().map(|f| f.kind);
        self.log.timings.total_ms = self.started.elapsed().as_millis() as u64;
        self.log.proposals = proposals.len() as u32;
        self.log.proposal_refusals = refusals.iter().map(|r| r.reason).collect();
        self.log.cited_segment_ids = sources.iter().map(|s| s.id.clone()).collect();
        let text = match status {
            AnswerStatus::NotFound => {
                let mut t = prompt::NOT_FOUND_REPLY.to_string();
                let n = self.trace.kept_local_not_searched;
                if n > 0 && !self.on_device {
                    t.push_str(&format!(
                        " ({n} kept-local meeting{} not searched.)",
                        if n == 1 { " was" } else { "s were" }
                    ));
                }
                t
            }
            AnswerStatus::Refused => match self.log.scope_refusal {
                Some(scope::ScopeRefusal::KeepLocal) => {
                    "This is marked Keep local, so it isn't sent to a model that runs off this computer.".into()
                }
                _ => "There's nothing to search here.".into(),
            },
            AnswerStatus::Cancelled => String::new(),
            AnswerStatus::ModelFailed => String::new(),
            AnswerStatus::Answered | AnswerStatus::Unverified => join(&sentences),
        };
        AskAnswer {
            status,
            text,
            sentences,
            sources,
            proposals,
            refusals,
            trace: self.trace,
            failure,
            run_log: self.log,
        }
    }
}

/// Earlier turns to send: alternating, starting with a user turn, the
/// newest `HISTORY_TURNS`. Assistant turns only when they can't carry
/// content this provider may not see (on device, or a narrow scope that
/// `build_provider` already cleared).
fn history_messages(history: &[Turn], with_answers: bool) -> (Vec<Message>, Vec<String>) {
    let clip = |t: &str| prefetch::truncate(t.trim(), HISTORY_TURN_CHARS);
    let turns: Vec<&Turn> = history
        .iter()
        .filter(|t| !t.text.trim().is_empty())
        .collect();
    if !with_answers {
        let users: Vec<&&Turn> = turns.iter().filter(|t| t.role == TurnRole::User).collect();
        let skip = users.len().saturating_sub(3);
        let qs: Vec<String> = users
            .into_iter()
            .skip(skip)
            .map(|t| clip(&t.text))
            .collect();
        return (vec![], qs);
    }
    let skip = turns.len().saturating_sub(HISTORY_TURNS);
    let mut msgs: Vec<Message> = Vec::new();
    for t in turns.into_iter().skip(skip) {
        let role = match t.role {
            TurnRole::User => Role::User,
            TurnRole::Assistant => Role::Assistant,
        };
        if msgs.is_empty() && role != Role::User {
            continue;
        }
        if msgs.last().is_some_and(|m| m.role == role) {
            continue; // keep strict alternation
        }
        msgs.push(Message::new(
            role,
            vec![ContentBlock::text(evidence::safe(&clip(&t.text)))],
        ));
    }
    // The question follows as a user turn, so history must end on an answer.
    if msgs.last().is_some_and(|m| m.role == Role::User) {
        msgs.pop();
    }
    (msgs, vec![])
}

/// Answer one question. Never panics on model output; a model or storage
/// failure is an [`AnswerStatus`], not an `Err`.
pub async fn ask<S: StoreAccess>(env: AskEnv<'_, S>, input: AskInput<'_>) -> AskAnswer {
    let started = Instant::now();
    let tier = env.tier;
    let budget = tier.budget();
    let vis = if env.on_device {
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
            ..Default::default()
        },
        started,
        on_device: env.on_device,
        _marker: std::marker::PhantomData,
    };
    run.log.history_turns = input.history.len() as u32;
    let none = || (Vec::new(), Vec::new(), Vec::new(), Vec::new());
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
            let (a, b, c, d) = none();
            return run.done(AnswerStatus::Refused, a, b, c, d, None);
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
    let prev_question = input
        .history
        .iter()
        .rev()
        .find(|t| t.role == TurnRole::User && !t.text.trim().is_empty())
        .map(|t| t.text.trim().to_string());
    let follow_up = prev_question
        .filter(|_| salient.len() < 3)
        .map(|p| format!("{} {}", prefetch::truncate(&p, 300), input.question));

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
        let (a, b, c, d) = none();
        return run.done(AnswerStatus::Cancelled, a, b, c, d, None);
    }

    // 4. Prefetch.
    let t = Instant::now();
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
                embed_model: env.embedder.map(|e| e.model()),
            },
        )
    });
    run.log.timings.prefetch_ms = t.elapsed().as_millis() as u64;
    let pf = match pf {
        Ok(p) => p,
        Err(_) => {
            let (a, b, c, d) = none();
            return run.done(
                AnswerStatus::ModelFailed,
                a,
                b,
                c,
                d,
                Some(Failure {
                    kind: FailureKind::Storage,
                    message: "Couldn't read your meetings to answer that.".into(),
                }),
            );
        }
    };
    run.log.prefetch = pf.stats.clone();
    run.trace.passages = pf.stats.transcript_passages;
    run.trace.items = pf.stats.items;
    events::emit(
        env.events,
        AskEvent::Sources {
            sources: pf
                .evidence
                .ids()
                .iter()
                .filter_map(|id| pf.evidence.get(id).cloned())
                .collect(),
            kept_local_not_searched: resolved.kept_local_excluded,
        },
    );

    // 5. Score floor (wide scopes): nothing about the topic → not found.
    let floor_applies = resolved.wide;
    let passed = !floor_applies
        || (!pf.evidence.is_empty() && (pf.terms == 0 || pf.coverage >= FLOOR_COVERAGE));
    run.log.floor = FloorLog {
        applied: floor_applies,
        terms: pf.terms as u32,
        coverage_pct: (pf.coverage * 100.0).round() as u32,
        passed,
    };
    if !passed {
        run.trace.floor_not_found = true;
        let (a, b, c, d) = none();
        return run.done(AnswerStatus::NotFound, a, b, c, d, None);
    }

    // 6. Synthesize.
    events::emit(
        env.events,
        AskEvent::Stage {
            stage: Stage::Answering,
        },
    );
    let metered = Metered::new(
        env.provider,
        env.events,
        env.cancel,
        budget.max_model_calls,
        budget.max_input_tokens,
        started,
    );
    let local_model = env.provider.is_local();
    let profile = fly_core::prompt_profile::profile_for(env.provider.model());
    let with_answers = env.on_device || !resolved.wide;
    let (history, earlier) = history_messages(input.history, with_answers);
    let mut messages = vec![Message::system(
        profile.apply_preamble(prompt::system(tier)),
    )];
    messages.extend(history);
    messages.push(Message::user(prompt::user(
        &pf.context,
        &earlier,
        input.question,
    )));
    let tools = if tier.uses_tools() {
        agent::tool_specs(&env.provider.capabilities())
    } else {
        vec![]
    };
    let req = ConverseRequest {
        messages,
        tools,
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
    );
    // (answer text, the conversation so far for a retry)
    let first: Result<(String, Vec<Message>), LlmError> = if tier.uses_tools() {
        let st = &state;
        let out = run_tool_loop_async(
            &metered,
            req.clone(),
            tier.loop_limits(),
            move |name, args| async move { st.call(name, args).await },
        )
        .await;
        match out {
            Ok(o) => {
                run.log.rounds = o.rounds;
                run.log.tool_calls = o
                    .calls
                    .iter()
                    .map(|c| ToolCallLog {
                        tool: ToolName::from_name(&c.name),
                        ok: c.ok,
                    })
                    .collect();
                match o.end {
                    LoopEnd::CallBudget => run.log.stops.push(Stop::CallBudget),
                    LoopEnd::RoundBudget => run.log.stops.push(Stop::RoundBudget),
                    LoopEnd::MaxTokens => run.log.stops.push(Stop::MaxTokens),
                    LoopEnd::Done => {}
                }
                Ok((o.answer, o.messages))
            }
            Err(e) => {
                if matches!(e, LlmError::ToolProtocol(_)) {
                    run.log.stops.push(Stop::ToolProtocol);
                }
                Err(e)
            }
        }
    } else {
        metered.converse(req.clone()).await.map(|r| {
            let text = r.text();
            let mut msgs = req.messages.clone();
            msgs.push(Message::assistant(text.clone()));
            (text, msgs)
        })
    };

    let finish_meter = |run: &mut Run<'_>| {
        let m = metered.meter();
        run.log.model_calls = m.calls;
        run.log.input_tokens_est = m.est_input;
        run.log.input_tokens = m.usage.input_tokens;
        run.log.output_tokens = m.usage.output_tokens;
        run.log.truncation_suspected = m.truncation_suspected;
        run.log.timings.first_token_ms = m.first_token_ms;
        run.log.timings.model_ms = m.model_ms;
        run.log
            .stops
            .splice(0..0, m.stops.iter().map(|s| stop_of(*s)));
        if m.budget_forced {
            run.log.stops.push(Stop::TokenBudget);
        }
        run.trace.context_truncated = m.truncation_suspected;
        run.trace.budget_forced = m.budget_forced;
        run.trace.tool_calls = run.log.tool_calls.clone();
    };
    let take_tool_results = |state: &agent::ToolState<'_, S>| {
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

    let (first_text, mut convo) = match first {
        Ok(x) => x,
        Err(e) => {
            finish_meter(&mut run);
            let (props, refs) = take_tool_results(&state);
            if provider::is_cancel(&e) || cancelled() {
                return run.done(AnswerStatus::Cancelled, vec![], vec![], props, refs, None);
            }
            return run.done(
                AnswerStatus::ModelFailed,
                vec![],
                vec![],
                props,
                refs,
                Some(Failure {
                    kind: FailureKind::from_error(&e),
                    message: failure_message(&e),
                }),
            );
        }
    };

    // 7. Validate, retry once if needed.
    events::emit(
        env.events,
        AskEvent::Stage {
            stage: Stage::Checking,
        },
    );
    let t = Instant::now();
    let evidence = state
        .evidence
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let mut v = validate::validate(&first_text, &evidence);
    let mut evidence = evidence;
    let retry_reason = if v.empty {
        Some(RetryReason::Empty)
    } else if !v.refusal && !v.any_valid_citation() {
        Some(RetryReason::NoValidCitations)
    } else if v.refusal && tier.uses_tools() && !state.searched.load(Ordering::Relaxed) {
        Some(RetryReason::Requery)
    } else {
        None
    };
    run.log.timings.validate_ms = t.elapsed().as_millis() as u64;

    if let Some(reason) = retry_reason.filter(|_| metered.calls_left() > 0 && !cancelled()) {
        // A re-query only helps if a second, keyword-led search finds lines
        // the model hasn't seen.
        let extra = if reason == RetryReason::Requery {
            requery(
                &env,
                &resolved,
                &hints,
                budget,
                input.question,
                &salient,
                &evidence,
            )
        } else {
            None
        };
        let go = reason != RetryReason::Requery || extra.is_some();
        if go {
            if let Some((extra_ctx, extra_ev)) = &extra {
                for id in extra_ev.ids() {
                    if let Some(info) = extra_ev.get(id) {
                        evidence.insert(info.clone());
                    }
                }
                let _ = extra_ctx;
            }
            run.log.retries.push(reason);
            run.trace.retried = true;
            events::emit(
                env.events,
                AskEvent::Stage {
                    stage: Stage::Retrying,
                },
            );
            let mut retry_req = ConverseRequest {
                tool_choice: fly_llm::ToolChoice::None,
                ..req.clone()
            };
            let note = prompt::retry(reason, extra.as_ref().map(|(c, _)| c.as_str()));
            if reason == RetryReason::Empty {
                // Never send an empty assistant turn; ask again in the user turn.
                let mut msgs = req.messages.clone();
                if let Some(last) = msgs.last_mut() {
                    last.content.push(ContentBlock::text(format!("\n\n{note}")));
                }
                retry_req.messages = msgs;
            } else {
                convo.push(Message::user(note));
                retry_req.messages = convo.clone();
            }
            if !tier.uses_tools() {
                retry_req.tools.clear();
            }
            match metered.converse(retry_req).await {
                Ok(resp) => {
                    let v2 = validate::validate(&resp.text(), &evidence);
                    // Keep the retry unless it's worse: empty, or citing
                    // nothing when the first did.
                    if !v2.empty && (v2.any_valid_citation() || !v.any_valid_citation()) {
                        v = v2;
                    }
                }
                Err(e) if provider::is_cancel(&e) || cancelled() => {
                    finish_meter(&mut run);
                    let (props, refs) = take_tool_results(&state);
                    return run.done(AnswerStatus::Cancelled, vec![], vec![], props, refs, None);
                }
                Err(_) => {} // keep the first answer
            }
        }
    }
    finish_meter(&mut run);
    run.log.validation = v.stats;
    let (proposals, refusals) = take_tool_results(&state);

    if v.empty {
        return run.done(
            AnswerStatus::ModelFailed,
            vec![],
            vec![],
            proposals,
            refusals,
            Some(Failure {
                kind: FailureKind::EmptyReply,
                message: "The model returned an empty answer. Try again, or switch to another model in Settings.".into(),
            }),
        );
    }
    if v.refusal {
        run.log.validation = validate::ValidationStats {
            links_removed: v.stats.links_removed,
            ..Default::default()
        };
        return run.done(
            AnswerStatus::NotFound,
            vec![],
            vec![],
            proposals,
            refusals,
            None,
        );
    }
    let status = if v.any_valid_citation() {
        AnswerStatus::Answered
    } else {
        validate::flag_all(&mut v);
        run.log.validation = v.stats;
        AnswerStatus::Unverified
    };
    let mut sources: Vec<SegmentInfo> = Vec::new();
    for s in &v.sentences {
        for id in &s.citations {
            if !sources.iter().any(|x| &x.id == id) {
                if let Some(info) = evidence.get(id) {
                    sources.push(info.clone());
                }
            }
        }
    }
    run.done(status, v.sentences, sources, proposals, refusals, None)
}

/// One deterministic re-query before accepting "not discussed" (tiers M/C,
/// §4.2a #12): the topic words alone, without router filters. Returns the
/// new lines (only ones not shown yet) as extra context.
fn requery<S: StoreAccess>(
    env: &AskEnv<'_, S>,
    resolved: &scope::Resolved,
    hints: &router::Hints,
    budget: tier::Budget,
    question: &str,
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
    let _ = question;
    let new: Vec<&String> = pf
        .evidence
        .ids()
        .iter()
        .filter(|id| !shown.contains(id))
        .collect();
    if new.is_empty() {
        return None;
    }
    Some((pf.context, pf.evidence))
}
