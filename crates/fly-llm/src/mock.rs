//! Deterministic mock provider for offline integration tests (spec §11).

use crate::{ChatRequest, LLMProvider, Result};

/// Echoes a canned, deterministic "enhancement" derived from the request so
/// tests can assert exact output without a network or a real model.
pub struct MockLLMProvider {
    pub canned_response: Option<String>,
}

impl MockLLMProvider {
    pub fn new() -> Self {
        Self {
            canned_response: None,
        }
    }

    pub fn with_response(response: impl Into<String>) -> Self {
        Self {
            canned_response: Some(response.into()),
        }
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
}
