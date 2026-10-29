//! Attachments: files are COPIED into `attachments/<note_id>/` inside the
//! data dir and referenced by a relative path, so notes stay portable.

use chrono::Utc;
use fly_core::{Attachment, Note};

use crate::{Result, Storage, StorageError};

impl Storage {
    /// The note's `attachments/<note_id>/` folder, or `None` when the id
    /// isn't a safe id (`safe_path::is_safe_id`). Stored ids are treated as untrusted
    /// here, so every write or delete under a note's attachments goes
    /// through this.
    pub fn note_attachments_dir(&self, note_id: &str) -> Option<std::path::PathBuf> {
        crate::safe_path::is_safe_id(note_id)
            .then(|| {
                crate::safe_path::contained(
                    &self.data_dir,
                    &format!("attachments/{note_id}"),
                    &["attachments"],
                    2,
                )
            })
            .flatten()
    }

    /// `rel_path` resolved inside the data dir when it names a file directly
    /// in this note's `attachments/<note_id>/` folder, else `None`.
    fn note_attachment_file(&self, note_id: &str, rel_path: &str) -> Option<std::path::PathBuf> {
        self.note_attachments_dir(note_id)?;
        let unified = rel_path.replace('\\', "/");
        let (dir, file) = unified.rsplit_once('/')?;
        if dir != format!("attachments/{note_id}") {
            return None;
        }
        crate::safe_path::contained(
            &self.data_dir,
            &format!("{dir}/{file}"),
            &["attachments"],
            3,
        )
    }

    /// Copy `src_path` into the note's attachment dir and record it.
    pub fn add_attachment(&self, note_id: &str, src_path: &std::path::Path) -> Result<Note> {
        let note = self.get_note(note_id)?;
        let dir = self
            .note_attachments_dir(note_id)
            .ok_or_else(|| StorageError::Invalid("this note can't hold attachments".into()))?;
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
        // Only a file inside this note's own attachments folder is deleted.
        if let Some(path) = self.note_attachment_file(note_id, &removed.rel_path) {
            let _ = std::fs::remove_file(path);
        }
        self.set_note_attachments(note_id, &attachments)
    }

    /// Absolute path of an attachment (for open/reveal in the UI). `rel_path`
    /// arrives over IPC, so it must stay inside the data dir: an absolute or
    /// UNC path, a drive prefix, or a `..` segment would otherwise replace or
    /// escape the base and let the caller open (launch) any file.
    pub fn attachment_abs_path(&self, rel_path: &str) -> Result<std::path::PathBuf> {
        // Only the app's own media folders — never the database, logs or
        // downloaded engine binaries elsewhere in the data dir.
        crate::safe_path::contained(&self.data_dir, rel_path, &["attachments", "recordings"], 3)
            .ok_or_else(|| {
                StorageError::Invalid(format!(
                    "attachment path must be inside attachments/ or recordings/: {rel_path}"
                ))
            })
    }

    /// Register a file that is ALREADY inside the data dir (e.g. a screen
    /// recording written straight to its final location) — no copy.
    pub fn add_attachment_in_place(&self, note_id: &str, rel_path: &str) -> Result<Note> {
        let note = self.get_note(note_id)?;
        let abs = self
            .note_attachment_file(note_id, rel_path)
            .ok_or_else(|| {
                StorageError::Invalid(format!("not inside this note's attachments: {rel_path}"))
            })?;
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
        let abs = s.attachment_abs_path(&n1.attachments[0].rel_path).unwrap();
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

    #[test]
    fn attachment_abs_path_rejects_escapes() {
        let (_dir, s) = test_storage();
        for bad in [
            "",
            "../outside.txt",
            "attachments/../../outside.txt",
            "/etc/passwd",
            r"C:\Windows\System32\calc.exe",
            r"\\attacker\share\x.exe",
        ] {
            assert!(
                s.attachment_abs_path(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
        assert!(s.attachment_abs_path("attachments/n1/report.txt").is_ok());
    }

    /// Removing an attachment deletes a file only inside that note's own
    /// folder, whatever path its record holds.
    #[test]
    fn remove_deletes_only_inside_the_notes_own_folder() {
        let (dir, s) = test_storage();
        let a = s.create_note("a", None).unwrap();
        let b = s.create_note("b", None).unwrap();
        let src = dir.path().join("report.txt");
        std::fs::write(&src, "hello").unwrap();
        let a_file = s.add_attachment(&a.id, &src).unwrap().attachments[0]
            .rel_path
            .clone();
        let foreign = fly_core::Attachment {
            id: "att-1".into(),
            file_name: "report.txt".into(),
            rel_path: a_file.clone(),
            mime: Some("text/plain".into()),
            added_at: chrono::Utc::now(),
        };
        s.set_note_attachments(&b.id, &[foreign]).unwrap();
        s.remove_attachment(&b.id, "att-1").unwrap();
        assert!(dir.path().join(&a_file).is_file(), "a's file stays");
        assert!(s.add_attachment_in_place(&b.id, &a_file).is_err());
    }
}
