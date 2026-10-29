//! The harness's view of the provider it was handed: every model call goes
//! through [`Metered`], which
//! * streams (`chat_stream`) and forwards text deltas to the UI;
//! * enforces the tier's model-call and input-token budgets: the last
//!   affordable call is forced to answer (`ToolChoice::None`), and a call
//!   past the hard cap is refused;
//! * stops at a cancel request;
//! * detects a silently truncated prompt: Ollama reports how many prompt
//!   tokens it evaluated (`prompt_eval_count`); far fewer than were sent
//!   means the server trimmed the front (its retry without `num_ctx` after
//!   a runner crash falls back to a small default context);
//! * records usage, stop reasons and timings for the run log.
//!
//! It never constructs a provider; it only wraps the one `build_provider`
//! made.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use fly_llm::stream::Accumulator;
use fly_llm::{
    Capabilities, ChatRequest, ChatResponse, ContentBlock, ConverseRequest, LLMProvider, LlmError,
    StopReason, StreamEvent, ToolChoice, Usage,
};
use futures_util::StreamExt;

use crate::events::{emit, AskEvent, EventSink};

/// The error text a cancelled call returns (never shown as a model error).
pub const CANCELLED: &str = "Ask was cancelled.";
/// The error text for a call past the hard budget.
pub const OVER_BUDGET: &str = "Ask used up its model-call budget.";

#[derive(Debug, Default, Clone)]
pub struct Meter {
    pub calls: u32,
    /// Estimated input tokens over every call (chars/3).
    pub est_input: u32,
    pub usage: Usage,
    pub first_token_ms: Option<u64>,
    pub model_ms: u64,
    pub truncation_suspected: bool,
    /// A call was forced to answer by the call or token budget.
    pub budget_forced: bool,
    pub stops: Vec<StopReason>,
}

pub struct Metered<'a> {
    inner: &'a dyn LLMProvider,
    sink: Option<EventSink<'a>>,
    cancel: Option<&'a AtomicBool>,
    max_calls: u32,
    max_input_tokens: u32,
    started: Instant,
    meter: Mutex<Meter>,
}

/// chars/3 over everything sent (the provider's own `num_ctx` margin).
pub fn estimate_tokens(req: &ConverseRequest) -> u32 {
    (request_chars(req) / 3) as u32
}

fn request_chars(req: &ConverseRequest) -> usize {
    let blocks: usize = req
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .map(|b| match b {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
            ContentBlock::ToolResult { content, .. } => content.len(),
            ContentBlock::Opaque { .. } => 0,
        })
        .sum();
    let tools: usize = req
        .tools
        .iter()
        .map(|t| t.name.len() + t.description.len() + t.input_schema.to_string().len())
        .sum();
    blocks + tools
}

/// A first-call prompt this large whose evaluated token count is under
/// 70% of the ~chars/4 it should be was trimmed by the server.
fn looks_truncated(chars: usize, evaluated: Option<u32>) -> bool {
    let expected = (chars / 4) as u32;
    match evaluated {
        Some(n) if expected >= 1_500 => n.saturating_mul(10) < expected.saturating_mul(7),
        _ => false,
    }
}

impl<'a> Metered<'a> {
    pub fn new(
        inner: &'a dyn LLMProvider,
        sink: Option<EventSink<'a>>,
        cancel: Option<&'a AtomicBool>,
        max_calls: u32,
        max_input_tokens: u32,
        started: Instant,
    ) -> Self {
        Self {
            inner,
            sink,
            cancel,
            max_calls,
            max_input_tokens,
            started,
            meter: Mutex::new(Meter::default()),
        }
    }

    pub fn meter(&self) -> Meter {
        self.meter.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.is_some_and(|c| c.load(Ordering::Relaxed))
    }

    /// Calls still affordable (0 = the next one is refused).
    pub fn calls_left(&self) -> u32 {
        self.max_calls.saturating_sub(self.meter().calls)
    }

    pub fn tokens_left(&self) -> u32 {
        self.max_input_tokens.saturating_sub(self.meter().est_input)
    }
}

fn is(err: &LlmError, text: &str) -> bool {
    matches!(err, LlmError::Unavailable(t) if t == text)
}

pub fn is_cancel(err: &LlmError) -> bool {
    is(err, CANCELLED)
}

pub fn is_over_budget(err: &LlmError) -> bool {
    is(err, OVER_BUDGET)
}

#[async_trait::async_trait]
impl LLMProvider for Metered<'_> {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn is_local(&self) -> bool {
        self.inner.is_local()
    }

    async fn chat(&self, req: ChatRequest) -> fly_llm::Result<String> {
        Ok(self.converse(req.into()).await?.text())
    }

    async fn test_connection(&self) -> fly_llm::Result<()> {
        self.inner.test_connection().await
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn converse(&self, mut req: ConverseRequest) -> fly_llm::Result<ChatResponse> {
        if self.cancelled() {
            return Err(LlmError::Unavailable(CANCELLED.into()));
        }
        let chars = request_chars(&req);
        let est = (chars / 3) as u32;
        let turn = {
            let mut m = self.meter.lock().unwrap_or_else(|p| p.into_inner());
            if m.calls >= self.max_calls {
                return Err(LlmError::Unavailable(OVER_BUDGET.into()));
            }
            // Keep one call for a retry: the second-to-last affordable call
            // (or the one that would cross the token budget) must answer.
            let last_for_tools = m.calls + 2 >= self.max_calls
                || m.est_input.saturating_add(est) > self.max_input_tokens;
            if last_for_tools && req.has_tools() && req.tool_choice != ToolChoice::None {
                req.tool_choice = ToolChoice::None;
                m.budget_forced = true;
            }
            m.calls += 1;
            m.est_input = m.est_input.saturating_add(est);
            m.calls
        };
        emit(self.sink, AskEvent::TurnStart { turn });
        let t0 = Instant::now();
        let result: fly_llm::Result<ChatResponse> = async {
            let mut stream = self.inner.chat_stream(req).await?;
            let mut acc = Accumulator::default();
            while let Some(ev) = stream.next().await {
                if self.cancelled() {
                    return Err(LlmError::Unavailable(CANCELLED.into()));
                }
                let ev = ev?;
                if let StreamEvent::TextDelta { text } = &ev {
                    if !text.is_empty() {
                        {
                            let mut m = self.meter.lock().unwrap_or_else(|p| p.into_inner());
                            if m.first_token_ms.is_none() {
                                m.first_token_ms = Some(self.started.elapsed().as_millis() as u64);
                            }
                        }
                        emit(
                            self.sink,
                            AskEvent::TextDelta {
                                turn,
                                text: text.clone(),
                            },
                        );
                    }
                }
                acc.push(ev);
            }
            acc.finish()
        }
        .await;
        let mut m = self.meter.lock().unwrap_or_else(|p| p.into_inner());
        m.model_ms += t0.elapsed().as_millis() as u64;
        let resp = result?;
        m.usage = m.usage + resp.usage;
        m.stops.push(resp.stop_reason);
        if turn == 1 && self.inner.is_local() && looks_truncated(chars, resp.usage.input_tokens) {
            m.truncation_suspected = true;
            tracing::warn!("ask: the server evaluated far fewer prompt tokens than were sent");
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_llm::mock::MockLLMProvider;
    use fly_llm::{Message, ToolSpec};
    use serde_json::json;

    fn tools() -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "t".into(),
            description: "d".into(),
            input_schema: json!({"type": "object"}),
            strict: false,
        }]
    }

    #[test]
    fn truncation_heuristic() {
        assert!(
            looks_truncated(24_000, Some(2_048)),
            "6k tokens sent, 2k read"
        );
        assert!(!looks_truncated(24_000, Some(5_600)));
        assert!(
            !looks_truncated(3_000, Some(10)),
            "small prompts aren't judged"
        );
        assert!(!looks_truncated(24_000, None), "no count, no claim");
    }

    #[tokio::test]
    async fn budgets_force_an_answer_then_refuse() {
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse::text_reply("a"),
            ChatResponse::text_reply("b"),
            ChatResponse::text_reply("c"),
        ]);
        let m = Metered::new(&mock, None, None, 3, 1_000_000, Instant::now());
        let req = ConverseRequest {
            messages: vec![Message::user("q")],
            tools: tools(),
            ..Default::default()
        };
        m.converse(req.clone()).await.unwrap();
        m.converse(req.clone()).await.unwrap();
        m.converse(req.clone()).await.unwrap();
        let seen = mock.requests();
        assert_eq!(seen[0].tool_choice, ToolChoice::Auto);
        assert_eq!(
            seen[1].tool_choice,
            ToolChoice::None,
            "keeps one call for a retry"
        );
        assert!(m.meter().budget_forced);
        let err = m.converse(req).await.unwrap_err();
        assert!(is_over_budget(&err));
    }

    #[tokio::test]
    async fn cancel_stops_before_calling() {
        let mock = MockLLMProvider::scripted(vec![ChatResponse::text_reply("a")]);
        let flag = AtomicBool::new(true);
        let m = Metered::new(&mock, None, Some(&flag), 3, 1_000_000, Instant::now());
        let err = m
            .converse(ConverseRequest {
                messages: vec![Message::user("q")],
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(is_cancel(&err));
        assert_eq!(mock.remaining(), 1);
    }

    #[tokio::test]
    async fn text_deltas_reach_the_sink() {
        let mock = MockLLMProvider::scripted(vec![ChatResponse::text_reply("hello")]);
        let seen = Mutex::new(Vec::new());
        let sink = |e: AskEvent| seen.lock().unwrap().push(e);
        let m = Metered::new(&mock, Some(&sink), None, 2, 1_000_000, Instant::now());
        m.converse(ConverseRequest {
            messages: vec![Message::user("q")],
            ..Default::default()
        })
        .await
        .unwrap();
        let evs = seen.lock().unwrap();
        assert!(matches!(evs[0], AskEvent::TurnStart { turn: 1 }));
        assert!(matches!(&evs[1], AskEvent::TextDelta { text, .. } if text == "hello"));
        assert!(m.meter().first_token_ms.is_some());
    }
}
