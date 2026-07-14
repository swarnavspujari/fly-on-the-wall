//! OpenAI-compatible chat provider — one implementation covers OpenAI,
//! NVIDIA NIM, and local Ollama.
//!
//! OpenAI and NIM speak `POST {base}/v1/chat/completions`. Ollama instead
//! uses its NATIVE `POST {root}/api/chat`: the OpenAI-compat endpoint has no
//! way to control "thinking", so thinking models (qwen3.5, deepseek-r1, …)
//! burn the whole token budget on a `reasoning` field the compat response
//! parser never sees and return EMPTY content (measured in
//! docs/BENCHMARKS.md). The native endpoint accepts `"think": false`
//! (`ThinkingMode::Disabled`); `ThinkingMode::Default` omits the field so
//! every model keeps its own default. Non-thinking models on current Ollama
//! ignore `think: false`; older servers that reject it get one retry with
//! the field stripped.

use serde_json::json;

use crate::{ChatRequest, LLMProvider, LlmError, Result, Role, ThinkingMode};

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

    fn model(&self) -> &str {
        &self.model
    }

    fn is_local(&self) -> bool {
        self.local
    }

    async fn chat(&self, req: ChatRequest) -> Result<String> {
        if self.provider_id == "ollama" {
            return self.chat_ollama_native(&req).await;
        }
        let body = openai_chat_body(&self.model, &req);

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
        let resp = http
            .send()
            .await
            .map_err(|e| crate::transport_error(self.provider_id, self.local, &self.base_url, e))?;
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
            thinking: crate::ThinkingMode::Default,
            format: None,
        })
        .await
        .map(|_| ())
    }
}

impl OpenAiCompatProvider {
    /// The Ollama server root (no `/v1`). Settings may store either form —
    /// same normalization the app's ollama manager applies.
    fn ollama_root(&self) -> String {
        let u = self.base_url.trim().trim_end_matches('/');
        let u = u.strip_suffix("/v1").unwrap_or(u);
        u.trim_end_matches('/').to_string()
    }

    /// Ollama native `POST {root}/api/chat`. Retries once without `think`
    /// when an older server rejects the field (they answer 400 mentioning
    /// thinking support), and once without `num_ctx` when a raised context
    /// crashes the model runner (some models 500 on constrained hardware —
    /// gemma4:e4b at num_ctx 8192 on 4 GB VRAM kills llama-server outright).
    async fn chat_ollama_native(&self, req: &ChatRequest) -> Result<String> {
        let root = self.ollama_root();
        let client = reqwest::Client::new();
        let mut include_think = req.thinking == ThinkingMode::Disabled;
        let mut num_ctx = Some(estimate_num_ctx(req));
        loop {
            let body = native_chat_body(&self.model, req, include_think, num_ctx);
            let resp = client
                .post(format!("{root}/api/chat"))
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    crate::transport_error(self.provider_id, self.local, &self.base_url, e)
                })?;
            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| LlmError::Network(e.to_string()))?;
            if !status.is_success() {
                if include_think && status.as_u16() == 400 && text.to_lowercase().contains("think")
                {
                    // Older server / non-thinking model that rejects the
                    // field instead of ignoring it: drop `think` and retry.
                    include_think = false;
                    continue;
                }
                if num_ctx.is_some() && status.as_u16() >= 500 {
                    // Runner crash (observed: 0xc0000409 loading big models
                    // with a raised context on small VRAM). Retry with the
                    // server's default context — a possibly-truncated answer
                    // beats no answer; the truncation risk is the status quo.
                    tracing::warn!(
                        model = %self.model,
                        "ollama runner failed with num_ctx set; retrying with server default"
                    );
                    num_ctx = None;
                    continue;
                }
                return Err(LlmError::Provider(format!(
                    "{status}: {}",
                    text.chars().take(300).collect::<String>()
                )));
            }
            return parse_native_chat(&text);
        }
    }
}

/// Context window to request for this call. Ollama's server default can be
/// as low as ~2048 tokens and overflow is trimmed SILENTLY from the front —
/// killing the system prompt first (docs/BENCHMARKS.md §8.1). Estimate the
/// prompt at chars/3 (≈33 % over the ~4 chars/token English average, so the
/// margin absorbs tokenizer variance), add the response budget, and clamp:
/// floor 4096 keeps short chats on a known-good size, cap 32768 bounds the
/// KV-cache allocation on small machines.
fn estimate_num_ctx(req: &ChatRequest) -> u32 {
    let prompt_chars: usize = req.messages.iter().map(|m| m.content.len()).sum();
    let prompt_tokens = (prompt_chars / 3) as u32;
    let response = req.max_tokens.unwrap_or(4096);
    (prompt_tokens + response + 512).clamp(4096, 32768)
}

/// Request body for the OpenAI-compatible `/chat/completions` endpoint
/// (OpenAI, NIM). `thinking` has no representation here and is ignored.
fn openai_chat_body(model: &str, req: &ChatRequest) -> serde_json::Value {
    let messages: Vec<_> = req
        .messages
        .iter()
        .map(|m| json!({"role": role_str(m.role), "content": m.content}))
        .collect();
    let mut body = json!({
        "model": model,
        "messages": messages,
    });
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(mt) = req.max_tokens {
        body["max_tokens"] = json!(mt);
    }
    body
}

/// Request body for Ollama's native `/api/chat`. `max_tokens` maps to
/// `options.num_predict`, `temperature` to `options.temperature`,
/// `num_ctx` sizes the context window (see `estimate_num_ctx`);
/// `include_think` adds `"think": false` (we never ask for MORE thinking —
/// `ThinkingMode::Default` leaves the model's own default in place);
/// `req.format` passes a JSON schema for grammar-constrained decoding.
fn native_chat_body(
    model: &str,
    req: &ChatRequest,
    include_think: bool,
    num_ctx: Option<u32>,
) -> serde_json::Value {
    let messages: Vec<_> = req
        .messages
        .iter()
        .map(|m| json!({"role": role_str(m.role), "content": m.content}))
        .collect();
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });
    if include_think {
        body["think"] = json!(false);
    }
    if let Some(schema) = &req.format {
        body["format"] = schema.clone();
    }
    let mut options = serde_json::Map::new();
    if let Some(t) = req.temperature {
        options.insert("temperature".into(), json!(t));
    }
    if let Some(mt) = req.max_tokens {
        options.insert("num_predict".into(), json!(mt));
    }
    if let Some(ctx) = num_ctx {
        options.insert("num_ctx".into(), json!(ctx));
    }
    if !options.is_empty() {
        body["options"] = serde_json::Value::Object(options);
    }
    body
}

/// Parse Ollama's native chat response: the answer is `message.content`
/// (`message.thinking` carries any reasoning trace and is ignored).
pub fn parse_native_chat(json_text: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(json_text)
        .map_err(|e| LlmError::Provider(format!("bad JSON from provider: {e}")))?;
    v.pointer("/message/content")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| LlmError::Provider("response had no message content".into()))
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
    use crate::ChatMessage;

    #[test]
    fn parses_chat_completion_content() {
        let json = r#"{"choices":[{"message":{"role":"assistant","content":"Notes:\n- ok"}}]}"#;
        assert_eq!(parse_chat_completion(json).unwrap(), "Notes:\n- ok");
    }

    #[test]
    fn missing_content_is_provider_error() {
        assert!(parse_chat_completion(r#"{"choices":[]}"#).is_err());
    }

    fn req(thinking: ThinkingMode) -> ChatRequest {
        ChatRequest {
            messages: vec![
                ChatMessage::system("sys"),
                ChatMessage::user("hi"),
                ChatMessage::assistant("yo"),
            ],
            temperature: Some(0.2),
            max_tokens: Some(4096),
            thinking,
            format: None,
        }
    }

    #[test]
    fn native_body_maps_options_and_disables_thinking() {
        let body = native_chat_body("qwen3.5:4b", &req(ThinkingMode::Disabled), true, Some(4096));
        assert_eq!(body["model"], "qwen3.5:4b");
        assert_eq!(body["stream"], false);
        assert_eq!(body["think"], false);
        let temp = body["options"]["temperature"].as_f64().unwrap();
        assert!((temp - 0.2).abs() < 1e-6, "temperature was {temp}");
        assert_eq!(body["options"]["num_predict"], 4096);
        assert_eq!(body["options"]["num_ctx"], 4096);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert_eq!(body["messages"][2]["role"], "assistant");
    }

    #[test]
    fn native_body_default_thinking_omits_think_field() {
        // ThinkingMode::Default is a no-op: the model keeps its own default.
        let body = native_chat_body("llama3.1", &req(ThinkingMode::Default), false, None);
        assert!(body.get("think").is_none());
        assert!(body["options"].get("num_ctx").is_none());
    }

    #[test]
    fn native_body_omits_empty_options() {
        let mut r = req(ThinkingMode::Default);
        r.temperature = None;
        r.max_tokens = None;
        let body = native_chat_body("llama3.1", &r, false, None);
        assert!(body.get("options").is_none());
    }

    #[test]
    fn native_body_passes_format_schema_through() {
        let mut r = req(ThinkingMode::Default);
        r.format = Some(json!({"type": "array"}));
        let body = native_chat_body("llama3.1", &r, false, None);
        assert_eq!(body["format"]["type"], "array");
        // And absent by default.
        let body = native_chat_body("llama3.1", &req(ThinkingMode::Default), false, None);
        assert!(body.get("format").is_none());
    }

    #[test]
    fn num_ctx_estimate_covers_prompt_plus_response_and_clamps() {
        // Short prompt + small response budget → floor.
        let mut r = req(ThinkingMode::Default);
        r.max_tokens = Some(100);
        assert_eq!(estimate_num_ctx(&r), 4096);
        // Prompt + response budget always fit: ~30k chars ≈ 10k estimated
        // prompt tokens + 4096 response + margin.
        let mut r = req(ThinkingMode::Default);
        r.messages = vec![ChatMessage::system("x".repeat(30_000))];
        let est = estimate_num_ctx(&r);
        assert!(est > 14_000 && est < 16_000, "est was {est}");
        // Enormous prompt → cap.
        r.messages = vec![ChatMessage::system("x".repeat(400_000))];
        assert_eq!(estimate_num_ctx(&r), 32_768);
    }

    #[test]
    fn openai_body_keeps_compat_shape_and_ignores_thinking() {
        let body = openai_chat_body("gpt-4o-mini", &req(ThinkingMode::Disabled));
        assert_eq!(body["max_tokens"], 4096);
        let temp = body["temperature"].as_f64().unwrap();
        assert!((temp - 0.2).abs() < 1e-6, "temperature was {temp}");
        assert!(body.get("think").is_none());
        assert!(body.get("stream").is_none());
        assert!(body.get("options").is_none());
    }

    #[test]
    fn parses_native_chat_content_and_ignores_thinking_trace() {
        let json = r#"{"message":{"role":"assistant","thinking":"...","content":"[{\"k\":1}]"},"done":true}"#;
        assert_eq!(parse_native_chat(json).unwrap(), r#"[{"k":1}]"#);
        assert!(parse_native_chat(r#"{"done":true}"#).is_err());
    }

    /// Live marker test for the num_ctx fix: a fact at the FRONT of a ~6k-token
    /// prompt must survive (Ollama trims overflow from the front silently;
    /// before the fix this prompt truncated to ~2048 tokens and the model
    /// hallucinated the answer). Needs a local Ollama with llama3.1.
    #[test]
    #[ignore = "needs a running Ollama with llama3.1"]
    fn ollama_num_ctx_marker_live() {
        let filler = "Sam: The quarterly infrastructure review covered database sharding, \
                      cache invalidation strategy, and the migration timeline.\n"
            .repeat(220); // ≈ 6k tokens
        let system = format!(
            "The secret code is FLYWHEEL-7391. Remember it. You are a meeting assistant; \
             answer questions about the transcript below.\n\nTRANSCRIPT:\n{filler}"
        );
        let provider = OpenAiCompatProvider::ollama(None, "llama3.1".into());
        let out = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(provider.chat(ChatRequest {
                messages: vec![
                    ChatMessage::system(system),
                    ChatMessage::user(
                        "What is the secret code stated at the very beginning of your \
                         instructions? Reply with the code only.",
                    ),
                ],
                temperature: Some(0.0),
                max_tokens: Some(30),
                thinking: ThinkingMode::Default,
                format: None,
            }))
            .expect("chat succeeds");
        assert!(
            out.contains("FLYWHEEL-7391"),
            "front of prompt was truncated — num_ctx sizing failed; got: {out:?}"
        );
    }

    #[test]
    fn ollama_root_strips_v1_suffix() {
        for (base, want) in [
            ("http://localhost:11434/v1", "http://localhost:11434"),
            ("http://localhost:11434/v1/", "http://localhost:11434"),
            ("http://localhost:11434", "http://localhost:11434"),
            ("http://box:8080/v1", "http://box:8080"),
        ] {
            let p = OpenAiCompatProvider::ollama(Some(base.into()), "m".into());
            assert_eq!(p.ollama_root(), want, "base: {base}");
        }
    }
}
