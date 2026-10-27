//! fly-llm: the `LLMProvider` trait and its backends.
//!
//! Backends: NVIDIA NIM, OpenAI, Anthropic Claude, and local Ollama — all
//! bring-your-own-key/base-URL. `is_local()` drives the UI's "this stays on
//! your machine" vs "this calls out" indicator.
//!
//! Two request surfaces share one body builder per provider:
//! * `chat(ChatRequest) -> String` — the original text-only call every
//!   current feature uses. Its request bodies are frozen by a golden test.
//! * `converse(ConverseRequest) -> ChatResponse` / `chat_stream()` — content
//!   blocks, tools, stop reasons and usage, for the agentic Ask harness.
//!
//! Providers are only ever constructed by the app's `build_provider`, which
//! enforces cloud consent and Keep local; nothing here changes that.

use std::pin::Pin;

use serde::{Deserialize, Serialize};

pub mod anthropic;
pub mod conversation;
pub mod mock;
pub mod openai_compat;
pub mod repair;
pub mod stream;
pub mod tool_loop;

#[cfg(test)]
mod golden;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("provider is not configured: {0}")]
    NotConfigured(String),
    #[error("authentication failed — check the API key")]
    Auth,
    #[error("provider returned an error: {0}")]
    Provider(String),
    #[error("network error: {0}")]
    Network(String),
    /// The provider exists but can't be reached right now — carries a
    /// person-actionable message (verbatim shown in the UI).
    #[error("{0}")]
    Unavailable(String),
    /// The request can't be expressed for this provider (e.g. `format` and
    /// `tools` together on Ollama, too many strict tools on Anthropic) — a
    /// caller bug, refused before anything is sent.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The model broke the tool-calling protocol and the one repair attempt
    /// failed too (see `tool_loop`).
    #[error("the model's tool call couldn't be understood: {0}")]
    ToolProtocol(String),
}

/// Turn a transport failure into something a person can act on. A local
/// provider refusing connections almost always means "Ollama isn't running",
/// which deserves better than a raw reqwest error with a URL in it.
pub fn transport_error(
    provider_id: &str,
    is_local: bool,
    base_url: &str,
    e: reqwest::Error,
) -> LlmError {
    if e.is_connect() {
        if is_local {
            return LlmError::Unavailable(format!(
                "Ollama isn't reachable at {base_url}. Start the Ollama app (or install it \
                 from ollama.com), or switch provider in Settings."
            ));
        }
        return LlmError::Unavailable(format!(
            "Can't reach {provider_id} — check your internet connection."
        ));
    }
    if e.is_timeout() {
        return LlmError::Unavailable(format!(
            "{provider_id} timed out. The model may still be loading — try again in a moment."
        ));
    }
    LlmError::Network(e.to_string())
}

pub type Result<T> = std::result::Result<T, LlmError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// Whether the provider should run its (adaptive/extended) thinking. Mechanical
/// transforms — like the transcript-cleanup pass — set `Disabled`: current
/// Anthropic models (e.g. claude-sonnet-5) run adaptive thinking by default,
/// and those reasoning tokens count against `max_tokens`, truncating the JSON
/// output mid-array. Providers without a thinking mode ignore this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingMode {
    /// Provider default (adaptive-on for current Anthropic models).
    #[default]
    Default,
    /// Turn extended thinking off — full token budget goes to the answer.
    Disabled,
    /// Ask for thinking where the provider lets a request opt in (Ollama
    /// `think: true`). Elsewhere the same as `Default`.
    Enabled,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Thinking control (see `ThinkingMode`); ignored by providers that don't
    /// support thinking.
    #[serde(default)]
    pub thinking: ThinkingMode,
    /// Optional JSON schema for structured output (grammar-constrained
    /// decoding). Honored by the Ollama native path (`format` field — the
    /// server compiles it to a llama.cpp grammar, making malformed JSON
    /// impossible); ignored by other providers. `None` = free-form text.
    #[serde(default)]
    pub format: Option<serde_json::Value>,
    /// Reasoning effort hint (`"low"` | `"medium"` | `"high"`), set per task
    /// from the model's prompt profile. Honored by Anthropic models that
    /// support `output_config.effort`; ignored elsewhere. `None` = model default.
    #[serde(default)]
    pub effort: Option<String>,
}

/// Per-provider connection settings, editable in the app's Settings screen.
/// The API key itself lives in the OS keychain (fly-secrets), never here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub base_url: String,
    pub model: String,
}

pub use conversation::{
    Capabilities, ChatResponse, ContentBlock, ConverseRequest, Message, StopReason, StreamEvent,
    ToolChoice, ToolSpec, Usage,
};

/// A streamed reply: events in order, ending with `StreamEvent::Done` (or an
/// error item).
pub type ChatStream = Pin<Box<dyn futures_util::Stream<Item = Result<StreamEvent>> + Send>>;

// See fly-diarize: async-trait output trips Rust 1.99 clippy::double_must_use.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait LLMProvider: Send + Sync {
    /// Stable id: "openai", "anthropic", "nim", "ollama".
    fn id(&self) -> &'static str;
    /// The resolved model this provider was built with (e.g. "llama3.1",
    /// "claude-sonnet-5"). Callers use it to look up per-model prompt
    /// profiles (`fly_core::prompt_profile`) at prompt-build time.
    fn model(&self) -> &str;
    /// True when inference happens on this machine (Ollama).
    fn is_local(&self) -> bool;
    async fn chat(&self, req: ChatRequest) -> Result<String>;
    /// Cheap round-trip used by the Settings "test connection" button.
    async fn test_connection(&self) -> Result<()>;

    /// What this provider/model supports, from a static table (no I/O).
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    /// Ask the server what the model supports (Ollama `/api/show`); falls
    /// back to [`LLMProvider::capabilities`] where there's nothing to ask.
    async fn probe_capabilities(&self) -> Result<Capabilities> {
        Ok(self.capabilities())
    }

    /// Block-structured chat with optional tools. The default serves
    /// text-only requests through `chat()` and refuses anything else, so a
    /// provider without tool support fails loudly instead of ignoring tools.
    async fn converse(&self, req: ConverseRequest) -> Result<ChatResponse> {
        if req.has_tools() || req.messages.iter().any(|m| !m.is_plain_text()) {
            return Err(LlmError::InvalidRequest(format!(
                "{} doesn't support tools or content blocks",
                self.id()
            )));
        }
        let text = self.chat(conversation::to_chat_request(req)).await?;
        Ok(ChatResponse::text_reply(text))
    }

    /// Streaming `converse()`. The default runs `converse()` and replays the
    /// whole reply as events (no incremental first token).
    async fn chat_stream(&self, req: ConverseRequest) -> Result<ChatStream> {
        let resp = self.converse(req).await?;
        Ok(stream::replay(resp))
    }
}
