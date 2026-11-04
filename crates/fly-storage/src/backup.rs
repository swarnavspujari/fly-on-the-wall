//! Automatic backup of the library database before an upgrade changes it.
//!
//! Opening a library can rewrite it (the schema steps in `migrations.rs`),
//! and the recording compression job rewrites rows and manifests after it
//! converts audio. Neither is meant to lose anything, but a user's only copy
//! of their notes deserves a way back. So, once per app-version change (and
//! whenever a schema step is about to run, and once before the compression
//! job's first pass on an install that never had one), the database is
//! copied to `<data dir>/backups/flyonthewall-<label>-<timestamp>.db`:
//!
//! * `<label>` is what the library was: the app version that last opened it,
//!   or `schemaN` when no version was recorded (builds before this feature).
//! * The copy is `VACUUM INTO` a temp name through a separate read-only
//!   connection: a consistent snapshot with the WAL folded in, without
//!   blocking the app's own writer, and without the pages of deleted text
//!   (`secure_delete` is on for the live file, and a vacuumed copy has no
//!   free pages at all). The temp file is synced, checked with
//!   `PRAGMA quick_check` and its `user_version`, then renamed into place.
//! * Only the newest [`KEEP_BACKUPS`] are kept, and only files whose names
//!   match the exact pattern are ever listed or deleted.
//! * Recordings and attachments are NOT backed up: audio compression is
//!   lossless and verified sample for sample, and a copy would be tens of GB.
//! * Too little free disk space ([`needed_free_bytes`]) or any failure skips
//!   the backup and tells the user once ([`NoticeKind`]); it never stops the
//!   library from opening.
//!
//! Nothing in here is on the hot path of a normal launch: [`before_open`]
//! reads two values through a read-only connection and returns.
//!
//! The folder holds copies of everything the library holds, so "Delete all my
//! data" removes it with the content folders, and every scan of the data dir
//! (self-heal, the portability backfill, the v2 orphan sweep) looks only
//! inside `recordings/` and `notes/`, never here. A backup is never opened by
//! the app or by the MCP server: both open `flyonthewall.db` in a data dir,
//! and refuse a dir that holds backup files ([`refuse_backup_dir`]).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::{migrations, rename_with_retry, Result, Storage, StorageError};

/// Folder under the data dir.
pub const BACKUP_DIR: &str = "backups";
/// How many backups are kept; older ones are deleted after a new one lands.
pub const KEEP_BACKUPS: usize = 3;
/// Floor of the free-space rule: `max(2 × database size, this)`.
pub const MIN_FREE_BYTES: u64 = 200 * 1024 * 1024;

/// The app version that last opened this library (a settings row, so it
/// travels with the database and a restored backup is judged on its own
/// content).
pub const LAST_RUN_VERSION_KEY: &str = "app.last_run_version";
/// `done` once a verified backup exists from before compression could have
/// touched the library (the startup backup counts).
pub const BEFORE_COMPRESSION_KEY: &str = "backup.before_compression";
/// `space` / `failed` while the one-time notice is due (see [`NoticeKind`]).
pub const NOTICE_KEY: &str = "backup.notice";

const NAME_PREFIX: &str = "flyonthewall-";
const NAME_SUFFIX: &str = ".db";
const PARTIAL_SUFFIX: &str = ".partial";

/// One saved backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFile {
    pub path: PathBuf,
    pub name: String,
    /// What the library was when it was copied: an app version (`2.4.0`) or
    /// `schemaN`.
    pub label: String,
    /// UTC, `YYYYMMDDTHHMMSSZ`.
    pub taken_at: String,
    pub bytes: u64,
}

/// What a launch's backup step did (see [`before_open`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No library yet (fresh install, or an empty file): nothing to protect.
    NewLibrary,
    /// Same version and schema as last time: nothing changed, nothing to
    /// back up.
    NotNeeded,
    /// A verified backup was saved.
    Saved(BackupFile),
    /// Skipped: less free disk space than [`needed_free_bytes`].
    NoSpace { free: u64, needed: u64 },
    /// Tried and failed (the reason is for the log, not the user).
    Failed(String),
    /// The library couldn't be read to tell whether a backup was due (the
    /// open that follows has its own retry and self-heal paths).
    Unchecked(String),
}

/// Why the one-time notice shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// Not enough free disk space for a backup.
    NoSpace,
    /// The backup failed for another reason.
    Failed,
}

impl NoticeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoSpace => "space",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "space" => Some(Self::NoSpace),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// How long a backup step waits on a busy or unreadable library.
#[derive(Debug, Clone, Copy)]
pub struct Patience {
    /// Attempts at the read that decides whether a backup is due.
    pub attempts: u32,
    pub backoff: Duration,
    /// SQLite busy timeout for the reads and the copy.
    pub busy: Duration,
}

impl Patience {
    /// A transient failure (the db-corruption incidents) clears in seconds;
    /// a launch shouldn't wait long on a library that is really broken.
    pub const STARTUP: Self = Self {
        attempts: 3,
        backoff: Duration::from_millis(400),
        busy: Duration::from_secs(5),
    };
}

/// One backup at a time per process: startup, the compression job's own, and
/// "Delete all my data" (which holds it while it empties the folder).
static BACKUP_LOCK: Mutex<()> = Mutex::new(());

pub fn lock() -> MutexGuard<'static, ()> {
    BACKUP_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Free space a backup needs: twice the database (so the copy and the
/// verification have room) or [`MIN_FREE_BYTES`], whichever is more.
pub fn needed_free_bytes(db_bytes: u64) -> u64 {
    db_bytes.saturating_mul(2).max(MIN_FREE_BYTES)
}

// ---------------------------------------------------------------------------
// Names and listing
// ---------------------------------------------------------------------------

fn label_ok(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 40
        && l.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+'))
}

fn stamp_ok(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() == 16
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'T'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15] == b'Z'
}

/// `(label, stamp)` of an exact backup name, else `None`.
fn parse_name(name: &str) -> Option<(&str, &str)> {
    let stem = name.strip_prefix(NAME_PREFIX)?.strip_suffix(NAME_SUFFIX)?;
    let (label, stamp) = stem.rsplit_once('-')?;
    (label_ok(label) && stamp_ok(stamp)).then_some((label, stamp))
}

/// Whether `name` is exactly an unfinished copy: a backup name plus
/// `.partial`.
fn is_partial_name(name: &str) -> bool {
    name.strip_suffix(PARTIAL_SUFFIX)
        .is_some_and(|n| parse_name(n).is_some())
}

/// A version string made safe for a file name (`2.5.0-beta.1` → `2.5.0_beta.1`).
fn sanitize_label(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '+') {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    if s.is_empty() {
        "unknown".into()
    } else {
        s
    }
}

fn backup_name(label: &str, at: DateTime<Utc>) -> String {
    format!(
        "{NAME_PREFIX}{label}-{}{NAME_SUFFIX}",
        at.format("%Y%m%dT%H%M%SZ")
    )
}

fn list_in(dir: &Path) -> Vec<BackupFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<BackupFile> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_owned();
            let (label, taken_at) = parse_name(&name).map(|(l, t)| (l.to_owned(), t.to_owned()))?;
            // A link or a folder with a backup's name is not one of ours.
            let meta = std::fs::symlink_metadata(e.path()).ok()?;
            meta.is_file().then(|| BackupFile {
                path: e.path(),
                label,
                taken_at,
                bytes: meta.len(),
                name,
            })
        })
        .collect();
    // newest first; the stamp is fixed-width, so a string compare is a time
    // compare
    out.sort_by(|a, b| {
        b.taken_at
            .cmp(&a.taken_at)
            .then_with(|| b.name.cmp(&a.name))
    });
    out
}

/// The backups in `data_dir`, newest first.
pub fn list(data_dir: &Path) -> Vec<BackupFile> {
    list_in(&data_dir.join(BACKUP_DIR))
}

/// Whether `dir` holds backup files (it is a `backups/` folder, not a
/// library).
pub fn is_backup_dir(dir: &Path) -> bool {
    !list_in(dir).is_empty()
}

/// Error for opening a folder of backups as if it were a library: neither
/// the app nor the MCP server may open a backup, and `Storage::open` would
/// otherwise create a fresh library next to them.
pub fn refuse_backup_dir(dir: &Path) -> Result<()> {
    if is_backup_dir(dir) {
        return Err(StorageError::Invalid(
            "this folder holds Fly on the Wall backups, not a library — open the data folder, \
             not its backups folder"
                .into(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The pre-open check
// ---------------------------------------------------------------------------

/// What the read-only look at the library found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Check {
    New,
    UpToDate,
    Due { label: String },
}

fn read_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'settings')",
        [],
        |r| r.get(0),
    )?;
    if !has_table {
        return Ok(None);
    }
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

fn open_read_only(db: &Path, busy: Duration) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(busy)?;
    Ok(conn)
}

/// Read-only: is a backup due for the library in `data_dir`, running
/// `running`? Due when a schema step will run on the next open, or the
/// recorded app version differs (or was never recorded).
fn check(data_dir: &Path, running: &str, busy: Duration) -> Result<Check> {
    let Some(db) = migrations::db_file_to_open(data_dir) else {
        return Ok(Check::New);
    };
    let conn = open_read_only(&db, busy)?;
    let from: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if from == 0 && Storage::has_no_tables(&conn)? {
        return Ok(Check::New);
    }
    let stored = read_setting(&conn, LAST_RUN_VERSION_KEY)?;
    let stored = stored.as_deref();
    let schema_step = from != Storage::SCHEMA_VERSION;
    if !schema_step && stored == Some(running) {
        return Ok(Check::UpToDate);
    }
    // The version the library was last opened by. When it matches the
    // running one but the schema still has a step to run (a downgrade and
    // back), the recorded version is stale: say what the file is.
    let label = match stored {
        Some(v) if v != running => sanitize_label(v),
        _ => format!("schema{from}"),
    };
    Ok(Check::Due { label })
}

fn check_retrying(data_dir: &Path, running: &str, patience: &Patience) -> Result<Check> {
    let attempts = patience.attempts.max(1);
    let mut attempt = 1;
    loop {
        match check(data_dir, running, patience.busy) {
            Err(e) if attempt < attempts => {
                tracing::warn!(attempt, error = %e, "backup check failed; retrying");
                attempt += 1;
                std::thread::sleep(patience.backoff);
            }
            other => return other,
        }
    }
}

/// Whether [`before_open`] will take a backup, so the caller can show it as a
/// step of the startup gate rather than freezing the window. Read-only; an
/// unreadable library says no (the open reports it).
pub fn is_due(data_dir: &Path, running: &str) -> bool {
    matches!(
        check(data_dir, running, Duration::from_millis(500)),
        Ok(Check::Due { .. })
    )
}

/// The backup step of a launch. Call it BEFORE `Storage::open*`, which is
/// what runs the schema steps. `on_backup` is called once, just before the
/// copy starts. Never fails: every problem is an [`Outcome`] for
/// [`after_open`] to record.
pub fn before_open(
    data_dir: &Path,
    running: &str,
    free_space: &dyn Fn(&Path) -> Option<u64>,
    on_backup: &mut dyn FnMut(),
) -> Outcome {
    before_open_with(data_dir, running, free_space, on_backup, &Patience::STARTUP)
}

pub fn before_open_with(
    data_dir: &Path,
    running: &str,
    free_space: &dyn Fn(&Path) -> Option<u64>,
    on_backup: &mut dyn FnMut(),
    patience: &Patience,
) -> Outcome {
    let _guard = lock();
    let label = match check_retrying(data_dir, running, patience) {
        Ok(Check::New) => return Outcome::NewLibrary,
        Ok(Check::UpToDate) => return Outcome::NotNeeded,
        Ok(Check::Due { label }) => label,
        Err(e) => {
            tracing::warn!(error = %e, "couldn't read the library to check for a backup");
            return Outcome::Unchecked(e.to_string());
        }
    };
    let Some(db) = migrations::db_file_to_open(data_dir) else {
        return Outcome::NewLibrary;
    };
    on_backup();
    let outcome = take(&db, data_dir, &label, Utc::now(), free_space, patience.busy);
    log_outcome("before the upgrade", &outcome);
    outcome
}

/// The compression job's pre-first-pass backup: the library as it is now,
/// labelled with the running version. Runs on the job's thread while the app
/// is open, through a read-only connection, so the writer isn't blocked.
pub fn before_compression(
    data_dir: &Path,
    running: &str,
    free_space: &dyn Fn(&Path) -> Option<u64>,
) -> Outcome {
    let _guard = lock();
    let Some(db) = migrations::db_file_to_open(data_dir) else {
        return Outcome::NewLibrary;
    };
    let outcome = take(
        &db,
        data_dir,
        &sanitize_label(running),
        Utc::now(),
        free_space,
        Patience::STARTUP.busy,
    );
    log_outcome("before compression", &outcome);
    outcome
}

fn log_outcome(when: &str, outcome: &Outcome) {
    match outcome {
        Outcome::Saved(f) => {
            tracing::info!(when, name = %f.name, bytes = f.bytes, "saved a library backup")
        }
        Outcome::NoSpace { free, needed } => tracing::warn!(
            when,
            free_mb = free / (1024 * 1024),
            needed_mb = needed / (1024 * 1024),
            "skipped the library backup: not enough free disk space"
        ),
        Outcome::Failed(why) => {
            tracing::warn!(when, error = %why, "the library backup failed")
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Taking one
// ---------------------------------------------------------------------------

/// Copy `db` into `<data_dir>/backups/` (see the module docs). Always
/// returns an [`Outcome`]; on anything but `Saved` no file is left behind.
fn take(
    db: &Path,
    data_dir: &Path,
    label: &str,
    now: DateTime<Utc>,
    free_space: &dyn Fn(&Path) -> Option<u64>,
    busy: Duration,
) -> Outcome {
    let dir = data_dir.join(BACKUP_DIR);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Outcome::Failed(format!("couldn't create the backups folder: {e}"));
    }
    // The WAL's committed pages are part of the library until they are
    // folded in, so they count toward its size.
    let mut db_bytes = std::fs::metadata(db).map(|m| m.len()).unwrap_or(0);
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    db_bytes += std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0);
    let needed = needed_free_bytes(db_bytes);
    if let Some(free) = free_space(&dir) {
        if free < needed {
            return Outcome::NoSpace { free, needed };
        }
    }
    sweep_partials(&dir);
    let name = unique_name(&dir, label, now);
    let (final_path, partial) = (dir.join(&name), dir.join(format!("{name}{PARTIAL_SUFFIX}")));
    match copy_verified(db, &partial, &final_path, busy) {
        Ok(()) => {
            apply_retention(&dir, &name);
            let bytes = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
            let (label, taken_at) = parse_name(&name)
                .map(|(l, t)| (l.to_owned(), t.to_owned()))
                .unwrap_or_default();
            Outcome::Saved(BackupFile {
                path: final_path,
                name,
                label,
                taken_at,
                bytes,
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Outcome::Failed(e.to_string())
        }
    }
}

/// A name no file in `dir` has yet: the stamp moves on by a second per clash
/// (two backups inside one second only happen in tests, but the rename must
/// never replace a backup).
fn unique_name(dir: &Path, label: &str, now: DateTime<Utc>) -> String {
    let mut at = now;
    for _ in 0..120 {
        let name = backup_name(label, at);
        if !dir.join(&name).exists() && !dir.join(format!("{name}{PARTIAL_SUFFIX}")).exists() {
            return name;
        }
        at += ChronoDuration::seconds(1);
    }
    backup_name(label, at)
}

fn copy_verified(db: &Path, partial: &Path, final_path: &Path, busy: Duration) -> Result<()> {
    let user_version = {
        let src = open_read_only(db, busy)?;
        let v: i64 = src.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let target = partial
            .to_str()
            .ok_or_else(|| StorageError::Invalid("the backup path isn't valid text".into()))?;
        // A consistent snapshot of the committed state, WAL included.
        src.execute("VACUUM INTO ?1", [target])?;
        v
    };
    // Flush the copy before it can count: the rename below is what makes it
    // a backup, so the bytes must already be on disk.
    std::fs::OpenOptions::new()
        .write(true)
        .open(partial)?
        .sync_all()?;
    verify(partial, user_version, busy)?;
    rename_with_retry(partial, final_path)?;
    sync_dir(final_path.parent());
    Ok(())
}

/// The copy opens, passes `quick_check` and carries the source's schema
/// version.
fn verify(copy: &Path, user_version: i64, busy: Duration) -> Result<()> {
    let conn = open_read_only(copy, busy)?;
    let rows: Vec<String> = conn
        .prepare("PRAGMA quick_check")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    if rows != ["ok"] {
        return Err(StorageError::Invalid(format!(
            "the backup failed its integrity check: {}",
            rows.join("; ")
        )));
    }
    let got: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if got != user_version {
        return Err(StorageError::Invalid(format!(
            "the backup has schema v{got}, expected v{user_version}"
        )));
    }
    Ok(())
}

/// Make the rename durable where the platform allows (Unix: sync the
/// folder; Windows has no equivalent for a directory handle).
fn sync_dir(dir: Option<&Path>) {
    #[cfg(unix)]
    if let Some(dir) = dir {
        if let Ok(f) = std::fs::File::open(dir) {
            let _ = f.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Unfinished copies of a crashed run. Only called under [`lock`], when no
/// copy of this process is in flight.
fn sweep_partials(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let is_ours = e.file_name().to_str().is_some_and(is_partial_name);
        if is_ours && std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_file()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Keep `keep_name` plus the newest others, [`KEEP_BACKUPS`] in all; delete
/// the rest. Only exact-pattern files are listed, so nothing else the user
/// put in the folder is touched.
fn apply_retention(dir: &Path, keep_name: &str) {
    let others = list_in(dir)
        .into_iter()
        .filter(|f| f.name != keep_name)
        .skip(KEEP_BACKUPS.saturating_sub(1));
    for old in others {
        match std::fs::remove_file(&old.path) {
            Ok(()) => tracing::info!(name = %old.name, "removed an old library backup"),
            Err(e) => {
                tracing::warn!(name = %old.name, error = %e, "couldn't remove an old library backup")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Recording what happened
// ---------------------------------------------------------------------------

/// Record a launch's backup step once the library is open: the running
/// version, the compression marker, and the notice when the backup didn't
/// happen. Best effort — a settings write that fails costs at most one more
/// backup next launch.
pub fn after_open(storage: &Storage, running: &str, outcome: &Outcome) {
    let stored = storage.get_setting(LAST_RUN_VERSION_KEY).ok().flatten();
    match outcome {
        // Nothing was there to lose, and compression of a library born
        // under this feature needs no "before" copy.
        Outcome::NewLibrary => set(storage, BEFORE_COMPRESSION_KEY, "done"),
        Outcome::Saved(_) => set(storage, BEFORE_COMPRESSION_KEY, "done"),
        Outcome::NoSpace { .. } => raise_notice(storage, NoticeKind::NoSpace),
        Outcome::Failed(_) => raise_notice(storage, NoticeKind::Failed),
        // Only an upgrade is worth a notice, and a read that failed can't say
        // whether this was one: the version row can, now the library is open.
        Outcome::Unchecked(_) if stored.as_deref() != Some(running) => {
            raise_notice(storage, NoticeKind::Failed)
        }
        Outcome::Unchecked(_) | Outcome::NotNeeded => {}
    }
    if stored.as_deref() != Some(running) {
        set(storage, LAST_RUN_VERSION_KEY, running);
    }
}

/// Whether the compression job still owes the library a backup.
pub fn compression_backup_due(storage: &Storage) -> bool {
    storage
        .get_setting(BEFORE_COMPRESSION_KEY)
        .ok()
        .flatten()
        .as_deref()
        != Some("done")
}

/// Record the compression job's backup. A failure raises the notice unless
/// one is already waiting (the startup step may just have raised it).
pub fn after_compression_backup(storage: &Storage, outcome: &Outcome) {
    match outcome {
        Outcome::Saved(_) | Outcome::NewLibrary => set(storage, BEFORE_COMPRESSION_KEY, "done"),
        Outcome::NoSpace { .. } if pending_notice(storage).is_none() => {
            raise_notice(storage, NoticeKind::NoSpace)
        }
        Outcome::Failed(_) if pending_notice(storage).is_none() => {
            raise_notice(storage, NoticeKind::Failed)
        }
        _ => {}
    }
}

fn set(storage: &Storage, key: &str, value: &str) {
    if let Err(e) = storage.set_setting(key, value) {
        tracing::warn!(key, error = %e, "couldn't record backup state");
    }
}

fn raise_notice(storage: &Storage, kind: NoticeKind) {
    set(storage, NOTICE_KEY, kind.as_str());
}

/// The notice that is due, if any.
pub fn pending_notice(storage: &Storage) -> Option<NoticeKind> {
    storage
        .get_setting(NOTICE_KEY)
        .ok()
        .flatten()
        .as_deref()
        .and_then(NoticeKind::parse)
}

/// The user dismissed the notice: it does not come back.
pub fn dismiss_notice(storage: &Storage) {
    if let Err(e) = storage.delete_setting(NOTICE_KEY) {
        tracing::warn!(error = %e, "couldn't clear the backup notice");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn names_round_trip_and_only_the_exact_pattern_parses() {
        let n = backup_name("2.4.0", at(0));
        assert_eq!(n, "flyonthewall-2.4.0-20231114T221320Z.db");
        assert_eq!(parse_name(&n), Some(("2.4.0", "20231114T221320Z")));
        assert_eq!(
            parse_name("flyonthewall-schema2-20231114T221320Z.db"),
            Some(("schema2", "20231114T221320Z"))
        );
        for bad in [
            "flyonthewall.db",
            "flyonthewall-2.4.0.db",
            "flyonthewall--20231114T221320Z.db",
            "flyonthewall-2.4.0-20231114T221320Z.db-wal",
            "flyonthewall-2.4.0-20231114T221320Z.db.partial",
            "flyonthewall-2.4.0-2023111422132Z.db",
            "flyonthewall-2.4.0-20231114X221320Z.db",
            "flyonthewall-2.4.0-20231114T22132aZ.db",
            "backup-2.4.0-20231114T221320Z.db",
            "notes.db",
            "../flyonthewall-2.4.0-20231114T221320Z.db",
            "flyonthewall-2.4/0-20231114T221320Z.db",
            "flyonthewall-2.4 0-20231114T221320Z.db",
        ] {
            assert_eq!(parse_name(bad), None, "{bad}");
        }
        assert!(is_partial_name(
            "flyonthewall-2.4.0-20231114T221320Z.db.partial"
        ));
        assert!(!is_partial_name("notes.partial"));
        assert!(!is_partial_name("flyonthewall.db.partial"));
    }

    #[test]
    fn labels_are_made_safe_for_file_names() {
        assert_eq!(sanitize_label("2.5.0"), "2.5.0");
        assert_eq!(sanitize_label("2.5.0-beta.1"), "2.5.0_beta.1");
        assert_eq!(sanitize_label("a/b\\c d"), "a_b_c_d");
        assert_eq!(sanitize_label(""), "unknown");
        assert!(label_ok(&sanitize_label(&"x".repeat(100))));
    }

    #[test]
    fn the_free_space_rule_is_twice_the_database_or_200_mb() {
        let mb = 1024 * 1024;
        assert_eq!(needed_free_bytes(0), 200 * mb);
        assert_eq!(needed_free_bytes(99 * mb), 200 * mb);
        assert_eq!(needed_free_bytes(100 * mb), 200 * mb);
        assert_eq!(needed_free_bytes(300 * mb), 600 * mb);
        assert_eq!(needed_free_bytes(u64::MAX), u64::MAX);
    }

    #[test]
    fn notice_kinds_round_trip() {
        for k in [NoticeKind::NoSpace, NoticeKind::Failed] {
            assert_eq!(NoticeKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(NoticeKind::parse(""), None);
        assert_eq!(NoticeKind::parse("pending"), None);
    }
}
