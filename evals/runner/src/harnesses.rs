//! The registered Ask harness: fly-ask (spec v2.5 PR 3.3).
//!
//! Wiring the real harness is a locked change (evals/README.md, "Wiring
//! the harness"). The adapter below only maps types between the contract
//! and fly-ask; every decision is fly-ask's.

use std::sync::Mutex;

use crate::ask_contract::{self as c, AskHarness};

/// The Ask harness under test.
pub fn registered() -> Option<Box<dyn AskHarness>> {
    Some(Box::new(FlyAsk))
}

struct FlyAsk;

/// The contract's query embedder, as fly-ask's.
struct Embedder<'a>(&'a dyn c::QueryEmbedder);

impl fly_ask::QueryEmbedder for Embedder<'_> {
    fn model(&self) -> &str {
        self.0.model()
    }
    async fn embed(&self, text: &str) -> Option<Vec<f32>> {
        self.0.embed_query(text).await.ok()
    }
}

fn failure(f: &fly_ask::Failure) -> Result<c::ModelFailure, String> {
    use c::ProviderErrorKind as P;
    use fly_ask::FailureKind as K;
    Ok(match f.kind {
        K::EmptyReply => c::ModelFailure::EmptyReply,
        K::Truncated => c::ModelFailure::Truncated,
        K::ToolProtocol => c::ModelFailure::ToolProtocol,
        K::BudgetExhausted => c::ModelFailure::BudgetExhausted,
        K::NotConfigured => c::ModelFailure::Provider(P::NotConfigured),
        K::Auth => c::ModelFailure::Provider(P::Auth),
        K::Unavailable => c::ModelFailure::Provider(P::Unavailable),
        K::Network => c::ModelFailure::Provider(P::Network),
        K::Provider => c::ModelFailure::Provider(P::Provider),
        K::InvalidRequest => c::ModelFailure::Provider(P::InvalidRequest),
        K::Storage => return Err("storage read failed".into()),
    })
}

#[async_trait::async_trait(?Send)]
impl AskHarness for FlyAsk {
    fn name(&self) -> &str {
        "fly-ask"
    }

    async fn ask(&self, req: c::AskRequest<'_>) -> Result<c::AskAnswer, String> {
        let frames = Mutex::new(Vec::new());
        let sink = |e: fly_ask::AskEvent| {
            if let fly_ask::AskEvent::Frame { text, .. } = e {
                frames.lock().expect("frames").push(text);
            }
        };
        let history: Vec<fly_ask::Turn> = req
            .history
            .iter()
            .map(|t| fly_ask::Turn {
                question: t.question.clone(),
                answer: t.answer.clone(),
                citations: t.citations.clone(),
                on_device: t.on_device,
            })
            .collect();
        let a = fly_ask::ask(
            fly_ask::AskEnv {
                store: req.storage,
                provider: req.provider,
                on_device: req.on_device,
                tier: match req.tier {
                    c::Tier::S => fly_ask::Tier::S,
                    c::Tier::M => fly_ask::Tier::M,
                    c::Tier::C => fly_ask::Tier::C,
                },
                limits: Some(fly_ask::Limits {
                    model_calls: req.budget.model_calls,
                    tool_rounds: req.budget.tool_rounds,
                    input_tokens: req.budget.input_tokens,
                    request_tokens: req.budget.request_tokens,
                }),
                embedder: Some(&Embedder(req.embedder)),
                events: Some(&sink),
                cancel: None,
            },
            fly_ask::AskInput {
                question: req.question,
                history: &history,
                scope: match req.scope {
                    c::AskScope::All => fly_ask::AskScope::All,
                    c::AskScope::Meeting(id) => fly_ask::AskScope::Meeting(id),
                    c::AskScope::Folder(id) => fly_ask::AskScope::Folder(id),
                },
                now: req.now,
            },
        )
        .await;
        use fly_ask::AnswerStatus as St;
        let outcome = match a.status {
            St::Answered | St::Unverified => c::AskOutcome::Answered,
            // A scope that can't be searched has nothing to cite.
            St::NotFound | St::Refused => c::AskOutcome::NotFound,
            St::ModelFailed => c::AskOutcome::ModelFailed(failure(
                a.failure.as_ref().ok_or("model failure without a reason")?,
            )?),
            St::Cancelled => return Err("cancelled".into()),
        };
        let status = |s: fly_ask::SentenceStatus| match s {
            fly_ask::SentenceStatus::Sourced => c::SentenceStatus::Sourced,
            fly_ask::SentenceStatus::Unsourced => c::SentenceStatus::Unsourced,
            fly_ask::SentenceStatus::Flagged => c::SentenceStatus::Flagged,
        };
        Ok(c::AskAnswer {
            outcome,
            sentences: a
                .sentences
                .iter()
                .map(|s| c::AnswerSentence {
                    text: s.text.clone(),
                    citations: s.citations.clone(),
                    status: status(s.status),
                })
                .collect(),
            sources: a
                .sources
                .iter()
                .map(|s| c::SourceView {
                    meeting_id: s.meeting_id.clone(),
                    segment_ids: s.lines.iter().map(|l| l.segment_id.clone()).collect(),
                    label: s.label.clone(),
                })
                .collect(),
            trace: a.trace.lines(),
            proposals: a
                .proposals
                .iter()
                .map(|p| c::ProposalView {
                    tool: p.tool.name().into(),
                    preview: p.preview(),
                    executed: p.executed,
                })
                .collect(),
            display_frames: frames.into_inner().expect("frames"),
            timing: c::AskTiming {
                first_token_ms: a.timing.first_token_ms,
                total_ms: a.timing.total_ms,
            },
            run_log: serde_json::to_value(&a.run_log).map_err(|e| e.to_string())?,
        })
    }
}
