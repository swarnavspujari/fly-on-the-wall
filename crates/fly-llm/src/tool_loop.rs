//! A bounded, provider-agnostic tool loop.
//!
//! The Ask harness (spec §4.2, tiers M and C) owns scope, prefetch,
//! citations and proposals; this is only the mechanical part every tier
//! shares: call the model, run the tools it asks for, feed results back,
//! stop at a budget. Rules (spec §4.2a #12):
//! * declared budgets — `max_calls` tool executions and `max_rounds` tool
//!   turns; when either runs out, pending calls get an error result and the
//!   model gets one last turn with `ToolChoice::None` to answer;
//! * unknown tool → an error result naming the real tools (no execution);
//! * observations are truncated to `max_observation_chars`;
//! * a tool call written as text is parsed once ([`crate::repair`]); a
//!   malformed one earns one corrective turn, and a second malformed call
//!   ends the loop with [`LlmError::ToolProtocol`];
//! * the outcome records tool names and success only — never arguments or
//!   results, which can contain meeting content.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::conversation::{
    ChatResponse, ContentBlock, ConverseRequest, Message, StopReason, ToolChoice, Usage,
};
use crate::repair::{self, TextToolCall};
use crate::{LLMProvider, LlmError, Result};

/// Budgets for one loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolLoopLimits {
    /// Most tool executions in total.
    pub max_calls: u32,
    /// Most model turns that may call tools.
    pub max_rounds: u32,
    /// Longest tool result passed back to the model, in chars.
    pub max_observation_chars: usize,
}

impl Default for ToolLoopLimits {
    /// Tier C (cloud) budgets from the spec: ≤ 8 calls / 5 rounds.
    fn default() -> Self {
        Self {
            max_calls: 8,
            max_rounds: 5,
            max_observation_chars: 6_000,
        }
    }
}

/// How the loop ended (a capped run is not a finished one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopEnd {
    /// The model answered on its own.
    Done,
    /// The call budget ran out; the answer was forced.
    CallBudget,
    /// The round budget ran out; the answer was forced.
    RoundBudget,
    /// The final answer hit `max_tokens`.
    MaxTokens,
}

/// One tool execution, for the run log. Deliberately content-free: `name`
/// is always a declared tool's name, or [`UNKNOWN_TOOL`] when the model
/// asked for one that doesn't exist (its invented name is model output and
/// never recorded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub name: String,
    pub ok: bool,
    /// Recovered from a text-written call.
    pub repaired: bool,
}

#[derive(Debug, Clone)]
pub struct ToolLoopOutcome {
    pub answer: String,
    pub end: LoopEnd,
    /// Full history including tool turns (for a follow-up question).
    pub messages: Vec<Message>,
    pub calls: Vec<ToolCallRecord>,
    /// Model turns that called tools.
    pub rounds: u32,
    /// Summed over every model call.
    pub usage: Usage,
    /// Model calls made (tool turns + corrective + final).
    pub model_calls: u32,
}

/// Runs a tool: `Ok(observation)` or `Err(message shown to the model)`.
pub type ToolExecutor<'a> =
    dyn FnMut(&str, &Value) -> std::result::Result<String, String> + Send + 'a;

/// `ToolCallRecord::name` for a call to a tool that wasn't offered.
pub const UNKNOWN_TOOL: &str = "<unknown tool>";

/// Longest model-invented tool name echoed back to the model itself.
const MAX_ECHOED_NAME: usize = 64;

const BUDGET_MESSAGE: &str =
    "Tool budget used up. Answer now using only the information you already have.";

/// Run the loop. `req.tools` are the tools on offer; `exec` performs them.
pub async fn run_tool_loop(
    provider: &dyn LLMProvider,
    mut req: ConverseRequest,
    limits: ToolLoopLimits,
    exec: &mut ToolExecutor<'_>,
) -> Result<ToolLoopOutcome> {
    let mut calls: Vec<ToolCallRecord> = Vec::new();
    let mut usage = Usage::default();
    let mut rounds = 0u32;
    let mut model_calls = 0u32;
    let mut corrected = false;
    let mut repaired_ids = 0u32;

    loop {
        let mut resp = provider.converse(req.clone()).await?;
        model_calls += 1;
        usage = usage + resp.usage;

        let mut repaired = false;
        if resp.tool_uses().is_empty() && req.has_tools() && req.tool_choice != ToolChoice::None {
            match repair::classify(&resp.text(), &req.tools) {
                TextToolCall::NotACall => {}
                TextToolCall::Call { name, input } => {
                    repaired_ids += 1;
                    // Keep provider-opaque blocks (Anthropic thinking): the
                    // API checks them on the turn that carries tool results.
                    // The call's text itself is replaced by the call.
                    let mut blocks: Vec<ContentBlock> = resp
                        .blocks
                        .into_iter()
                        .filter(|b| matches!(b, ContentBlock::Opaque { .. }))
                        .collect();
                    blocks.push(ContentBlock::ToolUse {
                        id: format!("repaired_{repaired_ids}"),
                        name,
                        input,
                    });
                    resp = ChatResponse {
                        blocks,
                        stop_reason: StopReason::ToolUse,
                        usage: resp.usage,
                    };
                    repaired = true;
                }
                TextToolCall::Malformed { reason } => {
                    if corrected {
                        return Err(LlmError::ToolProtocol(reason));
                    }
                    corrected = true;
                    // The whole reply (opaque blocks included), not just text.
                    req.messages.push(resp.to_message());
                    req.messages.push(Message::user(format!(
                        "That looked like a tool call, but {reason}. Either call a tool \
                         with valid JSON arguments, or answer in plain text."
                    )));
                    continue;
                }
            }
        }

        let uses: Vec<(String, String, Value)> = resp
            .tool_uses()
            .into_iter()
            .map(|(id, name, input)| (id.to_string(), name.to_string(), input.clone()))
            .collect();
        if uses.is_empty() {
            let end = if resp.stop_reason == StopReason::MaxTokens {
                LoopEnd::MaxTokens
            } else {
                LoopEnd::Done
            };
            let answer = resp.text();
            req.messages.push(resp.to_message());
            return Ok(ToolLoopOutcome {
                answer,
                end,
                messages: req.messages,
                calls,
                rounds,
                usage,
                model_calls,
            });
        }

        rounds += 1;
        req.messages.push(resp.to_message());
        let mut results = Vec::with_capacity(uses.len());
        let mut out_of_calls = false;
        let mut bad_args = false;
        for (id, name, input) in uses {
            let result = |content: String, is_error: bool| ContentBlock::ToolResult {
                tool_use_id: id.clone(),
                content,
                is_error,
            };
            if calls.len() as u32 >= limits.max_calls {
                out_of_calls = true;
                results.push(result(BUDGET_MESSAGE.into(), true));
                continue;
            }
            if req.tool(&name).is_none() {
                results.push(result(
                    format!(
                        "There is no tool named {:?}. Available tools: {}.",
                        truncate_plain(&name, MAX_ECHOED_NAME),
                        tool_names(&req)
                    ),
                    true,
                ));
                calls.push(ToolCallRecord {
                    name: UNKNOWN_TOOL.into(),
                    ok: false,
                    repaired,
                });
                continue;
            }
            if !input.is_object() {
                bad_args = true;
                results.push(result(
                    "The arguments were not a valid JSON object. Call the tool again \
                     with arguments matching its schema."
                        .into(),
                    true,
                ));
                calls.push(ToolCallRecord {
                    name,
                    ok: false,
                    repaired,
                });
                continue;
            }
            let (content, ok) = match exec(&name, &input) {
                Ok(obs) => (truncate(&obs, limits.max_observation_chars), true),
                Err(e) => (truncate(&e, limits.max_observation_chars), false),
            };
            results.push(result(content, !ok));
            calls.push(ToolCallRecord { name, ok, repaired });
        }
        req.messages.push(Message::tool_results(results));

        if bad_args {
            if corrected {
                return Err(LlmError::ToolProtocol(
                    "tool arguments were not a JSON object twice".into(),
                ));
            }
            corrected = true;
        }

        if out_of_calls || rounds >= limits.max_rounds {
            // Forced final answer: tools stay declared (the history refers
            // to them) but none may be called.
            req.tool_choice = ToolChoice::None;
            let resp = provider.converse(req.clone()).await?;
            model_calls += 1;
            usage = usage + resp.usage;
            let text = resp.text();
            let still_calling = !resp.tool_uses().is_empty()
                || repair::classify(&text, &req.tools) != TextToolCall::NotACall;
            // The answer was forced by a budget, so the run is capped, never
            // `Done` — whatever the model did on this last turn.
            let end = if out_of_calls {
                LoopEnd::CallBudget
            } else {
                LoopEnd::RoundBudget
            };
            let answer = if still_calling { String::new() } else { text };
            // Keep only text: an unanswered tool_use would poison a follow-up.
            req.messages.push(Message::assistant(answer.clone()));
            return Ok(ToolLoopOutcome {
                answer,
                end,
                messages: req.messages,
                calls,
                rounds,
                usage,
                model_calls,
            });
        }
    }
}

fn tool_names(req: &ConverseRequest) -> String {
    req.tools
        .iter()
        .map(|t| t.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// At most `max` chars, no marker (for echoing model-supplied names).
fn truncate_plain(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Cut to at most `max` chars on a char boundary, marking the cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}\n… [truncated]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ToolSpec;
    use crate::mock::MockLLMProvider;
    use serde_json::json;

    fn tools() -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "search_passages".into(),
            description: "Search meeting passages".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
                "additionalProperties": false
            }),
            strict: true,
        }]
    }

    fn req() -> ConverseRequest {
        ConverseRequest {
            messages: vec![
                Message::system("Answer from the meetings."),
                Message::user("What was the budget?"),
            ],
            tools: tools(),
            ..Default::default()
        }
    }

    fn search_exec() -> impl FnMut(&str, &Value) -> std::result::Result<String, String> + Send {
        |name: &str, input: &Value| {
            assert_eq!(name, "search_passages");
            Ok(format!(
                "[seg:s1] Dana: the budget is $8,400 (query={})",
                input["query"]
            ))
        }
    }

    #[tokio::test]
    async fn mock_tool_loop_calls_then_answers() {
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse::tool_call("c1", "search_passages", json!({"query": "budget"})),
            ChatResponse::text_reply("The budget is $8,400 [seg:s1]."),
        ]);
        let mut exec = search_exec();
        let out = run_tool_loop(&mock, req(), ToolLoopLimits::default(), &mut exec)
            .await
            .unwrap();
        assert_eq!(out.answer, "The budget is $8,400 [seg:s1].");
        assert_eq!(out.end, LoopEnd::Done);
        assert_eq!(out.rounds, 1);
        assert_eq!(out.model_calls, 2);
        assert_eq!(
            out.calls,
            vec![ToolCallRecord {
                name: "search_passages".into(),
                ok: true,
                repaired: false
            }]
        );
        // The second request carried the assistant tool_use and the result.
        let seen = mock.requests();
        assert_eq!(seen.len(), 2);
        let second = &seen[1].messages;
        assert!(matches!(
            second[second.len() - 2].content[0],
            ContentBlock::ToolUse { .. }
        ));
        match &second[second.len() - 1].content[0] {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id, "c1");
                assert!(content.contains("$8,400"));
                assert!(!is_error);
            }
            other => panic!("expected tool result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn text_written_call_is_repaired_once() {
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse::text_reply(
                r#"<tool_call>{"name": "search_passages", "arguments": {"query": "budget"}}</tool_call>"#,
            ),
            ChatResponse::text_reply("It was $8,400 [seg:s1]."),
        ]);
        let mut exec = search_exec();
        let out = run_tool_loop(&mock, req(), ToolLoopLimits::default(), &mut exec)
            .await
            .unwrap();
        assert_eq!(out.end, LoopEnd::Done);
        assert!(out.calls[0].repaired);
        assert_eq!(out.answer, "It was $8,400 [seg:s1].");
    }

    #[tokio::test]
    async fn malformed_call_gets_one_correction_then_errors() {
        // One malformed reply, then a good answer → fine.
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse::text_reply(r#"<tool_call>{"name": "search_passages", "arguments": {"#),
            ChatResponse::text_reply("Plain answer."),
        ]);
        let mut exec = search_exec();
        let out = run_tool_loop(&mock, req(), ToolLoopLimits::default(), &mut exec)
            .await
            .unwrap();
        assert_eq!(out.answer, "Plain answer.");
        let corrective = mock.requests()[1].messages.last().unwrap().text();
        assert!(corrective.contains("not valid JSON"), "{corrective}");

        // Two malformed replies → ToolProtocol error.
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse::text_reply(r#"<tool_call>{"name": "nope"}</tool_call>"#),
            ChatResponse::text_reply(r#"<tool_call>{"name": "nope"}</tool_call>"#),
        ]);
        let mut exec = search_exec();
        let err = run_tool_loop(&mock, req(), ToolLoopLimits::default(), &mut exec)
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::ToolProtocol(_)), "{err}");
    }

    #[tokio::test]
    async fn unknown_tool_gets_an_error_result_without_execution() {
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse::tool_call("c1", "delete_meeting", json!({"id": "m1"})),
            ChatResponse::text_reply("Sorry."),
        ]);
        let mut exec = |_: &str, _: &Value| -> std::result::Result<String, String> {
            panic!("must not execute an unknown tool")
        };
        let out = run_tool_loop(&mock, req(), ToolLoopLimits::default(), &mut exec)
            .await
            .unwrap();
        assert!(!out.calls[0].ok);
        let result = mock.requests()[1].messages.last().unwrap().content[0].clone();
        match result {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                assert!(is_error);
                assert!(content.contains("search_passages"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn call_budget_forces_a_final_answer_without_tools() {
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse {
                blocks: vec![
                    ContentBlock::ToolUse {
                        id: "a".into(),
                        name: "search_passages".into(),
                        input: json!({"query": "1"}),
                    },
                    ContentBlock::ToolUse {
                        id: "b".into(),
                        name: "search_passages".into(),
                        input: json!({"query": "2"}),
                    },
                ],
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            },
            ChatResponse::text_reply("Best effort answer."),
        ]);
        let mut exec = search_exec();
        let limits = ToolLoopLimits {
            max_calls: 1,
            ..Default::default()
        };
        let out = run_tool_loop(&mock, req(), limits, &mut exec)
            .await
            .unwrap();
        assert_eq!(out.end, LoopEnd::CallBudget);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.answer, "Best effort answer.");
        let last = &mock.requests()[1];
        assert_eq!(last.tool_choice, ToolChoice::None);
        // Both tool_use ids were answered (one with the budget error).
        let results = &last.messages.last().unwrap().content;
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn round_budget_and_observation_truncation() {
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse::tool_call("a", "search_passages", json!({"query": "1"})),
            ChatResponse::text_reply("Done."),
        ]);
        let mut exec =
            |_: &str, _: &Value| -> std::result::Result<String, String> { Ok("x".repeat(50)) };
        let limits = ToolLoopLimits {
            max_rounds: 1,
            max_observation_chars: 10,
            ..Default::default()
        };
        let out = run_tool_loop(&mock, req(), limits, &mut exec)
            .await
            .unwrap();
        // The final answer was forced by the round budget: capped, not Done.
        assert_eq!(out.end, LoopEnd::RoundBudget);
        assert_eq!(out.answer, "Done.");
        assert_eq!(mock.requests()[1].tool_choice, ToolChoice::None);
        let obs = match &mock.requests()[1].messages.last().unwrap().content[0] {
            ContentBlock::ToolResult { content, .. } => content.clone(),
            other => panic!("{other:?}"),
        };
        assert!(obs.starts_with("xxxxxxxxxx\n… [truncated]"), "{obs}");
    }

    #[tokio::test]
    async fn outcome_records_hold_no_content() {
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse::tool_call("c1", "search_passages", json!({"query": "secret plan"})),
            ChatResponse::text_reply("ok"),
        ]);
        let mut exec = search_exec();
        let out = run_tool_loop(&mock, req(), ToolLoopLimits::default(), &mut exec)
            .await
            .unwrap();
        let log = serde_json::to_string(&out.calls).unwrap();
        assert!(!log.contains("secret"), "{log}");
    }

    #[tokio::test]
    async fn unknown_tool_names_are_not_recorded_or_echoed_unbounded() {
        let invented = format!("steal_{}", "x".repeat(500));
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse::tool_call("c1", invented.clone(), json!({})),
            ChatResponse::text_reply("ok"),
        ]);
        let mut exec = |_: &str, _: &Value| -> std::result::Result<String, String> {
            panic!("must not execute")
        };
        let out = run_tool_loop(&mock, req(), ToolLoopLimits::default(), &mut exec)
            .await
            .unwrap();
        assert_eq!(out.calls[0].name, UNKNOWN_TOOL);
        assert!(!serde_json::to_string(&out.calls).unwrap().contains("steal"));
        match &mock.requests()[1].messages.last().unwrap().content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert!(content.len() < 200, "{content}");
                assert!(content.contains("search_passages"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn repaired_call_keeps_thinking_blocks() {
        let thinking = ContentBlock::Opaque {
            provider: "anthropic".into(),
            raw: json!({"type": "thinking", "thinking": "plan", "signature": "sig"}),
        };
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse {
                blocks: vec![
                    thinking.clone(),
                    ContentBlock::text(
                        r#"<tool_call>{"name": "search_passages", "arguments": {"query": "q"}}</tool_call>"#,
                    ),
                ],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
            ChatResponse::text_reply("answer"),
        ]);
        let mut exec = search_exec();
        let out = run_tool_loop(&mock, req(), ToolLoopLimits::default(), &mut exec)
            .await
            .unwrap();
        assert!(out.calls[0].repaired);
        let second = &mock.requests()[1].messages;
        let assistant = &second[second.len() - 2];
        assert_eq!(assistant.content[0], thinking, "thinking block kept first");
        assert!(matches!(assistant.content[1], ContentBlock::ToolUse { .. }));
        assert_eq!(assistant.content.len(), 2, "the call text is replaced");
    }

    #[tokio::test]
    async fn malformed_turn_is_echoed_with_its_opaque_blocks() {
        let thinking = ContentBlock::Opaque {
            provider: "anthropic".into(),
            raw: json!({"type": "thinking", "thinking": "", "signature": "s"}),
        };
        let mock = MockLLMProvider::scripted(vec![
            ChatResponse {
                blocks: vec![
                    thinking.clone(),
                    ContentBlock::text(r#"<tool_call>{"name": "search_passages""#),
                ],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
            ChatResponse::text_reply("plain"),
        ]);
        let mut exec = search_exec();
        run_tool_loop(&mock, req(), ToolLoopLimits::default(), &mut exec)
            .await
            .unwrap();
        let second = &mock.requests()[1].messages;
        assert_eq!(second[second.len() - 2].content[0], thinking);
    }
}
