//! Storage safety fixes (v2.4 PR 0.4): nothing here changes a stored format
//! or the folder layout — these pin down that the app stops damaging files
//! it already writes.
//!
//! - a fresh database never runs the v2 orphan sweep (no mass rename into
//!   `_unlinked/`);
//! - the startup backfill never overwrites an on-disk mirror that may be
//!   newer than the database;
//! - `-wal` / `-shm` survive close (persistent WAL);
//! - pipeline scratch lives outside the meeting folder, keyed by meeting id.
//!
//! (Atomic writes are unit-tested in `src/atomic.rs`, where a write can be
//! made to fail halfway.)

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use fly_core::{Attendee, RecordingRef};
use fly_storage::{PortableNote, RecordingManifest, Storage, NOTE_MIRROR, RECORDING_MANIFEST};
use rusqlite::Connection;

fn rec_in(folder: &str) -> RecordingRef {
    RecordingRef {
        mic_path: None,
        system_path: None,
        mixed_path: Some(format!("recordings/{folder}/recording.mixed.wav")),
        playback_path: None,
        duration_ms: 61_000,
    }
}

fn folder_name(dir: &Path) -> String {
    dir.file_name().unwrap().to_string_lossy().into_owned()
}

/// A finished, recorded meeting: note + meeting rows, a folder with a WAV,
/// the manifest stashed at stop, and the recording attached.
fn recorded_meeting(s: &Storage, title: &str) -> (String, String, PathBuf) {
    let note = s.create_note(title, None).unwrap();
    let meeting = s.create_meeting(title, &note.id, &[]).unwrap();
    let dir = s.allocate_meeting_dir(title, meeting.started_at).unwrap();
    std::fs::write(dir.join("recording.mixed.wav"), b"RIFF").unwrap();
    let rec = rec_in(&folder_name(&dir));
    s.stash_recording_manifest(&meeting.id, &note.id, &rec)
        .unwrap();
    s.end_meeting(&meeting.id, &rec).unwrap();
    (note.id, meeting.id, dir)
}

fn read_manifest(dir: &Path) -> RecordingManifest {
    serde_json::from_str(&std::fs::read_to_string(dir.join(RECORDING_MANIFEST)).unwrap()).unwrap()
}

fn read_note_mirror(dir: &Path) -> PortableNote {
    serde_json::from_str(&std::fs::read_to_string(dir.join(NOTE_MIRROR)).unwrap()).unwrap()
}

fn user_version(data_dir: &Path) -> i64 {
    Connection::open(data_dir.join("flyonthewall.db"))
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

// ---- (a) fresh database -------------------------------------------------

/// The 2026-07-13 disaster, replayed: the index file is gone but the data
/// dir still holds meeting folders and note mirrors. Opening must NOT park
/// them under `_unlinked/` (on a synced Library that's a mass rename); the
/// version is stamped and self-heal re-attaches the folder where it is.
#[test]
fn fresh_database_never_creates_unlinked() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = "2026-07-13 1000 Big meeting";
    let meeting_dir = tmp.path().join("recordings").join(folder);
    std::fs::create_dir_all(&meeting_dir).unwrap();
    std::fs::write(meeting_dir.join("recording.mixed.wav"), b"RIFF").unwrap();
    std::fs::write(meeting_dir.join("transcript.md"), "# transcript").unwrap();
    let manifest = RecordingManifest {
        version: 2,
        meeting_id: "m-orig".into(),
        note_id: "n-orig".into(),
        ended_at: "2026-07-13T15:00:00Z".parse().unwrap(),
        recording: rec_in(folder),
        title: Some("Big meeting".into()),
        started_at: Some("2026-07-13T14:00:00Z".parse().unwrap()),
        attendees: None,
        attendees_confirmed: None,
        folder_path: None,
        keep_local: None,
        folder_keep_local: None,
    };
    std::fs::write(
        meeting_dir.join(RECORDING_MANIFEST),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    // a meeting folder with no manifest, and a note mirror with no row
    let bare_dir = tmp.path().join("recordings").join("2026-07-01 Old call");
    std::fs::create_dir_all(&bare_dir).unwrap();
    std::fs::write(bare_dir.join("recording.mixed.wav"), b"RIFF").unwrap();
    std::fs::create_dir_all(tmp.path().join("notes")).unwrap();
    let mirror = tmp.path().join("notes").join("2026-07-13 Big meeting.md");
    std::fs::write(&mirror, "# Big meeting\n\nbudget approved").unwrap();

    let s = Storage::open(tmp.path()).unwrap();

    assert_eq!(
        user_version(tmp.path()),
        Storage::SCHEMA_VERSION,
        "fresh database is stamped"
    );
    assert!(!tmp.path().join("recordings/_unlinked").exists());
    assert!(!tmp.path().join("notes/_unlinked").exists());
    assert!(meeting_dir.join("recording.mixed.wav").exists());
    assert!(bare_dir.join("recording.mixed.wav").exists());
    assert_eq!(
        std::fs::read_to_string(&mirror).unwrap(),
        "# Big meeting\n\nbudget approved"
    );

    // self-heal resurrects the meeting in place: no move, no new name
    let report = s.self_heal_recordings().unwrap();
    assert_eq!(report.resurrected, vec!["m-orig".to_string()]);
    let m = s.get_meeting("m-orig").unwrap();
    assert_eq!(
        m.recording.unwrap().mixed_path.as_deref(),
        Some(format!("recordings/{folder}/recording.mixed.wav").as_str())
    );
    assert!(!tmp.path().join("recordings/_unlinked").exists());

    // and a reopen of the now-current database is a no-op too
    drop(s);
    Storage::open(tmp.path()).unwrap();
    assert!(!tmp.path().join("recordings/_unlinked").exists());
    assert!(bare_dir.is_dir());
}

/// An existing (pre-v2) database keeps today's behavior: the v2 sweep still
/// parks folders no row points at — including an UNVERSIONED database
/// (`user_version` 0, the earliest builds never stamped one) that has
/// tables. Only a database with no tables at all counts as fresh. (The full
/// v1 fixture lives in `migration_v2.rs`; this pins the boundary.)
#[test]
fn existing_pre_v2_database_still_parks_orphans() {
    for version in [0, 1] {
        let tmp = tempfile::tempdir().unwrap();
        {
            let c = Connection::open(tmp.path().join("flyonthewall.db")).unwrap();
            c.execute_batch(&format!(
                "CREATE TABLE notes (id TEXT PRIMARY KEY, title TEXT NOT NULL,
                                     created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 PRAGMA user_version = {version};"
            ))
            .unwrap();
        }
        let orphan = tmp.path().join("recordings").join("orphan-folder");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("recording.mixed.wav"), b"RIFF").unwrap();

        Storage::open(tmp.path()).unwrap();
        assert!(
            tmp.path()
                .join("recordings/_unlinked/orphan-folder/recording.mixed.wav")
                .exists(),
            "user_version {version}"
        );
        assert_eq!(user_version(tmp.path()), Storage::SCHEMA_VERSION);
    }
}

// ---- (c) backfill never clobbers ----------------------------------------

/// Both mirrors on disk are NEWER than the database (a restored or forked
/// index, or a folder copied back from another machine): the startup
/// backfill must leave them byte for byte.
#[test]
fn backfill_does_not_clobber_newer_user_edited_files() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Storage::open(tmp.path()).unwrap();
    let (note_id, meeting_id, dir) = recorded_meeting(&s, "Acme kickoff");
    s.update_note_scratchpad(&note_id, "old notes").unwrap();

    // note.json edited elsewhere, later than the database's copy
    let mut newer = read_note_mirror(&dir);
    newer.scratchpad = "edited on the laptop".into();
    newer.updated_at += Duration::hours(1);
    let note_text = serde_json::to_string_pretty(&newer).unwrap();
    std::fs::write(dir.join(NOTE_MIRROR), &note_text).unwrap();

    // manifest with values the database doesn't have (hand-formatted too)
    let mut edited = read_manifest(&dir);
    edited.title = Some("Acme kickoff (final)".into());
    edited.attendees = Some(vec![Attendee {
        name: "Priya Kapoor".into(),
        email: None,
    }]);
    edited.attendees_confirmed = Some(true);
    let manifest_text = serde_json::to_string(&edited).unwrap(); // compact
    std::fs::write(dir.join(RECORDING_MANIFEST), &manifest_text).unwrap();

    let report = s.self_heal_recordings().unwrap();
    assert!(report.refreshed.is_empty(), "{report:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join(NOTE_MIRROR)).unwrap(),
        note_text
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(RECORDING_MANIFEST)).unwrap(),
        manifest_text
    );

    // same age but different content (a hand edit) is left alone too
    let mut same_age = read_note_mirror(&dir);
    same_age.updated_at = s.get_note(&note_id).unwrap().updated_at;
    same_age.scratchpad = "hand edit".into();
    let same_age_text = serde_json::to_string_pretty(&same_age).unwrap();
    std::fs::write(dir.join(NOTE_MIRROR), &same_age_text).unwrap();
    s.self_heal_recordings().unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join(NOTE_MIRROR)).unwrap(),
        same_age_text
    );

    // files it can't read (torn, or a future format) are never replaced
    std::fs::write(dir.join(NOTE_MIRROR), "{torn").unwrap();
    let mut future = read_manifest(&dir);
    future.version = fly_storage::MANIFEST_VERSION + 1;
    let future_text = serde_json::to_string_pretty(&future).unwrap();
    std::fs::write(dir.join(RECORDING_MANIFEST), &future_text).unwrap();
    s.self_heal_recordings().unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join(NOTE_MIRROR)).unwrap(),
        "{torn"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(RECORDING_MANIFEST)).unwrap(),
        future_text
    );
    assert!(s.get_meeting(&meeting_id).unwrap().recording.is_some());
}

/// What the backfill is FOR still works: missing mirrors are created, an
/// older note.json is refreshed, and a v1 manifest gains the v2 payload
/// without losing any value it already had.
#[test]
fn backfill_still_fills_missing_and_provably_stale_files() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Storage::open(tmp.path()).unwrap();
    let (note_id, meeting_id, dir) = recorded_meeting(&s, "Weekly sync");
    s.update_note_scratchpad(&note_id, "current notes").unwrap();

    // note.json older than the row → refreshed from the database
    let mut stale = read_note_mirror(&dir);
    stale.scratchpad = "an older draft".into();
    stale.updated_at -= Duration::hours(1);
    std::fs::write(
        dir.join(NOTE_MIRROR),
        serde_json::to_string_pretty(&stale).unwrap(),
    )
    .unwrap();
    // v1 manifest (v1.2.2–v1.3.x): only the re-attachment core
    let ended: DateTime<Utc> = "2026-07-13T17:00:00Z".parse().unwrap();
    let folder = folder_name(&dir);
    std::fs::write(
        dir.join(RECORDING_MANIFEST),
        format!(
            r#"{{"meeting_id":"{meeting_id}","note_id":"{note_id}","ended_at":"2026-07-13T17:00:00Z",
                "recording":{{"mic_path":null,"system_path":null,
                "mixed_path":"recordings/{folder}/recording.mixed.wav","playback_path":null,
                "duration_ms":61000}}}}"#
        ),
    )
    .unwrap();

    let report = s.self_heal_recordings().unwrap();
    assert_eq!(report.refreshed, vec![meeting_id.clone()]);
    assert_eq!(read_note_mirror(&dir).scratchpad, "current notes");
    let m = read_manifest(&dir);
    assert_eq!(m.version, fly_storage::MANIFEST_VERSION);
    assert_eq!(m.title.as_deref(), Some("Weekly sync"));
    assert!(m.started_at.is_some());
    assert_eq!(m.ended_at, ended, "values already in the file win");

    // both missing → both written; then the sweep is read-only
    std::fs::remove_file(dir.join(NOTE_MIRROR)).unwrap();
    std::fs::remove_file(dir.join(RECORDING_MANIFEST)).unwrap();
    assert_eq!(
        s.self_heal_recordings().unwrap().refreshed,
        vec![meeting_id.clone()]
    );
    assert!(dir.join(NOTE_MIRROR).is_file());
    assert!(dir.join(RECORDING_MANIFEST).is_file());
    assert!(s.self_heal_recordings().unwrap().refreshed.is_empty());
}

/// The backfill no longer propagates edits into existing manifests, so the
/// edits do it themselves: folder rename/move/delete and a title edit that
/// couldn't rename its folder (transcription running) all reach the file.
#[test]
fn edits_refresh_manifests_without_the_backfill() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Storage::open(tmp.path()).unwrap();
    let clients = s.create_folder("Clients", None).unwrap();
    let acme = s.create_folder("Acme", Some(&clients.id)).unwrap();
    let (note_id, meeting_id, dir) = recorded_meeting(&s, "Kickoff");
    s.move_note(&note_id, Some(&acme.id)).unwrap();
    assert_eq!(
        read_manifest(&dir).folder_path,
        Some(vec!["Clients".to_string(), "Acme".to_string()])
    );

    s.rename_folder(&clients.id, "Customers").unwrap();
    assert_eq!(
        read_manifest(&dir).folder_path,
        Some(vec!["Customers".to_string(), "Acme".to_string()])
    );
    s.move_folder(&acme.id, None).unwrap();
    assert_eq!(
        read_manifest(&dir).folder_path,
        Some(vec!["Acme".to_string()])
    );
    s.delete_folder(&acme.id).unwrap();
    assert_eq!(read_manifest(&dir).folder_path, None, "now unfiled");

    // title edit while transcribing: the folder keeps its name, the
    // manifest still gets the new title
    s.enqueue_transcription(&meeting_id).unwrap();
    s.mark_transcription_running(&meeting_id).unwrap();
    s.update_note_title(&note_id, "Kickoff (renamed)").unwrap();
    assert!(dir.is_dir(), "running job: folder not renamed");
    assert_eq!(
        read_manifest(&dir).title.as_deref(),
        Some("Kickoff (renamed)")
    );
}

// ---- (d) persistent WAL -------------------------------------------------

/// `-wal` and `-shm` stay on disk after the last connection closes — for
/// the app's open and for the MCP server's `open_existing` alike — and the
/// WAL is truncated to zero bytes so no old page images linger in it.
#[test]
fn wal_and_shm_persist_after_close() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("flyonthewall.db");
    let wal = tmp.path().join("flyonthewall.db-wal");
    let shm = tmp.path().join("flyonthewall.db-shm");
    {
        let s = Storage::open(tmp.path()).unwrap();
        s.create_note("Persist me", None).unwrap();
    }
    assert!(db.is_file());
    assert!(wal.is_file(), "-wal deleted on close");
    assert!(shm.is_file(), "-shm deleted on close");
    assert_eq!(
        std::fs::metadata(&wal).unwrap().len(),
        0,
        "WAL not truncated"
    );

    {
        let s = Storage::open_existing(tmp.path()).unwrap();
        let notes = s
            .list_notes_filtered(10, None, None, None, None, false)
            .unwrap();
        assert!(notes.iter().any(|n| n.title == "Persist me"));
        s.create_folder("written by the second process", None)
            .unwrap();
    }
    assert!(
        wal.is_file() && shm.is_file(),
        "open_existing must persist too"
    );
    assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);

    // everything committed is in the main file and reopens cleanly
    let s = Storage::open(tmp.path()).unwrap();
    assert!(s
        .list_folders()
        .unwrap()
        .iter()
        .any(|f| f.name == "written by the second process"));
}

// ---- (e) scratch outside the meeting folder ------------------------------

/// The 2026-07-19 import incident: a title edit renamed the meeting folder
/// under a running pipeline, whose 16 kHz intermediate path pointed inside
/// it. Scratch is keyed by meeting id and lives outside `recordings/`, so a
/// rename can't move it.
#[test]
fn scratch_path_is_independent_of_the_folder_name() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Storage::open(tmp.path()).unwrap();
    let (note_id, meeting_id, dir) = recorded_meeting(&s, "Import");
    let scratch = s.scratch_dir(&meeting_id);
    assert_eq!(
        scratch,
        fly_storage::scratch::meeting_scratch_dir(tmp.path(), &meeting_id)
    );
    assert!(!scratch.starts_with(tmp.path().join("recordings")));
    assert!(!scratch.starts_with(&dir));

    let prepared =
        fly_storage::scratch::prepare_meeting_scratch(tmp.path(), &meeting_id, Some(&dir)).unwrap();
    assert_eq!(prepared, scratch);
    std::fs::write(scratch.join("track.16k.wav"), b"RIFF").unwrap();

    s.update_note_title(&note_id, "Quarterly review").unwrap();
    let renamed = s.get_meeting(&meeting_id).unwrap().recording.unwrap();
    let new_rel = fly_storage::recording_dir_rel(&renamed).unwrap();
    assert_ne!(
        new_rel,
        format!("recordings/{}", folder_name(&dir)),
        "the folder really was renamed"
    );
    assert!(!dir.exists() && tmp.path().join(&new_rel).is_dir());
    assert_eq!(
        s.scratch_dir(&meeting_id),
        scratch,
        "scratch path unchanged"
    );
    assert!(scratch.join("track.16k.wav").is_file());
}

/// Scratch older builds left inside meeting folders is swept at startup
/// once nothing can resume from it; a failed job's checkpoints (a retry
/// resumes from them) are kept. Deleting a meeting, or retention deleting
/// its audio, drops its cache scratch. (The cache-folder sweep and its grace
/// period are unit-tested in `src/scratch.rs`.)
#[test]
fn stale_scratch_is_swept_but_resumable_scratch_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Storage::open(tmp.path()).unwrap();
    let (_, done_id, done_dir) = recorded_meeting(&s, "Done meeting");
    let (_, failed_id, failed_dir) = recorded_meeting(&s, "Failed meeting");
    for dir in [&done_dir, &failed_dir] {
        std::fs::write(dir.join("track.16k.wav"), wav_header(16_000, 1)).unwrap();
        std::fs::write(dir.join("track.16k.asrpart.json"), b"{}").unwrap();
        std::fs::create_dir_all(dir.join("live-tmp")).unwrap();
    }
    s.enqueue_transcription(&failed_id).unwrap();
    s.mark_transcription_failed(&failed_id, 3, "boom").unwrap();
    let done_scratch = s.scratch_dir(&done_id);
    let failed_scratch = s.scratch_dir(&failed_id);
    for d in [&done_scratch, &failed_scratch] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(d.join("mic.16k.wav"), b"RIFF").unwrap();
    }

    s.self_heal_recordings().unwrap();

    // finished meeting: old in-folder scratch gone, recording untouched
    assert!(!done_dir.join("track.16k.wav").exists());
    assert!(!done_dir.join("track.16k.asrpart.json").exists());
    assert!(!done_dir.join("live-tmp").exists());
    assert!(done_dir.join("recording.mixed.wav").exists());
    // failed job: everything a retry resumes from stays
    assert!(failed_dir.join("track.16k.asrpart.json").exists());
    assert!(failed_scratch.join("mic.16k.wav").exists());

    // retention deleting the audio takes the 16 kHz copies too
    s.delete_meeting_audio(&done_id).unwrap();
    assert!(!done_scratch.exists());
    // deleting the meeting takes its scratch with it
    s.purge_meeting(&failed_id).unwrap();
    assert!(!failed_scratch.exists());
}

/// Media import keeps the user's own file names, so a meeting folder can
/// hold a user's `Board call.16k.wav`. Neither a pipeline start nor the
/// startup sweep may touch it — nor a file that carries one of the
/// pipeline's exact names but isn't in its 16 kHz mono format. Only the
/// pipeline's own leftovers go.
#[test]
fn imported_16k_wav_survives_the_sweep_and_a_pipeline_start() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Storage::open(tmp.path()).unwrap();
    let (_, meeting_id, dir) = recorded_meeting(&s, "Board call");
    let originals = [
        ("Board call.16k.wav", wav_header(16_000, 1)),
        ("x.16k.wav", wav_header(16_000, 1)),
        // an exact pipeline name, but a 44.1 kHz stereo original
        ("track.16k.wav", wav_header(44_100, 2)),
    ];
    for (name, bytes) in &originals {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    // the pipeline's own leftover, for contrast
    std::fs::write(dir.join("mic.16k.wav"), wav_header(16_000, 1)).unwrap();

    fly_storage::scratch::prepare_meeting_scratch(tmp.path(), &meeting_id, Some(&dir)).unwrap();
    for (name, bytes) in &originals {
        assert_eq!(&std::fs::read(dir.join(name)).unwrap(), bytes, "{name}");
    }
    assert!(
        dir.join("mic.16k.wav").exists(),
        "pipeline start deletes nothing"
    );

    s.self_heal_recordings().unwrap();
    for (name, bytes) in &originals {
        assert_eq!(&std::fs::read(dir.join(name)).unwrap(), bytes, "{name}");
    }
    assert!(
        !dir.join("mic.16k.wav").exists(),
        "the real leftover is swept"
    );
    assert!(dir.join("recording.mixed.wav").exists());
}

/// A canonical 44-byte PCM 16-bit WAV header (no samples).
fn wav_header(rate: u32, channels: u16) -> Vec<u8> {
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&36u32.to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    h.extend_from_slice(&(channels * 2).to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&0u32.to_le_bytes());
    h
}

/// "Delete all my data" must reach the scratch root too: it sits outside
/// the data dir by default, where the restart-time wipe never looks.
#[test]
fn erase_all_content_removes_every_meetings_scratch() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Storage::open(tmp.path()).unwrap();
    let (_, meeting_id, _) = recorded_meeting(&s, "Confidential");
    let scratch = s.scratch_dir(&meeting_id);
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("mic.16k.asrpart.json"), b"{\"words\":[]}").unwrap();

    s.erase_all_content().unwrap();
    assert!(!fly_storage::scratch::scratch_root(tmp.path()).exists());
}
