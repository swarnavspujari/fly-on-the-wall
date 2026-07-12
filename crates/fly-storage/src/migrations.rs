//! Version-gated data migrations, keyed off SQLite `user_version` (stamped
//! by `Storage::migrate`). Each step runs exactly once per data dir and must
//! tolerate a rerun after a mid-migration crash: work is committed per
//! artifact, and every step skips artifacts that are already in shape.
//!
//! v2 — human-readable disk layout:
//!   * meeting folders `recordings/<uuid>/` → `recordings/<date> <title>/`,
//!     with `recording_json` (the source of truth for recording locations)
//!     rewritten alongside each rename;
//!   * transcript mirrors `transcripts/<uuid>.{md,json}` move into their
//!     meeting's folder as `transcript.{md,json}`;
//!   * stale 16 kHz pipeline intermediates (`*.16k.wav`) are swept;
//!   * note mirrors `notes/<uuid>.md` → `notes/<date> <title>.md`, recorded
//!     in the new `notes.disk_path` column;
//!   * artifacts with no DB row are parked under `recordings/_unlinked/` and
//!     `notes/_unlinked/` — preserved, never deleted. (The markdown mirrors
//!     flatten provenance, so an orphan note can't be re-indexed losslessly;
//!     parking is honest, resurrecting would be guesswork.)

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use fly_core::RecordingRef;
use rusqlite::Connection;

use crate::folders::parse_ts;
use crate::meetings::recording_dir_rel;
use crate::{naming, Result};

pub(crate) fn to_v2(conn: &Connection, data_dir: &Path) -> Result<()> {
    rename_meeting_dirs(conn, data_dir)?;
    backfill_note_paths(conn, data_dir)?;
    park_orphan_recordings(conn, data_dir)?;
    park_orphan_notes(conn, data_dir)?;
    Ok(())
}

/// One-time rename of the pre-rebrand SQLite file (and its WAL/SHM sidecars)
/// from `looma.db` to `flyonthewall.db`. Runs in `Storage::open` *before* the
/// connection is opened, so it can't be a `user_version` step (those fire
/// after open). Idempotent: each file is skipped when its target already
/// exists or its source is absent, so a fresh install and a rerun are no-ops.
pub(crate) fn rename_legacy_db(data_dir: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let old = data_dir.join(format!("looma.db{suffix}"));
        let new = data_dir.join(format!("flyonthewall.db{suffix}"));
        if old.exists() && !new.exists() {
            std::fs::rename(&old, &new)?;
        }
    }
    Ok(())
}

fn rename_meeting_dirs(conn: &Connection, data_dir: &Path) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.title, m.started_at, m.recording_json, n.title
         FROM meetings m LEFT JOIN notes n ON n.id = m.note_id
         WHERE m.recording_json IS NOT NULL",
    )?;
    let rows: Vec<(String, String, String, String, Option<String>)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?;

    for (meeting_id, meeting_title, started_at, recording_json, note_title) in rows {
        let Ok(rec) = serde_json::from_str::<RecordingRef>(&recording_json) else {
            continue;
        };
        let Some(old_rel) = recording_dir_rel(&rec) else {
            continue;
        };
        // Folders are named after the note when there is one: the note title
        // is the one the user curates (meetings snapshot it at creation).
        let title = note_title.unwrap_or(meeting_title);
        let base = naming::disk_label(parse_ts(started_at), &title);
        let old_abs = data_dir.join(&old_rel);
        let old_name = Path::new(&old_rel)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let dir_abs = if naming::already_labeled(&old_name, &base) {
            old_abs
        } else if old_abs.is_dir() {
            let new_abs = naming::unique_path(&data_dir.join("recordings"), &base, "", &|_| false);
            if let Err(e) = std::fs::rename(&old_abs, &new_abs) {
                // e.g. a file inside is open — keep the old (still valid) name
                tracing::warn!(meeting_id, error = %e, "skipping meeting folder rename");
                old_abs
            } else {
                let new_rel = format!(
                    "recordings/{}",
                    new_abs.file_name().unwrap_or_default().to_string_lossy()
                );
                conn.execute(
                    "UPDATE meetings SET recording_json = ?1 WHERE id = ?2",
                    (
                        serde_json::to_string(&remap_into(&rec, &new_rel))?,
                        &meeting_id,
                    ),
                )?;
                new_abs
            }
        } else {
            // referenced folder missing on disk — nothing to move
            continue;
        };

        // legacy top-level transcript mirrors join their meeting's folder
        for (ext, target) in [("md", "transcript.md"), ("json", "transcript.json")] {
            let legacy = data_dir
                .join("transcripts")
                .join(format!("{meeting_id}.{ext}"));
            if legacy.exists() {
                let _ = std::fs::rename(&legacy, dir_abs.join(target));
            }
        }
        sweep_16k(&dir_abs);
    }
    Ok(())
}

/// Rebase every recording path onto `new_rel_dir`, keeping file names.
fn remap_into(rec: &RecordingRef, new_rel_dir: &str) -> RecordingRef {
    let rebase = |p: &Option<String>| {
        p.as_ref().map(|p| {
            let file = p.rsplit('/').next().unwrap_or(p);
            format!("{new_rel_dir}/{file}")
        })
    };
    RecordingRef {
        mic_path: rebase(&rec.mic_path),
        system_path: rebase(&rec.system_path),
        mixed_path: rebase(&rec.mixed_path),
        duration_ms: rec.duration_ms,
    }
}

/// Delete leftover 16 kHz intermediates (`mic.16k.wav`, `system.16k.wav`,
/// `track.16k.wav`) that pre-cleanup pipelines never removed. Pure derived
/// data: the pipeline regenerates them from the originals when needed.
fn sweep_16k(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(".16k.wav") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn backfill_note_paths(conn: &Connection, data_dir: &Path) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, title, created_at FROM notes
         WHERE disk_path IS NULL OR disk_path = ''",
    )?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;

    // names allocated this run but whose files may not exist yet (notes
    // without a mirror on disk) must not collide with each other
    let mut allocated: HashSet<PathBuf> = HashSet::new();
    for (id, title, created_at) in rows {
        let base = naming::disk_label(parse_ts(created_at), &title);
        let target = naming::unique_path(&data_dir.join("notes"), &base, ".md", &|p| {
            allocated.contains(p)
        });
        allocated.insert(target.clone());
        let legacy = data_dir.join("notes").join(format!("{id}.md"));
        if legacy.exists() {
            let _ = std::fs::rename(&legacy, &target);
        }
        conn.execute(
            "UPDATE notes SET disk_path = ?1 WHERE id = ?2",
            (
                format!(
                    "notes/{}",
                    target.file_name().unwrap_or_default().to_string_lossy()
                ),
                &id,
            ),
        )?;
    }
    Ok(())
}

/// Recording folders no meeting row points at: keep them, but grouped under
/// `recordings/_unlinked/` (with their transcript mirrors) so the browsable
/// top level only shows named meetings.
fn park_orphan_recordings(conn: &Connection, data_dir: &Path) -> Result<()> {
    let mut stmt =
        conn.prepare("SELECT recording_json FROM meetings WHERE recording_json IS NOT NULL")?;
    let referenced: HashSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|j| j.ok())
        .filter_map(|j| serde_json::from_str::<RecordingRef>(&j).ok())
        .filter_map(|rec| recording_dir_rel(&rec))
        .filter_map(|rel| {
            Path::new(&rel)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .collect();

    let recordings = data_dir.join("recordings");
    let Ok(entries) = std::fs::read_dir(&recordings) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_dir() || name == "_unlinked" || referenced.contains(&name) {
            continue;
        }
        let parked = recordings.join("_unlinked").join(&name);
        std::fs::create_dir_all(recordings.join("_unlinked"))?;
        if std::fs::rename(entry.path(), &parked).is_err() {
            continue;
        }
        for (ext, target) in [("md", "transcript.md"), ("json", "transcript.json")] {
            let legacy = data_dir.join("transcripts").join(format!("{name}.{ext}"));
            if legacy.exists() {
                let _ = std::fs::rename(&legacy, parked.join(target));
            }
        }
    }
    Ok(())
}

/// Note mirrors no notes row points at → `notes/_unlinked/`.
fn park_orphan_notes(conn: &Connection, data_dir: &Path) -> Result<()> {
    let mut stmt = conn.prepare("SELECT disk_path FROM notes WHERE disk_path IS NOT NULL")?;
    let known: HashSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|p| p.ok())
        .filter_map(|rel| {
            Path::new(&rel)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .collect();

    let notes = data_dir.join("notes");
    let Ok(entries) = std::fs::read_dir(&notes) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_file() || !name.ends_with(".md") || known.contains(&name) {
            continue;
        }
        std::fs::create_dir_all(notes.join("_unlinked"))?;
        let _ = std::fs::rename(entry.path(), notes.join("_unlinked").join(&name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_legacy_db_moves_main_file_and_wal_shm() {
        let dir = tempfile::tempdir().unwrap();
        // An uncheckpointed WAL holds committed rows, so it must move with the
        // main file or that data is lost. (Bytes here are opaque, not real DBs.)
        std::fs::write(dir.path().join("looma.db"), b"main").unwrap();
        std::fs::write(dir.path().join("looma.db-wal"), b"wal").unwrap();
        std::fs::write(dir.path().join("looma.db-shm"), b"shm").unwrap();

        rename_legacy_db(dir.path()).unwrap();

        assert_eq!(std::fs::read(dir.path().join("flyonthewall.db")).unwrap(), b"main");
        assert_eq!(std::fs::read(dir.path().join("flyonthewall.db-wal")).unwrap(), b"wal");
        assert_eq!(std::fs::read(dir.path().join("flyonthewall.db-shm")).unwrap(), b"shm");
        assert!(!dir.path().join("looma.db").exists());

        // Rerun is a no-op that never clobbers the already-migrated file.
        rename_legacy_db(dir.path()).unwrap();
        assert_eq!(std::fs::read(dir.path().join("flyonthewall.db")).unwrap(), b"main");
    }
}
