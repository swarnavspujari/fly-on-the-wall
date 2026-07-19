//! Meetings: the bridge between a note, its recording, and (from M3) its
//! transcript. Recording paths are stored relative to the data dir; the
//! folder they share (`recordings/<date> <title>/`) is the meeting's home on
//! disk and also holds the transcript mirrors.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fly_core::{Attendee, Meeting, RecordingRef};
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
    /// Attendees at creation come from the calendar (or nowhere), so the
    /// list starts unconfirmed.
    pub fn create_meeting(
        &self,
        title: &str,
        note_id: &str,
        attendees: &[Attendee],
    ) -> Result<Meeting> {
        let meeting = Meeting {
            id: fly_core::new_id(),
            title: title.to_string(),
            note_id: note_id.to_string(),
            attendees: attendees.to_vec(),
            attendees_confirmed: false,
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

    /// Replace a meeting's attendee list post-creation (the attendee editor's
    /// Save). Saving is the user's confirmation, so the confirmed flag is set
    /// — from then on the attendee count may drive diarization.
    pub fn update_attendees(&self, id: &str, attendees: &[Attendee]) -> Result<Meeting> {
        let n = self.conn.execute(
            "UPDATE meetings SET attendees_json = ?1, attendees_confirmed = 1 WHERE id = ?2",
            (serde_json::to_string(&attendees)?, id),
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("meeting {id}")));
        }
        // the attendee list travels in the portable manifest
        self.refresh_recording_manifest(id)?;
        self.get_meeting(id)
    }

    /// Remove a meeting and everything derived from it: the transcript row
    /// (raw + cleaned, FTS, embedding chunks), extracted items, its
    /// transcription job, the meeting row, and the recordings folder on
    /// disk (which also holds the transcript mirrors). Used when the owning
    /// note is deleted — a meeting must not survive as an orphan.
    pub fn purge_meeting(&self, id: &str) -> Result<()> {
        // resolve the on-disk folder before the row disappears
        let rec_dir = self
            .get_meeting(id)
            .ok()
            .and_then(|m| m.recording)
            .and_then(|r| recording_dir_rel(&r));
        self.conn
            .execute("DELETE FROM transcripts WHERE meeting_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM transcripts_fts WHERE meeting_id = ?1", [id])?;
        self.delete_chunks("transcript", id)?;
        self.conn
            .execute("DELETE FROM meeting_items WHERE meeting_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM transcription_jobs WHERE meeting_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM meetings WHERE id = ?1", [id])?;
        if let Some(rel) = rec_dir {
            // best-effort: a locked file only strands the folder, never the delete
            if let Err(e) = std::fs::remove_dir_all(self.data_dir.join(&rel)) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(dir = %rel, error = %e, "could not remove recordings folder");
                }
            }
        }
        Ok(())
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

    /// Set a meeting's start date/time (the note header's date editor).
    /// Length is preserved: `ended_at` shifts by the same delta. The date is
    /// then re-mirrored everywhere it already lives: the date-prefixed
    /// `recordings/<date> <title>/` folder is renamed (best-effort, same
    /// policy as title renames) and the folder's `recording.manifest.json`
    /// is rewritten so self-heal on another machine resurrects the meeting
    /// under the edited date.
    pub fn set_meeting_started_at(&self, id: &str, started_at: DateTime<Utc>) -> Result<Meeting> {
        let old = self.get_meeting(id)?;
        let delta = started_at - old.started_at;
        let ended_at = old.ended_at.map(|e| e + delta);
        self.conn.execute(
            "UPDATE meetings SET started_at = ?1, ended_at = ?2 WHERE id = ?3",
            (
                started_at.to_rfc3339(),
                ended_at.map(|e| e.to_rfc3339()),
                id,
            ),
        )?;
        if old.recording.is_some() {
            // Folders carry the note's title (the title-edit policy); the
            // rename pass re-derives each folder name from the meeting's
            // (now updated) started_at, so it moves date prefixes too.
            let title = self
                .get_note(&old.note_id)
                .map(|n| n.title)
                .unwrap_or(old.title.clone());
            self.rename_meeting_dirs_for_note(&old.note_id, &title)?;
            self.refresh_recording_manifest(id)?;
        }
        self.get_meeting(id)
    }

    /// Rewrite a meeting's `recording.manifest.json` from its current rows
    /// (full v2 payload: exact dates, attendees, folder path), so edits made
    /// after the recording stopped port to another machine.
    pub(crate) fn refresh_recording_manifest(&self, id: &str) -> Result<()> {
        let meeting = self.get_meeting(id)?;
        let Some(manifest) = self.build_portable_manifest(&meeting) else {
            return Ok(());
        };
        let Some(rel) = recording_dir_rel(&manifest.recording) else {
            return Ok(());
        };
        let dir = self.data_dir.join(&rel);
        if !dir.is_dir() {
            return Ok(());
        }
        crate::recovery::write_recording_manifest(&dir, &manifest)?;
        Ok(())
    }

    pub fn get_meeting(&self, id: &str) -> Result<Meeting> {
        self.conn
            .query_row(
                "SELECT id, title, note_id, attendees_json, started_at, ended_at, recording_json, attendees_confirmed
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
                "SELECT id, title, note_id, attendees_json, started_at, ended_at, recording_json, attendees_confirmed
                 FROM meetings WHERE note_id = ?1 ORDER BY started_at DESC LIMIT 1",
                [note_id],
                row_to_meeting,
            )
            .optional()?)
    }

    /// All meetings attached to a note, oldest first.
    pub fn meetings_for_note(&self, note_id: &str) -> Result<Vec<Meeting>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, note_id, attendees_json, started_at, ended_at, recording_json, attendees_confirmed
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
            // A running pipeline captured absolute paths at start; renaming
            // under it invalidates hours of ASR. Keep the old name — the
            // next title/date edit renames once the job is over.
            if self
                .transcription_job(&meeting.id)?
                .is_some_and(|j| j.status == crate::jobs::JOB_RUNNING)
            {
                tracing::info!(
                    meeting_id = meeting.id,
                    "transcription running; keeping the meeting folder's old name"
                );
                continue;
            }
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
            // The manifest inside the folder still carries the old relative
            // paths — rewrite it, or a post-rename resurrection resolves to
            // the folder's old (gone) name. Best-effort like the rename.
            if let Err(e) = self.refresh_recording_manifest(&meeting.id) {
                tracing::warn!(
                    meeting_id = meeting.id,
                    error = %e,
                    "renamed meeting folder but couldn't rewrite its manifest"
                );
            }
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
        // Attendee's serde accepts both the legacy string form and the
        // struct form, so pre-feature rows parse without a data migration.
        attendees: serde_json::from_str(&attendees_json).unwrap_or_default(),
        attendees_confirmed: r.get::<_, i64>(7)? != 0,
        started_at: parse_ts(r.get::<_, String>(4)?),
        ended_at: r.get::<_, Option<String>>(5)?.map(parse_ts),
        recording: recording_json.and_then(|j| serde_json::from_str(&j).ok()),
    })
}

#[cfg(test)]
mod tests {
    use crate::test_storage;
    use fly_core::{Attendee, RecordingRef};

    /// Purging removes the meeting row, its transcript, its job, and the
    /// recordings folder on disk — nothing of the meeting survives the
    /// owning note's deletion.
    #[test]
    fn purge_meeting_removes_rows_and_the_recordings_folder() {
        let (dir, s) = test_storage();
        let note = s.create_note("Doomed", None).unwrap();
        let meeting = s.create_meeting("Doomed", &note.id, &[]).unwrap();
        let rec_dir = dir.path().join("recordings/doomed");
        std::fs::create_dir_all(&rec_dir).unwrap();
        std::fs::write(rec_dir.join("recording.mixed.wav"), b"RIFF").unwrap();
        s.end_meeting(
            &meeting.id,
            &RecordingRef {
                mic_path: None,
                system_path: None,
                mixed_path: Some("recordings/doomed/recording.mixed.wav".into()),
                playback_path: None,
                duration_ms: 1000,
            },
        )
        .unwrap();
        s.save_transcript(&fly_core::Transcript {
            meeting_id: meeting.id.clone(),
            language: Some("en".into()),
            engine: "whisper.cpp".into(),
            segments: vec![],
            speakers: vec![],
        })
        .unwrap();
        s.enqueue_transcription(&meeting.id).unwrap();

        s.purge_meeting(&meeting.id).unwrap();

        assert!(s.get_meeting(&meeting.id).is_err(), "meeting row must go");
        assert!(s.get_transcript(&meeting.id).unwrap().is_none());
        assert!(s.transcription_job(&meeting.id).unwrap().is_none());
        assert!(!rec_dir.exists(), "recordings folder must go");
        // purging a meeting that never recorded (no folder) is fine too
        let note2 = s.create_note("Doomed 2", None).unwrap();
        let m2 = s.create_meeting("Doomed 2", &note2.id, &[]).unwrap();
        s.purge_meeting(&m2.id).unwrap();
        assert!(s.get_meeting(&m2.id).is_err());
    }

    #[test]
    fn meeting_lifecycle() {
        let (_dir, s) = test_storage();
        let note = s.create_note("Weekly sync", None).unwrap();
        let meeting = s
            .create_meeting(
                "Weekly sync",
                &note.id,
                &[Attendee::from_legacy("dana@example.com")],
            )
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

    /// Attendees are mutable post-creation: the editor's Save replaces the
    /// list and marks it user-confirmed. Legacy rows (plain email strings in
    /// attendees_json) keep parsing.
    #[test]
    fn attendees_update_and_legacy_rows() {
        let (_dir, s) = test_storage();
        let note = s.create_note("m", None).unwrap();
        let meeting = s.create_meeting("m", &note.id, &[]).unwrap();
        assert!(!meeting.attendees_confirmed);

        // simulate a pre-feature row: raw email strings
        s.conn
            .execute(
                "UPDATE meetings SET attendees_json = ?1 WHERE id = ?2",
                (r#"["priya@acme.com","jordan@acme.com"]"#, &meeting.id),
            )
            .unwrap();
        let legacy = s.get_meeting(&meeting.id).unwrap();
        assert_eq!(legacy.attendees.len(), 2);
        assert_eq!(legacy.attendees[0].email.as_deref(), Some("priya@acme.com"));
        assert!(!legacy.attendees_confirmed);

        // the editor renames + confirms
        let updated = s
            .update_attendees(
                &meeting.id,
                &[Attendee {
                    name: "Priya Kapoor".into(),
                    email: Some("priya@acme.com".into()),
                }],
            )
            .unwrap();
        assert!(updated.attendees_confirmed);
        assert_eq!(updated.attendees[0].display_name(), "Priya Kapoor");
        // round-trips through the row parser
        let again = s.get_meeting(&meeting.id).unwrap();
        assert_eq!(again.attendees, updated.attendees);
        assert!(again.attendees_confirmed);

        // unknown meeting errors
        assert!(s.update_attendees("nope", &[]).is_err());
    }

    /// Editing the meeting date shifts ended_at by the same delta (length
    /// preserved), renames the date-prefixed folder, rebases recording_json,
    /// and rewrites the manifest so the edited date ports to other machines
    /// (the v2 manifest carries started_at explicitly).
    #[test]
    fn set_started_at_shifts_ended_and_remirrors_disk_names() {
        let (dir, s) = test_storage();
        let note = s.create_note("Board sync", None).unwrap();
        let meeting = s.create_meeting("Board sync", &note.id, &[]).unwrap();
        let rec_dir = s
            .allocate_meeting_dir("Board sync", meeting.started_at)
            .unwrap();
        std::fs::write(rec_dir.join("recording.mixed.wav"), b"RIFF").unwrap();
        let rel = format!(
            "recordings/{}/recording.mixed.wav",
            rec_dir.file_name().unwrap().to_string_lossy()
        );
        let rec = RecordingRef {
            mic_path: None,
            system_path: None,
            mixed_path: Some(rel),
            playback_path: None,
            duration_ms: 60_000,
        };
        let ended = s.end_meeting(&meeting.id, &rec).unwrap();
        s.stash_recording_manifest(&meeting.id, &note.id, &rec)
            .unwrap();
        let old_span = ended.ended_at.unwrap() - ended.started_at;

        let new_start = "2026-06-01T15:30:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let updated = s.set_meeting_started_at(&meeting.id, new_start).unwrap();
        assert_eq!(updated.started_at, new_start);
        assert_eq!(updated.ended_at.unwrap() - updated.started_at, old_span);

        // folder carries the new date; recording_json follows it
        let label = crate::naming::disk_label(new_start, "Board sync");
        let new_dir = dir.path().join("recordings").join(&label);
        assert!(new_dir.is_dir(), "folder should carry the new date");
        assert!(!rec_dir.exists(), "old folder should be gone");
        let mixed = updated.recording.unwrap().mixed_path.unwrap();
        assert_eq!(mixed, format!("recordings/{label}/recording.mixed.wav"));
        assert!(dir.path().join(&mixed).exists());

        // manifest travelled + rewritten: rebased paths, portable exact dates
        let manifest: crate::RecordingManifest = serde_json::from_str(
            &std::fs::read_to_string(new_dir.join(crate::RECORDING_MANIFEST)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest.recording.mixed_path.as_deref(),
            Some(mixed.as_str())
        );
        assert_eq!(manifest.started_at, Some(new_start));
        assert_eq!(manifest.ended_at, updated.ended_at.unwrap());
    }

    /// A meeting without a recording (no folder, no manifest) still gets its
    /// dates updated.
    #[test]
    fn set_started_at_without_recording_updates_row_only() {
        let (_dir, s) = test_storage();
        let note = s.create_note("m", None).unwrap();
        let meeting = s.create_meeting("m", &note.id, &[]).unwrap();
        let new_start = "2026-05-05T09:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let updated = s.set_meeting_started_at(&meeting.id, new_start).unwrap();
        assert_eq!(updated.started_at, new_start);
        assert_eq!(updated.ended_at, None);
        assert!(s.set_meeting_started_at("nope", new_start).is_err());
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

    /// A meeting whose transcription job is RUNNING keeps its folder name:
    /// the pipeline captured absolute paths at start, and a mid-run rename
    /// invalidates them after hours of ASR (observed: a 586-minute import
    /// lost ~9 h of decoding when diarization couldn't find its input). The
    /// rename applies on the next title/date edit once the job ends.
    #[test]
    fn rename_skips_meetings_with_a_running_transcription_job() {
        let (dir, s) = test_storage();
        let note = s.create_note("Untitled", None).unwrap();
        let meeting = s.create_meeting("Untitled", &note.id, &[]).unwrap();
        let rec_dir = s
            .allocate_meeting_dir("Untitled", meeting.started_at)
            .unwrap();
        std::fs::write(rec_dir.join("recording.mixed.wav"), b"RIFF").unwrap();
        let rel = format!(
            "recordings/{}/recording.mixed.wav",
            rec_dir.file_name().unwrap().to_string_lossy()
        );
        let rec = RecordingRef {
            mic_path: None,
            system_path: None,
            mixed_path: Some(rel.clone()),
            playback_path: None,
            duration_ms: 1_000,
        };
        s.end_meeting(&meeting.id, &rec).unwrap();

        s.enqueue_transcription(&meeting.id).unwrap();
        s.mark_transcription_running(&meeting.id).unwrap();
        s.update_note_title(&note.id, "Power Writing Course")
            .unwrap();
        assert!(rec_dir.is_dir(), "folder must keep its name mid-pipeline");
        assert_eq!(
            s.get_meeting(&meeting.id)
                .unwrap()
                .recording
                .unwrap()
                .mixed_path
                .as_deref(),
            Some(rel.as_str()),
            "stored paths must stay untouched mid-pipeline"
        );

        // job over → the next edit renames as usual
        s.mark_transcription_done(&meeting.id).unwrap();
        s.update_note_title(&note.id, "Power Writing Course v2")
            .unwrap();
        let label = crate::naming::disk_label(meeting.started_at, "Power Writing Course v2");
        assert!(dir.path().join("recordings").join(&label).is_dir());
        assert!(!rec_dir.exists());
    }

    /// A title rename must also rewrite the manifest inside the renamed
    /// folder: its relative paths otherwise keep the old folder name, and a
    /// post-rename resurrection (database lost) would restore broken paths.
    #[test]
    fn note_rename_rewrites_manifest_paths() {
        let (dir, s) = test_storage();
        let note = s.create_note("Untitled", None).unwrap();
        let meeting = s.create_meeting("Untitled", &note.id, &[]).unwrap();
        let rec_dir = s
            .allocate_meeting_dir("Untitled", meeting.started_at)
            .unwrap();
        std::fs::write(rec_dir.join("recording.mixed.wav"), b"RIFF").unwrap();
        let rel = format!(
            "recordings/{}/recording.mixed.wav",
            rec_dir.file_name().unwrap().to_string_lossy()
        );
        let rec = RecordingRef {
            mic_path: None,
            system_path: None,
            mixed_path: Some(rel),
            playback_path: None,
            duration_ms: 1000,
        };
        s.end_meeting(&meeting.id, &rec).unwrap();
        s.stash_recording_manifest(&meeting.id, &note.id, &rec)
            .unwrap();

        s.update_note_title(&note.id, "Vendor call").unwrap();

        let renamed = crate::naming::disk_label(meeting.started_at, "Vendor call");
        let manifest: crate::RecordingManifest = serde_json::from_str(
            &std::fs::read_to_string(
                dir.path()
                    .join("recordings")
                    .join(&renamed)
                    .join(crate::RECORDING_MANIFEST),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest.recording.mixed_path.as_deref(),
            Some(format!("recordings/{renamed}/recording.mixed.wav").as_str())
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
