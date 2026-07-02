//! looma-llm: the `LLMProvider` trait and its backends.
//!
//! Backends (landing in M4): NVIDIA NIM, OpenAI, Anthropic Claude, and local
//! Ollama — all bring-your-own-key/base-URL. `is_local()` drives the UI's
//! "this stays on your machine" vs "this calls out" indicator.

use serde::{Deserialize, Serialize};

pub mod anthropic;
pub mod mock;
pub mod openai_compat;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

/// Per-provider connection settings, editable in the app's Settings screen.
/// The API key itself lives in the OS keychain (looma-secrets), never here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub base_url: String,
    pub model: String,
}

#[async_trait::async_trait]
pub trait LLMProvider: Send + Sync {
    /// Stable id: "openai", "anthropic", "nim", "ollama".
    fn id(&self) -> &'static str;
    /// True when inference happens on this machine (Ollama).
    fn is_local(&self) -> bool;
    async fn chat(&self, req: ChatRequest) -> Result<String>;
    /// Cheap round-trip used by the Settings "test connection" button.
    async fn test_connection(&self) -> Result<()>;
}
