//! Anthropic Claude provider (`POST /v1/messages`).

use serde_json::json;

use crate::{ChatRequest, LLMProvider, LlmError, Result, Role};

pub const ANTHROPIC_DEFAULT_MODEL: &str = "claude-sonnet-5";

/// Model-name prefixes known to accept the `temperature` sampling parameter.
///
/// Current frontier models (Sonnet 5, Opus 4.8/4.7, Fable/Mythos 5) REMOVED
/// sampling params: sending `temperature` returns HTTP 400 ("`temperature` is
/// deprecated for this model"). Older models still accept it. Sampling params
/// are being dropped going forward, so we allow `temperature` only for this
/// fixed set of older families and OMIT it for everything else — including any
/// future model. Omitting is always safe (the model uses its own default);
/// sending to a rejecting model is a hard 400. None of these prefixes is a
/// prefix of a rejecting id (e.g. `claude-sonnet-4` never matches
/// `claude-sonnet-5`; `claude-opus-4-5/6` never match `-7/-8`).
const TEMPERATURE_ACCEPTING_PREFIXES: &[&str] = &[
    "claude-opus-4-0",
    "claude-opus-4-1",
    "claude-opus-4-5",
    "claude-opus-4-6",
    "claude-sonnet-4", // sonnet-4, 4-0, 4-5, 4-6 (sonnet-5 is a different prefix)
    "claude-haiku-4-5",
    "claude-haiku-3",
    "claude-3",
    "claude-2",
];

/// Whether `model` accepts an explicit `temperature`. See
/// `TEMPERATURE_ACCEPTING_PREFIXES` — allowlist by design so unrecognized and
/// future models default to omitting the param (safe) rather than 400-ing.
fn model_accepts_temperature(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    TEMPERATURE_ACCEPTING_PREFIXES
        .iter()
        .any(|p| m.starts_with(p))
}

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

    /// Build the `/v1/messages` request body. Split out from `chat` so the
    /// parameter shaping (system out-of-band, optional temperature, thinking
    /// off) is unit-testable without a network round-trip.
    fn request_body(&self, req: &ChatRequest) -> serde_json::Value {
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
            // Cache the (stable) system prefix so a request doesn't re-bill the
            // whole system prompt every call. Prompt caching is Anthropic-
            // specific and GA (no beta header); the breakpoint goes on `system`
            // — the block identical across a session — with the varying user
            // turn after it. Below the model's ~1024-token minimum it is a
            // silent no-op (true for the small enhance/cleanup prompts), so it
            // only ever helps: chiefly `ask_meeting`, whose system carries the
            // full transcript and is re-sent on every chat turn.
            body["system"] = json!([{
                "type": "text",
                "text": system,
                "cache_control": { "type": "ephemeral" },
            }]);
        }
        // Only send `temperature` to models that accept it; current frontier
        // models (claude-sonnet-5, opus-4-8, …) reject it with a 400.
        if let Some(t) = req.temperature {
            if model_accepts_temperature(&self.model) {
                body["temperature"] = json!(t);
            }
        }
        // Current models (claude-sonnet-5, opus-4-8, …) run adaptive thinking by
        // default; for mechanical transforms that budget is wasted and can
        // truncate the answer, so let callers turn it off. `disabled` is
        // accepted on these models (it is NOT on Fable 5, which we don't target).
        if matches!(req.thinking, crate::ThinkingMode::Disabled) {
            body["thinking"] = json!({ "type": "disabled" });
        }
        body
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
        let body = self.request_body(&req);

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
            .map_err(|e| crate::transport_error("anthropic", false, &self.base_url, e))?;
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
            thinking: crate::ThinkingMode::Default,
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

    fn req(temp: Option<f32>, thinking: crate::ThinkingMode) -> ChatRequest {
        ChatRequest {
            messages: vec![
                crate::ChatMessage::system("be terse"),
                crate::ChatMessage::user("hi"),
            ],
            temperature: temp,
            max_tokens: Some(1000),
            thinking,
        }
    }

    #[test]
    fn request_body_omits_temperature_for_rejecting_models() {
        // claude-sonnet-5 (like opus-4-8 / fable-5) rejects `temperature` with a
        // 400, so it must be dropped from the body even when the caller asked
        // for one. This is what unbreaks enhance_note / ask_meeting.
        let p = AnthropicProvider::new("k".into(), "claude-sonnet-5".into());
        let body = p.request_body(&req(Some(0.2), crate::ThinkingMode::Default));
        assert!(
            body.get("temperature").is_none(),
            "temperature must be omitted for claude-sonnet-5"
        );
        // system out-of-band as a cacheable text block; only the user turn in messages.
        assert_eq!(body["system"][0]["text"], "be terse");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        // opus-4-8 rejects it too.
        let p8 = AnthropicProvider::new("k".into(), "claude-opus-4-8".into());
        assert!(p8
            .request_body(&req(Some(0.3), crate::ThinkingMode::Default))
            .get("temperature")
            .is_none());
    }

    #[test]
    fn request_body_keeps_temperature_for_accepting_models() {
        // Older models still accept `temperature` — enhance/ask keep their
        // intended value there.
        for model in ["claude-sonnet-4-6", "claude-haiku-4-5", "claude-opus-4-6"] {
            let p = AnthropicProvider::new("k".into(), model.into());
            let body = p.request_body(&req(Some(0.2), crate::ThinkingMode::Default));
            assert!(
                (body["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6,
                "temperature should be sent for {model}"
            );
        }
    }

    #[test]
    fn model_temperature_predicate_has_no_prefix_collisions() {
        for m in ["claude-sonnet-4", "claude-sonnet-4-6", "claude-opus-4-5", "claude-opus-4-6", "claude-haiku-4-5", "claude-3-5-haiku"] {
            assert!(model_accepts_temperature(m), "{m} should accept temperature");
        }
        for m in ["claude-sonnet-5", "claude-opus-4-7", "claude-opus-4-8", "claude-fable-5", "claude-mythos-5"] {
            assert!(!model_accepts_temperature(m), "{m} must NOT accept temperature");
        }
    }

    #[test]
    fn request_body_thinking_disabled_frees_budget() {
        // Disabled thinking → `thinking: {type: disabled}` so the full token
        // budget goes to the answer (the transcript-cleanup pass relies on this
        // to avoid claude-sonnet-5 truncating its JSON mid-array). No temperature.
        let p = AnthropicProvider::new("k".into(), "claude-sonnet-5".into());
        let body = p.request_body(&ChatRequest {
            messages: vec![crate::ChatMessage::user("hi")],
            temperature: None,
            max_tokens: Some(8192),
            thinking: crate::ThinkingMode::Disabled,
        });
        assert_eq!(body["thinking"], json!({ "type": "disabled" }));
        assert!(body.get("temperature").is_none());
    }
}
