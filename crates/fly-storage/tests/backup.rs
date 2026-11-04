//! Automatic pre-upgrade backup (`fly_storage::backup`) against libraries
//! built the way a v2 build leaves them. Everything lives in temp dirs: no
//! real user data, and the app's data dir is never touched.

use std::path::Path;
use std::time::Duration;

use fly_storage::backup::{
    self, BackupFile, NoticeKind, Outcome, Patience, BACKUP_DIR, KEEP_BACKUPS,
};
use fly_storage::{OpenRetry, Storage};
use rusqlite::Connection;

/// A test never waits on a library that is genuinely broken.
const QUICK: Patience = Patience {
    attempts: 2,
    backoff: Duration::from_millis(1),
    busy: Duration::from_millis(50),
};

const ROOMY: &dyn Fn(&Path) -> Option<u64> = &|_| Some(u64::MAX);

/// The v2 tables a launch touches, as v2 builds ship them (the full DDL is
/// in `migration_v3.rs`; this keeps the tables the backup tests read).
const V2_DDL: &str = r#"
CREATE TABLE folders (
    id TEXT PRIMARY KEY, name TEXT NOT NULL,
    parent_id TEXT REFERENCES folders(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL, keep_local INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE notes (
    id TEXT PRIMARY KEY, title TEXT NOT NULL,
    folder_id TEXT REFERENCES folders(id) ON DELETE SET NULL,
    meeting_id TEXT, scratchpad TEXT NOT NULL DEFAULT '',
    blocks_json TEXT NOT NULL DEFAULT '[]',
    attachments_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, disk_path TEXT
);
CREATE TABLE meetings (
    id TEXT PRIMARY KEY, title TEXT NOT NULL, note_id TEXT NOT NULL,
    attendees_json TEXT NOT NULL DEFAULT '[]',
    attendees_confirmed INTEGER NOT NULL DEFAULT 0,
    started_at TEXT NOT NULL, ended_at TEXT, recording_json TEXT,
    keep_local INTEGER NOT NULL DEFAULT 0,
    participants_notified INTEGER NOT NULL DEFAULT 0,
    audio_deleted_at TEXT
);
CREATE TABLE transcripts (
    meeting_id TEXT PRIMARY KEY, json TEXT NOT NULL,
    cleaned_json TEXT, speaker_undo_json TEXT
);
CREATE TABLE templates (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, system_prompt TEXT NOT NULL,
    structure_hint TEXT NOT NULL, built_in INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE transcription_jobs (
    meeting_id TEXT PRIMARY KEY, status TEXT NOT NULL DEFAULT 'queued',
    attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE meeting_items (
    id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL, kind TEXT NOT NULL,
    text TEXT NOT NULL, owner TEXT, status TEXT, speaker_key TEXT,
    segment_ids_json TEXT NOT NULL DEFAULT '[]', created_at TEXT NOT NULL,
    extracted_by TEXT NOT NULL DEFAULT ''
);
CREATE VIRTUAL TABLE notes_fts USING fts5(note_id UNINDEXED, title, body);
CREATE VIRTUAL TABLE transcripts_fts USING fts5(meeting_id UNINDEXED, body);
CREATE TABLE embedding_chunks (
    id INTEGER PRIMARY KEY, owner_kind TEXT NOT NULL, owner_id TEXT NOT NULL,
    chunk_index INTEGER NOT NULL, title TEXT NOT NULL DEFAULT '',
    text TEXT NOT NULL, start_ms INTEGER, embedding BLOB, model TEXT,
    UNIQUE(owner_kind, owner_id, chunk_index)
);
PRAGMA user_version = 2;
"#;

/// A synthetic v2 library in `dir` (WAL mode like the app's), returned with
/// its connection STILL OPEN: the second note is committed only in the WAL,
/// so a backup that copied the main file alone would miss it.
fn v2_library(dir: &Path) -> Connection {
    std::fs::create_dir_all(dir).unwrap();
    let conn = Connection::open(dir.join("flyonthewall.db")).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch(V2_DDL).unwrap();
    conn.execute_batch(
        "INSERT INTO notes (id, title, created_at, updated_at)
         VALUES ('n1', 'Weekly sync', '2026-01-01', '2026-01-01');
         INSERT INTO settings (key, value) VALUES ('asr.tier', 'balanced');",
    )
    .unwrap();
    // Everything above is folded into the main file; what follows is not.
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
        .unwrap();
    conn.execute(
        "INSERT INTO notes (id, title, created_at, updated_at)
         VALUES ('n2', 'Only in the WAL', '2026-01-02', '2026-01-02')",
        [],
    )
    .unwrap();
    conn
}

fn before(dir: &Path, version: &str) -> Outcome {
    backup::before_open_with(dir, version, ROOMY, &mut || {}, &QUICK)
}

/// Open the library the way the app does and record the backup step.
fn open_and_record(dir: &Path, version: &str, outcome: &Outcome) -> Storage {
    let storage = Storage::open(dir).unwrap();
    backup::after_open(&storage, version, outcome);
    storage
}

fn saved(outcome: Outcome) -> BackupFile {
    match outcome {
        Outcome::Saved(f) => f,
        other => panic!("expected a saved backup, got {other:?}"),
    }
}

fn read_only(path: &Path) -> Connection {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn user_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn titles(conn: &Connection) -> Vec<String> {
    let mut stmt = conn.prepare("SELECT title FROM notes ORDER BY id").unwrap();
    let rows = stmt.query_map([], |r| r.get(0)).unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn backup_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir.join(BACKUP_DIR))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn upgrading_a_v2_library_makes_exactly_one_verified_backup() {
    let dir = tempfile::tempdir().unwrap();
    let live = v2_library(dir.path());
    let mut announced = 0;

    let outcome =
        backup::before_open_with(dir.path(), "2.5.0", ROOMY, &mut || announced += 1, &QUICK);
    let file = saved(outcome.clone());
    assert_eq!(announced, 1, "the startup gate is told once");

    // One file, in backups/, with the documented name: this build recorded
    // no version, so the label is the schema it found.
    assert_eq!(backup_files(dir.path()), std::slice::from_ref(&file.name));
    assert!(
        file.name.starts_with("flyonthewall-schema2-"),
        "{}",
        file.name
    );
    assert!(file.name.ends_with(".db"));
    assert_eq!(file.path, dir.path().join(BACKUP_DIR).join(&file.name));
    assert!(file.bytes > 0);

    // It is a whole, consistent v2 library: the WAL-only row is in it, it
    // passes integrity_check, and it is NOT in WAL mode with side files.
    let copy = read_only(&file.path);
    assert_eq!(user_version(&copy), 2);
    assert_eq!(titles(&copy), ["Weekly sync", "Only in the WAL"]);
    let check: String = copy
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    drop(copy);

    // The live library was only read: still v2 until the open migrates it.
    assert_eq!(user_version(&live), 2);
    drop(live);
    let storage = open_and_record(dir.path(), "2.5.0", &outcome);
    assert_eq!(
        storage.get_setting("asr.tier").unwrap().as_deref(),
        Some("balanced")
    );
    drop(storage);
    assert_eq!(
        user_version(&read_only(&dir.path().join("flyonthewall.db"))),
        Storage::SCHEMA_VERSION
    );
    // ...while the backup stays at the version it saved.
    assert_eq!(user_version(&read_only(&file.path)), 2);
    assert_eq!(backup_files(dir.path()).len(), 1);
}

#[test]
fn a_second_launch_on_the_same_version_makes_none() {
    let dir = tempfile::tempdir().unwrap();
    drop(v2_library(dir.path()));
    let first = before(dir.path(), "2.5.0");
    let file = saved(first.clone());
    drop(open_and_record(dir.path(), "2.5.0", &first));

    for _ in 0..3 {
        let again = before(dir.path(), "2.5.0");
        assert_eq!(again, Outcome::NotNeeded);
        drop(open_and_record(dir.path(), "2.5.0", &again));
    }
    assert_eq!(backup_files(dir.path()), std::slice::from_ref(&file.name));

    // A new app version is a new reason, labelled with the one it replaces.
    let next = saved(before(dir.path(), "2.5.1"));
    assert!(next.label == "2.5.0", "{}", next.label);
    assert_eq!(backup_files(dir.path()).len(), 2);
}

#[test]
fn a_fresh_install_has_nothing_to_back_up() {
    let dir = tempfile::tempdir().unwrap();
    // No database at all...
    assert_eq!(before(dir.path(), "2.5.0"), Outcome::NewLibrary);
    assert!(!dir.path().join(BACKUP_DIR).exists());
    // ...nor an empty file the first open just created.
    let storage = open_and_record(dir.path(), "2.5.0", &Outcome::NewLibrary);
    drop(storage);
    assert_eq!(before(dir.path(), "2.5.0"), Outcome::NotNeeded);
    assert!(!dir.path().join(BACKUP_DIR).exists());
    // The next version does back the (new, small) library up.
    saved(before(dir.path(), "2.6.0"));
}

#[test]
fn a_schema_step_backs_up_even_when_the_version_did_not_change() {
    let dir = tempfile::tempdir().unwrap();
    drop(v2_library(dir.path()));
    // The recorded version already says 2.5.0 (a downgrade to a build that
    // doesn't record versions, then back): the file itself says v2.
    {
        let c = Connection::open(dir.path().join("flyonthewall.db")).unwrap();
        c.execute(
            "INSERT INTO settings (key, value) VALUES ('app.last_run_version', '2.5.0')",
            [],
        )
        .unwrap();
    }
    let file = saved(before(dir.path(), "2.5.0"));
    assert_eq!(file.label, "schema2");
}

#[test]
fn the_recorded_version_labels_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    drop(Storage::open(dir.path()).unwrap());
    {
        let c = Connection::open(dir.path().join("flyonthewall.db")).unwrap();
        c.execute(
            "INSERT INTO settings (key, value) VALUES ('app.last_run_version', '2.5.0-beta.1')",
            [],
        )
        .unwrap();
    }
    let file = saved(before(dir.path(), "2.5.0"));
    assert_eq!(file.label, "2.5.0_beta.1");
    assert!(file.name.contains("-2.5.0_beta.1-"));
}

#[test]
fn retention_keeps_the_newest_three_across_launches() {
    let dir = tempfile::tempdir().unwrap();
    drop(Storage::open(dir.path()).unwrap());
    let mut names = Vec::new();
    // Eight upgrades in quick succession: the clock barely moves, which the
    // backup names must survive (labels differ, stamps may not).
    for minor in 0..8 {
        let version = format!("3.{minor}.0");
        let outcome = before(dir.path(), &version);
        let file = saved(outcome.clone());
        names.push(file.name);
        drop(open_and_record(dir.path(), &version, &outcome));
        assert!(backup_files(dir.path()).len() <= KEEP_BACKUPS);
    }
    let left = backup_files(dir.path());
    assert_eq!(left.len(), KEEP_BACKUPS, "{left:?}");
    // The newest one (just made) always survives.
    assert!(left.contains(names.last().unwrap()), "{left:?}");
    assert_eq!(backup::list(dir.path()).len(), KEEP_BACKUPS);
}

#[test]
fn retention_only_touches_exact_backup_names() {
    let dir = tempfile::tempdir().unwrap();
    drop(Storage::open(dir.path()).unwrap());
    let folder = dir.path().join(BACKUP_DIR);
    std::fs::create_dir_all(&folder).unwrap();
    let bystanders = [
        "notes.txt",
        "flyonthewall.db",
        "flyonthewall-2.0.0.db",
        "flyonthewall-2.0.0-20200101T000000Z.db.bak",
        "My flyonthewall-2.0.0-20200101T000000Z.db",
    ];
    for name in bystanders {
        std::fs::write(folder.join(name), b"keep me").unwrap();
    }
    // a folder with a backup's name is not a backup
    let lookalike = "flyonthewall-0.9.0-20190101T000000Z.db";
    std::fs::create_dir(folder.join(lookalike)).unwrap();
    // Four older exact-name backups: only the newest two stay beside the new one.
    for (i, stamp) in [
        "20200101T000000Z",
        "20200102T000000Z",
        "20200103T000000Z",
        "20200104T000000Z",
    ]
    .iter()
    .enumerate()
    {
        std::fs::write(
            folder.join(format!("flyonthewall-1.0.{i}-{stamp}.db")),
            b"old",
        )
        .unwrap();
    }

    saved(before(dir.path(), "9.9.9"));
    // (the fresh library has no recorded version, so the label is a schema)
    let left = backup_files(dir.path());
    for name in bystanders {
        assert!(
            left.contains(&name.to_string()),
            "{name} was removed: {left:?}"
        );
    }
    assert!(
        left.contains(&lookalike.to_string()),
        "the folder was removed: {left:?}"
    );
    let exact: Vec<_> = backup::list(dir.path())
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(exact.len(), KEEP_BACKUPS, "{exact:?}");
    assert!(exact.iter().any(|n| n.contains("-1.0.3-")));
    assert!(exact.iter().any(|n| n.contains("-1.0.2-")));
    for gone in [
        "flyonthewall-1.0.1-20200102T000000Z.db",
        "flyonthewall-1.0.0-20200101T000000Z.db",
    ] {
        assert!(
            !left.contains(&gone.to_string()),
            "{gone} should be deleted: {left:?}"
        );
    }
}

#[test]
fn too_little_disk_space_skips_the_backup_with_a_notice_and_the_open_still_runs() {
    let dir = tempfile::tempdir().unwrap();
    let live = v2_library(dir.path());
    let db_bytes = std::fs::metadata(dir.path().join("flyonthewall.db"))
        .unwrap()
        .len()
        + std::fs::metadata(dir.path().join("flyonthewall.db-wal"))
            .unwrap()
            .len();
    let needed = backup::needed_free_bytes(db_bytes);
    assert_eq!(needed, backup::MIN_FREE_BYTES, "a test library is tiny");

    // One byte short: skipped.
    let outcome = backup::before_open_with(
        dir.path(),
        "2.5.0",
        &|_| Some(needed - 1),
        &mut || {},
        &QUICK,
    );
    assert_eq!(
        outcome,
        Outcome::NoSpace {
            free: needed - 1,
            needed
        }
    );
    assert!(backup::list(dir.path()).is_empty());
    assert!(
        backup_files(dir.path()).is_empty(),
        "no partial file either"
    );

    // Startup carries on: the library opens and upgrades, and the one-time
    // notice is recorded.
    drop(live);
    let storage = open_and_record(dir.path(), "2.5.0", &outcome);
    assert_eq!(backup::pending_notice(&storage), Some(NoticeKind::NoSpace));
    assert!(
        backup::compression_backup_due(&storage),
        "compression still owes one"
    );
    backup::dismiss_notice(&storage);
    assert_eq!(backup::pending_notice(&storage), None);
    drop(storage);

    // Exactly enough is enough, and an unknown amount doesn't block.
    let dir2 = tempfile::tempdir().unwrap();
    drop(v2_library(dir2.path()));
    saved(backup::before_open_with(
        dir2.path(),
        "2.5.0",
        &|_| Some(needed),
        &mut || {},
        &QUICK,
    ));
    let dir3 = tempfile::tempdir().unwrap();
    drop(v2_library(dir3.path()));
    saved(backup::before_open_with(
        dir3.path(),
        "2.5.0",
        &|_| None,
        &mut || {},
        &QUICK,
    ));
}

#[test]
fn a_backup_that_cannot_be_written_raises_the_notice_and_the_open_still_runs() {
    let dir = tempfile::tempdir().unwrap();
    drop(v2_library(dir.path()));
    // A FILE where the folder must go.
    std::fs::write(dir.path().join(BACKUP_DIR), b"in the way").unwrap();
    let outcome = before(dir.path(), "2.5.0");
    assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");

    let storage = open_and_record(dir.path(), "2.5.0", &outcome);
    assert_eq!(backup::pending_notice(&storage), Some(NoticeKind::Failed));
    // The upgrade ran regardless.
    drop(storage);
    assert_eq!(
        user_version(&read_only(&dir.path().join("flyonthewall.db"))),
        Storage::SCHEMA_VERSION
    );
}

#[test]
fn a_corrupt_library_does_not_crash_the_backup_step_or_skip_the_open_paths() {
    let dir = tempfile::tempdir().unwrap();
    // Not a database at all.
    std::fs::write(dir.path().join("flyonthewall.db"), b"this is not sqlite").unwrap();
    let outcome = before(dir.path(), "2.5.0");
    assert!(matches!(outcome, Outcome::Unchecked(_)), "{outcome:?}");
    assert!(backup::list(dir.path()).is_empty());
    // The open's own retry and error path is what reports it.
    let retry = OpenRetry {
        attempts: 2,
        backoff: Duration::from_millis(1),
    };
    assert!(Storage::open_with_retry(dir.path(), retry).is_err());

    // A real library with its schema pages scribbled over (header intact).
    let dir = tempfile::tempdir().unwrap();
    drop(v2_library(dir.path()));
    let db = dir.path().join("flyonthewall.db");
    let _ = std::fs::remove_file(dir.path().join("flyonthewall.db-wal"));
    let _ = std::fs::remove_file(dir.path().join("flyonthewall.db-shm"));
    let mut bytes = std::fs::read(&db).unwrap();
    for b in bytes.iter_mut().skip(100) {
        *b = 0xA5;
    }
    std::fs::write(&db, bytes).unwrap();
    let outcome = before(dir.path(), "2.5.0");
    assert!(
        matches!(outcome, Outcome::Unchecked(_) | Outcome::Failed(_)),
        "{outcome:?}"
    );
    assert!(
        backup::list(dir.path()).is_empty(),
        "a bad copy never counts"
    );
    assert!(
        backup_files(dir.path()).is_empty(),
        "and leaves nothing behind: {:?}",
        backup_files(dir.path())
    );
    assert!(Storage::open_with_retry(dir.path(), retry).is_err());
}

#[test]
fn a_locked_library_does_not_crash_the_backup_step() {
    let dir = tempfile::tempdir().unwrap();
    // Rollback-journal mode, so an exclusive lock really blocks readers.
    let locker = {
        let c = Connection::open(dir.path().join("flyonthewall.db")).unwrap();
        c.execute_batch(V2_DDL).unwrap();
        c.execute_batch("BEGIN EXCLUSIVE").unwrap();
        c
    };
    let outcome = before(dir.path(), "2.5.0");
    assert!(
        matches!(outcome, Outcome::Unchecked(_) | Outcome::Failed(_)),
        "{outcome:?}"
    );
    assert!(backup::list(dir.path()).is_empty());
    // Once the lock is released, the next launch's check works again.
    locker.execute_batch("COMMIT").unwrap();
    saved(before(dir.path(), "2.5.0"));
}

#[test]
fn an_unreadable_check_only_raises_the_notice_when_the_version_changed() {
    let dir = tempfile::tempdir().unwrap();
    drop(Storage::open(dir.path()).unwrap());
    let unchecked = Outcome::Unchecked("database is locked".into());

    // Same version as recorded: the failed read cost nothing (no upgrade).
    let storage = open_and_record(dir.path(), "2.5.0", &Outcome::NotNeeded);
    backup::after_open(&storage, "2.5.0", &unchecked);
    assert_eq!(backup::pending_notice(&storage), None);
    // A new version with no backup is something to tell the user.
    backup::after_open(&storage, "2.5.1", &unchecked);
    assert_eq!(backup::pending_notice(&storage), Some(NoticeKind::Failed));
}

#[test]
fn the_compression_backup_is_owed_once_and_not_after_a_startup_backup() {
    // After the upgrade's own backup, compression needs none.
    let dir = tempfile::tempdir().unwrap();
    drop(v2_library(dir.path()));
    let outcome = before(dir.path(), "2.5.0");
    let storage = open_and_record(dir.path(), "2.5.0", &outcome);
    assert!(!backup::compression_backup_due(&storage));
    drop(storage);

    // A library the startup step never covered (the backup was skipped for
    // space, or it predates this feature) is backed up before compression.
    let dir = tempfile::tempdir().unwrap();
    drop(Storage::open(dir.path()).unwrap());
    let storage = open_and_record(dir.path(), "2.5.0", &Outcome::NotNeeded);
    assert!(backup::compression_backup_due(&storage));
    let outcome = backup::before_compression(dir.path(), "2.5.0", ROOMY);
    let file = saved(outcome.clone());
    assert_eq!(file.label, "2.5.0");
    backup::after_compression_backup(&storage, &outcome);
    assert!(!backup::compression_backup_due(&storage));

    // A failed one raises the notice once, and not over a pending notice.
    let low: &dyn Fn(&Path) -> Option<u64> = &|_| Some(10);
    let skipped = backup::before_compression(dir.path(), "2.5.0", low);
    assert!(matches!(skipped, Outcome::NoSpace { .. }));
    storage
        .delete_setting(backup::BEFORE_COMPRESSION_KEY)
        .unwrap();
    backup::after_compression_backup(&storage, &skipped);
    assert_eq!(backup::pending_notice(&storage), Some(NoticeKind::NoSpace));
    assert!(backup::compression_backup_due(&storage));
}

#[test]
fn a_backup_restores_by_the_documented_steps() {
    let dir = tempfile::tempdir().unwrap();
    drop(v2_library(dir.path()));
    let outcome = before(dir.path(), "2.5.0");
    let file = saved(outcome.clone());
    // The upgraded library gets a change the backup doesn't have.
    let storage = open_and_record(dir.path(), "2.5.0", &outcome);
    storage.set_setting("asr.tier", "best").unwrap();
    drop(storage);

    // Close the app (it is closed), replace flyonthewall.db with the copy,
    // and remove the -wal and -shm files.
    let db = dir.path().join("flyonthewall.db");
    std::fs::copy(&file.path, &db).unwrap();
    for side in ["flyonthewall.db-wal", "flyonthewall.db-shm"] {
        let _ = std::fs::remove_file(dir.path().join(side));
    }

    let restored = Storage::open(dir.path()).unwrap();
    assert_eq!(
        restored.get_setting("asr.tier").unwrap().as_deref(),
        Some("balanced")
    );
    assert_eq!(
        restored.get_setting(backup::LAST_RUN_VERSION_KEY).unwrap(),
        None,
        "the restored library is judged on its own content"
    );
    // Both notes came back, and the backup itself was not consumed.
    let conn = read_only(&file.path);
    assert_eq!(titles(&conn), ["Weekly sync", "Only in the WAL"]);
    // The next launch backs the restored (v2) library up again before
    // upgrading it.
    drop(restored);
    assert!(backup::list(dir.path()).len() == 1);
}

#[test]
fn backups_are_never_opened_as_a_library() {
    let dir = tempfile::tempdir().unwrap();
    drop(Storage::open(dir.path()).unwrap());
    // Poisoned backups: if anything read these as the library, it would fail
    // or show their content.
    let folder = dir.path().join(BACKUP_DIR);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("flyonthewall-2.4.0-20260101T000000Z.db"),
        b"not a db",
    )
    .unwrap();

    // The MCP server's open (open_existing) reads flyonthewall.db only.
    let s = Storage::open_existing(dir.path()).unwrap();
    assert_eq!(s.data_dir(), dir.path());
    drop(s);
    // Pointed at the backups folder itself, neither open path works or
    // creates a library there.
    let err = Storage::open_existing(&folder).err().unwrap();
    assert!(err.to_string().contains("backups"), "{err}");
    let err = Storage::open(&folder).err().unwrap();
    assert!(err.to_string().contains("backups"), "{err}");
    assert_eq!(
        backup_files(dir.path()),
        ["flyonthewall-2.4.0-20260101T000000Z.db"],
        "nothing was created next to the backups"
    );
}

#[test]
fn the_backups_folder_is_invisible_to_self_heal_and_the_v2_upgrade() {
    // v2 upgrade: its orphan sweep parks recordings/ and notes/ strays under
    // _unlinked. A backups folder beside them is none of its business.
    let dir = tempfile::tempdir().unwrap();
    drop(v2_library(dir.path()));
    {
        let c = Connection::open(dir.path().join("flyonthewall.db")).unwrap();
        c.execute_batch("PRAGMA user_version = 1").unwrap();
    }
    let folder = dir.path().join(BACKUP_DIR);
    // Lookalikes of what the portability code scans: a meeting folder with a
    // manifest and a note mirror, both inside backups/.
    let decoy = folder.join("2026-01-01 Decoy meeting");
    std::fs::create_dir_all(&decoy).unwrap();
    std::fs::write(decoy.join("recording.manifest.json"), br#"{"version":3}"#).unwrap();
    std::fs::write(folder.join("Decoy note.md"), b"# decoy").unwrap();
    std::fs::write(folder.join("flyonthewall-2.4.0-20260101T000000Z.db"), b"x").unwrap();
    let before_files = backup_files(dir.path());

    let storage = Storage::open(dir.path()).unwrap();
    // Self-heal and the portability backfill (what startup runs).
    let report = storage.self_heal_recordings().unwrap();
    assert!(report.is_empty(), "{report:?}");
    assert!(report.refreshed.is_empty(), "{report:?}");
    drop(storage);

    assert_eq!(
        backup_files(dir.path()),
        before_files,
        "backups/ was not changed"
    );
    assert!(decoy.join("recording.manifest.json").is_file());
    assert!(!dir
        .path()
        .join("recordings")
        .join("_unlinked")
        .join("2026-01-01 Decoy meeting")
        .exists());
    assert!(!dir
        .path()
        .join("notes")
        .join("_unlinked")
        .join("Decoy note.md")
        .exists());
}

#[test]
fn a_library_left_by_a_crash_is_backed_up_with_its_wal() {
    let dir = tempfile::tempdir().unwrap();
    let live = v2_library(dir.path());
    // The files a crash leaves: the main file and an uncheckpointed WAL, but
    // no shared-memory index (and nobody holding the library open).
    let crashed = tempfile::tempdir().unwrap();
    for name in ["flyonthewall.db", "flyonthewall.db-wal"] {
        std::fs::copy(dir.path().join(name), crashed.path().join(name)).unwrap();
    }
    drop(live);

    let file = saved(before(crashed.path(), "2.5.0"));
    let copy = read_only(&file.path);
    assert_eq!(titles(&copy), ["Weekly sync", "Only in the WAL"]);
    assert_eq!(user_version(&copy), 2);
}
