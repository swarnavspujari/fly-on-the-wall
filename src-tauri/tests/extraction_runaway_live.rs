//! Live check of the local-model runaway guard against a real Ollama.
//! `#[ignore]`d: needs Ollama with `llama3.1` (nothing else touched).
//!
//!   cargo test -p fly-app --test extraction_runaway_live -- --ignored --nocapture
//!
//! llama3.1 at temperature 0 with the extraction grammar sometimes falls into
//! a loop: the same two action items, again and again, the array never
//! closing, until the token cap. It depends on the input and on server state
//! (the prompt-prefix cache from the request before it changes the numbers
//! enough), so it can't be summoned on demand: it showed up for one of ten
//! one-line-removed variants of the invented kickoff meeting below, when they
//! ran back to back. The captured loop is replayed offline in
//! tests/extraction_runaway.rs; these tests check the same code against the
//! real server, whichever way the model goes: the reply is bounded, and when
//! it does loop it is cut off early with its items kept.

use std::time::{Duration, Instant};

use fly_app_lib::extraction::{
    build_extraction_prompt, extract_items, extraction_items_schema, local_output_budget,
    salvage_items, Runaway, RunawayGuard,
};
use fly_core::prompt_profile::profile_for;
use fly_core::{Attendee, Meeting, Speaker, Transcript, TranscriptSegment};
use fly_llm::openai_compat::OpenAiCompatProvider;
use fly_llm::{ChatMessage, ChatRequest, ConverseRequest, LLMProvider, StreamEvent, ThinkingMode};
use futures_util::StreamExt;

const LINES: [(&str, &str, &str); 9] = [
    ("9aa91bb8-bf72-417c-8792-0d6f8ca1e8bd", "spk_0", "Welcome everyone. This is the kickoff for Project Bluebird, our new customer portal."),
    ("e66efbe8-d3e6-492d-bff6-6a5e92dd231b", "spk_1", "Thanks Maya. I will start with the budget. We have approved $48,000 for the first phase."),
    ("8e46cf73-e9ac-4f6f-a0c9-487dd7eb2292", "spk_0", "Good. The launch date is March 14th. That date is fixed because of the conference."),
    ("16b7727e-c8cb-44a2-ba0c-06c609a8955d", "spk_1", "Understood. Who owns the security review?"),
    ("6c011bc1-9a89-4f2b-beee-6646629db557", "spk_0", "Priya Raman owns the security review. She will deliver the report by February 2nd."),
    ("2d2d1d17-f6ed-4f8b-a3f0-c6b7dae42064", "spk_1", "I will handle the vendor shortlist. I will compare three vendors and send a summary by next Friday."),
    ("557fb3d6-7e91-49ad-aad7-2c378abe688f", "spk_0", "One open question, do we need a second data center for the launch? Nobody knows yet."),
    ("ca0ad6d7-bf1a-42c8-a87b-e1de2ac1895e", "spk_1", "We decided to use Postgres for the main database. That decision is final."),
    ("e900f2cb-d3cc-4b8f-993e-5b61b4b056e5", "spk_0", "Great. Let us meet again in two weeks to review progress."),
];

/// The meeting with line `skip` taken out.
fn transcript_without(skip: usize) -> Transcript {
    Transcript {
        meeting_id: "m-live".into(),
        language: Some("en".into()),
        engine: "whisper.cpp".into(),
        segments: LINES
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != skip)
            .map(|(i, (id, key, text))| TranscriptSegment {
                id: (*id).into(),
                speaker_key: (*key).into(),
                start_ms: i as u64 * 6_000,
                end_ms: i as u64 * 6_000 + 5_000,
                text: (*text).into(),
                words: vec![],
            })
            .collect(),
        speakers: vec![
            Speaker {
                key: "spk_0".into(),
                label: "Speaker 1".into(),
            },
            Speaker {
                key: "spk_1".into(),
                label: "Speaker 2".into(),
            },
        ],
    }
}

fn meeting() -> Meeting {
    Meeting {
        id: "m-live".into(),
        title: "bluebird-kickoff".into(),
        note_id: "n-live".into(),
        attendees: Vec::<Attendee>::new(),
        attendees_confirmed: false,
        started_at: chrono::Utc::now(),
        ended_at: None,
        recording: None,
    }
}

/// The app's whole extraction path on the transcript that looped: finishes
/// well inside what an unguarded loop took, and gives items.
#[tokio::test]
#[ignore = "needs Ollama with llama3.1"]
async fn extraction_of_the_transcript_that_looped_finishes_with_items() {
    let provider = OpenAiCompatProvider::ollama(None, "llama3.1".into());
    let t = transcript_without(1);
    let started = Instant::now();
    let items = extract_items(&provider, &meeting(), &t)
        .await
        .expect("items");
    let took = started.elapsed();
    eprintln!(
        "{} items in {:.0}s (allowance {} tokens):",
        items.len(),
        took.as_secs_f64(),
        local_output_budget(
            t.segments.len(),
            t.segments
                .iter()
                .map(|s| s.text.split_whitespace().count())
                .sum(),
            8192
        )
    );
    for i in &items {
        eprintln!("  {}: {}", i.kind.as_str(), i.text);
    }
    // Normal replies take 60-100 s here; the loop alone ran past 200 s.
    assert!(
        took < Duration::from_secs(150),
        "took {took:?}: a loop wasn't cut off"
    );
    assert!(items.len() >= 3, "kept only {} items", items.len());
}

/// The one-line-removed variants back to back (the order that produced the
/// loop), each through the guard with the 3,500-token cap it was measured at.
/// Nothing may run past the guard's limits; if one loops, it must be cut.
#[tokio::test]
#[ignore = "needs Ollama with llama3.1; ten minutes or so"]
async fn no_variant_runs_past_the_guard() {
    let provider = OpenAiCompatProvider::ollama(None, "llama3.1".into());
    let profile = profile_for("llama3.1");
    let mut loops = 0;
    for skip in 0..LINES.len() {
        let t = transcript_without(skip);
        let prompt = build_extraction_prompt(&meeting(), &t, 0..t.segments.len(), profile);
        let request = ChatRequest {
            messages: vec![
                ChatMessage::system(prompt.system),
                ChatMessage::user(prompt.user),
            ],
            temperature: Some(0.0),
            max_tokens: Some(3500),
            thinking: ThinkingMode::Disabled,
            format: Some(extraction_items_schema()),
            effort: None,
        };
        let started = Instant::now();
        let mut stream = provider
            .chat_stream(ConverseRequest::from(request))
            .await
            .expect("stream");
        let mut guard = RunawayGuard::new(2 * t.segments.len() + 5);
        let (mut pieces, mut cut) = (0, None);
        while let Some(event) = stream.next().await {
            match event.expect("event") {
                StreamEvent::TextDelta { text } => {
                    pieces += 1;
                    if let Some(why) = guard.push(&text) {
                        cut = Some(why);
                        break;
                    }
                }
                StreamEvent::Done { .. } => break,
                _ => {}
            }
        }
        drop(stream);
        let kept = salvage_items(guard.text()).len();
        eprintln!(
            "without line {skip}: cut={cut:?} after {pieces} pieces / {:.0}s, {kept} items",
            started.elapsed().as_secs_f64()
        );
        if cut.is_some() {
            loops += 1;
            assert_eq!(cut, Some(Runaway::Repeating));
            assert!(kept >= 5, "kept only {kept} items");
        }
        // A normal reply is 500-850 pieces here. A loop the guard cuts runs
        // to the third exact repeat (580 and 1,185 pieces when measured,
        // against 3,500 uncut); the element limit (2 per line + 5) ends one
        // that varies its wording a little before that.
        assert!(pieces < 1600, "without line {skip}: {pieces} pieces");
    }
    eprintln!("{loops} of {} variants looped and were cut", LINES.len());
}
