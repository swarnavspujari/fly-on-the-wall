//! v2 → v3 upgrade (per-segment index, label-free chunks with spans,
//! citation locators) against a database built the way a v2 build leaves
//! it: v2 DDL, labelled chunks with vectors, a polished transcript, AI
//! blocks and items citing segments, keep-local folders (incl. a
//! sub-folder) and a keep-local meeting, plus an orphan chunk. Everything is
//! built in a temp dir — no real user data.
//!
//! Also covers what happens when an OLDER (v2) build opens the migrated
//! file: its writes are replayed as the exact SQL the v2 code runs, the
//! triggers keep the v3 tables consistent meanwhile, and the next v3 open
//! heals the rest.

use std::collections::BTreeMap;
use std::path::Path;

use fly_core::{BlockOrigin, NoteBlock, Speaker, Transcript, TranscriptSegment};
use fly_storage::citations::{OWNER_ITEM, OWNER_NOTE_BLOCK};
use fly_storage::{
    CitationStatus, MeetingFilter, OpenRetry, SearchFilter, SearchHitKind, Storage,
    UpgradeProgress, UpgradeStage,
};
use rusqlite::Connection;

/// `create_baseline` exactly as v2 builds ship it (comments dropped).
const V2_DDL: &str = r#"
CREATE TABLE folders (
    id TEXT PRIMARY KEY, name TEXT NOT NULL,
    parent_id TEXT REFERENCES folders(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL, keep_local INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE notes (
    id TEXT PRIMARY KEY, title TEXT NOT NULL,
    folder_id TEXT REFERENCES folders(id) ON DELETE SET NULL,
    meeting_id TEXT, scratchpad TEXT NOT NULL DEFAULT '',
    blocks_json TEXT NOT NULL DEFAULT '[]',
    attachments_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, disk_path TEXT
);
CREATE TABLE meetings (
    id TEXT PRIMARY KEY, title TEXT NOT NULL, note_id TEXT NOT NULL,
    attendees_json TEXT NOT NULL DEFAULT '[]',
    attendees_confirmed INTEGER NOT NULL DEFAULT 0,
    started_at TEXT NOT NULL, ended_at TEXT, recording_json TEXT,
    keep_local INTEGER NOT NULL DEFAULT 0,
    participants_notified INTEGER NOT NULL DEFAULT 0,
    audio_deleted_at TEXT
);
CREATE TABLE transcripts (
    meeting_id TEXT PRIMARY KEY, json TEXT NOT NULL,
    cleaned_json TEXT, speaker_undo_json TEXT
);
CREATE TABLE templates (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, system_prompt TEXT NOT NULL,
    structure_hint TEXT NOT NULL, built_in INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE transcription_jobs (
    meeting_id TEXT PRIMARY KEY, status TEXT NOT NULL DEFAULT 'queued',
    attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE meeting_items (
    id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL, kind TEXT NOT NULL,
    text TEXT NOT NULL, owner TEXT, status TEXT, speaker_key TEXT,
    segment_ids_json TEXT NOT NULL DEFAULT '[]', created_at TEXT NOT NULL,
    extracted_by TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_meeting_items_meeting ON meeting_items(meeting_id);
CREATE INDEX idx_meeting_items_kind ON meeting_items(kind);
CREATE VIRTUAL TABLE notes_fts USING fts5(note_id UNINDEXED, title, body);
CREATE VIRTUAL TABLE transcripts_fts USING fts5(meeting_id UNINDEXED, body);
CREATE TABLE embedding_chunks (
    id INTEGER PRIMARY KEY, owner_kind TEXT NOT NULL, owner_id TEXT NOT NULL,
    chunk_index INTEGER NOT NULL, title TEXT NOT NULL DEFAULT '',
    text TEXT NOT NULL, start_ms INTEGER, embedding BLOB, model TEXT,
    UNIQUE(owner_kind, owner_id, chunk_index)
);
CREATE INDEX idx_embedding_chunks_owner ON embedding_chunks(owner_kind, owner_id);
PRAGMA user_version = 2;
"#;

const MODEL: &str = "embeddinggemma";
const BULK_MEETINGS: usize = 30;
const BULK_SEGMENTS: usize = 300;

fn blob(v: &[f32]) -> Vec<u8> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter().flat_map(|x| (x / n).to_le_bytes()).collect()
}

fn seg(id: &str, key: &str, start_ms: u64, text: &str) -> TranscriptSegment {
    TranscriptSegment {
        id: id.into(),
        speaker_key: key.into(),
        start_ms,
        end_ms: start_ms + 1_800,
        text: text.into(),
        words: vec![],
    }
}

fn transcript(meeting_id: &str, segs: Vec<TranscriptSegment>) -> Transcript {
    Transcript {
        meeting_id: meeting_id.into(),
        language: Some("en".into()),
        engine: "whisper".into(),
        segments: segs,
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

/// The v2 chunk text: `Label: text`, one line per turn (one chunk is
/// enough for a fixture — it only has to be what v2 stored).
fn v2_chunk_text(t: &Transcript) -> String {
    t.segments
        .iter()
        .map(|s| format!("{}: {}", t.label_for(&s.speaker_key), s.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// v2 `save_transcript` + `sync_transcript_derived` + `sync_chunks`, as SQL.
fn v2_save_transcript(conn: &Connection, t: &Transcript, title: &str, vector: Option<&[f32]>) {
    conn.execute(
        "INSERT INTO transcripts (meeting_id, json) VALUES (?1, ?2)
         ON CONFLICT(meeting_id) DO UPDATE SET json = excluded.json",
        (&t.meeting_id, serde_json::to_string(t).unwrap()),
    )
    .unwrap();
    conn.execute(
        "DELETE FROM transcripts_fts WHERE meeting_id = ?1",
        [&t.meeting_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO transcripts_fts (meeting_id, body) VALUES (?1, ?2)",
        (&t.meeting_id, v2_chunk_text(t)),
    )
    .unwrap();
    conn.execute(
        "DELETE FROM embedding_chunks WHERE owner_kind = ?1 AND owner_id = ?2",
        ("transcript", &t.meeting_id),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO embedding_chunks
         (owner_kind, owner_id, chunk_index, title, text, start_ms, embedding, model)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            "transcript",
            &t.meeting_id,
            0,
            title,
            v2_chunk_text(t),
            t.segments.first().map(|s| s.start_ms as i64),
            vector.map(blob),
            vector.map(|_| MODEL),
        ],
    )
    .unwrap();
}

struct Fixture {
    budget_note: String,
    budget_block: String,
    case_block: String,
    budget_item: String,
}

fn v2_fixture(dir: &Path) -> Fixture {
    for sub in ["notes", "transcripts", "attachments", "recordings"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let conn = Connection::open(dir.join("flyonthewall.db")).unwrap();
    conn.execute_batch(V2_DDL).unwrap();
    let ts = "2026-09-01T10:00:00Z";
    conn.execute_batch(&format!(
        "INSERT INTO folders VALUES ('f-clients', 'Clients', NULL, '{ts}', 0);
         INSERT INTO folders VALUES ('f-legal', 'Legal', NULL, '{ts}', 1);
         INSERT INTO folders VALUES ('f-cases', 'Cases', 'f-legal', '{ts}', 0);"
    ))
    .unwrap();

    let budget_block = NoteBlock::ai("- Fuel bid at $4.90", vec!["b1".into(), "b2".into()]);
    let case_block = NoteBlock::ai("- Settlement figure", vec!["c1".into()]);
    let notes = [
        (
            "n-budget",
            "Q3 budget",
            Some("f-clients"),
            "budget scratch notes",
            vec![budget_block.clone(), NoteBlock::user("mine")],
        ),
        (
            "n-case",
            "Case review",
            Some("f-cases"),
            "",
            vec![case_block.clone()],
        ),
        ("n-one", "1:1 Dana", None, "", vec![]),
        ("n-standup", "Standup", None, "", vec![]),
    ];
    for (id, title, folder, scratch, blocks) in &notes {
        conn.execute(
            "INSERT INTO notes (id, title, folder_id, scratchpad, blocks_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![id, title, folder, scratch, serde_json::to_string(blocks).unwrap(), ts],
        )
        .unwrap();
    }
    // v2 note chunk with a vector (unchanged by v3: keeps its vector)
    conn.execute(
        "INSERT INTO embedding_chunks
         (owner_kind, owner_id, chunk_index, title, text, start_ms, embedding, model)
         VALUES ('note', 'n-budget', 0, 'Q3 budget', 'budget scratch notes', NULL, ?1, ?2)",
        (blob(&[0.0, 0.0, 1.0]), MODEL),
    )
    .unwrap();

    let meetings = [
        (
            "m-budget",
            "Q3 budget",
            "n-budget",
            "2026-09-02T10:00:00Z",
            0,
        ),
        ("m-case", "Case review", "n-case", "2026-09-03T10:00:00Z", 0),
        ("m-one", "1:1 Dana", "n-one", "2026-09-04T10:00:00Z", 1),
        (
            "m-standup",
            "Standup",
            "n-standup",
            "2026-09-05T10:00:00Z",
            0,
        ),
        // Its note row is gone: privacy can't be checked → fail closed.
        (
            "m-noteless",
            "Noteless",
            "n-missing",
            "2026-09-06T10:00:00Z",
            0,
        ),
    ];
    for (id, title, note, at, keep) in meetings {
        conn.execute(
            "INSERT INTO meetings (id, title, note_id, started_at, keep_local) VALUES (?1, ?2, ?3, ?4, ?5)",
            (id, title, note, at, keep),
        )
        .unwrap();
    }
    let budget = transcript(
        "m-budget",
        vec![
            seg(
                "b1",
                "mic",
                0,
                "we agreed the fule bid goes out at four ninety",
            ),
            seg("b2", "spk_0", 2_000, "and we ship the renewal on friday"),
            seg("b3", "spk_0", 4_000, ""),
        ],
    );
    v2_save_transcript(&conn, &budget, "Q3 budget", Some(&[1.0, 0.0, 0.0]));
    let mut polished = budget.clone();
    polished.segments[0].text = "We agreed the fuel bid goes out at $4.90.".into();
    conn.execute(
        "UPDATE transcripts SET cleaned_json = ?1 WHERE meeting_id = 'm-budget'",
        [serde_json::to_string(&polished).unwrap()],
    )
    .unwrap();
    // Keep-local content scores HIGHEST for the [1,0,0] query below.
    v2_save_transcript(
        &conn,
        &transcript(
            "m-case",
            vec![seg(
                "c1",
                "spk_0",
                0,
                "the settlement figure SECRETCASE fuel",
            )],
        ),
        "Case review",
        Some(&[1.0, 0.0, 0.0]),
    );
    v2_save_transcript(
        &conn,
        &transcript(
            "m-one",
            vec![seg("o1", "spk_0", 0, "SECRETONE salary fuel talk")],
        ),
        "1:1 Dana",
        Some(&[1.0, 0.0, 0.0]),
    );
    v2_save_transcript(
        &conn,
        &transcript(
            "m-standup",
            vec![seg("s1", "mic", 0, "standup blockers and fuel")],
        ),
        "Standup",
        Some(&[0.9, 0.1, 0.0]),
    );
    v2_save_transcript(
        &conn,
        &transcript(
            "m-noteless",
            vec![seg("n1", "spk_0", 0, "SECRETNOTELESS fuel ledger")],
        ),
        "Noteless",
        Some(&[1.0, 0.0, 0.0]),
    );
    // orphan chunk of a meeting deleted long ago
    conn.execute(
        "INSERT INTO embedding_chunks
         (owner_kind, owner_id, chunk_index, title, text, start_ms, embedding, model)
         VALUES ('transcript', 'm-gone', 0, 'Gone', 'Dana: SECRETORPHAN fuel', 0, ?1, ?2)",
        (blob(&[1.0, 0.0, 0.0]), MODEL),
    )
    .unwrap();

    let budget_item = fly_core::new_id();
    for (id, meeting, segs) in [
        (budget_item.as_str(), "m-budget", "[\"b2\"]"),
        ("item-case", "m-case", "[\"c1\"]"),
    ] {
        conn.execute(
            "INSERT INTO meeting_items (id, meeting_id, kind, text, segment_ids_json, created_at, extracted_by)
             VALUES (?1, ?2, 'decision', 'ship friday', ?3, ?4, 'test')",
            (id, meeting, segs, ts),
        )
        .unwrap();
    }

    // Realistic volume: 30 meetings × 300 segments.
    for i in 0..BULK_MEETINGS {
        let (mid, nid) = (format!("m-bulk-{i}"), format!("n-bulk-{i}"));
        conn.execute(
            "INSERT INTO notes (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            (&nid, format!("Weekly sync {i}"), ts),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meetings (id, title, note_id, started_at) VALUES (?1, ?2, ?3, ?4)",
            (
                &mid,
                format!("Weekly sync {i}"),
                &nid,
                format!("2026-08-{:02}T09:00:00Z", 1 + i % 28),
            ),
        )
        .unwrap();
        let segs = (0..BULK_SEGMENTS)
            .map(|j| {
                seg(
                    &format!("{mid}-s{j}"),
                    if j % 3 == 0 { "mic" } else { "spk_0" },
                    j as u64 * 2_000,
                    &format!(
                        "item {j} of the weekly sync covers roadmap hiring and the budget line {i}"
                    ),
                )
            })
            .collect();
        v2_save_transcript(
            &conn,
            &transcript(&mid, segs),
            &format!("Weekly sync {i}"),
            Some(&[0.0, 1.0, 0.0]),
        );
    }

    Fixture {
        budget_note: "n-budget".into(),
        budget_block: budget_block.id,
        case_block: case_block.id,
        budget_item,
    }
}

/// A v2 library at a chosen size, shaped like real data: every segment
/// carries word timings (~14 words), a third of the meetings have a polished
/// variant, each note has AI blocks citing segments, each meeting has
/// extracted items, and the v2 transcript chunks carry 768-dim vectors.
/// Synthetic text only. Written in one transaction (minutes otherwise).
fn v2_library(dir: &Path, meetings: usize, segments: usize) {
    const VOCAB: &[&str] = &[
        "roadmap", "hiring", "budget", "launch", "review", "customer", "pricing", "renewal",
        "timeline", "design", "metrics", "quarter", "vendor", "contract", "follow", "up", "the",
        "we", "should", "next", "week", "plan", "team", "agreed", "on", "and",
    ];
    for sub in ["notes", "transcripts", "attachments", "recordings"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let conn = Connection::open(dir.join("flyonthewall.db")).unwrap();
    conn.execute_batch(V2_DDL).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    let ts = "2026-09-01T10:00:00Z";
    let vector: Vec<f32> = (0..768).map(|i| (i % 7) as f32 + 0.5).collect();
    for i in 0..meetings {
        let (mid, nid) = (format!("m-lib-{i}"), format!("n-lib-{i}"));
        let title = format!("Weekly sync {i}");
        let segs: Vec<TranscriptSegment> = (0..segments)
            .map(|j| {
                let start = j as u64 * 4_000;
                let words: Vec<fly_core::Word> = (0..14)
                    .map(|w| fly_core::Word {
                        text: VOCAB[(i * 7 + j * 3 + w) % VOCAB.len()].into(),
                        start_ms: start + w as u64 * 250,
                        end_ms: start + w as u64 * 250 + 200,
                    })
                    .collect();
                TranscriptSegment {
                    id: format!("{mid}-s{j}"),
                    speaker_key: ["mic", "spk_0", "spk_1"][j % 3].into(),
                    start_ms: start,
                    end_ms: start + 3_600,
                    text: words
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    words,
                }
            })
            .collect();
        let t = transcript(&mid, segs);
        // AI blocks citing segments; items citing segments.
        let blocks: Vec<NoteBlock> = (0..6)
            .map(|b| {
                NoteBlock::ai(
                    format!("- point {b}"),
                    (0..3)
                        .map(|k| format!("{mid}-s{}", (b * 40 + k) % segments))
                        .collect(),
                )
            })
            .collect();
        conn.execute(
            "INSERT INTO notes (id, title, blocks_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            (&nid, &title, serde_json::to_string(&blocks).unwrap(), ts),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meetings (id, title, note_id, started_at) VALUES (?1, ?2, ?3, ?4)",
            (
                &mid,
                &title,
                &nid,
                format!("2026-08-{:02}T09:00:00Z", 1 + i % 28),
            ),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO transcripts (meeting_id, json) VALUES (?1, ?2)",
            (&mid, serde_json::to_string(&t).unwrap()),
        )
        .unwrap();
        if i % 3 == 0 {
            let mut polished = t.clone();
            for s in &mut polished.segments {
                s.text = format!("{}.", s.text);
            }
            conn.execute(
                "UPDATE transcripts SET cleaned_json = ?1 WHERE meeting_id = ?2",
                (serde_json::to_string(&polished).unwrap(), &mid),
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO transcripts_fts (meeting_id, body) VALUES (?1, ?2)",
            (&mid, v2_chunk_text(&t)),
        )
        .unwrap();
        // v2 chunking: labelled lines, ~25 segments a chunk, all embedded.
        for (c, part) in t.segments.chunks(25).enumerate() {
            let text = part
                .iter()
                .map(|s| format!("{}: {}", t.label_for(&s.speaker_key), s.text))
                .collect::<Vec<_>>()
                .join("\n");
            conn.execute(
                "INSERT INTO embedding_chunks
                 (owner_kind, owner_id, chunk_index, title, text, start_ms, embedding, model)
                 VALUES ('transcript', ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    &mid,
                    c as i64,
                    &title,
                    text,
                    part[0].start_ms as i64,
                    blob(&vector),
                    MODEL
                ],
            )
            .unwrap();
        }
        for k in 0..8 {
            conn.execute(
                "INSERT INTO meeting_items (id, meeting_id, kind, text, segment_ids_json, created_at, extracted_by)
                 VALUES (?1, ?2, 'action_item', 'follow up', ?3, ?4, 'test')",
                (
                    format!("{mid}-item-{k}"),
                    &mid,
                    format!("[\"{mid}-s{}\"]", (k * 30 + 5) % segments),
                    ts,
                ),
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
}

fn bench_meetings() -> usize {
    std::env::var("FOTW_V3_BENCH_MEETINGS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000)
}

/// Times the v3 step, per stage, over a realistic large library (release
/// build): `cargo test --release -p fly-storage --test migration_v3 --
/// --ignored --nocapture v3_step_timing`. Size via FOTW_V3_BENCH_MEETINGS
/// (default 1,000 meetings × 300 segments).
#[test]
#[ignore = "manual benchmark: about a minute in release"]
fn v3_step_timing() {
    let meetings = bench_meetings();
    let tmp = tempfile::tempdir().unwrap();
    let built = std::time::Instant::now();
    v2_library(tmp.path(), meetings, 300);
    let size = std::fs::metadata(tmp.path().join("flyonthewall.db"))
        .unwrap()
        .len();
    eprintln!(
        "fixture: {meetings} meetings x 300 segments, {} MB, built in {:?}",
        size / 1_000_000,
        built.elapsed()
    );
    // A stage's first report marks its start (stage changes are never
    // throttled); the final report comes right after the commit.
    let mut marks: Vec<(UpgradeStage, std::time::Instant)> = Vec::new();
    let mut reports = 0;
    let started = std::time::Instant::now();
    let s = Storage::open_with_progress(tmp.path(), &mut |p| {
        reports += 1;
        if marks.last().is_none_or(|(s, _)| *s != p.stage) {
            marks.push((p.stage, std::time::Instant::now()));
        }
    })
    .unwrap();
    let opened = std::time::Instant::now();
    eprintln!("open + v3 step: {:?} ({reports} reports)", opened - started);
    for (i, (stage, at)) in marks.iter().enumerate() {
        let end = marks.get(i + 1).map_or(opened, |(_, t)| *t);
        eprintln!("  {stage:?}: {:?}", end - *at);
    }
    drop(s);
    assert_eq!(user_version(tmp.path()), 3);
    assert_eq!(
        count(tmp.path(), "SELECT COUNT(*) FROM transcript_segments"),
        (meetings * 300) as i64
    );
}

/// Writes a synthetic v2 library (as [`v3_step_timing`] measures) into
/// FOTW_V2_LIBRARY_DIR for a manual first-launch check of the app's upgrade
/// screen. Refuses a dir that already holds a database.
#[test]
#[ignore = "manual: writes outside the temp dir"]
fn write_v2_library_for_manual_check() {
    let Ok(dir) = std::env::var("FOTW_V2_LIBRARY_DIR") else {
        eprintln!("set FOTW_V2_LIBRARY_DIR");
        return;
    };
    let dir = Path::new(&dir);
    assert!(
        !dir.join("flyonthewall.db").exists() && !dir.join("looma.db").exists(),
        "{} already has a database",
        dir.display()
    );
    std::fs::create_dir_all(dir).unwrap();
    v2_library(dir, bench_meetings(), 300);
    assert_eq!(user_version(dir), 2);
}

/// Every report, collected.
fn open_reporting(dir: &Path) -> (Storage, Vec<UpgradeProgress>) {
    let mut seen = Vec::new();
    let s = Storage::open_with_retry_and_progress(
        dir,
        OpenRetry {
            attempts: 1,
            backoff: std::time::Duration::ZERO,
        },
        &mut |p| seen.push(p),
    )
    .unwrap();
    (s, seen)
}

fn dir_listing(dir: &Path) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = std::fs::read_dir(dir)
        .map(|it| {
            it.flatten()
                .map(|e| {
                    let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                    (e.file_name().to_string_lossy().into_owned(), len)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// `pending_upgrade` predicts exactly whether `open` runs the v3 step (it
/// reports progress only then), and touches nothing itself.
#[test]
fn pending_upgrade_agrees_with_open_and_writes_nothing() {
    // Fresh install: no data dir at all, then an empty one.
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("not-yet");
    assert_eq!(Storage::pending_upgrade(&missing).unwrap(), None);
    assert!(!missing.exists(), "the peek creates nothing");
    assert_eq!(Storage::pending_upgrade(tmp.path()).unwrap(), None);
    assert!(dir_listing(tmp.path()).is_empty());
    let (s, seen) = open_reporting(&missing);
    assert!(seen.is_empty(), "a fresh database runs no step: {seen:?}");
    drop(s);
    // Now v3: nothing pending, nothing reported.
    assert_eq!(Storage::pending_upgrade(&missing).unwrap(), None);
    assert!(open_reporting(&missing).1.is_empty());
    // A zero-byte database file counts as fresh too.
    let empty = tempfile::tempdir().unwrap();
    std::fs::write(empty.path().join("flyonthewall.db"), b"").unwrap();
    assert_eq!(Storage::pending_upgrade(empty.path()).unwrap(), None);
    assert!(open_reporting(empty.path()).1.is_empty());

    // v2 file: pending, sized, and left byte-for-byte alone by the peek.
    let v2 = tempfile::tempdir().unwrap();
    v2_fixture(v2.path());
    let before = dir_listing(v2.path());
    let pending = Storage::pending_upgrade(v2.path()).unwrap().unwrap();
    assert_eq!(pending.from_version, 2);
    assert_eq!(pending.transcripts, (BULK_MEETINGS + 5) as u64);
    assert_eq!(dir_listing(v2.path()), before, "the peek writes nothing");
    assert_eq!(user_version(v2.path()), 2);
    assert_eq!(
        count(
            v2.path(),
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'transcript_segments'"
        ),
        0
    );
    let (s, seen) = open_reporting(v2.path());
    assert!(!seen.is_empty());
    drop(s);
    assert_eq!(Storage::pending_upgrade(v2.path()).unwrap(), None);
    assert!(open_reporting(v2.path()).1.is_empty(), "v3 reopen: no step");

    // A NEWER-stamped file re-runs the step (see SCHEMA_VERSION).
    db(v2.path())
        .pragma_update(None, "user_version", Storage::SCHEMA_VERSION + 1)
        .unwrap();
    let pending = Storage::pending_upgrade(v2.path()).unwrap().unwrap();
    assert_eq!(pending.from_version, Storage::SCHEMA_VERSION + 1);
    assert!(!open_reporting(v2.path()).1.is_empty());
    assert_eq!(user_version(v2.path()), Storage::SCHEMA_VERSION);

    // A pre-rebrand looma.db is what open will migrate; the peek reads it
    // without renaming it.
    let legacy = tempfile::tempdir().unwrap();
    v2_fixture(legacy.path());
    std::fs::rename(
        legacy.path().join("flyonthewall.db"),
        legacy.path().join("looma.db"),
    )
    .unwrap();
    assert!(Storage::pending_upgrade(legacy.path()).unwrap().is_some());
    assert!(!legacy.path().join("flyonthewall.db").exists());
    assert!(!open_reporting(legacy.path()).1.is_empty());
    assert!(legacy.path().join("flyonthewall.db").exists());
}

#[test]
fn upgrade_progress_runs_through_every_stage_to_total() {
    let tmp = tempfile::tempdir().unwrap();
    v2_fixture(tmp.path());
    let (_s, seen) = open_reporting(tmp.path());
    let (first, last) = (seen.first().unwrap(), seen.last().unwrap());
    assert_eq!((first.stage, first.done), (UpgradeStage::Segments, 0));
    assert_eq!(last.stage, UpgradeStage::Index);
    assert_eq!(last.done, last.total, "ends at total");
    assert!(seen.iter().all(|p| p.total == last.total), "fixed total");
    assert!(
        seen.windows(2)
            .all(|w| w[0].done <= w[1].done && w[0].stage <= w[1].stage),
        "monotonic: {seen:?}"
    );
    let stages: Vec<UpgradeStage> = seen.iter().map(|p| p.stage).fold(vec![], |mut v, s| {
        if v.last() != Some(&s) {
            v.push(s);
        }
        v
    });
    assert_eq!(
        stages,
        [
            UpgradeStage::Segments,
            UpgradeStage::Chunks,
            UpgradeStage::Citations,
            UpgradeStage::Index
        ]
    );
    // Throttled: a small library sends a handful of reports, not one per
    // meeting and stage.
    assert!(seen.len() < 20, "{} reports", seen.len());
    assert_eq!(user_version(tmp.path()), 3);
}

fn db(dir: &Path) -> Connection {
    Connection::open(dir.join("flyonthewall.db")).unwrap()
}

fn user_version(dir: &Path) -> i64 {
    db(dir)
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn count(dir: &Path, sql: &str) -> i64 {
    db(dir).query_row(sql, [], |r| r.get(0)).unwrap()
}

fn fts_integrity(dir: &Path) {
    db(dir)
        .execute(
            "INSERT INTO segments_fts (segments_fts) VALUES ('integrity-check')",
            [],
        )
        .unwrap();
}

fn rows(dir: &Path, sql: &str) -> Vec<String> {
    db(dir)
        .prepare(sql)
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

const TRANSCRIPT_ROWS: &str = "SELECT meeting_id||'|'||json||'|'||COALESCE(cleaned_json,'')
                                 FROM transcripts ORDER BY meeting_id";
/// Chunk rows in a form that exists in both v2 and v3 files.
const V2_CHUNK_ROWS: &str =
    "SELECT owner_kind||'|'||owner_id||'|'||text||'|'||COALESCE(hex(embedding),'')
                               FROM embedding_chunks ORDER BY id";

/// Everything v3 owns, for before/after comparisons.
fn v3_state(dir: &Path) -> BTreeMap<&'static str, Vec<String>> {
    let conn = db(dir);
    let mut out = BTreeMap::new();
    for (name, sql) in [
        ("chunks", "SELECT owner_kind||'|'||owner_id||'|'||chunk_index||'|'||title||'|'||text||'|'||
                           COALESCE(start_ms,'')||'|'||COALESCE(end_ms,'')||'|'||segment_ids_json||'|'||
                           variant||'|'||COALESCE(hex(embedding),'')||'|'||COALESCE(model,'')
                      FROM embedding_chunks ORDER BY owner_kind, owner_id, chunk_index"),
        ("segments", "SELECT meeting_id||'|'||seq||'|'||segment_id||'|'||variant||'|'||text
                        FROM transcript_segments ORDER BY meeting_id, seq"),
        ("locators", "SELECT owner_kind||'|'||owner_id||'|'||seg_id||'|'||text_hash||'|'||status
                        FROM citation_locators ORDER BY 1"),
        ("transcripts", "SELECT meeting_id||'|'||json||'|'||COALESCE(cleaned_json,'')
                           FROM transcripts ORDER BY meeting_id"),
        ("notes", "SELECT id||'|'||blocks_json||'|'||updated_at FROM notes ORDER BY id"),
        ("items", "SELECT id||'|'||segment_ids_json FROM meeting_items ORDER BY id"),
    ] {
        let rows: Vec<String> = conn
            .prepare(sql)
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        out.insert(name, rows);
    }
    out
}

fn filter(limit: usize, exclude_keep_local: bool) -> SearchFilter {
    SearchFilter {
        limit,
        exclude_keep_local,
        ..Default::default()
    }
}

#[test]
fn v2_database_migrates_to_v3() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = v2_fixture(tmp.path());
    let before_transcripts = rows(tmp.path(), TRANSCRIPT_ROWS);

    let started = std::time::Instant::now();
    let s = Storage::open(tmp.path()).unwrap();
    eprintln!(
        "v3 step over {} segments: {:?}",
        BULK_MEETINGS * BULK_SEGMENTS + 7,
        started.elapsed()
    );
    assert_eq!(user_version(tmp.path()), 3);

    // Source rows are untouched.
    let state = v3_state(tmp.path());
    assert_eq!(state["transcripts"], before_transcripts);

    // Per-segment index: one row per segment, polished text by id.
    assert_eq!(
        count(tmp.path(), "SELECT COUNT(*) FROM transcript_segments"),
        (BULK_MEETINGS * BULK_SEGMENTS + 7) as i64
    );
    fts_integrity(tmp.path());
    let hits = s.search_segments("fuel", &filter(50, false)).unwrap();
    let budget_hit = hits.iter().find(|h| h.meeting_id == "m-budget").unwrap();
    assert_eq!(budget_hit.segment_id, "b1");
    assert_eq!(
        (budget_hit.start_ms, budget_hit.end_ms),
        (Some(0), Some(1_800))
    );
    assert_eq!(budget_hit.variant, "polished");
    assert!(s
        .search_segments("fule", &filter(50, false))
        .unwrap()
        .is_empty());
    // Labels aren't indexed per segment.
    assert!(s
        .search_segments("Dana", &filter(50, false))
        .unwrap()
        .is_empty());

    // Transcript chunks: label-free, exact spans, polished where present,
    // and pending re-embedding (their text changed). Note chunks keep vectors.
    let conn = db(tmp.path());
    let (text, ids, end_ms, variant, emb): (String, String, Option<i64>, String, Option<Vec<u8>>) =
        conn.query_row(
            "SELECT text, segment_ids_json, end_ms, variant, embedding FROM embedding_chunks
              WHERE owner_kind = 'transcript' AND owner_id = 'm-budget'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        text,
        "We agreed the fuel bid goes out at $4.90.\nand we ship the renewal on friday"
    );
    assert_eq!(ids, r#"["b1","b2"]"#, "empty segment b3 isn't chunked");
    assert_eq!(end_ms, Some(3_800));
    assert_eq!(variant, "polished");
    assert!(emb.is_none(), "changed text → pending");
    let note_emb: Option<Vec<u8>> = conn
        .query_row(
            "SELECT embedding FROM embedding_chunks WHERE owner_kind = 'note' AND owner_id = 'n-budget'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(note_emb.is_some(), "note chunk unchanged → vector kept");
    assert_eq!(
        count(
            tmp.path(),
            "SELECT COUNT(*) FROM embedding_chunks WHERE owner_id = 'm-gone'"
        ),
        0,
        "orphan chunk dropped"
    );
    assert_eq!(
        count(
            tmp.path(),
            "SELECT COUNT(*) FROM embedding_chunks WHERE text LIKE '%Dana:%' OR text LIKE '%You:%'"
        ),
        0,
        "no labelled chunk survives"
    );
    let bulk_chunks = count(
        tmp.path(),
        "SELECT COUNT(*) FROM embedding_chunks WHERE owner_id LIKE 'm-bulk-%'",
    );
    assert!(
        bulk_chunks > BULK_MEETINGS as i64,
        "bulk meetings re-chunked: {bulk_chunks}"
    );
    assert_eq!(
        count(
            tmp.path(),
            "SELECT COUNT(*) FROM embedding_chunks WHERE owner_kind = 'transcript' AND segment_ids_json = '[]'"
        ),
        0
    );

    // Citation locators for existing AI blocks and items, all `ok`.
    let block = s
        .citation_locators(OWNER_NOTE_BLOCK, &fx.budget_block)
        .unwrap();
    assert_eq!(
        block.iter().map(|l| l.seg_id.as_str()).collect::<Vec<_>>(),
        ["b1", "b2"]
    );
    assert!(
        block.iter().all(|l| l.status == CitationStatus::Ok),
        "{block:?}"
    );
    assert_eq!(
        s.citation_locators(OWNER_NOTE_BLOCK, &fx.case_block)
            .unwrap()
            .len(),
        1
    );
    let item = s.citation_locators(OWNER_ITEM, &fx.budget_item).unwrap();
    assert_eq!(item.len(), 1);
    assert_eq!(
        (item[0].seg_id.as_str(), item[0].start_ms, item[0].end_ms),
        ("b2", 2_000, 3_800)
    );

    // Notes weren't touched (no updated_at bump, blocks byte-identical).
    let note = s.get_note(&fx.budget_note).unwrap();
    assert!(matches!(note.blocks[0].origin, BlockOrigin::Ai { .. }));
}

#[test]
fn rerun_is_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    v2_fixture(tmp.path());
    drop(Storage::open(tmp.path()).unwrap());
    let first = v3_state(tmp.path());
    drop(Storage::open(tmp.path()).unwrap());
    assert_eq!(
        v3_state(tmp.path()),
        first,
        "a v3 open of a v3 file changes nothing"
    );
    // A forced re-run of the step (what a downgrade round-trip causes) is
    // also a no-op on a file nobody else wrote to.
    db(tmp.path())
        .pragma_update(None, "user_version", 2)
        .unwrap();
    drop(Storage::open(tmp.path()).unwrap());
    assert_eq!(v3_state(tmp.path()), first);
    assert_eq!(user_version(tmp.path()), 3);
}

#[test]
fn a_failed_step_leaves_the_file_unchanged_and_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    v2_fixture(tmp.path());
    // Make the step fail late: at the locator reconciliation, after the
    // segment index and the re-chunk have run inside the same transaction.
    // (A per-note or per-meeting failure is only logged; this batch
    // statement is not.) A stale locator row makes its UPDATE fire.
    db(tmp.path())
        .execute_batch(
            "CREATE TABLE citation_locators (
                owner_kind TEXT NOT NULL, owner_id TEXT NOT NULL, note_id TEXT,
                meeting_id TEXT NOT NULL, seg_id TEXT NOT NULL,
                start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
                text_hash TEXT NOT NULL, speaker_key TEXT,
                status TEXT NOT NULL DEFAULT 'ok',
                PRIMARY KEY (owner_kind, owner_id, seg_id));
             INSERT INTO citation_locators
                 (owner_kind, owner_id, meeting_id, seg_id, start_ms, end_ms, text_hash)
                 VALUES ('item', 'item-case', 'm-case', 'gone-seg', 0, 0, 'h');
             CREATE TRIGGER boom BEFORE UPDATE ON citation_locators
             BEGIN SELECT RAISE(ABORT, 'simulated crash'); END;",
        )
        .unwrap();
    let chunks_before = rows(tmp.path(), V2_CHUNK_ROWS);

    let err = Storage::open(tmp.path()).err().expect("step must fail");
    assert!(err.to_string().contains("simulated crash"), "{err}");
    assert_eq!(user_version(tmp.path()), 2, "not stamped");
    assert_eq!(
        count(tmp.path(), "SELECT COUNT(*) FROM transcript_segments"),
        0,
        "rolled back"
    );
    assert_eq!(
        rows(tmp.path(), V2_CHUNK_ROWS),
        chunks_before,
        "rolled back"
    );

    db(tmp.path()).execute_batch("DROP TRIGGER boom;").unwrap();
    let s = Storage::open(tmp.path()).unwrap();
    assert_eq!(user_version(tmp.path()), 3);
    assert_eq!(
        s.citation_locators(OWNER_NOTE_BLOCK, &v2_block_id(tmp.path()))
            .unwrap()
            .len(),
        2
    );
}

fn v2_block_id(dir: &Path) -> String {
    let json: String = db(dir)
        .query_row(
            "SELECT blocks_json FROM notes WHERE id = 'n-budget'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let blocks: Vec<NoteBlock> = serde_json::from_str(&json).unwrap();
    blocks[0].id.clone()
}

#[test]
fn keep_local_is_filtered_inside_sql_on_every_read_path() {
    let tmp = tempfile::tempdir().unwrap();
    v2_fixture(tmp.path());
    let s = Storage::open(tmp.path()).unwrap();
    // Re-embed what v3 re-chunked: hidden meetings get the vector closest
    // to the query.
    let pending = s.pending_embedding_chunks(MODEL, 10_000).unwrap();
    let conn = db(tmp.path());
    let vectors: Vec<(i64, Vec<f32>)> = pending
        .iter()
        .map(|p| {
            let owner: String = conn
                .query_row(
                    "SELECT owner_id FROM embedding_chunks WHERE id = ?1",
                    [p.id],
                    |r| r.get(0),
                )
                .unwrap();
            let v = match owner.as_str() {
                "m-case" | "m-one" | "m-noteless" => vec![1.0, 0.0, 0.0],
                "m-budget" | "m-standup" => vec![0.8, 0.2, 0.0],
                _ => vec![0.0, 1.0, 0.0],
            };
            (p.id, v)
        })
        .collect();
    s.store_chunk_embeddings(MODEL, &vectors).unwrap();

    let secret = |text: &str| text.contains("SECRET");

    // vector_search: limit applies AFTER the keep-local filter, so a top-2
    // query still returns two shareable hits although the two hidden
    // meetings score higher.
    let all = s
        .vector_search(&[1.0, 0.0, 0.0], MODEL, &filter(0, false), 2)
        .unwrap();
    assert!(
        all.iter().all(|h| secret(&h.text)),
        "unfiltered: hidden ones win"
    );
    let shared = s
        .vector_search(&[1.0, 0.0, 0.0], MODEL, &filter(0, true), 2)
        .unwrap();
    assert_eq!(shared.len(), 2);
    assert!(
        shared
            .iter()
            .all(|h| !secret(&h.text) && !secret(&h.snippet)),
        "{shared:?}"
    );
    let budget = shared
        .iter()
        .find(|h| h.meeting_id.as_deref() == Some("m-budget"))
        .unwrap();
    assert_eq!(budget.kind, SearchHitKind::Transcript);
    assert_eq!(budget.segment_ids, ["b1", "b2"]);
    assert_eq!(budget.end_ms, Some(3_800));
    // the orphan chunk never comes back (dropped by v3, and filtered anyway)
    let everything = s
        .vector_search(&[1.0, 0.0, 0.0], MODEL, &filter(0, false), 10_000)
        .unwrap();
    assert!(everything.iter().all(|h| !h.text.contains("SECRETORPHAN")));
    // The note-less meeting is returned unfiltered, never when filtered.
    assert!(everything
        .iter()
        .any(|h| h.meeting_id.as_deref() == Some("m-noteless")));
    let shared_all = s
        .vector_search(&[1.0, 0.0, 0.0], MODEL, &filter(0, true), 10_000)
        .unwrap();
    assert!(shared_all.iter().all(|h| !matches!(
        h.meeting_id.as_deref(),
        Some("m-case" | "m-one" | "m-noteless")
    )));

    // search_segments
    let hidden = |m: &str| matches!(m, "m-case" | "m-one" | "m-noteless");
    let seg_all = s.search_segments("fuel", &filter(50, false)).unwrap();
    for m in ["m-case", "m-one", "m-noteless"] {
        assert!(seg_all.iter().any(|h| h.meeting_id == m), "{m} unfiltered");
    }
    let seg_shared = s.search_segments("fuel", &filter(50, true)).unwrap();
    assert!(!seg_shared.is_empty());
    assert!(
        seg_shared.iter().all(|h| !hidden(&h.meeting_id)),
        "{seg_shared:?}"
    );
    // paging can't reveal hidden rows: offset over the shareable list only
    let page = s.search_segments("secretcase", &filter(50, true)).unwrap();
    assert!(page.is_empty());

    // list_meetings_filtered
    let listed: Vec<String> = s
        .list_meetings_filtered(&MeetingFilter {
            exclude_keep_local: true,
            limit: 100,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(listed.len(), BULK_MEETINGS + 2);
    assert!(!listed.iter().any(|m| hidden(m)));
    let unfiltered = s
        .list_meetings_filtered(&MeetingFilter {
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(unfiltered.len(), BULK_MEETINGS + 5);
    assert_eq!(unfiltered[0].id, "m-noteless", "newest, shown unfiltered");
    // limit after the filter: newest two shareable meetings
    let top2: Vec<String> = s
        .list_meetings_filtered(&MeetingFilter {
            exclude_keep_local: true,
            limit: 2,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(top2, ["m-standup", "m-budget"]);
    let until: Vec<String> = s
        .list_meetings_filtered(&MeetingFilter {
            since: Some("2026-09-02T00:00:00Z".parse().unwrap()),
            until: Some("2026-09-04T23:00:00Z".parse().unwrap()),
            limit: 100,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(until, ["m-one", "m-case", "m-budget"]);
    let in_folder = s
        .list_meetings_filtered(&MeetingFilter {
            folder_id: Some("f-clients".into()),
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(in_folder.len(), 1);

    // A chunk of an owner kind the filter doesn't know (e.g. a future
    // attachment kind) fails closed, in search and in the cloud-embedding
    // queue alike.
    db(tmp.path())
        .execute(
            "INSERT INTO embedding_chunks
             (owner_kind, owner_id, chunk_index, title, text, embedding, model)
             VALUES ('attachment', 'att-1', 0, 'Scan', 'SECRETATTACHMENT', ?1, ?2)",
            (
                [1.0f32, 0.0, 0.0]
                    .iter()
                    .flat_map(|x| x.to_le_bytes())
                    .collect::<Vec<u8>>(),
                MODEL,
            ),
        )
        .unwrap();
    let with_kind = |exclude: bool| {
        s.vector_search(&[1.0, 0.0, 0.0], MODEL, &filter(0, exclude), 10_000)
            .unwrap()
            .iter()
            .any(|h| h.text.contains("SECRETATTACHMENT"))
    };
    assert!(with_kind(false), "fixture chunk is searchable unfiltered");
    assert!(!with_kind(true), "unknown owner kind fails closed");
    db(tmp.path())
        .execute(
            "UPDATE embedding_chunks SET embedding = NULL WHERE owner_id = 'att-1'",
            [],
        )
        .unwrap();
    assert!(s
        .pending_embedding_chunks_shareable(MODEL, 10_000)
        .unwrap()
        .iter()
        .all(|p| !p.text.contains("SECRETATTACHMENT")));
    // The note-less meeting's chunks never reach the remote-embedding
    // queue either (nor its backlog count).
    let backlog_before = s.embedding_backlog_shareable(MODEL).unwrap();
    db(tmp.path())
        .execute(
            "UPDATE embedding_chunks SET embedding = NULL WHERE owner_id = 'm-noteless'",
            [],
        )
        .unwrap();
    assert!(s.embedding_backlog(MODEL).unwrap() > s.embedding_backlog_shareable(MODEL).unwrap());
    assert!(s
        .pending_embedding_chunks_shareable(MODEL, 10_000)
        .unwrap()
        .iter()
        .all(|p| !p.text.contains("SECRETNOTELESS")));
    assert!(
        s.pending_embedding_chunks(MODEL, 10_000)
            .unwrap()
            .iter()
            .any(|p| p.text.contains("SECRETNOTELESS")),
        "fixture chunk is pending"
    );
    assert_eq!(
        s.embedding_backlog_shareable(MODEL).unwrap(),
        backlog_before
    );

    // A meeting moved under the keep-local folder later disappears at once.
    s.move_note("n-budget", Some("f-cases")).unwrap();
    assert!(s
        .vector_search(&[1.0, 0.0, 0.0], MODEL, &filter(0, true), 100)
        .unwrap()
        .iter()
        .all(|h| h.meeting_id.as_deref() != Some("m-budget") && h.note_id != "n-budget"));
    assert!(s
        .search_segments("fuel", &filter(50, true))
        .unwrap()
        .iter()
        .all(|h| h.meeting_id != "m-budget"));
}

/// What a v2 build does to a v3 file, replayed as the exact SQL its code
/// runs: `migrate` stamps user_version back to 2; a re-transcribe saves a
/// transcript with NEW segment ids and writes labelled chunks with the old
/// column list; `purge_meeting` deletes another meeting; re-extraction
/// replaces items. Meanwhile the triggers keep the segment index and the
/// locators consistent; the next v3 open re-runs the step and heals the rest.
#[test]
fn an_older_build_writing_to_a_v3_file_stays_safe_and_heals() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = v2_fixture(tmp.path());
    drop(Storage::open(tmp.path()).unwrap());
    assert_eq!(user_version(tmp.path()), 3);

    {
        let conn = db(tmp.path());
        conn.pragma_update(None, "user_version", 2).unwrap(); // v2 migrate()
                                                              // v2 re-transcribe of m-budget: new ids, same speech for b1.
        let retranscribed = transcript(
            "m-budget",
            vec![
                seg(
                    "x1",
                    "mic",
                    50,
                    "we agreed the fule bid goes out at four ninety",
                ),
                seg("x2", "spk_0", 2_050, "background noise only"),
            ],
        );
        v2_save_transcript(&conn, &retranscribed, "Q3 budget", None);
        conn.execute(
            "UPDATE transcripts SET cleaned_json = NULL WHERE meeting_id = 'm-budget'",
            [],
        )
        .unwrap(); // v2 clear_cleaned_transcript
                   // v2 purge_meeting("m-case")
        let tx = conn.unchecked_transaction().unwrap();
        for sql in [
            "DELETE FROM transcripts WHERE meeting_id = ?1",
            "DELETE FROM transcripts_fts WHERE meeting_id = ?1",
            "DELETE FROM embedding_chunks WHERE owner_kind = 'transcript' AND owner_id = ?1",
            "DELETE FROM meeting_items WHERE meeting_id = ?1",
            "DELETE FROM transcription_jobs WHERE meeting_id = ?1",
            "DELETE FROM meetings WHERE id = ?1",
        ] {
            conn.execute(sql, ["m-case"]).unwrap();
        }
        tx.commit().unwrap();
        // v2 replace_meeting_items for m-budget (new item citing x1)
        conn.execute(
            "DELETE FROM meeting_items WHERE meeting_id = 'm-budget'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meeting_items (id, meeting_id, kind, text, segment_ids_json, created_at, extracted_by)
             VALUES ('item-new', 'm-budget', 'decision', 'bid at 4.90', '[\"x1\"]', '2026-09-10T00:00:00Z', 'v2')",
            [],
        )
        .unwrap();

        // While the v2 build runs: the segment index already follows its
        // writes (triggers), the deleted meeting's text is gone from it,
        // and citations of vanished segments read as changed.
        let segs: Vec<String> = conn
            .prepare("SELECT segment_id FROM transcript_segments WHERE meeting_id = 'm-budget' ORDER BY seq")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(segs, ["x1", "x2"]);
        let case_rows: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM transcript_segments WHERE meeting_id = 'm-case')
                      + (SELECT COUNT(*) FROM citation_locators WHERE meeting_id = 'm-case')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(case_rows, 0);
        let stale: Vec<String> = conn
            .prepare("SELECT status FROM citation_locators WHERE meeting_id = 'm-budget' AND owner_kind = 'note_block'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(stale, ["changed", "changed"]);
    }
    fts_integrity(tmp.path());
    assert_eq!(
        count(
            tmp.path(),
            "SELECT COUNT(*) FROM segments_fts WHERE segments_fts MATCH 'secretcase'"
        ),
        0,
        "deleted meeting's text left the keyword index"
    );

    // Next v3 open: step re-runs (version was stamped back) and heals.
    let s = Storage::open(tmp.path()).unwrap();
    assert_eq!(user_version(tmp.path()), 3);
    let conn = db(tmp.path());
    let (text, ids): (String, String) = conn
        .query_row(
            "SELECT text, segment_ids_json FROM embedding_chunks
              WHERE owner_kind = 'transcript' AND owner_id = 'm-budget'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(
        !text.contains("You:"),
        "v2's labelled chunk was re-chunked: {text}"
    );
    assert_eq!(ids, r#"["x1","x2"]"#);
    // The v2 build re-transcribed without re-anchoring; v3 can't recover the
    // old text afterwards, so those citations honestly stay `changed`.
    let block = s
        .citation_locators(OWNER_NOTE_BLOCK, &fx.budget_block)
        .unwrap();
    assert!(
        block.iter().all(|l| l.status == CitationStatus::Changed),
        "{block:?}"
    );
    // Locators for what v2 wrote are recorded; the replaced item's are gone.
    assert!(s
        .citation_locators(OWNER_ITEM, &fx.budget_item)
        .unwrap()
        .is_empty());
    let new_item = s.citation_locators(OWNER_ITEM, "item-new").unwrap();
    assert_eq!(
        (new_item.len(), new_item[0].status),
        (1, CitationStatus::Ok)
    );
    assert!(s
        .citation_locators(OWNER_NOTE_BLOCK, &fx.case_block)
        .unwrap()
        .is_empty());
    // Rerun after heal: no-op.
    let healed = v3_state(tmp.path());
    drop(s);
    drop(Storage::open(tmp.path()).unwrap());
    assert_eq!(v3_state(tmp.path()), healed);
}

#[test]
fn erase_all_content_leaves_nothing_in_the_segment_index() {
    let tmp = tempfile::tempdir().unwrap();
    v2_fixture(tmp.path());
    let s = Storage::open(tmp.path()).unwrap();
    assert!(!s
        .search_segments("fuel", &filter(10, false))
        .unwrap()
        .is_empty());
    let outcome = s.erase_all_content().unwrap();
    assert!(outcome.checkpointed);
    fts_integrity(tmp.path());
    for table in [
        "transcript_segments",
        "citation_locators",
        "embedding_chunks",
    ] {
        assert_eq!(
            count(tmp.path(), &format!("SELECT COUNT(*) FROM {table}")),
            0,
            "{table}"
        );
    }
    assert_eq!(
        count(
            tmp.path(),
            "SELECT COUNT(*) FROM segments_fts WHERE segments_fts MATCH 'fuel'"
        ),
        0
    );
    // The index's own term data is empty too (no lingering vocabulary).
    let conn = db(tmp.path());
    conn.execute_batch("CREATE VIRTUAL TABLE temp.v USING fts5vocab(main, segments_fts, row);")
        .unwrap();
    let terms: i64 = conn
        .query_row("SELECT COUNT(*) FROM temp.v", [], |r| r.get(0))
        .unwrap();
    assert_eq!(terms, 0);
}

/// A file stamped v3 by an earlier v3 build (before review) has
/// `citation_locators` without `speaker_key`. Opening it repairs the column,
/// so saving AI blocks and items doesn't fail after the data was written.
#[test]
fn early_v3_file_without_speaker_key_is_repaired() {
    let tmp = tempfile::tempdir().unwrap();
    let (note_id, mid) = {
        let s = Storage::open(tmp.path()).unwrap();
        let note = s.create_note("n", None).unwrap();
        let m = s.create_meeting("n", &note.id, &[]).unwrap();
        s.save_transcript(&transcript(
            &m.id,
            vec![seg("a", "spk_0", 0, "hello there")],
        ))
        .unwrap();
        (note.id, m.id)
    };
    db(tmp.path())
        .execute_batch("ALTER TABLE citation_locators DROP COLUMN speaker_key;")
        .unwrap();
    assert_eq!(user_version(tmp.path()), 3);

    let s = Storage::open(tmp.path()).unwrap();
    let b = NoteBlock::ai("- hi", vec!["a".into()]);
    s.update_note_blocks(&note_id, std::slice::from_ref(&b))
        .expect("blocks save after repair");
    let l = s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap();
    assert_eq!(
        (l[0].status, l[0].speaker_key.as_deref()),
        (CitationStatus::Ok, Some("spk_0"))
    );
    // The MCP's read-only open accepts the repaired file.
    drop(s);
    assert!(Storage::open_existing(tmp.path()).is_ok());
    assert!(!mid.is_empty());
}
