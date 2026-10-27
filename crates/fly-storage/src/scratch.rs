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
//! Older builds left their scratch in meeting folders: a pipeline moves its
//! own meeting's old ASR checkpoints over ([`prepare_meeting_scratch`],
//! which never deletes anything), and the startup sweep
//! ([`Storage::sweep_stale_scratch`]) removes the rest once nothing can
//! still resume from it. Old scratch is recognized by its EXACT names only
//! (media import keeps the user's own file names), and a 16 kHz
//! intermediate must also be in the pipeline's own WAV format.

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
/// `legacy_meeting_dir` — the meeting's folder — ASR checkpoints an older
/// build left there are moved over first, so a run that failed under the
/// old build still resumes. Nothing in the meeting folder is ever DELETED
/// here: a checkpoint that can't be moved (the scratch dir already has one
/// by that name, a full disk across volumes) stays where it is, and the
/// old 16 kHz intermediates are left for the startup sweep, which only
/// removes them once the meeting is finished and checks they really are the
/// pipeline's own files.
pub fn prepare_meeting_scratch(
    data_dir: &Path,
    meeting_id: &str,
    legacy_meeting_dir: Option<&Path>,
) -> io::Result<PathBuf> {
    prepare_meeting_scratch_with(data_dir, meeting_id, legacy_meeting_dir, move_file)
}

fn prepare_meeting_scratch_with(
    data_dir: &Path,
    meeting_id: &str,
    legacy_meeting_dir: Option<&Path>,
    mut move_file: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let dir = meeting_scratch_dir(data_dir, meeting_id);
    std::fs::create_dir_all(&dir)?;
    let Some(legacy) = legacy_meeting_dir else {
        return Ok(dir);
    };
    for (path, kind) in legacy_scratch_in(legacy) {
        if kind != Legacy::Checkpoint {
            continue;
        }
        let name = path.file_name().unwrap_or_default();
        let to = dir.join(name);
        if to.exists() {
            tracing::info!(
                file = %name.to_string_lossy(),
                "scratch already has this checkpoint; leaving the old one in the meeting folder"
            );
        } else if let Err(e) = move_file(&path, &to) {
            tracing::warn!(
                file = %name.to_string_lossy(),
                error = %e,
                "couldn't move an old ASR checkpoint to scratch; left it in place"
            );
        }
    }
    Ok(dir)
}

/// Same-volume rename, else copy (Roaming and Local AppData can sit on
/// different volumes under folder redirection). A failed copy removes its
/// own partial output — never the source; a copy whose source then can't be
/// deleted still counts (the startup sweep removes the leftover later).
fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if let Err(e) = std::fs::copy(from, to) {
        let _ = std::fs::remove_file(to);
        return Err(e);
    }
    let _ = std::fs::remove_file(from);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Legacy {
    /// `<stem>.asrpart.json`: resumable ASR progress.
    Checkpoint,
    /// A torn checkpoint temp (`<stem>.asrpart[.asrpart].json.tmp`).
    CheckpointTmp,
    /// `<stem>.wav`: a 16 kHz intermediate — or a user's import that happens
    /// to carry the same name (see [`is_pipeline_wav`]).
    Intermediate,
    /// The live-caption chunk folder.
    LiveTmp,
}

/// The exact stems older builds wrote into meeting folders: the pipeline's
/// per-channel and single-track inputs, re-diarize's input, and media
/// import's per-file conversions (`import-00.16k` …). Media import keeps the
/// user's own file names, so a suffix like `.16k.wav` proves nothing — only
/// these names count.
fn is_pipeline_stem(stem: &str) -> bool {
    matches!(
        stem,
        "mic.16k" | "system.16k" | "track.16k" | "rediarize.16k"
    ) || stem
        .strip_prefix("import-")
        .and_then(|rest| rest.strip_suffix(".16k"))
        .is_some_and(|n| n.len() == 2 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// What a meeting-folder entry is, if it's scratch an older build left.
fn classify_legacy(name: &str, is_dir: bool, is_file: bool) -> Option<Legacy> {
    if is_dir {
        return (name == LIVE_TMP_DIR).then_some(Legacy::LiveTmp);
    }
    if !is_file {
        return None; // links are never followed
    }
    // (fly-asr names its checkpoint temp `with_extension("asrpart.json.tmp")`
    // on the checkpoint path, which doubles the `.asrpart`)
    [
        (".asrpart.asrpart.json.tmp", Legacy::CheckpointTmp),
        (".asrpart.json.tmp", Legacy::CheckpointTmp),
        (".asrpart.json", Legacy::Checkpoint),
        (".wav", Legacy::Intermediate),
    ]
    .into_iter()
    .find_map(|(suffix, kind)| {
        name.strip_suffix(suffix)
            .filter(|stem| is_pipeline_stem(stem))
            .map(|_| kind)
    })
}

/// Whether a plain file name is one of the 16 kHz intermediates older
/// builds wrote (the v2 migration's sweep uses this too).
pub(crate) fn is_legacy_intermediate_name(name: &str) -> bool {
    classify_legacy(name, false, true) == Some(Legacy::Intermediate)
}

/// Scratch an older build left directly inside a meeting folder.
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
        if let Some(kind) = classify_legacy(&name, ft.is_dir(), ft.is_file()) {
            found.push((entry.path(), kind));
        }
    }
    found
}

/// Whether a WAV is in the pipeline's own intermediate format (RIFF/WAVE,
/// `fmt ` first, PCM, mono, 16 000 Hz, 16-bit — what `fly-audio` and the
/// import's ffmpeg conversion write). The second check before a legacy
/// intermediate is deleted: a user's import with one of the exact names but
/// any other format is kept, and anything unreadable is kept.
fn is_pipeline_wav(path: &Path) -> bool {
    use std::io::Read;
    let mut h = [0u8; 36];
    let read = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut h));
    if read.is_err() {
        return false;
    }
    let u16_at = |i: usize| u16::from_le_bytes([h[i], h[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    &h[0..4] == b"RIFF"
        && &h[8..12] == b"WAVE"
        && &h[12..16] == b"fmt "
        && u16_at(20) == 1 // PCM
        && u16_at(22) == 1 // mono
        && u32_at(24) == 16_000
        && u16_at(34) == 16
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
                    Legacy::Intermediate if !is_pipeline_wav(&path) => continue,
                    Legacy::Intermediate | Legacy::Checkpoint | Legacy::CheckpointTmp => {
                        std::fs::remove_file(&path)
                    }
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

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// An old-build meeting folder: real recording + transcript, the
    /// pipeline's leftovers, and the live-caption folder.
    fn legacy_folder(tmp: &Path) -> PathBuf {
        let folder = tmp.join("recordings").join("2026-07-18 Import");
        std::fs::create_dir_all(folder.join(LIVE_TMP_DIR)).unwrap();
        for f in [
            "recording.mixed.wav",
            "track.16k.wav",
            "track.16k.asrpart.json",
            "track.16k.asrpart.asrpart.json.tmp",
            "transcript.md",
        ] {
            std::fs::write(folder.join(f), f).unwrap();
        }
        std::fs::write(folder.join(LIVE_TMP_DIR).join("you-0.wav"), b"x").unwrap();
        folder
    }

    /// Pipeline start: the old checkpoint moves to scratch (the retry
    /// resumes); nothing else in the meeting folder is touched.
    #[test]
    fn prepare_adopts_old_checkpoints_and_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = legacy_folder(tmp.path());

        let dir = prepare_meeting_scratch(tmp.path(), "m1", Some(&folder)).unwrap();
        assert_eq!(dir, meeting_scratch_dir(tmp.path(), "m1"));
        assert_eq!(
            std::fs::read_to_string(dir.join("track.16k.asrpart.json")).unwrap(),
            "track.16k.asrpart.json",
            "checkpoint adopted so the retry resumes"
        );
        assert_eq!(
            names_in(&folder),
            vec![
                LIVE_TMP_DIR,
                "recording.mixed.wav",
                "track.16k.asrpart.asrpart.json.tmp",
                "track.16k.wav",
                "transcript.md",
            ]
        );
    }

    /// The 07-19 loss must not come back through adoption: a checkpoint
    /// that can't be moved (full disk across volumes, …) or that scratch
    /// already has stays in the meeting folder, byte for byte.
    #[test]
    fn a_checkpoint_that_cant_be_moved_is_never_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = legacy_folder(tmp.path());
        let legacy_ckpt = folder.join("track.16k.asrpart.json");

        let dir = prepare_meeting_scratch_with(tmp.path(), "m1", Some(&folder), |_, _| {
            Err(io::Error::other("simulated: disk full"))
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&legacy_ckpt).unwrap(),
            "track.16k.asrpart.json"
        );
        assert!(!dir.join("track.16k.asrpart.json").exists());

        // scratch already holds one by that name → the old one stays too
        std::fs::write(dir.join("track.16k.asrpart.json"), "newer run").unwrap();
        prepare_meeting_scratch(tmp.path(), "m1", Some(&folder)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&legacy_ckpt).unwrap(),
            "track.16k.asrpart.json"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("track.16k.asrpart.json")).unwrap(),
            "newer run"
        );

        // a copy that dies halfway leaves no partial file in scratch
        let to = tmp.path().join("partial.json");
        let missing_src = tmp.path().join("gone.json");
        assert!(move_file(&missing_src, &to).is_err());
        assert!(!to.exists());
    }

    /// Only the exact names older builds wrote are scratch. Media import
    /// keeps the user's file names, so `Board call.16k.wav` is the user's.
    #[test]
    fn only_exact_pipeline_names_count_as_scratch() {
        let file = |n: &str| classify_legacy(n, false, true);
        for n in [
            "mic.16k.wav",
            "system.16k.wav",
            "track.16k.wav",
            "rediarize.16k.wav",
            "import-00.16k.wav",
            "import-17.16k.wav",
        ] {
            assert_eq!(file(n), Some(Legacy::Intermediate), "{n}");
        }
        assert_eq!(file("mic.16k.asrpart.json"), Some(Legacy::Checkpoint));
        assert_eq!(
            file("track.16k.asrpart.json.tmp"),
            Some(Legacy::CheckpointTmp)
        );
        assert_eq!(
            file("track.16k.asrpart.asrpart.json.tmp"),
            Some(Legacy::CheckpointTmp)
        );
        for n in [
            "Board call.16k.wav",
            "x.16k.wav",
            "import-1.16k.wav",
            "import-001.16k.wav",
            "import-ab.16k.wav",
            "track.16k (2).wav",
            "my track.16k.wav",
            "recording.mixed.wav",
            "notes.asrpart.json",
            "track.wav",
        ] {
            assert_eq!(file(n), None, "{n}");
        }
        assert_eq!(
            classify_legacy(LIVE_TMP_DIR, true, false),
            Some(Legacy::LiveTmp)
        );
        assert_eq!(classify_legacy("track.16k.wav", false, false), None, "link");
        assert_eq!(classify_legacy("other-dir", true, false), None);
    }

    /// The format check behind deleting an intermediate.
    #[test]
    fn pipeline_wav_check_reads_the_header() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = |name: &str, rate: u32, channels: u16| {
            let p = tmp.path().join(name);
            std::fs::write(&p, test_wav(rate, channels)).unwrap();
            p
        };
        assert!(is_pipeline_wav(&wav("ours.wav", 16_000, 1)));
        assert!(!is_pipeline_wav(&wav("stereo.wav", 16_000, 2)));
        assert!(!is_pipeline_wav(&wav("cd.wav", 44_100, 1)));
        let junk = tmp.path().join("junk.wav");
        std::fs::write(&junk, b"RIFF").unwrap();
        assert!(!is_pipeline_wav(&junk), "too short to be ours");
        assert!(!is_pipeline_wav(&tmp.path().join("missing.wav")));
    }

    /// A canonical 44-byte PCM 16-bit WAV header (no samples).
    fn test_wav(rate: u32, channels: u16) -> Vec<u8> {
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
}
