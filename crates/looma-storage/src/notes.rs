//! Note CRUD. Every write keeps three things in sync: the notes row, the
//! FTS index, and the plain-markdown mirror under `notes/<id>.md`.

use chrono::Utc;
use looma_core::{Attachment, Note, NoteBlock};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::folders::parse_ts;
use crate::{Result, Storage, StorageError};

/// Lightweight row for list views (no blocks/scratchpad payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
    pub folder_id: Option<String>,
    pub meeting_id: Option<String>,
    pub updated_at: chrono::DateTime<Utc>,
}

impl Storage {
    pub fn create_note(&self, title: &str, folder_id: Option<&str>) -> Result<Note> {
        let now = Utc::now();
        let note = Note {
            id: looma_core::new_id(),
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
        self.conn.execute(
            "INSERT INTO notes (id, title, folder_id, meeting_id, scratchpad, blocks_json, attachments_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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

    /// Notes in one folder (`None` = unfiled/root notes), newest first.
    pub fn list_notes_in_folder(&self, folder_id: Option<&str>) -> Result<Vec<NoteSummary>> {
        let sql = "SELECT id, title, folder_id, meeting_id, updated_at FROM notes
                   WHERE (?1 IS NULL AND folder_id IS NULL) OR folder_id = ?1
                   ORDER BY updated_at DESC";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([folder_id], row_to_summary)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Most recently updated notes across all folders.
    pub fn list_recent_notes(&self, limit: usize) -> Result<Vec<NoteSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, folder_id, meeting_id, updated_at FROM notes
             ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], row_to_summary)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn update_note_title(&self, id: &str, title: &str) -> Result<Note> {
        let title = title.trim();
        if title.is_empty() {
            return Err(StorageError::Invalid("note title is empty".into()));
        }
        self.touch_note(id, "title", &title.to_string())?;
        let note = self.get_note(id)?;
        self.sync_note_derived(&note)?;
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
        Ok(note)
    }

    /// Edit one block's markdown; an edited AI block is reclaimed as user
    /// text (looma-core provenance semantics).
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

    pub fn move_note(&self, id: &str, folder_id: Option<&str>) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE notes SET folder_id = ?1, updated_at = ?2 WHERE id = ?3",
            (folder_id, Utc::now().to_rfc3339(), id),
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("note {id}")));
        }
        Ok(())
    }

    pub fn delete_note(&self, id: &str) -> Result<()> {
        let n = self.conn.execute("DELETE FROM notes WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("note {id}")));
        }
        self.conn
            .execute("DELETE FROM notes_fts WHERE note_id = ?1", [id])?;
        let _ = std::fs::remove_file(self.note_md_path(id));
        let _ = std::fs::remove_dir_all(self.data_dir.join("attachments").join(id));
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

    /// Rebuild the FTS row and the on-disk markdown mirror for a note.
    fn sync_note_derived(&self, note: &Note) -> Result<()> {
        let body = note_body_text(note);
        self.conn
            .execute("DELETE FROM notes_fts WHERE note_id = ?1", [&note.id])?;
        self.conn.execute(
            "INSERT INTO notes_fts (note_id, title, body) VALUES (?1, ?2, ?3)",
            (&note.id, &note.title, &body),
        )?;
        std::fs::write(self.note_md_path(&note.id), note.to_markdown(false))?;
        Ok(())
    }

    fn note_md_path(&self, id: &str) -> std::path::PathBuf {
        self.data_dir.join("notes").join(format!("{id}.md"))
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
    })
}

#[cfg(test)]
mod tests {
    use crate::test_storage;

    #[test]
    fn note_crud_roundtrip_and_markdown_mirror() {
        let (dir, s) = test_storage();
        let note = s.create_note("Kickoff", None).unwrap();
        s.update_note_scratchpad(&note.id, "- budget approved\n- next steps w/ dana")
            .unwrap();
        let got = s.get_note(&note.id).unwrap();
        assert!(got.scratchpad.contains("budget approved"));

        // markdown mirror exists and is readable outside the app
        let md = std::fs::read_to_string(dir.path().join("notes").join(format!("{}.md", note.id)))
            .unwrap();
        assert!(md.contains("# Kickoff"));
        assert!(md.contains("budget approved"));

        s.update_note_title(&note.id, "Kickoff — Q3").unwrap();
        assert_eq!(s.get_note(&note.id).unwrap().title, "Kickoff — Q3");

        s.delete_note(&note.id).unwrap();
        assert!(s.get_note(&note.id).is_err());
        assert!(!dir
            .path()
            .join("notes")
            .join(format!("{}.md", note.id))
            .exists());
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
}
