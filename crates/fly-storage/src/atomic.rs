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

/// Rename attempts before giving up on an atomic replace. Windows refuses to
/// replace a file another process holds open without delete sharing (sync
/// clients and scanners do this briefly); a short retry rides that out.
const RENAME_ATTEMPTS: u32 = 4;
const RENAME_BACKOFF: Duration = Duration::from_millis(25);

/// Atomically replace `path` with `contents`.
pub(crate) fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let contents = contents.as_ref();
    write_atomic_with(path, |f| f.write_all(contents), contents)
}

/// Core of [`write_atomic`]: `fill` writes the temp file. Split out so tests
/// can make the write fail halfway. `fallback` is what a direct write puts
/// in place when the target stays locked against replacement.
fn write_atomic_with(
    path: &Path,
    fill: impl FnOnce(&mut File) -> io::Result<()>,
    fallback: &[u8],
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
    let mut attempt = 1;
    loop {
        match std::fs::rename(&tmp, path) {
            Ok(()) => {
                sync_parent(path);
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && path.is_file() => {
                if attempt < RENAME_ATTEMPTS {
                    attempt += 1;
                    std::thread::sleep(RENAME_BACKOFF);
                    continue;
                }
                // Still held open by someone else. Writing in place is what
                // the app did before atomic writes (and may succeed where a
                // replace can't); better a mirror that's current than one
                // that silently stops updating.
                let _ = std::fs::remove_file(&tmp);
                tracing::warn!(
                    file = %path.file_name().unwrap_or_default().to_string_lossy(),
                    error = %e,
                    "mirror file is locked against replacement; writing it in place"
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
