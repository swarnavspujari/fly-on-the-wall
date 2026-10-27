//! Anthropic Claude provider (`POST /v1/messages`).
//!
//! Tool calling (`converse` / `chat_stream`): tools go out as
//! `{name, description, input_schema, strict}` — strict (schema-guaranteed
//! arguments, objects closed) on models with the structured-outputs surface,
//! at most [`MAX_STRICT_TOOLS`] per request. Calls come back as `tool_use`
//! blocks and results go back as `tool_result` blocks, first in the next
//! user turn. `thinking` / `redacted_thinking` blocks are kept as opaque
//! blocks and echoed back, because the API checks them when a tool result
//! follows. Forced tool choice (`any`) is never sent to Opus/Sonnet 5.5,
//! Fable or Mythos (they reject it) or to a request that may be thinking;
//! those fall back to auto. `relax_after_400` also drops a rejected
//! `tool_choice` or `strict`.

use serde_json::{json, Value};

use crate::conversation::{
    closed_schema, Capabilities, ChatResponse, ContentBlock, ConverseRequest, Message, StopReason,
    StreamEvent, ToolChoice, Usage,
};
use crate::stream::{decode_body, sse_data, LineDecoder};
use crate::{ChatRequest, ChatStream, LLMProvider, LlmError, Result, Role};

/// Anthropic's limit on strict tools in one request.
pub const MAX_STRICT_TOOLS: usize = 20;

pub const ANTHROPIC_DEFAULT_MODEL: &str = "claude-sonnet-5-5";

/// The fast, low-cost alternative offered next to the default.
pub const ANTHROPIC_FAST_MODEL: &str = "claude-haiku-5-5";

/// Map a saved, previous-generation Sonnet/Haiku id to its 5.5 successor.
/// Returns `None` for ids that should be left alone (already current, an
/// Opus/Fable pick, or anything unrecognised) so a deliberate choice of a
/// different tier is never overridden.
pub fn successor_model(model: &str) -> Option<&'static str> {
    let m = model.trim().to_ascii_lowercase();
    if m == ANTHROPIC_DEFAULT_MODEL || m == ANTHROPIC_FAST_MODEL {
        return None;
    }
    // Known previous generations only: a newer id typed in as a custom model
    // (say a future claude-sonnet-6) must never be "migrated" backwards.
    let old_sonnet = m == "claude-sonnet-5"
        || m.starts_with("claude-sonnet-5-20")
        || m.starts_with("claude-sonnet-4")
        || (m.starts_with("claude-3") && m.contains("sonnet"));
    if old_sonnet {
        return Some(ANTHROPIC_DEFAULT_MODEL);
    }
    let old_haiku = m.starts_with("claude-haiku-4")
        || m.starts_with("claude-haiku-3")
        || (m.starts_with("claude-3") && m.contains("haiku"));
    if old_haiku {
        return Some(ANTHROPIC_FAST_MODEL);
    }
    None
}

/// How to ask a model for "no extended thinking". The API shape differs by
/// model: `{type: disabled}` returns a 400 on Sonnet 5.5 (which takes
/// `between_tools` instead) and on Opus 5.5 / Fable / Mythos (thinking can't
/// be turned off there — lowest effort is the closest equivalent).
enum ThinkingOff {
    Disabled,
    BetweenTools,
    LowEffort,
}

fn thinking_off_for(model: &str) -> ThinkingOff {
    let m = model.trim().to_ascii_lowercase();
    if m.starts_with("claude-sonnet-5-5") {
        ThinkingOff::BetweenTools
    } else if m.starts_with("claude-opus-5-5")
        || m.starts_with("claude-fable")
        || m.starts_with("claude-mythos")
    {
        ThinkingOff::LowEffort
    } else {
        ThinkingOff::Disabled
    }
}

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
    /// Retry a 400 without the optional feature it names (see
    /// `relax_after_400`). On in the app; live tests turn it off so a
    /// rejected request shape fails loudly instead of degrading silently.
    pub relax_on_400: bool,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            base_url: "https://api.anthropic.com".into(),
            relax_on_400: true,
        }
    }

    /// Build the `/v1/messages` request body for a legacy text request.
    /// Split out from `chat` so the parameter shaping (system out-of-band,
    /// optional temperature, thinking off) is unit-testable without a
    /// network round-trip.
    pub(crate) fn request_body(&self, req: &ChatRequest) -> serde_json::Value {
        self.converse_body(&ConverseRequest::from(req))
    }

    /// The `/v1/messages` body for a block request. Text-only messages keep
    /// the legacy `{role, content: "<text>"}` shape exactly.
    fn converse_body(&self, req: &ConverseRequest) -> serde_json::Value {
        // Anthropic takes the system prompt out-of-band.
        let system: String = req
            .messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(Message::text)
            .collect::<Vec<_>>()
            .join("\n\n");
        let messages: Vec<_> = req
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(anthropic_message)
            .collect();

        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens.unwrap_or(4096),
            "messages": messages,
        });
        if !system.is_empty() {
            // Cache the (stable) system prefix so a request doesn't re-bill the
            // whole system prompt every call (GA, no beta header). Below the
            // model's minimum cacheable length (512 tokens on the 5.5 models,
            // more on older ones) it is a silent no-op, so it only ever helps:
            // chiefly `ask_meeting`, whose system carries the full transcript
            // and is re-sent on every chat turn.
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
        let modern = supports_output_config(&self.model);
        let mut effort = req
            .effort
            .as_deref()
            .filter(|e| modern && matches!(*e, "low" | "medium" | "high"))
            .map(str::to_string);
        // Current models run adaptive thinking by default; for mechanical
        // transforms that budget is wasted and can truncate the answer, so let
        // callers turn it off — in whatever shape this model accepts.
        if matches!(req.thinking, crate::ThinkingMode::Disabled) {
            match thinking_off_for(&self.model) {
                ThinkingOff::Disabled => body["thinking"] = json!({ "type": "disabled" }),
                // `between_tools` takes no other field and allows effort up
                // to `high` only (the filter above already caps it there).
                ThinkingOff::BetweenTools => body["thinking"] = json!({ "type": "between_tools" }),
                // Thinking can't be turned off: lowest effort is the closest.
                ThinkingOff::LowEffort => effort = Some("low".into()),
            }
        }
        if let Some(e) = effort {
            output_config(&mut body).insert("effort".into(), json!(e));
        }
        // Server-side structured outputs: schema-guaranteed JSON. The API wants
        // an object at the root, so a top-level array schema is wrapped in
        // `{"items": [...]}` and unwrapped again in `chat`.
        if let (true, Some(schema)) = (modern, &req.format) {
            output_config(&mut body).insert(
                "format".into(),
                json!({ "type": "json_schema", "schema": api_schema(schema) }),
            );
        }
        // Sonnet 5.5 can decline borderline requests (e.g. a security team's
        // meeting tripping the `cyber` classifier). Server-side fallback
        // re-runs those on the previous Sonnet inside the same call. Claude
        // API only, Sonnet 5.5 only; Haiku 5.5 has no server-side fallback.
        if self
            .model
            .trim()
            .to_ascii_lowercase()
            .starts_with("claude-sonnet-5-5")
            && self.base_url.contains("api.anthropic.com")
        {
            body["fallbacks"] = json!("default");
        }
        if req.has_tools() {
            let strict_ok = supports_output_config(&self.model);
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    let strict = t.strict && strict_ok;
                    let mut tool = json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": if strict {
                            closed_schema(&t.input_schema)
                        } else {
                            t.input_schema.clone()
                        },
                    });
                    if strict {
                        tool["strict"] = json!(true);
                    }
                    tool
                })
                .collect();
            body["tools"] = json!(tools);
            match req.tool_choice {
                // `auto` is the API default.
                ToolChoice::Auto => {}
                ToolChoice::None => body["tool_choice"] = json!({ "type": "none" }),
                ToolChoice::Required => {
                    if self.forced_tool_choice_ok(req) {
                        body["tool_choice"] = json!({ "type": "any" });
                    }
                }
            }
        }
        body
    }

    /// Forced tool choice is a 400 on Opus/Sonnet 5.5, Fable and Mythos, and
    /// on any request where thinking may be on (current models think by
    /// default) — only send it where neither applies.
    fn forced_tool_choice_ok(&self, req: &ConverseRequest) -> bool {
        if rejects_forced_tool_choice(&self.model) {
            return false;
        }
        let thinking_off = req.thinking == crate::ThinkingMode::Disabled
            && matches!(thinking_off_for(&self.model), ThinkingOff::Disabled);
        thinking_off || !supports_output_config(&self.model)
    }

    /// Refuse request shapes the API would reject outright.
    fn validate(&self, req: &ConverseRequest) -> Result<()> {
        if supports_output_config(&self.model) {
            let strict = req.tools.iter().filter(|t| t.strict).count();
            if strict > MAX_STRICT_TOOLS {
                return Err(LlmError::InvalidRequest(format!(
                    "{strict} strict tools; Anthropic allows at most {MAX_STRICT_TOOLS} per request"
                )));
            }
        }
        Ok(())
    }

    /// POST with `relax_after_400` retries; the success body text.
    async fn post_relaxed(&self, body: &mut serde_json::Value) -> Result<String> {
        let (mut status, mut text) = self.post(body).await?;
        for _ in 0..4 {
            if !self.relax_on_400 || status.as_u16() != 400 || !relax_after_400(body, &text) {
                break;
            }
            (status, text) = self.post(body).await?;
        }
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(LlmError::Auth);
        }
        if !status.is_success() {
            return Err(LlmError::Provider(format!(
                "{status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        Ok(text)
    }

    /// Streaming send: relax-and-retry happens on the 400 before any bytes
    /// stream, so it behaves like `post_relaxed`.
    async fn send_stream(&self, body: &mut serde_json::Value) -> Result<reqwest::Response> {
        let mut attempts = 0;
        loop {
            let resp = self
                .request(body)
                .send()
                .await
                .map_err(|e| crate::transport_error("anthropic", false, &self.base_url, e))?;
            let status = resp.status();
            if status.is_success() {
                return Ok(resp);
            }
            let text = resp
                .text()
                .await
                .map_err(|e| LlmError::Network(e.to_string()))?;
            if attempts < 4
                && self.relax_on_400
                && status.as_u16() == 400
                && relax_after_400(body, &text)
            {
                attempts += 1;
                continue;
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(LlmError::Auth);
            }
            return Err(LlmError::Provider(format!(
                "{status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
    }

    fn request(&self, body: &serde_json::Value) -> reqwest::RequestBuilder {
        let mut request = http_client()
            .post(format!(
                "{}/v1/messages",
                self.base_url.trim_end_matches('/')
            ))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01");
        if body.get("fallbacks").is_some() {
            request = request.header("anthropic-beta", FALLBACK_BETA);
        }
        request.json(body)
    }

    async fn post(&self, body: &serde_json::Value) -> Result<(reqwest::StatusCode, String)> {
        let resp = self
            .request(body)
            .send()
            .await
            .map_err(|e| crate::transport_error("anthropic", false, &self.base_url, e))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        Ok((status, text))
    }
}

const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// One non-system message. Plain text keeps the legacy string content;
/// anything else becomes a block array with tool results first (the API
/// requires them at the start of the user turn after a `tool_use`). Opaque
/// blocks from other providers are dropped; empty text blocks too (the API
/// rejects them).
fn anthropic_message(m: &Message) -> Value {
    let role = if m.role == Role::Assistant {
        "assistant"
    } else {
        "user"
    };
    if m.is_plain_text() {
        return json!({ "role": role, "content": m.text() });
    }
    let mut results = Vec::new();
    let mut rest = Vec::new();
    for b in &m.content {
        match b {
            ContentBlock::Text { text } if !text.is_empty() => {
                rest.push(json!({ "type": "text", "text": text }))
            }
            ContentBlock::Text { .. } => {}
            ContentBlock::ToolUse { id, name, input } => rest.push(json!({
                "type": "tool_use", "id": id, "name": name, "input": input,
            })),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let mut r = json!({
                    "type": "tool_result", "tool_use_id": tool_use_id, "content": content,
                });
                if *is_error {
                    r["is_error"] = json!(true);
                }
                results.push(r);
            }
            ContentBlock::Opaque { provider, raw } if provider == "anthropic" => {
                rest.push(raw.clone())
            }
            ContentBlock::Opaque { .. } => {}
        }
    }
    results.extend(rest);
    json!({ "role": role, "content": results })
}

/// Models that 400 on `tool_choice: any | tool`.
fn rejects_forced_tool_choice(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    [
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-fable",
        "claude-mythos",
    ]
    .iter()
    .any(|p| m.starts_with(p))
}

/// Models with the current request surface: `output_config.effort` and
/// `output_config.format` (structured outputs). Older ids keep the legacy
/// shape; anything unrecognised also stays legacy (safe — a field the model
/// doesn't know would be a 400).
fn supports_output_config(model: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "claude-sonnet-5",
        "claude-haiku-5",
        "claude-opus-5",
        "claude-opus-4-6",
        "claude-opus-4-7",
        "claude-opus-4-8",
        "claude-sonnet-4-6",
        "claude-fable",
        "claude-mythos",
    ];
    let m = model.trim().to_ascii_lowercase();
    PREFIXES.iter().any(|p| m.starts_with(p))
}

fn output_config(body: &mut serde_json::Value) -> &mut serde_json::Map<String, serde_json::Value> {
    if !body["output_config"].is_object() {
        body["output_config"] = json!({});
    }
    body["output_config"].as_object_mut().expect("just set")
}

/// Adapt a caller's JSON schema to the structured-outputs rules: an object at
/// the root (arrays are wrapped as `{"items": [...]}`) and
/// `additionalProperties: false` on every object.
fn api_schema(schema: &serde_json::Value) -> serde_json::Value {
    let s = closed_schema(schema);
    if schema_root_is_array(schema) {
        json!({
            "type": "object",
            "properties": { "items": s },
            "required": ["items"],
            "additionalProperties": false,
        })
    } else {
        s
    }
}

fn schema_root_is_array(schema: &serde_json::Value) -> bool {
    schema.get("type").and_then(|t| t.as_str()) == Some("array")
}

/// One shared client (connection pool + TLS session reuse) with a bounded
/// timeout so a stalled connection can't hang a pipeline step forever.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(1200))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// After a 400, drop the optional request feature the error names and say
/// whether a retry is worthwhile. Each feature is removed at most once, so a
/// model released after this build degrades to the plain request shape
/// instead of failing outright.
fn relax_after_400(body: &mut serde_json::Value, err_body: &str) -> bool {
    // Match on the API's error message only (never the whole body, which can
    // echo request text), case-insensitively, by parameter path.
    let err = serde_json::from_str::<serde_json::Value>(err_body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")?
                .as_str()
                .map(str::to_ascii_lowercase)
        })
        .unwrap_or_default();
    if err.is_empty() {
        return false;
    }
    let Some(obj) = body.as_object_mut() else {
        return false;
    };
    if err.contains("fallback") && obj.remove("fallbacks").is_some() {
        return true;
    }
    // Tool features first: a forced-choice rejection can mention thinking.
    // Legacy bodies carry neither key, so these never fire for them.
    if err.contains("tool_choice") && obj.remove("tool_choice").is_some() {
        return true;
    }
    if err.contains("strict") {
        let mut dropped = false;
        if let Some(tools) = obj.get_mut("tools").and_then(|t| t.as_array_mut()) {
            for t in tools.iter_mut().filter_map(|t| t.as_object_mut()) {
                dropped |= t.remove("strict").is_some();
            }
        }
        if dropped {
            return true;
        }
    }
    if err.contains("thinking.") && obj.remove("thinking").is_some() {
        // Closest stand-in for "thinking off" on models with an effort knob;
        // older models (no output_config at all) just drop the field.
        if let Some(oc) = obj.get_mut("output_config").and_then(|v| v.as_object_mut()) {
            oc.insert("effort".into(), json!("low"));
        }
        return true;
    }
    if let Some(oc) = obj.get_mut("output_config").and_then(|v| v.as_object_mut()) {
        if (err.contains("output_config.format")
            || err.contains("json_schema")
            || err.contains("schema"))
            && oc.remove("format").is_some()
        {
            return true;
        }
        if err.contains("effort") && oc.remove("effort").is_some() {
            return true;
        }
    }
    false
}

#[async_trait::async_trait]
impl LLMProvider for AnthropicProvider {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn chat(&self, req: ChatRequest) -> Result<String> {
        let mut body = self.request_body(&req);
        let text = self.post_relaxed(&mut body).await?;
        let structured = body.pointer("/output_config/format").is_some();
        let out = parse_messages_response_checked(&text, structured)?;
        // Undo the root-array wrapping applied in `api_schema`.
        match (&req.format, structured) {
            (Some(schema), true) if schema_root_is_array(schema) => Ok(unwrap_items(&out)),
            _ => Ok(out),
        }
    }

    async fn test_connection(&self) -> Result<()> {
        // Thinking off and room to spare: on 5.5-generation models thinking is
        // on by default and counts against max_tokens, so a tiny cap could end
        // the turn before any text and fail a perfectly good key.
        self.chat(ChatRequest {
            effort: None,
            messages: vec![crate::ChatMessage::user("Reply with the single word: ok")],
            temperature: Some(0.0),
            max_tokens: Some(256),
            thinking: crate::ThinkingMode::Disabled,
            format: None,
        })
        .await
        .map(|_| ())
    }

    fn capabilities(&self) -> Capabilities {
        let modern = supports_output_config(&self.model);
        Capabilities {
            tools: true,
            tool_choice_none: true,
            tool_choice_required: !rejects_forced_tool_choice(&self.model),
            strict_tools: modern,
            max_strict_tools: if modern { MAX_STRICT_TOOLS } else { 0 },
            streaming: true,
            thinking_control: true,
            structured_output: modern,
            probed: false,
        }
    }

    async fn converse(&self, req: ConverseRequest) -> Result<ChatResponse> {
        self.validate(&req)?;
        let mut body = self.converse_body(&req);
        let text = self.post_relaxed(&mut body).await?;
        let structured = body.pointer("/output_config/format").is_some();
        let mut resp = parse_messages_converse(&text, structured)?;
        // Undo the root-array wrapping applied in `api_schema`.
        if let (Some(schema), true) = (&req.format, structured) {
            if schema_root_is_array(schema) {
                let joined = resp.text();
                resp.blocks
                    .retain(|b| !matches!(b, ContentBlock::Text { .. }));
                resp.blocks
                    .insert(0, ContentBlock::text(unwrap_items(&joined)));
            }
        }
        Ok(resp)
    }

    /// Note: with an array-rooted `format`, streamed text is the wrapped
    /// `{"items": [...]}` object (deltas can't be unwrapped mid-stream) —
    /// stream prose, use `converse` for structured output.
    async fn chat_stream(&self, req: ConverseRequest) -> Result<ChatStream> {
        self.validate(&req)?;
        let mut body = self.converse_body(&req);
        body["stream"] = json!(true);
        let resp = self.send_stream(&mut body).await?;
        let structured = body.pointer("/output_config/format").is_some();
        Ok(decode_body(
            resp.bytes_stream(),
            AnthropicSse {
                structured,
                ..Default::default()
            },
        ))
    }
}

fn refusal_error(category: Option<&str>) -> LlmError {
    LlmError::Provider(format!(
        "Claude declined this request (safety category: {}). \
         Try again, or switch to another model in Settings.",
        category.unwrap_or("unspecified")
    ))
}

fn stop_reason_of(stop: &str) -> StopReason {
    match stop {
        "tool_use" => StopReason::ToolUse,
        "max_tokens" | "model_context_window_exceeded" => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    }
}

fn usage_of(u: Option<&Value>) -> Usage {
    let get = |k: &str| {
        u.and_then(|u| u.get(k))
            .and_then(|n| n.as_u64())
            .map(|n| n as u32)
    };
    Usage {
        input_tokens: get("input_tokens"),
        output_tokens: get("output_tokens"),
        cache_read_tokens: get("cache_read_input_tokens"),
        cache_write_tokens: get("cache_creation_input_tokens"),
    }
}

/// Parse a Messages response into blocks, with the same refusal and
/// truncation rules as [`parse_messages_response_checked`]: a refusal is an
/// error; hitting `max_tokens` is an error when `strict` (structured JSON)
/// or when nothing usable came back.
pub fn parse_messages_converse(json_text: &str, strict: bool) -> Result<ChatResponse> {
    let v: Value = serde_json::from_str(json_text)
        .map_err(|e| LlmError::Provider(format!("bad JSON from provider: {e}")))?;
    let stop = v.get("stop_reason").and_then(|s| s.as_str()).unwrap_or("");
    if stop == "refusal" {
        return Err(refusal_error(
            v.pointer("/stop_details/category").and_then(|c| c.as_str()),
        ));
    }
    let content = v
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| LlmError::Provider("response had no content array".into()))?;
    let mut blocks = Vec::new();
    for b in content {
        let s = |k: &str| b.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        match b.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                let text = s("text");
                if !text.is_empty() {
                    blocks.push(ContentBlock::Text { text });
                }
            }
            Some("tool_use") => blocks.push(ContentBlock::ToolUse {
                id: s("id"),
                name: s("name"),
                input: b.get("input").cloned().unwrap_or_else(|| json!({})),
            }),
            Some("thinking") | Some("redacted_thinking") => blocks.push(ContentBlock::Opaque {
                provider: "anthropic".into(),
                raw: b.clone(),
            }),
            // e.g. server-side `fallback` markers: not part of the answer.
            _ => {}
        }
    }
    let stop_reason = stop_reason_of(stop);
    let resp = ChatResponse {
        blocks,
        stop_reason,
        usage: usage_of(v.get("usage")),
    };
    if stop_reason == StopReason::MaxTokens {
        let empty = resp.text().is_empty() && resp.tool_uses().is_empty();
        if strict || empty {
            return Err(LlmError::Provider(if empty {
                "response hit the token limit before any text (thinking used the budget)".into()
            } else {
                "response was cut off at the token limit before the JSON was complete".into()
            }));
        }
    }
    Ok(resp)
}

/// Messages API SSE: `message_start` → per-block `content_block_start` /
/// `_delta` / `_stop` → `message_delta` (stop reason, output usage) →
/// `message_stop`. Thinking blocks are accumulated and emitted whole as
/// opaque blocks (with their signature) when they close.
#[derive(Debug, Default)]
pub(crate) struct AnthropicSse {
    structured: bool,
    /// block index → open block
    open: std::collections::HashMap<u64, SseBlock>,
    stop: Option<StopReason>,
    usage: Usage,
    output: bool,
    done: bool,
}

#[derive(Debug)]
enum SseBlock {
    Text,
    Tool(String),
    Opaque(Value),
    Other,
}

impl LineDecoder for AnthropicSse {
    fn line(&mut self, line: &str) -> Result<Vec<StreamEvent>> {
        let Some(data) = sse_data(line) else {
            return Ok(vec![]);
        };
        if self.done || data.trim().is_empty() {
            return Ok(vec![]);
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| LlmError::Provider(format!("bad JSON in stream: {e}")))?;
        let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
        let mut out = Vec::new();
        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "message_start" => {
                self.usage = usage_of(v.pointer("/message/usage"));
            }
            "content_block_start" => {
                let b = v.get("content_block").cloned().unwrap_or(Value::Null);
                let s = |k: &str| b.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                let block = match b.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        let text = s("text");
                        if !text.is_empty() {
                            self.output = true;
                            out.push(StreamEvent::TextDelta { text });
                        }
                        SseBlock::Text
                    }
                    Some("tool_use") => {
                        let id = s("id");
                        self.output = true;
                        out.push(StreamEvent::ToolUseStart {
                            id: id.clone(),
                            name: s("name"),
                        });
                        SseBlock::Tool(id)
                    }
                    Some("thinking") | Some("redacted_thinking") => SseBlock::Opaque(b),
                    _ => SseBlock::Other,
                };
                self.open.insert(index, block);
            }
            "content_block_delta" => {
                let d = v.get("delta").cloned().unwrap_or(Value::Null);
                let s = |k: &str| d.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                match (
                    self.open.get_mut(&index),
                    d.get("type").and_then(|t| t.as_str()),
                ) {
                    (Some(SseBlock::Text), Some("text_delta")) => {
                        let text = s("text");
                        if !text.is_empty() {
                            self.output = true;
                            out.push(StreamEvent::TextDelta { text });
                        }
                    }
                    (Some(SseBlock::Tool(id)), Some("input_json_delta")) => {
                        out.push(StreamEvent::ToolInputDelta {
                            id: id.clone(),
                            partial_json: s("partial_json"),
                        });
                    }
                    (Some(SseBlock::Opaque(raw)), Some("thinking_delta")) => {
                        append_str(raw, "thinking", &s("thinking"));
                    }
                    (Some(SseBlock::Opaque(raw)), Some("signature_delta")) => {
                        append_str(raw, "signature", &s("signature"));
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(SseBlock::Opaque(raw)) = self.open.remove(&index) {
                    out.push(StreamEvent::Opaque {
                        provider: "anthropic".into(),
                        raw,
                    });
                }
            }
            "message_delta" => {
                if let Some(stop) = v.pointer("/delta/stop_reason").and_then(|s| s.as_str()) {
                    if stop == "refusal" {
                        return Err(refusal_error(
                            v.pointer("/delta/stop_details/category")
                                .and_then(|c| c.as_str()),
                        ));
                    }
                    self.stop = Some(stop_reason_of(stop));
                }
                if let Some(n) = v.pointer("/usage/output_tokens").and_then(|n| n.as_u64()) {
                    self.usage.output_tokens = Some(n as u32);
                }
            }
            "message_stop" => {
                self.done = true;
                let stop_reason = self.stop.unwrap_or(StopReason::EndTurn);
                if stop_reason == StopReason::MaxTokens && (self.structured || !self.output) {
                    return Err(LlmError::Provider(
                        "response hit the token limit before it was complete".into(),
                    ));
                }
                out.push(StreamEvent::Done {
                    stop_reason,
                    usage: self.usage,
                });
            }
            "error" => {
                let msg = v
                    .pointer("/error/message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("stream error");
                return Err(LlmError::Provider(msg.chars().take(300).collect()));
            }
            _ => {} // ping and future event types
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>> {
        if self.done {
            Ok(vec![])
        } else {
            Err(LlmError::Network(
                "Claude's reply stream ended before it finished".into(),
            ))
        }
    }
}

fn append_str(obj: &mut Value, key: &str, more: &str) {
    let prev = obj.get(key).and_then(|t| t.as_str()).unwrap_or("");
    obj[key] = json!(format!("{prev}{more}"));
}

/// `{"items": [...]}` → `[...]` as JSON text; anything else passes through.
fn unwrap_items(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("items").cloned())
        .filter(|v| v.is_array())
        .map(|v| v.to_string())
        .unwrap_or_else(|| text.to_string())
}

pub fn parse_messages_response(json_text: &str) -> Result<String> {
    parse_messages_response_checked(json_text, false)
}

/// Read the text blocks of a Messages response (by block `type`, so leading
/// `thinking` / `fallback` blocks are skipped). `stop_reason` is checked
/// first: a refusal is always an error, even with partial text; with
/// `strict` (structured JSON requested) hitting `max_tokens` is an error too,
/// because a cut-off schema answer is not valid output.
pub fn parse_messages_response_checked(json_text: &str, strict: bool) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(json_text)
        .map_err(|e| LlmError::Provider(format!("bad JSON from provider: {e}")))?;
    let stop = v.get("stop_reason").and_then(|s| s.as_str()).unwrap_or("");
    if stop == "refusal" {
        let category = v
            .pointer("/stop_details/category")
            .and_then(|c| c.as_str())
            .unwrap_or("unspecified");
        return Err(LlmError::Provider(format!(
            "Claude declined this request (safety category: {category}). \
             Try again, or switch to another model in Settings."
        )));
    }
    let content = v
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| LlmError::Provider("response had no content array".into()))?;
    let text: String = content
        .iter()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
        .map(|block| block.get("text").and_then(|t| t.as_str()).unwrap_or(""))
        .collect();
    if stop == "max_tokens" && (strict || text.is_empty()) {
        return Err(LlmError::Provider(if text.is_empty() {
            "response hit the token limit before any text (thinking used the budget)".into()
        } else {
            "response was cut off at the token limit before the JSON was complete".into()
        }));
    }
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
            effort: None,
            messages: vec![
                crate::ChatMessage::system("be terse"),
                crate::ChatMessage::user("hi"),
            ],
            temperature: temp,
            max_tokens: Some(1000),
            thinking,
            format: None,
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
        for m in [
            "claude-sonnet-4",
            "claude-sonnet-4-6",
            "claude-opus-4-5",
            "claude-opus-4-6",
            "claude-haiku-4-5",
            "claude-3-5-haiku",
        ] {
            assert!(
                model_accepts_temperature(m),
                "{m} should accept temperature"
            );
        }
        for m in [
            "claude-sonnet-5",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-fable-5",
            "claude-mythos-5",
        ] {
            assert!(
                !model_accepts_temperature(m),
                "{m} must NOT accept temperature"
            );
        }
    }

    #[test]
    fn request_body_thinking_disabled_frees_budget() {
        // Disabled thinking → `thinking: {type: disabled}` so the full token
        // budget goes to the answer (the transcript-cleanup pass relies on this
        // to avoid claude-sonnet-5 truncating its JSON mid-array). No temperature.
        let p = AnthropicProvider::new("k".into(), "claude-sonnet-5".into());
        let body = p.request_body(&ChatRequest {
            effort: None,
            messages: vec![crate::ChatMessage::user("hi")],
            temperature: None,
            max_tokens: Some(8192),
            thinking: crate::ThinkingMode::Disabled,
            format: None,
        });
        assert_eq!(body["thinking"], json!({ "type": "disabled" }));
        assert!(body.get("temperature").is_none());
    }

    fn off_body(model: &str) -> serde_json::Value {
        AnthropicProvider::new("k".into(), model.into()).request_body(&ChatRequest {
            effort: None,
            messages: vec![crate::ChatMessage::user("hi")],
            temperature: Some(0.0),
            max_tokens: Some(8192),
            thinking: crate::ThinkingMode::Disabled,
            format: None,
        })
    }

    #[test]
    fn default_model_is_sonnet_5_5() {
        assert_eq!(ANTHROPIC_DEFAULT_MODEL, "claude-sonnet-5-5");
        assert_eq!(ANTHROPIC_FAST_MODEL, "claude-haiku-5-5");
    }

    #[test]
    fn sonnet_5_5_turns_thinking_off_with_between_tools() {
        // `{type: disabled}` is a 400 on Sonnet 5.5; `between_tools` takes no
        // other field, and sampling params must not be sent.
        let body = off_body("claude-sonnet-5-5");
        assert_eq!(body["thinking"], json!({ "type": "between_tools" }));
        assert!(body.get("output_config").is_none());
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn haiku_5_5_accepts_disabled_at_default_effort() {
        let body = off_body("claude-haiku-5-5");
        assert_eq!(body["thinking"], json!({ "type": "disabled" }));
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn always_thinking_models_get_low_effort_instead() {
        for m in ["claude-opus-5-5", "claude-fable-5-1", "claude-mythos-5-1"] {
            let body = off_body(m);
            assert!(body.get("thinking").is_none(), "{m}");
            assert_eq!(body["output_config"]["effort"], "low", "{m}");
        }
    }

    #[test]
    fn default_thinking_sends_no_thinking_field() {
        let p = AnthropicProvider::new("k".into(), "claude-sonnet-5-5".into());
        let body = p.request_body(&req(None, crate::ThinkingMode::Default));
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn successor_model_upgrades_old_sonnet_and_haiku_only() {
        assert_eq!(
            successor_model("claude-sonnet-5"),
            Some("claude-sonnet-5-5")
        );
        assert_eq!(
            successor_model("claude-sonnet-4-6"),
            Some("claude-sonnet-5-5")
        );
        assert_eq!(
            successor_model("claude-3-5-sonnet-latest"),
            Some("claude-sonnet-5-5")
        );
        assert_eq!(
            successor_model("claude-haiku-4-5"),
            Some("claude-haiku-5-5")
        );
        assert_eq!(
            successor_model("claude-3-5-haiku-latest"),
            Some("claude-haiku-5-5")
        );
        for keep in [
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-opus-5-5",
            "claude-fable-5-1",
            "",
            "my-proxy-model",
            "claude-sonnet-6",
            "claude-haiku-6",
        ] {
            assert_eq!(successor_model(keep), None, "{keep:?}");
        }
    }

    #[test]
    fn refusal_and_truncation_get_clear_errors() {
        let refusal = r#"{"content":[],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}}"#;
        let e = parse_messages_response(refusal).unwrap_err().to_string();
        assert!(e.contains("declined") && e.contains("cyber"), "{e}");
        let cut = r#"{"content":[{"type":"thinking","thinking":""}],"stop_reason":"max_tokens"}"#;
        let e = parse_messages_response(cut).unwrap_err().to_string();
        assert!(e.contains("token limit"), "{e}");
    }

    #[test]
    fn thinking_blocks_are_skipped_when_reading_text() {
        let json = r#"{"content":[{"type":"thinking","thinking":""},{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#;
        assert_eq!(parse_messages_response(json).unwrap(), "ok");
    }

    fn task(
        model: &str,
        thinking: crate::ThinkingMode,
        effort: Option<&str>,
        format: Option<serde_json::Value>,
    ) -> serde_json::Value {
        AnthropicProvider::new("k".into(), model.into()).request_body(&ChatRequest {
            messages: vec![
                crate::ChatMessage::system("sys"),
                crate::ChatMessage::user("hi"),
            ],
            temperature: Some(0.0),
            max_tokens: Some(8192),
            thinking,
            format,
            effort: effort.map(String::from),
        })
    }

    fn array_schema() -> serde_json::Value {
        json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }
        })
    }

    #[test]
    fn effort_goes_in_output_config_for_current_models() {
        let b = task(
            "claude-haiku-5-5",
            crate::ThinkingMode::Default,
            Some("low"),
            None,
        );
        assert_eq!(b["output_config"]["effort"], "low");
        assert!(b.get("thinking").is_none());
        // Sonnet 5.5 thinking-off: between_tools alone in `thinking`, effort
        // beside it in output_config (allowed up to `high`).
        let b = task(
            "claude-sonnet-5-5",
            crate::ThinkingMode::Disabled,
            Some("high"),
            None,
        );
        assert_eq!(b["thinking"], json!({ "type": "between_tools" }));
        assert_eq!(b["output_config"]["effort"], "high");
        // Values the API would reject with between_tools never get through.
        for bad in ["xhigh", "max", "turbo"] {
            let b = task(
                "claude-sonnet-5-5",
                crate::ThinkingMode::Disabled,
                Some(bad),
                None,
            );
            assert!(b.pointer("/output_config/effort").is_none(), "{bad}");
        }
    }

    #[test]
    fn opus_5_5_thinking_off_maps_to_low_effort_overriding_the_hint() {
        let b = task(
            "claude-opus-5-5",
            crate::ThinkingMode::Disabled,
            Some("high"),
            None,
        );
        assert!(b.get("thinking").is_none());
        assert_eq!(b["output_config"]["effort"], "low");
    }

    #[test]
    fn legacy_models_get_no_output_config() {
        for m in [
            "claude-haiku-4-5",
            "claude-sonnet-4-5",
            "claude-3-5-haiku",
            "some-proxy",
        ] {
            let b = task(
                m,
                crate::ThinkingMode::Default,
                Some("low"),
                Some(array_schema()),
            );
            assert!(b.get("output_config").is_none(), "{m}");
            assert!(b.get("fallbacks").is_none(), "{m}");
        }
    }

    #[test]
    fn structured_outputs_wrap_array_roots_and_close_objects() {
        let b = task(
            "claude-sonnet-5-5",
            crate::ThinkingMode::Disabled,
            Some("high"),
            Some(array_schema()),
        );
        let f = &b["output_config"]["format"];
        assert_eq!(f["type"], "json_schema");
        assert_eq!(f["schema"]["type"], "object");
        assert_eq!(f["schema"]["required"], json!(["items"]));
        assert_eq!(f["schema"]["additionalProperties"], false);
        assert_eq!(
            f["schema"]["properties"]["items"]["items"]["additionalProperties"],
            false
        );
        // Effort and format share one output_config (merged, not overwritten).
        assert_eq!(b["output_config"]["effort"], "high");
        assert!(b.get("temperature").is_none());
        // Object-rooted schemas pass through unwrapped.
        let obj = json!({"type":"object","properties":{"a":{"type":"string"}},"required":["a"]});
        let b = task(
            "claude-haiku-5-5",
            crate::ThinkingMode::Default,
            None,
            Some(obj),
        );
        assert_eq!(b["output_config"]["format"]["schema"]["type"], "object");
        assert!(b["output_config"]["format"]["schema"]["properties"]
            .get("items")
            .is_none());
    }

    #[test]
    fn unwrap_items_restores_the_array_text() {
        assert_eq!(
            unwrap_items(r#"{"items":[{"text":"a"}]}"#),
            r#"[{"text":"a"}]"#
        );
        assert_eq!(unwrap_items("not json"), "not json");
        assert_eq!(unwrap_items(r#"{"other":1}"#), r#"{"other":1}"#);
    }

    #[test]
    fn server_side_fallback_only_for_sonnet_5_5_on_the_claude_api() {
        let b = task(
            "claude-sonnet-5-5",
            crate::ThinkingMode::Default,
            None,
            None,
        );
        assert_eq!(b["fallbacks"], "default");
        for m in ["claude-haiku-5-5", "claude-opus-5-5", "claude-sonnet-5"] {
            assert!(task(m, crate::ThinkingMode::Default, None, None)
                .get("fallbacks")
                .is_none());
        }
        let mut p = AnthropicProvider::new("k".into(), "claude-sonnet-5-5".into());
        p.base_url = "https://my-gateway.example".into();
        assert!(p
            .request_body(&req(None, crate::ThinkingMode::Default))
            .get("fallbacks")
            .is_none());
    }

    #[test]
    fn relax_after_400_drops_the_named_feature_once() {
        let mut b = task(
            "claude-sonnet-5-5",
            crate::ThinkingMode::Disabled,
            Some("high"),
            Some(array_schema()),
        );
        assert!(relax_after_400(
            &mut b,
            &api_err("fallbacks: unknown field")
        ));
        assert!(b.get("fallbacks").is_none());
        assert!(relax_after_400(
            &mut b,
            &api_err("\"thinking.type.between_tools\" is not supported")
        ));
        assert!(b.get("thinking").is_none());
        assert_eq!(b["output_config"]["effort"], "low");
        assert!(relax_after_400(
            &mut b,
            &api_err("output_config.format.schema: unsupported keyword")
        ));
        assert!(b.pointer("/output_config/format").is_none());
        assert!(relax_after_400(
            &mut b,
            &api_err("Output_Config.Effort is not supported")
        ));
        assert!(b.pointer("/output_config/effort").is_none());
        // Nothing left to drop → no retry loop.
        assert!(!relax_after_400(
            &mut b,
            &api_err("messages: field required")
        ));
        assert!(!relax_after_400(
            &mut b,
            &api_err("thinking.type still unhappy")
        ));
    }

    fn api_err(msg: &str) -> String {
        json!({"type": "error", "error": {"type": "invalid_request_error", "message": msg}})
            .to_string()
    }

    #[test]
    fn relax_only_reads_the_error_message() {
        let mut b = task(
            "claude-sonnet-5-5",
            crate::ThinkingMode::Disabled,
            None,
            Some(array_schema()),
        );
        // A non-JSON body, or keywords outside error.message (e.g. echoed
        // request text), never relaxes anything.
        assert!(!relax_after_400(
            &mut b,
            "format schema thinking. fallback effort"
        ));
        let echo = json!({"error": {"message": "messages.0: too long"}, "echo": "format schema"});
        assert!(!relax_after_400(&mut b, &echo.to_string()));
        assert!(b.pointer("/output_config/format").is_some());
    }

    #[test]
    fn legacy_thinking_relax_does_not_add_output_config() {
        let mut b = task(
            "claude-sonnet-4-5",
            crate::ThinkingMode::Disabled,
            None,
            None,
        );
        assert!(relax_after_400(&mut b, &api_err("thinking.type: invalid")));
        assert!(b.get("thinking").is_none());
        assert!(b.get("output_config").is_none());
    }

    #[test]
    fn refusal_is_an_error_even_with_partial_text() {
        let j = r#"{"content":[{"type":"text","text":"Sure, here"}],"stop_reason":"refusal","stop_details":{"type":"refusal","category":null}}"#;
        let e = parse_messages_response(j).unwrap_err().to_string();
        assert!(e.contains("declined") && e.contains("unspecified"), "{e}");
    }

    #[test]
    fn cut_off_json_is_an_error_only_when_structured() {
        let j = r#"{"content":[{"type":"text","text":"[{\"a\":1},"}],"stop_reason":"max_tokens"}"#;
        assert!(parse_messages_response_checked(j, true).is_err());
        // Free-form callers (Enhance's salvage parser, Ask) still get the text.
        assert_eq!(
            parse_messages_response_checked(j, false).unwrap(),
            "[{\"a\":1},"
        );
    }

    // ---- tool calling -------------------------------------------------

    use crate::conversation::ToolSpec;

    fn tool(name: &str, strict: bool) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("{name} tool"),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "filters": {"type": "object", "properties": {"since": {"type": "string"}}}
                },
                "required": ["query"]
            }),
            strict,
        }
    }

    fn tool_req(choice: ToolChoice, thinking: crate::ThinkingMode) -> ConverseRequest {
        ConverseRequest {
            messages: vec![
                Message::system("sys"),
                Message::user("What was the budget?"),
                Message::new(
                    Role::Assistant,
                    vec![
                        ContentBlock::Opaque {
                            provider: "anthropic".into(),
                            raw: json!({"type": "thinking", "thinking": "hm", "signature": "sig"}),
                        },
                        ContentBlock::Opaque {
                            provider: "ollama".into(),
                            raw: json!({"x": 1}),
                        },
                        ContentBlock::text(""),
                        ContentBlock::ToolUse {
                            id: "toolu_1".into(),
                            name: "search_passages".into(),
                            input: json!({"query": "budget"}),
                        },
                    ],
                ),
                Message::new(
                    Role::User,
                    vec![
                        ContentBlock::text("also check dates"),
                        ContentBlock::ToolResult {
                            tool_use_id: "toolu_1".into(),
                            content: "[seg:s1] $8,400".into(),
                            is_error: false,
                        },
                        ContentBlock::ToolResult {
                            tool_use_id: "toolu_2".into(),
                            content: "no such meeting".into(),
                            is_error: true,
                        },
                    ],
                ),
            ],
            tools: vec![tool("search_passages", true), tool("get_note", false)],
            tool_choice: choice,
            max_tokens: Some(2048),
            thinking,
            ..Default::default()
        }
    }

    fn body(model: &str, req: &ConverseRequest) -> serde_json::Value {
        AnthropicProvider::new("k".into(), model.into()).converse_body(req)
    }

    #[test]
    fn tool_body_shape_strict_and_blocks() {
        let b = body(
            "claude-sonnet-5-5",
            &tool_req(ToolChoice::Auto, crate::ThinkingMode::Default),
        );
        // Strict tool: closed schema, nested objects too. Non-strict: as given.
        assert_eq!(b["tools"][0]["strict"], true);
        assert_eq!(b["tools"][0]["input_schema"]["additionalProperties"], false);
        assert_eq!(
            b["tools"][0]["input_schema"]["properties"]["filters"]["additionalProperties"],
            false
        );
        assert!(b["tools"][1].get("strict").is_none());
        assert!(b["tools"][1]["input_schema"]
            .get("additionalProperties")
            .is_none());
        assert!(b.get("tool_choice").is_none(), "auto is the default");
        assert_eq!(b["system"][0]["text"], "sys");

        let msgs = b["messages"].as_array().unwrap();
        // Plain-text turn keeps the legacy string content.
        assert_eq!(msgs[0]["content"], "What was the budget?");
        // Assistant: anthropic thinking echoed first, ollama opaque and the
        // empty text dropped, then the tool_use.
        let a = msgs[1]["content"].as_array().unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0]["type"], "thinking");
        assert_eq!(a[0]["signature"], "sig");
        assert_eq!(
            a[1],
            json!({"type": "tool_use", "id": "toolu_1", "name": "search_passages", "input": {"query": "budget"}})
        );
        // User: tool results first, then the text; is_error only when set.
        let u = msgs[2]["content"].as_array().unwrap();
        assert_eq!(u[0]["type"], "tool_result");
        assert_eq!(u[0]["tool_use_id"], "toolu_1");
        assert!(u[0].get("is_error").is_none());
        assert_eq!(u[1]["is_error"], true);
        assert_eq!(u[2], json!({"type": "text", "text": "also check dates"}));
    }

    #[test]
    fn legacy_models_get_no_strict() {
        let b = body(
            "claude-sonnet-4-5",
            &tool_req(ToolChoice::Auto, crate::ThinkingMode::Default),
        );
        assert!(b["tools"][0].get("strict").is_none());
        assert!(b["tools"][0]["input_schema"]
            .get("additionalProperties")
            .is_none());
    }

    #[test]
    fn forced_tool_choice_only_where_accepted() {
        use crate::ThinkingMode::{Default as D, Disabled as Off};
        let choice = |model: &str, thinking| {
            body(model, &tool_req(ToolChoice::Required, thinking))
                .get("tool_choice")
                .cloned()
        };
        // 5.5 / Fable / Mythos reject forced choice → auto (omitted).
        for m in [
            "claude-sonnet-5-5",
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-mythos-5-1",
        ] {
            assert_eq!(choice(m, Off), None, "{m}");
        }
        // Haiku 5.5 with thinking really off → any; thinking default → auto.
        assert_eq!(
            choice("claude-haiku-5-5", Off),
            Some(json!({"type": "any"}))
        );
        assert_eq!(choice("claude-haiku-5-5", D), None);
        // Older models don't think unless asked → any.
        assert_eq!(choice("claude-sonnet-4-5", D), Some(json!({"type": "any"})));
        // None is always expressible.
        let b = body(
            "claude-sonnet-5-5",
            &tool_req(ToolChoice::None, crate::ThinkingMode::Default),
        );
        assert_eq!(b["tool_choice"], json!({"type": "none"}));
    }

    #[test]
    fn more_than_twenty_strict_tools_is_refused() {
        let p = AnthropicProvider::new("k".into(), "claude-sonnet-5-5".into());
        let mut req = tool_req(ToolChoice::Auto, crate::ThinkingMode::Default);
        req.tools = (0..21).map(|i| tool(&format!("t{i}"), true)).collect();
        assert!(matches!(p.validate(&req), Err(LlmError::InvalidRequest(_))));
        req.tools.truncate(20);
        assert!(p.validate(&req).is_ok());
        // Legacy models don't get strict at all, so no limit applies.
        let old = AnthropicProvider::new("k".into(), "claude-sonnet-4-5".into());
        req.tools = (0..25).map(|i| tool(&format!("t{i}"), true)).collect();
        assert!(old.validate(&req).is_ok());
    }

    #[test]
    fn relax_drops_rejected_tool_choice_and_strict() {
        let mut b = body(
            "claude-haiku-5-5",
            &tool_req(ToolChoice::Required, crate::ThinkingMode::Disabled),
        );
        assert!(b.get("tool_choice").is_some());
        assert!(relax_after_400(
            &mut b,
            &api_err("tool_choice.type: 'any' is not supported when thinking is enabled")
        ));
        assert!(b.get("tool_choice").is_none());
        // Thinking stays: the tool_choice branch ran first.
        assert!(b.get("thinking").is_some());
        assert!(relax_after_400(
            &mut b,
            &api_err("tools.0.strict: not supported for this model")
        ));
        assert!(b["tools"][0].get("strict").is_none());
        // A legacy body is untouched by a message that mentions strict.
        let mut legacy = task(
            "claude-sonnet-5-5",
            crate::ThinkingMode::Default,
            None,
            None,
        );
        let before = legacy.clone();
        assert!(!relax_after_400(
            &mut legacy,
            &api_err("strict mode rejected")
        ));
        assert_eq!(legacy, before);
    }

    #[test]
    fn parses_tool_use_thinking_and_usage() {
        let j = r#"{"content":[
            {"type":"thinking","thinking":"plan","signature":"s1"},
            {"type":"text","text":"Let me search."},
            {"type":"tool_use","id":"toolu_9","name":"search_passages","input":{"query":"q"}},
            {"type":"fallback","from":"x"}],
            "stop_reason":"tool_use",
            "usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":80,"cache_creation_input_tokens":0}}"#;
        let r = parse_messages_converse(j, false).unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.blocks.len(), 3);
        assert!(matches!(r.blocks[0], ContentBlock::Opaque { .. }));
        assert_eq!(r.text(), "Let me search.");
        assert_eq!(r.tool_uses()[0].0, "toolu_9");
        assert_eq!(r.usage.input_tokens, Some(100));
        assert_eq!(r.usage.cache_read_tokens, Some(80));
        // The opaque block round-trips into the next request unchanged.
        let next = AnthropicProvider::new("k".into(), "claude-sonnet-5-5".into()).converse_body(
            &ConverseRequest {
                messages: vec![Message::user("q"), r.to_message()],
                ..Default::default()
            },
        );
        assert_eq!(
            next["messages"][1]["content"][0],
            json!({"type":"thinking","thinking":"plan","signature":"s1"})
        );
    }

    #[test]
    fn converse_parse_keeps_refusal_and_truncation_rules() {
        let refusal =
            r#"{"content":[],"stop_reason":"refusal","stop_details":{"category":"cyber"}}"#;
        let e = parse_messages_converse(refusal, false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("declined") && e.contains("cyber"), "{e}");
        let empty = r#"{"content":[{"type":"thinking","thinking":"","signature":""}],"stop_reason":"max_tokens"}"#;
        assert!(parse_messages_converse(empty, false).is_err());
        // A cut-off answer with text is usable for prose…
        let cut = r#"{"content":[{"type":"text","text":"The bud"}],"stop_reason":"max_tokens"}"#;
        let r = parse_messages_converse(cut, false).unwrap();
        assert_eq!(r.stop_reason, StopReason::MaxTokens);
        // …but not for structured JSON.
        assert!(parse_messages_converse(cut, true).is_err());
    }

    fn decode_sse(text: &str) -> Result<ChatResponse> {
        let mut d = AnthropicSse::default();
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
    fn sse_stream_decodes_text_tools_and_thinking() {
        let text = r#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":42,"output_tokens":1,"cache_read_input_tokens":30}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me "}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"search."}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQ"}}
data: {"type":"content_block_stop","index":0}
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}
event: ping
data: {"type":"ping"}
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Searching"}}
data: {"type":"content_block_stop","index":1}
data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"search_passages","input":{}}}
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"query\": "}}
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"budget\"}"}}
data: {"type":"content_block_stop","index":2}
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":33}}
data: {"type":"message_stop"}"#;
        let r = decode_sse(text).unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(
            r.blocks[0],
            ContentBlock::Opaque {
                provider: "anthropic".into(),
                raw: json!({"type":"thinking","thinking":"Let me search.","signature":"EqQ"}),
            }
        );
        assert_eq!(r.text(), "Searching");
        assert_eq!(r.tool_uses()[0].2, &json!({"query": "budget"}));
        assert_eq!(r.usage.input_tokens, Some(42));
        assert_eq!(r.usage.output_tokens, Some(33));
        assert_eq!(r.usage.cache_read_tokens, Some(30));
    }

    #[test]
    fn sse_stream_errors_on_refusal_error_event_and_cut() {
        let refusal = r#"data: {"type":"message_delta","delta":{"stop_reason":"refusal"}}"#;
        assert!(decode_sse(refusal).is_err());
        let err =
            r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert!(decode_sse(err)
            .unwrap_err()
            .to_string()
            .contains("Overloaded"));
        let cut = r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Hi"}}"#;
        assert!(decode_sse(cut).is_err());
    }

    #[test]
    fn capabilities_follow_the_model() {
        let caps = |m: &str| AnthropicProvider::new("k".into(), m.into()).capabilities();
        let s55 = caps("claude-sonnet-5-5");
        assert!(s55.tools && s55.strict_tools && !s55.tool_choice_required);
        assert_eq!(s55.max_strict_tools, MAX_STRICT_TOOLS);
        let h55 = caps("claude-haiku-5-5");
        assert!(h55.tool_choice_required);
        let old = caps("claude-sonnet-4-5");
        assert!(old.tools && !old.strict_tools && old.max_strict_tools == 0);
    }

    /// Live: one tool round trip against the real API (each of the default
    /// models). Set ANTHROPIC_API_KEY; optional FLY_LLM_ANTHROPIC_MODELS
    /// (comma-separated). `relax_on_400` is off so a rejected request shape
    /// fails loudly instead of degrading.
    #[test]
    #[ignore = "needs ANTHROPIC_API_KEY"]
    fn anthropic_tool_round_trip_live() {
        let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
        let models = std::env::var("FLY_LLM_ANTHROPIC_MODELS")
            .unwrap_or_else(|_| format!("{ANTHROPIC_DEFAULT_MODEL},{ANTHROPIC_FAST_MODEL}"));
        let rt = tokio::runtime::Runtime::new().unwrap();
        for model in models.split(',').map(str::trim) {
            let mut p = AnthropicProvider::new(key.clone(), model.into());
            p.relax_on_400 = false;
            let mut req = ConverseRequest {
                messages: vec![
                    Message::system(
                        "Use search_passages before answering. Answer in one sentence.",
                    ),
                    Message::user("What budget did Dana agree to?"),
                ],
                tools: vec![tool("search_passages", true)],
                max_tokens: Some(2048),
                ..Default::default()
            };
            let first = rt.block_on(p.converse(req.clone())).expect(model);
            let (id, _, input) = first.tool_uses()[0];
            assert!(input.get("query").is_some(), "{model}: {input}");
            let id = id.to_string();
            req.messages.push(first.to_message());
            req.messages
                .push(Message::tool_results(vec![ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: "[seg:s1] Dana: We agreed on $8,400 for Q3.".into(),
                    is_error: false,
                }]));
            let second = rt
                .block_on(async { crate::stream::collect(p.chat_stream(req).await?).await })
                .expect(model);
            assert!(
                second.text().contains("8,400"),
                "{model}: {}",
                second.text()
            );
        }
    }
}
