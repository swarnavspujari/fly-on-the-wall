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
use fly_core::{Attendee, Meeting, Note, NoteBlock, RecordingRef, Transcript};
use serde::{Deserialize, Serialize};

use crate::{naming, Result, Storage};

/// File name of the manifest inside a meeting's recording folder.
pub const RECORDING_MANIFEST: &str = "recording.manifest.json";

/// File name of the faithful note mirror inside a meeting's recording folder
/// (scratchpad + blocks with provenance — what `notes/<date> <title>.md`
/// flattens away). Written on every note write and restored by self-heal.
pub const NOTE_MIRROR: &str = "note.json";

/// Manifest schema version written by this build. v1 carried only the
/// recording re-attachment core; v2 added the portability payload (title,
/// started_at, attendees, folder path) so a copied folder restores the whole
/// meeting on another machine.
pub const MANIFEST_VERSION: u32 = 2;

fn manifest_v1() -> u32 {
    1
}

/// Everything needed to re-attach (or fully resurrect) a finished recording,
/// knowable WITHOUT a working database at stop time.
///
/// The portability fields are all optional: v1 manifests (and manifests
/// stashed while the database was unreadable) parse with them absent, and
/// self-heal falls back to deriving those values exactly as it always did.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordingManifest {
    #[serde(default = "manifest_v1")]
    pub version: u32,
    pub meeting_id: String,
    pub note_id: String,
    pub ended_at: DateTime<Utc>,
    pub recording: RecordingRef,
    /// Exact note/meeting title (folder names sanitize characters away).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Exact start time; without it heal derives `ended_at − duration_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees: Option<Vec<Attendee>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees_confirmed: Option<bool>,
    /// App folder the note is filed in, as names root → leaf (folder ids are
    /// machine-local; heal finds-or-creates the path by name). Absent =
    /// unfiled or unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_path: Option<Vec<String>>,
}

impl RecordingManifest {
    /// The data-dir-relative folder the manifest's paths live in.
    fn dir_rel(&self) -> Option<String> {
        crate::meetings::recording_dir_rel(&self.recording)
    }
}

/// The faithful on-disk copy of a note's editable content, mirrored into its
/// meeting folder as [`NOTE_MIRROR`]. Unlike the markdown mirror this keeps
/// the scratchpad and the enhanced blocks (with provenance) separately, so a
/// resurrected note is byte-identical to the original.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PortableNote {
    #[serde(default = "manifest_v1")]
    pub version: u32,
    pub note_id: String,
    pub title: String,
    pub scratchpad: String,
    pub blocks: Vec<NoteBlock>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PortableNote {
    pub fn from_note(note: &Note) -> Self {
        Self {
            version: MANIFEST_VERSION,
            note_id: note.id.clone(),
            title: note.title.clone(),
            scratchpad: note.scratchpad.clone(),
            blocks: note.blocks.clone(),
            created_at: note.created_at,
            updated_at: note.updated_at,
        }
    }
}

/// Write the note mirror into a meeting folder (pure filesystem).
pub fn write_note_mirror(dir: &Path, note: &PortableNote) -> std::io::Result<()> {
    std::fs::write(dir.join(NOTE_MIRROR), serde_json::to_string_pretty(note)?)
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
    /// Meetings whose portable artifacts (manifest / note mirror) were
    /// (re)written by the backfill pass. Not a repair — excluded from
    /// [`is_empty`](Self::is_empty).
    pub refreshed: Vec<String>,
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
        report.refreshed = self.refresh_portable_artifacts();
        Ok(report)
    }

    /// Backfill pass, run after healing: (re)write the v2 manifest and the
    /// note mirror for every meeting that has a recording folder, so folders
    /// from before the portability feature become portable without the user
    /// re-saving each note. Content is compared before writing — an
    /// up-to-date install is a read-only sweep. Per-meeting errors are
    /// logged and skipped.
    fn refresh_portable_artifacts(&self) -> Vec<String> {
        let mut refreshed = Vec::new();
        let ids: Vec<String> = match self
            .conn
            .prepare("SELECT id FROM meetings WHERE recording_json IS NOT NULL")
            .and_then(|mut stmt| {
                stmt.query_map([], |r| r.get(0))?
                    .collect::<std::result::Result<_, _>>()
            }) {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "portability backfill could not list meetings");
                return refreshed;
            }
        };
        for id in ids {
            match self.refresh_one_portable(&id) {
                Ok(true) => refreshed.push(id),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(meeting_id = id, error = %e, "portability backfill skipped a meeting");
                }
            }
        }
        refreshed
    }

    /// Refresh one meeting's portable artifacts; returns whether anything
    /// was written.
    fn refresh_one_portable(&self, meeting_id: &str) -> Result<bool> {
        let meeting = self.get_meeting(meeting_id)?;
        let Some(manifest) = self.build_portable_manifest(&meeting) else {
            return Ok(false);
        };
        let Some(rel) = manifest.dir_rel() else {
            return Ok(false);
        };
        let dir = self.data_dir.join(rel);
        if !dir.is_dir() {
            return Ok(false);
        }
        let mut wrote = write_if_changed(
            &dir.join(RECORDING_MANIFEST),
            &serde_json::to_string_pretty(&manifest)?,
        )?;
        if let Ok(note) = self.get_note(&meeting.note_id) {
            wrote |= write_if_changed(
                &dir.join(NOTE_MIRROR),
                &serde_json::to_string_pretty(&PortableNote::from_note(&note))?,
            )?;
        }
        Ok(wrote)
    }

    /// Build the full v2 manifest for a meeting from its live rows.
    /// Best-effort on the enrichment: a missing note degrades fields to the
    /// meeting row's values rather than failing.
    pub(crate) fn build_portable_manifest(&self, meeting: &Meeting) -> Option<RecordingManifest> {
        let recording = meeting.recording.clone()?;
        let ended_at = meeting.ended_at.unwrap_or_else(|| {
            meeting.started_at + chrono::Duration::milliseconds(recording.duration_ms as i64)
        });
        let mut manifest = RecordingManifest {
            version: MANIFEST_VERSION,
            meeting_id: meeting.id.clone(),
            note_id: meeting.note_id.clone(),
            ended_at,
            recording,
            title: Some(meeting.title.clone()),
            started_at: Some(meeting.started_at),
            attendees: Some(meeting.attendees.clone()),
            attendees_confirmed: Some(meeting.attendees_confirmed),
            folder_path: None,
        };
        if let Ok(note) = self.get_note(&meeting.note_id) {
            manifest.title = Some(note.title);
            manifest.folder_path = note
                .folder_id
                .and_then(|fid| self.folder_path_names(&fid).ok());
        }
        Some(manifest)
    }
}

/// Write `content` to `path` only when the file's current content differs
/// (keeps the startup backfill from rewriting every folder every launch).
/// Returns whether a write happened.
fn write_if_changed(path: &Path, content: &str) -> Result<bool> {
    if std::fs::read_to_string(path).is_ok_and(|cur| cur == content) {
        return Ok(false);
    }
    std::fs::write(path, content)?;
    Ok(true)
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

        // No meeting row: the database was lost/recreated (or this folder was
        // copied in from another machine). Restore the folder to its normal
        // home, then resurrect the rows under their ORIGINAL ids so the
        // folder's transcript.json (same meeting_id) still lines up and
        // provenance citations keep resolving. Manifest data wins over
        // derived values; a v1 manifest degrades to exactly the old behavior.
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
        let title = manifest
            .title
            .clone()
            .unwrap_or_else(|| naming::strip_date_prefix(&folder_name));
        let started_at = manifest.started_at.unwrap_or_else(|| {
            manifest.ended_at
                - chrono::Duration::milliseconds(manifest.recording.duration_ms as i64)
        });
        let folder_id = match manifest.folder_path.as_deref() {
            Some(names) => self.ensure_folder_path(names)?,
            None => None,
        };
        let dir_abs = manifest
            .dir_rel()
            .map(|r| self.data_dir.join(r))
            .unwrap_or_else(|| dir.to_path_buf());
        // The folder's faithful note mirror, when it carries this note.
        let portable: Option<PortableNote> = std::fs::read_to_string(dir_abs.join(NOTE_MIRROR))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .filter(|p: &PortableNote| p.note_id == manifest.note_id);

        if self.get_note(&manifest.note_id).is_err() {
            self.insert_recovered_note(
                &manifest.note_id,
                &title,
                folder_id.as_deref(),
                started_at,
                portable.as_ref(),
            )?;
        }
        self.conn.execute(
            "INSERT INTO meetings (id, title, note_id, attendees_json, attendees_confirmed, started_at, ended_at, recording_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            (
                &manifest.meeting_id,
                &title,
                &manifest.note_id,
                serde_json::to_string(&manifest.attendees.clone().unwrap_or_default())?,
                manifest.attendees_confirmed.unwrap_or(false) as i64,
                started_at.to_rfc3339(),
                manifest.ended_at.to_rfc3339(),
                serde_json::to_string(&manifest.recording)?,
            ),
        )?;
        self.conn.execute(
            "UPDATE notes SET meeting_id = ?1 WHERE id = ?2",
            (&manifest.meeting_id, &manifest.note_id),
        )?;
        // Rebuild what's derived (FTS, markdown mirror, note.json) now that
        // the note is whole again.
        if let Ok(note) = self.get_note(&manifest.note_id) {
            self.sync_note_derived(&note)?;
        }

        // Restore the transcript from the folder's own mirror when present.
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

    /// Note row for a resurrected meeting. Body (scratchpad + blocks) comes
    /// from the folder's `note.json` mirror when present; without it the
    /// note is the same empty stub as before the portability feature.
    /// Derived state (FTS, mirrors) is the caller's job via
    /// `sync_note_derived` once the meeting link is in place.
    fn insert_recovered_note(
        &self,
        note_id: &str,
        title: &str,
        folder_id: Option<&str>,
        created_at: DateTime<Utc>,
        portable: Option<&PortableNote>,
    ) -> Result<()> {
        let created_at = portable.map(|p| p.created_at).unwrap_or(created_at);
        let updated_at = portable.map(|p| p.updated_at).unwrap_or(created_at);
        let scratchpad = portable.map(|p| p.scratchpad.as_str()).unwrap_or("");
        let blocks_json = match portable {
            Some(p) => serde_json::to_string(&p.blocks)?,
            None => "[]".to_string(),
        };
        let disk_path = self.allocate_note_path(&naming::disk_label(created_at, title));
        self.conn.execute(
            "INSERT INTO notes (id, title, folder_id, meeting_id, scratchpad, blocks_json, attachments_json, created_at, updated_at, disk_path)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, '[]', ?6, ?7, ?8)",
            (
                note_id,
                title,
                folder_id,
                scratchpad,
                blocks_json,
                created_at.to_rfc3339(),
                updated_at.to_rfc3339(),
                &disk_path,
            ),
        )?;
        Ok(())
    }

    /// Build (and persist) the manifest for a just-finished recording, plus
    /// the note mirror so in-meeting scratchpad text is portable immediately.
    /// The write itself is filesystem-only and the enrichment is best-effort:
    /// with a broken database this still writes a v1-grade manifest, which is
    /// the whole point of stashing before the database write.
    pub fn stash_recording_manifest(
        &self,
        meeting_id: &str,
        note_id: &str,
        recording: &RecordingRef,
    ) -> Result<()> {
        let Some(rel) = crate::meetings::recording_dir_rel(recording) else {
            return Ok(()); // no folder to stash into
        };
        let mut manifest = RecordingManifest {
            version: MANIFEST_VERSION,
            meeting_id: meeting_id.to_string(),
            note_id: note_id.to_string(),
            ended_at: Utc::now(),
            recording: recording.clone(),
            title: None,
            started_at: None,
            attendees: None,
            attendees_confirmed: None,
            folder_path: None,
        };
        let dir = self.data_dir.join(rel);
        if let Ok(meeting) = self.get_meeting(meeting_id) {
            manifest.title = Some(meeting.title);
            manifest.started_at = Some(meeting.started_at);
            manifest.attendees = Some(meeting.attendees);
            manifest.attendees_confirmed = Some(meeting.attendees_confirmed);
        }
        if let Ok(note) = self.get_note(note_id) {
            manifest.title = Some(note.title.clone());
            manifest.folder_path = note
                .folder_id
                .as_deref()
                .and_then(|fid| self.folder_path_names(fid).ok());
            let _ = write_note_mirror(&dir, &PortableNote::from_note(&note));
        }
        write_recording_manifest(&dir, &manifest)?;
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
            version: 1,
            meeting_id: "m-orig".into(),
            note_id: "n-orig".into(),
            ended_at: Utc::now(),
            recording: rec(folder),
            title: None,
            started_at: None,
            attendees: None,
            attendees_confirmed: None,
            folder_path: None,
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

    fn copy_dir(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap().flatten() {
            let to = dst.join(e.file_name());
            if e.path().is_dir() {
                copy_dir(&e.path(), &to);
            } else {
                std::fs::copy(e.path(), &to).unwrap();
            }
        }
    }

    /// The portability contract: copying `recordings/` to a fresh machine
    /// (new database) and healing restores the meeting with FULL fidelity —
    /// title, exact dates, confirmed attendees, nested folder (reusing a
    /// same-named folder that already exists there), scratchpad, and the
    /// enhanced blocks with their provenance.
    #[test]
    fn copied_recordings_dir_restores_full_meeting_on_fresh_machine() {
        use fly_core::{Attendee, NoteBlock};
        let (tmp_a, a) = test_storage();
        let work = a.create_folder("Work", None).unwrap();
        let ones = a.create_folder("1:1s", Some(&work.id)).unwrap();
        let note = a.create_note("Tina 1-1", Some(&ones.id)).unwrap();
        let meeting = a.create_meeting("Tina 1-1", &note.id, &[]).unwrap();
        let dir = a
            .allocate_meeting_dir("Tina 1-1", meeting.started_at)
            .unwrap();
        std::fs::write(dir.join("recording.mixed.wav"), b"RIFF").unwrap();
        let rec = RecordingRef {
            mic_path: None,
            system_path: None,
            mixed_path: Some(format!(
                "recordings/{}/recording.mixed.wav",
                dir.file_name().unwrap().to_string_lossy()
            )),
            playback_path: None,
            duration_ms: 61_000,
        };
        a.stash_recording_manifest(&meeting.id, &note.id, &rec)
            .unwrap();
        a.end_meeting(&meeting.id, &rec).unwrap();
        a.update_attendees(
            &meeting.id,
            &[Attendee {
                name: "Priya Kapoor".into(),
                email: Some("priya@acme.com".into()),
            }],
        )
        .unwrap();
        a.update_note_scratchpad(&note.id, "- budget approved\n- follow up with dana")
            .unwrap();
        a.update_note_blocks(
            &note.id,
            &[
                NoteBlock::user("## Summary"),
                NoteBlock::ai("Decision: ship it", vec!["seg-1".into()]),
            ],
        )
        .unwrap();
        // edited meeting date must port exactly
        let new_start = "2026-06-01T15:30:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();
        a.set_meeting_started_at(&meeting.id, new_start).unwrap();
        let want_meeting = a.get_meeting(&meeting.id).unwrap();
        let want_note = a.get_note(&note.id).unwrap();

        // fresh machine: new database, copied recordings dir; "Work" already
        // exists there and must be reused, not duplicated
        let tmp_b = tempfile::tempdir().unwrap();
        copy_dir(
            &tmp_a.path().join("recordings"),
            &tmp_b.path().join("recordings"),
        );
        let b = crate::Storage::open(tmp_b.path()).unwrap();
        let work_b = b.create_folder("Work", None).unwrap();

        let report = b.self_heal_recordings().unwrap();
        assert_eq!(report.resurrected, vec![meeting.id.clone()]);

        let m = b.get_meeting(&meeting.id).unwrap();
        assert_eq!(m.title, "Tina 1-1");
        assert_eq!(m.started_at, want_meeting.started_at);
        assert_eq!(m.ended_at, want_meeting.ended_at);
        assert_eq!(m.attendees, want_meeting.attendees);
        assert!(m.attendees_confirmed);

        let n = b.get_note(&note.id).unwrap();
        assert_eq!(n.title, "Tina 1-1");
        assert_eq!(n.scratchpad, want_note.scratchpad);
        assert_eq!(n.blocks, want_note.blocks);

        // folder path recreated by name under the pre-existing root
        let folders = b.list_folders().unwrap();
        assert_eq!(
            folders.iter().filter(|f| f.name == "Work").count(),
            1,
            "existing folder must be reused, not duplicated"
        );
        let ones_b = folders.iter().find(|f| f.name == "1:1s").unwrap();
        assert_eq!(ones_b.parent_id.as_deref(), Some(work_b.id.as_str()));
        assert_eq!(n.folder_id.as_deref(), Some(ones_b.id.as_str()));

        // restored note is searchable by its body
        assert!(b
            .search("budget", 10)
            .unwrap()
            .iter()
            .any(|h| h.note_id == note.id));
        // second run is a no-op
        assert!(b.self_heal_recordings().unwrap().is_empty());
    }

    /// Manifests written before the portability fields existed (v1: only
    /// meeting_id/note_id/ended_at/recording) must heal exactly as today:
    /// title from the folder name, started_at derived from ended − duration,
    /// unfiled, empty attendees, stub note.
    #[test]
    fn legacy_v1_manifest_still_heals_like_today() {
        let (tmp, s) = test_storage();
        let folder = "2026-07-13 Big meeting";
        let dir = make_folder(tmp.path(), "recordings", folder);
        let ended = "2026-07-13T17:00:00Z".parse::<DateTime<Utc>>().unwrap();
        std::fs::write(
            dir.join(RECORDING_MANIFEST),
            format!(
                r#"{{
  "meeting_id": "m-legacy",
  "note_id": "n-legacy",
  "ended_at": "2026-07-13T17:00:00Z",
  "recording": {{
    "mic_path": "recordings/{folder}/recording.mic.wav",
    "system_path": null,
    "mixed_path": "recordings/{folder}/recording.mixed.wav",
    "playback_path": null,
    "duration_ms": 61000
  }}
}}"#
            ),
        )
        .unwrap();

        let report = s.self_heal_recordings().unwrap();
        assert_eq!(report.resurrected, vec!["m-legacy".to_string()]);
        let m = s.get_meeting("m-legacy").unwrap();
        assert_eq!(m.title, "Big meeting");
        assert_eq!(m.started_at, ended - chrono::Duration::milliseconds(61_000));
        assert_eq!(m.ended_at, Some(ended));
        assert!(m.attendees.is_empty());
        assert!(!m.attendees_confirmed);
        let n = s.get_note("n-legacy").unwrap();
        assert_eq!(n.folder_id, None);
        assert_eq!(n.scratchpad, "");
        assert!(n.blocks.is_empty());
    }

    /// Startup backfill: a meeting from before this feature (no manifest at
    /// all, no note mirror) gets a v2 manifest + note.json written on the
    /// next sweep, making its folder portable without re-saving the note.
    /// A second sweep rewrites nothing.
    #[test]
    fn startup_backfill_makes_existing_meetings_portable() {
        let (_tmp, s) = test_storage();
        let clients = s.create_folder("Clients", None).unwrap();
        let note = s.create_note("Acme kickoff", Some(&clients.id)).unwrap();
        let meeting = s.create_meeting("Acme kickoff", &note.id, &[]).unwrap();
        let dir = s
            .allocate_meeting_dir("Acme kickoff", meeting.started_at)
            .unwrap();
        std::fs::write(dir.join("recording.mixed.wav"), b"RIFF").unwrap();
        let rec = RecordingRef {
            mic_path: None,
            system_path: None,
            mixed_path: Some(format!(
                "recordings/{}/recording.mixed.wav",
                dir.file_name().unwrap().to_string_lossy()
            )),
            playback_path: None,
            duration_ms: 5_000,
        };
        s.end_meeting(&meeting.id, &rec).unwrap();
        s.update_note_scratchpad(&note.id, "kickoff notes").unwrap();
        // pre-feature folders have neither file
        std::fs::remove_file(dir.join(NOTE_MIRROR)).ok();
        std::fs::remove_file(dir.join(RECORDING_MANIFEST)).ok();

        let report = s.self_heal_recordings().unwrap();
        assert!(report.is_empty(), "nothing to heal, only to backfill");
        assert_eq!(report.refreshed, vec![meeting.id.clone()]);

        let manifest: RecordingManifest =
            serde_json::from_str(&std::fs::read_to_string(dir.join(RECORDING_MANIFEST)).unwrap())
                .unwrap();
        assert_eq!(manifest.version, MANIFEST_VERSION);
        assert_eq!(manifest.title.as_deref(), Some("Acme kickoff"));
        assert_eq!(manifest.started_at, Some(meeting.started_at));
        assert_eq!(manifest.folder_path, Some(vec!["Clients".to_string()]));
        let mirror = std::fs::read_to_string(dir.join(NOTE_MIRROR)).unwrap();
        assert!(mirror.contains("kickoff notes"));

        let again = s.self_heal_recordings().unwrap();
        assert!(again.refreshed.is_empty(), "backfill must be idempotent");
    }
}
