//! Cost of the memory paths on a synthetic library of 1,000 meetings
//! (40 weekly series, 30-minute transcripts with word timings, 8 extracted
//! items each). Ignored in CI; run with:
//!
//! ```text
//! cargo test --release -p fly-context --test memory_cost -- --ignored --nocapture
//! ```
//!
//! Prints one line per path: the median of 5 runs, in milliseconds.

use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use fly_context::{
    memory, render, Context, GetBriefingArgs, MemoryPrefetch, OpenItemsArgs, Visibility,
};
use fly_core::{Attendee, ItemKind, MeetingItem, Speaker, Transcript, TranscriptSegment, Word};
use fly_storage::Storage;

const MEETINGS: usize = 1_000;
const SERIES: usize = 40;
const SEGMENTS: usize = 300;
const WORDS_PER_SEGMENT: usize = 12;

/// A letters-only series tag (titles lose digits when normalized).
fn tag(k: usize) -> String {
    format!(
        "q{}{}",
        (b'a' + (k / 26) as u8) as char,
        (b'a' + (k % 26) as u8) as char
    )
}

fn people(i: usize) -> Vec<Attendee> {
    (0..4)
        .map(|k| {
            let p = (i + k * 7) % 30;
            Attendee {
                name: format!("Person{p} Example"),
                email: Some(format!("person{p}@example.com")),
            }
        })
        .collect()
}

fn build(dir: &std::path::Path) -> Storage {
    let s = Storage::open(dir).unwrap();
    for i in 0..MEETINGS {
        let title = format!("Project {} weekly", tag(i % SERIES));
        let note = s.create_note(&title, None).unwrap();
        let attendees = people(i % SERIES);
        let m = s.create_meeting(&title, &note.id, &attendees).unwrap();
        let start = Utc.with_ymd_and_hms(2024, 1, 1, 9, 0, 0).unwrap()
            + chrono::Duration::hours(12 * i as i64);
        s.set_meeting_started_at(&m.id, start).unwrap();
        let segments = (0..SEGMENTS)
            .map(|k| {
                let words: Vec<Word> = (0..WORDS_PER_SEGMENT)
                    .map(|w| Word {
                        text: format!("word{w}"),
                        start_ms: (k * 6_000 + w * 500) as u64,
                        end_ms: (k * 6_000 + w * 500 + 400) as u64,
                    })
                    .collect();
                TranscriptSegment {
                    id: format!("m{i}-s{k}"),
                    speaker_key: format!("spk_{}", k % 4),
                    start_ms: (k * 6_000) as u64,
                    end_ms: (k * 6_000 + 5_500) as u64,
                    text: words
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    words,
                }
            })
            .collect();
        s.save_transcript(&Transcript {
            meeting_id: m.id.clone(),
            language: Some("en".into()),
            engine: "synthetic".into(),
            segments,
            speakers: (0..4)
                .map(|k| Speaker {
                    key: format!("spk_{k}"),
                    label: attendees[k].name.clone(),
                })
                .collect(),
        })
        .unwrap();
        let items: Vec<MeetingItem> = (0..8)
            .map(|k| MeetingItem {
                id: fly_core::new_id(),
                meeting_id: m.id.clone(),
                kind: [
                    ItemKind::Decision,
                    ItemKind::ActionItem,
                    ItemKind::Commitment,
                ][k % 3],
                text: format!("item {k} of meeting {i}"),
                owner: Some(attendees[k % 4].name.clone()),
                status: Some("open".into()),
                speaker_key: Some(format!("spk_{}", k % 4)),
                segment_ids: vec![format!("m{i}-s{}", k * 10)],
                created_at: Utc::now(),
                extracted_by: "synthetic".into(),
            })
            .collect();
        s.replace_meeting_items(&m.id, &items).unwrap();
    }
    s
}

fn median(mut f: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..5)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    times.sort();
    times[2]
}

#[test]
#[ignore = "cost measurement; run in release with --ignored --nocapture"]
fn memory_cost_on_a_synthetic_library() {
    // FLY_MEMORY_COST_DIR keeps the built library between runs (building
    // takes minutes); otherwise a temp dir is used.
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::env::var_os("FLY_MEMORY_COST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| tmp.path().to_path_buf());
    let t = Instant::now();
    let s = if dir.join("flyonthewall.db").is_file() {
        Storage::open(&dir).unwrap()
    } else {
        build(&dir)
    };
    eprintln!(
        "built {MEETINGS} meetings in {:.1}s",
        t.elapsed().as_secs_f64()
    );
    let ctx = Context::new(&s, Visibility::ShareableOnly);
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let row = |name: &str, d: Duration| eprintln!("{name:<58} {:>8.1} ms", ms(d));

    // The fixed cost under every memory call.
    row(
        "  Keep-local sets (keep_local_note_ids + _meeting_ids)",
        median(|| {
            s.keep_local_note_ids().unwrap();
            s.keep_local_meeting_ids().unwrap();
        }),
    );
    row(
        "  newest 2,000 visible meetings (list_meetings_filtered)",
        median(|| {
            s.list_meetings_filtered(&fly_storage::MeetingFilter {
                exclude_keep_local: true,
                limit: 2_000,
                ..Default::default()
            })
            .unwrap();
        }),
    );
    row(
        "  1,000 newest items (query_items, Keep local in SQL)",
        median(|| {
            s.query_items(&fly_storage::ItemFilter {
                limit: 1_000,
                exclude_keep_local: true,
                ..Default::default()
            })
            .unwrap();
        }),
    );
    row(
        "  1,000 newest items (query_items, no Keep-local filter)",
        median(|| {
            s.query_items(&fly_storage::ItemFilter {
                limit: 1_000,
                ..Default::default()
            })
            .unwrap();
        }),
    );
    row(
        "  200 newest items (query_items, Keep local in SQL)",
        median(|| {
            s.query_items(&fly_storage::ItemFilter {
                limit: 200,
                exclude_keep_local: true,
                ..Default::default()
            })
            .unwrap();
        }),
    );
    let all = s
        .list_meetings_filtered(&fly_storage::MeetingFilter {
            limit: 2_000,
            ..Default::default()
        })
        .unwrap();
    row(
        "  detect_series over 1,000 meetings",
        median(|| {
            memory::detect_series(all.clone());
        }),
    );

    let opts = MemoryPrefetch::default();
    row(
        "prefetch, no match (\"What is the capital of France?\")",
        median(|| {
            assert!(ctx
                .memory_prefetch("What is the capital of France?", &opts)
                .unwrap()
                .is_none())
        }),
    );
    row(
        "prefetch, unknown name (\"What did Priya say?\")",
        median(|| {
            assert!(ctx
                .memory_prefetch("What did Priya say?", &opts)
                .unwrap()
                .is_none())
        }),
    );
    row(
        "prefetch, person (\"What did Person3 Example commit to?\")",
        median(|| {
            let b = ctx
                .memory_prefetch("What did Person3 Example commit to?", &opts)
                .unwrap()
                .unwrap();
            assert!(!b.facts.is_empty());
            let _ = render::briefing_rendered(&b, 6_000);
        }),
    );
    row(
        "prefetch, series (\"What changed in the project qah weekly?\")",
        median(|| {
            assert!(ctx
                .memory_prefetch("What changed in the project qah weekly?", &opts)
                .unwrap()
                .is_some())
        }),
    );
    row(
        "get_briefing (\"project qah weekly\", 12 meetings)",
        median(|| {
            ctx.get_briefing(&GetBriefingArgs {
                query: "project qah weekly".into(),
                ..Default::default()
            })
            .unwrap();
        }),
    );
    row(
        "open_items (all, Ask, with evidence)",
        median(|| {
            ctx.open_items(&OpenItemsArgs::default()).unwrap();
        }),
    );
    row(
        "MCP get_context (\"project qah weekly\", all 25 meetings)",
        median(|| {
            memory::mcp::get_context(&ctx, "project qah weekly", None).unwrap();
        }),
    );
    row(
        "MCP open_items (all)",
        median(|| {
            memory::mcp::open_items(&ctx, "all").unwrap();
        }),
    );
    // For comparison: what reading speaker labels cost when it parsed
    // whole transcripts (30 of them, word timings included).
    let ids: Vec<String> = s
        .list_meetings_filtered(&fly_storage::MeetingFilter {
            limit: 30,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    row(
        "30 speaker legends via transcript_speakers",
        median(|| {
            assert_eq!(s.transcript_speakers(&ids).unwrap().len(), 30);
        }),
    );
    row(
        "30 speaker legends via get_transcript (old)",
        median(|| {
            for id in &ids {
                s.get_transcript(id).unwrap().unwrap();
            }
        }),
    );
}
