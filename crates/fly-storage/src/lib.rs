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
//!   (pipeline scratch — 16 kHz intermediates, ASR checkpoints, live-caption
//!   chunks — lives OUTSIDE the data dir, in a machine-local cache folder
//!   keyed by meeting id; see scratch.rs)
//!   transcripts/…                     — legacy fallback for transcripts whose
//!                                       meeting folder is unresolvable
//!   attachments/<note_id>/…           — attached files, referenced relatively

mod atomic;
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
pub mod privacy;
mod recovery;
pub mod safe_path;
pub mod scratch;
mod search;
mod settings;
mod templates;
mod transcripts;

pub use atomic::rename_with_retry;
pub use embeddings::{PendingChunk, VectorHit};
pub use items::ItemFilter;
pub use jobs::{TranscriptionJob, JOB_DONE, JOB_FAILED, JOB_QUEUED, JOB_RUNNING};
pub use meetings::{contained_recordings_dir, recording_dir_rel};
pub use notes::NoteSummary;
pub use privacy::{
    AudioPurge, EraseOutcome, MeetingPrivacy, AUDIO_RETENTION_SETTING, RETENTION_CHOICES,
};
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

/// Tables every current-schema database has (checked by
/// [`Storage::open_existing`]; created by `create_baseline`).
const REQUIRED_TABLES: &[&str] = &[
    "folders",
    "notes",
    "meetings",
    "transcripts",
    "templates",
    "settings",
    "transcription_jobs",
    "meeting_items",
    "notes_fts",
    "transcripts_fts",
    "embedding_chunks",
];

/// Columns added after a table's first release, per table. Repaired by
/// `repair_missing_columns`; required by [`Storage::open_existing`].
const EXPECTED_COLUMNS: &[(&str, &[(&str, &str)])] = &[
    (
        "folders",
        &[
            ("parent_id", "TEXT REFERENCES folders(id) ON DELETE CASCADE"),
            ("created_at", "TEXT NOT NULL DEFAULT ''"),
            ("keep_local", "INTEGER NOT NULL DEFAULT 0"),
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
            // Privacy controls (keep-local, consent helper, retention).
            ("keep_local", "INTEGER NOT NULL DEFAULT 0"),
            ("participants_notified", "INTEGER NOT NULL DEFAULT 0"),
            ("audio_deleted_at", "TEXT"),
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
        Self::persist_wal(&conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Deleted meeting text is overwritten in the database file instead of
        // lingering in free pages until reused (confidential-data disposal).
        conn.pragma_update(None, "secure_delete", "ON")?;
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

    /// Open an index the app has ALREADY created, for secondary processes
    /// (the MCP server) that run alongside the app. Unlike [`open`](Self::open)
    /// this never creates directories or the database, never migrates,
    /// repairs columns or seeds templates — the app owns every schema write,
    /// and doing them concurrently from a second process is how two writers
    /// end up fighting over the same file. Waits up to
    /// [`EXISTING_BUSY_TIMEOUT`](Self::EXISTING_BUSY_TIMEOUT) on a locked
    /// database, and fails with a clear message when the schema isn't
    /// exactly the one this build expects (older: open the app to upgrade
    /// it; newer: this binary is out of date).
    pub fn open_existing(data_dir: &Path) -> Result<Self> {
        use rusqlite::OpenFlags;
        let db = data_dir.join(Self::DB_FILE);
        if !db.is_file() {
            return Err(StorageError::NotFound(
                "no Fly on the Wall database yet — open the app once to create it".into(),
            ));
        }
        let conn = Connection::open_with_flags(
            &db,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Self::EXISTING_BUSY_TIMEOUT)?;
        // Per-connection settings only. journal_mode is persistent in the
        // file (the app set WAL) and changing it needs an exclusive lock.
        // Persistent WAL matters most HERE: the MCP server is often the last
        // connection to close, and the last closer decides whether the
        // `-wal`/`-shm` files are deleted.
        Self::persist_wal(&conn)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        Self::check_schema(&conn)?;
        Ok(Self {
            conn,
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// How long [`open_existing`](Self::open_existing) connections wait for
    /// the app's write lock before reporting "database is locked".
    pub const EXISTING_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

    /// WAL size cap after a checkpoint (`journal_size_limit`). Any value
    /// `>= 0` also makes a persistent WAL truncate to zero bytes when the
    /// last connection closes, so old page images — text of deleted notes
    /// included — don't linger in the file between runs.
    const JOURNAL_SIZE_LIMIT: i64 = 16 * 1024 * 1024;

    /// Keep `-wal` and `-shm` on disk when the last connection closes
    /// (`SQLITE_FCNTL_PERSIST_WAL`) instead of deleting and recreating them
    /// every run. The app and the MCP server share this database, and the
    /// MCP server is often spawned by Claude Desktop — an MSIX app whose
    /// AppData overlay once paired a stale copy of the database with the
    /// real `-wal`/`-shm` ("database disk image is malformed"). Files that
    /// always exist are opened in place rather than created anew, which
    /// keeps every process on the same three files. Per-connection: both
    /// open paths call this.
    fn persist_wal(conn: &Connection) -> Result<()> {
        let mut on: std::os::raw::c_int = 1;
        // SAFETY: `conn.handle()` is the live sqlite3* owned by `conn`, valid
        // for this call; "main" is a NUL-terminated schema name; and
        // SQLITE_FCNTL_PERSIST_WAL takes a pointer to one int, which `on`
        // outlives.
        let rc = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                conn.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
                (&mut on as *mut std::os::raw::c_int).cast(),
            )
        };
        if rc != rusqlite::ffi::SQLITE_OK {
            // A VFS without the control (not the case for the bundled
            // win32/unix ones) just keeps SQLite's default behavior.
            tracing::warn!(rc, "SQLite VFS refused persistent WAL; using the default");
        }
        conn.pragma_update(None, "journal_size_limit", Self::JOURNAL_SIZE_LIMIT)?;
        Ok(())
    }

    /// Read-only check that the schema is exactly what this build expects.
    fn check_schema(conn: &Connection) -> Result<()> {
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < Self::SCHEMA_VERSION {
            return Err(StorageError::Invalid(format!(
                "database schema v{version} is older than v{} — open the Fly on the Wall app once to upgrade it",
                Self::SCHEMA_VERSION
            )));
        }
        if version > Self::SCHEMA_VERSION {
            return Err(StorageError::Invalid(format!(
                "database schema v{version} is newer than this build supports (v{}) — update Fly on the Wall",
                Self::SCHEMA_VERSION
            )));
        }
        // Columns can be added without a version bump (repair_missing_columns),
        // so a same-version file from an older build may still lack some.
        for table in REQUIRED_TABLES {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)",
                [table],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Self::needs_upgrade(table));
            }
        }
        for (table, cols) in EXPECTED_COLUMNS {
            let existing = Self::table_columns(conn, table)?;
            if cols.iter().any(|(name, _)| !existing.contains(*name)) {
                return Err(Self::needs_upgrade(table));
            }
        }
        Ok(())
    }

    fn needs_upgrade(table: &str) -> StorageError {
        StorageError::Invalid(format!(
            "database is missing parts of the current schema ({table}) — open the Fly on the Wall app once to upgrade it"
        ))
    }

    fn table_columns(conn: &Connection, table: &str) -> Result<std::collections::HashSet<String>> {
        Ok(conn
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<_, _>>()?)
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
        // Checked BEFORE the baseline tables are created below.
        let fresh = from == 0 && Self::has_no_tables(conn)?;
        Self::create_baseline(conn)?;
        Self::repair_missing_columns(conn)?;
        // v2: human-readable disk layout (meeting folders + note mirrors
        // named by date + title; transcripts inside their meeting folder).
        //
        // A fresh database has nothing to upgrade, but its data dir may still
        // hold every meeting folder and note mirror of a lost or replaced
        // index. Running the v2 orphan sweep there would park all of them
        // under `_unlinked/` (a mass rename — the 2026-07-13 incident, and on
        // a synced folder a mass re-upload). Stamp the version instead; the
        // startup self-heal re-attaches folders in place from their manifests.
        if from < 2 && !fresh {
            migrations::to_v2(conn, data_dir)?;
        }
        conn.pragma_update(None, "user_version", Self::SCHEMA_VERSION)?;
        Ok(())
    }

    /// True for a database file with no schema at all: one this open just
    /// created (fresh install, or a deleted/replaced index file).
    fn has_no_tables(conn: &Connection) -> Result<bool> {
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))?;
        Ok(n == 0)
    }

    fn create_baseline(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS folders (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                parent_id  TEXT REFERENCES folders(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL,
                -- 1 = notes filed here (or in a sub-folder) never go to a
                -- cloud processor (see privacy.rs)
                keep_local INTEGER NOT NULL DEFAULT 0
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
                recording_json TEXT,
                -- privacy controls (see privacy.rs): never send to a cloud
                -- processor; user recorded that participants were told;
                -- when retention deleted the audio (transcript/notes kept)
                keep_local            INTEGER NOT NULL DEFAULT 0,
                participants_notified INTEGER NOT NULL DEFAULT 0,
                audio_deleted_at      TEXT
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
        for (table, cols) in EXPECTED_COLUMNS {
            let existing = Self::table_columns(conn, table)?;
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

    fn user_version(dir: &Path) -> i64 {
        Connection::open(dir.join(Storage::DB_FILE))
            .unwrap()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn open_existing_refuses_a_missing_database_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let err = Storage::open_existing(dir.path()).err().unwrap();
        assert!(err.to_string().contains("open the app once"), "{err}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn open_existing_never_migrates_or_seeds() {
        let dir = tempfile::tempdir().unwrap();
        drop(Storage::open(dir.path()).unwrap());
        {
            let c = Connection::open(dir.path().join(Storage::DB_FILE)).unwrap();
            c.execute("DELETE FROM templates", []).unwrap();
        }
        let s = Storage::open_existing(dir.path()).unwrap();
        let templates: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM templates", [], |r| r.get(0))
            .unwrap();
        assert_eq!(templates, 0, "open_existing must not seed templates");
        let busy: i64 = s
            .conn
            .pragma_query_value(None, "busy_timeout", |r| r.get(0))
            .unwrap();
        assert_eq!(busy, 5000);
        // ordinary reads and the one MCP write work
        assert!(s
            .list_notes_filtered(10, None, None, None, None, false)
            .is_ok());
    }

    #[test]
    fn open_existing_rejects_other_schema_versions_without_touching_them() {
        let dir = tempfile::tempdir().unwrap();
        drop(Storage::open(dir.path()).unwrap());
        for (v, want) in [(1, "older"), (Storage::SCHEMA_VERSION + 1, "newer")] {
            Connection::open(dir.path().join(Storage::DB_FILE))
                .unwrap()
                .pragma_update(None, "user_version", v)
                .unwrap();
            let err = Storage::open_existing(dir.path()).err().unwrap();
            assert!(err.to_string().contains(want), "{err}");
            assert_eq!(user_version(dir.path()), v, "version must be left as is");
        }
    }

    #[test]
    fn open_existing_rejects_a_database_missing_current_columns() {
        let dir = tempfile::tempdir().unwrap();
        drop(Storage::open(dir.path()).unwrap());
        Connection::open(dir.path().join(Storage::DB_FILE))
            .unwrap()
            .execute_batch("ALTER TABLE transcripts DROP COLUMN speaker_undo_json")
            .unwrap();
        let err = Storage::open_existing(dir.path()).err().unwrap();
        assert!(err.to_string().contains("transcripts"), "{err}");
    }

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
