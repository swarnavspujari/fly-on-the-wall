//! Offline integration test (spec §11): the enhance flow end-to-end with a
//! deterministic MockLLMProvider — note + transcript → prompt → canned LLM
//! blocks → provenance-tagged storage → zoom-in mapping → reclaim-on-edit.
//! Runs in CI: no network, no models.

use looma_core::{enhance, BlockOrigin, RecordingRef, Speaker, Transcript, TranscriptSegment};
use looma_llm::mock::MockLLMProvider;
use looma_llm::{ChatMessage, ChatRequest, LLMProvider};
use looma_storage::Storage;

#[test]
fn enhance_flow_with_mock_provider_offline() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();

    // --- a note with scratchpad + a transcribed meeting ---
    let note = storage.create_note("Budget sync", None).unwrap();
    storage
        .update_note_scratchpad(
            &note.id,
            "- marketing wants more $$\n- ask dana re forecast",
        )
        .unwrap();
    let meeting = storage
        .create_meeting("Budget sync", &note.id, &[])
        .unwrap();
    storage
        .end_meeting(
            &meeting.id,
            &RecordingRef {
                mic_path: None,
                system_path: None,
                mixed_path: Some("recordings/x/mixed.wav".into()),
                duration_ms: 60_000,
            },
        )
        .unwrap();
    storage
        .save_transcript(&Transcript {
            meeting_id: meeting.id.clone(),
            language: Some("en".into()),
            engine: "whisper.cpp".into(),
            segments: vec![
                TranscriptSegment {
                    id: "seg-real-1".into(),
                    speaker_key: "mic".into(),
                    start_ms: 0,
                    end_ms: 4_000,
                    text: "marketing needs another fifty thousand dollars".into(),
                    words: vec![],
                },
                TranscriptSegment {
                    id: "seg-real-2".into(),
                    speaker_key: "spk_0".into(),
                    start_ms: 4_000,
                    end_ms: 8_000,
                    text: "I will share the forecast tomorrow".into(),
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
        })
        .unwrap();

    // --- build prompt exactly like the enhance command does ---
    let loaded_note = storage.get_note(&note.id).unwrap();
    let template = storage.get_template("tpl-general").unwrap();
    let transcript = storage.get_transcript(&meeting.id).unwrap();
    let prompt = enhance::build_enhance_prompt(&loaded_note, transcript.as_ref(), &template);
    assert!(prompt.user.contains("[0] You: marketing needs"));
    assert!(prompt.user.contains("[1] Dana: I will share"));

    // --- deterministic provider returns the block JSON ---
    let provider = MockLLMProvider::with_response(
        r#"[
            {"type": "user", "markdown": "- marketing wants more $$", "sources": []},
            {"type": "ai", "markdown": "**Decision:** approve extra $50k for marketing", "sources": [0]},
            {"type": "ai", "markdown": "**Action:** Dana shares forecast tomorrow", "sources": [1]}
        ]"#,
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let output = runtime
        .block_on(provider.chat(ChatRequest {
            messages: vec![
                ChatMessage::system(prompt.system.clone()),
                ChatMessage::user(prompt.user.clone()),
            ],
            temperature: Some(0.2),
            max_tokens: None,
        }))
        .unwrap();

    // --- parse + persist ---
    let blocks = enhance::parse_enhanced_blocks(&output, &prompt.segment_ids);
    let updated = storage.update_note_blocks(&note.id, &blocks).unwrap();
    assert_eq!(updated.blocks.len(), 3);
    assert_eq!(updated.blocks[0].origin, BlockOrigin::User);
    match &updated.blocks[1].origin {
        BlockOrigin::Ai { source_segment_ids } => {
            assert_eq!(source_segment_ids, &vec!["seg-real-1".to_string()])
        }
        other => panic!("expected ai block, got {other:?}"),
    }

    // enhanced content is searchable and mirrored to markdown on disk
    let hits = storage.search("forecast tomorrow", 10).unwrap();
    assert!(hits.iter().any(|h| h.note_id == note.id));
    let md =
        std::fs::read_to_string(dir.path().join("notes").join(format!("{}.md", note.id))).unwrap();
    assert!(md.contains("approve extra $50k"));

    // --- editing an AI block reclaims it as user text ---
    let block_id = updated.blocks[1].id.clone();
    let reclaimed = storage
        .edit_note_block(&note.id, &block_id, "**Decision:** approved $60k actually")
        .unwrap();
    let edited = reclaimed.blocks.iter().find(|b| b.id == block_id).unwrap();
    assert_eq!(edited.origin, BlockOrigin::User);
    assert!(edited.markdown.contains("$60k"));
}
