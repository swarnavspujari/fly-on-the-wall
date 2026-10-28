//! Crash-safe replacement of the mirror files the app writes next to the
//! database (note markdown, `note.json`, `recording.manifest.json`, the
//! transcript md/json/vtt files).
//!
//! A plain `fs::write` truncates the target first, so a crash, power cut or
//! full disk mid-write leaves a torn file — and these mirrors are what
//! self-heal restores meetings from when the database is lost. Instead the
//! new content goes to a temp file in the SAME folder (same volume, so the
//! rename is atomic), is flushed to disk, and only then renamed over the
//! target: readers see the old file or the new one, never half of either.
//! Pattern from `fly-asr`'s checkpoint writer.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Distinguishes temp files of concurrent writers (the app and the MCP
/// server both write transcript mirrors).
static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

/// How long to keep retrying a rename that a lock is blocking.
#[derive(Debug, Clone, Copy)]
struct RetryPolicy {
    /// Total rename attempts (at least 1).
    attempts: u32,
    /// Wait before the second attempt; doubles after each further failure.
    first_backoff: Duration,
}

/// 6 attempts, waiting 50 → 800 ms (about 1.5 s in all): long enough for a
/// virus scanner or sync client to let go of a file it just opened.
const RENAME_RETRY: RetryPolicy = RetryPolicy {
    attempts: 6,
    first_backoff: Duration::from_millis(50),
};

/// A rename failure that another process's open handle causes and that
/// clears by itself. Windows reports a scanner reading our fresh temp file,
/// or a sync client / editor holding the target without delete sharing, as
/// ERROR_ACCESS_DENIED (5), ERROR_SHARING_VIOLATION (32) or
/// ERROR_LOCK_VIOLATION (33) — whether or not the target exists yet. Other
/// platforms rename regardless of open handles.
fn is_transient_lock(e: &io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

/// Atomically replace `path` with `contents`.
pub(crate) fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let contents = contents.as_ref();
    write_atomic_with(
        path,
        |f| f.write_all(contents),
        contents,
        |from, to| std::fs::rename(from, to),
        RENAME_RETRY,
    )
}

/// Core of [`write_atomic`], with its seams exposed for tests: `fill` writes
/// the temp file (a test can make it fail halfway), `rename` replaces the
/// target (a test can make it hit a lock), `retry` sets the patience.
/// `fallback` is what a direct write puts in place when the lock never
/// clears.
fn write_atomic_with(
    path: &Path,
    fill: impl FnOnce(&mut File) -> io::Result<()>,
    fallback: &[u8],
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
    retry: RetryPolicy,
) -> io::Result<()> {
    let tmp = temp_path_for(path)?;
    let staged = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        fill(&mut file)?;
        // Durable before it becomes visible: without this a power cut right
        // after the rename can leave the new name pointing at empty blocks.
        file.sync_all()
    })();
    if let Err(e) = staged {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let mut backoff = retry.first_backoff;
    let mut attempt = 1;
    loop {
        match rename(&tmp, path) {
            Ok(()) => {
                sync_parent(path);
                return Ok(());
            }
            // (a directory in the way is not a lock — nothing to wait out)
            Err(e) if is_transient_lock(&e) && !path.is_dir() => {
                if attempt < retry.attempts.max(1) {
                    attempt += 1;
                    std::thread::sleep(backoff);
                    backoff = backoff.saturating_mul(2);
                    continue;
                }
                // Still locked. Writing in place is what the app did before
                // atomic writes, and it can succeed where a replace can't (a
                // holder that shares writing but not deletion); a mirror
                // that's current beats one that silently stops updating.
                let _ = std::fs::remove_file(&tmp);
                tracing::warn!(
                    file = %path.file_name().unwrap_or_default().to_string_lossy(),
                    error = %e,
                    attempts = attempt,
                    "mirror file stayed locked against replacement; writing it in place"
                );
                return std::fs::write(path, fallback);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
}

/// Rename `from` over `to`, retrying while another process's open handle
/// blocks it (the same patience as the mirror writes, about 1.5 s). For
/// callers that stage their own temp file, like media import's converted
/// audio. Unlike [`write_atomic`] there's no in-place fallback: the error
/// comes back, and removing `from` is the caller's call. Blocks the thread
/// while it waits.
pub fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    rename_with_retry_using(from, to, |a, b| std::fs::rename(a, b), RENAME_RETRY)
}

fn rename_with_retry_using(
    from: &Path,
    to: &Path,
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
    retry: RetryPolicy,
) -> io::Result<()> {
    let mut backoff = retry.first_backoff;
    let mut attempt = 1;
    loop {
        match rename(from, to) {
            // (a directory in the way is not a lock — nothing to wait out)
            Err(e) if is_transient_lock(&e) && !to.is_dir() && attempt < retry.attempts.max(1) => {
                attempt += 1;
                std::thread::sleep(backoff);
                backoff = backoff.saturating_mul(2);
            }
            other => return other,
        }
    }
}

/// `<dir>/.<name>.<pid><n>.tmp` next to the target. The leading dot and
/// `.tmp` suffix keep it out of every scan that matches mirror names
/// (`*.md`, `recording.manifest.json`, …).
fn temp_path_for(path: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "mirror path has no file name")
    })?;
    let n = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.{:x}{:x}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        n
    );
    Ok(path.with_file_name(tmp_name))
}

/// Make the rename itself durable where the OS allows syncing a directory
/// (Unix). Best-effort: the data is already safe, this only pins the name.
fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn replaces_existing_content_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.json");
        std::fs::write(&path, "old").unwrap();
        write_atomic(&path, "new content").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content");
        assert_eq!(entries(dir.path()), vec!["note.json"]);
        // and creates a file that didn't exist yet
        let fresh = dir.path().join("transcript.vtt");
        write_atomic(&fresh, "WEBVTT").unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "WEBVTT");
    }

    /// The torn-write scenario: the write dies halfway (disk full, crash).
    /// The previous file must survive byte for byte and no partial or temp
    /// file may be left behind.
    #[test]
    fn failed_write_leaves_previous_file_intact_and_no_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recording.manifest.json");
        let original = r#"{"meeting_id":"m1","note_id":"n1"}"#;
        std::fs::write(&path, original).unwrap();

        let err = write_atomic_with(
            &path,
            |f| {
                f.write_all(b"{\"meeting_id\":\"m1\",\"no")?; // half a document
                Err(io::Error::other("simulated: disk full"))
            },
            b"unused",
            |a, b| std::fs::rename(a, b),
            FAST,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "simulated: disk full");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(entries(dir.path()), vec!["recording.manifest.json"]);

        // a failed first write of a new file leaves nothing at all
        let missing = dir.path().join("note.json");
        write_atomic_with(
            &missing,
            |f| {
                f.write_all(b"{\"half")?;
                Err(io::Error::other("simulated"))
            },
            b"unused",
            |a, b| std::fs::rename(a, b),
            FAST,
        )
        .unwrap_err();
        assert!(!missing.exists());
        assert_eq!(entries(dir.path()), vec!["recording.manifest.json"]);
    }

    /// A rename that can't happen (target is a directory) fails cleanly:
    /// the error surfaces and the staged temp file is removed.
    #[test]
    fn failed_rename_cleans_up_the_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("transcript.md");
        std::fs::create_dir(&target).unwrap();
        assert!(write_atomic(&target, "x").is_err());
        assert!(target.is_dir());
        assert_eq!(entries(dir.path()), vec!["transcript.md"]);
    }

    /// Lock-retry tests don't need to wait the real 1.5 s.
    const FAST: RetryPolicy = RetryPolicy {
        attempts: 4,
        first_backoff: Duration::from_millis(1),
    };

    /// A rename that fails for any reason other than a lock is not retried.
    #[test]
    fn non_lock_rename_errors_fail_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.json");
        let calls = std::cell::Cell::new(0);
        let err = write_atomic_with(
            &path,
            |f| f.write_all(b"x"),
            b"x",
            |_, _| {
                calls.set(calls.get() + 1);
                Err(io::Error::new(io::ErrorKind::NotFound, "simulated"))
            },
            FAST,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(calls.get(), 1);
        assert!(entries(dir.path()).is_empty(), "temp file cleaned up");
    }

    /// A scanner or sync client briefly holding the file: Windows sharing
    /// and lock violations (32, 33) and access denied (5) are retried —
    /// also on a FIRST write, where the lock is on our own fresh temp file.
    #[cfg(windows)]
    #[test]
    fn sharing_violation_is_retried_until_it_clears() {
        let dir = tempfile::tempdir().unwrap();
        for code in [5, 32, 33] {
            let path = dir.path().join(format!("first-write-{code}.json"));
            let calls = std::cell::Cell::new(0);
            write_atomic_with(
                &path,
                |f| f.write_all(b"new"),
                b"new",
                |a, b| {
                    calls.set(calls.get() + 1);
                    if calls.get() < 3 {
                        Err(io::Error::from_raw_os_error(code))
                    } else {
                        std::fs::rename(a, b)
                    }
                },
                FAST,
            )
            .unwrap();
            assert_eq!(calls.get(), 3, "error {code}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        }
        let mut want: Vec<String> = [5, 32, 33]
            .iter()
            .map(|c| format!("first-write-{c}.json"))
            .collect();
        want.sort();
        assert_eq!(entries(dir.path()), want, "no temp files left");
    }

    /// A lock that never clears: after the retries the mirror is written in
    /// place (the pre-atomic behavior) rather than silently going stale —
    /// for an existing target and a first write alike.
    #[cfg(windows)]
    #[test]
    fn lock_that_never_clears_falls_back_to_an_in_place_write() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("transcript.md");
        std::fs::write(&existing, "old").unwrap();
        let missing = dir.path().join("transcript.vtt");
        for path in [&existing, &missing] {
            let calls = std::cell::Cell::new(0);
            write_atomic_with(
                path,
                |f| f.write_all(b"new"),
                b"new",
                |_, _| {
                    calls.set(calls.get() + 1);
                    Err(io::Error::from_raw_os_error(32))
                },
                FAST,
            )
            .unwrap();
            assert_eq!(calls.get(), FAST.attempts);
            assert_eq!(std::fs::read_to_string(path).unwrap(), "new");
        }
        assert_eq!(entries(dir.path()), vec!["transcript.md", "transcript.vtt"]);
    }

    /// The real thing: another process holds the target open for reading
    /// and writing but NOT deletion (what sync clients and editors do), so
    /// Windows refuses to replace it. The write still lands.
    #[cfg(windows)]
    #[test]
    fn target_held_open_without_delete_sharing_still_gets_written() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.json");
        std::fs::write(&path, "old").unwrap();
        let _holder = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
            .unwrap();
        let calls = std::cell::Cell::new(0);
        write_atomic_with(
            &path,
            |f| f.write_all(b"new"),
            b"new",
            |a, b| {
                calls.set(calls.get() + 1);
                std::fs::rename(a, b)
            },
            FAST,
        )
        .unwrap();
        assert!(calls.get() > 1, "the held target really blocked the rename");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(entries(dir.path()), vec!["note.json"]);
    }

    /// `rename_with_retry` (for callers that stage their own temp file):
    /// the same patience with a lock, but no in-place fallback.
    #[cfg(windows)]
    #[test]
    fn rename_with_retry_waits_out_a_transient_lock() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("a.part"), dir.path().join("a.wav"));
        std::fs::write(&from, "audio").unwrap();
        for code in [5, 32, 33] {
            let calls = std::cell::Cell::new(0);
            let res = rename_with_retry_using(
                &from,
                &to,
                |_, _| {
                    calls.set(calls.get() + 1);
                    Err(io::Error::from_raw_os_error(code))
                },
                FAST,
            );
            assert_eq!(res.unwrap_err().raw_os_error(), Some(code));
            assert_eq!(calls.get(), FAST.attempts, "error {code}");
        }
        let calls = std::cell::Cell::new(0);
        rename_with_retry_using(
            &from,
            &to,
            |a, b| {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    Err(io::Error::from_raw_os_error(32))
                } else {
                    std::fs::rename(a, b)
                }
            },
            FAST,
        )
        .unwrap();
        assert_eq!(calls.get(), 3);
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "audio");
        assert!(!from.exists());
    }

    #[test]
    fn rename_with_retry_fails_at_once_on_other_errors_and_leaves_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("a.part"), dir.path().join("a.wav"));
        std::fs::write(&from, "audio").unwrap();
        let calls = std::cell::Cell::new(0);
        let err = rename_with_retry_using(
            &from,
            &to,
            |_, _| {
                calls.set(calls.get() + 1);
                Err(io::Error::new(io::ErrorKind::NotFound, "simulated"))
            },
            FAST,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(calls.get(), 1);
        // cleaning up the temp file is the caller's call
        assert!(from.exists());
        // and the real thing renames
        rename_with_retry(&from, &to).unwrap();
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "audio");
    }

    #[test]
    fn temp_names_are_unique_hidden_and_never_match_mirror_names() {
        let p = Path::new("dir").join("transcript.md");
        let a = temp_path_for(&p).unwrap();
        let b = temp_path_for(&p).unwrap();
        assert_ne!(a, b);
        for t in [&a, &b] {
            let name = t.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with(".transcript.md."), "{name}");
            assert!(name.ends_with(".tmp"), "{name}");
            assert_eq!(t.parent(), p.parent(), "same folder → atomic rename");
        }
    }
}
