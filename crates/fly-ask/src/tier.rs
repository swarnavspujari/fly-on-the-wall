//! Capability tiers (spec §4.2 step 3), decided by measured capability, not
//! a Settings dropdown:
//!
//! * **S** — local models up to ~8B (the default llama3.1): the harness
//!   prefetches, the model writes one answer. No model-chosen tool calls,
//!   at most 2 model calls (the answer and one retry).
//! * **M** — larger local models whose tool calling passed
//!   [`tool_smoke`]: ≤ 3 tool calls in ≤ 2 rounds.
//! * **C** — cloud models: ≤ 8 tool calls, ≤ 5 rounds, ≤ 40k input tokens
//!   for the whole answer.
//!
//! Every tier also has a prompt budget, so the request is sized to what the
//! model can take (and a silent Ollama front-truncation is detectable, see
//! `provider.rs`).

use fly_llm::tool_loop::{run_tool_loop, ToolLoopLimits};
use fly_llm::{Capabilities, ConverseRequest, LLMProvider, Message, ThinkingMode, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tier {
    S,
    M,
    C,
}

/// Declared limits for one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Budget {
    /// Model-chosen tool executions.
    pub max_tool_calls: u32,
    /// Model turns that may call tools.
    pub max_rounds: u32,
    /// Every model call: tool turns, the answer, a retry, a re-query.
    pub max_model_calls: u32,
    /// Estimated input tokens summed over every model call.
    pub max_input_tokens: u32,
    /// Transcript excerpts in the prefetch, in chars.
    pub context_chars: usize,
    /// Extracted items in the prefetch, in chars.
    pub item_chars: usize,
    /// The user's own notes (scratchpad) in the prefetch, in chars.
    pub note_chars: usize,
    /// Longest single excerpt, in chars (spec §4.2a #10: clipped passages
    /// for a faster first token).
    pub passage_clip_chars: usize,
    /// Note paragraphs from search (they can't be cited, so they're capped
    /// and placed after the transcript excerpts).
    pub max_note_passages: usize,
    /// Longest tool result passed back, in chars.
    pub observation_chars: usize,
    /// Answer length.
    pub max_output_tokens: u32,
}

impl Tier {
    pub fn budget(self) -> Budget {
        match self {
            Tier::S => Budget {
                max_tool_calls: 0,
                max_rounds: 0,
                max_model_calls: 2,
                max_input_tokens: 12_000,
                context_chars: 9_000,
                item_chars: 1_500,
                note_chars: 1_000,
                passage_clip_chars: 900,
                max_note_passages: 1,
                observation_chars: 0,
                max_output_tokens: 1_024,
            },
            Tier::M => Budget {
                max_tool_calls: 3,
                max_rounds: 2,
                max_model_calls: 5,
                max_input_tokens: 32_000,
                context_chars: 12_000,
                item_chars: 2_000,
                note_chars: 1_500,
                passage_clip_chars: 1_100,
                max_note_passages: 1,
                observation_chars: 4_000,
                max_output_tokens: 1_536,
            },
            Tier::C => Budget {
                max_tool_calls: 8,
                max_rounds: 5,
                max_model_calls: 8,
                max_input_tokens: 40_000,
                context_chars: 15_000,
                item_chars: 3_000,
                note_chars: 2_500,
                passage_clip_chars: 1_500,
                max_note_passages: 2,
                observation_chars: 6_000,
                max_output_tokens: 4_096,
            },
        }
    }

    pub fn uses_tools(self) -> bool {
        self != Tier::S
    }

    pub fn loop_limits(self) -> ToolLoopLimits {
        let b = self.budget();
        ToolLoopLimits {
            max_calls: b.max_tool_calls,
            max_rounds: b.max_rounds,
            max_observation_chars: b.observation_chars,
        }
    }
}

/// Parameter count (billions) from a model tag: `qwen3.5:9b` → 9,
/// `gemma4:26b-a4b` → 26, `llama3.1` → 8 (known families without a size
/// tag). `None` when unknown.
pub fn model_params_b(model: &str) -> Option<f32> {
    let model = model.trim().to_lowercase();
    let (base, tag) = model.split_once(':').unwrap_or((model.as_str(), ""));
    let sized = |s: &str| -> Option<f32> {
        s.split(['-', '_'])
            .find_map(|part| part.strip_suffix('b')?.parse::<f32>().ok())
    };
    if let Some(n) = sized(tag) {
        return Some(n);
    }
    // Known default sizes when no tag says otherwise.
    match base {
        "llama3.1" | "llama3" => Some(8.0),
        "llama3.2" => Some(3.0),
        "mistral" => Some(7.0),
        "gemma3" => Some(4.0),
        "qwen3.5" => Some(4.0),
        _ => sized(base),
    }
}

/// What the tier decision is based on.
#[derive(Debug, Clone, Copy)]
pub struct TierInputs<'a> {
    /// A local kind of provider (Ollama), wherever it runs.
    pub local_model: bool,
    pub model: &'a str,
    pub caps: Capabilities,
    /// [`tool_smoke`] result for this model (`None` = not run).
    pub tool_smoke: Option<bool>,
}

/// Largest local model that stays on tier S however well it calls tools.
pub const S_MAX_PARAMS_B: f32 = 8.5;

pub fn measure_tier(i: TierInputs<'_>) -> Tier {
    if !i.local_model {
        return Tier::C;
    }
    let big = model_params_b(i.model).is_some_and(|p| p > S_MAX_PARAMS_B);
    if big && i.caps.tools && i.caps.probed && i.tool_smoke == Some(true) {
        Tier::M
    } else {
        Tier::S
    }
}

/// Whether running [`tool_smoke`] could change the tier (skip it otherwise).
pub fn smoke_worth_running(i: TierInputs<'_>) -> bool {
    i.local_model
        && i.caps.tools
        && i.caps.probed
        && model_params_b(i.model).is_some_and(|p| p > S_MAX_PARAMS_B)
}

/// Why the smoke test failed (typed, for the run log and settings cache).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeResult {
    Passed,
    NoToolCall,
    BadArguments,
    EmptyAfterResult,
    WrongAnswer,
    Error,
}

/// A tiny tool-calling check: call one tool with the right argument, then
/// answer from its result. Catches models that ignore tools, invent
/// arguments, or (gemma4 with `think: false`, ollama#18861) go silent
/// after a tool result.
pub async fn tool_smoke(provider: &dyn LLMProvider) -> SmokeResult {
    let tool = ToolSpec {
        name: "lookup_codename".into(),
        description: "Look up a project's codename by its number.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"project": {"type": "integer", "description": "Project number."}},
            "required": ["project"],
            "additionalProperties": false
        }),
        strict: false,
    };
    let req = ConverseRequest {
        messages: vec![
            Message::system(
                "Use the lookup_codename tool to answer. Reply with the codename only.",
            ),
            Message::user("What is the codename of project 7?"),
        ],
        tools: vec![tool],
        temperature: Some(0.0),
        max_tokens: Some(200),
        thinking: ThinkingMode::Disabled,
        ..Default::default()
    };
    let mut saw_args: Option<bool> = None;
    let mut exec = |_: &str, input: &Value| -> Result<String, String> {
        let ok = match &input["project"] {
            Value::Number(n) => n.as_f64() == Some(7.0),
            Value::String(s) => s.trim() == "7",
            _ => false,
        };
        saw_args = Some(ok);
        if ok {
            Ok("Project 7 codename: BLUE HERON".into())
        } else {
            Err("Unknown project.".into())
        }
    };
    let limits = ToolLoopLimits {
        max_calls: 2,
        max_rounds: 2,
        max_observation_chars: 200,
    };
    let out = match run_tool_loop(provider, req, limits, &mut exec).await {
        Ok(o) => o,
        Err(_) => return SmokeResult::Error,
    };
    match saw_args {
        None => SmokeResult::NoToolCall,
        Some(false) => SmokeResult::BadArguments,
        Some(true) if out.answer.trim().is_empty() => SmokeResult::EmptyAfterResult,
        Some(true) if !out.answer.to_lowercase().contains("heron") => SmokeResult::WrongAnswer,
        Some(true) => SmokeResult::Passed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_llm::mock::MockLLMProvider;
    use fly_llm::ChatResponse;

    fn caps(tools: bool) -> Capabilities {
        Capabilities {
            tools,
            probed: true,
            ..Default::default()
        }
    }

    #[test]
    fn sizes_from_tags_and_families() {
        assert_eq!(model_params_b("qwen3.5:9b"), Some(9.0));
        assert_eq!(model_params_b("gemma4:26b-a4b"), Some(26.0));
        assert_eq!(model_params_b("gemma4:12b-it-qat"), Some(12.0));
        assert_eq!(model_params_b("qwen3.6:35b"), Some(35.0));
        assert_eq!(model_params_b("llama3.1"), Some(8.0));
        assert_eq!(model_params_b("llama3.1:latest"), Some(8.0));
        assert_eq!(model_params_b("granite4.1:3b"), Some(3.0));
        assert_eq!(model_params_b("something-custom"), None);
    }

    #[test]
    fn tiers_follow_measured_capability() {
        let t = |local, model, tools, smoke| {
            measure_tier(TierInputs {
                local_model: local,
                model,
                caps: caps(tools),
                tool_smoke: smoke,
            })
        };
        assert_eq!(t(false, "claude-sonnet-5-5", true, None), Tier::C);
        assert_eq!(t(true, "llama3.1", true, Some(true)), Tier::S, "8B stays S");
        assert_eq!(t(true, "qwen3.5:9b", true, Some(true)), Tier::M);
        assert_eq!(t(true, "qwen3.5:9b", true, Some(false)), Tier::S);
        assert_eq!(t(true, "qwen3.5:9b", true, None), Tier::S, "unmeasured");
        assert_eq!(
            t(true, "gemma4:12b", false, Some(true)),
            Tier::S,
            "no tools"
        );
        assert_eq!(t(true, "custom", true, Some(true)), Tier::S, "unknown size");
    }

    #[test]
    fn budgets_match_the_spec() {
        let s = Tier::S.budget();
        assert_eq!((s.max_tool_calls, s.max_model_calls), (0, 2));
        let m = Tier::M.budget();
        assert_eq!((m.max_tool_calls, m.max_rounds), (3, 2));
        let c = Tier::C.budget();
        assert_eq!(
            (c.max_tool_calls, c.max_rounds, c.max_input_tokens),
            (8, 5, 40_000)
        );
    }

    #[tokio::test]
    async fn smoke_passes_a_working_tool_caller_and_fails_silent_ones() {
        let good = MockLLMProvider::scripted(vec![
            ChatResponse::tool_call("c1", "lookup_codename", json!({"project": 7})),
            ChatResponse::text_reply("BLUE HERON"),
        ]);
        assert_eq!(tool_smoke(&good).await, SmokeResult::Passed);
        let silent = MockLLMProvider::scripted(vec![
            ChatResponse::tool_call("c1", "lookup_codename", json!({"project": 7})),
            ChatResponse::text_reply(""),
        ]);
        assert_eq!(tool_smoke(&silent).await, SmokeResult::EmptyAfterResult);
        let ignores = MockLLMProvider::scripted(vec![ChatResponse::text_reply("No idea.")]);
        assert_eq!(tool_smoke(&ignores).await, SmokeResult::NoToolCall);
    }
}
