//! Folder CRUD. Folders nest arbitrarily deep; deleting a folder cascades to
//! child folders (SQLite FK) while notes in deleted folders become unfiled
//! (FK ON DELETE SET NULL).

use chrono::Utc;
use fly_core::Folder;
use rusqlite::OptionalExtension;

use crate::{Result, Storage, StorageError};

impl Storage {
    pub fn create_folder(&self, name: &str, parent_id: Option<&str>) -> Result<Folder> {
        let folder = Folder {
            id: fly_core::new_id(),
            name: name.trim().to_string(),
            parent_id: parent_id.map(str::to_string),
            created_at: Utc::now(),
        };
        if folder.name.is_empty() {
            return Err(StorageError::Invalid("folder name is empty".into()));
        }
        self.conn.execute(
            "INSERT INTO folders (id, name, parent_id, created_at) VALUES (?1, ?2, ?3, ?4)",
            (
                &folder.id,
                &folder.name,
                &folder.parent_id,
                folder.created_at.to_rfc3339(),
            ),
        )?;
        Ok(folder)
    }

    pub fn list_folders(&self) -> Result<Vec<Folder>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, parent_id, created_at FROM folders ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok(Folder {
                id: r.get(0)?,
                name: r.get(1)?,
                parent_id: r.get(2)?,
                created_at: parse_ts(r.get::<_, String>(3)?),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn rename_folder(&self, id: &str, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(StorageError::Invalid("folder name is empty".into()));
        }
        let n = self
            .conn
            .execute("UPDATE folders SET name = ?1 WHERE id = ?2", (name, id))?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("folder {id}")));
        }
        Ok(())
    }

    /// Re-parent a folder. Rejects moves that would create a cycle
    /// (a folder inside its own descendant).
    pub fn move_folder(&self, id: &str, new_parent: Option<&str>) -> Result<()> {
        if let Some(mut cursor) = new_parent.map(str::to_string) {
            loop {
                if cursor == id {
                    return Err(StorageError::Invalid(
                        "cannot move a folder into its own subtree".into(),
                    ));
                }
                let parent: Option<String> = self
                    .conn
                    .query_row(
                        "SELECT parent_id FROM folders WHERE id = ?1",
                        [&cursor],
                        |r| r.get(0),
                    )
                    .optional()?
                    .ok_or_else(|| StorageError::NotFound(format!("folder {cursor}")))?;
                match parent {
                    Some(p) => cursor = p,
                    None => break,
                }
            }
        }
        let n = self.conn.execute(
            "UPDATE folders SET parent_id = ?1 WHERE id = ?2",
            (new_parent, id),
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("folder {id}")));
        }
        Ok(())
    }

    /// Folder names from the root down to (and including) `folder_id` —
    /// the machine-independent identity of a folder that the portability
    /// manifest carries (ids are local to one database).
    pub(crate) fn folder_path_names(&self, folder_id: &str) -> Result<Vec<String>> {
        let mut names = Vec::new();
        let mut cursor = Some(folder_id.to_string());
        while let Some(id) = cursor {
            let (name, parent): (String, Option<String>) = self
                .conn
                .query_row(
                    "SELECT name, parent_id FROM folders WHERE id = ?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| StorageError::NotFound(format!("folder {id}")))?;
            names.push(name);
            cursor = parent;
            if names.len() > 100 {
                break; // corrupt parent cycle — stop rather than spin
            }
        }
        names.reverse();
        Ok(names)
    }

    /// Find-or-create the nested folder path `names` (root → leaf), matching
    /// by name at each level, and return the leaf's id. Empty input maps to
    /// `None` (unfiled).
    pub(crate) fn ensure_folder_path(&self, names: &[String]) -> Result<Option<String>> {
        let mut parent: Option<String> = None;
        for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
            let existing: Option<String> = self
                .conn
                .query_row(
                    "SELECT id FROM folders
                     WHERE name = ?1 AND ((?2 IS NULL AND parent_id IS NULL) OR parent_id = ?2)
                     ORDER BY created_at LIMIT 1",
                    (name, &parent),
                    |r| r.get(0),
                )
                .optional()?;
            parent = Some(match existing {
                Some(id) => id,
                None => self.create_folder(name, parent.as_deref())?.id,
            });
        }
        Ok(parent)
    }

    pub fn delete_folder(&self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM folders WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("folder {id}")));
        }
        Ok(())
    }
}

pub(crate) fn parse_ts(s: String) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

#[cfg(test)]
mod tests {
    use crate::test_storage;

    #[test]
    fn folder_crud_and_nesting() {
        let (_dir, s) = test_storage();
        let root = s.create_folder("Work", None).unwrap();
        let child = s.create_folder("1:1s", Some(&root.id)).unwrap();
        assert_eq!(s.list_folders().unwrap().len(), 2);

        s.rename_folder(&child.id, "One on ones").unwrap();
        let folders = s.list_folders().unwrap();
        assert!(folders.iter().any(|f| f.name == "One on ones"));

        // deleting the root cascades to children
        s.delete_folder(&root.id).unwrap();
        assert!(s.list_folders().unwrap().is_empty());
    }

    #[test]
    fn cycle_moves_are_rejected() {
        let (_dir, s) = test_storage();
        let a = s.create_folder("a", None).unwrap();
        let b = s.create_folder("b", Some(&a.id)).unwrap();
        let c = s.create_folder("c", Some(&b.id)).unwrap();
        // a -> inside c (its own grandchild) must fail
        assert!(s.move_folder(&a.id, Some(&c.id)).is_err());
        // moving into itself must fail
        assert!(s.move_folder(&a.id, Some(&a.id)).is_err());
        // sane move works
        s.move_folder(&c.id, None).unwrap();
    }

    #[test]
    fn deleting_folder_unfiles_notes() {
        let (_dir, s) = test_storage();
        let f = s.create_folder("Inbox", None).unwrap();
        let note = s.create_note("hello", Some(&f.id)).unwrap();
        s.delete_folder(&f.id).unwrap();
        let got = s.get_note(&note.id).unwrap();
        assert_eq!(got.folder_id, None);
    }
}
