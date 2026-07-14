//! Offline integration test: the transcript-polish flow end-to-end with a
//! deterministic MockLLMProvider — raw transcript → cleanup prompt → canned
//! LLM response → id-keyed mapping + drop-content guard → cleaned variant
//! stored ALONGSIDE the raw one. Runs in CI: no network, no models.
//!
//! Proves the two contracts the feature rests on:
//!   1. segment ids / speaker keys / timestamps / words survive the polish
//!      exactly (so the Enhanced doc's segment-id citations keep resolving);
//!   2. a lossy response trips the guard — the raw segment is kept and flagged,
//!      never overwritten with a shortened/hallucinated version.

use std::collections::HashMap;

use fly_core::{enhance, RecordingRef, Speaker, Transcript, TranscriptSegment, Word};
use fly_llm::mock::MockLLMProvider;
use fly_llm::{ChatMessage, ChatRequest, LLMProvider};
use fly_storage::Storage;

fn raw_transcript(meeting_id: &str) -> Transcript {
    Transcript {
        meeting_id: meeting_id.into(),
        language: Some("en".into()),
        engine: "whisper.cpp".into(),
        segments: vec![
            TranscriptSegment {
                id: "t1".into(),
                speaker_key: "mic".into(),
                start_ms: 16_000,
                end_ms: 20_000,
                text: "yeah it's because of the recording I I think Swarnoff uh Dubay".into(),
                words: vec![Word {
                    text: "recording".into(),
                    start_ms: 16_500,
                    end_ms: 17_000,
                }],
            },
            TranscriptSegment {
                id: "t2".into(),
                speaker_key: "spk_0".into(),
                start_ms: 20_000,
                end_ms: 26_000,
                text: "the the the budget is approved for fifty thousand dollars this quarter"
                    .into(),
                words: vec![],
            },
        ],
        speakers: vec![
            Speaker {
                key: "mic".into(),
                label: "You".into(),
            },
            Speaker {
                key: "spk_0".into(),
                label: "Dana".into(),
            },
        ],
    }
}

fn seed(storage: &Storage) -> String {
    let note = storage.create_note("Polish sync", None).unwrap();
    let meeting = storage
        .create_meeting("Polish sync", &note.id, &[])
        .unwrap();
    storage
        .end_meeting(
            &meeting.id,
            &RecordingRef {
                mic_path: None,
                system_path: None,
                mixed_path: Some("recordings/x/mixed.wav".into()),
                playback_path: None,
                duration_ms: 60_000,
            },
        )
        .unwrap();
    storage
        .save_transcript(&raw_transcript(&meeting.id))
        .unwrap();
    meeting.id
}

/// Drive the real pieces of the command by hand with a canned response.
fn run_polish(storage: &Storage, meeting_id: &str, canned: &str) -> enhance::PolishOutcome {
    let raw = storage.get_transcript(meeting_id).unwrap().unwrap();

    // build_cleanup_prompt sends the runtime contract verbatim + the segments.
    let prompt =
        enhance::build_cleanup_prompt(&raw.segments, &fly_core::prompt_profile::DEFAULT_PROFILE);
    assert_eq!(prompt.system, enhance::CLEANUP_SYSTEM_PROMPT);
    assert!(prompt.user.contains(r#""id":"t1""#));
    assert!(prompt.user.contains(r#""id":"t2""#));
    assert_eq!(prompt.segment_ids, vec!["t1", "t2"]);

    let provider = MockLLMProvider::with_response(canned);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let output = runtime
        .block_on(provider.chat(ChatRequest {
            messages: vec![
                ChatMessage::system(prompt.system),
                ChatMessage::user(prompt.user),
            ],
            temperature: None,
            max_tokens: Some(8192),
            thinking: fly_llm::ThinkingMode::Disabled,
            format: None,
        }))
        .unwrap();

    let cleaned_map: HashMap<String, String> = enhance::parse_cleanup_response(&output)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let outcome = enhance::apply_cleanup(&raw, &cleaned_map);

    // The persist path refuses anything that drifted from the raw structure.
    assert!(enhance::preserves_provenance(&raw, &outcome.transcript));
    storage
        .save_cleaned_transcript(&outcome.transcript)
        .unwrap();
    outcome
}

#[test]
fn polish_preserves_ids_speakers_timestamps_offline() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    let meeting_id = seed(&storage);
    let raw = storage.get_transcript(&meeting_id).unwrap().unwrap();

    // A faithful cleanup: disfluencies removed, proper noun corrected, no loss.
    let canned = r#"{"segments":[
        {"id":"t1","text":"Yeah, it's because of the recording, I think. Swarnav Pujari."},
        {"id":"t2","text":"The budget is approved for fifty thousand dollars this quarter."}
    ]}"#;
    let outcome = run_polish(&storage, &meeting_id, canned);

    assert_eq!(outcome.segments_cleaned, 2);
    assert_eq!(outcome.segments_kept_raw, 0);
    assert!(outcome.flags.is_empty());

    // Cleaned variant is stored alongside raw; raw is untouched.
    let cleaned = storage
        .get_cleaned_transcript(&meeting_id)
        .unwrap()
        .unwrap();
    let raw_again = storage.get_transcript(&meeting_id).unwrap().unwrap();
    assert_eq!(raw_again.segments[0].text, raw.segments[0].text);

    // Text was actually polished ("Swarnoff … Dubay" → "Swarnav Pujari").
    assert!(cleaned.segments[0].text.contains("Swarnav Pujari"));
    assert!(!cleaned.segments[0].text.contains("Swarnoff"));

    // Contract: ids / speaker keys / timestamps / words identical to raw.
    for (r, c) in raw.segments.iter().zip(&cleaned.segments) {
        assert_eq!(r.id, c.id);
        assert_eq!(r.speaker_key, c.speaker_key);
        assert_eq!(r.start_ms, c.start_ms);
        assert_eq!(r.end_ms, c.end_ms);
        assert_eq!(r.words, c.words);
    }
    assert_eq!(raw.speakers, cleaned.speakers);
}

#[test]
fn polish_guard_trips_on_lossy_response_offline() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    let meeting_id = seed(&storage);
    let raw = storage.get_transcript(&meeting_id).unwrap().unwrap();

    // A lossy/hallucinated response: t2 (11 raw words) is "cleaned" down to a
    // 2-word summary. The guard must reject it, keep the raw text, and flag it.
    let canned = r#"{"segments":[
        {"id":"t1","text":"Yeah, it's because of the recording, I think. Swarnav Pujari."},
        {"id":"t2","text":"Budget approved."}
    ]}"#;
    let outcome = run_polish(&storage, &meeting_id, canned);

    assert_eq!(outcome.segments_cleaned, 1);
    assert_eq!(outcome.segments_kept_raw, 1);
    assert_eq!(outcome.flags.len(), 1);
    assert_eq!(outcome.flags[0].segment_id, "t2");

    // The persisted cleaned variant kept t2's RAW words, not the 2-word summary.
    let cleaned = storage
        .get_cleaned_transcript(&meeting_id)
        .unwrap()
        .unwrap();
    assert_eq!(cleaned.segments[1].text, raw.segments[1].text);
    assert_ne!(cleaned.segments[1].text, "Budget approved.");
}
