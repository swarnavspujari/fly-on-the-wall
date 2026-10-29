//! The harness's view of the provider it was handed: every model call goes
//! through [`Metered`], which
//! * streams (`chat_stream`) and shows the UI sanitized frames of the reply
//!   so far — reasoning tags, links and images removed — never raw text;
//! * enforces the budget: model calls, the largest request and the summed
//!   input tokens. The last affordable call (or one whose successor would
//!   cross the token cap) is forced to answer (`ToolChoice::None`); a call
//!   past a cap is refused before anything is sent;
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
use crate::tier::Budget;

/// The error text a cancelled call returns (never shown as a model error).
pub const CANCELLED: &str = "Ask was cancelled.";
/// The error text for a call past a budget cap.
pub const OVER_BUDGET: &str = "Ask used up its budget.";

/// Tokens of one text: characters / 4, rounded up — the unit every budget
/// is stated in (the same estimate the system evals measure with).
pub fn text_tokens(text: &str) -> u32 {
    (text.chars().count() as u32).div_ceil(4)
}

/// Estimated input tokens of one request: every content block plus the
/// tool definitions offered.
pub fn request_tokens(req: &ConverseRequest) -> u32 {
    let mut n = 0;
    for m in &req.messages {
        for b in &m.content {
            n += match b {
                ContentBlock::Text { text } => text_tokens(text),
                ContentBlock::ToolUse { name, input, .. } => {
                    text_tokens(name) + text_tokens(&input.to_string())
                }
                ContentBlock::ToolResult { content, .. } => text_tokens(content),
                ContentBlock::Opaque { .. } => 0,
            };
        }
    }
    for t in &req.tools {
        n += text_tokens(&t.name)
            + text_tokens(&t.description)
            + text_tokens(&t.input_schema.to_string());
    }
    n
}

/// Room a forced final answer needs beyond the request before it.
const FINAL_TURN_HEADROOM: u32 = 2_000;

#[derive(Debug, Default, Clone)]
pub struct Meter {
    pub calls: u32,
    /// Input tokens summed over every request sent.
    pub input_tokens: u32,
    /// The largest single request.
    pub max_request: u32,
    pub usage: Usage,
    pub first_token_ms: Option<u64>,
    pub model_ms: u64,
    pub truncation_suspected: bool,
    /// The previous request's size (for the cache-aware truncation check).
    pub last_request: u32,
    /// A call was forced to answer by a budget.
    pub budget_forced: bool,
    pub stops: Vec<StopReason>,
    /// Sanitized display frames shown while streaming, in order.
    pub frames: u32,
}

pub struct Metered<'a> {
    inner: &'a dyn LLMProvider,
    sink: Option<EventSink<'a>>,
    cancel: Option<&'a AtomicBool>,
    budget: Budget,
    started: Instant,
    meter: Mutex<Meter>,
}

/// A prompt this large whose evaluated token count is under 70% of what
/// was sent was trimmed by the server. On later rounds, Ollama doesn't
/// count the prefix it already holds, so `sent_tokens` is what's new since
/// the previous request (those can't be cached).
fn looks_truncated(sent_tokens: u32, evaluated: Option<u32>) -> bool {
    match evaluated {
        Some(n) if sent_tokens >= 1_500 => n.saturating_mul(10) < sent_tokens.saturating_mul(7),
        _ => false,
    }
}

/// What the UI may show of a reply in progress.
///
/// No citation markers: they aren't validated yet (a model may write ids it
/// was never shown, or half an id mid-stream). The validated answer, with
/// its citations, arrives as the final frame (`turn` 0).
pub fn frame(raw: &str) -> String {
    let t = crate::text::strip_reasoning(raw);
    let t = crate::text::take_markers(&t).0;
    // A marker still being written ("… [seg:x09-1") isn't one yet.
    let t = match t.rfind("[seg") {
        Some(i) if !t[i..].contains(']') => t[..i].trim_end().to_string(),
        _ => t,
    };
    crate::text::strip_links(&t).0
}

/// Show a frame after this much new text (and at line ends / the end).
const FRAME_EVERY_CHARS: usize = 48;

impl<'a> Metered<'a> {
    pub fn new(
        inner: &'a dyn LLMProvider,
        sink: Option<EventSink<'a>>,
        cancel: Option<&'a AtomicBool>,
        budget: Budget,
        started: Instant,
    ) -> Self {
        Self {
            inner,
            sink,
            cancel,
            budget,
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

    /// Whether a request of `tokens` may still be sent.
    pub fn affords(&self, tokens: u32) -> bool {
        let m = self.meter();
        m.calls < self.budget.max_model_calls
            && tokens <= self.budget.max_request_tokens
            && self
                .budget
                .max_input_tokens
                .is_none_or(|cap| m.input_tokens.saturating_add(tokens) <= cap)
    }

    fn emit_frame(&self, turn: u32, raw: &str) {
        emit(
            self.sink,
            AskEvent::Frame {
                turn,
                text: frame(raw),
            },
        );
        self.meter.lock().unwrap_or_else(|p| p.into_inner()).frames += 1;
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

/// Worth one more try: the network, a busy/unavailable server, a 5xx.
pub fn is_transient(err: &LlmError) -> bool {
    !is_cancel(err)
        && !is_over_budget(err)
        && matches!(
            err,
            LlmError::Network(_) | LlmError::Unavailable(_) | LlmError::Provider(_)
        )
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
        let tokens = request_tokens(&req);
        let turn = {
            let mut m = self.meter.lock().unwrap_or_else(|p| p.into_inner());
            let b = &self.budget;
            let over_input = b
                .max_input_tokens
                .is_some_and(|cap| m.input_tokens.saturating_add(tokens) > cap);
            if m.calls >= b.max_model_calls || tokens > b.max_request_tokens || over_input {
                return Err(LlmError::Unavailable(OVER_BUDGET.into()));
            }
            // The last affordable call must answer: no calls left after it,
            // or the next (larger) request wouldn't fit the token cap.
            let last_call = m.calls + 1 >= b.max_model_calls;
            let next_too_big = b.max_input_tokens.is_some_and(|cap| {
                m.input_tokens
                    .saturating_add(tokens)
                    .saturating_add(tokens + FINAL_TURN_HEADROOM)
                    > cap
            }) || tokens + FINAL_TURN_HEADROOM > b.max_request_tokens;
            if (last_call || next_too_big) && req.has_tools() && req.tool_choice != ToolChoice::None
            {
                req.tool_choice = ToolChoice::None;
                m.budget_forced = true;
            }
            m.calls += 1;
            m.input_tokens += tokens;
            m.max_request = m.max_request.max(tokens);
            m.calls
        };
        emit(self.sink, AskEvent::TurnStart { turn });
        let t0 = Instant::now();
        let result: fly_llm::Result<ChatResponse> = async {
            let mut stream = self.inner.chat_stream(req).await?;
            let mut acc = Accumulator::default();
            let mut text = String::new();
            let mut shown = 0usize;
            while let Some(ev) = stream.next().await {
                if self.cancelled() {
                    return Err(LlmError::Unavailable(CANCELLED.into()));
                }
                let ev = ev?;
                if let StreamEvent::TextDelta { text: delta } = &ev {
                    if !delta.is_empty() {
                        {
                            let mut m = self.meter.lock().unwrap_or_else(|p| p.into_inner());
                            if m.first_token_ms.is_none() {
                                m.first_token_ms = Some(self.started.elapsed().as_millis() as u64);
                            }
                        }
                        text.push_str(delta);
                        if text.len() >= shown + FRAME_EVERY_CHARS || delta.contains('\n') {
                            self.emit_frame(turn, &text);
                            shown = text.len();
                        }
                    }
                }
                acc.push(ev);
            }
            if text.len() > shown {
                self.emit_frame(turn, &text);
            }
            acc.finish()
        }
        .await;
        let mut m = self.meter.lock().unwrap_or_else(|p| p.into_inner());
        m.model_ms += t0.elapsed().as_millis() as u64;
        let resp = result?;
        m.usage = m.usage + resp.usage;
        m.stops.push(resp.stop_reason);
        // Every round: a server that ran with less context than asked, or
        // evaluated far fewer of the new prompt tokens than were sent.
        let fresh = if turn == 1 {
            tokens
        } else {
            tokens.saturating_sub(m.last_request)
        };
        m.last_request = tokens;
        if resp.usage.context_fallback
            || (self.inner.is_local() && looks_truncated(fresh, resp.usage.input_tokens))
        {
            m.truncation_suspected = true;
            tracing::warn!(
                turn,
                "ask: the server read less of the prompt than was sent"
            );
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier::{Limits, Tier};
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

    fn budget(calls: u32, input: Option<u32>, request: u32) -> Budget {
        Tier::C.budget_with(Limits {
            model_calls: calls,
            tool_rounds: 5,
            input_tokens: input,
            request_tokens: request,
        })
    }

    #[test]
    fn truncation_heuristic() {
        assert!(
            looks_truncated(6_000, Some(2_048)),
            "6k tokens sent, 2k read"
        );
        assert!(!looks_truncated(6_000, Some(5_600)));
        assert!(
            !looks_truncated(700, Some(10)),
            "small prompts aren't judged"
        );
        assert!(!looks_truncated(6_000, None), "no count, no claim");
    }

    fn reply_reading(tokens: u32, fallback: bool) -> ChatResponse {
        let mut r = ChatResponse::text_reply("a");
        r.usage.input_tokens = Some(tokens);
        r.usage.context_fallback = fallback;
        r
    }

    #[tokio::test]
    async fn truncation_is_checked_every_round_against_what_is_new() {
        let first = "x".repeat(8_000); // 2000 tokens
        let more = "y".repeat(8_000); // 2000 more
        let req1 = ConverseRequest {
            messages: vec![Message::user(first.clone())],
            ..Default::default()
        };
        let req2 = ConverseRequest {
            messages: vec![Message::user(first), Message::user(more)],
            ..Default::default()
        };
        // Round 2 reads only the new 2000 tokens (the rest is cached): fine.
        let mock = MockLLMProvider::scripted(vec![
            reply_reading(2_000, false),
            reply_reading(1_990, false),
        ]);
        let m = Metered::new(&mock, None, None, budget(8, None, 40_000), Instant::now());
        m.converse(req1.clone()).await.unwrap();
        m.converse(req2.clone()).await.unwrap();
        assert!(!m.meter().truncation_suspected);
        // Round 2 reads far fewer than its new tokens: cut.
        let mock =
            MockLLMProvider::scripted(vec![reply_reading(2_000, false), reply_reading(400, false)]);
        let m = Metered::new(&mock, None, None, budget(8, None, 40_000), Instant::now());
        m.converse(req1.clone()).await.unwrap();
        assert!(!m.meter().truncation_suspected);
        m.converse(req2).await.unwrap();
        assert!(m.meter().truncation_suspected, "a later round counts too");
        // A reply from the server's default context is suspect, whatever
        // it reports.
        let mock = MockLLMProvider::scripted(vec![reply_reading(2_000, true)]);
        let m = Metered::new(&mock, None, None, budget(8, None, 40_000), Instant::now());
        m.converse(req1).await.unwrap();
        assert!(m.meter().truncation_suspected);
    }

    #[test]
    fn token_estimate_is_chars_over_four() {
        assert_eq!(text_tokens(""), 0);
        assert_eq!(text_tokens("abcde"), 2);
        assert_eq!(text_tokens("éééé"), 1, "characters, not bytes");
    }

    #[test]
    fn frames_never_render_links() {
        let f = frame("Logo ![x](https://e.example/p.png) and [pi");
        assert!(!f.contains("http") && !f.contains("]("), "{f}");
        assert_eq!(frame("<think>plan</think>Hi"), "Hi");
    }

    #[tokio::test]
    async fn the_last_call_must_answer_and_a_call_past_the_cap_is_refused() {
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse::text_reply("a"),
            ChatResponse::text_reply("b"),
        ]);
        let m = Metered::new(&mock, None, None, budget(2, None, 40_000), Instant::now());
        let req = ConverseRequest {
            messages: vec![Message::user("q")],
            tools: tools(),
            ..Default::default()
        };
        m.converse(req.clone()).await.unwrap();
        m.converse(req.clone()).await.unwrap();
        let seen = mock.requests();
        assert_eq!(seen[0].tool_choice, ToolChoice::Auto);
        assert_eq!(
            seen[1].tool_choice,
            ToolChoice::None,
            "last affordable call"
        );
        assert!(m.meter().budget_forced);
        assert!(is_over_budget(&m.converse(req).await.unwrap_err()));
        assert_eq!(mock.requests().len(), 2, "nothing sent past the cap");
    }

    #[tokio::test]
    async fn token_caps_force_then_refuse() {
        let big = "x".repeat(4_000); // 1000 tokens
        let req = ConverseRequest {
            messages: vec![Message::user(big)],
            tools: tools(),
            ..Default::default()
        };
        // A request over the per-request cap is never sent.
        let mock = MockLLMProvider::scripted(vec![ChatResponse::text_reply("a")]);
        let m = Metered::new(&mock, None, None, budget(8, None, 500), Instant::now());
        assert!(is_over_budget(&m.converse(req.clone()).await.unwrap_err()));
        assert!(mock.requests().is_empty());
        // Under a summed cap, the call whose successor won't fit answers.
        let mock = MockLLMProvider::scripted(vec![ChatResponse::text_reply("a")]);
        let m = Metered::new(
            &mock,
            None,
            None,
            budget(8, Some(3_500), 40_000),
            Instant::now(),
        );
        m.converse(req).await.unwrap();
        assert_eq!(mock.requests()[0].tool_choice, ToolChoice::None);
    }

    #[tokio::test]
    async fn cancel_stops_before_calling() {
        let mock = MockLLMProvider::scripted(vec![ChatResponse::text_reply("a")]);
        let flag = AtomicBool::new(true);
        let m = Metered::new(
            &mock,
            None,
            Some(&flag),
            budget(3, None, 40_000),
            Instant::now(),
        );
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
    async fn sanitized_frames_reach_the_sink() {
        let mock = MockLLMProvider::scripted(vec![ChatResponse::text_reply(
            "hello ![t](https://e.example/t.png)",
        )]);
        let seen = Mutex::new(Vec::new());
        let sink = |e: AskEvent| seen.lock().unwrap().push(e);
        let m = Metered::new(
            &mock,
            Some(&sink),
            None,
            budget(2, None, 40_000),
            Instant::now(),
        );
        m.converse(ConverseRequest {
            messages: vec![Message::user("q")],
            ..Default::default()
        })
        .await
        .unwrap();
        let evs = seen.lock().unwrap();
        assert!(matches!(evs[0], AskEvent::TurnStart { turn: 1 }));
        assert!(matches!(&evs[1], AskEvent::Frame { text, .. } if text == "hello"));
        assert!(m.meter().first_token_ms.is_some());
    }
}
