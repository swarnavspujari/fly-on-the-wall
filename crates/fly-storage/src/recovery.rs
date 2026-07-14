//! Recording self-heal: make a finished recording survive ANY database
//! failure.
//!
//! `stop_recording` writes a `recording.manifest.json` into the meeting's
//! folder BEFORE attempting the database write — the manifest depends on
//! nothing but the filesystem, so it exists even when SQLite is corrupted,
//! locked, or replaced mid-write ("database disk image is malformed").
//! On every startup `self_heal_recordings` scans `recordings/` AND
//! `recordings/_unlinked/` (where a fresh database's v2 orphan sweep parks
//! folders it doesn't recognize) and repairs whatever the manifest proves:
//!
//! - meeting row exists but has no recording attached → attach it
//! - meeting row missing entirely (database was recreated) → move the folder
//!   back out of `_unlinked` if needed, recreate the note + meeting rows
//!   under their ORIGINAL ids, and restore the transcript row from the
//!   folder's `transcript.json` mirror (ids preserved, so provenance keeps
//!   resolving)
//! - already attached → no-op
//!
//! Real incident this encodes (2026-07-13): an externally-replaced database
//! made `end_meeting` fail after a 2.8-hour recording; the WAVs were safe on
//! disk but nothing pointed at them, and a recreated index parked every
//! existing meeting folder.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fly_core::{RecordingRef, Transcript};
use serde::{Deserialize, Serialize};

use crate::{naming, Result, Storage};

/// File name of the manifest inside a meeting's recording folder.
pub const RECORDING_MANIFEST: &str = "recording.manifest.json";

/// Everything needed to re-attach (or fully resurrect) a finished recording,
/// knowable WITHOUT a working database at stop time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordingManifest {
    pub meeting_id: String,
    pub note_id: String,
    pub ended_at: DateTime<Utc>,
    pub recording: RecordingRef,
}

impl RecordingManifest {
    /// The data-dir-relative folder the manifest's paths live in.
    fn dir_rel(&self) -> Option<String> {
        crate::meetings::recording_dir_rel(&self.recording)
    }
}

/// Write the manifest into the recording's folder (pure filesystem — safe to
/// call even when the database is unusable). `dir` is the meeting folder.
pub fn write_recording_manifest(dir: &Path, manifest: &RecordingManifest) -> std::io::Result<()> {
    std::fs::write(
        dir.join(RECORDING_MANIFEST),
        serde_json::to_string_pretty(manifest)?,
    )
}

/// What one startup sweep repaired.
#[derive(Debug, Default, PartialEq)]
pub struct HealReport {
    /// Meetings whose recording was re-attached to an existing row.
    pub attached: Vec<String>,
    /// Meetings fully resurrected (note + meeting rows recreated, folder
    /// moved back from `_unlinked` when needed).
    pub resurrected: Vec<String>,
}

impl HealReport {
    pub fn is_empty(&self) -> bool {
        self.attached.is_empty() && self.resurrected.is_empty()
    }
}

impl Storage {
    /// Scan for recording manifests and repair whatever they prove. Runs at
    /// every startup; a healthy install is a fast no-op. Errors on a single
    /// folder are logged and skipped — one bad manifest must not stop the
    /// rest from healing.
    pub fn self_heal_recordings(&self) -> Result<HealReport> {
        let mut report = HealReport::default();
        let recordings = self.data_dir.join("recordings");
        let mut dirs: Vec<(PathBuf, bool)> = Vec::new(); // (dir, parked)
        for (root, parked) in [
            (recordings.clone(), false),
            (recordings.join("_unlinked"), true),
        ] {
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() && p.join(RECORDING_MANIFEST).is_file() {
                    dirs.push((p, parked));
                }
            }
        }
        for (dir, parked) in dirs {
            match self.heal_one(&dir, parked) {
                Ok(Some(Healed::Attached(id))) => report.attached.push(id),
                Ok(Some(Healed::Resurrected(id))) => report.resurrected.push(id),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(dir = %dir.display(), error = %e, "recording self-heal skipped this folder");
                }
            }
        }
        Ok(report)
    }
}

enum Healed {
    Attached(String),
    Resurrected(String),
}

impl Storage {
    fn heal_one(&self, dir: &Path, parked: bool) -> Result<Option<Healed>> {
        let manifest: RecordingManifest =
            serde_json::from_str(&std::fs::read_to_string(dir.join(RECORDING_MANIFEST))?)?;

        // Meeting already whole → nothing to do.
        if let Ok(meeting) = self.get_meeting(&manifest.meeting_id) {
            if meeting.recording.is_some() {
                return Ok(None);
            }
            if parked {
                // A row exists but its folder was parked (shouldn't normally
                // happen together) — restore the folder first.
                let manifest = self.unpark(dir, manifest)?;
                self.attach(&manifest)?;
                return Ok(Some(Healed::Attached(manifest.meeting_id)));
            }
            self.attach(&manifest)?;
            return Ok(Some(Healed::Attached(manifest.meeting_id)));
        }

        // No meeting row: the database was lost/recreated. Restore the folder
        // to its normal home, then resurrect the rows under their ORIGINAL
        // ids so the folder's transcript.json (same meeting_id) still lines
        // up and provenance citations keep resolving.
        let manifest = if parked {
            self.unpark(dir, manifest)?
        } else {
            manifest
        };
        let folder_name = manifest
            .dir_rel()
            .and_then(|r| {
                Path::new(&r)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        let title = naming::strip_date_prefix(&folder_name);
        let started_at = manifest.ended_at
            - chrono::Duration::milliseconds(manifest.recording.duration_ms as i64);

        if self.get_note(&manifest.note_id).is_err() {
            self.insert_recovered_note(&manifest.note_id, &title, started_at)?;
        }
        self.conn.execute(
            "INSERT INTO meetings (id, title, note_id, attendees_json, started_at, ended_at, recording_json)
             VALUES (?1, ?2, ?3, '[]', ?4, ?5, ?6)",
            (
                &manifest.meeting_id,
                &title,
                &manifest.note_id,
                started_at.to_rfc3339(),
                manifest.ended_at.to_rfc3339(),
                serde_json::to_string(&manifest.recording)?,
            ),
        )?;
        self.conn.execute(
            "UPDATE notes SET meeting_id = ?1 WHERE id = ?2",
            (&manifest.meeting_id, &manifest.note_id),
        )?;

        // Restore the transcript from the folder's own mirror when present.
        let dir_abs = manifest
            .dir_rel()
            .map(|r| self.data_dir.join(r))
            .unwrap_or_else(|| dir.to_path_buf());
        let tpath = dir_abs.join("transcript.json");
        if tpath.is_file() {
            if let Ok(t) = serde_json::from_str::<Transcript>(&std::fs::read_to_string(&tpath)?) {
                if t.meeting_id == manifest.meeting_id {
                    self.save_transcript(&t)?;
                }
            }
        }
        Ok(Some(Healed::Resurrected(manifest.meeting_id)))
    }

    /// Attach the manifest's recording to its existing meeting row.
    fn attach(&self, manifest: &RecordingManifest) -> Result<()> {
        self.conn.execute(
            "UPDATE meetings SET ended_at = ?1, recording_json = ?2 WHERE id = ?3",
            (
                manifest.ended_at.to_rfc3339(),
                serde_json::to_string(&manifest.recording)?,
                &manifest.meeting_id,
            ),
        )?;
        Ok(())
    }

    /// Move a parked folder back under `recordings/` (deduping the name if
    /// taken), rewrite the manifest's relative paths for the new home, and
    /// persist the updated manifest.
    fn unpark(&self, dir: &Path, manifest: RecordingManifest) -> Result<RecordingManifest> {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let recordings = self.data_dir.join("recordings");
        let mut dest = recordings.join(&name);
        if dest.exists() {
            dest = naming::unique_path(&recordings, &name, "", &|_| false);
        }
        std::fs::rename(dir, &dest)?;
        let new_rel = format!(
            "recordings/{}",
            dest.file_name().unwrap_or_default().to_string_lossy()
        );
        let rebase = |p: &Option<String>| {
            p.as_ref().map(|p| {
                let file = p.rsplit('/').next().unwrap_or(p);
                format!("{new_rel}/{file}")
            })
        };
        let recording = RecordingRef {
            mic_path: rebase(&manifest.recording.mic_path),
            system_path: rebase(&manifest.recording.system_path),
            mixed_path: rebase(&manifest.recording.mixed_path),
            playback_path: rebase(&manifest.recording.playback_path),
            duration_ms: manifest.recording.duration_ms,
        };
        let healed = RecordingManifest {
            recording,
            ..manifest
        };
        write_recording_manifest(&dest, &healed)?;
        Ok(healed)
    }

    /// Minimal note row for a resurrected meeting (mirror written too, so
    /// the note behaves like any other).
    fn insert_recovered_note(
        &self,
        note_id: &str,
        title: &str,
        created_at: DateTime<Utc>,
    ) -> Result<()> {
        let label = naming::disk_label(created_at, title);
        let disk_path = format!("notes/{label}.md");
        let _ = std::fs::write(self.data_dir.join(&disk_path), format!("# {title}\n"));
        self.conn.execute(
            "INSERT INTO notes (id, title, folder_id, meeting_id, scratchpad, blocks_json, attachments_json, created_at, updated_at, disk_path)
             VALUES (?1, ?2, NULL, NULL, '', '[]', '[]', ?3, ?3, ?4)",
            (note_id, title, created_at.to_rfc3339(), &disk_path),
        )?;
        self.conn.execute(
            "INSERT INTO notes_fts (note_id, title, body) VALUES (?1, ?2, '')",
            (note_id, title),
        )?;
        Ok(())
    }

    /// Build (and persist) the manifest for a just-finished recording.
    /// Filesystem-only: callable even when the meetings table is broken.
    pub fn stash_recording_manifest(
        &self,
        meeting_id: &str,
        note_id: &str,
        recording: &RecordingRef,
    ) -> Result<()> {
        let Some(rel) = crate::meetings::recording_dir_rel(recording) else {
            return Ok(()); // no folder to stash into
        };
        let manifest = RecordingManifest {
            meeting_id: meeting_id.to_string(),
            note_id: note_id.to_string(),
            ended_at: Utc::now(),
            recording: recording.clone(),
        };
        write_recording_manifest(&self.data_dir.join(rel), &manifest)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_storage;
    use fly_core::{Speaker, TranscriptSegment};

    fn rec(folder: &str) -> RecordingRef {
        RecordingRef {
            mic_path: Some(format!("recordings/{folder}/recording.mic.wav")),
            system_path: Some(format!("recordings/{folder}/recording.system.wav")),
            mixed_path: Some(format!("recordings/{folder}/recording.mixed.wav")),
            playback_path: None,
            duration_ms: 61_000,
        }
    }

    fn make_folder(data_dir: &Path, under: &str, folder: &str) -> PathBuf {
        let dir = data_dir.join(under).join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "recording.mic.wav",
            "recording.system.wav",
            "recording.mixed.wav",
        ] {
            std::fs::write(dir.join(f), b"RIFF").unwrap();
        }
        dir
    }

    /// The core disaster: database recreated from scratch, folder parked in
    /// _unlinked by the orphan sweep. Self-heal moves it back, resurrects
    /// note + meeting under the ORIGINAL ids, and restores the transcript.
    #[test]
    fn parked_folder_with_manifest_is_fully_resurrected() {
        let (tmp, s) = test_storage();
        let folder = "2026-07-13 Big meeting";
        let dir = make_folder(tmp.path(), "recordings/_unlinked", folder);
        let manifest = RecordingManifest {
            meeting_id: "m-orig".into(),
            note_id: "n-orig".into(),
            ended_at: Utc::now(),
            recording: rec(folder),
        };
        write_recording_manifest(&dir, &manifest).unwrap();
        let transcript = Transcript {
            meeting_id: "m-orig".into(),
            language: Some("en".into()),
            engine: "whisper.cpp".into(),
            segments: vec![TranscriptSegment {
                id: "seg-1".into(),
                speaker_key: "mic".into(),
                start_ms: 0,
                end_ms: 900,
                text: "the roadmap is approved".into(),
                words: vec![],
            }],
            speakers: vec![Speaker {
                key: "mic".into(),
                label: "You".into(),
            }],
        };
        std::fs::write(
            dir.join("transcript.json"),
            serde_json::to_string(&transcript).unwrap(),
        )
        .unwrap();

        let report = s.self_heal_recordings().unwrap();
        assert_eq!(report.resurrected, vec!["m-orig".to_string()]);

        // folder back in its normal home
        assert!(tmp.path().join("recordings").join(folder).is_dir());
        assert!(!tmp
            .path()
            .join("recordings/_unlinked")
            .join(folder)
            .exists());
        // rows resurrected under the original ids
        let m = s.get_meeting("m-orig").unwrap();
        assert_eq!(m.title, "Big meeting");
        assert_eq!(m.note_id, "n-orig");
        let recng = m.recording.unwrap();
        assert_eq!(
            recng.mic_path.as_deref(),
            Some(format!("recordings/{folder}/recording.mic.wav").as_str())
        );
        // transcript restored and searchable
        let t = s.get_transcript("m-orig").unwrap().unwrap();
        assert_eq!(t.segments[0].text, "the roadmap is approved");
        assert!(s
            .search("roadmap", 10)
            .unwrap()
            .iter()
            .any(|h| h.note_id == "n-orig"));
        // second run is a no-op
        assert!(s.self_heal_recordings().unwrap().is_empty());
    }

    /// Today's simpler failure: the row survived but end_meeting's write was
    /// lost. Self-heal re-attaches the recording.
    #[test]
    fn unattached_meeting_gets_its_recording_back() {
        let (tmp, s) = test_storage();
        let note = s.create_note("Standup", None).unwrap();
        let meeting = s.create_meeting("Standup", &note.id, &[]).unwrap();
        let folder = "2026-07-13 Standup";
        make_folder(tmp.path(), "recordings", folder);
        s.stash_recording_manifest(&meeting.id, &note.id, &rec(folder))
            .unwrap();

        let report = s.self_heal_recordings().unwrap();
        assert_eq!(report.attached, vec![meeting.id.clone()]);
        let m = s.get_meeting(&meeting.id).unwrap();
        assert!(m.ended_at.is_some());
        assert_eq!(m.recording.unwrap().duration_ms, 61_000);
        assert!(s.self_heal_recordings().unwrap().is_empty());
    }

    /// Healthy meetings (manifest present, recording attached) are no-ops.
    #[test]
    fn attached_meeting_is_untouched() {
        let (tmp, s) = test_storage();
        let note = s.create_note("m", None).unwrap();
        let meeting = s.create_meeting("m", &note.id, &[]).unwrap();
        let folder = "2026-07-13 m";
        make_folder(tmp.path(), "recordings", folder);
        s.stash_recording_manifest(&meeting.id, &note.id, &rec(folder))
            .unwrap();
        s.end_meeting(&meeting.id, &rec(folder)).unwrap();
        assert!(s.self_heal_recordings().unwrap().is_empty());
    }

    /// A corrupt manifest is skipped without stopping the sweep.
    #[test]
    fn bad_manifest_is_skipped() {
        let (tmp, s) = test_storage();
        let dir = make_folder(tmp.path(), "recordings", "2026-07-13 bad");
        std::fs::write(dir.join(RECORDING_MANIFEST), b"{not json").unwrap();
        assert!(s.self_heal_recordings().unwrap().is_empty());
    }
}
