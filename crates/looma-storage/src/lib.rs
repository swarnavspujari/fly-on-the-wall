//! looma-storage: SQLite (bundled, FTS5) as the search/index layer plus
//! portable markdown/JSON files on disk as the source of truth the user owns.
//!
//! Layout under the user-visible data dir:
//!   looma.db                     — index (folders, notes, meetings, FTS)
//!   notes/<note_id>.md           — plain-markdown mirror of each note
//!   transcripts/<meeting_id>.md  — human-readable transcript
//!   transcripts/<meeting_id>.json— structured transcript (words, speakers)
//!   attachments/<note_id>/…      — attached files, referenced relatively
//!   recordings/…                 — captured audio

mod attachments;
mod folders;
mod notes;
mod search;

pub use notes::NoteSummary;
pub use search::{SearchHit, SearchHitKind};

use std::path::{Path, PathBuf};

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

pub struct Storage {
    conn: Connection,
    data_dir: PathBuf,
}

impl Storage {
    /// Open (creating if needed) the Looma data dir and its SQLite index.
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        for sub in ["notes", "transcripts", "attachments", "recordings"] {
            std::fs::create_dir_all(data_dir.join(sub))?;
        }
        let conn = Connection::open(data_dir.join("looma.db"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn,
            data_dir: data_dir.to_path_buf(),
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn migrate(conn: &Connection) -> Result<()> {
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
                updated_at       TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS meetings (
                id             TEXT PRIMARY KEY,
                title          TEXT NOT NULL,
                note_id        TEXT NOT NULL,
                attendees_json TEXT NOT NULL DEFAULT '[]',
                started_at     TEXT NOT NULL,
                ended_at       TEXT,
                recording_json TEXT
            );

            CREATE TABLE IF NOT EXISTS transcripts (
                meeting_id TEXT PRIMARY KEY,
                json       TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS templates (
                id             TEXT PRIMARY KEY,
                name           TEXT NOT NULL,
                system_prompt  TEXT NOT NULL,
                structure_hint TEXT NOT NULL,
                built_in       INTEGER NOT NULL DEFAULT 0
            );

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
            "#,
        )?;
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
        assert!(dir.path().join("looma.db").exists());
        assert!(dir.path().join("notes").is_dir());
        assert!(dir.path().join("attachments").is_dir());
        // reopen works (migrations are idempotent)
        drop(storage);
        Storage::open(dir.path()).unwrap();
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
