//! v1 → v2 upgrade against a fixture shaped like a real `%APPDATA%/FlyOnTheWall`
//! data dir: UUID-named artifacts with DB rows, one orphaned recordings
//! folder (with transcript mirrors but no meeting row), and one orphaned
//! note mirror (no notes row). After `Storage::open`, everything must be
//! human-readable, playback paths must resolve, transcripts must open,
//! search must work, and orphans must be preserved — never deleted.

use std::path::Path;

use chrono::{DateTime, Utc};
use fly_storage::{naming, SearchHitKind, Storage};
use rusqlite::Connection;

const NOTE_ID: &str = "3aae37f4-168f-4504-8c19-e69887918e60";
const NOTE_TITLE: &str = "TB<1:1>SSP July 2 2026"; // '<', ':', '>' are illegal on Windows
const MEETING_ID: &str = "862e2405-a77a-48ad-af38-fefcda797fa0";
const MEETING_TITLE: &str = "Meeting 2026-07-02 12:06";
const STARTED_AT: &str = "2026-07-02T16:06:00Z";
const ORPHAN_REC_ID: &str = "08b9d116-055c-4299-89a9-35b5174afaff";
const ORPHAN_NOTE_ID: &str = "9472c651-c615-4b67-ba7d-360ed1ae8734";

fn started_at() -> DateTime<Utc> {
    STARTED_AT.parse().unwrap()
}

/// Build the data dir exactly as a v1 build would have left it.
fn v1_fixture(dir: &Path) {
    for sub in ["notes", "transcripts", "attachments", "recordings"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let conn = Connection::open(dir.join("looma.db")).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE notes (
            id TEXT PRIMARY KEY, title TEXT NOT NULL, folder_id TEXT,
            meeting_id TEXT, scratchpad TEXT NOT NULL DEFAULT '',
            blocks_json TEXT NOT NULL DEFAULT '[]',
            attachments_json TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL
        );
        CREATE TABLE meetings (
            id TEXT PRIMARY KEY, title TEXT NOT NULL, note_id TEXT NOT NULL,
            attendees_json TEXT NOT NULL DEFAULT '[]',
            started_at TEXT NOT NULL, ended_at TEXT, recording_json TEXT
        );
        CREATE TABLE transcripts (meeting_id TEXT PRIMARY KEY, json TEXT NOT NULL);
        CREATE VIRTUAL TABLE notes_fts USING fts5(note_id UNINDEXED, title, body);
        CREATE VIRTUAL TABLE transcripts_fts USING fts5(meeting_id UNINDEXED, body);
        PRAGMA user_version = 1;
        "#,
    )
    .unwrap();

    conn.execute(
        "INSERT INTO notes (id, title, meeting_id, scratchpad, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'budget approved', ?4, ?4)",
        (NOTE_ID, NOTE_TITLE, MEETING_ID, STARTED_AT),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO notes_fts (note_id, title, body) VALUES (?1, ?2, 'budget approved')",
        (NOTE_ID, NOTE_TITLE),
    )
    .unwrap();

    let recording_json = serde_json::json!({
        "mic_path": format!("recordings/{MEETING_ID}/recording.mic.wav"),
        "system_path": format!("recordings/{MEETING_ID}/recording.system.wav"),
        "mixed_path": format!("recordings/{MEETING_ID}/recording.mixed.wav"),
        "duration_ms": 61_000,
    });
    conn.execute(
        "INSERT INTO meetings (id, title, note_id, started_at, ended_at, recording_json)
         VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
        (
            MEETING_ID,
            MEETING_TITLE,
            NOTE_ID,
            STARTED_AT,
            recording_json.to_string(),
        ),
    )
    .unwrap();

    let transcript_json = serde_json::json!({
        "meeting_id": MEETING_ID,
        "language": "en",
        "engine": "whisper.cpp",
        "segments": [{
            "id": "seg-1", "speaker_key": "mic", "start_ms": 0, "end_ms": 1500,
            "text": "the budget is approved", "words": []
        }],
        "speakers": [{ "key": "mic", "label": "You" }],
    });
    conn.execute(
        "INSERT INTO transcripts (meeting_id, json) VALUES (?1, ?2)",
        (MEETING_ID, transcript_json.to_string()),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO transcripts_fts (meeting_id, body) VALUES (?1, 'You: the budget is approved')",
        [MEETING_ID],
    )
    .unwrap();

    // meeting folder: real WAVs plus the 16 kHz leftovers old pipelines kept
    let rec_dir = dir.join("recordings").join(MEETING_ID);
    std::fs::create_dir_all(&rec_dir).unwrap();
    for f in [
        "recording.mic.wav",
        "recording.system.wav",
        "recording.mixed.wav",
        "mic.16k.wav",
        "system.16k.wav",
    ] {
        std::fs::write(rec_dir.join(f), b"RIFF").unwrap();
    }
    std::fs::write(
        dir.join("transcripts").join(format!("{MEETING_ID}.md")),
        "# transcript",
    )
    .unwrap();
    std::fs::write(
        dir.join("transcripts").join(format!("{MEETING_ID}.json")),
        transcript_json.to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("notes").join(format!("{NOTE_ID}.md")),
        "# TB<1:1>SSP July 2 2026\n\nbudget approved",
    )
    .unwrap();

    // orphaned recording folder + transcript mirrors, no DB rows
    let orphan_dir = dir.join("recordings").join(ORPHAN_REC_ID);
    std::fs::create_dir_all(&orphan_dir).unwrap();
    std::fs::write(orphan_dir.join("recording.mixed.wav"), b"RIFF").unwrap();
    std::fs::write(orphan_dir.join("track.16k.wav"), b"RIFF").unwrap();
    std::fs::write(
        dir.join("transcripts").join(format!("{ORPHAN_REC_ID}.md")),
        "# orphan transcript",
    )
    .unwrap();
    std::fs::write(
        dir.join("transcripts")
            .join(format!("{ORPHAN_REC_ID}.json")),
        "{}",
    )
    .unwrap();

    // orphaned note mirror, no DB row
    std::fs::write(
        dir.join("notes").join(format!("{ORPHAN_NOTE_ID}.md")),
        "# Design QA note",
    )
    .unwrap();
}

#[test]
fn v1_data_dir_migrates_to_human_readable_layout() {
    let tmp = tempfile::tempdir().unwrap();
    v1_fixture(tmp.path());

    let storage = Storage::open(tmp.path()).unwrap();

    // meeting folder is named by local date + the NOTE's (curated) title
    let label = naming::disk_label(started_at(), NOTE_TITLE);
    assert!(label.ends_with("TB 1 1 SSP July 2 2026"), "label: {label}");
    let meeting_dir = tmp.path().join("recordings").join(&label);
    assert!(meeting_dir.is_dir(), "renamed meeting dir missing");

    // recording_json rewritten; every playback path resolves on disk
    let meeting = storage.get_meeting(MEETING_ID).unwrap();
    let rec = meeting.recording.expect("recording survived");
    for path in [&rec.mic_path, &rec.system_path, &rec.mixed_path] {
        let rel = path.as_ref().expect("path kept");
        assert!(
            rel.starts_with(&format!("recordings/{label}/")),
            "path not rewritten: {rel}"
        );
        assert!(tmp.path().join(rel).exists(), "file missing: {rel}");
    }

    // transcript mirrors live inside the meeting folder now
    assert!(meeting_dir.join("transcript.md").exists());
    assert!(meeting_dir.join("transcript.json").exists());
    assert!(!tmp
        .path()
        .join("transcripts")
        .join(format!("{MEETING_ID}.md"))
        .exists());

    // stale 16 kHz intermediates are gone, the real WAVs are not
    assert!(!meeting_dir.join("mic.16k.wav").exists());
    assert!(!meeting_dir.join("system.16k.wav").exists());
    assert!(meeting_dir.join("recording.mic.wav").exists());

    // transcripts still open, search still works (note + transcript hits)
    assert!(storage.get_transcript(MEETING_ID).unwrap().is_some());
    let hits = storage.search("budget", 10).unwrap();
    assert!(hits
        .iter()
        .any(|h| h.kind == SearchHitKind::Note && h.note_id == NOTE_ID));
    assert!(hits
        .iter()
        .any(|h| h.kind == SearchHitKind::Transcript && h.note_id == NOTE_ID));

    // note mirror renamed to date + title, content preserved
    let note_label = naming::disk_label(started_at(), NOTE_TITLE);
    let note_file = tmp.path().join("notes").join(format!("{note_label}.md"));
    assert!(note_file.exists(), "renamed note mirror missing");
    assert!(std::fs::read_to_string(&note_file)
        .unwrap()
        .contains("budget approved"));

    // orphans are parked under _unlinked, together with their transcripts
    let parked = tmp
        .path()
        .join("recordings")
        .join("_unlinked")
        .join(ORPHAN_REC_ID);
    assert!(parked.join("recording.mixed.wav").exists());
    assert!(parked.join("transcript.md").exists());
    assert!(parked.join("transcript.json").exists());
    assert!(tmp
        .path()
        .join("notes")
        .join("_unlinked")
        .join(format!("{ORPHAN_NOTE_ID}.md"))
        .exists());

    // reopening is a no-op: nothing gets renamed or duplicated again
    drop(storage);
    let storage = Storage::open(tmp.path()).unwrap();
    assert!(meeting_dir.is_dir());
    assert!(!tmp
        .path()
        .join("recordings")
        .join(format!("{label} (2)"))
        .exists());
    let rec = storage
        .get_meeting(MEETING_ID)
        .unwrap()
        .recording
        .expect("recording still attached");
    assert!(tmp.path().join(rec.mixed_path.unwrap()).exists());
}
