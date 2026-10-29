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

use fly_core::{Meeting, RecordingRef};

use crate::meetings::{contained_recordings_dir, recording_dir_rel};
use crate::recovery::{RecordingManifest, MANIFEST_VERSION, RECORDING_MANIFEST};
use crate::{Result, Storage, StorageError};

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
    /// The manifest is rebuilt from the rows (the full current payload, Keep
    /// local included), never with a lower version: a manifest from a newer
    /// build is refused. Keep local only ever stays on: if the file says on
    /// and the row says off (a heal hasn't merged them yet), it stays on.
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
        let next = Meeting {
            recording: Some(recording.clone()),
            ..meeting
        };
        if manifest_is_newer(&dir) {
            return Err(StorageError::Invalid(
                "the folder's manifest is from a newer version of the app".into(),
            ));
        }
        let mut manifest = self
            .build_portable_manifest(&next)
            .ok_or_else(|| StorageError::Invalid("no manifest for this meeting".into()))?;
        let on_disk: Option<RecordingManifest> =
            std::fs::read_to_string(dir.join(RECORDING_MANIFEST))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok());
        if let Some(disk) = on_disk.filter(|d| d.meeting_id == manifest.meeting_id) {
            if disk.keep_local == Some(true) {
                manifest.keep_local = Some(true);
            }
            if disk.folder_path == manifest.folder_path {
                match (&mut manifest.folder_keep_local, disk.folder_keep_local) {
                    (Some(ours), Some(theirs)) if ours.len() == theirs.len() => {
                        for (o, t) in ours.iter_mut().zip(theirs) {
                            *o |= t;
                        }
                    }
                    (ours @ None, theirs) => *ours = theirs,
                    _ => {}
                }
            }
        }
        crate::recovery::write_recording_manifest(&dir, &manifest)?;
        self.conn.execute(
            "UPDATE meetings SET recording_json = ?1 WHERE id = ?2",
            (serde_json::to_string(recording)?, id),
        )?;
        self.get_meeting(id)
    }
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
