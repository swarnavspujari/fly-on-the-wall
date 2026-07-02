//! Anthropic Claude provider (`POST /v1/messages`).

use serde_json::json;

use crate::{ChatRequest, LLMProvider, LlmError, Result, Role};

pub const ANTHROPIC_DEFAULT_MODEL: &str = "claude-sonnet-5";

pub struct AnthropicProvider {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            base_url: "https://api.anthropic.com".into(),
        }
    }
}

#[async_trait::async_trait]
impl LLMProvider for AnthropicProvider {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn chat(&self, req: ChatRequest) -> Result<String> {
        // Anthropic takes the system prompt out-of-band.
        let system: String = req
            .messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        let messages: Vec<_> = req
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| {
                json!({
                    "role": if m.role == Role::Assistant { "assistant" } else { "user" },
                    "content": m.content,
                })
            })
            .collect();

        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens.unwrap_or(4096),
            "messages": messages,
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }

        let client = reqwest::Client::new();
        let resp = client
            .post(format!(
                "{}/v1/messages",
                self.base_url.trim_end_matches('/')
            ))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(LlmError::Auth);
        }
        if !status.is_success() {
            return Err(LlmError::Provider(format!(
                "{status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        parse_messages_response(&text)
    }

    async fn test_connection(&self) -> Result<()> {
        self.chat(ChatRequest {
            messages: vec![crate::ChatMessage::user("Reply with the single word: ok")],
            temperature: Some(0.0),
            max_tokens: Some(5),
        })
        .await
        .map(|_| ())
    }
}

pub fn parse_messages_response(json_text: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(json_text)
        .map_err(|e| LlmError::Provider(format!("bad JSON from provider: {e}")))?;
    let content = v
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| LlmError::Provider("response had no content array".into()))?;
    let text: String = content
        .iter()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
        .map(|block| block.get("text").and_then(|t| t.as_str()).unwrap_or(""))
        .collect();
    if text.is_empty() {
        return Err(LlmError::Provider("response had no text blocks".into()));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_blocks() {
        let json =
            r#"{"content":[{"type":"text","text":"hello "},{"type":"text","text":"world"}]}"#;
        assert_eq!(parse_messages_response(json).unwrap(), "hello world");
    }
}
