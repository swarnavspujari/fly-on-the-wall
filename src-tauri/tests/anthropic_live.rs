//! Live regression check: the real AnthropicProvider path against
//! claude-sonnet-5. `#[ignore]`d (needs a network + ANTHROPIC_API_KEY).
//!
//! Proves the temperature-400 fix end-to-end: a request that asks for a
//! `temperature` (exactly like enhance_note / ask_meeting) must now SUCCEED
//! against claude-sonnet-5, because the provider omits the rejected param.
//!
//!   ANTHROPIC_API_KEY=sk-ant-... \
//!     cargo test -p fly-app --test anthropic_live -- --ignored --nocapture

use fly_llm::{ChatMessage, ChatRequest, LLMProvider, ThinkingMode};

#[test]
#[ignore = "hits the real Anthropic API; needs ANTHROPIC_API_KEY"]
fn temperature_request_succeeds_on_sonnet5() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        eprintln!("SKIP: set ANTHROPIC_API_KEY");
        return;
    };
    let provider = fly_llm::anthropic::AnthropicProvider::new(key, "claude-sonnet-5".to_string());

    // Mirror enhance_note exactly: an explicit temperature + default thinking.
    // Before the fix this returned HTTP 400 ("`temperature` is deprecated").
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let out = runtime
        .block_on(provider.chat(ChatRequest {
            messages: vec![
                ChatMessage::system("You are terse."),
                ChatMessage::user("Reply with exactly: ok"),
            ],
            temperature: Some(0.2),
            max_tokens: Some(1024),
            thinking: ThinkingMode::Default,
            format: None,
        }))
        .expect("chat with temperature must succeed on claude-sonnet-5 after the fix");

    eprintln!("sonnet-5 replied: {:?}", out.trim());
    assert!(!out.trim().is_empty(), "expected a non-empty reply");
}
