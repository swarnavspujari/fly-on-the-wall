//! Live checks of the real AnthropicProvider against the Claude 5.5 models,
//! using the app's own per-model profiles, schemas and request shapes.
//! `#[ignore]`d: they hit the real API (a few cents) and need a key.
//!
//! The provider's 400-fallback is switched OFF here, so a request shape the
//! API rejects (thinking mode, effort, structured outputs, fallbacks) fails
//! the test instead of silently degrading.
//!
//! PowerShell:
//!   $env:ANTHROPIC_API_KEY = "sk-ant-..."
//!   cargo test -p fly-app --test anthropic_live -- --ignored --nocapture --test-threads 1
//!
//! Optional: FOTW_LIVE_MODELS="claude-sonnet-5-5,claude-haiku-5-5" (default both).

use fly_core::prompt_profile::profile_for;
use fly_llm::anthropic::AnthropicProvider;
use fly_llm::{ChatMessage, ChatRequest, LLMProvider, ThinkingMode};

fn key() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
}

fn models() -> Vec<String> {
    std::env::var("FOTW_LIVE_MODELS")
        .unwrap_or_else(|_| "claude-sonnet-5-5,claude-haiku-5-5".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn strict(key: &str, model: &str) -> AnthropicProvider {
    let mut p = AnthropicProvider::new(key.to_string(), model.to_string());
    p.relax_on_400 = false;
    p
}

fn run(p: &AnthropicProvider, req: ChatRequest) -> Result<String, String> {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(p.chat(req))
        .map_err(|e| e.to_string())
}

const TRANSCRIPT: &str = "\
[seg001] Dana (spk_0): ok so we agreed to ship the beta on friday the 14th
[seg002] Lee (spk_1): yeah and i'll send the release notes to marketing by thursday
[seg003] Dana (spk_0): budget for the launch is fifty thousand dollars
[seg004] Lee (spk_1): open question is whether we support windows 10 at all";

/// Ask: adaptive thinking at the profile's effort, generous budget.
#[test]
#[ignore = "hits the real Anthropic API; needs ANTHROPIC_API_KEY"]
fn ask_shape_answers() {
    let Some(key) = key() else {
        return eprintln!("SKIP: set ANTHROPIC_API_KEY");
    };
    for model in models() {
        let profile = profile_for(&model);
        let out = run(
            &strict(&key, &model),
            ChatRequest {
                messages: vec![
                    ChatMessage::system(format!(
                        "Answer about the meeting below.\n<meeting_transcript>\n{TRANSCRIPT}\n</meeting_transcript>"
                    )),
                    ChatMessage::user("What budget was mentioned?"),
                ],
                temperature: Some(0.3),
                max_tokens: Some(profile.max_tokens.ask.unwrap_or(2048)),
                thinking: ThinkingMode::Default,
                format: None,
                effort: profile.effort.ask.map(String::from),
            },
        )
        .unwrap_or_else(|e| panic!("{model} ask failed: {e}"));
        eprintln!("{model} ask → {}", out.trim());
        assert!(
            out.contains("50") || out.to_lowercase().contains("fifty"),
            "{model}: {out}"
        );
    }
}

/// Extraction: thinking off in each model's accepted shape + structured
/// outputs with the app's real (array-rooted) schema.
#[test]
#[ignore = "hits the real Anthropic API; needs ANTHROPIC_API_KEY"]
fn extraction_shape_returns_schema_valid_json() {
    let Some(key) = key() else {
        return eprintln!("SKIP: set ANTHROPIC_API_KEY");
    };
    for model in models() {
        let profile = profile_for(&model);
        assert!(
            profile.constrained_json,
            "{model} should use structured outputs"
        );
        let out = run(
            &strict(&key, &model),
            ChatRequest {
                messages: vec![
                    ChatMessage::system(
                        "Extract decisions, action items, open questions and figures from the \
                         transcript as JSON. Cite segment ids in [brackets].",
                    ),
                    ChatMessage::user(format!("<transcript>\n{TRANSCRIPT}\n</transcript>")),
                ],
                temperature: Some(0.0),
                max_tokens: Some(profile.max_tokens.extract.unwrap_or(8192)),
                thinking: ThinkingMode::Disabled,
                format: Some(fly_app_lib::extraction::extraction_items_schema()),
                effort: profile.effort.extract.map(String::from),
            },
        )
        .unwrap_or_else(|e| panic!("{model} extraction failed: {e}"));
        let v: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("{model}: not JSON ({e}): {out}"));
        let items = v
            .as_array()
            .unwrap_or_else(|| panic!("{model}: not an array: {out}"));
        eprintln!("{model} extraction → {} items", items.len());
        assert!(!items.is_empty(), "{model}: expected items");
        assert!(items
            .iter()
            .all(|i| i.get("kind").is_some() && i.get("segment_ids").is_some()));
    }
}

/// Polish: the cleanup schema (object-rooted) with thinking off.
#[test]
#[ignore = "hits the real Anthropic API; needs ANTHROPIC_API_KEY"]
fn polish_shape_returns_schema_valid_json() {
    let Some(key) = key() else {
        return eprintln!("SKIP: set ANTHROPIC_API_KEY");
    };
    for model in models() {
        let profile = profile_for(&model);
        let out = run(
            &strict(&key, &model),
            ChatRequest {
                messages: vec![
                    ChatMessage::system(fly_core::enhance::CLEANUP_SYSTEM_PROMPT),
                    ChatMessage::user(
                        "<segments>\n[{\"id\":\"t1\",\"speaker\":\"mic\",\"start\":0,\"text\":\"um so we we should ship friday\"}]\n</segments>",
                    ),
                ],
                temperature: Some(0.0),
                max_tokens: Some(profile.max_tokens.polish.unwrap_or(8192)),
                thinking: ThinkingMode::Disabled,
                format: Some(fly_core::enhance::cleanup_response_schema()),
                effort: profile.effort.polish.map(String::from),
            },
        )
        .unwrap_or_else(|e| panic!("{model} polish failed: {e}"));
        let v: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("{model}: not JSON ({e}): {out}"));
        eprintln!("{model} polish → {v}");
        assert_eq!(v["segments"][0]["id"], "t1", "{model}: {out}");
    }
}

/// Enhance: adaptive thinking at medium effort, free-form JSON blocks.
#[test]
#[ignore = "hits the real Anthropic API; needs ANTHROPIC_API_KEY"]
fn enhance_shape_returns_blocks() {
    let Some(key) = key() else {
        return eprintln!("SKIP: set ANTHROPIC_API_KEY");
    };
    for model in models() {
        let profile = profile_for(&model);
        let out = run(
            &strict(&key, &model),
            ChatRequest {
                messages: vec![
                    ChatMessage::system(
                        "Respond with only a JSON array of {\"type\":\"user\"|\"ai\",\"markdown\":\"...\",\"sources\":[numbers]}.",
                    ),
                    ChatMessage::user(format!(
                        "TRANSCRIPT (numbered segments):\n<transcript>\n[0] {TRANSCRIPT}\n</transcript>\n\nMY NOTES:\n<my_notes>\n- launch\n</my_notes>"
                    )),
                ],
                temperature: Some(0.2),
                max_tokens: Some(8000),
                thinking: ThinkingMode::Default,
                format: None,
                effort: profile.effort.enhance.map(String::from),
            },
        )
        .unwrap_or_else(|e| panic!("{model} enhance failed: {e}"));
        eprintln!("{model} enhance → {} chars", out.len());
        assert!(out.contains("\"markdown\""), "{model}: {out}");
    }
}

/// Settings → Test connection path.
#[test]
#[ignore = "hits the real Anthropic API; needs ANTHROPIC_API_KEY"]
fn test_connection_succeeds() {
    let Some(key) = key() else {
        return eprintln!("SKIP: set ANTHROPIC_API_KEY");
    };
    for model in models() {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(strict(&key, &model).test_connection())
            .unwrap_or_else(|e| panic!("{model} test_connection failed: {e}"));
        eprintln!("{model} test_connection ok");
    }
}
