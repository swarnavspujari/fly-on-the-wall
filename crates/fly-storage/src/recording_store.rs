//! Recording storage v2 hooks: the app's compression job re-stores a
//! finished meeting's WAVs as lossless FLAC (see `fly_audio::flac` and the
//! app's `compress.rs`). This crate only provides the storage side:
//!
//! - folder leases, so a title/date edit doesn't rename a folder the job is
//!   writing into (the rename is skipped, like during a transcription);
//! - the list of meetings whose audio is still on disk;
//! - [`Storage::replace_meeting_recording`], the commit step: the folder's
//!   manifest first (it must never point at files about to be deleted), then
//!   `recording_json`. Callers delete the old files only after it succeeds.

use std::path::{Path, PathBuf};

use fly_core::{Meeting, RecordingRef};

use crate::meetings::{contained_recordings_dir, recording_dir_rel};
use crate::recovery::{RecordingManifest, MANIFEST_VERSION, RECORDING_MANIFEST};
use crate::{Result, Storage, StorageError};

/// `path_rel` (data-dir relative) as an absolute path, but only when it is a
/// plain file name directly inside the meeting folder `folder_rel`, itself
/// contained in `recordings/`. Recording paths can arrive from a copied-in
/// manifest, so a role pointing anywhere else (`..`, another meeting's
/// folder, an absolute path) is never converted, replaced or deleted.
pub fn recording_file_in_folder(
    data_dir: &Path,
    folder_rel: &str,
    path_rel: &str,
) -> Option<PathBuf> {
    let unified = path_rel.replace('\\', "/");
    let (parent, name) = unified.rsplit_once('/')?;
    if parent != folder_rel.replace('\\', "/") || !crate::safe_path::is_plain_name(name) {
        return None;
    }
    contained_recordings_dir(data_dir, folder_rel).map(|dir| dir.join(name))
}

/// Every path in `rec` is a file directly inside `folder_rel`.
fn all_paths_in_folder(data_dir: &Path, folder_rel: &str, rec: &RecordingRef) -> bool {
    [
        &rec.mic_path,
        &rec.system_path,
        &rec.mixed_path,
        &rec.playback_path,
    ]
    .into_iter()
    .flatten()
    .all(|p| recording_file_in_folder(data_dir, folder_rel, p).is_some())
}

/// Keep local only ever stays on: on if either side says on; a side that
/// doesn't say (`None`) defers to the other.
fn either_keeps_local(disk: Option<bool>, db: Option<bool>) -> Option<bool> {
    match (disk, db) {
        (Some(a), Some(b)) => Some(a || b),
        (a, None) => a,
        (None, b) => b,
    }
}

/// The manifest to write when a meeting's files are replaced: the one on
/// disk with only `recording` and `version` changed, so whatever it records
/// that the rows don't (or record differently) survives. Fields it lacks
/// (a v1/v2 manifest) are filled from the rows, and Keep local is merged so
/// it never turns off. No usable manifest on disk → the rows' manifest.
fn merged_manifest(
    on_disk: Option<RecordingManifest>,
    from_rows: RecordingManifest,
) -> RecordingManifest {
    let Some(mut m) = on_disk else {
        return from_rows;
    };
    m.version = MANIFEST_VERSION;
    m.recording = from_rows.recording;
    m.title = m.title.or(from_rows.title);
    m.started_at = m.started_at.or(from_rows.started_at);
    m.attendees = m.attendees.or(from_rows.attendees);
    m.attendees_confirmed = m.attendees_confirmed.or(from_rows.attendees_confirmed);
    m.keep_local = either_keeps_local(m.keep_local, from_rows.keep_local);
    match (&m.folder_path, &from_rows.folder_path) {
        (None, Some(_)) => {
            m.folder_path = from_rows.folder_path;
            m.folder_keep_local = from_rows.folder_keep_local;
        }
        (Some(disk), Some(rows)) if disk == rows => {
            match (&mut m.folder_keep_local, from_rows.folder_keep_local) {
                (Some(ours), Some(theirs)) if ours.len() == theirs.len() => {
                    for (o, t) in ours.iter_mut().zip(theirs) {
                        *o |= t;
                    }
                }
                (ours @ None, theirs) => *ours = theirs,
                _ => {}
            }
        }
        // Filed differently on disk: the mirror owns that; leave it.
        _ => {}
    }
    m
}

/// The manifest in a meeting folder was written by a NEWER build: rewriting
/// it would lower its version and drop what this build doesn't know, so the
/// folder's recording must be left alone. Unreadable or missing → `false`.
pub fn manifest_is_newer(dir: &std::path::Path) -> bool {
    std::fs::read_to_string(dir.join(RECORDING_MANIFEST))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("version").and_then(|v| v.as_u64()))
        .is_some_and(|v| v > u64::from(MANIFEST_VERSION))
}

impl Storage {
    /// Take the folder lease for a meeting. `false` when it's already held.
    pub fn lease_meeting_folder(&self, meeting_id: &str) -> bool {
        self.folder_leases
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(meeting_id.to_string())
    }

    /// Give a folder lease back (no-op when not held).
    pub fn release_meeting_folder(&self, meeting_id: &str) {
        self.folder_leases
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(meeting_id);
    }

    /// Whether a background job holds this meeting's folder.
    pub fn meeting_folder_leased(&self, meeting_id: &str) -> bool {
        self.folder_leases
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(meeting_id)
    }

    /// Finished meetings whose audio hasn't been deleted by retention,
    /// oldest first.
    pub fn meetings_with_stored_audio(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM meetings
              WHERE recording_json IS NOT NULL
                AND ended_at IS NOT NULL
                AND audio_deleted_at IS NULL
              ORDER BY started_at",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Point a meeting at new files in the SAME folder (the compression
    /// commit). Writes the folder's `recording.manifest.json` first (atomic),
    /// then `recording_json`. If either step fails the old files are still
    /// referenced somewhere and must be kept; the caller deletes them only
    /// after this returns `Ok`.
    ///
    /// Every path, old and new, must be a file directly inside the meeting's
    /// own folder. The manifest on disk is kept and only its `recording`
    /// (and version) replaced ([`merged_manifest`]); a manifest from a newer
    /// build, or one naming another meeting, is refused. Missing or
    /// unreadable → written fresh from the rows.
    pub fn replace_meeting_recording(&self, id: &str, recording: &RecordingRef) -> Result<Meeting> {
        let meeting = self.get_meeting(id)?;
        let old = meeting
            .recording
            .as_ref()
            .ok_or_else(|| StorageError::Invalid("meeting has no recording".into()))?;
        let old_rel = recording_dir_rel(old);
        if old_rel.is_none() || recording_dir_rel(recording) != old_rel {
            return Err(StorageError::Invalid(
                "a recording can only be replaced inside its own folder".into(),
            ));
        }
        let rel = old_rel.expect("checked above");
        let dir = contained_recordings_dir(&self.data_dir, &rel).ok_or_else(|| {
            StorageError::Invalid(format!("recording folder outside recordings/: {rel}"))
        })?;
        if !all_paths_in_folder(&self.data_dir, &rel, old)
            || !all_paths_in_folder(&self.data_dir, &rel, recording)
        {
            return Err(StorageError::Invalid(
                "a recording file is outside its meeting folder".into(),
            ));
        }
        let next = Meeting {
            recording: Some(recording.clone()),
            ..meeting
        };
        if manifest_is_newer(&dir) {
            return Err(StorageError::Invalid(
                "the folder's manifest is from a newer version of the app".into(),
            ));
        }
        let from_rows = self
            .build_portable_manifest(&next)
            .ok_or_else(|| StorageError::Invalid("no manifest for this meeting".into()))?;
        let on_disk: Option<RecordingManifest> =
            std::fs::read_to_string(dir.join(RECORDING_MANIFEST))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok());
        if on_disk.as_ref().is_some_and(|d| d.meeting_id != id) {
            return Err(StorageError::Invalid(
                "the folder's manifest belongs to another meeting".into(),
            ));
        }
        let manifest = merged_manifest(on_disk, from_rows);
        crate::recovery::write_recording_manifest(&dir, &manifest)?;
        self.conn.execute(
            "UPDATE meetings SET recording_json = ?1 WHERE id = ?2",
            (serde_json::to_string(recording)?, id),
        )?;
        self.get_meeting(id)
    }
}

impl Storage {
    /// Point every note attachment at `old_rel` (a video copy being
    /// replaced by its audio) to `new_rel`, renaming its display name and
    /// MIME type to match. Returns how many attachments changed.
    pub fn repoint_attachments(&self, old_rel: &str, new_rel: &str) -> Result<usize> {
        let new_name = new_rel.rsplit('/').next().unwrap_or(new_rel).to_string();
        let ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM notes WHERE instr(attachments_json, ?1) > 0")?;
            let rows = stmt.query_map([old_rel], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut changed = 0;
        for id in ids {
            let note = self.get_note(&id)?;
            let mut attachments = note.attachments.clone();
            let mut hit = false;
            for a in attachments.iter_mut().filter(|a| a.rel_path == old_rel) {
                a.rel_path = new_rel.to_string();
                a.file_name = new_name.clone();
                a.mime = audio_mime(&new_name);
                hit = true;
                changed += 1;
            }
            if hit {
                self.set_note_attachments(&id, &attachments)?;
            }
        }
        Ok(changed)
    }
}

fn audio_mime(name: &str) -> Option<String> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(
        match ext.as_str() {
            "m4a" => "audio/mp4",
            "mp3" => "audio/mpeg",
            "ogg" => "audio/ogg",
            "flac" => "audio/flac",
            "wav" => "audio/wav",
            "mka" => "audio/x-matroska",
            _ => return None,
        }
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use crate::recovery::{RecordingManifest, RECORDING_MANIFEST};
    use crate::test_storage;
    use fly_core::RecordingRef;

    fn recorded(s: &crate::Storage, folder: &str) -> String {
        let note = s.create_note("Weekly sync", None).unwrap();
        let m = s.create_meeting("Weekly sync", &note.id, &[]).unwrap();
        let dir = s.data_dir().join("recordings").join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["recording.mic.wav", "recording.system.wav"] {
            std::fs::write(dir.join(f), b"RIFF").unwrap();
        }
        s.end_meeting(
            &m.id,
            &RecordingRef {
                mic_path: Some(format!("recordings/{folder}/recording.mic.wav")),
                system_path: Some(format!("recordings/{folder}/recording.system.wav")),
                mixed_path: Some(format!("recordings/{folder}/recording.mixed.wav")),
                playback_path: Some(format!("recordings/{folder}/recording.playback.wav")),
                duration_ms: 1000,
            },
        )
        .unwrap();
        m.id
    }

    #[test]
    fn replace_writes_the_manifest_then_the_row() {
        let (_d, s) = test_storage();
        let id = recorded(&s, "2026-01-02 Weekly sync");
        let next = RecordingRef {
            mic_path: Some("recordings/2026-01-02 Weekly sync/recording.mic.flac".into()),
            system_path: Some("recordings/2026-01-02 Weekly sync/recording.system.flac".into()),
            mixed_path: None,
            playback_path: None,
            duration_ms: 1000,
        };
        let m = s.replace_meeting_recording(&id, &next).unwrap();
        assert_eq!(m.recording.as_ref(), Some(&next));
        let manifest: RecordingManifest = serde_json::from_str(
            &std::fs::read_to_string(
                s.data_dir()
                    .join("recordings/2026-01-02 Weekly sync")
                    .join(RECORDING_MANIFEST),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.recording, next);
        assert_eq!(manifest.meeting_id, id);
        assert_eq!(manifest.title.as_deref(), Some("Weekly sync"));
    }

    #[test]
    fn replace_keeps_the_manifest_v3_payload_and_never_turns_keep_local_off() {
        let (_d, s) = test_storage();
        let id = recorded(&s, "a");
        let note_id = s.get_meeting(&id).unwrap().note_id;
        s.set_note_keep_local(&note_id, true).unwrap();
        let next = RecordingRef {
            mic_path: Some("recordings/a/recording.mic.flac".into()),
            system_path: Some("recordings/a/recording.system.flac".into()),
            mixed_path: None,
            playback_path: None,
            duration_ms: 1000,
        };
        s.replace_meeting_recording(&id, &next).unwrap();
        let read = || -> RecordingManifest {
            serde_json::from_str(
                &std::fs::read_to_string(
                    s.data_dir().join("recordings/a").join(RECORDING_MANIFEST),
                )
                .unwrap(),
            )
            .unwrap()
        };
        let m = read();
        assert_eq!(m.version, crate::MANIFEST_VERSION);
        assert_eq!(m.keep_local, Some(true), "from the row");
        // The file says on, the row says off (not merged yet): stays on.
        s.conn
            .execute("UPDATE meetings SET keep_local = 0 WHERE id = ?1", [&id])
            .unwrap();
        s.replace_meeting_recording(&id, &next).unwrap();
        assert_eq!(read().keep_local, Some(true));
    }

    #[test]
    fn replace_never_lowers_a_newer_manifest() {
        let (_d, s) = test_storage();
        let id = recorded(&s, "a");
        let dir = s.data_dir().join("recordings/a");
        let future = format!(
            r#"{{"version": 99, "meeting_id": "{id}", "note_id": "n", "ended_at": "2026-01-01T00:00:00Z",
                "recording": {{"mic_path": null, "system_path": null, "mixed_path": null, "duration_ms": 0}},
                "from_the_future": true}}"#
        );
        std::fs::write(dir.join(RECORDING_MANIFEST), &future).unwrap();
        assert!(super::manifest_is_newer(&dir));
        let next = RecordingRef {
            mic_path: Some("recordings/a/recording.mic.flac".into()),
            system_path: None,
            mixed_path: None,
            playback_path: None,
            duration_ms: 1000,
        };
        assert!(s.replace_meeting_recording(&id, &next).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join(RECORDING_MANIFEST)).unwrap(),
            future
        );
        assert!(s
            .get_meeting(&id)
            .unwrap()
            .recording
            .unwrap()
            .mic_path
            .unwrap()
            .ends_with(".wav"));
    }

    #[test]
    fn repoint_attachments_moves_a_video_attachment_to_its_audio() {
        let (_d, s) = test_storage();
        let note = s.create_note("Board call", None).unwrap();
        let rel = "recordings/a/Board call.mp4";
        s.set_note_attachments(
            &note.id,
            &[fly_core::Attachment {
                id: "att1".into(),
                file_name: "Board call.mp4".into(),
                rel_path: rel.into(),
                mime: Some("video/mp4".into()),
                added_at: chrono::Utc::now(),
            }],
        )
        .unwrap();
        let other = s.create_note("Other", None).unwrap();
        assert_eq!(
            s.repoint_attachments(rel, "recordings/a/Board call.m4a")
                .unwrap(),
            1
        );
        let a = &s.get_note(&note.id).unwrap().attachments[0];
        assert_eq!(
            (
                a.id.as_str(),
                a.rel_path.as_str(),
                a.file_name.as_str(),
                a.mime.as_deref()
            ),
            (
                "att1",
                "recordings/a/Board call.m4a",
                "Board call.m4a",
                Some("audio/mp4")
            )
        );
        assert!(s.get_note(&other.id).unwrap().attachments.is_empty());
        assert_eq!(
            s.repoint_attachments(rel, "x").unwrap(),
            0,
            "nothing left to move"
        );
    }

    #[test]
    fn replace_refuses_another_folder() {
        let (_d, s) = test_storage();
        let id = recorded(&s, "a");
        let elsewhere = RecordingRef {
            mic_path: Some("recordings/b/recording.mic.flac".into()),
            system_path: None,
            mixed_path: None,
            playback_path: None,
            duration_ms: 1000,
        };
        assert!(s.replace_meeting_recording(&id, &elsewhere).is_err());
        let escape = RecordingRef {
            mic_path: Some("notes/x.flac".into()),
            ..elsewhere
        };
        assert!(s.replace_meeting_recording(&id, &escape).is_err());
        // nothing changed
        let m = s.get_meeting(&id).unwrap();
        assert!(m.recording.unwrap().mic_path.unwrap().ends_with(".wav"));
    }

    fn flac_next(folder: &str) -> RecordingRef {
        RecordingRef {
            mic_path: Some(format!("recordings/{folder}/recording.mic.flac")),
            system_path: Some(format!("recordings/{folder}/recording.system.flac")),
            mixed_path: None,
            playback_path: None,
            duration_ms: 1000,
        }
    }

    fn read_manifest(s: &crate::Storage, folder: &str) -> RecordingManifest {
        serde_json::from_str(
            &std::fs::read_to_string(
                s.data_dir()
                    .join("recordings")
                    .join(folder)
                    .join(RECORDING_MANIFEST),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn replace_keeps_what_the_manifest_on_disk_records() {
        let (_d, s) = test_storage();
        let id = recorded(&s, "a");
        let dir = s.data_dir().join("recordings/a");
        // Written by another copy of the app: a different title and
        // attendees than the rows, and Keep local on.
        let disk = format!(
            r#"{{"version": {v}, "meeting_id": "{id}", "note_id": "n-disk",
                "ended_at": "2026-01-01T00:00:00Z",
                "recording": {{"mic_path": "recordings/a/recording.mic.wav", "system_path": null, "mixed_path": null, "duration_ms": 1000}},
                "title": "Title on disk", "attendees": [{{"name": "Sample Person"}}],
                "keep_local": true}}"#,
            v = crate::MANIFEST_VERSION
        );
        std::fs::write(dir.join(RECORDING_MANIFEST), disk).unwrap();
        s.replace_meeting_recording(&id, &flac_next("a")).unwrap();
        let m = read_manifest(&s, "a");
        assert_eq!(m.recording, flac_next("a"), "only the recording changes");
        assert_eq!(m.title.as_deref(), Some("Title on disk"));
        assert_eq!(m.note_id, "n-disk");
        assert_eq!(m.attendees.as_ref().map(Vec::len), Some(1));
        assert_eq!(m.keep_local, Some(true));
        assert_eq!(m.ended_at.to_rfc3339(), "2026-01-01T00:00:00+00:00");
        assert_eq!(m.version, crate::MANIFEST_VERSION);
    }

    #[test]
    fn replace_fills_what_an_old_manifest_lacks_from_the_rows() {
        let (_d, s) = test_storage();
        let id = recorded(&s, "a");
        let dir = s.data_dir().join("recordings/a");
        // A v1 manifest: no version field, no title, no Keep local.
        let v1 = format!(
            r#"{{"meeting_id": "{id}", "note_id": "n", "ended_at": "2026-01-01T00:00:00Z",
                "recording": {{"mic_path": "recordings/a/recording.mic.wav", "system_path": null, "mixed_path": null, "duration_ms": 1000}}}}"#
        );
        std::fs::write(dir.join(RECORDING_MANIFEST), v1).unwrap();
        s.replace_meeting_recording(&id, &flac_next("a")).unwrap();
        let m = read_manifest(&s, "a");
        assert_eq!(m.version, crate::MANIFEST_VERSION);
        assert_eq!(m.title.as_deref(), Some("Weekly sync"), "from the rows");
        assert_eq!(m.keep_local, Some(false), "from the rows");
        assert!(m.started_at.is_some() && m.attendees.is_some());
    }

    #[test]
    fn replace_refuses_a_manifest_that_names_another_meeting() {
        let (_d, s) = test_storage();
        let id = recorded(&s, "a");
        let dir = s.data_dir().join("recordings/a");
        let other = r#"{"version": 1, "meeting_id": "someone-else", "note_id": "n", "ended_at": "2026-01-01T00:00:00Z",
            "recording": {"mic_path": null, "system_path": null, "mixed_path": null, "duration_ms": 0}}"#;
        std::fs::write(dir.join(RECORDING_MANIFEST), other).unwrap();
        assert!(s.replace_meeting_recording(&id, &flac_next("a")).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join(RECORDING_MANIFEST)).unwrap(),
            other
        );
        let rec = s.get_meeting(&id).unwrap().recording.unwrap();
        assert!(rec.mic_path.unwrap().ends_with(".wav"), "row untouched");
    }

    #[test]
    fn replace_writes_a_fresh_manifest_over_an_unreadable_one() {
        let (_d, s) = test_storage();
        let id = recorded(&s, "a");
        let dir = s.data_dir().join("recordings/a");
        std::fs::write(dir.join(RECORDING_MANIFEST), "{ torn").unwrap();
        s.replace_meeting_recording(&id, &flac_next("a")).unwrap();
        let m = read_manifest(&s, "a");
        assert_eq!(m.meeting_id, id);
        assert_eq!(m.recording, flac_next("a"));
    }

    #[test]
    fn replace_refuses_any_file_outside_the_meeting_folder() {
        let (_d, s) = test_storage();
        let id = recorded(&s, "a");
        for stray in [
            "recordings/b/recording.playback.wav",
            "recordings/a/../b/recording.playback.wav",
            "recordings/a/sub/recording.playback.wav",
        ] {
            let next = RecordingRef {
                playback_path: Some(stray.into()),
                ..flac_next("a")
            };
            assert!(s.replace_meeting_recording(&id, &next).is_err(), "{stray}");
        }
        let rec = s.get_meeting(&id).unwrap().recording.unwrap();
        assert!(rec.mic_path.unwrap().ends_with(".wav"), "row untouched");
        // and the helper itself
        let d = s.data_dir();
        assert!(
            super::recording_file_in_folder(d, "recordings/a", "recordings/a/x.flac").is_some()
        );
        assert!(
            super::recording_file_in_folder(d, "recordings/a", "recordings/b/x.flac").is_none()
        );
        assert!(super::recording_file_in_folder(d, "recordings/a", "recordings/a/..").is_none());
        assert!(super::recording_file_in_folder(d, "../a", "../a/x.flac").is_none());
    }

    #[test]
    fn a_leased_folder_is_not_renamed_until_released() {
        let (_d, s) = test_storage();
        let id = recorded(&s, "2026-01-02 Weekly sync");
        let note_id = s.get_meeting(&id).unwrap().note_id;
        assert!(s.lease_meeting_folder(&id));
        assert!(!s.lease_meeting_folder(&id), "one holder at a time");
        s.update_note_title(&note_id, "Planning").unwrap();
        let rel = |s: &crate::Storage| {
            crate::recording_dir_rel(s.get_meeting(&id).unwrap().recording.as_ref().unwrap())
                .unwrap()
        };
        assert_eq!(rel(&s), "recordings/2026-01-02 Weekly sync");
        s.release_meeting_folder(&id);
        s.update_note_title(&note_id, "Planning 2").unwrap();
        assert!(rel(&s).ends_with("Planning 2"), "{}", rel(&s));
    }

    #[test]
    fn stored_audio_list_skips_deleted_audio_and_unfinished_meetings() {
        let (_d, s) = test_storage();
        let a = recorded(&s, "a");
        let b = recorded(&s, "b");
        let note = s.create_note("Live", None).unwrap();
        s.create_meeting("Live", &note.id, &[]).unwrap();
        s.delete_meeting_audio(&b).unwrap();
        assert_eq!(s.meetings_with_stored_audio().unwrap(), vec![a]);
    }
}
