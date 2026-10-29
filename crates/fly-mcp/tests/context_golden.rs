//! Golden text for the context-layer tools (`get_context`, `whats_changed`,
//! `open_items`). Their wording is a contract for MCP clients, and their
//! logic now lives in `fly_context::memory`: these pins prove the move kept
//! every byte. Ids are replaced by placeholders, dates are fixed.

use chrono::{TimeZone, Utc};
use fly_core::{Attendee, ItemKind, MeetingItem, Speaker, Transcript, TranscriptSegment};
use fly_mcp::Server;
use fly_storage::Storage;
use serde_json::{json, Value};

struct Golden {
    _dir: tempfile::TempDir,
    server: Server,
    /// (placeholder, id) pairs for redaction.
    ids: Vec<(String, String)>,
}

fn seg(id: &str, key: &str, text: &str) -> TranscriptSegment {
    TranscriptSegment {
        id: id.into(),
        speaker_key: key.into(),
        start_ms: 0,
        end_ms: 1_000,
        text: text.into(),
        words: vec![],
    }
}

fn item(
    meeting_id: &str,
    kind: ItemKind,
    text: &str,
    owner: Option<&str>,
    status: Option<&str>,
    speaker: Option<&str>,
    segs: &[&str],
) -> MeetingItem {
    MeetingItem {
        id: fly_core::new_id(),
        meeting_id: meeting_id.into(),
        kind,
        text: text.into(),
        owner: owner.map(String::from),
        status: status.map(String::from),
        speaker_key: speaker.map(String::from),
        segment_ids: segs.iter().map(|s| s.to_string()).collect(),
        created_at: Utc::now(),
        extracted_by: "golden".into(),
    }
}

fn golden() -> Golden {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).unwrap();
    let mut ids = Vec::new();
    let mut meeting = |title: &str, day: u32, attendees: &[&str], tag: &str| -> String {
        let note = s.create_note(title, None).unwrap();
        let att: Vec<Attendee> = attendees.iter().map(|a| Attendee::from_legacy(a)).collect();
        let m = s.create_meeting(title, &note.id, &att).unwrap();
        s.set_meeting_started_at(&m.id, Utc.with_ymd_and_hms(2026, 9, day, 16, 0, 0).unwrap())
            .unwrap();
        ids.push((format!("<{tag}>"), m.id.clone()));
        ids.push((format!("<{tag}-note>"), note.id.clone()));
        m.id
    };
    let w1 = meeting(
        "Launch weekly 2026-09-01",
        1,
        &["dana@example.com", "sam@example.com"],
        "w1",
    );
    let w2 = meeting(
        "Launch weekly #2",
        8,
        &["dana@example.com", "sam@example.com"],
        "w2",
    );
    let w3 = meeting("Launch Weekly", 15, &["dana@example.com"], "w3");
    let budget = meeting("Budget review", 10, &[], "budget");
    let hidden = meeting(
        "Launch weekly (private)",
        12,
        &["dana@example.com", "sam@example.com"],
        "hidden",
    );

    let speakers = vec![
        Speaker {
            key: "mic".into(),
            label: "You".into(),
        },
        Speaker {
            key: "spk_0".into(),
            label: "Dana".into(),
        },
        Speaker {
            key: "spk_1".into(),
            label: "Speaker 2".into(),
        },
    ];
    for (mid, prefix, text) in [
        (&w1, "a", "the beta date moves to October"),
        (&w2, "b", "pricing tiers are final"),
        (&budget, "c", "the travel budget is frozen"),
        (&hidden, "h", "secretword plans"),
    ] {
        s.save_transcript(&Transcript {
            meeting_id: mid.clone(),
            language: Some("en".into()),
            engine: "test".into(),
            segments: vec![
                seg(&format!("{prefix}1"), "mic", text),
                seg(&format!("{prefix}2"), "spk_0", "I will send the deck"),
                seg(&format!("{prefix}3"), "spk_1", "sounds good"),
            ],
            speakers: speakers.clone(),
        })
        .unwrap();
    }
    s.replace_meeting_items(
        &w1,
        &[
            item(
                &w1,
                ItemKind::Decision,
                "Beta moves to October",
                None,
                None,
                Some("mic"),
                &["a1"],
            ),
            item(
                &w1,
                ItemKind::ActionItem,
                "Send the launch deck",
                Some("Dana"),
                Some("open"),
                Some("spk_0"),
                &["a2"],
            ),
            item(
                &w1,
                ItemKind::Question,
                "Who signs off on PR?",
                None,
                None,
                Some("spk_1"),
                &["a3"],
            ),
        ],
    )
    .unwrap();
    s.replace_meeting_items(
        &w2,
        &[
            item(
                &w2,
                ItemKind::Decision,
                "Pricing tiers are final",
                None,
                None,
                Some("mic"),
                &["b1"],
            ),
            item(
                &w2,
                ItemKind::ActionItem,
                "Book the venue",
                Some("Sam"),
                Some("done"),
                Some("spk_1"),
                &["b3"],
            ),
            item(
                &w2,
                ItemKind::Commitment,
                "Dana shares the deck by Friday",
                Some("Dana"),
                None,
                Some("spk_0"),
                &["b2"],
            ),
            item(
                &w2,
                ItemKind::Figure,
                "Three pricing tiers",
                None,
                None,
                None,
                &[],
            ),
        ],
    )
    .unwrap();
    s.replace_meeting_items(
        &budget,
        &[item(
            &budget,
            ItemKind::ActionItem,
            "Freeze travel spend",
            Some("You"),
            Some("open"),
            Some("mic"),
            &["c1"],
        )],
    )
    .unwrap();
    s.replace_meeting_items(
        &hidden,
        &[item(
            &hidden,
            ItemKind::ActionItem,
            "secretword follow-up",
            Some("Dana"),
            Some("open"),
            Some("spk_0"),
            &["h2"],
        )],
    )
    .unwrap();
    let hidden_note = s.get_meeting(&hidden).unwrap().note_id;
    s.set_note_keep_local(&hidden_note, true).unwrap();
    let _ = w3;
    Golden {
        _dir: dir,
        server: Server::new(s),
        ids,
    }
}

fn tool(g: &Golden, name: &str, args: Value) -> String {
    let msg = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}});
    let resp: Value =
        serde_json::from_str(&g.server.handle_message(&msg.to_string()).unwrap()).unwrap();
    assert_ne!(resp["result"]["isError"], true, "{name} errored: {resp}");
    let mut text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    for (placeholder, id) in &g.ids {
        text = text.replace(id.as_str(), placeholder);
    }
    // (a query is echoed back in a miss, so ignore the quoted echo)
    assert!(
        !text.replace("\"secretword\"", "").contains("secretword"),
        "keep-local leaked: {text}"
    );
    assert!(!text.contains("<hidden"), "keep-local id leaked: {text}");
    text
}

const FOOTER: &str = "\nEvery line above cites its meeting and transcript segment ids — verify any claim with get_transcript(meeting_id) before relying on it.\n";

#[test]
fn get_context_text_is_pinned() {
    let g = golden();
    let text = tool(&g, "get_context", json!({"query": "launch weekly"}));
    let want = "# Context: launch weekly
3 meeting(s), 2026-09-01 → 2026-09-15
Participants: dana@example.com, sam@example.com, You, Dana

## Timeline
- 2026-09-01 — Launch weekly 2026-09-01 (meeting: <w1>, transcript ✓, 3 extracted item(s))
- 2026-09-08 — Launch weekly #2 (meeting: <w2>, transcript ✓, 4 extracted item(s))
- 2026-09-15 — Launch Weekly (meeting: <w3>, no transcript, 0 extracted item(s))

## Decisions
- Beta moves to October (said by: You · 2026-09-01 · meeting: <w1> · segments: a1)
- Pricing tiers are final (said by: You · 2026-09-08 · meeting: <w2> · segments: b1)

## Action items
- Send the launch deck (owner: Dana · status: open · said by: Dana · 2026-09-01 · meeting: <w1> · segments: a2)
- Book the venue (owner: Sam · status: done · said by: Speaker 2 · 2026-09-08 · meeting: <w2> · segments: b3)

## Commitments
- Dana shares the deck by Friday (owner: Dana · said by: Dana · 2026-09-08 · meeting: <w2> · segments: b2)

## Open questions
- Who signs off on PR? (said by: Speaker 2 · 2026-09-01 · meeting: <w1> · segments: a3)

## Key figures
- Three pricing tiers (2026-09-08 · meeting: <w2>)
";
    assert_eq!(text, format!("{want}{FOOTER}"));

    let since = tool(
        &g,
        "get_context",
        json!({"query": "launch weekly", "since": "2026-09-05"}),
    );
    assert_eq!(since, format!("{WANT_SINCE}{FOOTER}"));

    // full-text fallback: no title says "travel", a transcript does
    let fallback = tool(&g, "get_context", json!({"query": "travel"}));
    let want_fallback = "# Context: budget review
1 meeting(s), 2026-09-10 → 2026-09-10
Participants: You, Dana

## Timeline
- 2026-09-10 — Budget review (meeting: <budget>, transcript ✓, 1 extracted item(s))

## Action items
- Freeze travel spend (owner: You · status: open · said by: You · 2026-09-10 · meeting: <budget> · segments: c1)
";
    assert_eq!(fallback, format!("{want_fallback}{FOOTER}"));
    misses_are_pinned(&g);
}

/// `launch weekly` from 2026-09-05 (or 09-08): the two newest meetings.
const WANT_SINCE: &str = "# Context: launch weekly
2 meeting(s), 2026-09-08 → 2026-09-15
Participants: dana@example.com, sam@example.com, You, Dana

## Timeline
- 2026-09-08 — Launch weekly #2 (meeting: <w2>, transcript ✓, 4 extracted item(s))
- 2026-09-15 — Launch Weekly (meeting: <w3>, no transcript, 0 extracted item(s))

## Decisions
- Pricing tiers are final (said by: You · 2026-09-08 · meeting: <w2> · segments: b1)

## Action items
- Book the venue (owner: Sam · status: done · said by: Speaker 2 · 2026-09-08 · meeting: <w2> · segments: b3)

## Commitments
- Dana shares the deck by Friday (owner: Dana · said by: Dana · 2026-09-08 · meeting: <w2> · segments: b2)

## Key figures
- Three pricing tiers (2026-09-08 · meeting: <w2>)
";

fn misses_are_pinned(g: &Golden) {
    let miss = tool(
        g,
        "get_context",
        json!({"query": "zebra", "since": "2026-01-02"}),
    );
    assert_eq!(
        miss,
        "No meetings matched \"zebra\" since 2026-01-02. Try search_notes for a broader text search."
    );
    // a query only the keep-local meeting matches reads like a miss
    let hidden = tool(g, "get_context", json!({"query": "secretword"}));
    assert_eq!(
        hidden.replace("secretword", "Q"),
        miss.replace("zebra", "Q").replace(" since 2026-01-02", "")
    );
}

#[test]
fn whats_changed_text_is_pinned() {
    let g = golden();
    let text = tool(
        &g,
        "whats_changed",
        json!({"series": "launch weekly", "since": "2026-09-08"}),
    );
    // the same briefing get_context gives for that window, byte for byte
    assert_eq!(text, format!("{WANT_SINCE}{FOOTER}"));
    let miss = tool(
        &g,
        "whats_changed",
        json!({"series": "board", "since": "2026-09-08"}),
    );
    assert_eq!(
        miss,
        "No series matches \"board\". get_context(query) lists series names."
    );
    let empty = tool(
        &g,
        "whats_changed",
        json!({"series": "launch weekly", "since": "2026-12-01"}),
    );
    assert_eq!(
        empty,
        "No meetings matched \"launch weekly\" since 2026-12-01. Try search_notes for a broader text search."
    );
}

#[test]
fn open_items_text_is_pinned() {
    let g = golden();
    let all = tool(&g, "open_items", json!({}));
    let want = "3 open item(s) in scope \"all\" (newest meeting first; status is as stated in that meeting — verify against later meetings before chasing anyone):
- Freeze travel spend (kind: action_item · owner: You · status: open · 2026-09-10 · meeting: <budget> · segments: c1)
- Dana shares the deck by Friday (kind: commitment · owner: Dana · 2026-09-08 · meeting: <w2> · segments: b2)
- Send the launch deck (kind: action_item · owner: Dana · status: open · 2026-09-01 · meeting: <w1> · segments: a2)
";
    assert_eq!(all, want);
    let scoped = tool(&g, "open_items", json!({"scope": "launch weekly"}));
    let want_scoped = "2 open item(s) in scope \"launch weekly\" (newest meeting first; status is as stated in that meeting — verify against later meetings before chasing anyone):
- Dana shares the deck by Friday (kind: commitment · owner: Dana · 2026-09-08 · meeting: <w2> · segments: b2)
- Send the launch deck (kind: action_item · owner: Dana · status: open · 2026-09-01 · meeting: <w1> · segments: a2)
";
    assert_eq!(scoped, want_scoped);
    let miss = tool(&g, "open_items", json!({"scope": "board"}));
    assert_eq!(
        miss,
        "No series matches \"board\" — pass \"all\" or a series name from get_context."
    );
}
