//! Anthropic Claude provider (`POST /v1/messages`).

use serde_json::json;

use crate::{ChatRequest, LLMProvider, LlmError, Result, Role};

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

    /// Build the `/v1/messages` request body. Split out from `chat` so the
    /// parameter shaping (system out-of-band, optional temperature, thinking
    /// off) is unit-testable without a network round-trip.
    pub(crate) fn request_body(&self, req: &ChatRequest) -> serde_json::Value {
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
        body
    }

    async fn post(&self, body: &serde_json::Value) -> Result<(reqwest::StatusCode, String)> {
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
        let resp = request
            .json(body)
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
    fn close_objects(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                if map.contains_key("properties") && !map.contains_key("additionalProperties") {
                    map.insert("additionalProperties".into(), json!(false));
                }
                for child in map.values_mut() {
                    close_objects(child);
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(close_objects),
            _ => {}
        }
    }
    let mut s = schema.clone();
    close_objects(&mut s);
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

        let (mut status, mut text) = self.post(&body).await?;
        for _ in 0..4 {
            if !self.relax_on_400 || status.as_u16() != 400 || !relax_after_400(&mut body, &text) {
                break;
            }
            (status, text) = self.post(&body).await?;
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
}
