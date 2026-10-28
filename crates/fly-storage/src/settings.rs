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

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Delete every setting whose key starts with `prefix` (compared
    /// literally, so `_` and `%` are not wildcards). An empty prefix deletes
    /// nothing. Returns how many rows went.
    pub fn delete_settings_with_prefix(&self, prefix: &str) -> Result<usize> {
        if prefix.is_empty() {
            return Ok(0);
        }
        Ok(self.conn.execute(
            "DELETE FROM settings WHERE substr(key, 1, length(?1)) = ?1",
            [prefix],
        )?)
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
        s.delete_setting("asr.tier").unwrap();
        assert_eq!(s.get_setting("asr.tier").unwrap(), None);
        s.delete_setting("asr.tier").unwrap(); // deleting a missing key is a no-op
    }

    #[test]
    fn prefix_delete_is_literal_and_scoped() {
        let (_dir, s) = test_storage();
        for k in [
            "consent.cloud_llm.anthropic",
            "consent.cloud_llm.openai",
            "consent.cloud_llmX", // no dot: not under the prefix
            "consent.recording_notice_accepted",
            "llm.provider",
        ] {
            s.set_setting(k, "true").unwrap();
        }
        assert_eq!(
            s.delete_settings_with_prefix("consent.cloud_llm.").unwrap(),
            2
        );
        assert_eq!(s.get_setting("consent.cloud_llm.anthropic").unwrap(), None);
        assert_eq!(s.get_setting("consent.cloud_llm.openai").unwrap(), None);
        for kept in [
            "consent.cloud_llmX",
            "consent.recording_notice_accepted",
            "llm.provider",
        ] {
            assert!(s.get_setting(kept).unwrap().is_some(), "{kept}");
        }
        // `_` / `%` aren't LIKE wildcards here, and "" deletes nothing
        assert_eq!(s.delete_settings_with_prefix("consent.cloud%").unwrap(), 0);
        assert_eq!(s.delete_settings_with_prefix("consent_").unwrap(), 0);
        assert_eq!(s.delete_settings_with_prefix("").unwrap(), 0);
        assert!(s.get_setting("llm.provider").unwrap().is_some());
    }
}
