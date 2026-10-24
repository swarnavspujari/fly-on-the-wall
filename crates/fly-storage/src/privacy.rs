//! Per-meeting privacy controls (SOC 2 Confidentiality / Privacy, code side).
//!
//! * **Keep local** — `meetings.keep_local`, plus a folder-level default
//!   (`folders.keep_local`) that applies to every note filed under that
//!   folder or any of its sub-folders. The effective value is computed at
//!   use time ([`Storage::meeting_keep_local`] / [`Storage::note_keep_local`])
//!   so moving a note into a local-only folder takes effect immediately.
//!   The backend refuses cloud processing (LLM providers, Groq ASR) when it
//!   is on; this module only answers the question.
//! * **Participants notified** — `meetings.participants_notified`, the
//!   user's own record that they told the other attendees about recording.
//! * **Audio retention** — [`Storage::delete_meeting_audio`] removes the media
//!   files from a meeting's `recordings/<folder>/` (never transcripts, notes
//!   or the portability manifest) and stamps `meetings.audio_deleted_at`.
//!   `recording_json` is kept as-is: its paths are what tie the meeting to
//!   its folder (transcript mirrors, manifest), so the stamp — not missing
//!   paths — is what tells the UI and pipeline the audio is gone.
//!
//! These live outside `fly_core::Meeting` on purpose: that struct is built
//! by literal in several crates, and privacy state has its own read/write
//! commands anyway.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use serde::Serialize;

use crate::folders::parse_ts;
use crate::meetings::{contained_recordings_dir, recording_dir_rel};
use crate::{Result, Storage, StorageError};

/// Settings key holding the audio-retention period in days (absent = keep
/// forever). Internal: written only by the validating command, never via
/// the webview's generic settings access, because it steers file deletes.
pub const AUDIO_RETENTION_SETTING: &str = "privacy.audio_retention_days";

/// The only retention periods the app offers (days).
pub const RETENTION_CHOICES: &[u32] = &[7, 30, 90, 365];

/// File extensions treated as recording media for retention. Everything else
/// in a meeting folder (transcript.md/json, note.json, the manifest) stays.
const MEDIA_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "m4a", "aac", "flac", "ogg", "oga", "opus", "wma", "webm", "mp4", "m4v", "mkv",
    "mov", "avi", "wmv", "3gp", "aiff", "aif", "caf",
];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MeetingPrivacy {
    pub meeting_id: String,
    /// The meeting's own flag.
    pub keep_local: bool,
    /// True when a folder (the note's or an ancestor) forces keep-local; the
    /// meeting's own flag can't turn it off then.
    pub keep_local_from_folder: bool,
    pub participants_notified: bool,
    /// When retention deleted this meeting's audio, if it did.
    pub audio_deleted_at: Option<DateTime<Utc>>,
}

impl MeetingPrivacy {
    /// The value enforcement uses.
    pub fn effective_keep_local(&self) -> bool {
        self.keep_local || self.keep_local_from_folder
    }
}

/// What one meeting's audio deletion did.
#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct AudioPurge {
    pub files_removed: usize,
    pub bytes_removed: u64,
    /// Files that couldn't be removed (locked, permissions) — retried by the
    /// next sweep because the meeting is only stamped when nothing failed.
    pub failed: Vec<String>,
}

/// CTE prefix defining the effective keep-local sets in SQL, so bulk readers
/// can filter before `LIMIT` instead of checking row by row:
///
/// * `local_folders(id, depth)` — folders flagged local-only plus every
///   descendant (same depth cap as the ancestor walk in
///   `folder_chain_keep_local`, so a corrupt cycle terminates);
/// * `local_notes(id)` — notes filed in one of those folders, or with any
///   meeting flagged keep-local (exactly [`Storage::note_keep_local`]).
///
/// A meeting is hidden from AI tools when its note is in `local_notes`
/// (a superset of [`Storage::meeting_keep_local`]).
pub(crate) const KEEP_LOCAL_CTE: &str = "WITH RECURSIVE
     local_folders(id, depth) AS (
         SELECT id, 0 FROM folders WHERE keep_local = 1
         UNION
         SELECT f.id, lf.depth + 1
           FROM folders f JOIN local_folders lf ON f.parent_id = lf.id
          WHERE lf.depth < 64
     ),
     local_notes(id) AS (
         SELECT n.id FROM notes n WHERE n.folder_id IN (SELECT id FROM local_folders)
         UNION
         SELECT m.note_id FROM meetings m WHERE m.keep_local = 1
     ) ";

fn is_media_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| MEDIA_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

impl Storage {
    fn folder_chain_keep_local(&self, folder_id: Option<&str>) -> Result<bool> {
        let Some(folder_id) = folder_id else {
            return Ok(false);
        };
        // Walk the folder and its ancestors; depth-capped so a (corrupt)
        // parent cycle can't spin forever.
        let hit: Option<i64> = self
            .conn
            .query_row(
                "WITH RECURSIVE chain(id, depth) AS (
                     SELECT ?1, 0
                     UNION ALL
                     SELECT f.parent_id, chain.depth + 1
                       FROM folders f JOIN chain ON f.id = chain.id
                      WHERE f.parent_id IS NOT NULL AND chain.depth < 64
                 )
                 SELECT 1 FROM folders f JOIN chain ON f.id = chain.id
                  WHERE f.keep_local = 1 LIMIT 1",
                [folder_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    fn note_folder(&self, note_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT folder_id FROM notes WHERE id = ?1",
                [note_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn get_meeting_privacy(&self, meeting_id: &str) -> Result<MeetingPrivacy> {
        let row = self
            .conn
            .query_row(
                "SELECT note_id, keep_local, participants_notified, audio_deleted_at
                   FROM meetings WHERE id = ?1",
                [meeting_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)? != 0,
                        r.get::<_, i64>(2)? != 0,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("meeting {meeting_id}")))?;
        let (note_id, keep_local, participants_notified, deleted) = row;
        let folder = self.note_folder(&note_id)?;
        Ok(MeetingPrivacy {
            meeting_id: meeting_id.to_string(),
            keep_local,
            keep_local_from_folder: self.folder_chain_keep_local(folder.as_deref())?,
            participants_notified,
            audio_deleted_at: deleted.map(parse_ts),
        })
    }

    /// Effective keep-local for one meeting (its flag, or its note's folder).
    /// An unknown meeting answers `true`: when in doubt, stay local.
    pub fn meeting_keep_local(&self, meeting_id: &str) -> bool {
        match self.get_meeting_privacy(meeting_id) {
            Ok(p) => p.effective_keep_local(),
            Err(e) => {
                tracing::warn!(meeting_id, error = %e, "keep-local lookup failed; staying local");
                true
            }
        }
    }

    /// Effective keep-local for a note: any of its meetings flagged, or its
    /// folder (chain) set to local-only. Lookup errors answer `true`.
    pub fn note_keep_local(&self, note_id: &str) -> bool {
        let check = || -> Result<bool> {
            let any_meeting: Option<i64> = self
                .conn
                .query_row(
                    "SELECT 1 FROM meetings WHERE note_id = ?1 AND keep_local = 1 LIMIT 1",
                    [note_id],
                    |r| r.get(0),
                )
                .optional()?;
            if any_meeting.is_some() {
                return Ok(true);
            }
            let folder = self.note_folder(note_id)?;
            self.folder_chain_keep_local(folder.as_deref())
        };
        check().unwrap_or_else(|e| {
            tracing::warn!(note_id, error = %e, "keep-local lookup failed; staying local");
            true
        })
    }

    /// Set the keep-local flag on every meeting of a note (the editor toggle
    /// works per note — the note *is* the meeting in the UI). New meetings
    /// recorded into the note inherit it (see `create_meeting`).
    pub fn set_note_keep_local(&self, note_id: &str, keep_local: bool) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE meetings SET keep_local = ?1 WHERE note_id = ?2",
            (keep_local as i64, note_id),
        )?)
    }

    pub fn set_participants_notified(&self, meeting_id: &str, notified: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE meetings SET participants_notified = ?1 WHERE id = ?2",
            (notified as i64, meeting_id),
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("meeting {meeting_id}")));
        }
        Ok(())
    }

    /// Copy the keep-local choice from the note's earlier meetings onto a new
    /// one, so re-recording into a "keep local" note stays local.
    pub(crate) fn inherit_keep_local(&self, meeting_id: &str, note_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE meetings SET keep_local = 1
              WHERE id = ?1 AND EXISTS (
                  SELECT 1 FROM meetings
                   WHERE note_id = ?2 AND id <> ?1 AND keep_local = 1)",
            (meeting_id, note_id),
        )?;
        Ok(())
    }

    pub fn set_folder_keep_local(&self, folder_id: &str, keep_local: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE folders SET keep_local = ?1 WHERE id = ?2",
            (keep_local as i64, folder_id),
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("folder {folder_id}")));
        }
        Ok(())
    }

    /// Ids of folders flagged local-only themselves (not inherited).
    pub fn local_only_folders(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM folders WHERE keep_local = 1 ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn keep_local_set(&self, select: &str) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare(&format!("{KEEP_LOCAL_CTE}{select}"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Folders that are effectively local-only: flagged themselves or under
    /// a flagged ancestor.
    pub fn keep_local_folder_ids(&self) -> Result<HashSet<String>> {
        self.keep_local_set("SELECT DISTINCT id FROM local_folders")
    }

    /// Every note for which [`Storage::note_keep_local`] is true, in one
    /// query. Errors propagate — callers must treat them as "hide all".
    pub fn keep_local_note_ids(&self) -> Result<HashSet<String>> {
        self.keep_local_set("SELECT id FROM local_notes")
    }

    /// Every meeting for which [`Storage::meeting_keep_local`] is true (its
    /// own flag, or its note's folder chain), in one query.
    pub fn keep_local_meeting_ids(&self) -> Result<HashSet<String>> {
        self.keep_local_set(
            "SELECT m.id FROM meetings m
              WHERE m.keep_local = 1
                 OR m.note_id IN (SELECT n.id FROM notes n
                                   WHERE n.folder_id IN (SELECT id FROM local_folders))",
        )
    }

    // ---- delete all my data -------------------------------------------

    /// Erase every row of every table (FTS through its virtual table, never
    /// its shadow tables), then VACUUM and truncate the WAL so the deleted
    /// text doesn't linger in free pages or the log. Used by "Delete all my
    /// data" before the file itself is removed at restart — if the file
    /// stays locked (another process holding it), it's at least empty.
    pub fn erase_all_content(&self) -> Result<()> {
        let tables: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT name, COALESCE(sql, '') FROM sqlite_master
                  WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let virtuals: Vec<&str> = tables
            .iter()
            .filter(|(_, sql)| sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE"))
            .map(|(n, _)| n.as_str())
            .collect();
        let tx = self.conn.unchecked_transaction()?;
        for (name, _) in &tables {
            let shadow = virtuals
                .iter()
                .any(|v| name != v && name.starts_with(&format!("{v}_")));
            if shadow {
                continue;
            }
            // names come from sqlite_master, quoted as identifiers
            self.conn.execute(
                &format!("DELETE FROM \"{}\"", name.replace('"', "\"\"")),
                [],
            )?;
        }
        tx.commit()?;
        self.conn.execute_batch("VACUUM;")?;
        let _ = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok(())
    }

    // ---- retention ----------------------------------------------------

    /// The configured retention period in days, or `None` (keep forever).
    /// Anything that isn't one of [`RETENTION_CHOICES`] reads as off.
    pub fn audio_retention_days(&self) -> Option<u32> {
        self.get_setting(AUDIO_RETENTION_SETTING)
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|d| RETENTION_CHOICES.contains(d))
    }

    pub fn set_audio_retention_days(&self, days: Option<u32>) -> Result<()> {
        match days {
            None => self.delete_setting(AUDIO_RETENTION_SETTING),
            Some(d) if RETENTION_CHOICES.contains(&d) => {
                self.set_setting(AUDIO_RETENTION_SETTING, &d.to_string())
            }
            Some(d) => Err(StorageError::Invalid(format!(
                "retention must be one of {RETENTION_CHOICES:?} days, got {d}"
            ))),
        }
    }

    /// Finished meetings that still have audio and started before `cutoff`.
    /// Meetings with a queued or running transcription are skipped — their
    /// audio is about to be read.
    pub fn retention_candidates(&self, cutoff: DateTime<Utc>) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.started_at FROM meetings m
              WHERE m.recording_json IS NOT NULL
                AND m.ended_at IS NOT NULL
                AND m.audio_deleted_at IS NULL
                AND NOT EXISTS (
                    SELECT 1 FROM transcription_jobs j
                     WHERE j.meeting_id = m.id AND j.status IN ('queued', 'running'))",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, started) = row?;
            // Parse rather than compare strings: offsets vary between rows.
            // An unparseable date never qualifies (parse_ts falls back to now).
            if parse_ts(started) < cutoff {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Delete the media files of one meeting's recordings folder, keeping
    /// transcripts, notes and the manifest. The folder must resolve inside
    /// `recordings/` (same containment guard as `purge_meeting`); only plain
    /// files directly in it are touched. The meeting is stamped
    /// `audio_deleted_at` once no media file is left behind.
    pub fn delete_meeting_audio(&self, meeting_id: &str) -> Result<AudioPurge> {
        let meeting = self.get_meeting(meeting_id)?;
        let mut purge = AudioPurge::default();
        let rel = meeting.recording.as_ref().and_then(recording_dir_rel);
        if let Some(rel) = rel {
            let Some(dir) = contained_recordings_dir(&self.data_dir, &rel) else {
                return Err(StorageError::Invalid(format!(
                    "refusing to delete audio outside recordings/: {rel}"
                )));
            };
            if dir.is_dir() {
                for entry in std::fs::read_dir(&dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    // symlink_metadata: never follow a link out of the folder
                    let Ok(meta) = std::fs::symlink_metadata(&path) else {
                        continue;
                    };
                    if !meta.file_type().is_file() || !is_media_file(&path) {
                        continue;
                    }
                    match std::fs::remove_file(&path) {
                        Ok(()) => {
                            purge.files_removed += 1;
                            purge.bytes_removed += meta.len();
                        }
                        Err(e) => {
                            tracing::warn!(meeting_id, file = %path.file_name().unwrap_or_default().to_string_lossy(), error = %e,
                                "retention: could not delete audio file");
                            purge.failed.push(
                                path.file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                            );
                        }
                    }
                }
            }
        }
        if purge.failed.is_empty() {
            self.conn.execute(
                "UPDATE meetings SET audio_deleted_at = ?1 WHERE id = ?2",
                (Utc::now().to_rfc3339(), meeting_id),
            )?;
        }
        Ok(purge)
    }
}

#[cfg(test)]
mod tests {
    use crate::test_storage;
    use chrono::{Duration, Utc};
    use fly_core::RecordingRef;

    fn recorded_meeting(
        s: &crate::Storage,
        dir: &std::path::Path,
        folder: &str,
        days_ago: i64,
    ) -> String {
        let note = s.create_note(folder, None).unwrap();
        let m = s.create_meeting(folder, &note.id, &[]).unwrap();
        let rec_dir = dir.join("recordings").join(folder);
        std::fs::create_dir_all(&rec_dir).unwrap();
        for f in [
            "recording.mic.wav",
            "recording.playback.wav",
            "source.mp4",
            "transcript.md",
            "transcript.json",
            "note.json",
            "recording.manifest.json",
        ] {
            std::fs::write(rec_dir.join(f), b"data").unwrap();
        }
        s.end_meeting(
            &m.id,
            &RecordingRef {
                mic_path: Some(format!("recordings/{folder}/recording.mic.wav")),
                system_path: None,
                mixed_path: None,
                playback_path: Some(format!("recordings/{folder}/recording.playback.wav")),
                duration_ms: 1000,
            },
        )
        .unwrap();
        backdate(s, &m.id, days_ago);
        m.id
    }

    /// Move a meeting's start date without the folder rename that
    /// `set_meeting_started_at` performs.
    fn backdate(s: &crate::Storage, id: &str, days_ago: i64) {
        s.conn
            .execute(
                "UPDATE meetings SET started_at = ?1 WHERE id = ?2",
                ((Utc::now() - Duration::days(days_ago)).to_rfc3339(), id),
            )
            .unwrap();
    }

    #[test]
    fn keep_local_defaults_off_and_toggles_per_note() {
        let (_d, s) = test_storage();
        let note = s.create_note("Board", None).unwrap();
        let m = s.create_meeting("Board", &note.id, &[]).unwrap();
        assert!(!s.meeting_keep_local(&m.id));
        assert!(!s.note_keep_local(&note.id));
        s.set_note_keep_local(&note.id, true).unwrap();
        assert!(s.meeting_keep_local(&m.id));
        assert!(s.note_keep_local(&note.id));
        // a re-recording into the same note inherits the choice
        let m2 = s.create_meeting("Board", &note.id, &[]).unwrap();
        assert!(s.get_meeting_privacy(&m2.id).unwrap().keep_local);
        s.set_note_keep_local(&note.id, false).unwrap();
        assert!(!s.meeting_keep_local(&m2.id));
    }

    #[test]
    fn folder_default_applies_to_nested_notes() {
        let (_d, s) = test_storage();
        let parent = s.create_folder("Legal", None).unwrap();
        let child = s.create_folder("Cases", Some(&parent.id)).unwrap();
        let note = s.create_note("Deposition", Some(&child.id)).unwrap();
        let m = s.create_meeting("Deposition", &note.id, &[]).unwrap();
        assert!(!s.note_keep_local(&note.id));
        s.set_folder_keep_local(&parent.id, true).unwrap();
        let p = s.get_meeting_privacy(&m.id).unwrap();
        assert!(!p.keep_local && p.keep_local_from_folder);
        assert!(s.meeting_keep_local(&m.id));
        assert!(s.note_keep_local(&note.id), "meeting-less check via folder");
        assert_eq!(s.local_only_folders().unwrap(), vec![parent.id.clone()]);
        s.set_folder_keep_local(&parent.id, false).unwrap();
        assert!(!s.meeting_keep_local(&m.id));
    }

    #[test]
    fn keep_local_id_sets_match_per_row_checks() {
        let (_d, s) = test_storage();
        let legal = s.create_folder("Legal", None).unwrap();
        let cases = s.create_folder("Cases", Some(&legal.id)).unwrap();
        let deep = s.create_folder("Deep", Some(&cases.id)).unwrap();
        let open = s.create_folder("Open", None).unwrap();
        s.set_folder_keep_local(&legal.id, true).unwrap();

        let mut notes = Vec::new();
        let mut meetings = Vec::new();
        for (title, folder) in [
            ("in-deep", Some(deep.id.as_str())),
            ("in-open", Some(open.id.as_str())),
            ("flagged", None),
            ("plain", None),
            ("no-meeting-in-cases", Some(cases.id.as_str())),
        ] {
            let n = s.create_note(title, folder).unwrap();
            if title != "no-meeting-in-cases" {
                meetings.push(s.create_meeting(title, &n.id, &[]).unwrap().id);
            }
            notes.push(n.id);
        }
        s.set_note_keep_local(&notes[2], true).unwrap();

        let note_set = s.keep_local_note_ids().unwrap();
        for n in &notes {
            assert_eq!(note_set.contains(n), s.note_keep_local(n), "note {n}");
        }
        assert_eq!(note_set.len(), 3);
        let meeting_set = s.keep_local_meeting_ids().unwrap();
        for m in &meetings {
            assert_eq!(
                meeting_set.contains(m),
                s.meeting_keep_local(m),
                "meeting {m}"
            );
        }
        assert_eq!(meeting_set.len(), 2);
        let folders = s.keep_local_folder_ids().unwrap();
        assert!(folders.contains(&legal.id) && folders.contains(&cases.id));
        assert!(folders.contains(&deep.id) && !folders.contains(&open.id));

        // a (corrupt) parent cycle still terminates
        s.conn
            .execute(
                "UPDATE folders SET parent_id = ?1 WHERE id = ?2",
                (&deep.id, &legal.id),
            )
            .unwrap();
        assert_eq!(s.keep_local_folder_ids().unwrap().len(), 3);
    }

    #[test]
    fn unknown_meeting_stays_local() {
        let (_d, s) = test_storage();
        assert!(s.meeting_keep_local("no-such-meeting"));
    }

    #[test]
    fn participants_notified_roundtrip() {
        let (_d, s) = test_storage();
        let note = s.create_note("Sync", None).unwrap();
        let m = s.create_meeting("Sync", &note.id, &[]).unwrap();
        assert!(!s.get_meeting_privacy(&m.id).unwrap().participants_notified);
        s.set_participants_notified(&m.id, true).unwrap();
        assert!(s.get_meeting_privacy(&m.id).unwrap().participants_notified);
        assert!(s.set_participants_notified("missing", true).is_err());
    }

    #[test]
    fn retention_setting_only_accepts_offered_choices() {
        let (_d, s) = test_storage();
        assert_eq!(s.audio_retention_days(), None, "off by default");
        s.set_audio_retention_days(Some(30)).unwrap();
        assert_eq!(s.audio_retention_days(), Some(30));
        assert!(s.set_audio_retention_days(Some(1)).is_err());
        assert_eq!(s.audio_retention_days(), Some(30));
        // a hand-edited bad value reads as off, never as "delete everything"
        s.set_setting(super::AUDIO_RETENTION_SETTING, "0").unwrap();
        assert_eq!(s.audio_retention_days(), None);
        s.set_audio_retention_days(None).unwrap();
        assert_eq!(s.audio_retention_days(), None);
    }

    #[test]
    fn retention_picks_only_old_finished_idle_meetings() {
        let (dir, s) = test_storage();
        let old = recorded_meeting(&s, dir.path(), "old", 40);
        let fresh = recorded_meeting(&s, dir.path(), "fresh", 3);
        let busy = recorded_meeting(&s, dir.path(), "busy", 40);
        s.enqueue_transcription(&busy).unwrap();
        // still recording (no ended_at)
        let note = s.create_note("live", None).unwrap();
        let live = s.create_meeting("live", &note.id, &[]).unwrap();
        backdate(&s, &live.id, 90);

        let cutoff = Utc::now() - Duration::days(30);
        let picked = s.retention_candidates(cutoff).unwrap();
        assert_eq!(picked, vec![old.clone()]);
        assert!(!picked.contains(&fresh));

        // once deleted, never picked again
        s.delete_meeting_audio(&old).unwrap();
        assert!(s.retention_candidates(cutoff).unwrap().is_empty());
    }

    #[test]
    fn delete_audio_keeps_transcripts_and_notes() {
        let (dir, s) = test_storage();
        let id = recorded_meeting(&s, dir.path(), "kept", 100);
        s.save_transcript(&fly_core::Transcript {
            meeting_id: id.clone(),
            language: Some("en".into()),
            engine: "whisper.cpp".into(),
            segments: vec![],
            speakers: vec![],
        })
        .unwrap();
        let purge = s.delete_meeting_audio(&id).unwrap();
        assert_eq!(purge.files_removed, 3, "two wavs + the imported mp4");
        assert!(purge.failed.is_empty());
        let rec_dir = dir.path().join("recordings/kept");
        for gone in ["recording.mic.wav", "recording.playback.wav", "source.mp4"] {
            assert!(!rec_dir.join(gone).exists(), "{gone} must be deleted");
        }
        for kept in [
            "transcript.md",
            "transcript.json",
            "note.json",
            "recording.manifest.json",
        ] {
            assert!(rec_dir.join(kept).exists(), "{kept} must survive");
        }
        assert!(s.get_transcript(&id).unwrap().is_some());
        let p = s.get_meeting_privacy(&id).unwrap();
        assert!(p.audio_deleted_at.is_some());
        // recording refs survive so the folder linkage (transcript mirror,
        // manifest) still resolves
        assert!(s.get_meeting(&id).unwrap().recording.is_some());
    }

    #[test]
    fn erase_all_content_empties_every_table() {
        let (dir, s) = test_storage();
        let id = recorded_meeting(&s, dir.path(), "secret", 1);
        s.save_transcript(&fly_core::Transcript {
            meeting_id: id.clone(),
            language: Some("en".into()),
            engine: "whisper.cpp".into(),
            segments: vec![],
            speakers: vec![],
        })
        .unwrap();
        s.set_setting("llm.provider", "anthropic").unwrap();
        s.create_folder("Clients", None).unwrap();

        s.erase_all_content().unwrap();

        assert!(s.get_meeting(&id).is_err());
        assert!(s.list_folders().unwrap().is_empty());
        assert_eq!(s.get_setting("llm.provider").unwrap(), None);
        let notes: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(notes, 0);
        // FTS still usable afterwards (shadow tables were left intact)
        s.create_note("after", None).unwrap();
    }

    #[test]
    fn delete_audio_refuses_paths_outside_recordings() {
        let (dir, s) = test_storage();
        let note = s.create_note("evil", None).unwrap();
        let m = s.create_meeting("evil", &note.id, &[]).unwrap();
        let outside = dir.path().join("notes");
        std::fs::write(outside.join("keep.wav"), b"x").unwrap();
        s.end_meeting(
            &m.id,
            &RecordingRef {
                mic_path: Some("notes/keep.wav".into()),
                system_path: None,
                mixed_path: None,
                playback_path: None,
                duration_ms: 1,
            },
        )
        .unwrap();
        assert!(s.delete_meeting_audio(&m.id).is_err());
        assert!(outside.join("keep.wav").exists());
        assert!(s
            .get_meeting_privacy(&m.id)
            .unwrap()
            .audio_deleted_at
            .is_none());
    }
}
