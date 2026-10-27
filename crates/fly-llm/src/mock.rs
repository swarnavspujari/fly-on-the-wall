//! Deterministic mock provider for offline integration tests (spec §11).

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::conversation::{Capabilities, ChatResponse, ConverseRequest};
use crate::{ChatRequest, LLMProvider, LlmError, Result};

/// Echoes a canned, deterministic "enhancement" derived from the request so
/// tests can assert exact output without a network or a real model.
///
/// For tool-loop tests, [`MockLLMProvider::scripted`] replays a fixed list
/// of [`ChatResponse`]s from `converse()` (and `chat_stream()`, via the
/// trait's replay default) in order, recording every request it receives so
/// a test can assert what the harness sent — e.g. that a tool result went
/// back with the right id, or that no keep-local text reached a request.
pub struct MockLLMProvider {
    pub canned_response: Option<String>,
    script: Mutex<VecDeque<ChatResponse>>,
    seen: Mutex<Vec<ConverseRequest>>,
}

impl MockLLMProvider {
    pub fn new() -> Self {
        Self {
            canned_response: None,
            script: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub fn with_response(response: impl Into<String>) -> Self {
        Self {
            canned_response: Some(response.into()),
            ..Self::new()
        }
    }

    /// Replies to `converse()` with these responses, in order. Running past
    /// the end is an error (a harness that loops more than scripted fails).
    pub fn scripted(responses: Vec<ChatResponse>) -> Self {
        Self {
            script: Mutex::new(responses.into()),
            ..Self::new()
        }
    }

    /// Every `converse()` request received so far.
    pub fn requests(&self) -> Vec<ConverseRequest> {
        self.seen.lock().expect("mock lock").clone()
    }

    /// Scripted responses not yet consumed.
    pub fn remaining(&self) -> usize {
        self.script.lock().expect("mock lock").len()
    }
}

impl Default for MockLLMProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl LLMProvider for MockLLMProvider {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn model(&self) -> &str {
        "mock"
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn chat(&self, req: ChatRequest) -> Result<String> {
        if let Some(resp) = &self.canned_response {
            return Ok(resp.clone());
        }
        let user_chars: usize = req
            .messages
            .iter()
            .filter(|m| m.role == crate::Role::User)
            .map(|m| m.content.len())
            .sum();
        Ok(format!(
            "## Mock summary\n\n- {} messages, {} user characters\n",
            req.messages.len(),
            user_chars
        ))
    }

    async fn test_connection(&self) -> Result<()> {
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: true,
            tool_choice_none: true,
            tool_choice_required: true,
            tool_choice_required_thinking_off: true,
            strict_tools: true,
            max_strict_tools: 0,
            streaming: false,
            thinking_control: false,
            structured_output: false,
            probed: false,
        }
    }

    async fn converse(&self, req: ConverseRequest) -> Result<ChatResponse> {
        self.seen.lock().expect("mock lock").push(req.clone());
        if let Some(next) = self.script.lock().expect("mock lock").pop_front() {
            return Ok(next);
        }
        if self.canned_response.is_some() && !req.has_tools() {
            let text = self.chat(crate::conversation::to_chat_request(req)).await?;
            return Ok(ChatResponse::text_reply(text));
        }
        Err(LlmError::Provider("mock script exhausted".into()))
    }
}
