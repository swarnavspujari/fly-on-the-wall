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
    backup::before_open_with(dir, version, ROOMY, &mut |_| {}, &QUICK)
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

/// A stand-in backup in `folder`: a small, real SQLite file at schema
/// version `schema`, which is what retention counts as a complete copy.
fn stand_in(folder: &Path, name: &str, schema: i64) {
    std::fs::create_dir_all(folder).unwrap();
    let conn = Connection::open(folder.join(name)).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE t (x); PRAGMA user_version = {schema};"
    ))
    .unwrap();
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
    let mut announced = Vec::new();
    let db_bytes = std::fs::metadata(dir.path().join("flyonthewall.db"))
        .unwrap()
        .len()
        + std::fs::metadata(dir.path().join("flyonthewall.db-wal"))
            .unwrap()
            .len();

    let outcome = backup::before_open_with(
        dir.path(),
        "2.5.0",
        ROOMY,
        &mut |bytes| announced.push(bytes),
        &QUICK,
    );
    let file = saved(outcome.clone());
    // The startup gate is told once, with how much is about to be copied (the
    // main file and its WAL: the row that only the WAL holds is in the copy).
    assert_eq!(announced, [db_bytes]);
    assert!(db_bytes > 0);

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
        stand_in(
            &folder,
            &format!("flyonthewall-1.0.{i}-{stamp}.db"),
            Storage::SCHEMA_VERSION,
        );
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
        &mut |_| {},
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
        &mut |_| {},
        &QUICK,
    ));
    let dir3 = tempfile::tempdir().unwrap();
    drop(v2_library(dir3.path()));
    saved(backup::before_open_with(
        dir3.path(),
        "2.5.0",
        &|_| None,
        &mut |_| {},
        &QUICK,
    ));
}

#[test]
fn a_copy_left_half_written_by_a_crash_is_cleared_before_the_space_check() {
    // The crashed run's `.partial` can be the very thing that fills the disk,
    // so the free space the folder reports depends on whether it is still
    // there. Checking first would skip this backup too, and every one after.
    let dir = tempfile::tempdir().unwrap();
    let live = v2_library(dir.path());
    let folder = dir.path().join(BACKUP_DIR);
    std::fs::create_dir_all(&folder).unwrap();
    let partial = folder.join("flyonthewall-2.4.0-20260101T000000Z.db.partial");
    std::fs::write(&partial, b"half a copy").unwrap();
    let free = |backups: &Path| {
        let has_partial = std::fs::read_dir(backups)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".partial"));
        Some(if has_partial { 0 } else { u64::MAX })
    };

    let file = saved(backup::before_open_with(
        dir.path(),
        "2.5.0",
        &free,
        &mut |_| {},
        &QUICK,
    ));
    assert!(!partial.exists(), "the leftover is swept");
    assert_eq!(backup_files(dir.path()), [file.name]);
    drop(live);
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
    backup::after_compression_backup(&storage, "2.5.0", &outcome);
    assert!(!backup::compression_backup_due(&storage));

    // A skipped one raises the notice, worded for this step (nothing was
    // upgraded here), and the backup stays owed.
    let low: &dyn Fn(&Path) -> Option<u64> = &|_| Some(10);
    let skipped = backup::before_compression(dir.path(), "2.5.0", low);
    assert!(matches!(skipped, Outcome::NoSpace { .. }));
    storage
        .delete_setting(backup::BEFORE_COMPRESSION_KEY)
        .unwrap();
    backup::after_compression_backup(&storage, "2.5.0", &skipped);
    assert_eq!(
        backup::pending_notice(&storage),
        Some(NoticeKind::CompressionNoSpace)
    );
    assert!(backup::compression_backup_due(&storage));
}

#[test]
fn a_dismissed_notice_stays_dismissed_for_the_rest_of_that_app_version() {
    let dir = tempfile::tempdir().unwrap();
    drop(Storage::open(dir.path()).unwrap());
    let storage = Storage::open(dir.path()).unwrap();
    let low = Outcome::NoSpace { free: 1, needed: 2 };

    // The startup step tells the user, who dismisses it.
    backup::after_open(&storage, "2.5.0", &low);
    assert_eq!(backup::pending_notice(&storage), Some(NoticeKind::NoSpace));
    backup::dismiss_notice(&storage);
    assert_eq!(backup::pending_notice(&storage), None);

    // The skipped backup is owed again every launch: the compression job's
    // attempt (and a startup step that fails again) must not tell them again.
    for _ in 0..3 {
        backup::after_compression_backup(&storage, "2.5.0", &low);
        backup::after_compression_backup(&storage, "2.5.0", &Outcome::Failed("x".into()));
        backup::after_open(&storage, "2.5.0", &Outcome::Failed("x".into()));
        assert_eq!(backup::pending_notice(&storage), None);
    }

    // A new app version is a new upgrade: it tells again, and the notice
    // names the first step that failed (a later step keeps quiet behind it).
    backup::after_open(&storage, "2.5.1", &Outcome::Failed("x".into()));
    assert_eq!(backup::pending_notice(&storage), Some(NoticeKind::Failed));
    backup::after_compression_backup(&storage, "2.5.1", &low);
    assert_eq!(backup::pending_notice(&storage), Some(NoticeKind::Failed));
    backup::dismiss_notice(&storage);

    // ...and when the compression job is the one that fails first, its own
    // wording is used.
    backup::after_compression_backup(&storage, "2.5.2", &Outcome::Failed("x".into()));
    assert_eq!(
        backup::pending_notice(&storage),
        Some(NoticeKind::CompressionFailed)
    );
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
        Some("balanced"),
        "the change made after the backup is gone"
    );
    assert_eq!(
        restored.get_setting(backup::LAST_RUN_VERSION_KEY).unwrap(),
        None,
        "the restored library is judged on its own content"
    );
    drop(restored);

    // The rows the backup held are back in the restored library (known
    // content, not just a file count): the one the main file held, and the
    // one only the WAL held when the backup was taken. The v2 copy was
    // upgraded on open, as an update would.
    let live = read_only(&db);
    assert_eq!(titles(&live), ["Weekly sync", "Only in the WAL"]);
    assert_eq!(user_version(&live), Storage::SCHEMA_VERSION);
    drop(live);

    // The backup itself was not consumed: still the v2 copy, still the only one.
    assert_eq!(
        titles(&read_only(&file.path)),
        ["Weekly sync", "Only in the WAL"]
    );
    assert_eq!(user_version(&read_only(&file.path)), 2);
    assert_eq!(backup::list(dir.path()).len(), 1);
}

#[test]
fn the_copy_from_before_the_schema_change_survives_a_run_of_version_bumps() {
    let dir = tempfile::tempdir().unwrap();
    drop(v2_library(dir.path()));

    // 2.5.0 is the first launch on the new schema: the v2 library is saved
    // first, then upgraded.
    let outcome = before(dir.path(), "2.5.0");
    let pre_migration = saved(outcome.clone());
    drop(open_and_record(dir.path(), "2.5.0", &outcome));

    // Routine updates, more than enough to push it out of a plain newest-3.
    // They run back to back, so on a fast machine several land in the same
    // second of the clock: the order must not depend on that. Every backup is
    // stamped after the one before it (never the same name, never replacing
    // one), and the listing puts the newest first, by the stamp in the name.
    let mut routine = Vec::new();
    let mut last_stamp = pre_migration.taken_at.clone();
    for patch in 1..=6 {
        let version = format!("2.5.{patch}");
        let outcome = before(dir.path(), &version);
        let file = saved(outcome.clone());
        assert!(
            file.taken_at > last_stamp,
            "{} was not stamped after {last_stamp}",
            file.name
        );
        last_stamp = file.taken_at.clone();
        assert_eq!(backup::list(dir.path())[0].name, file.name);
        routine.push(file.name);
        drop(open_and_record(dir.path(), &version, &outcome));
        let left = backup_files(dir.path());
        assert!(
            left.contains(&pre_migration.name),
            "the pre-migration copy was removed at {version}: {left:?}"
        );
        assert!(left.len() <= KEEP_BACKUPS + 1, "{left:?}");
    }

    // The newest three plus the last copy from before the schema change.
    let left = backup_files(dir.path());
    assert_eq!(left.len(), KEEP_BACKUPS + 1, "{left:?}");
    for newest in routine.iter().rev().take(KEEP_BACKUPS) {
        assert!(left.contains(newest), "{newest} missing: {left:?}");
    }

    // And it is the real thing: the library exactly as v2 left it.
    let copy = read_only(&pre_migration.path);
    assert_eq!(user_version(&copy), 2);
    assert_eq!(titles(&copy), ["Weekly sync", "Only in the WAL"]);
}

#[test]
fn the_last_copy_of_each_recent_schema_is_kept_and_older_ones_are_not() {
    let dir = tempfile::tempdir().unwrap();
    drop(Storage::open(dir.path()).unwrap());
    let folder = dir.path().join(BACKUP_DIR);
    // The history of an old install: copies at schemas 0, 1, 2 (two of them)
    // and 3 (four), oldest first. Names carry the schema so the result reads
    // plainly.
    for (stamp, schema) in [
        ("20180101T000000Z", 0),
        ("20190101T000000Z", 1),
        ("20200101T000000Z", 2),
        ("20200201T000000Z", 2),
        ("20210101T000000Z", 3),
        ("20210201T000000Z", 3),
        ("20210301T000000Z", 3),
        ("20210401T000000Z", 3),
    ] {
        stand_in(
            &folder,
            &format!("flyonthewall-s{schema}-{stamp}.db"),
            schema,
        );
    }
    saved(before(dir.path(), "9.9.9"));

    let kept: Vec<String> = backup::list(dir.path())
        .into_iter()
        .map(|f| f.name)
        .collect();
    // Newest first: the new copy and the two before it, then the newest copy
    // of each of the next two schemas down. Schema 0 is the fourth-latest
    // schema: gone, with the older schema-2 copy and the other schema-3 ones.
    assert_eq!(kept.len(), 5, "{kept:?}");
    assert!(kept[0].starts_with("flyonthewall-schema3-"), "{kept:?}");
    assert_eq!(kept[1], "flyonthewall-s3-20210401T000000Z.db");
    assert_eq!(kept[2], "flyonthewall-s3-20210301T000000Z.db");
    assert_eq!(kept[3], "flyonthewall-s2-20200201T000000Z.db");
    assert_eq!(kept[4], "flyonthewall-s1-20190101T000000Z.db");
}

#[test]
fn a_damaged_backup_does_not_take_a_slot_and_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    drop(Storage::open(dir.path()).unwrap());
    let folder = dir.path().join(BACKUP_DIR);
    let schema = Storage::SCHEMA_VERSION;
    for name in [
        "flyonthewall-1.0.0-20200101T000000Z.db",
        "flyonthewall-1.0.1-20200102T000000Z.db",
        "flyonthewall-1.0.2-20200103T000000Z.db",
    ] {
        stand_in(&folder, name, schema);
    }
    // The newest of the older ones is a stub (a full disk, a scanner): counted
    // by name alone it would push a good copy out.
    let stub = "flyonthewall-1.0.3-20200104T000000Z.db";
    std::fs::write(folder.join(stub), b"half a copy").unwrap();
    // ...and a good copy cut short by one page is just as unusable.
    let cut = "flyonthewall-1.0.4-20200105T000000Z.db";
    stand_in(&folder, cut, schema);
    let len = std::fs::metadata(folder.join(cut)).unwrap().len();
    assert!(len >= 8192, "needs at least two pages: {len}");
    std::fs::OpenOptions::new()
        .write(true)
        .open(folder.join(cut))
        .unwrap()
        .set_len(len - 4096)
        .unwrap();

    let new = saved(before(dir.path(), "9.9.9"));

    // Complete copies: the new one and the two before it. The oldest good one
    // is the only casualty; the damaged files take no slot and are left for
    // the user to look at.
    let left = backup_files(dir.path());
    for kept in [
        new.name.as_str(),
        "flyonthewall-1.0.2-20200103T000000Z.db",
        "flyonthewall-1.0.1-20200102T000000Z.db",
        stub,
        cut,
    ] {
        assert!(left.iter().any(|n| n == kept), "{kept} missing: {left:?}");
    }
    assert!(
        !left
            .iter()
            .any(|n| n == "flyonthewall-1.0.0-20200101T000000Z.db"),
        "{left:?}"
    );
    assert_eq!(left.len(), 5, "{left:?}");
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
