//! Meetings: the bridge between a note, its recording, and (from M3) its
//! transcript. Recording paths are stored relative to the data dir; the
//! folder they share (`recordings/<date> <title>/`) is the meeting's home on
//! disk and also holds the transcript mirrors.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fly_core::{Meeting, RecordingRef};
use rusqlite::OptionalExtension;

use crate::folders::parse_ts;
use crate::{naming, Result, Storage, StorageError};

/// The data-dir-relative folder holding a meeting's recording files (the
/// parent shared by the paths in `recording_json`).
pub fn recording_dir_rel(rec: &RecordingRef) -> Option<String> {
    let path = rec
        .mic_path
        .as_ref()
        .or(rec.system_path.as_ref())
        .or(rec.mixed_path.as_ref())?;
    path.rsplit_once('/').map(|(dir, _)| dir.to_string())
}

impl Storage {
    /// Create a meeting attached to a note and point the note back at it.
    pub fn create_meeting(
        &self,
        title: &str,
        note_id: &str,
        attendees: &[String],
    ) -> Result<Meeting> {
        let meeting = Meeting {
            id: fly_core::new_id(),
            title: title.to_string(),
            note_id: note_id.to_string(),
            attendees: attendees.to_vec(),
            started_at: Utc::now(),
            ended_at: None,
            recording: None,
        };
        self.conn.execute(
            "INSERT INTO meetings (id, title, note_id, attendees_json, started_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                &meeting.id,
                &meeting.title,
                &meeting.note_id,
                serde_json::to_string(&meeting.attendees)?,
                meeting.started_at.to_rfc3339(),
            ),
        )?;
        self.conn.execute(
            "UPDATE notes SET meeting_id = ?1 WHERE id = ?2",
            (&meeting.id, note_id),
        )?;
        Ok(meeting)
    }

    /// Mark a meeting finished and store its recording.
    pub fn end_meeting(&self, id: &str, recording: &RecordingRef) -> Result<Meeting> {
        let n = self.conn.execute(
            "UPDATE meetings SET ended_at = ?1, recording_json = ?2 WHERE id = ?3",
            (
                Utc::now().to_rfc3339(),
                serde_json::to_string(recording)?,
                id,
            ),
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("meeting {id}")));
        }
        self.get_meeting(id)
    }

    pub fn get_meeting(&self, id: &str) -> Result<Meeting> {
        self.conn
            .query_row(
                "SELECT id, title, note_id, attendees_json, started_at, ended_at, recording_json
                 FROM meetings WHERE id = ?1",
                [id],
                row_to_meeting,
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("meeting {id}")))
    }

    pub fn get_meeting_for_note(&self, note_id: &str) -> Result<Option<Meeting>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, title, note_id, attendees_json, started_at, ended_at, recording_json
                 FROM meetings WHERE note_id = ?1 ORDER BY started_at DESC LIMIT 1",
                [note_id],
                row_to_meeting,
            )
            .optional()?)
    }

    /// All meetings attached to a note, oldest first.
    pub fn meetings_for_note(&self, note_id: &str) -> Result<Vec<Meeting>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, note_id, attendees_json, started_at, ended_at, recording_json
             FROM meetings WHERE note_id = ?1 ORDER BY started_at",
        )?;
        let rows = stmt.query_map([note_id], row_to_meeting)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Allocate (and create, reserving the name) the disk folder for a
    /// meeting's artifacts: `recordings/<date> <title>/`, deduped when two
    /// same-day meetings share a title. Callers put recordings inside it;
    /// the relative paths stored in `recording_json` are what ties the
    /// meeting to the folder afterwards.
    pub fn allocate_meeting_dir(&self, title: &str, started_at: DateTime<Utc>) -> Result<PathBuf> {
        let base = naming::disk_label(started_at, title);
        let dir = naming::unique_path(&self.data_dir.join("recordings"), &base, "", &|_| false);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Title-edit policy for meeting folders: renaming a note renames the
    /// folders of its meetings too — best-effort. A folder currently in use
    /// (active capture or a pipeline holding files open) fails to rename on
    /// Windows; we keep the old name and `recording_json` stays valid. A
    /// pipeline that raced a successful rename re-resolves paths from
    /// `recording_json` on its retry.
    pub(crate) fn rename_meeting_dirs_for_note(&self, note_id: &str, title: &str) -> Result<()> {
        for meeting in self.meetings_for_note(note_id)? {
            let Some(rec) = &meeting.recording else {
                continue;
            };
            let Some(old_rel) = recording_dir_rel(rec) else {
                continue;
            };
            let base = naming::disk_label(meeting.started_at, title);
            let old_name = Path::new(&old_rel)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if naming::already_labeled(&old_name, &base) {
                continue; // already carries this title
            }
            let old_abs = self.data_dir.join(&old_rel);
            if !old_abs.is_dir() {
                continue;
            }
            let new_abs =
                naming::unique_path(&self.data_dir.join("recordings"), &base, "", &|_| false);
            if let Err(e) = std::fs::rename(&old_abs, &new_abs) {
                tracing::warn!(
                    meeting_id = meeting.id,
                    error = %e,
                    "meeting folder busy; keeping its old name"
                );
                continue;
            }
            let new_rel = format!(
                "recordings/{}",
                new_abs.file_name().unwrap_or_default().to_string_lossy()
            );
            let rebase = |p: &Option<String>| {
                p.as_ref().map(|p| {
                    let file = p.rsplit('/').next().unwrap_or(p);
                    format!("{new_rel}/{file}")
                })
            };
            let updated = RecordingRef {
                mic_path: rebase(&rec.mic_path),
                system_path: rebase(&rec.system_path),
                mixed_path: rebase(&rec.mixed_path),
                playback_path: rebase(&rec.playback_path),
                duration_ms: rec.duration_ms,
            };
            self.conn.execute(
                "UPDATE meetings SET recording_json = ?1 WHERE id = ?2",
                (serde_json::to_string(&updated)?, &meeting.id),
            )?;
        }
        Ok(())
    }
}

pub(crate) fn row_to_meeting(r: &rusqlite::Row<'_>) -> rusqlite::Result<Meeting> {
    let attendees_json: String = r.get(3)?;
    let recording_json: Option<String> = r.get(6)?;
    Ok(Meeting {
        id: r.get(0)?,
        title: r.get(1)?,
        note_id: r.get(2)?,
        attendees: serde_json::from_str(&attendees_json).unwrap_or_default(),
        started_at: parse_ts(r.get::<_, String>(4)?),
        ended_at: r.get::<_, Option<String>>(5)?.map(parse_ts),
        recording: recording_json.and_then(|j| serde_json::from_str(&j).ok()),
    })
}

#[cfg(test)]
mod tests {
    use crate::test_storage;
    use fly_core::RecordingRef;

    #[test]
    fn meeting_lifecycle() {
        let (_dir, s) = test_storage();
        let note = s.create_note("Weekly sync", None).unwrap();
        let meeting = s
            .create_meeting("Weekly sync", &note.id, &["dana@example.com".into()])
            .unwrap();

        // note points back at the meeting
        assert_eq!(
            s.get_note(&note.id).unwrap().meeting_id,
            Some(meeting.id.clone())
        );
        assert!(s.get_meeting(&meeting.id).unwrap().ended_at.is_none());

        let rec = RecordingRef {
            mic_path: Some("recordings/x/recording.mic.wav".into()),
            system_path: Some("recordings/x/recording.system.wav".into()),
            mixed_path: Some("recordings/x/recording.mixed.wav".into()),
            playback_path: Some("recordings/x/recording.playback.wav".into()),
            duration_ms: 61_000,
        };
        let ended = s.end_meeting(&meeting.id, &rec).unwrap();
        assert!(ended.ended_at.is_some());
        let stored = ended.recording.unwrap();
        assert_eq!(stored.duration_ms, 61_000);
        assert_eq!(
            stored.playback_path.as_deref(),
            Some("recordings/x/recording.playback.wav")
        );

        let by_note = s.get_meeting_for_note(&note.id).unwrap().unwrap();
        assert_eq!(by_note.id, meeting.id);
    }

    /// recording_json rows written before playback_path existed must keep
    /// deserializing (players fall back to the mixed track).
    #[test]
    fn legacy_recording_json_without_playback_path_still_parses() {
        let legacy = r#"{
            "mic_path": "recordings/x/recording.mic.wav",
            "system_path": null,
            "mixed_path": "recordings/x/recording.mixed.wav",
            "duration_ms": 5000
        }"#;
        let rec: RecordingRef = serde_json::from_str(legacy).unwrap();
        assert_eq!(rec.playback_path, None);
        assert_eq!(
            rec.mixed_path.as_deref(),
            Some("recordings/x/recording.mixed.wav")
        );
    }

    /// Renaming a note renames its meeting folder and rewrites the
    /// recording paths (the title-edit policy).
    #[test]
    fn note_rename_cascades_to_meeting_folder() {
        let (dir, s) = test_storage();
        let note = s.create_note("Untitled", None).unwrap();
        let meeting = s.create_meeting("Untitled", &note.id, &[]).unwrap();
        let rec_dir = s
            .allocate_meeting_dir("Untitled", meeting.started_at)
            .unwrap();
        std::fs::write(rec_dir.join("recording.mixed.wav"), b"RIFF").unwrap();
        std::fs::write(rec_dir.join("transcript.md"), "# t").unwrap();
        let rel = format!(
            "recordings/{}/recording.mixed.wav",
            rec_dir.file_name().unwrap().to_string_lossy()
        );
        s.end_meeting(
            &meeting.id,
            &RecordingRef {
                mic_path: None,
                system_path: None,
                mixed_path: Some(rel),
                playback_path: None,
                duration_ms: 1000,
            },
        )
        .unwrap();

        s.update_note_title(&note.id, "Tina 1-1").unwrap();

        let renamed = crate::naming::disk_label(meeting.started_at, "Tina 1-1");
        let new_dir = dir.path().join("recordings").join(&renamed);
        assert!(new_dir.is_dir(), "folder should carry the new title");
        assert!(!rec_dir.exists(), "old folder should be gone");
        // transcript mirror travelled with the folder
        assert!(new_dir.join("transcript.md").exists());
        // recording_json rewritten and resolvable
        let rec = s.get_meeting(&meeting.id).unwrap().recording.unwrap();
        let mixed = rec.mixed_path.unwrap();
        assert_eq!(mixed, format!("recordings/{renamed}/recording.mixed.wav"));
        assert!(dir.path().join(mixed).exists());
    }
}
