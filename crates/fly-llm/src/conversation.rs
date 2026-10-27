//! Block-structured conversations: content blocks, tool definitions, and
//! the structured response every provider maps into.
//!
//! These types sit beside the legacy text-only [`ChatRequest`] /
//! [`ChatMessage`] rather than replacing them: the app builds those with
//! exhaustive struct literals, and every existing request must stay
//! byte-identical. A [`ChatRequest`] converts losslessly into a
//! [`ConverseRequest`] (one text block per message, no tools), which is how
//! `chat()` and `converse()` share one body builder per provider.

use serde::{Deserialize, Serialize};

use crate::{ChatMessage, ChatRequest, Role, ThinkingMode};

/// One piece of a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// The model asking to run a tool. `id` is the provider's call id
    /// (Anthropic `tool_use.id`, OpenAI `tool_calls[].id`); Ollama has no
    /// ids, so one is synthesized (`call_<n>`) and results are matched back
    /// by tool name.
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// The harness's answer to a [`ContentBlock::ToolUse`], sent in a user
    /// turn.
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    /// A provider-specific block that has to be echoed back verbatim on the
    /// next turn of a tool loop: Anthropic `thinking` / `redacted_thinking`
    /// blocks carry signatures the API checks when a tool result follows.
    /// Sent only to the provider that produced it; dropped for any other.
    /// May hold model reasoning text, so never log it.
    Opaque {
        provider: String,
        raw: serde_json::Value,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

/// A conversation turn made of blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self::new(Role::System, vec![ContentBlock::text(text)])
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, vec![ContentBlock::text(text)])
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::new(Role::Assistant, vec![ContentBlock::text(text)])
    }
    pub fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Self { role, content }
    }
    /// A user turn carrying tool results.
    pub fn tool_results(results: Vec<ContentBlock>) -> Self {
        Self::new(Role::User, results)
    }

    /// All text blocks, concatenated (no separator — providers return text
    /// split across blocks mid-sentence).
    pub fn text(&self) -> String {
        text_of(&self.content)
    }

    /// True when the message is plain text (what a legacy [`ChatMessage`]
    /// can express) — such messages serialize exactly as before.
    pub fn is_plain_text(&self) -> bool {
        self.content.len() == 1 && matches!(self.content[0], ContentBlock::Text { .. })
    }
}

impl From<ChatMessage> for Message {
    fn from(m: ChatMessage) -> Self {
        Self::new(m.role, vec![ContentBlock::Text { text: m.content }])
    }
}

pub(crate) fn text_of(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A tool the model may call. `input_schema` is a JSON Schema object.
///
/// `strict` asks for schema-guaranteed arguments where the provider
/// supports it (Anthropic strict tool use, OpenAI strict functions); for
/// that the schema must close every object (`additionalProperties: false`),
/// which the Anthropic mapping adds when missing. Providers without strict
/// mode ignore the flag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub strict: bool,
}

/// Whether the model must, may, or must not call a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    /// Model decides (every provider's default).
    #[default]
    Auto,
    /// Tools stay declared (history may reference them) but none may be
    /// called — used for the final "answer with what you have" turn.
    None,
    /// Some tool must be called. Best effort: Ollama has no `tool_choice`,
    /// and Anthropic's 5.5 / Fable models reject forced choice, so both fall
    /// back to auto. Check [`Capabilities::tool_choice_required`].
    Required,
}

/// A block-structured chat request, optionally with tools.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConverseRequest {
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    pub tool_choice: ToolChoice,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub thinking: ThinkingMode,
    /// JSON schema for structured output. Never combine with `tools` on
    /// Ollama (that request shape breaks tool calling, ollama#18944): the
    /// Ollama mapping rejects it.
    #[serde(default)]
    pub format: Option<serde_json::Value>,
    #[serde(default)]
    pub effort: Option<String>,
}

impl From<ChatRequest> for ConverseRequest {
    fn from(r: ChatRequest) -> Self {
        Self {
            messages: r.messages.into_iter().map(Message::from).collect(),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            temperature: r.temperature,
            max_tokens: r.max_tokens,
            thinking: r.thinking,
            format: r.format,
            effort: r.effort,
        }
    }
}

impl From<&ChatRequest> for ConverseRequest {
    fn from(r: &ChatRequest) -> Self {
        r.clone().into()
    }
}

impl ConverseRequest {
    /// Tools offered to the model this turn (none under `ToolChoice::None`
    /// on providers that can't express it).
    pub fn has_tools(&self) -> bool {
        !self.tools.is_empty()
    }

    pub fn tool(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.iter().find(|t| t.name == name)
    }
}

/// Why the model stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Finished its answer (also stop sequences).
    EndTurn,
    /// Wants tool results before continuing.
    ToolUse,
    /// Ran out of `max_tokens` (or context).
    MaxTokens,
}

/// Token accounting, when the provider reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    /// Prompt-cache reads (Anthropic).
    pub cache_read_tokens: Option<u32>,
    /// Prompt-cache writes (Anthropic).
    pub cache_write_tokens: Option<u32>,
}

/// Sum two reports; a field stays `None` only when both are `None`.
impl std::ops::Add for Usage {
    type Output = Usage;

    fn add(self, other: Usage) -> Usage {
        fn sum(a: Option<u32>, b: Option<u32>) -> Option<u32> {
            match (a, b) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
            }
        }
        Usage {
            input_tokens: sum(self.input_tokens, other.input_tokens),
            output_tokens: sum(self.output_tokens, other.output_tokens),
            cache_read_tokens: sum(self.cache_read_tokens, other.cache_read_tokens),
            cache_write_tokens: sum(self.cache_write_tokens, other.cache_write_tokens),
        }
    }
}

/// A provider's structured reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub blocks: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    #[serde(default)]
    pub usage: Usage,
}

impl ChatResponse {
    /// A plain text answer (tests, mocks).
    pub fn text_reply(text: impl Into<String>) -> Self {
        Self {
            blocks: vec![ContentBlock::text(text)],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        }
    }

    /// A single tool call (tests, mocks).
    pub fn tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        input: serde_json::Value,
    ) -> Self {
        Self {
            blocks: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input,
            }],
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        }
    }

    /// Concatenated text blocks.
    pub fn text(&self) -> String {
        text_of(&self.blocks)
    }

    /// `(id, name, input)` of every tool call, in order.
    pub fn tool_uses(&self) -> Vec<(&str, &str, &serde_json::Value)> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.as_str(), name.as_str(), input))
                }
                _ => None,
            })
            .collect()
    }

    /// The assistant turn to append to the history before sending results.
    pub fn to_message(&self) -> Message {
        Message::new(Role::Assistant, self.blocks.clone())
    }
}

/// One incremental piece of a streamed reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta {
        text: String,
    },
    ToolUseStart {
        id: String,
        name: String,
    },
    /// A fragment of the JSON arguments of tool call `id`. Fragments
    /// concatenate to the full JSON object (Ollama sends it in one piece).
    ToolInputDelta {
        id: String,
        partial_json: String,
    },
    /// A complete provider-opaque block (see [`ContentBlock::Opaque`]).
    Opaque {
        provider: String,
        raw: serde_json::Value,
    },
    Done {
        stop_reason: StopReason,
        usage: Usage,
    },
}

/// What a provider/model can do, for the Ask tier gate. `probed` is false
/// when the answer is a static guess rather than something the server said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Native tool calling.
    pub tools: bool,
    /// `ToolChoice::None` is honored (else tools are omitted instead).
    pub tool_choice_none: bool,
    /// `ToolChoice::Required` is honored (else it degrades to auto).
    pub tool_choice_required: bool,
    /// Strict (schema-guaranteed) tool arguments.
    pub strict_tools: bool,
    /// Most strict tools one request may declare (0 = no limit known).
    pub max_strict_tools: usize,
    /// Incremental `chat_stream()` (else it yields one batch at the end).
    pub streaming: bool,
    /// `ThinkingMode` changes what the model does.
    pub thinking_control: bool,
    /// `format` JSON-schema output is enforced.
    pub structured_output: bool,
    /// The server confirmed these (e.g. Ollama `/api/show`), rather than a
    /// static table.
    pub probed: bool,
}

/// Back to the legacy request (text-only messages; tools dropped). Used by
/// the default `converse()` for providers without native support.
pub(crate) fn to_chat_request(r: ConverseRequest) -> ChatRequest {
    ChatRequest {
        messages: r
            .messages
            .iter()
            .map(|m| ChatMessage {
                role: m.role,
                content: m.text(),
            })
            .collect(),
        temperature: r.temperature,
        max_tokens: r.max_tokens,
        thinking: r.thinking,
        format: r.format,
        effort: r.effort,
    }
}

/// A copy of `schema` with `additionalProperties: false` on every object
/// that declares properties — what strict tools / structured outputs need.
pub(crate) fn closed_schema(schema: &serde_json::Value) -> serde_json::Value {
    fn close(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                if map.contains_key("properties") && !map.contains_key("additionalProperties") {
                    map.insert("additionalProperties".into(), serde_json::json!(false));
                }
                for child in map.values_mut() {
                    close(child);
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(close),
            _ => {}
        }
    }
    let mut s = schema.clone();
    close(&mut s);
    s
}

/// Tool results for providers without an `is_error` flag carry the error
/// state in the text.
pub(crate) fn result_text(content: &str, is_error: bool) -> String {
    if is_error {
        format!("ERROR: {content}")
    } else {
        content.to_string()
    }
}

/// The name of the tool call `tool_use_id` answers: the nearest preceding
/// assistant `ToolUse` with that id (ids can repeat across Ollama rounds).
pub(crate) fn tool_name_for(
    messages: &[Message],
    before: usize,
    tool_use_id: &str,
) -> Option<String> {
    messages[..before].iter().rev().find_map(|m| {
        m.content.iter().find_map(|b| match b {
            ContentBlock::ToolUse { id, name, .. } if id == tool_use_id => Some(name.clone()),
            _ => None,
        })
    })
}
