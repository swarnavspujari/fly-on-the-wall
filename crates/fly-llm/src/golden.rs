//! Byte-identity guard for the legacy `chat()` request bodies.
//!
//! `tests/golden/legacy_bodies.jsonl` was generated from the request
//! builders BEFORE content blocks and tools were added (bless with
//! `FLY_LLM_BLESS=1 cargo test -p fly-llm golden`). Every existing call
//! shape — Ask, Enhance, polish, extraction, test-connection — must keep
//! serializing to exactly those bytes for every provider, so the tool work
//! stays dormant for today's features.

use serde_json::json;

use crate::anthropic::AnthropicProvider;
use crate::openai_compat::{estimate_num_ctx, native_chat_body, openai_chat_body};
use crate::{ChatMessage, ChatRequest, ThinkingMode};

const GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/golden/legacy_bodies.jsonl"
);

fn array_schema() -> serde_json::Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "text": { "type": "string" },
                "source_ids": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["text", "source_ids"]
        }
    })
}

fn object_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": { "items": { "type": "array", "items": { "type": "string" } } },
        "required": ["items"]
    })
}

/// Request shapes mirroring the app's call sites (llm_commands.rs,
/// extraction.rs, test_connection).
fn requests() -> Vec<(&'static str, ChatRequest)> {
    let transcript = "TRANSCRIPT:\nDana: We agreed on the Q3 budget of eighty four hundred.\n"
        .repeat(40);
    let ask_messages = vec![
        ChatMessage::system(format!("You answer questions about a meeting.\n\n{transcript}")),
        ChatMessage::user("What did we decide?"),
        ChatMessage::assistant("You decided on the Q3 budget."),
        ChatMessage::user("How much was it?"),
    ];
    let pair = |s: &str, u: &str| vec![ChatMessage::system(s), ChatMessage::user(u)];
    vec![
        (
            "ask",
            ChatRequest {
                effort: Some("medium".into()),
                messages: ask_messages.clone(),
                temperature: Some(0.3),
                max_tokens: Some(2048),
                thinking: ThinkingMode::Default,
                format: None,
            },
        ),
        (
            "ask_thinking_off",
            ChatRequest {
                effort: None,
                messages: ask_messages,
                temperature: Some(0.3),
                max_tokens: Some(2048),
                thinking: ThinkingMode::Disabled,
                format: None,
            },
        ),
        (
            "enhance",
            ChatRequest {
                effort: Some("high".into()),
                messages: pair("Write enhanced notes.", "Segments:\n[0] Dana: hi"),
                temperature: Some(0.2),
                max_tokens: Some(4096),
                thinking: ThinkingMode::Default,
                format: None,
            },
        ),
        (
            "enhance_schema",
            ChatRequest {
                effort: None,
                messages: pair("Write enhanced notes.", "Segments:\n[0] Dana: hi"),
                temperature: Some(0.0),
                max_tokens: Some(16384),
                thinking: ThinkingMode::Default,
                format: Some(array_schema()),
            },
        ),
        (
            "polish",
            ChatRequest {
                effort: Some("low".into()),
                messages: pair("Clean each segment.", "[{\"id\":\"s1\",\"text\":\"um hi\"}]"),
                temperature: None,
                max_tokens: Some(8192),
                thinking: ThinkingMode::Disabled,
                format: Some(array_schema()),
            },
        ),
        (
            "extraction",
            ChatRequest {
                effort: Some("low".into()),
                messages: pair("Extract items.", "[s1] Dana: I'll send it Friday."),
                temperature: Some(0.0),
                max_tokens: Some(4096),
                thinking: ThinkingMode::Disabled,
                format: Some(object_schema()),
            },
        ),
        (
            "test_connection",
            ChatRequest {
                effort: None,
                messages: vec![ChatMessage::user("Reply with the single word: ok")],
                temperature: Some(0.0),
                max_tokens: Some(256),
                thinking: ThinkingMode::Disabled,
                format: None,
            },
        ),
    ]
}

/// Every (case, serialized body) pair the legacy builders produce.
fn bodies() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let anthropic_models = [
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
        "claude-opus-5-5",
        "claude-fable-5-1",
        "claude-sonnet-4-5",
        "claude-3-5-haiku-latest",
    ];
    for (name, req) in requests() {
        for model in anthropic_models {
            let p = AnthropicProvider::new("k".into(), model.into());
            out.push((
                format!("anthropic/{model}/{name}"),
                p.request_body(&req).to_string(),
            ));
        }
        let mut gw = AnthropicProvider::new("k".into(), "claude-sonnet-5-5".into());
        gw.base_url = "https://gateway.example".into();
        out.push((
            format!("anthropic-gateway/{name}"),
            gw.request_body(&req).to_string(),
        ));
        for model in ["gpt-4o-mini", "meta/llama-3.1-8b-instruct"] {
            out.push((
                format!("openai/{model}/{name}"),
                openai_chat_body(model, &req).to_string(),
            ));
        }
        for model in ["llama3.1", "qwen3.5:4b"] {
            let think = req.thinking == ThinkingMode::Disabled;
            let ctx = estimate_num_ctx(&req);
            for (variant, include_think, num_ctx) in [
                ("first", think, Some(ctx)),
                ("no_think_retry", false, Some(ctx)),
                ("no_ctx_retry", think, None),
            ] {
                out.push((
                    format!("ollama/{model}/{name}/{variant}"),
                    native_chat_body(model, &req, include_think, num_ctx).to_string(),
                ));
            }
        }
    }
    out
}

#[test]
fn golden_legacy_request_bodies_are_byte_identical() {
    let got = bodies();
    if std::env::var_os("FLY_LLM_BLESS").is_some() {
        let lines: Vec<String> = got
            .iter()
            .map(|(case, body)| json!({ "case": case, "body": body }).to_string())
            .collect();
        std::fs::create_dir_all(std::path::Path::new(GOLDEN_PATH).parent().unwrap()).unwrap();
        std::fs::write(GOLDEN_PATH, lines.join("\n") + "\n").unwrap();
        return;
    }
    let text = std::fs::read_to_string(GOLDEN_PATH).expect("golden file present");
    let want: Vec<(String, String)> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            (
                v["case"].as_str().unwrap().to_string(),
                v["body"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(got.len(), want.len(), "case count changed");
    for ((gc, gb), (wc, wb)) in got.iter().zip(&want) {
        assert_eq!(gc, wc, "case order changed");
        assert_eq!(gb, wb, "request body changed for {gc}");
    }
}
