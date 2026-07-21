//! fly-storage: SQLite (bundled, FTS5) as the search/index layer plus
//! portable markdown/JSON files on disk as the source of truth the user owns.
//!
//! Layout under the user-visible data dir (human-readable names since v2;
//! see naming.rs for the sanitization rules and migrations.rs for upgrades):
//!   flyonthewall.db                   — index (folders, notes, meetings, FTS)
//!   notes/<date> <title>.md           — plain-markdown mirror of each note
//!   notes/_unlinked/…                 — mirrors with no DB row (preserved)
//!   recordings/<date> <title>/        — one folder per meeting:
//!     recording.{mic,system,mixed}.wav, source.<ext> (imports),
//!     transcript.md, transcript.json,
//!     recording.manifest.json (portability manifest; see recovery.rs),
//!     note.json (faithful note mirror: scratchpad + blocks)
//!   recordings/_unlinked/…            — recordings with no DB row (preserved)
//!   transcripts/…                     — legacy fallback for transcripts whose
//!                                       meeting folder is unresolvable
//!   attachments/<note_id>/…           — attached files, referenced relatively

mod attachments;
pub mod embeddings;
mod folders;
pub mod hybrid;
mod items;
mod jobs;
mod meetings;
mod migrations;
pub mod naming;
mod notes;
mod recovery;
mod search;
mod settings;
mod templates;
mod transcripts;

pub use embeddings::{PendingChunk, VectorHit};
pub use items::ItemFilter;
pub use jobs::{TranscriptionJob, JOB_DONE, JOB_FAILED, JOB_QUEUED, JOB_RUNNING};
pub use meetings::recording_dir_rel;
pub use notes::NoteSummary;
pub use recovery::{
    HealReport, PortableNote, RecordingManifest, MANIFEST_VERSION, NOTE_MIRROR, RECORDING_MANIFEST,
};
pub use search::{SearchFilter, SearchHit, SearchHitKind};
pub use transcripts::SpeakerSnapshot;

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid operation: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StorageError>;

/// Retry policy for [`Storage::open_with_retry`].
///
/// The startup DB open has twice failed with "database disk image is malformed"
/// against a file that was provably fine — `integrity_check` passed on copies
/// both times (WAL applied), and relaunching minutes later worked with zero
/// repair. Some external process interferes at the exact moment of open, then
/// clears. We can't tell a transient from real corruption at open time, so we
/// retry a handful of times over a short window before surfacing the failure;
/// a genuinely corrupt DB just fails every attempt and still ends up reported.
#[derive(Debug, Clone, Copy)]
pub struct OpenRetry {
    /// Total number of open attempts (clamped to at least 1).
    pub attempts: u32,
    /// Delay slept between attempts.
    pub backoff: Duration,
}

impl OpenRetry {
    /// Startup policy: 6 attempts, 2.5s apart — up to ~12.5s of retrying, which
    /// covers the observed transient (cleared well within a minute both times)
    /// without making a truly corrupt DB wait long before the error dialog.
    pub const STARTUP: Self = Self {
        attempts: 6,
        backoff: Duration::from_millis(2500),
    };
}

pub struct Storage {
    conn: Connection,
    data_dir: PathBuf,
}

impl Storage {
    /// On-disk name of the SQLite index. Pre-rebrand builds used `looma.db`;
    /// `migrations::rename_legacy_db` renames it in place on open.
    const DB_FILE: &str = "flyonthewall.db";

    /// Open (creating if needed) the app data dir and its SQLite index.
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        for sub in ["notes", "transcripts", "attachments", "recordings"] {
            std::fs::create_dir_all(data_dir.join(sub))?;
        }
        migrations::rename_legacy_db(data_dir)?;
        let conn = Connection::open(data_dir.join(Self::DB_FILE))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Self::migrate(&conn, data_dir)?;
        let storage = Self {
            conn,
            data_dir: data_dir.to_path_buf(),
        };
        storage.seed_builtin_templates()?;
        Ok(storage)
    }

    /// Like [`open`](Self::open) but retries transient failures with backoff
    /// per `retry` before giving up. Each failed attempt is logged with its
    /// number and error text so a recurring interferer finally leaves evidence.
    /// The last attempt's error is returned unchanged, so the caller's fatal
    /// path (e.g. the startup error dialog) is reached only after retrying.
    pub fn open_with_retry(data_dir: &Path, retry: OpenRetry) -> Result<Self> {
        Self::retry_open(retry, || Self::open(data_dir))
    }

    /// Retry core, generic over the open closure so a test can drive the
    /// "fails N times, then succeeds" path without an external interferer.
    fn retry_open<F>(retry: OpenRetry, mut open: F) -> Result<Self>
    where
        F: FnMut() -> Result<Self>,
    {
        let attempts = retry.attempts.max(1);
        let mut last_err = None;
        for attempt in 1..=attempts {
            match open() {
                Ok(storage) => return Ok(storage),
                Err(e) => {
                    tracing::warn!(
                        attempt,
                        attempts,
                        error = %e,
                        "storage open failed"
                    );
                    last_err = Some(e);
                    if attempt < attempts {
                        std::thread::sleep(retry.backoff);
                    }
                }
            }
        }
        Err(last_err.expect("attempts >= 1 guarantees at least one error"))
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Current schema/layout version stamped into SQLite `user_version`.
    /// Bump it and add a `if from < N` step below for every upgrade that
    /// must run exactly once against existing data.
    const SCHEMA_VERSION: i64 = 2;

    fn migrate(conn: &Connection, data_dir: &Path) -> Result<()> {
        let from: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        Self::create_baseline(conn)?;
        Self::repair_missing_columns(conn)?;
        // v2: human-readable disk layout (meeting folders + note mirrors
        // named by date + title; transcripts inside their meeting folder).
        if from < 2 {
            migrations::to_v2(conn, data_dir)?;
        }
        conn.pragma_update(None, "user_version", Self::SCHEMA_VERSION)?;
        Ok(())
    }

    fn create_baseline(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS folders (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                parent_id  TEXT REFERENCES folders(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS notes (
                id               TEXT PRIMARY KEY,
                title            TEXT NOT NULL,
                folder_id        TEXT REFERENCES folders(id) ON DELETE SET NULL,
                meeting_id       TEXT,
                scratchpad       TEXT NOT NULL DEFAULT '',
                blocks_json      TEXT NOT NULL DEFAULT '[]',
                attachments_json TEXT NOT NULL DEFAULT '[]',
                created_at       TEXT NOT NULL,
                updated_at       TEXT NOT NULL,
                -- data-dir-relative markdown mirror ("notes/<date> <title>.md");
                -- stored because dedup suffixes make it non-derivable
                disk_path        TEXT
            );

            CREATE TABLE IF NOT EXISTS meetings (
                id             TEXT PRIMARY KEY,
                title          TEXT NOT NULL,
                note_id        TEXT NOT NULL,
                attendees_json TEXT NOT NULL DEFAULT '[]',
                -- 1 once the user confirmed the list in the attendee editor;
                -- calendar-seeded lists stay 0 (unreliable count proxies).
                attendees_confirmed INTEGER NOT NULL DEFAULT 0,
                started_at     TEXT NOT NULL,
                ended_at       TEXT,
                recording_json TEXT
            );

            CREATE TABLE IF NOT EXISTS transcripts (
                meeting_id   TEXT PRIMARY KEY,
                json         TEXT NOT NULL,
                -- LLM-polished variant, stored ALONGSIDE the raw `json` (never
                -- overwriting it). NULL until the polish pass runs; a re-run
                -- replaces it from the raw source. See transcripts.rs.
                cleaned_json TEXT,
                -- One-level undo for "Re-analyze speakers": the speaker
                -- assignment (segment id → key + label map) captured right
                -- before the last re-diarize overwrote it. See transcripts.rs.
                speaker_undo_json TEXT
            );

            CREATE TABLE IF NOT EXISTS templates (
                id             TEXT PRIMARY KEY,
                name           TEXT NOT NULL,
                system_prompt  TEXT NOT NULL,
                structure_hint TEXT NOT NULL,
                built_in       INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            -- Pending/failed transcription pipeline runs (see jobs.rs).
            -- meeting_id only: recording paths are resolved at execution time.
            CREATE TABLE IF NOT EXISTS transcription_jobs (
                meeting_id TEXT PRIMARY KEY,
                status     TEXT NOT NULL DEFAULT 'queued',
                attempts   INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Extracted meeting items (see items.rs): one row per typed fact
            -- with provenance. kind '_none' marks "extraction ran, found
            -- nothing" and is filtered from every read.
            CREATE TABLE IF NOT EXISTS meeting_items (
                id               TEXT PRIMARY KEY,
                meeting_id       TEXT NOT NULL,
                kind             TEXT NOT NULL,
                text             TEXT NOT NULL,
                owner            TEXT,
                status           TEXT,
                speaker_key      TEXT,
                segment_ids_json TEXT NOT NULL DEFAULT '[]',
                created_at       TEXT NOT NULL,
                extracted_by     TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_meeting_items_meeting
                ON meeting_items(meeting_id);
            CREATE INDEX IF NOT EXISTS idx_meeting_items_kind
                ON meeting_items(kind);

            -- Full-text search. Kept in sync by the write paths in this crate.
            CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
                note_id UNINDEXED,
                title,
                body
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS transcripts_fts USING fts5(
                meeting_id UNINDEXED,
                body
            );

            -- Semantic-search chunk store (see embeddings.rs). embedding is
            -- NULL until the embedding worker fills it; `model` records which
            -- embedding model produced the vector so a model change simply
            -- re-queues everything.
            CREATE TABLE IF NOT EXISTS embedding_chunks (
                id          INTEGER PRIMARY KEY,
                owner_kind  TEXT NOT NULL,      -- 'note' | 'transcript'
                owner_id    TEXT NOT NULL,      -- note id / meeting id
                chunk_index INTEGER NOT NULL,
                title       TEXT NOT NULL DEFAULT '',
                text        TEXT NOT NULL,
                start_ms    INTEGER,
                embedding   BLOB,
                model       TEXT,
                UNIQUE(owner_kind, owner_id, chunk_index)
            );
            CREATE INDEX IF NOT EXISTS idx_embedding_chunks_owner
                ON embedding_chunks(owner_kind, owner_id);
            "#,
        )?;
        Ok(())
    }

    /// `CREATE TABLE IF NOT EXISTS` never adds columns to tables made by an
    /// older build, so upgrades would break on the first new column (seen in
    /// the wild: a pre-`scratchpad` looma.db). Compare each table against the
    /// baseline schema and `ALTER TABLE ADD COLUMN` whatever is missing —
    /// idempotent, and safe for every DB regardless of the build that made it.
    fn repair_missing_columns(conn: &Connection) -> Result<()> {
        const EXPECTED: &[(&str, &[(&str, &str)])] = &[
            (
                "folders",
                &[
                    ("parent_id", "TEXT REFERENCES folders(id) ON DELETE CASCADE"),
                    ("created_at", "TEXT NOT NULL DEFAULT ''"),
                ],
            ),
            (
                "notes",
                &[
                    (
                        "folder_id",
                        "TEXT REFERENCES folders(id) ON DELETE SET NULL",
                    ),
                    ("meeting_id", "TEXT"),
                    ("scratchpad", "TEXT NOT NULL DEFAULT ''"),
                    ("blocks_json", "TEXT NOT NULL DEFAULT '[]'"),
                    ("attachments_json", "TEXT NOT NULL DEFAULT '[]'"),
                    ("created_at", "TEXT NOT NULL DEFAULT ''"),
                    ("updated_at", "TEXT NOT NULL DEFAULT ''"),
                    ("disk_path", "TEXT"),
                ],
            ),
            (
                "meetings",
                &[
                    ("attendees_json", "TEXT NOT NULL DEFAULT '[]'"),
                    ("attendees_confirmed", "INTEGER NOT NULL DEFAULT 0"),
                    ("started_at", "TEXT NOT NULL DEFAULT ''"),
                    ("ended_at", "TEXT"),
                    ("recording_json", "TEXT"),
                ],
            ),
            (
                "templates",
                &[
                    ("system_prompt", "TEXT NOT NULL DEFAULT ''"),
                    ("structure_hint", "TEXT NOT NULL DEFAULT ''"),
                    ("built_in", "INTEGER NOT NULL DEFAULT 0"),
                ],
            ),
            // Added with the transcript-polish feature (cleaned_json) and the
            // attendee editor's re-diarize undo (speaker_undo_json); older DBs
            // get them via ALTER so the columns exist before first write.
            (
                "transcripts",
                &[("cleaned_json", "TEXT"), ("speaker_undo_json", "TEXT")],
            ),
        ];
        for (table, cols) in EXPECTED {
            let existing: std::collections::HashSet<String> = conn
                .prepare(&format!("PRAGMA table_info({table})"))?
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<std::result::Result<_, _>>()?;
            for (name, decl) in *cols {
                if !existing.contains(*name) {
                    conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {name} {decl}"))?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn test_storage() -> (tempfile::TempDir, Storage) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    (dir, storage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_schema_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        assert!(dir.path().join("flyonthewall.db").exists());
        assert!(dir.path().join("notes").is_dir());
        assert!(dir.path().join("attachments").is_dir());
        // reopen works (migrations are idempotent)
        drop(storage);
        Storage::open(dir.path()).unwrap();
    }

    /// The transient-open bug: open fails a few times (some external process
    /// interfering, as in the db-corruption incidents) then succeeds. Retry
    /// must swallow the early failures and return the eventually-opened DB.
    #[test]
    fn open_with_retry_succeeds_after_transient_failures() {
        use std::cell::Cell;
        let dir = tempfile::tempdir().unwrap();
        let calls = Cell::new(0u32);
        let retry = OpenRetry {
            attempts: 5,
            backoff: Duration::from_millis(1),
        };
        let storage = Storage::retry_open(retry, || {
            let n = calls.get();
            calls.set(n + 1);
            if n < 3 {
                Err(StorageError::Invalid(
                    "database disk image is malformed".into(),
                ))
            } else {
                Storage::open(dir.path())
            }
        })
        .unwrap();
        assert_eq!(calls.get(), 4, "3 failures then 1 success");
        assert!(dir.path().join("flyonthewall.db").exists());
        drop(storage);
    }

    /// If every attempt fails, the final error is returned unchanged so the
    /// startup dialog (the fatal fallback) still fires.
    #[test]
    fn open_with_retry_surfaces_error_after_exhausting_attempts() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let retry = OpenRetry {
            attempts: 3,
            backoff: Duration::from_millis(1),
        };
        let result = Storage::retry_open(retry, || {
            calls.set(calls.get() + 1);
            Err::<Storage, _>(StorageError::Invalid("still malformed".into()))
        });
        assert_eq!(calls.get(), 3, "all attempts made before giving up");
        assert!(matches!(result, Err(StorageError::Invalid(m)) if m == "still malformed"));
    }

    /// A looma.db created by an older build (missing later columns) must be
    /// repaired on open, not error on first write. Regression test for the
    /// pre-`scratchpad` DB found during real-machine validation.
    #[test]
    fn open_repairs_old_schema() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("looma.db")).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE notes (
                    id         TEXT PRIMARY KEY,
                    title      TEXT NOT NULL,
                    folder_id  TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE meetings (
                    id      TEXT PRIMARY KEY,
                    title   TEXT NOT NULL,
                    note_id TEXT NOT NULL
                );
                "#,
            )
            .unwrap();
        }
        let storage = Storage::open(dir.path()).unwrap();
        let note = storage.create_note("upgraded", None).unwrap();
        assert_eq!(storage.get_note(&note.id).unwrap().title, "upgraded");
    }

    #[test]
    fn open_fresh_install_creates_new_db() {
        let dir = tempfile::tempdir().unwrap();
        Storage::open(dir.path()).unwrap();
        assert!(dir.path().join("flyonthewall.db").exists());
        assert!(!dir.path().join("looma.db").exists());
    }

    /// A pre-rebrand data dir (`looma.db`) is migrated in place on open: the
    /// file is renamed to `flyonthewall.db` and its rows survive.
    #[test]
    fn open_migrates_legacy_looma_db() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("looma.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE folders (id TEXT PRIMARY KEY, name TEXT NOT NULL, \
                 parent_id TEXT, created_at TEXT NOT NULL);\
                 INSERT INTO folders (id, name, parent_id, created_at) \
                 VALUES ('f1', 'Legacy', NULL, '2026-01-01');",
            )
            .unwrap();
        }
        let storage = Storage::open(dir.path()).unwrap();
        assert!(dir.path().join("flyonthewall.db").exists());
        assert!(!dir.path().join("looma.db").exists());
        drop(storage);
        let conn = Connection::open(dir.path().join("flyonthewall.db")).unwrap();
        let name: String = conn
            .query_row("SELECT name FROM folders WHERE id = 'f1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Legacy");
    }

    /// Guards the "bundled SQLite has FTS5" assumption the whole search
    /// feature rests on.
    #[test]
    fn fts5_is_available_and_matches() {
        let (_dir, storage) = test_storage();
        storage
            .conn
            .execute(
                "INSERT INTO notes_fts (note_id, title, body) VALUES (?1, ?2, ?3)",
                (
                    "n1",
                    "Quarterly planning",
                    "we discussed the roadmap and hiring",
                ),
            )
            .unwrap();
        let hit: String = storage
            .conn
            .query_row(
                "SELECT note_id FROM notes_fts WHERE notes_fts MATCH 'roadmap'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, "n1");
    }
}
