//! Note CRUD. Every write keeps three things in sync: the notes row, the
//! FTS index, and the plain-markdown mirror under `notes/<date> <title>.md`
//! (the mirror's relative path lives in the `disk_path` column; dedup
//! suffixes make it non-derivable from title alone).

use chrono::Utc;
use fly_core::{Attachment, Note, NoteBlock};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::folders::parse_ts;
use crate::{naming, Result, Storage, StorageError};

/// Lightweight row for list views (no blocks/scratchpad payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
    pub folder_id: Option<String>,
    pub meeting_id: Option<String>,
    pub updated_at: chrono::DateTime<Utc>,
    /// When the note's meeting happened (its `started_at`), falling back to
    /// the note's `created_at` for meeting-less notes. List views sort by and
    /// display this date.
    pub happened_at: chrono::DateTime<Utc>,
    // Read-only status for the list's chips, computed by the query (nothing
    // is stored; IPC only).
    /// The note's meeting has a transcript.
    #[serde(default)]
    pub has_transcript: bool,
    /// An enhanced doc exists (`blocks_json` isn't empty). Not "has AI
    /// blocks": AI blocks the user edits become user blocks and still count.
    #[serde(default)]
    pub has_enhanced_doc: bool,
    /// The meeting's audio is on disk to transcribe (recorded, and not
    /// removed by retention).
    #[serde(default)]
    pub has_recording: bool,
    /// The meeting's transcription job: queued | running | done | failed.
    /// None when it never went through the queue.
    #[serde(default)]
    pub job_status: Option<String>,
}

/// Shared SELECT for summary rows: joins the note's meeting so `happened_at`
/// (COALESCE(meeting started_at, note created_at)) comes back as column 5,
/// then the chip status as columns 6-9 (primary-key joins, no scans).
const SUMMARY_SELECT: &str = "SELECT n.id, n.title, n.folder_id, n.meeting_id, n.updated_at,
            COALESCE(m.started_at, n.created_at) AS happened_at,
            EXISTS(SELECT 1 FROM transcripts t WHERE t.meeting_id = n.meeting_id),
            n.blocks_json <> '[]',
            m.recording_json IS NOT NULL AND m.audio_deleted_at IS NULL,
            j.status
     FROM notes n LEFT JOIN meetings m ON m.id = n.meeting_id
     LEFT JOIN transcription_jobs j ON j.meeting_id = n.meeting_id";

impl Storage {
    pub fn create_note(&self, title: &str, folder_id: Option<&str>) -> Result<Note> {
        let now = Utc::now();
        let note = Note {
            id: fly_core::new_id(),
            title: if title.trim().is_empty() {
                "Untitled".to_string()
            } else {
                title.trim().to_string()
            },
            folder_id: folder_id.map(str::to_string),
            meeting_id: None,
            scratchpad: String::new(),
            blocks: vec![],
            attachments: vec![],
            created_at: now,
            updated_at: now,
        };
        let disk_path = self.allocate_note_path(&naming::disk_label(now, &note.title));
        self.conn.execute(
            "INSERT INTO notes (id, title, folder_id, meeting_id, scratchpad, blocks_json, attachments_json, created_at, updated_at, disk_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            (
                &note.id,
                &note.title,
                &note.folder_id,
                &note.meeting_id,
                &note.scratchpad,
                serde_json::to_string(&note.blocks)?,
                serde_json::to_string(&note.attachments)?,
                now.to_rfc3339(),
                now.to_rfc3339(),
                &disk_path,
            ),
        )?;
        self.sync_note_derived(&note)?;
        Ok(note)
    }

    pub fn get_note(&self, id: &str) -> Result<Note> {
        self.conn
            .query_row(
                "SELECT id, title, folder_id, meeting_id, scratchpad, blocks_json, attachments_json, created_at, updated_at
                 FROM notes WHERE id = ?1",
                [id],
                row_to_note,
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("note {id}")))
    }

    /// Notes in one folder (`None` = unfiled/root notes), newest meeting
    /// date first.
    pub fn list_notes_in_folder(&self, folder_id: Option<&str>) -> Result<Vec<NoteSummary>> {
        let sql = format!(
            "{SUMMARY_SELECT}
             WHERE (?1 IS NULL AND n.folder_id IS NULL) OR n.folder_id = ?1
             ORDER BY happened_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([folder_id], row_to_summary)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Notes across all folders, newest meeting date first.
    pub fn list_recent_notes(&self, limit: usize) -> Result<Vec<NoteSummary>> {
        let sql = format!("{SUMMARY_SELECT} ORDER BY happened_at DESC LIMIT ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([limit as i64], row_to_summary)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Filtered, cursor-paginated listing (MCP `list_recent`): most recently
    /// updated first. `before` is the `(updated_at, id)` of the last row of
    /// the previous page — rows strictly after it in the sort order are
    /// returned. Timestamps compare lexicographically (uniform RFC 3339 UTC).
    /// `exclude_keep_local` drops keep-local notes inside the query, so they
    /// never fill a page or become the cursor (MCP).
    pub fn list_notes_filtered(
        &self,
        limit: usize,
        folder_id: Option<&str>,
        since: Option<chrono::DateTime<chrono::Utc>>,
        until: Option<chrono::DateTime<chrono::Utc>>,
        before: Option<(String, String)>,
        exclude_keep_local: bool,
    ) -> Result<Vec<NoteSummary>> {
        let mut sql = if exclude_keep_local {
            format!(
                "{}{SUMMARY_SELECT} WHERE n.id NOT IN (SELECT id FROM local_notes)",
                crate::privacy::KEEP_LOCAL_CTE
            )
        } else {
            format!("{SUMMARY_SELECT} WHERE 1=1")
        };
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(f) = folder_id {
            sql.push_str(&format!(" AND n.folder_id = ?{}", params.len() + 1));
            params.push(Box::new(f.to_string()));
        }
        if let Some(s) = since {
            sql.push_str(&format!(" AND n.updated_at >= ?{}", params.len() + 1));
            params.push(Box::new(s.to_rfc3339()));
        }
        if let Some(u) = until {
            sql.push_str(&format!(" AND n.updated_at <= ?{}", params.len() + 1));
            params.push(Box::new(u.to_rfc3339()));
        }
        if let Some((ts, id)) = before {
            sql.push_str(&format!(
                " AND (n.updated_at < ?{n} OR (n.updated_at = ?{n} AND n.id > ?{m}))",
                n = params.len() + 1,
                m = params.len() + 2
            ));
            params.push(Box::new(ts));
            params.push(Box::new(id));
        }
        sql.push_str(&format!(
            " ORDER BY n.updated_at DESC, n.id LIMIT {}",
            limit.max(1)
        ));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            row_to_summary,
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn update_note_title(&self, id: &str, title: &str) -> Result<Note> {
        let title = title.trim();
        if title.is_empty() {
            return Err(StorageError::Invalid("note title is empty".into()));
        }
        self.touch_note(id, "title", &title.to_string())?;
        let note = self.get_note(id)?;
        self.rename_note_mirror(&note)?;
        self.sync_note_derived(&note)?;
        // title-edit policy: meeting folders follow the note's title
        self.rename_meeting_dirs_for_note(id, title)?;
        // The manifests carry the title too — including folders that kept
        // their old name (busy, or a transcription running).
        self.refresh_manifests_for_notes(&[id.to_string()]);
        Ok(note)
    }

    pub fn update_note_scratchpad(&self, id: &str, scratchpad: &str) -> Result<Note> {
        self.touch_note(id, "scratchpad", &scratchpad.to_string())?;
        let note = self.get_note(id)?;
        self.sync_note_derived(&note)?;
        Ok(note)
    }

    /// Replace the enhanced document (M4's Enhance writes through this).
    pub fn update_note_blocks(&self, id: &str, blocks: &[NoteBlock]) -> Result<Note> {
        self.touch_note(id, "blocks_json", &serde_json::to_string(blocks)?)?;
        let note = self.get_note(id)?;
        self.sync_note_derived(&note)?;
        self.sync_note_citations(&note)?;
        Ok(note)
    }

    /// Edit one block's markdown; an edited AI block is reclaimed as user
    /// text (fly-core provenance semantics).
    pub fn edit_note_block(&self, id: &str, block_id: &str, markdown: &str) -> Result<Note> {
        let note = self.get_note(id)?;
        let mut blocks = note.blocks;
        let block = blocks
            .iter_mut()
            .find(|b| b.id == block_id)
            .ok_or_else(|| StorageError::NotFound(format!("block {block_id}")))?;
        block.apply_edit(markdown);
        if markdown.trim().is_empty() {
            blocks.retain(|b| b.id != block_id);
        }
        self.update_note_blocks(id, &blocks)
    }

    /// Keep one block as the user's own text, unchanged ("Keep" on an AI
    /// line with no source). An edit only reclaims when the text changes, so
    /// this is the explicit form of the same rule.
    pub fn keep_note_block(&self, id: &str, block_id: &str) -> Result<Note> {
        let note = self.get_note(id)?;
        let mut blocks = note.blocks;
        blocks
            .iter_mut()
            .find(|b| b.id == block_id)
            .ok_or_else(|| StorageError::NotFound(format!("block {block_id}")))?
            .reclaim();
        self.update_note_blocks(id, &blocks)
    }

    pub fn move_note(&self, id: &str, folder_id: Option<&str>) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE notes SET folder_id = ?1, updated_at = ?2 WHERE id = ?3",
            (folder_id, Utc::now().to_rfc3339(), id),
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("note {id}")));
        }
        // the filed-under path lives in each meeting's portable manifest
        for meeting in self.meetings_for_note(id)? {
            self.refresh_recording_manifest(&meeting.id)?;
        }
        Ok(())
    }

    pub fn delete_note(&self, id: &str) -> Result<()> {
        let mirror = self.note_mirror_path(id); // before the row disappears
                                                // All-or-nothing: a crash mid-delete must not leave searchable text
                                                // (FTS / embedding chunks) behind for a note that no longer exists.
        let tx = self.conn.unchecked_transaction()?;
        let n = self.conn.execute("DELETE FROM notes WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("note {id}")));
        }
        self.conn
            .execute("DELETE FROM notes_fts WHERE note_id = ?1", [id])?;
        self.delete_chunks("note", id)?;
        self.conn
            .execute("DELETE FROM citation_locators WHERE note_id = ?1", [id])?;
        tx.commit()?;
        self.checkpoint_after_delete();
        for path in [mirror, self.data_dir.join("attachments").join(id)] {
            let res = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if let Err(e) = res {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(note = %id, error = %e, "could not remove a deleted note's files");
                }
            }
        }
        Ok(())
    }

    pub(crate) fn set_note_attachments(
        &self,
        id: &str,
        attachments: &[Attachment],
    ) -> Result<Note> {
        self.touch_note(id, "attachments_json", &serde_json::to_string(attachments)?)?;
        let note = self.get_note(id)?;
        self.sync_note_derived(&note)?;
        Ok(note)
    }

    fn touch_note(&self, id: &str, column: &str, value: &String) -> Result<()> {
        // column names come only from this module — never from user input
        let sql = format!("UPDATE notes SET {column} = ?1, updated_at = ?2 WHERE id = ?3");
        let n = self
            .conn
            .execute(&sql, (value, Utc::now().to_rfc3339(), id))?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("note {id}")));
        }
        Ok(())
    }

    /// Rebuild the FTS row and the on-disk mirrors for a note: the markdown
    /// mirror under `notes/`, plus a faithful `note.json` (scratchpad +
    /// blocks with provenance) inside each of the note's meeting folders —
    /// what makes a copied meeting folder restore the note body, not just
    /// its flattened markdown.
    pub(crate) fn sync_note_derived(&self, note: &Note) -> Result<()> {
        let body = note_body_text(note);
        self.conn
            .execute("DELETE FROM notes_fts WHERE note_id = ?1", [&note.id])?;
        self.conn.execute(
            "INSERT INTO notes_fts (note_id, title, body) VALUES (?1, ?2, ?3)",
            (&note.id, &note.title, &body),
        )?;
        self.sync_note_chunks(note)?;
        crate::atomic::write_atomic(&self.note_mirror_path(&note.id), note.to_markdown(false))?;
        let portable = crate::recovery::PortableNote::from_note(note);
        for meeting in self.meetings_for_note(&note.id)? {
            let Some(rel) = meeting
                .recording
                .as_ref()
                .and_then(crate::meetings::recording_dir_rel)
            else {
                continue;
            };
            let dir = self.data_dir.join(rel);
            if dir.is_dir() {
                crate::recovery::write_note_mirror(&dir, &portable)?;
            }
        }
        Ok(())
    }

    /// Absolute path of a note's markdown mirror, for callers that hand the
    /// file itself to the user (export). Same resolution as the writer side.
    pub fn note_mirror_abs(&self, id: &str) -> std::path::PathBuf {
        self.note_mirror_path(id)
    }

    /// Absolute path of a note's markdown mirror. Reads `disk_path`; a row
    /// without one (can't happen after the v2 migration) falls back to the
    /// legacy `notes/<id>.md` name.
    fn note_mirror_path(&self, id: &str) -> std::path::PathBuf {
        let rel = self
            .conn
            .query_row("SELECT disk_path FROM notes WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()
            .ok()
            .flatten()
            .flatten()
            .unwrap_or_else(|| format!("notes/{id}.md"));
        self.data_dir.join(rel)
    }

    /// First free `notes/<base>.md` (returned data-dir-relative), checking
    /// both the disk and paths already claimed by other rows (whose mirror
    /// file may not have been written yet).
    pub(crate) fn allocate_note_path(&self, base: &str) -> String {
        let taken = |p: &std::path::Path| {
            let rel = format!(
                "notes/{}",
                p.file_name().unwrap_or_default().to_string_lossy()
            );
            self.conn
                .query_row("SELECT 1 FROM notes WHERE disk_path = ?1", [&rel], |_| {
                    Ok(())
                })
                .optional()
                .ok()
                .flatten()
                .is_some()
        };
        let path = naming::unique_path(&self.data_dir.join("notes"), base, ".md", &taken);
        format!(
            "notes/{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    }

    /// Move the markdown mirror when a title change makes its name stale.
    fn rename_note_mirror(&self, note: &Note) -> Result<()> {
        let base = naming::disk_label(note.created_at, &note.title);
        let old_rel = self
            .conn
            .query_row(
                "SELECT disk_path FROM notes WHERE id = ?1",
                [&note.id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        if let Some(old) = &old_rel {
            let stem = std::path::Path::new(old)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if naming::already_labeled(&stem, &base) {
                return Ok(()); // name already carries this title
            }
        }
        let new_rel = self.allocate_note_path(&base);
        if let Some(old) = old_rel {
            // missing source is fine — sync_note_derived rewrites the mirror
            let _ = std::fs::rename(self.data_dir.join(&old), self.data_dir.join(&new_rel));
        }
        self.conn.execute(
            "UPDATE notes SET disk_path = ?1 WHERE id = ?2",
            (&new_rel, &note.id),
        )?;
        Ok(())
    }
}

/// Searchable text of a note: raw scratchpad plus the enhanced blocks.
fn note_body_text(note: &Note) -> String {
    let mut body = note.scratchpad.clone();
    for b in &note.blocks {
        body.push('\n');
        body.push_str(&b.markdown);
    }
    body
}

fn row_to_note(r: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    let blocks_json: String = r.get(5)?;
    let attachments_json: String = r.get(6)?;
    Ok(Note {
        id: r.get(0)?,
        title: r.get(1)?,
        folder_id: r.get(2)?,
        meeting_id: r.get(3)?,
        scratchpad: r.get(4)?,
        blocks: serde_json::from_str(&blocks_json).unwrap_or_default(),
        attachments: serde_json::from_str(&attachments_json).unwrap_or_default(),
        created_at: parse_ts(r.get::<_, String>(7)?),
        updated_at: parse_ts(r.get::<_, String>(8)?),
    })
}

fn row_to_summary(r: &rusqlite::Row<'_>) -> rusqlite::Result<NoteSummary> {
    Ok(NoteSummary {
        id: r.get(0)?,
        title: r.get(1)?,
        folder_id: r.get(2)?,
        meeting_id: r.get(3)?,
        updated_at: parse_ts(r.get::<_, String>(4)?),
        happened_at: parse_ts(r.get::<_, String>(5)?),
        has_transcript: r.get(6)?,
        has_enhanced_doc: r.get(7)?,
        has_recording: r.get(8)?,
        job_status: r.get(9)?,
    })
}

impl Storage {
    /// Fold the write-ahead log back into the database and truncate it, so
    /// text just deleted (and scrubbed by `secure_delete`) doesn't linger in
    /// `flyonthewall.db-wal`. Best-effort: a concurrent reader (e.g. the MCP
    /// server) can make it a partial checkpoint, which is fine.
    pub(crate) fn checkpoint_after_delete(&self) {
        let _ = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
}

#[cfg(test)]
mod tests {
    use crate::{naming, test_storage};

    #[test]
    fn note_crud_roundtrip_and_markdown_mirror() {
        let (dir, s) = test_storage();
        let note = s.create_note("Kickoff", None).unwrap();
        s.update_note_scratchpad(&note.id, "- budget approved\n- next steps w/ dana")
            .unwrap();
        let got = s.get_note(&note.id).unwrap();
        assert!(got.scratchpad.contains("budget approved"));

        // markdown mirror is named by date + title, readable outside the app
        let label = naming::disk_label(note.created_at, "Kickoff");
        let mirror = dir.path().join("notes").join(format!("{label}.md"));
        let md = std::fs::read_to_string(&mirror).unwrap();
        assert!(md.contains("# Kickoff"));
        assert!(md.contains("budget approved"));

        // retitling moves the mirror to the new name
        s.update_note_title(&note.id, "Kickoff — Q3").unwrap();
        assert_eq!(s.get_note(&note.id).unwrap().title, "Kickoff — Q3");
        assert!(!mirror.exists());
        let renamed = naming::disk_label(note.created_at, "Kickoff — Q3");
        let mirror = dir.path().join("notes").join(format!("{renamed}.md"));
        assert!(std::fs::read_to_string(&mirror)
            .unwrap()
            .contains("budget approved"));

        s.delete_note(&note.id).unwrap();
        assert!(s.get_note(&note.id).is_err());
        assert!(!mirror.exists());
    }

    #[test]
    fn keep_and_remove_an_unsourced_ai_block() {
        use fly_core::{BlockOrigin, NoteBlock};
        let (_dir, s) = test_storage();
        let note = s.create_note("Launch", None).unwrap();
        let kept = NoteBlock::ai("Launch likely moves to May.", vec![]);
        let dropped = NoteBlock::ai("Budget is final.", vec![]);
        s.update_note_blocks(&note.id, &[kept.clone(), dropped.clone()])
            .unwrap();

        // Keep: same text, now the user's
        let after = s.keep_note_block(&note.id, &kept.id).unwrap();
        let b = after.blocks.iter().find(|b| b.id == kept.id).unwrap();
        assert_eq!(b.origin, BlockOrigin::User);
        assert_eq!(b.markdown, "Launch likely moves to May.");

        // Remove = an edit to empty text, which drops the block
        let after = s.edit_note_block(&note.id, &dropped.id, "").unwrap();
        assert_eq!(after.blocks.len(), 1);
        assert_eq!(after.blocks[0].id, kept.id);

        assert!(s.keep_note_block(&note.id, "nope").is_err());
    }

    #[test]
    fn same_day_same_title_notes_get_deduped_mirrors() {
        let (dir, s) = test_storage();
        let a = s.create_note("Untitled", None).unwrap();
        let b = s.create_note("Untitled", None).unwrap();
        let label = naming::disk_label(a.created_at, "Untitled");
        assert!(dir
            .path()
            .join("notes")
            .join(format!("{label}.md"))
            .exists());
        assert!(dir
            .path()
            .join("notes")
            .join(format!("{label} (2).md"))
            .exists());
        // each mirror belongs to its own note
        s.update_note_scratchpad(&b.id, "second note body").unwrap();
        let second =
            std::fs::read_to_string(dir.path().join("notes").join(format!("{label} (2).md")))
                .unwrap();
        assert!(second.contains("second note body"));
    }

    #[test]
    fn list_by_folder_and_recent() {
        let (_dir, s) = test_storage();
        let f = s.create_folder("Sales", None).unwrap();
        let a = s.create_note("in folder", Some(&f.id)).unwrap();
        let _b = s.create_note("unfiled", None).unwrap();

        let in_folder = s.list_notes_in_folder(Some(&f.id)).unwrap();
        assert_eq!(in_folder.len(), 1);
        assert_eq!(in_folder[0].id, a.id);

        let unfiled = s.list_notes_in_folder(None).unwrap();
        assert_eq!(unfiled.len(), 1);
        assert_eq!(unfiled[0].title, "unfiled");

        assert_eq!(s.list_recent_notes(10).unwrap().len(), 2);
    }

    /// Lists order by when the meeting happened (started_at, else the note's
    /// created_at) — a later edit must not resurface an old meeting.
    #[test]
    fn lists_order_by_meeting_date_not_edit_time() {
        let (_dir, s) = test_storage();
        let plain = s.create_note("plain note", None).unwrap();
        let old = s.create_note("old meeting", None).unwrap();
        let m = s.create_meeting("old meeting", &old.id, &[]).unwrap();
        let past = chrono::Utc::now() - chrono::Duration::days(30);
        s.set_meeting_started_at(&m.id, past).unwrap();
        // editing the note bumps updated_at but must not change its place
        s.update_note_scratchpad(&old.id, "edited today").unwrap();

        let recent = s.list_recent_notes(10).unwrap();
        assert_eq!(
            recent.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            vec![plain.id.as_str(), old.id.as_str()]
        );
        assert_eq!(recent[0].happened_at, plain.created_at);
        assert_eq!(recent[1].happened_at.timestamp(), past.timestamp());

        let unfiled = s.list_notes_in_folder(None).unwrap();
        assert_eq!(
            unfiled.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            vec![plain.id.as_str(), old.id.as_str()]
        );
    }

    /// The list's status columns, one storage state per test.
    mod summary_status {
        use crate::{test_storage, NoteSummary, Storage};
        use fly_core::{NoteBlock, RecordingRef, Transcript};

        fn only(s: &Storage) -> NoteSummary {
            let mut rows = s.list_recent_notes(10).unwrap();
            assert_eq!(rows.len(), 1);
            let row = rows.remove(0);
            // the folder query reads the same columns
            let unfiled = s.list_notes_in_folder(None).unwrap();
            assert_eq!(unfiled[0].has_transcript, row.has_transcript);
            assert_eq!(unfiled[0].job_status, row.job_status);
            row
        }

        /// A note with a meeting that recorded into `recordings/<folder>`.
        fn recorded(s: &Storage, data_dir: &std::path::Path, folder: &str) -> String {
            let note = s.create_note(folder, None).unwrap();
            let m = s.create_meeting(folder, &note.id, &[]).unwrap();
            std::fs::create_dir_all(data_dir.join("recordings").join(folder)).unwrap();
            s.end_meeting(
                &m.id,
                &RecordingRef {
                    mic_path: Some(format!("recordings/{folder}/recording.mic.wav")),
                    system_path: None,
                    mixed_path: None,
                    playback_path: None,
                    duration_ms: 1_000,
                },
            )
            .unwrap();
            m.id
        }

        fn transcribe(s: &Storage, meeting_id: &str) {
            s.save_transcript(&Transcript {
                meeting_id: meeting_id.to_string(),
                language: None,
                engine: "whisper.cpp".into(),
                segments: vec![],
                speakers: vec![],
            })
            .unwrap();
        }

        #[test]
        fn notes_only() {
            let (_d, s) = test_storage();
            s.create_note("typed", None).unwrap();
            let r = only(&s);
            assert!(r.meeting_id.is_none());
            assert!(!r.has_transcript && !r.has_enhanced_doc && !r.has_recording);
            assert_eq!(r.job_status, None);
        }

        #[test]
        fn recorded_not_transcribed() {
            let (d, s) = test_storage();
            recorded(&s, d.path(), "rec");
            let r = only(&s);
            assert!(r.has_recording);
            assert!(!r.has_transcript);
            assert_eq!(r.job_status, None);
        }

        #[test]
        fn meeting_without_audio_has_no_recording() {
            let (_d, s) = test_storage();
            let note = s.create_note("live", None).unwrap();
            s.create_meeting("live", &note.id, &[]).unwrap();
            assert!(!only(&s).has_recording);
        }

        #[test]
        fn job_queued_running_done_failed() {
            let (d, s) = test_storage();
            let m = recorded(&s, d.path(), "job");
            s.enqueue_transcription(&m).unwrap();
            assert_eq!(only(&s).job_status.as_deref(), Some("queued"));
            s.mark_transcription_running(&m).unwrap();
            assert_eq!(only(&s).job_status.as_deref(), Some("running"));
            s.mark_transcription_failed(&m, 3, "boom").unwrap();
            assert_eq!(only(&s).job_status.as_deref(), Some("failed"));
            s.enqueue_transcription(&m).unwrap();
            s.mark_transcription_done(&m).unwrap();
            assert_eq!(only(&s).job_status.as_deref(), Some("done"));
        }

        #[test]
        fn transcribed() {
            let (d, s) = test_storage();
            let m = recorded(&s, d.path(), "tx");
            transcribe(&s, &m);
            let r = only(&s);
            assert!(r.has_transcript);
            assert!(!r.has_enhanced_doc);
        }

        #[test]
        fn enhanced_doc_counts_any_block_and_clears_when_emptied() {
            let (d, s) = test_storage();
            let m = recorded(&s, d.path(), "enh");
            transcribe(&s, &m);
            let note_id = s.list_recent_notes(1).unwrap()[0].id.clone();
            s.update_note_blocks(&note_id, &[NoteBlock::ai("Decided X", vec![])])
                .unwrap();
            assert!(only(&s).has_enhanced_doc);
            // an AI block the user kept is a user block now: still a doc
            s.update_note_blocks(&note_id, &[NoteBlock::user("Decided X")])
                .unwrap();
            assert!(only(&s).has_enhanced_doc);
            // every block deleted: back to "ready to enhance"
            s.update_note_blocks(&note_id, &[]).unwrap();
            let r = only(&s);
            assert!(!r.has_enhanced_doc);
            assert!(r.has_transcript);
        }

        #[test]
        fn audio_deleted_by_retention_is_not_a_recording() {
            let (d, s) = test_storage();
            let m = recorded(&s, d.path(), "old");
            transcribe(&s, &m);
            s.delete_meeting_audio(&m).unwrap();
            let r = only(&s);
            assert!(!r.has_recording, "nothing left to re-transcribe");
            assert!(r.has_transcript, "the transcript stays");
        }
    }
}
