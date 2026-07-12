//! Offline polish harness for real transcripts: runs a stored raw
//! transcript.json through the transcript-polish pass against a REAL LLM
//! provider and writes the cleaned variant + a scorecard. `#[ignore]`d: needs
//! a transcript on disk and an API key.
//!
//! It exercises the exact code the `polish_transcript` command uses
//! (`enhance::plan_cleanup_batches` → `build_cleanup_prompt` → provider.chat →
//! `parse_cleanup_response` → `apply_cleanup`), so it validates the shipped
//! path, not a parallel reimplementation. It asserts the provenance contract
//! (ids/speakers/timestamps identical) before writing anything.
//!
//!   POLISH_HARNESS_JSON=path\to\transcript.json \       # raw input (required)
//!   POLISH_HARNESS_OUT=path\to\cleaned.json \           # cleaned output (optional)
//!   POLISH_HARNESS_PROVIDER=anthropic \                 # anthropic | ollama (default anthropic)
//!   POLISH_HARNESS_MODEL=claude-sonnet-5 \              # optional model override
//!   ANTHROPIC_API_KEY=sk-ant-... \                      # required for anthropic
//!     cargo test -p looma-app --test polish_harness -- --ignored --nocapture

use std::collections::HashMap;

use looma_core::{enhance, Transcript};
use looma_llm::{ChatMessage, ChatRequest, LLMProvider};

const MAX_BATCH_WORDS: usize = 1200;
const MAX_BATCH_SEGMENTS: usize = 40;

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

fn total_words(t: &Transcript) -> usize {
    t.segments.iter().map(|s| word_count(&s.text)).sum()
}

fn build_provider() -> Box<dyn LLMProvider> {
    let which = std::env::var("POLISH_HARNESS_PROVIDER").unwrap_or_else(|_| "anthropic".into());
    match which.as_str() {
        "ollama" => {
            let model = std::env::var("POLISH_HARNESS_MODEL").unwrap_or_else(|_| "llama3.1".into());
            Box::new(looma_llm::openai_compat::OpenAiCompatProvider::ollama(
                None, model,
            ))
        }
        _ => {
            let key = std::env::var("ANTHROPIC_API_KEY")
                .expect("ANTHROPIC_API_KEY must be set for the anthropic provider");
            let model = std::env::var("POLISH_HARNESS_MODEL")
                .unwrap_or_else(|_| looma_llm::anthropic::ANTHROPIC_DEFAULT_MODEL.into());
            Box::new(looma_llm::anthropic::AnthropicProvider::new(key, model))
        }
    }
}

/// One provider call with a small retry for transient failures.
async fn chat_with_retry(provider: &dyn LLMProvider, req: ChatRequest) -> String {
    let mut last = String::new();
    for attempt in 1..=4 {
        match provider.chat(req.clone()).await {
            Ok(out) => return out,
            Err(e) => {
                last = e.to_string();
                eprintln!("  chat error (attempt {attempt}/4): {last}");
                tokio::time::sleep(std::time::Duration::from_secs(5 * attempt)).await;
            }
        }
    }
    panic!("provider.chat failed after retries: {last}");
}

#[test]
#[ignore = "offline polish harness; needs a transcript.json + an API key, see file docs"]
fn polish_harness() {
    let json_path = match std::env::var("POLISH_HARNESS_JSON") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("SKIP: set POLISH_HARNESS_JSON=path\\to\\transcript.json");
            return;
        }
    };
    let raw: Transcript = serde_json::from_str(
        &std::fs::read_to_string(&json_path).expect("read raw transcript.json"),
    )
    .expect("parse raw transcript json");
    assert!(!raw.segments.is_empty(), "raw transcript has no segments");

    let provider = build_provider();
    let batches = enhance::plan_cleanup_batches(&raw.segments, MAX_BATCH_WORDS, MAX_BATCH_SEGMENTS);
    eprintln!(
        "== polishing {} segments ({} words) over {} batch(es) via {} ==",
        raw.segments.len(),
        total_words(&raw),
        batches.len(),
        provider.id()
    );

    // Diagnostic: when set, dump every batch's raw provider response so a
    // parse failure can be inspected (evidence before any parser change).
    let dump_dir = std::env::var("POLISH_HARNESS_DUMP_DIR").ok();
    if let Some(d) = &dump_dir {
        std::fs::create_dir_all(d).ok();
    }

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let cleaned_map: HashMap<String, String> = runtime.block_on(async {
        let mut map = HashMap::new();
        for (bi, range) in batches.iter().enumerate() {
            let batch = &raw.segments[range.clone()];
            let prompt = enhance::build_cleanup_prompt(batch);
            let output = chat_with_retry(
                provider.as_ref(),
                ChatRequest {
                    messages: vec![
                        ChatMessage::system(prompt.system),
                        ChatMessage::user(prompt.user),
                    ],
                    // Omitted: claude-sonnet-5 rejects an explicit temperature.
                    temperature: None,
                    max_tokens: Some(8192),
                    // Mechanical cleanup: no thinking, so the full budget goes
                    // to the JSON answer (see AnthropicProvider::request_body).
                    thinking: looma_llm::ThinkingMode::Disabled,
                },
            )
            .await;
            if let Some(d) = &dump_dir {
                std::fs::write(format!("{d}/batch_{:02}.txt", bi + 1), &output).ok();
            }
            let pairs = enhance::parse_cleanup_response(&output).unwrap_or_default();
            eprintln!(
                "  batch {}/{}: {} segments → {} cleaned pairs parsed{}",
                bi + 1,
                batches.len(),
                batch.len(),
                pairs.len(),
                if pairs.is_empty() {
                    format!(
                        "  ⚠ PARSE EMPTY (len={}, head={:?}, tail={:?})",
                        output.len(),
                        output.chars().take(120).collect::<String>(),
                        output.chars().rev().take(120).collect::<String>().chars().rev().collect::<String>(),
                    )
                } else {
                    String::new()
                }
            );
            for (id, text) in pairs {
                map.insert(id, text);
            }
        }
        map
    });

    let outcome = enhance::apply_cleanup(&raw, &cleaned_map);

    // --- provenance contract: hard-assert before persisting anything ---
    assert!(
        enhance::preserves_provenance(&raw, &outcome.transcript),
        "PROVENANCE VIOLATION: cleaned transcript drifted from raw structure"
    );
    for (r, c) in raw.segments.iter().zip(&outcome.transcript.segments) {
        assert_eq!(r.id, c.id, "segment id changed");
        assert_eq!(r.speaker_key, c.speaker_key, "speaker key changed");
        assert_eq!(r.start_ms, c.start_ms, "start_ms changed");
        assert_eq!(r.end_ms, c.end_ms, "end_ms changed");
    }

    // --- scorecard ---
    let raw_words = total_words(&raw);
    let cleaned_words = total_words(&outcome.transcript);
    eprintln!("== polish scorecard ==");
    eprintln!(
        "segments: total={} cleaned={} kept_raw={}",
        raw.segments.len(),
        outcome.segments_cleaned,
        outcome.segments_kept_raw
    );
    eprintln!(
        "words: raw={} polished={} retention={:.1}%",
        raw_words,
        cleaned_words,
        cleaned_words as f64 / raw_words.max(1) as f64 * 100.0
    );
    eprintln!("guard flags (segments the guard kept raw): {}", outcome.flags.len());
    for f in &outcome.flags {
        eprintln!(
            "  FLAG {} [{}]: {} → {} chars — {}",
            f.segment_id, f.speaker_key, f.raw_chars, f.cleaned_chars, f.reason
        );
    }

    // a few before/after examples for eyeballing quality
    eprintln!("== sample segments (raw → polished) ==");
    for (i, (r, c)) in raw
        .segments
        .iter()
        .zip(&outcome.transcript.segments)
        .enumerate()
        .take(4)
    {
        eprintln!("  [{i}] {} @{}s", r.speaker_key, r.start_ms / 1000);
        eprintln!("    raw : {}", r.text.chars().take(160).collect::<String>());
        eprintln!("    poli: {}", c.text.chars().take(160).collect::<String>());
    }

    let metrics = serde_json::json!({
        "segments_total": raw.segments.len(),
        "segments_cleaned": outcome.segments_cleaned,
        "segments_kept_raw": outcome.segments_kept_raw,
        "words_raw": raw_words,
        "words_polished": cleaned_words,
        "word_retention": cleaned_words as f64 / raw_words.max(1) as f64,
        "guard_flags": outcome.flags.len(),
    });
    eprintln!("POLISH_HARNESS_METRICS_JSON: {metrics}");

    if let Ok(out) = std::env::var("POLISH_HARNESS_OUT") {
        std::fs::write(
            &out,
            serde_json::to_string_pretty(&outcome.transcript).unwrap(),
        )
        .unwrap();
        eprintln!("cleaned transcript written to {out}");
    }
}
