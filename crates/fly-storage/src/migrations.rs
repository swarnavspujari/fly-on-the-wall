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
use crate::{naming, Result, Storage};

pub(crate) fn to_v2(conn: &Connection, data_dir: &Path) -> Result<()> {
    rename_meeting_dirs(conn, data_dir)?;
    backfill_note_paths(conn, data_dir)?;
    park_orphan_recordings(conn, data_dir)?;
    park_orphan_notes(conn, data_dir)?;
    Ok(())
}

/// Which part of the v3 step is running (see [`UpgradeProgress`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UpgradeStage {
    /// Rebuilding `transcript_segments` through the triggers.
    Segments,
    /// Re-chunking transcripts for semantic search.
    Chunks,
    /// Recording citation locators.
    Citations,
    /// Rebuilding `segments_fts` and committing.
    Index,
}

/// Progress of the one-time v3 step, reported by
/// [`Storage::open_with_progress`]. `done` / `total` are work units weighted
/// by each stage's measured cost, not meetings: use them as a fraction.
/// `total` is fixed for the run, `done` never decreases, and the last report
/// has `done == total` (sent once the step has committed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpgradeProgress {
    pub stage: UpgradeStage,
    pub done: u64,
    pub total: u64,
}

/// Units per transcript for each stage, in proportion to the stages' times
/// in the 1,000-meeting benchmark (`tests/migration_v3.rs::v3_step_timing`):
/// the trigger rebuild takes about 60 %, the re-chunk and the locators about
/// 15 % each, the index rebuild and commit the rest.
const UNITS_SEGMENTS: u64 = 8;
const UNITS_CHUNKS: u64 = 2;
const UNITS_CITATIONS: u64 = 2;
const UNITS_INDEX: u64 = 1;

/// Minimum gap between two reports within a stage.
const REPORT_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

/// Throttles progress reports (a stage's first report and the final one
/// always go out) and times each stage for the log line.
struct Reporter<'a> {
    sink: &'a mut dyn FnMut(UpgradeProgress),
    total: u64,
    sent_at: Option<std::time::Instant>,
    stage: Option<(UpgradeStage, std::time::Instant)>,
    stage_ms: Vec<(UpgradeStage, u64)>,
}

impl<'a> Reporter<'a> {
    fn new(sink: &'a mut dyn FnMut(UpgradeProgress), total: u64) -> Self {
        Self {
            sink,
            total,
            sent_at: None,
            stage: None,
            stage_ms: Vec::new(),
        }
    }

    fn at(&mut self, stage: UpgradeStage, done: u64) {
        let now = std::time::Instant::now();
        let new_stage = self.stage.is_none_or(|(s, _)| s != stage);
        if new_stage {
            self.close_stage(now);
            self.stage = Some((stage, now));
        }
        let due = new_stage
            || done >= self.total
            || self
                .sent_at
                .is_none_or(|t| now.duration_since(t) >= REPORT_EVERY);
        if due {
            (self.sink)(UpgradeProgress {
                stage,
                done: done.min(self.total),
                total: self.total,
            });
            self.sent_at = Some(now);
        }
    }

    fn close_stage(&mut self, now: std::time::Instant) {
        if let Some((stage, since)) = self.stage.take() {
            let ms = now.duration_since(since).as_millis() as u64;
            self.stage_ms.push((stage, ms));
        }
    }

    fn ms(&self, stage: UpgradeStage) -> u64 {
        self.stage_ms
            .iter()
            .find(|(s, _)| *s == stage)
            .map_or(0, |(_, ms)| *ms)
    }
}

/// v3 — retrieval index + citation locators (one transaction, so a crash
/// leaves the file exactly as it was and the step simply runs again):
///   * `transcript_segments` / `segments_fts` rebuilt for every transcript
///     through the `transcripts` triggers (segments.rs), after dropping rows
///     whose transcript is gone;
///   * every transcript re-chunked: label-free text, exact spans, polished
///     text where it exists. Chunk text changes once, so the corpus
///     re-embeds once (the embedding worker picks the pending rows up);
///   * citation locators recorded for every AI block and item from the
///     current transcripts (their ids are valid now, so they start `ok`).
///
/// It is also what heals a file an OLDER build wrote to (that build stamps
/// `user_version` back, so this runs again): labelled chunks it wrote are
/// re-chunked, its deletes are reconciled, and locators it never wrote are
/// recorded. Unreadable rows are logged and skipped — never fatal, since a
/// failing step would stop the app from opening.
///
/// The trigger rebuild runs one transcript at a time — the same trigger,
/// fired for the same rows, as a table-wide `UPDATE` — so `progress` can
/// follow it.
pub(crate) fn to_v3(storage: &Storage, progress: &mut dyn FnMut(UpgradeProgress)) -> Result<()> {
    let started = std::time::Instant::now();
    let conn = &storage.conn;
    let tx = conn.unchecked_transaction()?;
    conn.execute_batch(
        "DELETE FROM transcript_segments
          WHERE meeting_id NOT IN (SELECT meeting_id FROM transcripts);
         DELETE FROM embedding_chunks
          WHERE owner_kind = 'transcript'
            AND owner_id NOT IN (SELECT meeting_id FROM transcripts);",
    )?;
    // Oldest meeting first: the re-chunk below hands each meeting's new
    // chunk rows the next ids, and the embedding queue takes the highest
    // ids first, so the newest meeting is re-embedded first. (Primary-key
    // order would be the random order of the UUIDs.)
    let meeting_ids: Vec<String> = conn
        .prepare(
            "SELECT t.meeting_id FROM transcripts t
               LEFT JOIN meetings m ON m.id = t.meeting_id
              ORDER BY COALESCE(m.started_at, ''), t.meeting_id",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    // Units per transcript; an empty library still goes 0 → total.
    let unit = (meeting_ids.len() as u64).max(1);
    let chunks_at = UNITS_SEGMENTS * unit;
    let citations_at = chunks_at + UNITS_CHUNKS * unit;
    let index_at = citations_at + UNITS_CITATIONS * unit;
    let total = index_at + UNITS_INDEX * unit;
    let mut report = Reporter::new(progress, total);

    report.at(UpgradeStage::Segments, 0);
    {
        let mut rebuild =
            conn.prepare("UPDATE transcripts SET json = json WHERE meeting_id = ?1")?;
        for (i, id) in meeting_ids.iter().enumerate() {
            rebuild.execute([id])?;
            report.at(UpgradeStage::Segments, UNITS_SEGMENTS * (i as u64 + 1));
        }
    }
    report.at(UpgradeStage::Chunks, chunks_at);
    for (i, id) in meeting_ids.iter().enumerate() {
        let res = storage
            .get_transcript(id)
            .and_then(|t| t.map_or(Ok(()), |t| storage.sync_transcript_chunks(&t)));
        if let Err(e) = res {
            tracing::warn!(meeting_id = %id, error = %e, "v3: skipping transcript chunks");
        }
        report.at(
            UpgradeStage::Chunks,
            chunks_at + UNITS_CHUNKS * (i as u64 + 1),
        );
    }
    report.at(UpgradeStage::Citations, citations_at);
    storage.sync_all_citations(&mut |done, of| {
        let share = UNITS_CITATIONS * unit * done as u64 / (of as u64).max(1);
        report.at(UpgradeStage::Citations, citations_at + share);
    })?;
    report.at(UpgradeStage::Index, index_at);
    // A keyword index left damaged by anything (an older build's crash, a
    // half-copied file) would now fail transcript writes: rebuild it from
    // its content table.
    storage.rebuild_segment_index()?;
    tx.commit()?;
    report.at(UpgradeStage::Index, total);
    report.close_stage(std::time::Instant::now());
    tracing::info!(
        meetings = meeting_ids.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        segments_ms = report.ms(UpgradeStage::Segments),
        chunks_ms = report.ms(UpgradeStage::Chunks),
        citations_ms = report.ms(UpgradeStage::Citations),
        index_commit_ms = report.ms(UpgradeStage::Index),
        "schema v3 step done"
    );
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

/// The database file [`Storage::open`] will open in `data_dir`, after
/// [`rename_legacy_db`]: `flyonthewall.db`, else a pre-rebrand `looma.db`
/// (which the open renames first), else none yet.
pub(crate) fn db_file_to_open(data_dir: &Path) -> Option<PathBuf> {
    ["flyonthewall.db", "looma.db"]
        .into_iter()
        .map(|name| data_dir.join(name))
        .find(|p| p.exists())
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
        playback_path: rebase(&rec.playback_path),
        duration_ms: rec.duration_ms,
    }
}

/// Delete leftover 16 kHz intermediates (`mic.16k.wav`, `system.16k.wav`,
/// `track.16k.wav`) that pre-cleanup pipelines never removed. Pure derived
/// data: the pipeline regenerates them from the originals when needed.
/// Exact names only — a user's own `<name>.16k.wav` is never matched.
fn sweep_16k(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let is_file = entry.file_type().is_ok_and(|t| t.is_file());
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_file && crate::scratch::is_legacy_intermediate_name(&name) {
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

        assert_eq!(
            std::fs::read(dir.path().join("flyonthewall.db")).unwrap(),
            b"main"
        );
        assert_eq!(
            std::fs::read(dir.path().join("flyonthewall.db-wal")).unwrap(),
            b"wal"
        );
        assert_eq!(
            std::fs::read(dir.path().join("flyonthewall.db-shm")).unwrap(),
            b"shm"
        );
        assert!(!dir.path().join("looma.db").exists());

        // Rerun is a no-op that never clobbers the already-migrated file.
        rename_legacy_db(dir.path()).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("flyonthewall.db")).unwrap(),
            b"main"
        );
    }
}
