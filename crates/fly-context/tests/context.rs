//! fly-context against a real fixture database (fly-storage public API
//! only): passages with segment ids, hybrid fusion with a fake embedder,
//! every read function, schemas, and — above all — Keep-local exclusion on
//! every path.

use chrono::{TimeZone, Utc};
use fly_context::{
    call, tool_defs, Context, ContextError, GetEventArgs, GetItemsArgs, GetNoteArgs,
    ListMeetingsArgs, NoteKind, PassageSource, QueryVector, ReadSegmentsArgs, SearchPassagesArgs,
    Visibility,
};
use fly_core::{
    Attendee, ItemKind, MeetingItem, NoteBlock, Speaker, Transcript, TranscriptSegment,
};
use fly_storage::Storage;
use serde_json::{json, Value};

const MODEL: &str = "fake-embed";
const DIMS: usize = 128;

struct Fixture {
    _dir: tempfile::TempDir,
    storage: Storage,
    budget: String,
    renewal: String,
    legal: String,
    one_on_one: String,
    budget_note: String,
    legal_note: String,
}

fn seg(i: usize, key: &str, text: &str) -> TranscriptSegment {
    TranscriptSegment {
        id: format!("seg{i:04}"),
        speaker_key: key.into(),
        start_ms: i as u64 * 5_000,
        end_ms: i as u64 * 5_000 + 4_500,
        text: text.into(),
        words: vec![],
    }
}

fn transcript(meeting_id: &str, prefix: &str, lines: &[(&str, &str)]) -> Transcript {
    Transcript {
        meeting_id: meeting_id.into(),
        language: Some("en".into()),
        engine: "test".into(),
        segments: lines
            .iter()
            .enumerate()
            .map(|(i, (k, t))| {
                let mut s = seg(i, k, t);
                s.id = format!("{prefix}{i:03}");
                s
            })
            .collect(),
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

fn filler(n: usize) -> Vec<(&'static str, &'static str)> {
    (0..n)
        .map(|i| {
            if i % 2 == 0 {
                (
                    "mic",
                    "Let us keep going through the agenda items one at a time please.",
                )
            } else {
                (
                    "spk_0",
                    "Sure, the next point is about logistics for the offsite venue.",
                )
            }
        })
        .collect()
}

fn meeting(
    s: &Storage,
    title: &str,
    folder: Option<&str>,
    day: u32,
    attendees: &[&str],
) -> (String, String) {
    let note = s.create_note(title, folder).unwrap();
    let att: Vec<Attendee> = attendees.iter().map(|a| Attendee::from_legacy(a)).collect();
    let m = s.create_meeting(title, &note.id, &att).unwrap();
    s.set_meeting_started_at(
        &m.id,
        Utc.with_ymd_and_hms(2026, 10, day, 15, 0, 0).unwrap(),
    )
    .unwrap();
    (m.id, note.id)
}

fn item(
    meeting_id: &str,
    kind: ItemKind,
    text: &str,
    owner: Option<&str>,
    segs: &[&str],
) -> MeetingItem {
    MeetingItem {
        id: fly_core::new_id(),
        meeting_id: meeting_id.into(),
        kind,
        text: text.into(),
        owner: owner.map(String::from),
        status: (kind == ItemKind::ActionItem).then(|| "open".to_string()),
        speaker_key: Some("spk_0".into()),
        segment_ids: segs.iter().map(|s| s.to_string()).collect(),
        created_at: Utc::now(),
        extracted_by: "test".into(),
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).unwrap();

    // Shareable: a budget review (long, so it spans several windows).
    let (budget, budget_note) = meeting(
        &s,
        "Q3 budget review",
        None,
        20,
        &["Dana Lee", "dana@acme.test"],
    );
    let mut lines = filler(30);
    lines.push((
        "spk_0",
        "We agreed on a Q3 budget of eight thousand four hundred dollars.",
    ));
    lines.push(("mic", "Great, and hiring stays frozen until January."));
    lines.extend(filler(30));
    lines.push((
        "spk_0",
        "One more thing: the vendor contract renews in March.",
    ));
    s.save_transcript(&transcript(&budget, "b", &lines))
        .unwrap();
    let mut cleaned = transcript(&budget, "b", &lines);
    cleaned.segments[30].text = "We agreed on a Q3 budget of $8,400.".into();
    s.save_cleaned_transcript(&cleaned).unwrap();
    s.update_note_scratchpad(
        &budget_note,
        "My notes: budget approved, check hiring freeze.",
    )
    .unwrap();
    s.update_note_blocks(
        &budget_note,
        &[NoteBlock::ai("- Budget set at $8,400", vec!["b030".into()])],
    )
    .unwrap();
    s.replace_meeting_items(
        &budget,
        &[
            item(
                &budget,
                ItemKind::Decision,
                "Q3 budget is $8,400",
                None,
                &["b030"],
            ),
            item(
                &budget,
                ItemKind::ActionItem,
                "Send the budget memo",
                Some("Dana"),
                &["b031"],
            ),
        ],
    )
    .unwrap();

    // Shareable: a renewal call.
    let (renewal, _) = meeting(&s, "Acme renewal", None, 22, &["Sam Ortiz"]);
    s.save_transcript(&transcript(
        &renewal,
        "r",
        &[
            (
                "spk_0",
                "The renewal pricing goes up five percent next year.",
            ),
            ("mic", "We need the contract redlines by Friday."),
        ],
    ))
    .unwrap();

    // Keep local via folder: legal.
    let folder = s.create_folder("Legal", None).unwrap();
    let (legal, legal_note) = meeting(&s, "Settlement budget", Some(&folder.id), 21, &["Counsel"]);
    s.save_transcript(&transcript(
        &legal,
        "l",
        &[(
            "spk_0",
            "The settlement budget is SECRET-SETTLEMENT nine million.",
        )],
    ))
    .unwrap();
    s.update_note_scratchpad(&legal_note, "SECRET-SETTLEMENT budget notes")
        .unwrap();
    s.replace_meeting_items(
        &legal,
        &[item(
            &legal,
            ItemKind::Decision,
            "SECRET-SETTLEMENT budget approved",
            None,
            &["l000"],
        )],
    )
    .unwrap();
    s.set_folder_keep_local(&folder.id, true).unwrap();

    // Keep local via the note's own flag: a 1:1.
    let (one_on_one, one_note) = meeting(&s, "1:1 budget chat", None, 23, &["Dana Lee"]);
    s.save_transcript(&transcript(
        &one_on_one,
        "o",
        &[("spk_0", "My personal budget worry is SECRET-ONE-ON-ONE.")],
    ))
    .unwrap();
    s.replace_meeting_items(
        &one_on_one,
        &[item(
            &one_on_one,
            ItemKind::ActionItem,
            "SECRET-ONE-ON-ONE budget follow-up",
            Some("Dana"),
            &["o000"],
        )],
    )
    .unwrap();
    s.set_note_keep_local(&one_note, true).unwrap();

    Fixture {
        _dir: dir,
        storage: s,
        budget,
        renewal,
        legal,
        one_on_one,
        budget_note,
        legal_note,
    }
}

/// Deterministic bag-of-words "embedding" (hashed term counts).
fn embed(text: &str) -> Vec<f32> {
    let mut v = vec![0f32; DIMS];
    for w in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
    {
        let h = w
            .to_lowercase()
            .bytes()
            .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
        v[h as usize % DIMS] += 1.0;
    }
    v
}

fn embed_all(s: &Storage) {
    let pending = s.pending_embedding_chunks(MODEL, 10_000).unwrap();
    assert!(!pending.is_empty());
    let vectors: Vec<(i64, Vec<f32>)> = pending.iter().map(|p| (p.id, embed(&p.text))).collect();
    s.store_chunk_embeddings(MODEL, &vectors).unwrap();
}

fn search(q: &str) -> SearchPassagesArgs {
    SearchPassagesArgs {
        query: q.into(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------

#[test]
fn keyword_search_returns_citable_passages() {
    let f = fixture();
    let ctx = Context::new(&f.storage, Visibility::ShareableOnly);
    let r = ctx.search_passages(&search("Q3 budget"), None).unwrap();
    assert!(!r.semantic);
    let first = &r.passages[0];
    assert_eq!(first.source, PassageSource::Transcript);
    assert_eq!(first.meeting_id.as_deref(), Some(f.budget.as_str()));
    // The passage containing the decision, with its exact segment id.
    let hit = first
        .segments
        .iter()
        .find(|s| s.text.contains("eight thousand"))
        .expect("decision line in passage");
    assert_eq!(hit.id, "b030");
    assert_eq!(hit.speaker, "Dana");
    assert!(first.start_ms.unwrap() <= hit.start_ms && hit.end_ms <= first.end_ms.unwrap());
    // The note paragraph surfaces too (keyword list over notes).
    assert!(r
        .passages
        .iter()
        .any(|p| p.source == PassageSource::Note && p.note_id == f.budget_note));
    // Rendered for the model with [seg:ID] markers.
    let out = call(
        &ctx,
        "search_passages",
        &json!({"query": "Q3 budget"}),
        None,
    )
    .unwrap();
    assert!(out.text.contains("[seg:b030] Dana (2:30):"), "{}", out.text);
    assert!(
        out.text.contains("2 Keep-local meetings were not searched"),
        "{}",
        out.text
    );
    assert_eq!(out.json["keep_local_excluded"], 2);
}

#[test]
fn scoped_search_and_date_filters() {
    let f = fixture();
    let ctx = Context::new(&f.storage, Visibility::All);
    let r = ctx
        .search_passages(
            &SearchPassagesArgs {
                query: "contract".into(),
                meeting_ids: Some(vec![f.renewal.clone()]),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    assert!(!r.passages.is_empty());
    assert!(r
        .passages
        .iter()
        .all(|p| p.meeting_id.as_deref() == Some(f.renewal.as_str())));
    // until 2026-10-21 excludes the renewal (22nd).
    let r = ctx
        .search_passages(
            &SearchPassagesArgs {
                query: "contract".into(),
                until: Some("2026-10-21".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    assert!(r
        .passages
        .iter()
        .all(|p| p.meeting_id.as_deref() != Some(f.renewal.as_str())));
    let bad = ctx.search_passages(
        &SearchPassagesArgs {
            query: "x".into(),
            since: Some("last tuesday".into()),
            ..Default::default()
        },
        None,
    );
    assert!(matches!(bad, Err(ContextError::InvalidArgs(_))));
    assert!(matches!(
        ctx.search_passages(&search("   "), None),
        Err(ContextError::InvalidArgs(_))
    ));
}

#[test]
fn semantic_hits_map_onto_windows_and_fuse() {
    let f = fixture();
    embed_all(&f.storage);
    let ctx = Context::new(&f.storage, Visibility::ShareableOnly);
    // FTS ANDs every term, so "timeline" (absent everywhere) kills the
    // keyword list; the vector list still finds the budget passage.
    let q = "hiring frozen timeline";
    let v = embed(q);
    let qv = QueryVector {
        vector: &v,
        model: MODEL,
    };
    let r = ctx.search_passages(&search(q), Some(qv)).unwrap();
    assert!(r.semantic);
    let p = r
        .passages
        .iter()
        .find(|p| p.segments.iter().any(|s| s.id == "b031"))
        .expect("semantic passage with the hiring line");
    assert_eq!(p.matched_by, vec!["semantic"]);
    // Agreement between lists ranks first.
    let q = "hiring frozen";
    let v = embed(q);
    let r = ctx
        .search_passages(
            &search(q),
            Some(QueryVector {
                vector: &v,
                model: MODEL,
            }),
        )
        .unwrap();
    assert_eq!(r.passages[0].matched_by, vec!["keyword", "semantic"]);
}

#[test]
fn read_segments_by_id_with_polished_option() {
    let f = fixture();
    let ctx = Context::new(&f.storage, Visibility::ShareableOnly);
    let w = ctx
        .read_segments(&ReadSegmentsArgs {
            meeting_id: f.budget.clone(),
            around_segment_id: "b030".into(),
            before: Some(1),
            after: Some(1),
            prefer_polished: None,
        })
        .unwrap();
    let ids: Vec<&str> = w.segments.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["b029", "b030", "b031"]);
    assert!(w.more_before && w.more_after && !w.polished);
    assert!(w.segments[1].text.contains("eight thousand"));
    let w = ctx
        .read_segments(&ReadSegmentsArgs {
            meeting_id: f.budget.clone(),
            around_segment_id: "b030".into(),
            before: Some(0),
            after: Some(0),
            prefer_polished: Some(true),
        })
        .unwrap();
    assert!(w.polished);
    assert_eq!(w.segments[0].text, "We agreed on a Q3 budget of $8,400.");
    // Bounds clamp; unknown ids are a clear error.
    let w = ctx
        .read_segments(&ReadSegmentsArgs {
            meeting_id: f.budget.clone(),
            around_segment_id: "b000".into(),
            before: Some(500),
            after: Some(0),
            prefer_polished: None,
        })
        .unwrap();
    assert!(!w.more_before);
    let e = ctx
        .read_segments(&ReadSegmentsArgs {
            meeting_id: f.budget.clone(),
            around_segment_id: "nope".into(),
            ..Default::default()
        })
        .unwrap_err();
    assert!(matches!(e, ContextError::NotFound(_)), "{e}");
}

#[test]
fn list_meetings_filters() {
    let f = fixture();
    let ctx = Context::new(&f.storage, Visibility::All);
    let all = ctx.list_meetings(&ListMeetingsArgs::default()).unwrap();
    assert_eq!(all.meetings.len(), 4);
    assert_eq!(all.meetings[0].meeting_id, f.one_on_one, "newest first");
    let r = ctx
        .list_meetings(&ListMeetingsArgs {
            since: Some("2026-10-21".into()),
            until: Some("2026-10-22".into()),
            ..Default::default()
        })
        .unwrap();
    let ids: Vec<&str> = r.meetings.iter().map(|m| m.meeting_id.as_str()).collect();
    assert_eq!(ids, [f.renewal.as_str(), f.legal.as_str()]);
    let r = ctx
        .list_meetings(&ListMeetingsArgs {
            attendee: Some("ACME.test".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.meetings.len(), 1);
    assert_eq!(r.meetings[0].meeting_id, f.budget);
    let r = ctx
        .list_meetings(&ListMeetingsArgs {
            title_contains: Some("budget".into()),
            limit: Some(2),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.meetings.len(), 2);
}

#[test]
fn get_event_note_and_items() {
    let f = fixture();
    let ctx = Context::new(&f.storage, Visibility::ShareableOnly);
    let e = ctx
        .get_event(&GetEventArgs {
            meeting_id: f.budget.clone(),
        })
        .unwrap();
    assert_eq!(e.title, "Q3 budget review");
    assert_eq!(e.segment_count, 63);
    assert!(e.has_polished_transcript);
    assert!(!e.attendees_confirmed);
    assert_eq!(e.item_counts["decision"], 1);
    assert_eq!(e.item_counts["action_item"], 1);

    let n = ctx
        .get_note(&GetNoteArgs {
            meeting_id: Some(f.budget.clone()),
            kind: Some(NoteKind::Enhanced),
            ..Default::default()
        })
        .unwrap();
    assert!(n.scratchpad.is_none());
    let blocks = n.enhanced.unwrap();
    assert_eq!(blocks[0].origin, "ai");
    assert_eq!(blocks[0].source_segment_ids, ["b030"]);
    assert!(matches!(
        ctx.get_note(&GetNoteArgs::default()),
        Err(ContextError::InvalidArgs(_))
    ));

    let items = ctx
        .get_items(&GetItemsArgs {
            kind: Some("action_item".into()),
            owner: Some("dana".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(items.items.len(), 1, "keep-local 1:1 item excluded");
    assert_eq!(items.items[0].text, "Send the budget memo");
    let out = call(&ctx, "get_items", &json!({"kind": "decision"}), None).unwrap();
    assert!(out.text.contains("[seg:b030]"), "{}", out.text);
    assert!(matches!(
        ctx.get_items(&GetItemsArgs {
            kind: Some("todo".into()),
            ..Default::default()
        }),
        Err(ContextError::InvalidArgs(_))
    ));
}

/// The privacy contract: under ShareableOnly no path returns keep-local
/// text, by-id reads of keep-local meetings/notes are refused, and the same
/// calls under All do see it (so the test isn't vacuous).
#[test]
fn keep_local_is_excluded_on_every_path() {
    let f = fixture();
    embed_all(&f.storage);
    let v = embed("settlement budget worry");
    let qv = QueryVector {
        vector: &v,
        model: MODEL,
    };
    let shared = Context::new(&f.storage, Visibility::ShareableOnly);
    let local = Context::new(&f.storage, Visibility::All);
    assert_eq!(shared.excluded_keep_local_meetings().unwrap(), 2);
    assert_eq!(local.excluded_keep_local_meetings().unwrap(), 0);

    let calls: Vec<(&str, Value)> = vec![
        ("search_passages", json!({"query": "budget"})),
        ("search_passages", json!({"query": "SECRET"})),
        (
            "search_passages",
            json!({"query": "settlement budget worry"}),
        ),
        (
            "search_passages",
            json!({"query": "budget", "meeting_ids": [f.legal, f.one_on_one, f.budget]}),
        ),
        ("list_meetings", json!({})),
        ("list_meetings", json!({"title_contains": "budget"})),
        ("get_items", json!({})),
        ("get_items", json!({"meeting_ids": [f.legal, f.one_on_one]})),
        ("get_note", json!({"note_id": f.budget_note})),
    ];
    let mut local_saw_secret = false;
    for (name, args) in &calls {
        let out = call(&shared, name, args, Some(qv)).unwrap();
        let all = format!("{}\n{}", out.json, out.text);
        assert!(!all.contains("SECRET"), "{name} {args} leaked:\n{all}");
        assert!(
            !all.contains(&f.legal) && !all.contains(&f.one_on_one),
            "{name} {args} named a keep-local meeting"
        );
        let out = call(&local, name, args, Some(qv)).unwrap();
        local_saw_secret |= format!("{}{}", out.json, out.text).contains("SECRET");
    }
    assert!(
        local_saw_secret,
        "fixture must expose keep-local text under All"
    );

    // By-id reads refuse, for both kinds of keep-local.
    for mid in [&f.legal, &f.one_on_one] {
        for (name, args) in [
            (
                "read_segments",
                json!({"meeting_id": mid, "around_segment_id": "l000"}),
            ),
            ("get_event", json!({"meeting_id": mid})),
            ("get_note", json!({"meeting_id": mid})),
        ] {
            let e = call(&shared, name, &args, None).unwrap_err();
            assert!(matches!(e, ContextError::KeepLocal(_)), "{name}: {e}");
            assert!(call(&local, name, &args, None).is_ok() || name == "read_segments");
        }
    }
    let e = call(&shared, "get_note", &json!({"note_id": f.legal_note}), None).unwrap_err();
    assert!(matches!(e, ContextError::KeepLocal(_)));
    // And the legal note's scratchpad is readable locally.
    assert!(
        call(&local, "get_note", &json!({"note_id": f.legal_note}), None)
            .unwrap()
            .text
            .contains("SECRET-SETTLEMENT")
    );
}

#[test]
fn schemas_are_strict_compatible_and_args_round_trip() {
    let defs = tool_defs();
    let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
    assert_eq!(
        names,
        [
            "search_passages",
            "read_segments",
            "list_meetings",
            "get_event",
            "get_note",
            "get_items"
        ]
    );
    for d in &defs {
        for schema in [&d.input_schema, &d.strict_input_schema] {
            assert_eq!(schema["type"], "object", "{}", d.name);
            assert_eq!(schema["additionalProperties"], false, "{}", d.name);
            let text = schema.to_string();
            for banned in [
                "minimum",
                "maximum",
                "minLength",
                "maxLength",
                "pattern",
                "format",
            ] {
                assert!(
                    !text.contains(&format!("\"{banned}\"")),
                    "{}: {banned}",
                    d.name
                );
            }
        }
        // Strict form: every property required, optional ones nullable.
        let props = d.strict_input_schema["properties"].as_object().unwrap();
        let required: Vec<&str> = d.strict_input_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(props.len(), required.len(), "{}", d.name);
        let plain_required = d.input_schema["required"].as_array().unwrap();
        for (k, p) in props {
            let optional = !plain_required.iter().any(|r| r == k);
            assert_eq!(p["type"].is_array(), optional, "{}.{k}", d.name);
        }
        // An all-null strict call deserializes (required ones filled in).
        let mut args = serde_json::Map::new();
        for (k, p) in props {
            let v = if p["type"].is_array() {
                Value::Null
            } else {
                json!("x")
            };
            args.insert(k.clone(), v);
        }
        let f = fixture();
        let ctx = Context::new(&f.storage, Visibility::All);
        match call(&ctx, d.name, &Value::Object(args), None) {
            Ok(_) | Err(ContextError::NotFound(_)) => {}
            Err(ContextError::InvalidArgs(m)) if d.name == "get_note" => {
                assert!(m.contains("exactly one"), "{m}")
            }
            Err(e) => panic!("{}: {e}", d.name),
        }
    }
    // Unknown fields and tools are clear errors.
    let f = fixture();
    let ctx = Context::new(&f.storage, Visibility::All);
    let e = call(
        &ctx,
        "get_event",
        &json!({"meeting_id": "x", "verbose": true}),
        None,
    )
    .unwrap_err();
    assert!(matches!(e, ContextError::InvalidArgs(_)), "{e}");
    let e = call(&ctx, "delete_meeting", &json!({}), None).unwrap_err();
    assert!(e.to_string().contains("search_passages"), "{e}");
}
