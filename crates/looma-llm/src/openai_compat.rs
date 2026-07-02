//! OpenAI-compatible chat provider — one implementation covers OpenAI,
//! NVIDIA NIM, and local Ollama (all speak `POST /chat/completions`).

use serde_json::json;

use crate::{ChatRequest, LLMProvider, LlmError, Result, Role};

pub struct OpenAiCompatProvider {
    /// "openai" | "nim" | "ollama"
    pub provider_id: &'static str,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub local: bool,
}

impl OpenAiCompatProvider {
    pub fn openai(api_key: String, model: String) -> Self {
        Self {
            provider_id: "openai",
            base_url: "https://api.openai.com/v1".into(),
            api_key: Some(api_key),
            model,
            local: false,
        }
    }

    pub fn nim(api_key: String, model: String) -> Self {
        Self {
            provider_id: "nim",
            base_url: "https://integrate.api.nvidia.com/v1".into(),
            api_key: Some(api_key),
            model,
            local: false,
        }
    }

    pub fn ollama(base_url: Option<String>, model: String) -> Self {
        Self {
            provider_id: "ollama",
            base_url: base_url.unwrap_or_else(|| "http://localhost:11434/v1".into()),
            api_key: None,
            model,
            local: true,
        }
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

#[async_trait::async_trait]
impl LLMProvider for OpenAiCompatProvider {
    fn id(&self) -> &'static str {
        self.provider_id
    }

    fn is_local(&self) -> bool {
        self.local
    }

    async fn chat(&self, req: ChatRequest) -> Result<String> {
        let messages: Vec<_> = req
            .messages
            .iter()
            .map(|m| json!({"role": role_str(m.role), "content": m.content}))
            .collect();
        let mut body = json!({
            "model": self.model,
            "messages": messages,
        });
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(mt) = req.max_tokens {
            body["max_tokens"] = json!(mt);
        }

        let client = reqwest::Client::new();
        let mut http = client
            .post(format!(
                "{}/chat/completions",
                self.base_url.trim_end_matches('/')
            ))
            .json(&body);
        if let Some(key) = &self.api_key {
            http = http.bearer_auth(key);
        }
        let resp = http.send().await.map_err(|e| {
            crate::transport_error(self.provider_id, self.local, &self.base_url, e)
        })?;
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
        parse_chat_completion(&text)
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

pub fn parse_chat_completion(json_text: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(json_text)
        .map_err(|e| LlmError::Provider(format!("bad JSON from provider: {e}")))?;
    v.pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| LlmError::Provider("response had no message content".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_completion_content() {
        let json = r#"{"choices":[{"message":{"role":"assistant","content":"Notes:\n- ok"}}]}"#;
        assert_eq!(parse_chat_completion(json).unwrap(), "Notes:\n- ok");
    }

    #[test]
    fn missing_content_is_provider_error() {
        assert!(parse_chat_completion(r#"{"choices":[]}"#).is_err());
    }
}
