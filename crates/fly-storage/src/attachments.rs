//! Attachments: files are COPIED into `attachments/<note_id>/` inside the
//! data dir and referenced by a relative path, so notes stay portable.

use chrono::Utc;
use fly_core::{Attachment, Note};

use crate::{Result, Storage, StorageError};

impl Storage {
    /// Copy `src_path` into the note's attachment dir and record it.
    pub fn add_attachment(&self, note_id: &str, src_path: &std::path::Path) -> Result<Note> {
        let note = self.get_note(note_id)?;
        if !src_path.is_file() {
            return Err(StorageError::Invalid(format!(
                "not a file: {}",
                src_path.display()
            )));
        }
        let file_name = src_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| StorageError::Invalid("attachment has no file name".into()))?
            .to_string();

        let dir = self.data_dir.join("attachments").join(note_id);
        std::fs::create_dir_all(&dir)?;

        // avoid clobbering an existing attachment with the same name
        let mut target_name = file_name.clone();
        let mut counter = 1;
        while dir.join(&target_name).exists() {
            let (stem, ext) = match file_name.rsplit_once('.') {
                Some((s, e)) => (s.to_string(), format!(".{e}")),
                None => (file_name.clone(), String::new()),
            };
            target_name = format!("{stem} ({counter}){ext}");
            counter += 1;
        }
        std::fs::copy(src_path, dir.join(&target_name))?;

        let mut attachments = note.attachments;
        attachments.push(Attachment {
            id: fly_core::new_id(),
            file_name,
            rel_path: format!("attachments/{note_id}/{target_name}"),
            mime: mime_from_ext(&target_name),
            added_at: Utc::now(),
        });
        self.set_note_attachments(note_id, &attachments)
    }

    /// Remove the record and best-effort delete the copied file.
    pub fn remove_attachment(&self, note_id: &str, attachment_id: &str) -> Result<Note> {
        let note = self.get_note(note_id)?;
        let mut attachments = note.attachments;
        let Some(pos) = attachments.iter().position(|a| a.id == attachment_id) else {
            return Err(StorageError::NotFound(format!(
                "attachment {attachment_id}"
            )));
        };
        let removed = attachments.remove(pos);
        let _ = std::fs::remove_file(self.data_dir.join(&removed.rel_path));
        self.set_note_attachments(note_id, &attachments)
    }

    /// Absolute path of an attachment (for open/reveal in the UI).
    pub fn attachment_abs_path(&self, rel_path: &str) -> std::path::PathBuf {
        self.data_dir.join(rel_path)
    }

    /// Register a file that is ALREADY inside the data dir (e.g. a screen
    /// recording written straight to its final location) — no copy.
    pub fn add_attachment_in_place(&self, note_id: &str, rel_path: &str) -> Result<Note> {
        let note = self.get_note(note_id)?;
        let abs = self.data_dir.join(rel_path);
        if !abs.is_file() {
            return Err(StorageError::Invalid(format!(
                "not a file inside the data dir: {rel_path}"
            )));
        }
        let file_name = abs
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_string();
        let mut attachments = note.attachments;
        attachments.push(Attachment {
            id: fly_core::new_id(),
            mime: mime_from_ext(&file_name),
            file_name,
            rel_path: rel_path.replace('\\', "/"),
            added_at: Utc::now(),
        });
        self.set_note_attachments(note_id, &attachments)
    }
}

fn mime_from_ext(name: &str) -> Option<String> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    let mime = match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "txt" | "md" => "text/plain",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => return None,
    };
    Some(mime.to_string())
}

#[cfg(test)]
mod tests {
    use crate::test_storage;

    #[test]
    fn attach_copy_dedupe_and_remove() {
        let (dir, s) = test_storage();
        let note = s.create_note("with files", None).unwrap();

        let src = dir.path().join("report.txt");
        std::fs::write(&src, "hello").unwrap();

        let n1 = s.add_attachment(&note.id, &src).unwrap();
        assert_eq!(n1.attachments.len(), 1);
        let abs = s.attachment_abs_path(&n1.attachments[0].rel_path);
        assert!(abs.exists());
        assert_eq!(std::fs::read_to_string(&abs).unwrap(), "hello");

        // same file again → gets a deduped name, not an overwrite
        let n2 = s.add_attachment(&note.id, &src).unwrap();
        assert_eq!(n2.attachments.len(), 2);
        assert_ne!(n2.attachments[0].rel_path, n2.attachments[1].rel_path);

        let n3 = s
            .remove_attachment(&note.id, &n2.attachments[0].id)
            .unwrap();
        assert_eq!(n3.attachments.len(), 1);
        assert!(!abs.exists());
    }
}
