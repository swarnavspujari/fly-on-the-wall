//! Pipeline scratch space, keyed by MEETING ID in a machine-local cache
//! folder: the 16 kHz intermediates (`*.16k.wav`), their ASR checkpoints
//! (`*.asrpart.json`) and the live-caption chunks (`live-tmp/`).
//!
//! These used to live inside the meeting's folder. That folder can be
//! renamed at any time (a title or date edit, or the user in Explorer), and a
//! pipeline that captured a path inside it lost nine hours of ASR when that
//! happened (2026-07-19 import incident). Keyed by id, a run's scratch path
//! never changes under it. It also keeps gigabytes of derived audio out of
//! Roaming AppData and out of any folder a sync client might watch.
//!
//! Older builds left their scratch in meeting folders: a pipeline adopts its
//! own meeting's leftovers ([`prepare_meeting_scratch`]), and the startup
//! sweep ([`Storage::sweep_stale_scratch`]) removes the rest once nothing can
//! still resume from them.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::{contained_recordings_dir, Storage};

/// Folder name of the scratch root.
pub const SCRATCH_DIR: &str = "scratch";

/// Live-caption chunk folder inside a meeting's scratch dir.
pub const LIVE_TMP_DIR: &str = "live-tmp";

/// Cache scratch folders modified more recently than this survive the
/// startup sweep whatever their meeting's state.
const SCRATCH_GRACE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Machine-local scratch root for `data_dir`:
/// - a data dir under the OS data dir (the default: Roaming AppData on
///   Windows, Application Support on macOS, `~/.local/share` on Linux) maps
///   to the same relative path under the OS cache dir (Local AppData,
///   Caches, `~/.cache`) — `%LOCALAPPDATA%\FlyOnTheWall\scratch` by default;
/// - any other data dir (tests, a custom location) keeps it inside:
///   `<data dir>/scratch`.
pub fn scratch_root(data_dir: &Path) -> PathBuf {
    scratch_root_in(
        data_dir,
        dirs::data_dir().as_deref(),
        dirs::cache_dir().as_deref(),
    )
}

fn scratch_root_in(
    data_dir: &Path,
    data_base: Option<&Path>,
    cache_base: Option<&Path>,
) -> PathBuf {
    if let (Some(data_base), Some(cache_base)) = (data_base, cache_base) {
        if data_base != cache_base {
            if let Ok(rel) = data_dir.strip_prefix(data_base) {
                if !rel.as_os_str().is_empty() {
                    return cache_base.join(rel).join(SCRATCH_DIR);
                }
            }
        }
    }
    data_dir.join(SCRATCH_DIR)
}

/// One meeting's scratch folder. Depends only on the data dir and the
/// meeting id — never on the meeting folder's name.
pub fn meeting_scratch_dir(data_dir: &Path, meeting_id: &str) -> PathBuf {
    scratch_root(data_dir).join(scratch_component(meeting_id))
}

/// Ids are UUIDs, but a resurrected meeting's id comes from a manifest that
/// may have been copied in from anywhere: anything that isn't a short plain
/// name is replaced by a stable hash, so it can never steer a path.
fn scratch_component(meeting_id: &str) -> String {
    let plain = !meeting_id.is_empty()
        && meeting_id.len() <= 64
        && meeting_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if plain {
        return meeting_id.to_string();
    }
    // FNV-1a: stability, not secrecy
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in meeting_id.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("id-{h:016x}")
}

/// Create (if needed) and return a meeting's scratch dir. With
/// `legacy_meeting_dir` — the meeting's folder — scratch an older build left
/// there is moved over first: ASR checkpoints are adopted (so a run that
/// failed under the old build still resumes) unless the new dir already has
/// one by that name, and 16 kHz intermediates are deleted (every run
/// regenerates them from the recordings).
pub fn prepare_meeting_scratch(
    data_dir: &Path,
    meeting_id: &str,
    legacy_meeting_dir: Option<&Path>,
) -> io::Result<PathBuf> {
    let dir = meeting_scratch_dir(data_dir, meeting_id);
    std::fs::create_dir_all(&dir)?;
    if let Some(legacy) = legacy_meeting_dir {
        for (path, kind) in legacy_scratch_in(legacy) {
            match kind {
                Legacy::Checkpoint => {
                    let to = dir.join(path.file_name().unwrap_or_default());
                    if to.exists() || move_file(&path, &to).is_err() {
                        let _ = std::fs::remove_file(&path);
                    }
                }
                Legacy::Derived => {
                    let _ = std::fs::remove_file(&path);
                }
                Legacy::LiveTmp => {
                    let _ = std::fs::remove_dir_all(&path);
                }
            }
        }
    }
    Ok(dir)
}

/// Same-volume rename, else copy + delete (Roaming and Local AppData can sit
/// on different volumes under folder redirection).
fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Legacy {
    /// `*.asrpart.json`: resumable ASR progress.
    Checkpoint,
    /// `*.16k.wav` and torn checkpoint temps (`*.asrpart.json.tmp`).
    Derived,
    /// The live-caption chunk folder.
    LiveTmp,
}

/// Scratch an older build left directly inside a meeting folder. Only plain
/// files (and the `live-tmp` folder) with the pipeline's own names count;
/// links are never followed.
fn legacy_scratch_in(meeting_dir: &Path) -> Vec<(PathBuf, Legacy)> {
    let Ok(entries) = std::fs::read_dir(meeting_dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = if ft.is_dir() && name == LIVE_TMP_DIR {
            Legacy::LiveTmp
        } else if !ft.is_file() {
            continue;
        } else if name.ends_with(".asrpart.json") {
            Legacy::Checkpoint
        } else if name.ends_with(".16k.wav") || name.ends_with(".asrpart.json.tmp") {
            Legacy::Derived
        } else {
            continue;
        };
        found.push((entry.path(), kind));
    }
    found
}

impl Storage {
    /// This data dir's scratch folder for one meeting (see the module docs).
    pub fn scratch_dir(&self, meeting_id: &str) -> PathBuf {
        meeting_scratch_dir(&self.data_dir, meeting_id)
    }

    /// Delete a meeting's scratch folder: it holds derived copies of the
    /// meeting's audio and partial transcripts, so it goes wherever the
    /// meeting or its audio goes. Best-effort.
    pub(crate) fn remove_meeting_scratch(&self, meeting_id: &str) {
        remove_dir_logged(&self.scratch_dir(meeting_id));
    }

    /// Delete every meeting's scratch ("Delete all my data"). Best-effort.
    pub(crate) fn remove_all_scratch(&self) {
        remove_dir_logged(&scratch_root(&self.data_dir));
    }

    /// Startup sweep of scratch nothing can use any more. A meeting whose
    /// transcription is queued, running or failed (a retry resumes from its
    /// checkpoints) keeps everything; so does a meeting still being recorded
    /// (no recording attached yet — its live captions write to scratch).
    /// Everything else goes:
    /// - scratch older builds left inside meeting folders;
    /// - cache scratch dirs of finished or deleted meetings, unless touched
    ///   in the last [`SCRATCH_GRACE`] (a re-analyze started right after
    ///   launch isn't a job, so only its fresh folder tells it apart).
    ///
    /// Runs with the storage lock held, so no job can be queued mid-sweep.
    pub(crate) fn sweep_stale_scratch(&self) {
        let cutoff = std::time::SystemTime::now()
            .checked_sub(SCRATCH_GRACE)
            .unwrap_or(std::time::UNIX_EPOCH);
        self.sweep_stale_scratch_before(cutoff);
    }

    fn sweep_stale_scratch_before(&self, cutoff: std::time::SystemTime) {
        let ScratchOwners { keep, finished } = match self.scratch_owners() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "scratch sweep could not read meetings");
                return;
            }
        };
        for (meeting_id, rel) in &finished {
            let Some(dir) = contained_recordings_dir(&self.data_dir, rel) else {
                continue;
            };
            for (path, kind) in legacy_scratch_in(&dir) {
                let res = match kind {
                    Legacy::LiveTmp => std::fs::remove_dir_all(&path),
                    _ => std::fs::remove_file(&path),
                };
                if let Err(e) = res {
                    tracing::warn!(meeting_id, error = %e, "couldn't remove old scratch from a meeting folder");
                }
            }
        }
        let root = scratch_root(&self.data_dir);
        let Ok(entries) = std::fs::read_dir(&root) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            let recent = entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t > cutoff);
            if is_dir && !recent && !keep.contains(&name) {
                remove_dir_logged(&entry.path());
            }
        }
    }

    fn scratch_owners(&self) -> crate::Result<ScratchOwners> {
        let mut keep = HashSet::new();
        let mut finished = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.recording_json,
                    EXISTS (SELECT 1 FROM transcription_jobs j
                             WHERE j.meeting_id = m.id
                               AND j.status IN ('queued', 'running', 'failed'))
               FROM meetings m",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })?;
        for row in rows {
            let (id, recording_json, pending) = row?;
            let rel = recording_json
                .and_then(|j| serde_json::from_str::<fly_core::RecordingRef>(&j).ok())
                .and_then(|rec| crate::recording_dir_rel(&rec));
            match rel {
                Some(rel) if !pending => finished.push((id, rel)),
                _ => {
                    keep.insert(scratch_component(&id));
                }
            }
        }
        Ok(ScratchOwners { keep, finished })
    }
}

/// Who may still need scratch, from the meeting rows.
struct ScratchOwners {
    /// Scratch folder names to keep: meetings with a pending job or no
    /// recording attached yet.
    keep: HashSet<String>,
    /// Finished meetings as (id, data-dir-relative folder).
    finished: Vec<(String, String)>,
}

fn remove_dir_logged(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir) {
        if e.kind() != io::ErrorKind::NotFound {
            tracing::warn!(error = %e, "couldn't remove a scratch folder");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_data_dir_maps_to_the_os_cache_dir() {
        let data = Path::new("/home/u/.local/share");
        let cache = Path::new("/home/u/.cache");
        assert_eq!(
            scratch_root_in(&data.join("FlyOnTheWall"), Some(data), Some(cache)),
            cache.join("FlyOnTheWall").join(SCRATCH_DIR)
        );
        // custom / test data dirs keep scratch inside themselves
        let custom = Path::new("/tmp/fotw-test");
        assert_eq!(
            scratch_root_in(custom, Some(data), Some(cache)),
            custom.join(SCRATCH_DIR)
        );
        // no OS dirs known → inside the data dir
        assert_eq!(
            scratch_root_in(&data.join("FlyOnTheWall"), None, None),
            data.join("FlyOnTheWall").join(SCRATCH_DIR)
        );
    }

    #[test]
    fn hostile_meeting_ids_never_steer_the_path() {
        let root = Path::new("data");
        for id in [
            "../../etc",
            "a/b",
            r"a\b",
            "C:",
            "",
            ".",
            "x".repeat(65).as_str(),
        ] {
            let dir = meeting_scratch_dir(root, id);
            let last = dir.file_name().unwrap().to_string_lossy().into_owned();
            assert!(last.starts_with("id-"), "{id:?} → {last}");
            assert_eq!(dir.parent().unwrap(), scratch_root(root));
        }
        // stable
        assert_eq!(
            meeting_scratch_dir(root, "../x"),
            meeting_scratch_dir(root, "../x")
        );
        let uuid = "862e2405-a77a-48ad-af38-fefcda797fa0";
        assert_eq!(
            meeting_scratch_dir(root, uuid),
            scratch_root(root).join(uuid)
        );
    }

    /// Cache scratch of finished and deleted meetings goes once it's past
    /// the grace period; a queued job's and an in-progress recording's
    /// stay, and nothing recently touched is removed at all.
    #[test]
    fn cache_sweep_drops_only_scratch_nothing_can_use() {
        use std::time::{Duration, SystemTime};
        let (_tmp, s) = crate::test_storage();
        let rec = fly_core::RecordingRef {
            mic_path: None,
            system_path: None,
            mixed_path: Some("recordings/x/recording.mixed.wav".into()),
            playback_path: None,
            duration_ms: 1_000,
        };
        let meeting = |title: &str| {
            let note = s.create_note(title, None).unwrap();
            s.create_meeting(title, &note.id, &[]).unwrap().id
        };
        let done = meeting("done");
        s.end_meeting(&done, &rec).unwrap();
        let queued = meeting("queued");
        s.end_meeting(&queued, &rec).unwrap();
        s.enqueue_transcription(&queued).unwrap();
        let recording_now = meeting("still recording"); // no recording yet
        let ids = [
            done.as_str(),
            queued.as_str(),
            recording_now.as_str(),
            "deleted-id",
        ];
        for id in ids {
            let d = s.scratch_dir(id).join(LIVE_TMP_DIR);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("you-0.wav"), b"RIFF").unwrap();
        }

        s.sweep_stale_scratch(); // all just created → inside the grace period
        assert!(ids.iter().all(|id| s.scratch_dir(id).is_dir()));

        s.sweep_stale_scratch_before(SystemTime::now() + Duration::from_secs(3600));
        assert!(!s.scratch_dir(&done).exists());
        assert!(!s.scratch_dir("deleted-id").exists());
        assert!(s.scratch_dir(&queued).is_dir());
        assert!(s.scratch_dir(&recording_now).is_dir());
    }

    #[test]
    fn prepare_adopts_old_checkpoints_and_drops_old_intermediates() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("recordings").join("2026-07-18 Import");
        std::fs::create_dir_all(folder.join(LIVE_TMP_DIR)).unwrap();
        for f in [
            "recording.mixed.wav",
            "track.16k.wav",
            "track.16k.asrpart.json",
            "track.16k.asrpart.json.tmp",
            "transcript.md",
        ] {
            std::fs::write(folder.join(f), f).unwrap();
        }
        std::fs::write(folder.join(LIVE_TMP_DIR).join("you-0.wav"), b"x").unwrap();

        let dir = prepare_meeting_scratch(tmp.path(), "m1", Some(&folder)).unwrap();
        assert_eq!(dir, meeting_scratch_dir(tmp.path(), "m1"));
        assert_eq!(
            std::fs::read_to_string(dir.join("track.16k.asrpart.json")).unwrap(),
            "track.16k.asrpart.json",
            "checkpoint adopted so the retry resumes"
        );
        let mut left: Vec<String> = std::fs::read_dir(&folder)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["recording.mixed.wav", "transcript.md"]);
    }
}
