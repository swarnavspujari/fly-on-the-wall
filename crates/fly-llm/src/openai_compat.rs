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
//!
//! Tool calling (`converse` / `chat_stream`):
//! * **OpenAI / NIM** — `tools: [{type: function, function: {…}}]`; calls
//!   come back as `tool_calls[{id, function: {name, arguments: "<json>"}}]`
//!   and results go back as `{role: tool, tool_call_id, content}` — matched
//!   by call id. `strict` is sent to OpenAI only (NIM/compatible servers
//!   vary). The Responses API is not used: NIM and compatible servers don't
//!   speak it, and the legacy request path must not change.
//! * **Ollama** — the same `tools` shape on `/api/chat`, arguments as a JSON
//!   object, results as `{role: tool, content, tool_name}`. There is no
//!   `tool_choice` (`None` omits the tools; `Required` is best effort).
//!   `think` is always sent explicitly when tools are present (omitting it
//!   turns thinking on, ollama#10976), and `format` + `tools` together is
//!   refused (it breaks tool calling, ollama#18944).

use serde_json::{json, Value};

use crate::conversation::{
    result_text, strict_schema, tool_name_for, Capabilities, ChatResponse, ContentBlock,
    ConverseRequest, Message, StopReason, StreamEvent, ToolChoice, Usage,
};
use crate::stream::{decode_body, parse_tool_args, sse_data, LineDecoder};
use crate::{ChatRequest, ChatStream, LLMProvider, LlmError, Result, Role, ThinkingMode};

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

    fn is_ollama(&self) -> bool {
        self.provider_id == "ollama"
    }
}

/// Client for the tool / streaming / probe paths (the legacy `chat()` path
/// keeps its own unbounded client, unchanged). Bounded connect, and an idle
/// read timeout rather than a total one: a stream may legitimately run for
/// minutes, but a server that goes silent for 10 minutes is gone. Ten
/// minutes covers a slow CPU model composing a non-streamed answer.
fn tool_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .read_timeout(std::time::Duration::from_secs(600))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
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
        if self.is_ollama() {
            return self.chat_ollama_native(&req).await;
        }
        let body = openai_chat_body(&self.model, &req);
        let text = self.openai_post(&reqwest::Client::new(), &body).await?;
        parse_chat_completion(&text)
    }

    async fn test_connection(&self) -> Result<()> {
        self.chat(ChatRequest {
            effort: None,
            messages: vec![crate::ChatMessage::user("Reply with the single word: ok")],
            temperature: Some(0.0),
            max_tokens: Some(5),
            thinking: crate::ThinkingMode::Default,
            format: None,
        })
        .await
        .map(|_| ())
    }

    fn capabilities(&self) -> Capabilities {
        match self.provider_id {
            // Unknown until probed: a model template may lack tool support.
            "ollama" => Capabilities {
                tools: false,
                tool_choice_none: false,
                tool_choice_required: false,
                tool_choice_required_thinking_off: false,
                strict_tools: false,
                max_strict_tools: 0,
                streaming: true,
                thinking_control: true,
                structured_output: true,
                probed: false,
            },
            "openai" => Capabilities {
                tools: true,
                tool_choice_none: true,
                tool_choice_required: true,
                tool_choice_required_thinking_off: true,
                strict_tools: true,
                max_strict_tools: 0,
                streaming: true,
                thinking_control: false,
                structured_output: false,
                probed: false,
            },
            // NIM: tool support is per model; "required" isn't universal.
            _ => Capabilities {
                tools: true,
                tool_choice_none: true,
                tool_choice_required: false,
                tool_choice_required_thinking_off: false,
                strict_tools: false,
                max_strict_tools: 0,
                streaming: true,
                thinking_control: false,
                structured_output: false,
                probed: false,
            },
        }
    }

    /// Ollama: `POST /api/show` lists the model's `capabilities`
    /// (`"tools"`, `"thinking"`, …). Others: the static table.
    async fn probe_capabilities(&self) -> Result<Capabilities> {
        if !self.is_ollama() {
            return Ok(self.capabilities());
        }
        let resp = tool_client()
            .post(format!("{}/api/show", self.ollama_root()))
            .json(&json!({ "model": self.model }))
            .send()
            .await
            .map_err(|e| crate::transport_error(self.provider_id, self.local, &self.base_url, e))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(LlmError::Provider(format!(
                "{status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        Ok(parse_ollama_show(&text, self.capabilities()))
    }

    async fn converse(&self, req: ConverseRequest) -> Result<ChatResponse> {
        if self.is_ollama() {
            validate_ollama(&req)?;
            let (resp, fallback) = self.ollama_send(tool_client(), &req, false).await?;
            let text = resp
                .text()
                .await
                .map_err(|e| LlmError::Network(e.to_string()))?;
            let mut r = parse_native_response(&text)?;
            r.usage.context_fallback |= fallback;
            return Ok(r);
        }
        let body = openai_body(&self.model, &req, false, &self.capabilities());
        let text = self.openai_post(tool_client(), &body).await?;
        parse_openai_response(&text)
    }

    async fn chat_stream(&self, req: ConverseRequest) -> Result<ChatStream> {
        if self.is_ollama() {
            validate_ollama(&req)?;
            let (resp, fallback) = self.ollama_send(tool_client(), &req, true).await?;
            let stream = decode_body(resp.bytes_stream(), OllamaNdjson::default());
            if !fallback {
                return Ok(stream);
            }
            // Say so on the final event, where the usage travels.
            return Ok(Box::pin(futures_util::StreamExt::map(stream, |ev| {
                ev.map(|ev| match ev {
                    StreamEvent::Done {
                        stop_reason,
                        mut usage,
                    } => {
                        usage.context_fallback = true;
                        StreamEvent::Done { stop_reason, usage }
                    }
                    other => other,
                })
            })));
        }
        let mut body = openai_body(&self.model, &req, true, &self.capabilities());
        if self.provider_id == "openai" {
            body["stream_options"] = json!({ "include_usage": true });
        }
        let resp = self.openai_send(tool_client(), &body).await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp
                .text()
                .await
                .map_err(|e| LlmError::Network(e.to_string()))?;
            return Err(http_error(status, &text));
        }
        Ok(decode_body(resp.bytes_stream(), OpenAiSse::default()))
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

    async fn openai_send(
        &self,
        client: &reqwest::Client,
        body: &Value,
    ) -> Result<reqwest::Response> {
        let mut http = client
            .post(format!(
                "{}/chat/completions",
                self.base_url.trim_end_matches('/')
            ))
            .json(body);
        if let Some(key) = &self.api_key {
            http = http.bearer_auth(key);
        }
        http.send()
            .await
            .map_err(|e| crate::transport_error(self.provider_id, self.local, &self.base_url, e))
    }

    /// POST a non-streaming chat-completions body; the response text on
    /// success.
    async fn openai_post(&self, client: &reqwest::Client, body: &Value) -> Result<String> {
        let resp = self.openai_send(client, body).await?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(http_error(status, &text));
        }
        Ok(text)
    }

    /// Ollama native `POST {root}/api/chat` for the legacy text path.
    async fn chat_ollama_native(&self, req: &ChatRequest) -> Result<String> {
        let (resp, _) = self
            .ollama_send(&reqwest::Client::new(), &ConverseRequest::from(req), false)
            .await?;
        let text = resp
            .text()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        parse_native_chat(&text)
    }

    /// Send to `/api/chat`, returning the successful response. Retries once
    /// without `think` when an older server rejects the field (they answer
    /// 400 mentioning thinking support), and once without `num_ctx` when a
    /// raised context crashes the model runner (some models 500 on
    /// constrained hardware — gemma4:e4b at num_ctx 8192 on 4 GB VRAM kills
    /// llama-server outright). The flag says the second retry happened: the
    /// reply came from a smaller context than the prompt was sized for
    /// (`Usage::context_fallback`).
    async fn ollama_send(
        &self,
        client: &reqwest::Client,
        req: &ConverseRequest,
        stream: bool,
    ) -> Result<(reqwest::Response, bool)> {
        let root = self.ollama_root();
        let mut think = ollama_think(req);
        let mut num_ctx = Some(estimate_num_ctx_for(req));
        loop {
            let body = ollama_body(&self.model, req, think, num_ctx, stream);
            let resp = client
                .post(format!("{root}/api/chat"))
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    crate::transport_error(self.provider_id, self.local, &self.base_url, e)
                })?;
            let status = resp.status();
            if status.is_success() {
                return Ok((resp, num_ctx.is_none()));
            }
            let text = resp
                .text()
                .await
                .map_err(|e| LlmError::Network(e.to_string()))?;
            if think.is_some() && status.as_u16() == 400 && text.to_lowercase().contains("think") {
                // Older server / non-thinking model that rejects the field
                // instead of ignoring it: drop `think` and retry.
                think = None;
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
    }
}

fn http_error(status: reqwest::StatusCode, text: &str) -> LlmError {
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return LlmError::Auth;
    }
    LlmError::Provider(format!(
        "{status}: {}",
        text.chars().take(300).collect::<String>()
    ))
}

/// Refuse request shapes Ollama can't serve correctly.
fn validate_ollama(req: &ConverseRequest) -> Result<()> {
    if req.has_tools() && req.format.is_some() {
        return Err(LlmError::InvalidRequest(
            "Ollama can't combine a JSON `format` with tools (it breaks tool calling, \
             ollama#18944) — send one or the other"
                .into(),
        ));
    }
    Ok(())
}

/// The `think` field: always explicit when tools are present (omitting it
/// there turns thinking ON, ollama#10976 — FOTW's tool loops run with it
/// off unless the caller asks); otherwise the legacy rule (`Disabled` →
/// `false`, `Default` → omitted).
fn ollama_think(req: &ConverseRequest) -> Option<bool> {
    if req.has_tools() {
        return Some(req.thinking == ThinkingMode::Enabled);
    }
    match req.thinking {
        ThinkingMode::Disabled => Some(false),
        ThinkingMode::Enabled => Some(true),
        ThinkingMode::Default => None,
    }
}

/// Context window to request for this call. Ollama's server default can be
/// as low as ~2048 tokens and overflow is trimmed SILENTLY from the front —
/// killing the system prompt first (docs/BENCHMARKS.md §8.1). Estimate the
/// prompt at chars/3 (≈33 % over the ~4 chars/token English average, so the
/// margin absorbs tokenizer variance), add the response budget, and clamp:
/// floor 4096 keeps short chats on a known-good size, cap 32768 bounds the
/// KV-cache allocation on small machines.
#[cfg(test)]
pub(crate) fn estimate_num_ctx(req: &ChatRequest) -> u32 {
    estimate_num_ctx_for(&ConverseRequest::from(req))
}

/// [`estimate_num_ctx`] over blocks: text, tool calls/results and the tool
/// schemas all occupy the prompt.
fn estimate_num_ctx_for(req: &ConverseRequest) -> u32 {
    let block_chars = |b: &ContentBlock| match b {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
        ContentBlock::ToolResult { content, .. } => content.len(),
        ContentBlock::Opaque { .. } => 0,
    };
    let mut prompt_chars: usize = req
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .map(block_chars)
        .sum();
    prompt_chars += req
        .tools
        .iter()
        .map(|t| t.name.len() + t.description.len() + t.input_schema.to_string().len())
        .sum::<usize>();
    let prompt_tokens = (prompt_chars / 3) as u32;
    let response = req.max_tokens.unwrap_or(4096);
    (prompt_tokens + response + 512).clamp(4096, 32768)
}

/// Request body for the OpenAI-compatible `/chat/completions` endpoint
/// (OpenAI, NIM). `thinking` has no representation here and is ignored.
pub(crate) fn openai_chat_body(model: &str, req: &ChatRequest) -> Value {
    openai_body(
        model,
        &ConverseRequest::from(req),
        false,
        &Capabilities::default(),
    )
}

/// Chat-completions body for a block request. Text-only messages keep the
/// legacy `{role, content}` shape exactly. `caps` decides what tool
/// features may be sent: `strict` only where `strict_tools` (with the
/// OpenAI strict schema form), `tool_choice: "required"` only where
/// `tool_choice_required` (else it degrades to auto, as documented on
/// `ToolChoice::Required`).
fn openai_body(model: &str, req: &ConverseRequest, stream: bool, caps: &Capabilities) -> Value {
    let strict_ok = caps.strict_tools;
    let mut body = json!({
        "model": model,
        "messages": openai_messages(&req.messages),
    });
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(mt) = req.max_tokens {
        body["max_tokens"] = json!(mt);
    }
    if req.has_tools() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut f = json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": if t.strict && strict_ok {
                        strict_schema(&t.input_schema)
                    } else {
                        t.input_schema.clone()
                    },
                });
                if t.strict && strict_ok {
                    f["strict"] = json!(true);
                }
                json!({ "type": "function", "function": f })
            })
            .collect();
        body["tools"] = json!(tools);
        match req.tool_choice {
            ToolChoice::Auto => {}
            ToolChoice::None => body["tool_choice"] = json!("none"),
            ToolChoice::Required if caps.tool_choice_required => {
                body["tool_choice"] = json!("required")
            }
            ToolChoice::Required => {}
        }
    }
    if stream {
        body["stream"] = json!(true);
    }
    body
}

fn openai_messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        if m.is_plain_text() {
            out.push(json!({"role": role_str(m.role), "content": m.text()}));
            continue;
        }
        let text = m.text();
        if m.role == Role::Assistant {
            let calls: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { id, name, input } => Some(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": input.to_string() },
                    })),
                    _ => None,
                })
                .collect();
            let mut msg = json!({
                "role": "assistant",
                "content": if text.is_empty() { Value::Null } else { json!(text) },
            });
            if !calls.is_empty() {
                msg["tool_calls"] = json!(calls);
            }
            out.push(msg);
            continue;
        }
        // Tool results must directly follow the assistant's tool_calls.
        for b in &m.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = b
            {
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": result_text(content, *is_error),
                }));
            }
        }
        if !text.is_empty() {
            out.push(json!({"role": role_str(m.role), "content": text}));
        }
    }
    out
}

/// Request body for Ollama's native `/api/chat`, legacy text path. See
/// [`ollama_body`]; `include_think` adds `"think": false` (we never ask for
/// MORE thinking — `ThinkingMode::Default` leaves the model's own default in
/// place).
#[cfg(test)]
pub(crate) fn native_chat_body(
    model: &str,
    req: &ChatRequest,
    include_think: bool,
    num_ctx: Option<u32>,
) -> Value {
    ollama_body(
        model,
        &ConverseRequest::from(req),
        include_think.then_some(false),
        num_ctx,
        false,
    )
}

/// Request body for Ollama's native `/api/chat`. `max_tokens` maps to
/// `options.num_predict`, `temperature` to `options.temperature`,
/// `num_ctx` sizes the context window (see `estimate_num_ctx`); `think` is
/// sent when `Some`; `req.format` passes a JSON schema for
/// grammar-constrained decoding (never together with tools — see
/// [`validate_ollama`]).
fn ollama_body(
    model: &str,
    req: &ConverseRequest,
    think: Option<bool>,
    num_ctx: Option<u32>,
    stream: bool,
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": ollama_messages(&req.messages),
        "stream": stream,
    });
    if let Some(t) = think {
        body["think"] = json!(t);
    }
    if let Some(schema) = &req.format {
        body["format"] = schema.clone();
    }
    // No tool_choice on Ollama: "none" can only be expressed by not
    // offering the tools.
    if req.has_tools() && req.tool_choice != ToolChoice::None {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();
        body["tools"] = json!(tools);
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
        body["options"] = Value::Object(options);
    }
    body
}

fn ollama_messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        if m.is_plain_text() {
            out.push(json!({"role": role_str(m.role), "content": m.text()}));
            continue;
        }
        let text = m.text();
        if m.role == Role::Assistant {
            let calls: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { name, input, .. } => Some(json!({
                        "function": { "name": name, "arguments": input },
                    })),
                    _ => None,
                })
                .collect();
            let mut msg = json!({"role": "assistant", "content": text});
            if !calls.is_empty() {
                msg["tool_calls"] = json!(calls);
            }
            out.push(msg);
            continue;
        }
        for b in &m.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = b
            {
                let mut msg = json!({
                    "role": "tool",
                    "content": result_text(content, *is_error),
                });
                if let Some(name) = tool_name_for(messages, i, tool_use_id) {
                    msg["tool_name"] = json!(name);
                }
                out.push(msg);
            }
        }
        if !text.is_empty() {
            out.push(json!({"role": role_str(m.role), "content": text}));
        }
    }
    out
}

/// Parse Ollama's native chat response: the answer is `message.content`
/// (`message.thinking` carries any reasoning trace and is ignored).
pub fn parse_native_chat(json_text: &str) -> Result<String> {
    let v: Value = serde_json::from_str(json_text)
        .map_err(|e| LlmError::Provider(format!("bad JSON from provider: {e}")))?;
    v.pointer("/message/content")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| LlmError::Provider("response had no message content".into()))
}

/// Ollama tool calls → blocks. Ollama has no call ids (newer servers may
/// send one); missing ids are synthesized per reply as `call_<n>`.
fn ollama_tool_uses(msg: &Value, first_index: usize) -> Vec<ContentBlock> {
    msg.get("tool_calls")
        .and_then(|c| c.as_array())
        .map(|calls| {
            calls
                .iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    let f = c.get("function")?;
                    let name = f.get("name")?.as_str()?.to_string();
                    let input = match f.get("arguments") {
                        Some(Value::String(s)) => parse_tool_args(s),
                        Some(v) => v.clone(),
                        None => json!({}),
                    };
                    let id = c
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_{}", first_index + i));
                    Some(ContentBlock::ToolUse { id, name, input })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn ollama_usage(v: &Value) -> Usage {
    Usage {
        input_tokens: v
            .get("prompt_eval_count")
            .and_then(|n| n.as_u64())
            .map(|n| n as u32),
        output_tokens: v
            .get("eval_count")
            .and_then(|n| n.as_u64())
            .map(|n| n as u32),
        ..Usage::default()
    }
}

/// Parse a non-streaming `/api/chat` reply into blocks.
pub fn parse_native_response(json_text: &str) -> Result<ChatResponse> {
    let v: Value = serde_json::from_str(json_text)
        .map_err(|e| LlmError::Provider(format!("bad JSON from provider: {e}")))?;
    let msg = v
        .get("message")
        .ok_or_else(|| LlmError::Provider("response had no message".into()))?;
    let mut blocks = Vec::new();
    if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
        if !text.is_empty() {
            blocks.push(ContentBlock::text(text));
        }
    }
    let uses = ollama_tool_uses(msg, 0);
    let has_uses = !uses.is_empty();
    blocks.extend(uses);
    let stop_reason = if v.get("done_reason").and_then(|r| r.as_str()) == Some("length") {
        StopReason::MaxTokens
    } else if has_uses {
        StopReason::ToolUse
    } else {
        StopReason::EndTurn
    };
    Ok(ChatResponse {
        blocks,
        stop_reason,
        usage: ollama_usage(&v),
    })
}

/// `/api/show` → capabilities (`"tools"`, `"thinking"` in `capabilities`).
fn parse_ollama_show(json_text: &str, base: Capabilities) -> Capabilities {
    let caps: Vec<String> = serde_json::from_str::<Value>(json_text)
        .ok()
        .and_then(|v| v.get("capabilities").cloned())
        .and_then(|c| serde_json::from_value(c).ok())
        .unwrap_or_default();
    let has = |c: &str| caps.iter().any(|x| x == c);
    Capabilities {
        tools: has("tools"),
        thinking_control: has("thinking"),
        probed: true,
        ..base
    }
}

/// Ollama streams NDJSON: one `{"message": {...}, "done": false}` per
/// token batch, then a final `done: true` line with counts. Tool calls
/// arrive whole, so each becomes a start + one full-arguments delta.
#[derive(Debug, Default)]
pub(crate) struct OllamaNdjson {
    calls: usize,
    done: bool,
}

impl LineDecoder for OllamaNdjson {
    fn line(&mut self, line: &str) -> Result<Vec<StreamEvent>> {
        if line.trim().is_empty() || self.done {
            return Ok(vec![]);
        }
        let v: Value = serde_json::from_str(line)
            .map_err(|e| LlmError::Provider(format!("bad JSON in stream: {e}")))?;
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            return Err(LlmError::Provider(err.chars().take(300).collect()));
        }
        let mut out = Vec::new();
        if let Some(msg) = v.get("message") {
            if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
                if !text.is_empty() {
                    out.push(StreamEvent::TextDelta { text: text.into() });
                }
            }
            for b in ollama_tool_uses(msg, self.calls) {
                if let ContentBlock::ToolUse { id, name, input } = b {
                    self.calls += 1;
                    out.push(StreamEvent::ToolUseStart {
                        id: id.clone(),
                        name,
                    });
                    out.push(StreamEvent::ToolInputDelta {
                        id,
                        partial_json: input.to_string(),
                    });
                }
            }
        }
        if v.get("done").and_then(|d| d.as_bool()) == Some(true) {
            self.done = true;
            let stop_reason = if v.get("done_reason").and_then(|r| r.as_str()) == Some("length") {
                StopReason::MaxTokens
            } else if self.calls > 0 {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            };
            out.push(StreamEvent::Done {
                stop_reason,
                usage: ollama_usage(&v),
            });
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>> {
        if self.done {
            Ok(vec![])
        } else {
            Err(LlmError::Network(
                "Ollama's reply stream ended before it finished".into(),
            ))
        }
    }
}

pub fn parse_chat_completion(json_text: &str) -> Result<String> {
    let v: Value = serde_json::from_str(json_text)
        .map_err(|e| LlmError::Provider(format!("bad JSON from provider: {e}")))?;
    v.pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| LlmError::Provider("response had no message content".into()))
}

fn openai_finish(reason: Option<&str>, saw_tool: bool) -> Result<StopReason> {
    Ok(match reason {
        Some("length") => StopReason::MaxTokens,
        Some("content_filter") => {
            return Err(LlmError::Provider(
                "the provider's content filter blocked this reply".into(),
            ))
        }
        Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
        _ if saw_tool => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    })
}

fn openai_usage(v: &Value) -> Usage {
    let u = |p: &str| v.pointer(p).and_then(|n| n.as_u64()).map(|n| n as u32);
    Usage {
        input_tokens: u("/usage/prompt_tokens"),
        output_tokens: u("/usage/completion_tokens"),
        cache_read_tokens: u("/usage/prompt_tokens_details/cached_tokens"),
        cache_write_tokens: None,
        context_fallback: false,
    }
}

/// Parse a non-streaming chat-completions reply into blocks.
pub fn parse_openai_response(json_text: &str) -> Result<ChatResponse> {
    let v: Value = serde_json::from_str(json_text)
        .map_err(|e| LlmError::Provider(format!("bad JSON from provider: {e}")))?;
    let choice = v
        .pointer("/choices/0")
        .ok_or_else(|| LlmError::Provider("response had no choices".into()))?;
    let msg = choice
        .get("message")
        .ok_or_else(|| LlmError::Provider("response had no message".into()))?;
    let mut blocks = Vec::new();
    if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
        if !text.is_empty() {
            blocks.push(ContentBlock::text(text));
        }
    }
    let mut saw_tool = false;
    if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
        for (i, c) in calls.iter().enumerate() {
            let Some(name) = c.pointer("/function/name").and_then(|n| n.as_str()) else {
                continue;
            };
            let input = match c.pointer("/function/arguments") {
                Some(Value::String(s)) => parse_tool_args(s),
                Some(v) => v.clone(),
                None => json!({}),
            };
            let id = c
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("call_{i}"));
            saw_tool = true;
            blocks.push(ContentBlock::ToolUse {
                id,
                name: name.into(),
                input,
            });
        }
    }
    let stop_reason = openai_finish(
        choice.get("finish_reason").and_then(|r| r.as_str()),
        saw_tool,
    )?;
    Ok(ChatResponse {
        blocks,
        stop_reason,
        usage: openai_usage(&v),
    })
}

/// Chat-completions SSE: `data: {choices:[{delta:{content, tool_calls}}]}`
/// lines, tool-call fragments keyed by `index`, then `data: [DONE]`.
#[derive(Debug, Default)]
pub(crate) struct OpenAiSse {
    /// tool-call index → call id
    ids: std::collections::HashMap<u64, String>,
    finish: Option<String>,
    usage: Usage,
    done: bool,
}

impl OpenAiSse {
    fn done_event(&mut self) -> Result<StreamEvent> {
        self.done = true;
        Ok(StreamEvent::Done {
            stop_reason: openai_finish(self.finish.as_deref(), !self.ids.is_empty())?,
            usage: self.usage,
        })
    }
}

impl LineDecoder for OpenAiSse {
    fn line(&mut self, line: &str) -> Result<Vec<StreamEvent>> {
        let Some(data) = sse_data(line) else {
            return Ok(vec![]);
        };
        if self.done {
            return Ok(vec![]);
        }
        if data.trim() == "[DONE]" {
            return Ok(vec![self.done_event()?]);
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| LlmError::Provider(format!("bad JSON in stream: {e}")))?;
        if let Some(msg) = v.pointer("/error/message").and_then(|m| m.as_str()) {
            return Err(LlmError::Provider(msg.chars().take(300).collect()));
        }
        if v.get("usage").is_some_and(|u| !u.is_null()) {
            self.usage = openai_usage(&v);
        }
        let mut out = Vec::new();
        let Some(choice) = v.pointer("/choices/0") else {
            return Ok(out);
        };
        if let Some(text) = choice.pointer("/delta/content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                out.push(StreamEvent::TextDelta { text: text.into() });
            }
        }
        if let Some(calls) = choice
            .pointer("/delta/tool_calls")
            .and_then(|c| c.as_array())
        {
            for c in calls {
                let index = c.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                let id = match self.ids.entry(index) {
                    std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        let id = c
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("call_{index}"));
                        let name = c
                            .pointer("/function/name")
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string();
                        e.insert(id.clone());
                        out.push(StreamEvent::ToolUseStart {
                            id: id.clone(),
                            name,
                        });
                        id
                    }
                };
                if let Some(args) = c.pointer("/function/arguments").and_then(|a| a.as_str()) {
                    if !args.is_empty() {
                        out.push(StreamEvent::ToolInputDelta {
                            id,
                            partial_json: args.into(),
                        });
                    }
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
            self.finish = Some(reason.to_string());
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>> {
        if self.done {
            return Ok(vec![]);
        }
        // Some compatible servers end without `[DONE]` after a finish_reason.
        if self.finish.is_some() {
            return Ok(vec![self.done_event()?]);
        }
        Err(LlmError::Network(
            "the reply stream ended before it finished".into(),
        ))
    }
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
            effort: None,
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
                effort: None,
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

    // ---- tool calling -------------------------------------------------

    use crate::conversation::{ToolChoice, ToolSpec};

    fn search_tool() -> ToolSpec {
        ToolSpec {
            name: "search_passages".into(),
            description: "Search meeting passages".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }),
            strict: true,
        }
    }

    /// A tool round trip: question → assistant tool call → tool result.
    fn tool_history() -> Vec<Message> {
        vec![
            Message::system("sys"),
            Message::user("What was the budget?"),
            Message::new(
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: "call_0".into(),
                    name: "search_passages".into(),
                    input: json!({"query": "budget"}),
                }],
            ),
            Message::tool_results(vec![ContentBlock::ToolResult {
                tool_use_id: "call_0".into(),
                content: "[seg:s1] Dana: $8,400".into(),
                is_error: false,
            }]),
        ]
    }

    fn tool_req() -> ConverseRequest {
        ConverseRequest {
            messages: tool_history(),
            tools: vec![search_tool()],
            temperature: Some(0.0),
            max_tokens: Some(1024),
            ..Default::default()
        }
    }

    #[test]
    fn ollama_tool_body_shape() {
        let req = tool_req();
        let body = ollama_body("qwen3.5:9b", &req, ollama_think(&req), Some(4096), false);
        // think is explicit with tools (#10976) and off by default.
        assert_eq!(body["think"], false);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "search_passages");
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["required"],
            json!(["query"])
        );
        assert!(
            body.get("tool_choice").is_none(),
            "Ollama has no tool_choice"
        );
        assert!(body.get("format").is_none());
        let msgs = body["messages"].as_array().unwrap();
        // Assistant tool call: arguments as an OBJECT, not a string.
        assert_eq!(
            msgs[2]["tool_calls"][0]["function"]["arguments"],
            json!({"query": "budget"})
        );
        // Result as role "tool", tied back by tool name.
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_name"], "search_passages");
        assert_eq!(msgs[3]["content"], "[seg:s1] Dana: $8,400");

        let mut on = tool_req();
        on.thinking = ThinkingMode::Enabled;
        assert_eq!(ollama_think(&on), Some(true));
        let mut none = tool_req();
        none.tool_choice = ToolChoice::None;
        let body = ollama_body("m", &none, ollama_think(&none), None, true);
        assert!(body.get("tools").is_none(), "None = don't offer tools");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn ollama_refuses_format_with_tools() {
        let mut req = tool_req();
        req.format = Some(json!({"type": "object"}));
        assert!(matches!(
            validate_ollama(&req),
            Err(LlmError::InvalidRequest(_))
        ));
        req.tools.clear();
        assert!(validate_ollama(&req).is_ok());
    }

    #[test]
    fn openai_tool_body_shape_and_strict_only_for_openai() {
        let mut req = tool_req();
        req.tool_choice = ToolChoice::Required;
        req.messages
            .push(Message::tool_results(vec![ContentBlock::ToolResult {
                tool_use_id: "call_9".into(),
                content: "boom".into(),
                is_error: true,
            }]));
        let openai = OpenAiCompatProvider::openai("k".into(), "gpt-4o-mini".into());
        let body = openai_body("gpt-4o-mini", &req, false, &openai.capabilities());
        let f = &body["tools"][0]["function"];
        assert_eq!(f["strict"], true);
        assert_eq!(f["parameters"]["additionalProperties"], false);
        // OpenAI strict: every property required, optional ones nullable.
        assert_eq!(f["parameters"]["required"], json!(["query"]));
        assert_eq!(body["tool_choice"], "required");
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[2]["role"], "assistant");
        assert!(msgs[2]["content"].is_null());
        assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_0");
        // Arguments go out as a JSON STRING on chat-completions.
        assert_eq!(
            msgs[2]["tool_calls"][0]["function"]["arguments"],
            "{\"query\":\"budget\"}"
        );
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "call_0");
        assert_eq!(msgs[4]["content"], "ERROR: boom");

        // NIM / compatible: no strict, schema untouched.
        let nim = OpenAiCompatProvider::nim("k".into(), "meta/llama-3.1-8b-instruct".into());
        let body = openai_body(
            "meta/llama-3.1-8b-instruct",
            &req,
            true,
            &nim.capabilities(),
        );
        // NIM can't honor "required": degrade to auto (omitted).
        assert!(body.get("tool_choice").is_none());
        assert!(body["tools"][0]["function"].get("strict").is_none());
        assert!(body["tools"][0]["function"]["parameters"]
            .get("additionalProperties")
            .is_none());
        assert_eq!(body["stream"], true);
        // Auto is the API default and isn't sent.
        let mut auto = tool_req();
        auto.tool_choice = ToolChoice::Auto;
        assert!(openai_body("m", &auto, false, &openai.capabilities())
            .get("tool_choice")
            .is_none());
    }

    #[test]
    fn parses_ollama_tool_calls_with_synthesized_ids() {
        let j = r#"{"message":{"role":"assistant","content":"","tool_calls":[
            {"function":{"name":"search_passages","arguments":{"query":"budget"}}},
            {"function":{"name":"read_segments","arguments":"{\"meeting_id\":\"m1\"}"}}]},
            "done":true,"done_reason":"stop","prompt_eval_count":120,"eval_count":9}"#;
        let r = parse_native_response(j).unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        let uses = r.tool_uses();
        assert_eq!(uses[0].0, "call_0");
        assert_eq!(uses[1].0, "call_1");
        assert_eq!(uses[1].2, &json!({"meeting_id": "m1"}));
        assert_eq!(r.usage.input_tokens, Some(120));
        assert_eq!(r.usage.output_tokens, Some(9));

        let cut = r#"{"message":{"role":"assistant","content":"The bud"},"done":true,"done_reason":"length"}"#;
        let r = parse_native_response(cut).unwrap();
        assert_eq!(r.stop_reason, StopReason::MaxTokens);
        assert_eq!(r.text(), "The bud");
    }

    #[test]
    fn parses_openai_tool_calls_and_filter() {
        let j = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
            {"id":"call_abc","type":"function","function":{"name":"search_passages","arguments":"{\"query\":\"x\"}"}}]},
            "finish_reason":"tool_calls"}],"usage":{"prompt_tokens":50,"completion_tokens":7}}"#;
        let r = parse_openai_response(j).unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.tool_uses()[0].0, "call_abc");
        assert_eq!(r.tool_uses()[0].2, &json!({"query": "x"}));
        assert_eq!(r.usage.input_tokens, Some(50));
        let filtered =
            r#"{"choices":[{"message":{"content":""},"finish_reason":"content_filter"}]}"#;
        assert!(parse_openai_response(filtered).is_err());
    }

    fn decode_all<D: LineDecoder>(mut d: D, text: &str) -> Result<ChatResponse> {
        let mut acc = crate::stream::Accumulator::default();
        for line in text.lines() {
            for ev in d.line(line)? {
                acc.push(ev);
            }
        }
        for ev in d.finish()? {
            acc.push(ev);
        }
        acc.finish()
    }

    #[test]
    fn ollama_ndjson_stream_decodes_text_and_tools() {
        let text = r#"{"message":{"role":"assistant","content":"The "},"done":false}
{"message":{"role":"assistant","content":"budget"},"done":false}
{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"search_passages","arguments":{"query":"q"}}}]},"done":false}
{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":10,"eval_count":3}"#;
        let r = decode_all(OllamaNdjson::default(), text).unwrap();
        assert_eq!(r.text(), "The budget");
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.tool_uses()[0].2, &json!({"query": "q"}));
        assert_eq!(r.usage.output_tokens, Some(3));
        // Cut before done → error.
        let cut = r#"{"message":{"content":"The "},"done":false}"#;
        assert!(decode_all(OllamaNdjson::default(), cut).is_err());
        let err = r#"{"error":"model not found"}"#;
        assert!(decode_all(OllamaNdjson::default(), err).is_err());
    }

    #[test]
    fn openai_sse_stream_reassembles_fragmented_arguments() {
        let text = r#"data: {"choices":[{"delta":{"role":"assistant","content":"Hi"}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"search_passages","arguments":""}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"query\":"}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"budget\"}"}}]}}]}
data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}
data: {"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":4}}
data: [DONE]"#;
        let r = decode_all(OpenAiSse::default(), text).unwrap();
        assert_eq!(r.text(), "Hi");
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.tool_uses()[0].0, "call_1");
        assert_eq!(r.tool_uses()[0].2, &json!({"query": "budget"}));
        assert_eq!(r.usage.input_tokens, Some(11));
    }

    #[test]
    fn ollama_show_probe_reads_capabilities() {
        let base = OpenAiCompatProvider::ollama(None, "m".into()).capabilities();
        assert!(!base.tools && !base.probed);
        let c = parse_ollama_show(
            r#"{"capabilities":["completion","tools","thinking"]}"#,
            base,
        );
        assert!(c.tools && c.thinking_control && c.probed);
        let c = parse_ollama_show(r#"{"capabilities":["completion"]}"#, base);
        assert!(!c.tools && c.probed);
    }

    #[test]
    fn num_ctx_counts_tool_schemas_and_results() {
        let mut req = tool_req();
        assert_eq!(estimate_num_ctx_for(&req), 4096, "small prompt → floor");
        req.messages
            .push(Message::tool_results(vec![ContentBlock::ToolResult {
                tool_use_id: "x".into(),
                content: "y".repeat(30_000),
                is_error: false,
            }]));
        // 30k chars ≈ 10k tokens + 1024 response + margin.
        assert!(estimate_num_ctx_for(&req) > 11_000);
    }

    /// Live: a real tool round trip through Ollama — the model must call
    /// the tool natively (or as repairable text) and then answer from the
    /// result, both non-streaming and streaming. Needs a local Ollama with
    /// a tools-capable model (FLY_LLM_TOOL_MODEL, default qwen3.5:4b).
    #[test]
    #[ignore = "needs a running Ollama with a tools-capable model"]
    fn ollama_tool_loop_live() {
        use crate::tool_loop::{run_tool_loop, ToolLoopLimits};
        let model = std::env::var("FLY_LLM_TOOL_MODEL").unwrap_or_else(|_| "qwen3.5:4b".into());
        let provider = OpenAiCompatProvider::ollama(None, model.clone());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let caps = rt.block_on(provider.probe_capabilities()).expect("probe");
        assert!(caps.probed, "probe must reach /api/show");
        assert!(caps.tools, "{model} has no tool support per /api/show");
        let req = ConverseRequest {
            messages: vec![
                Message::system(
                    "You answer questions about the user's meetings. Always call \
                     search_passages before answering, then answer in one sentence.",
                ),
                Message::user("What budget did Dana agree to?"),
            ],
            tools: vec![search_tool()],
            temperature: Some(0.0),
            max_tokens: Some(512),
            ..Default::default()
        };
        let mut calls = 0;
        let mut exec = |name: &str, _: &serde_json::Value| -> std::result::Result<String, String> {
            calls += 1;
            assert_eq!(name, "search_passages");
            Ok("[seg:s1] Dana: We agreed on a budget of $8,400 for Q3.".into())
        };
        let out = rt
            .block_on(run_tool_loop(
                &provider,
                req.clone(),
                ToolLoopLimits {
                    max_calls: 3,
                    max_rounds: 2,
                    max_observation_chars: 2_000,
                },
                &mut exec,
            ))
            .expect("tool loop");
        eprintln!(
            "live tool loop ({model}): end={:?} rounds={} model_calls={} calls={:?} usage={:?}",
            out.end, out.rounds, out.model_calls, out.calls, out.usage
        );
        assert!(calls >= 1, "model never called the tool");
        assert!(
            out.answer.contains("8,400") || out.answer.contains("8400"),
            "{}",
            out.answer
        );

        // Streaming: first turn should come back as a tool call too.
        let resp = rt
            .block_on(async {
                let s = provider.chat_stream(req).await?;
                crate::stream::collect(s).await
            })
            .expect("stream");
        assert!(
            !resp.tool_uses().is_empty() || !resp.text().is_empty(),
            "empty streamed reply"
        );
    }

    #[test]
    fn openai_strict_schema_requires_every_property_with_nullable_optionals() {
        let mut req = tool_req();
        req.tools[0].input_schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer"},
                "kind": {"type": "string", "enum": ["a", "b"]},
                "ids": {"type": ["array"], "items": {"type": "string"}},
                "filters": {
                    "type": "object",
                    "properties": {"since": {"type": "string"}},
                    "required": []
                },
                "anything": {"description": "untyped"}
            },
            "required": ["query"]
        });
        let openai = OpenAiCompatProvider::openai("k".into(), "gpt-4o-mini".into());
        let body = openai_body("gpt-4o-mini", &req, false, &openai.capabilities());
        let p = &body["tools"][0]["function"]["parameters"];
        let mut required: Vec<&str> = p["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        required.sort();
        assert_eq!(
            required,
            ["anything", "filters", "ids", "kind", "limit", "query"]
        );
        assert_eq!(p["additionalProperties"], false);
        let props = &p["properties"];
        assert_eq!(props["query"]["type"], "string", "required stays non-null");
        assert_eq!(props["limit"]["type"], json!(["integer", "null"]));
        assert_eq!(props["kind"]["enum"], json!(["a", "b", null]));
        assert_eq!(props["ids"]["type"], json!(["array", "null"]));
        assert_eq!(props["filters"]["type"], json!(["object", "null"]));
        // Nested objects get the same treatment.
        assert_eq!(props["filters"]["required"], json!(["since"]));
        assert_eq!(props["filters"]["additionalProperties"], false);
        assert_eq!(
            props["filters"]["properties"]["since"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            props["anything"]["anyOf"][1],
            json!({"type": "null"}),
            "untyped optionals are wrapped"
        );
    }

    /// The real no-tools mapping used by every legacy and converse request:
    /// Default omits `think`, Disabled sends false, Enabled sends true.
    #[test]
    fn ollama_think_without_tools_follows_the_legacy_rule() {
        for (mode, want) in [
            (ThinkingMode::Default, None),
            (ThinkingMode::Disabled, Some(false)),
            (ThinkingMode::Enabled, Some(true)),
        ] {
            let r = ConverseRequest {
                messages: vec![Message::user("hi")],
                thinking: mode,
                ..Default::default()
            };
            assert_eq!(ollama_think(&r), want, "{mode:?}");
            let body = ollama_body("m", &r, ollama_think(&r), None, false);
            match want {
                None => assert!(body.get("think").is_none(), "{mode:?}"),
                Some(v) => assert_eq!(body["think"], v, "{mode:?}"),
            }
            // Same request converted from the legacy type.
            let legacy = ChatRequest {
                messages: vec![ChatMessage::user("hi")],
                temperature: None,
                max_tokens: None,
                thinking: mode,
                format: None,
                effort: None,
            };
            assert_eq!(ollama_think(&ConverseRequest::from(&legacy)), want);
        }
    }
}
