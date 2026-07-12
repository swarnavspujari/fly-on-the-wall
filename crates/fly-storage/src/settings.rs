//! Simple key/value settings (non-secret — secrets live in the keychain).

use rusqlite::OptionalExtension;

use crate::{Result, Storage};

impl Storage {
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, value),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::test_storage;

    #[test]
    fn settings_roundtrip_and_overwrite() {
        let (_dir, s) = test_storage();
        assert_eq!(s.get_setting("asr.tier").unwrap(), None);
        s.set_setting("asr.tier", "balanced").unwrap();
        s.set_setting("asr.tier", "best").unwrap();
        assert_eq!(s.get_setting("asr.tier").unwrap().as_deref(), Some("best"));
    }
}
