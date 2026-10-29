//! Recording storage v2: finished recordings are kept as lossless FLAC.
//!
//! Capture writes WAV (crash-safe while recording; the capture path is
//! unchanged). Once a meeting's transcript is final, a low-priority
//! background job ("Compress recordings") re-stores it:
//!
//! | File | Role | After compression |
//! |---|---|---|
//! | `recording.mic.wav`, `recording.system.wav` | the sources | `.flac`, same rate and bit depth, verified sample for sample |
//! | `recording.mixed.wav` | 16 kHz ASR mixdown, derived | dropped when it regenerates bit-identically from the sources, else `.flac` (imports: it IS the source) |
//! | `recording.playback.wav` | native-rate mix, derived | same rule; a mic-only capture's playback is the mic file itself |
//!
//! Every reader accepts WAV or FLAC ([`fly_audio::mix::read_audio_mono`]).
//! A recording whose sources are FLAC and whose mixdown/playback paths are
//! absent is "storage v2": readers regenerate those on demand
//! ([`regenerate_mixdown_for_asr`], [`prepare_playback`]), bit-identical to
//! what capture wrote, so transcripts and diarization don't change.
//!
//! Commit protocol per meeting (crash-safe and resumable at every step):
//! 1. encode to `.<name>.compressing.flac`, fsync, decode it again with an
//!    independent decoder and compare every sample with the WAV, copy the
//!    WAV's modified time (retention ages audio by it), rename into place;
//! 2. prove each derived file regenerates bit-identically from the new FLACs
//!    (else it is converted like a source);
//! 3. under the storage lock, re-check nothing started using the meeting,
//!    write the manifest, then `recording_json`
//!    ([`fly_storage::Storage::replace_meeting_recording`]);
//! 4. delete the old WAVs. A WAV left behind by a crash after step 3 is
//!    deleted on a later pass, again only after re-verifying it.
//!
//! Only files directly inside the meeting's own folder are ever touched; a
//! meeting with any recording path elsewhere is left as it is. If the
//! meeting or its audio was deleted while the job worked, the FLACs it made
//! are removed again instead of committed.
//!
//! The job follows the shared background rule
//! ([`AppState::background_yield_reason`], like the embedding worker): it
//! asks before each meeting and every [`IN_FLIGHT_CHECK`] while a file is
//! encoded or verified. When something needs the machine (a recording, a
//! transcription stage, a foreground AI request) it abandons the step in
//! flight (temp file deleted, WAVs untouched), waits, and takes the meeting
//! again, reusing FLACs it already verified.
//!
//! Lock order: `storage`, then `pipeline_stage` or this module's own
//! locks. Never take `recording` or `screen` while holding `storage`:
//! starting a recording holds `recording` and then takes `storage`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use fly_audio::derive::DerivedKind;
use fly_core::RecordingRef;
use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::state::{AppState, BackgroundYield};

/// Event carrying [`CompressStatus`] whenever the job's state changes.
pub const COMPRESS_EVENT: &str = "storage:compress";
/// Cumulative bytes freed by compression (internal; not webview-settable).
pub const SAVED_BYTES_KEY: &str = "storage.compress.saved_bytes";
/// `pending` once a pass has freed space and the one-time notice should
/// show; `shown` after the user dismissed it.
pub const NOTICE_KEY: &str = "storage.compress.notice";

/// Wait after launch before the first pass (startup work, self-heal first).
const START_DELAY: Duration = Duration::from_secs(120);
/// Re-scan at least this often even without a nudge.
const IDLE_RECHECK: Duration = Duration::from_secs(30 * 60);
/// How often a paused job re-checks whether it may continue.
const PAUSE_POLL: Duration = Duration::from_millis(500);
/// A meeting whose playback was requested this recently is left alone: the
/// player may be streaming a file compression would delete.
const PLAYBACK_GRACE: Duration = Duration::from_secs(30 * 60);
/// Regenerated playback mixes kept in scratch, newest first, up to this.
const PLAYBACK_CACHE_BUDGET: u64 = 1536 * 1024 * 1024;
/// File name of a regenerated playback mix inside a meeting's scratch dir.
const PLAYBACK_CACHE_FILE: &str = "playback.wav";
/// Temp-file infix: `.recording.mic.compressing.flac`. Keeps a media
/// extension so retention and "Delete all my data" treat strays as audio.
const TEMP_INFIX: &str = ".compressing";

// ---------------------------------------------------------------------------
// Reading a recording, WAV or FLAC
// ---------------------------------------------------------------------------

fn has_ext(p: &str, ext: &str) -> bool {
    Path::new(p)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Storage v2: a source is FLAC, so absent derived files (the mixdown, the
/// playback mix) regenerate from the sources instead of being stored.
pub fn is_storage_v2(rec: &RecordingRef) -> bool {
    [&rec.mic_path, &rec.system_path]
        .into_iter()
        .any(|p| p.as_deref().is_some_and(|p| has_ext(p, "flac")))
}

/// The single-track ASR/diarization input of a storage-v2 recording whose
/// stored mixdown was dropped: regenerate it into `work_dir`, bit-identical
/// to the `recording.mixed.wav` capture wrote. `Ok(None)` when the stored
/// mixdown exists or the recording isn't storage v2, so callers keep their
/// usual fallback order.
pub fn regenerate_mixdown_for_asr(
    rec: &RecordingRef,
    mixed: Option<&Path>,
    mic: Option<&Path>,
    system: Option<&Path>,
    work_dir: &Path,
) -> Result<Option<PathBuf>, String> {
    if mixed.is_some() || !is_storage_v2(rec) || (mic.is_none() && system.is_none()) {
        return Ok(None);
    }
    let out = work_dir.join("mixdown.16k.wav");
    fly_audio::derive::regenerate(DerivedKind::Mixdown, mic, system, &out, &mut || true)
        .map_err(|e| format!("couldn't rebuild the recording's mixdown: {e}"))?;
    Ok(Some(out))
}

// ---------------------------------------------------------------------------
// Job state
// ---------------------------------------------------------------------------

/// What Settings › Storage shows about the job.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct CompressStatus {
    /// `waiting` (not started yet), `working`, `paused`, `done`, `stopped`.
    pub state: String,
    /// Why it's paused: `recording`, `transcribing`, `ai` (a foreground AI
    /// request: Ask, Enhance, polish, item extraction).
    pub paused_for: Option<String>,
    /// Meetings in this pass that had something to compress, and how many
    /// of them are finished.
    pub meetings_total: usize,
    pub meetings_done: usize,
    /// Of the finished ones: left for a later pass (in use, just played,
    /// its audio deleted meanwhile), and failed (originals kept; the latest
    /// reason is `last_error`). This pass only.
    pub meetings_skipped: usize,
    pub meetings_failed: usize,
    /// WAV bytes this pass will process, and how many it has.
    pub bytes_total: u64,
    pub bytes_done: u64,
    /// Freed so far, across every pass.
    pub saved_bytes: u64,
    /// WAV files left as they are because they aren't in the layout capture
    /// writes (e.g. a recording cut short by a crash). Never deleted.
    pub kept_files: usize,
    /// The one-time "recordings compressed" notice is due.
    pub notice_pending: bool,
    pub last_error: Option<String>,
}

/// Lives in [`AppState::audio_store`].
#[derive(Default)]
pub struct StoreState {
    status: Mutex<CompressStatus>,
    /// Meeting the job is working on.
    current: Mutex<Option<String>>,
    /// Meetings whose compression must stop (note deleted, playback opened).
    cancel: Mutex<HashSet<String>>,
    /// Stop for good ("Delete all my data" is about to run).
    halted: AtomicBool,
    wake: (Mutex<bool>, Condvar),
    recent_playback: Mutex<HashMap<String, Instant>>,
    /// One playback regeneration at a time.
    playback_lock: tokio::sync::Mutex<()>,
    /// Leftover WAVs that didn't match what's stored, with the size and
    /// modified time they had: not decoded again every pass until they
    /// change (in memory; a restart tries each once more).
    unproven: Mutex<HashMap<PathBuf, FileStamp>>,
}

/// Size and modified time: whether a file changed since it was last looked at.
type FileStamp = (u64, Option<std::time::SystemTime>);

fn stamp(p: &Path) -> Option<FileStamp> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()))
}

impl StoreState {
    /// Wake the job (a transcription finished).
    pub fn nudge(&self) {
        let (m, c) = &self.wake;
        *m.lock().unwrap() = true;
        c.notify_all();
    }

    fn sleep(&self, d: Duration) {
        let (m, c) = &self.wake;
        let g = m.lock().unwrap();
        let (mut g, _) = c.wait_timeout_while(g, d, |woken| !*woken).unwrap();
        *g = false;
    }

    fn cancelled(&self, id: &str) -> bool {
        self.halted.load(Ordering::SeqCst) || self.cancel.lock().unwrap().contains(id)
    }

    pub fn status(&self) -> CompressStatus {
        self.status.lock().unwrap().clone()
    }

    /// The meeting being compressed right now, if any.
    pub fn current(&self) -> Option<String> {
        self.current.lock().unwrap().clone()
    }

    fn recently_played(&self, id: &str) -> bool {
        self.recent_playback
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|t| t.elapsed() < PLAYBACK_GRACE)
    }

    /// A leftover already found not to match, and unchanged since.
    fn known_unproven(&self, p: &Path) -> bool {
        let known = self.unproven.lock().unwrap().get(p).copied();
        known.is_some() && known == stamp(p)
    }

    fn mark_unproven(&self, p: &Path) {
        if let Some(s) = stamp(p) {
            self.unproven.lock().unwrap().insert(p.to_path_buf(), s);
        }
    }
}

/// Stop compressing `meeting_id` and wait (up to `timeout`) until the job
/// has let go of its folder. Used before a note's meetings are purged.
pub fn stop_meeting(state: &AppState, meeting_id: &str, timeout: Duration) -> bool {
    let store = &state.audio_store;
    store.cancel.lock().unwrap().insert(meeting_id.to_string());
    let deadline = Instant::now() + timeout;
    while store.current().as_deref() == Some(meeting_id) {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// Stop the job and wait until it's idle. It stays stopped until
/// [`resume`] ("Delete all my data" halts it first and resumes it if the
/// wipe is refused or fails; after a wipe the app restarts).
pub fn halt(state: &AppState, timeout: Duration) -> bool {
    let store = &state.audio_store;
    store.halted.store(true, Ordering::SeqCst);
    store.nudge();
    let deadline = Instant::now() + timeout;
    while store.current().is_some() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// Undo [`halt`]: the job picks up again with a fresh pass.
pub fn resume(state: &AppState) {
    state.audio_store.halted.store(false, Ordering::SeqCst);
    state.audio_store.nudge();
}

/// While a file is encoded or verified, how often the job asks the shared
/// rule whether to step aside (the embedding worker's cadence).
const IN_FLIGHT_CHECK: Duration = Duration::from_millis(500);

/// Why the job is paused, as Settings names it: the shared background rule
/// ([`AppState::background_yield_reason`]). The video job uses it too.
pub(crate) fn paused_for(state: &AppState) -> Option<&'static str> {
    state.background_yield_reason().map(|why| match why {
        BackgroundYield::Recording => "recording",
        BackgroundYield::Transcription => "transcribing",
        BackgroundYield::ForegroundAi => "ai",
    })
}

fn emit(app: &tauri::AppHandle, store: &StoreState, f: impl FnOnce(&mut CompressStatus)) {
    let snapshot = {
        let mut s = store.status.lock().unwrap();
        let before = s.clone();
        f(&mut s);
        if *s == before {
            return;
        }
        s.clone()
    };
    let _ = app.emit(COMPRESS_EVENT, snapshot);
}

// ---------------------------------------------------------------------------
// One meeting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Mic,
    System,
    Mixed,
    Playback,
}

const ROLES: [Role; 4] = [Role::Mic, Role::System, Role::Mixed, Role::Playback];

impl Role {
    fn path(self, rec: &RecordingRef) -> &Option<String> {
        match self {
            Role::Mic => &rec.mic_path,
            Role::System => &rec.system_path,
            Role::Mixed => &rec.mixed_path,
            Role::Playback => &rec.playback_path,
        }
    }

    fn path_mut(self, rec: &mut RecordingRef) -> &mut Option<String> {
        match self {
            Role::Mic => &mut rec.mic_path,
            Role::System => &mut rec.system_path,
            Role::Mixed => &mut rec.mixed_path,
            Role::Playback => &mut rec.playback_path,
        }
    }

    fn derived(self) -> Option<DerivedKind> {
        match self {
            Role::Mic | Role::System => None,
            Role::Mixed => Some(DerivedKind::Mixdown),
            Role::Playback => Some(DerivedKind::Playback),
        }
    }

    /// The name capture gives this role (`recording.<role>.wav`).
    fn wav_name(self) -> &'static str {
        match self {
            Role::Mic => "recording.mic.wav",
            Role::System => "recording.system.wav",
            Role::Mixed => "recording.mixed.wav",
            Role::Playback => "recording.playback.wav",
        }
    }
}

/// `a/b/recording.mic.wav` → `a/b/recording.mic.flac` (also for the
/// data-dir-relative `/`-separated form).
fn flac_twin(rel: &str) -> String {
    let stem = rel
        .strip_suffix(".wav")
        .or_else(|| rel.strip_suffix(".WAV"));
    format!("{}.flac", stem.unwrap_or(rel))
}

fn temp_for(final_flac: &Path) -> PathBuf {
    let name = final_flac
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name.strip_suffix(".flac").unwrap_or(&name);
    final_flac.with_file_name(format!(".{stem}{TEMP_INFIX}.flac"))
}

/// What compressing one meeting did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MeetingOutcome {
    /// WAV bytes removed.
    pub wav_bytes_removed: u64,
    /// FLAC bytes added.
    pub flac_bytes_added: u64,
    /// WAV files left as they are (unsupported layout), with the reason.
    pub kept: Vec<String>,
    /// Bytes of WAV processed (for progress).
    pub wav_bytes_seen: u64,
    /// Leftover WAVs that didn't match what's stored (kept; see
    /// `MeetingCtx::skip_leftover`).
    pub unproven: Vec<PathBuf>,
}

impl MeetingOutcome {
    pub fn saved(&self) -> u64 {
        self.wav_bytes_removed.saturating_sub(self.flac_bytes_added)
    }
}

#[derive(Debug, PartialEq)]
pub enum MeetingError {
    /// Something started using the meeting, or it changed underneath; try
    /// again on a later pass. Nothing was deleted.
    Busy(String),
    /// Asked to stop (cancel, halt). Nothing was deleted.
    Stopped,
    /// The meeting, its recording or its audio was deleted while the job
    /// worked: the FLACs it made were removed again, nothing committed.
    Gone(String),
    Failed(String),
}

impl std::fmt::Display for MeetingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeetingError::Busy(why) => write!(f, "busy: {why}"),
            MeetingError::Stopped => write!(f, "stopped"),
            MeetingError::Gone(why) => write!(f, "gone: {why}"),
            MeetingError::Failed(why) => write!(f, "{why}"),
        }
    }
}

fn failed(e: impl std::fmt::Display) -> MeetingError {
    MeetingError::Failed(e.to_string())
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn fsync_dir(_dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(_dir) {
        let _ = d.sync_all();
    }
}

/// Room kept free on the disk on top of what an encode needs.
const FREE_SPACE_MARGIN: u64 = 256 * 1024 * 1024;

fn under_mount(path: &Path, mount: &Path) -> bool {
    if cfg!(windows) {
        // drive letters and folder names compare without case there
        let (p, m) = (
            path.to_string_lossy().to_lowercase(),
            mount.to_string_lossy().to_lowercase(),
        );
        p.starts_with(&m)
    } else {
        path.starts_with(mount)
    }
}

/// Free bytes on the disk holding `path` (the longest matching mount
/// point). `None` when it can't be told; the job then doesn't wait for it.
pub fn free_space(path: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| under_mount(path, d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

/// `Failed` (shown in Settings, originals kept) when the disk holding `dir`
/// can't take `need` more bytes plus a margin.
fn ensure_room(
    dir: &Path,
    need: u64,
    free_space: &dyn Fn(&Path) -> Option<u64>,
) -> Result<(), MeetingError> {
    let want = need.saturating_add(FREE_SPACE_MARGIN);
    match free_space(dir) {
        Some(free) if free < want => Err(failed(format!(
            "not enough free disk space to compress recordings ({} MB free, about {} MB needed)",
            free / 1_000_000,
            want / 1_000_000
        ))),
        _ => Ok(()),
    }
}

/// Encode `wav` → verified `final_flac` (via the temp name). An existing
/// `final_flac` (an interrupted earlier run renamed it but never committed)
/// is reused only if it verifies against the WAV; otherwise it's rebuilt.
fn convert_one(
    wav: &Path,
    final_flac: &Path,
    checkpoint: &mut dyn FnMut() -> bool,
    free_space: &dyn Fn(&Path) -> Option<u64>,
) -> Result<u64, MeetingError> {
    let stop = |e: fly_audio::flac::FlacError| match e {
        fly_audio::flac::FlacError::Interrupted => MeetingError::Stopped,
        e => failed(e),
    };
    if final_flac.exists() {
        match fly_audio::flac::verify_flac_matches_wav(final_flac, wav, checkpoint) {
            Ok(_) => return Ok(std::fs::metadata(final_flac).map_err(failed)?.len()),
            Err(fly_audio::flac::FlacError::Interrupted) => return Err(MeetingError::Stopped),
            Err(e) => {
                tracing::warn!(error = %e, "compress: replacing a FLAC that doesn't match its WAV");
                std::fs::remove_file(final_flac).map_err(failed)?;
            }
        }
    }
    // A FLAC of 16-bit audio is never meaningfully bigger than its WAV.
    let need = std::fs::metadata(wav).map(|m| m.len()).unwrap_or(0);
    ensure_room(final_flac.parent().unwrap_or(final_flac), need, free_space)?;
    let temp = temp_for(final_flac);
    let result = (|| {
        fly_audio::flac::encode_pcm16_wav_to_flac(wav, &temp, checkpoint).map_err(stop)?;
        fly_audio::flac::verify_flac_matches_wav(&temp, wav, checkpoint).map_err(stop)?;
        // Retention ages audio by file time: the FLAC inherits the WAV's.
        if let Some(t) = mtime(wav) {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(&temp)
                .map_err(failed)?;
            f.set_modified(t).map_err(failed)?;
            f.sync_all().map_err(failed)?;
        }
        fly_storage::rename_with_retry(&temp, final_flac).map_err(failed)?;
        if let Some(dir) = final_flac.parent() {
            fsync_dir(dir);
        }
        std::fs::metadata(final_flac)
            .map(|m| m.len())
            .map_err(failed)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Hooks the job gives one meeting's compression: may it continue (pauses
/// inside), and the commit-time check run under the storage lock.
pub struct MeetingCtx<'a> {
    pub data_dir: &'a Path,
    pub checkpoint: &'a mut dyn FnMut() -> bool,
    /// `Err(reason)` when the meeting may not be committed right now. Runs
    /// with the storage lock held: it must not take `recording`/`screen`.
    pub may_commit: &'a dyn Fn(&fly_storage::Storage) -> Result<(), String>,
    pub storage: &'a Mutex<fly_storage::Storage>,
    pub on_file_done: &'a mut dyn FnMut(u64),
    /// Leftover WAVs not to decode again (found not to match before).
    pub skip_leftover: &'a dyn Fn(&Path) -> bool,
    /// Free bytes on the disk holding a path ([`free_space`]).
    pub free_space: &'a dyn Fn(&Path) -> Option<u64>,
}

/// The meeting's folder, data-dir relative and absolute (contained in
/// `recordings/`).
fn meeting_folder(data_dir: &Path, rec: &RecordingRef) -> Option<(String, PathBuf)> {
    let rel = fly_storage::recording_dir_rel(rec)?;
    let dir = fly_storage::contained_recordings_dir(data_dir, &rel)?;
    Some((rel, dir))
}

/// Every path in `rec` names a file directly inside the folder `rel`.
fn all_in_folder(data_dir: &Path, rel: &str, rec: &RecordingRef) -> bool {
    ROLES
        .iter()
        .filter_map(|r| r.path(rec).as_deref())
        .all(|p| fly_storage::recording_file_in_folder(data_dir, rel, p).is_some())
}

/// The WAV capture wrote for `role`, still in the folder though `rec` no
/// longer references it, when there's something to prove it against: the
/// role's FLAC twin (a converted source, or an import's mixed track), or,
/// for a derived role of a storage-v2 recording, the sources it
/// regenerates from.
fn leftover_candidate(
    data_dir: &Path,
    dir: &Path,
    rec: &RecordingRef,
    role: Role,
) -> Option<PathBuf> {
    let twin = dir.join(role.wav_name());
    if !twin.exists() {
        return None;
    }
    let referenced = ROLES.iter().any(|r| {
        r.path(rec)
            .as_deref()
            .is_some_and(|p| data_dir.join(p) == twin)
    });
    if referenced {
        return None;
    }
    match role.path(rec).as_deref() {
        Some(p) if data_dir.join(p) == dir.join(flac_twin(role.wav_name())) => Some(twin),
        None if role.derived().is_some() && is_storage_v2(rec) => Some(twin),
        _ => None,
    }
}

/// Does this meeting have anything to compress or clean up? Returns
/// `(work, wav_bytes_to_process, wavs_that_stay_wav)`. Cheap: reads
/// directory entries and WAV headers only.
pub fn needs_work(data_dir: &Path, rec: &RecordingRef) -> (bool, u64, usize) {
    needs_work_skipping(data_dir, rec, &|_| false)
}

/// [`needs_work`], not counting leftovers `skip_leftover` names as work.
pub fn needs_work_skipping(
    data_dir: &Path,
    rec: &RecordingRef,
    skip_leftover: &dyn Fn(&Path) -> bool,
) -> (bool, u64, usize) {
    let Some((rel, dir)) = meeting_folder(data_dir, rec) else {
        return (false, 0, 0);
    };
    // A newer build owns this folder's manifest: leave the folder alone.
    if fly_storage::manifest_is_newer(&dir) {
        return (false, 0, 0);
    }
    // A recording path outside its folder: the meeting stays as it is.
    if !all_in_folder(data_dir, &rel, rec) {
        return (false, 0, 1);
    }
    let (mut bytes, mut kept, mut work) = (0, 0, false);
    let mut seen: HashSet<&str> = HashSet::new();
    for role in ROLES {
        if let Some(p) = role.path(rec).as_deref().filter(|p| has_ext(p, "wav")) {
            if !seen.insert(p) {
                continue; // a mic-only capture's playback IS the mic file
            }
            let abs = data_dir.join(p);
            if !abs.exists() {
                continue;
            }
            if fly_audio::flac::inspect_pcm16_wav(&abs).is_ok() {
                work = true;
                bytes += std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
            } else {
                kept += 1;
            }
        } else if let Some(twin) = leftover_candidate(data_dir, &dir, rec, role) {
            // a WAV left behind after an earlier commit
            if skip_leftover(&twin) {
                kept += 1;
            } else {
                work = true;
            }
        }
    }
    (work, bytes, kept)
}

/// Compress one meeting (see the module docs for the protocol).
pub fn compress_meeting(
    ctx: &mut MeetingCtx<'_>,
    meeting_id: &str,
) -> Result<MeetingOutcome, MeetingError> {
    // FLACs this run put in place but never committed. A stop or a refused
    // commit keeps them (the next pass re-verifies and reuses them); a hard
    // failure, or the meeting or its audio being deleted meanwhile, removes
    // them, so no unreferenced copy of the audio lingers. Once the commit
    // starts they may be referenced.
    let mut uncommitted: Vec<PathBuf> = Vec::new();
    let result = compress_meeting_inner(ctx, meeting_id, &mut uncommitted);
    if matches!(result, Err(MeetingError::Failed(_) | MeetingError::Gone(_))) {
        for f in uncommitted {
            let _ = std::fs::remove_file(f);
        }
    }
    result
}

fn compress_meeting_inner(
    ctx: &mut MeetingCtx<'_>,
    meeting_id: &str,
    uncommitted: &mut Vec<PathBuf>,
) -> Result<MeetingOutcome, MeetingError> {
    let planned = {
        let storage = ctx.storage.lock().unwrap();
        storage
            .get_meeting(meeting_id)
            .map_err(failed)?
            .recording
            .ok_or_else(|| MeetingError::Busy("no recording".into()))?
    };
    let data_dir = ctx.data_dir;
    let (rel, dir) = meeting_folder(data_dir, &planned)
        .ok_or_else(|| failed("recording folder isn't inside recordings/"))?;
    if !all_in_folder(data_dir, &rel, &planned) {
        return Err(failed(
            "a recording file is outside its meeting folder; left as it is",
        ));
    }
    if !dir.is_dir() {
        return Err(MeetingError::Busy("recording folder is missing".into()));
    }
    if fly_storage::manifest_is_newer(&dir) {
        return Err(MeetingError::Busy(
            "the folder's manifest is from a newer version of the app".into(),
        ));
    }
    let mut out = MeetingOutcome::default();
    // Only ever a file directly inside this meeting's folder.
    let abs = |p: &Option<String>| -> Option<PathBuf> {
        p.as_deref()
            .and_then(|r| fly_storage::recording_file_in_folder(data_dir, &rel, r))
            .filter(|p| p.exists())
    };

    // Strays from a run that died mid-encode.
    for role in ROLES {
        let stray = temp_for(&dir.join(flac_twin(role.wav_name())));
        if stray.exists() {
            let _ = std::fs::remove_file(&stray);
        }
    }

    // Room for the sources' FLACs (an earlier run's FLAC is already there).
    let need: u64 = [Role::Mic, Role::System]
        .into_iter()
        .filter_map(|role| role.path(&planned).clone())
        .filter(|rel| has_ext(rel, "wav"))
        .filter(|rel| !data_dir.join(flac_twin(rel)).exists())
        .filter_map(|rel| abs(&Some(rel)))
        .filter_map(|wav| std::fs::metadata(wav).ok().map(|m| m.len()))
        .sum();
    ensure_room(&dir, need, ctx.free_space)?;

    let mut next = planned.clone();
    let mut to_delete: Vec<PathBuf> = Vec::new();

    // 1. Sources → FLAC.
    for role in [Role::Mic, Role::System] {
        let Some(rel) = role.path(&planned).clone() else {
            continue;
        };
        if !has_ext(&rel, "wav") {
            continue;
        }
        let Some(wav) = abs(&Some(rel.clone())) else {
            continue;
        };
        let size = std::fs::metadata(&wav).map(|m| m.len()).unwrap_or(0);
        if let Err(e) = fly_audio::flac::inspect_pcm16_wav(&wav) {
            out.kept.push(format!("{}: {e}", role.wav_name()));
            continue;
        }
        let flac_rel = flac_twin(&rel);
        let flac_bytes = convert_one(
            &wav,
            &data_dir.join(&flac_rel),
            ctx.checkpoint,
            ctx.free_space,
        )?;
        uncommitted.push(data_dir.join(&flac_rel));
        out.flac_bytes_added += flac_bytes;
        out.wav_bytes_removed += size;
        out.wav_bytes_seen += size;
        (ctx.on_file_done)(size);
        // A mic-only capture's playback IS the mic file.
        for derived in [Role::Mixed, Role::Playback] {
            if derived.path(&planned).as_deref() == Some(rel.as_str()) {
                *derived.path_mut(&mut next) = Some(flac_rel.clone());
            }
        }
        *role.path_mut(&mut next) = Some(flac_rel);
        to_delete.push(wav);
    }

    // 2. Derived files: drop what regenerates bit-identically from the
    //    sources as they'll be stored, convert the rest.
    let mic_now = abs(&next.mic_path);
    let sys_now = abs(&next.system_path);
    for role in [Role::Mixed, Role::Playback] {
        let Some(rel) = role.path(&planned).clone() else {
            continue;
        };
        if !has_ext(&rel, "wav") || role.path(&next).as_deref() != Some(rel.as_str()) {
            continue; // not a WAV, or it followed a source above
        }
        let Some(wav) = abs(&Some(rel.clone())) else {
            continue;
        };
        let size = std::fs::metadata(&wav).map(|m| m.len()).unwrap_or(0);
        let kind = role.derived().expect("derived role");
        let sources_are_v2 = is_storage_v2(&next);
        let regenerable = sources_are_v2
            && (mic_now.is_some() || sys_now.is_some())
            && fly_audio::derive::matches_stored(
                kind,
                mic_now.as_deref(),
                sys_now.as_deref(),
                &wav,
                ctx.checkpoint,
            )
            .or_else(|e| {
                if ctx_stopped(&e) {
                    Err(MeetingError::Stopped)
                } else {
                    tracing::info!(error = %e, "compress: a stored mix couldn't be compared; keeping it");
                    Ok(false)
                }
            })?;
        if regenerable {
            *role.path_mut(&mut next) = None;
            out.wav_bytes_removed += size;
            out.wav_bytes_seen += size;
            (ctx.on_file_done)(size);
            to_delete.push(wav);
            continue;
        }
        if let Err(e) = fly_audio::flac::inspect_pcm16_wav(&wav) {
            out.kept.push(format!("{}: {e}", role.wav_name()));
            continue;
        }
        let flac_rel = flac_twin(&rel);
        let flac_bytes = convert_one(
            &wav,
            &data_dir.join(&flac_rel),
            ctx.checkpoint,
            ctx.free_space,
        )?;
        uncommitted.push(data_dir.join(&flac_rel));
        out.flac_bytes_added += flac_bytes;
        out.wav_bytes_removed += size;
        out.wav_bytes_seen += size;
        (ctx.on_file_done)(size);
        *role.path_mut(&mut next) = Some(flac_rel);
        to_delete.push(wav);
    }

    // Leftover WAVs from a crash between an earlier commit and its deletes
    // (any layout, imports included): re-prove each before it goes. One
    // that doesn't match stays, and isn't decoded again until it changes.
    let mut leftovers: Vec<PathBuf> = Vec::new();
    for role in ROLES {
        let Some(twin) = leftover_candidate(data_dir, &dir, &next, role) else {
            continue;
        };
        if to_delete.contains(&twin) {
            continue;
        }
        let unmatched = format!(
            "{}: left over from an earlier run and doesn't match the stored copy; kept",
            role.wav_name()
        );
        if (ctx.skip_leftover)(&twin) {
            out.kept.push(unmatched);
            continue;
        }
        let proven = match role.path(&next) {
            // its FLAC twin (a missing one proves nothing: the WAV stays)
            Some(_) => abs(role.path(&next)).is_some_and(|flac| {
                fly_audio::flac::verify_flac_matches_wav(&flac, &twin, ctx.checkpoint).is_ok()
            }),
            // a dropped derived file: what the sources regenerate
            None => role.derived().is_some_and(|kind| {
                fly_audio::derive::matches_stored(
                    kind,
                    mic_now.as_deref(),
                    sys_now.as_deref(),
                    &twin,
                    ctx.checkpoint,
                )
                .unwrap_or(false)
            }),
        };
        if !(ctx.checkpoint)() {
            return Err(MeetingError::Stopped);
        }
        if proven {
            leftovers.push(twin);
        } else {
            tracing::info!(file = %role.wav_name(), "compress: a leftover WAV doesn't match what's stored; kept");
            out.kept.push(unmatched);
            out.unproven.push(twin);
        }
    }

    // 3. Commit (nothing to commit when only leftovers were found).
    {
        let storage = ctx.storage.lock().unwrap();
        let gone = |why: &str| MeetingError::Gone(why.to_string());
        let current = match storage.get_meeting(meeting_id) {
            Ok(m) => m.recording,
            Err(fly_storage::StorageError::NotFound(_)) => {
                return Err(gone("the meeting was deleted"))
            }
            Err(e) => return Err(failed(e)),
        };
        let Some(current) = current else {
            return Err(gone("the recording was removed"));
        };
        if storage
            .get_meeting_privacy(meeting_id)
            .map_err(failed)?
            .audio_deleted_at
            .is_some()
        {
            return Err(gone("its audio was deleted"));
        }
        (ctx.may_commit)(&storage).map_err(MeetingError::Busy)?;
        if current != planned {
            return Err(MeetingError::Busy("the recording changed".into()));
        }
        // From here the manifest may reference the new files.
        uncommitted.clear();
        if next != planned {
            storage
                .replace_meeting_recording(meeting_id, &next)
                .map_err(failed)?;
        }
        // 4. Delete, still under the lock: no reader can pick up an old path
        //    between the commit and the delete.
        for wav in to_delete.iter().chain(&leftovers) {
            let size = std::fs::metadata(wav).map(|m| m.len()).unwrap_or(0);
            match std::fs::remove_file(wav) {
                Ok(()) => {
                    if leftovers.contains(wav) {
                        out.wav_bytes_removed += size;
                    }
                }
                Err(e) => {
                    // Committed already; the next pass removes it after
                    // re-verifying (leftover path). Not counted as saved.
                    tracing::warn!(meeting_id, error = %e, "compress: couldn't delete a replaced WAV yet");
                    if !leftovers.contains(wav) {
                        out.wav_bytes_removed = out.wav_bytes_removed.saturating_sub(size);
                    }
                }
            }
        }
    }
    fsync_dir(&dir);
    Ok(out)
}

fn ctx_stopped(e: &fly_audio::AudioError) -> bool {
    matches!(e, fly_audio::AudioError::Backend(m) if m.contains("stopped before finishing"))
}

// ---------------------------------------------------------------------------
// The background job
// ---------------------------------------------------------------------------

/// Lower this thread's CPU (and, where the OS ties them, I/O) priority:
/// Windows background mode, macOS's background QoS class, a high nice
/// value on Linux (per thread there). Best effort. The video job uses it too.
pub(crate) fn enter_background_priority() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
        };
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
    }
    #[cfg(target_os = "macos")]
    unsafe {
        let _ = libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
    }
    #[cfg(target_os = "linux")]
    unsafe {
        // who = 0 is the calling thread: Linux keeps nice values per thread.
        let _ = libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
}

/// Start the job (once, at app setup). It runs on its own low-priority
/// thread: a first pass shortly after launch, then whenever a transcription
/// finishes (nudge) or every [`IDLE_RECHECK`].
pub fn spawn(app: tauri::AppHandle) {
    {
        let state = app.state::<AppState>();
        let (saved, notice) = {
            let storage = state.storage.lock().unwrap();
            let get = |k: &str| storage.get_setting(k).ok().flatten();
            (
                get(SAVED_BYTES_KEY)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                // a notice due before a restart is still due
                get(NOTICE_KEY).as_deref() == Some("pending"),
            )
        };
        let mut s = state.audio_store.status.lock().unwrap();
        s.state = "waiting".into();
        s.saved_bytes = saved;
        s.notice_pending = notice;
    }
    let spawned = std::thread::Builder::new()
        .name("fotw-compress".into())
        .spawn(move || {
            enter_background_priority();
            let state = app.state::<AppState>();
            state.audio_store.sleep(START_DELAY);
            loop {
                // Halted ("Delete all my data"): idle until resumed.
                if state.audio_store.halted.load(Ordering::SeqCst) {
                    emit(&app, &state.audio_store, |s| s.state = "stopped".into());
                    while state.audio_store.halted.load(Ordering::SeqCst) {
                        state.audio_store.sleep(IDLE_RECHECK);
                    }
                }
                run_pass(&app, &state);
                if !state.audio_store.halted.load(Ordering::SeqCst) {
                    state.audio_store.sleep(IDLE_RECHECK);
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "couldn't start the recording compression job");
    }
}

fn run_pass(app: &tauri::AppHandle, state: &AppState) {
    let store = &state.audio_store;
    let ids = match state.storage.lock().unwrap().meetings_with_stored_audio() {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "compress: couldn't list meetings");
            return;
        }
    };
    // Plan: which meetings have work, and how many bytes.
    let mut plan: Vec<(String, u64)> = Vec::new();
    let mut kept_files = 0usize;
    for id in ids {
        let rec = state
            .storage
            .lock()
            .unwrap()
            .get_meeting(&id)
            .ok()
            .and_then(|m| m.recording);
        if let Some(rec) = rec {
            let (work, bytes, kept) =
                needs_work_skipping(&state.data_dir, &rec, &|p| store.known_unproven(p));
            kept_files += kept;
            if work {
                plan.push((id, bytes));
            }
        }
    }
    store.cancel.lock().unwrap().clear();
    emit(app, store, |s| {
        s.state = if plan.is_empty() { "done" } else { "working" }.into();
        s.paused_for = None;
        s.meetings_total = plan.len();
        s.meetings_done = 0;
        s.meetings_skipped = 0;
        s.meetings_failed = 0;
        s.last_error = None;
        s.bytes_total = plan.iter().map(|(_, b)| b).sum();
        s.bytes_done = 0;
        s.kept_files = kept_files;
    });
    let mut pass_saved = 0u64;
    for (id, _) in &plan {
        if store.halted.load(Ordering::SeqCst) {
            break;
        }
        // Abandoned because something needed the machine: wait it out,
        // then take the same meeting again (its verified FLACs are reused).
        let mut result = compress_one_now(app, state, id);
        while matches!(result, Err(MeetingError::Stopped)) && !store.cancelled(id) {
            result = compress_one_now(app, state, id);
        }
        match result {
            Ok(Some(o)) => {
                pass_saved += o.saved();
                for p in &o.unproven {
                    store.mark_unproven(p);
                }
                let total = {
                    let storage = state.storage.lock().unwrap();
                    let prev: u64 = storage
                        .get_setting(SAVED_BYTES_KEY)
                        .ok()
                        .flatten()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let total = prev + o.saved();
                    let _ = storage.set_setting(SAVED_BYTES_KEY, &total.to_string());
                    total
                };
                tracing::info!(
                    saved = o.saved(),
                    kept = o.kept.len(),
                    "compress: meeting stored losslessly"
                );
                for k in &o.kept {
                    tracing::info!(file = %k, "compress: kept as WAV");
                }
                emit(app, store, |s| {
                    s.saved_bytes = total;
                    s.meetings_done += 1;
                });
            }
            Ok(None) => emit(app, store, |s| s.meetings_done += 1),
            Err(MeetingError::Busy(why)) => {
                tracing::info!(why, "compress: meeting skipped for now");
                emit(app, store, |s| {
                    s.meetings_done += 1;
                    s.meetings_skipped += 1;
                });
            }
            // deleted meanwhile: nothing left to compress later
            Err(MeetingError::Gone(why)) => {
                tracing::info!(why, "compress: meeting gone; its new FLACs were removed");
                emit(app, store, |s| s.meetings_done += 1);
            }
            Err(MeetingError::Stopped) => {
                emit(app, store, |s| {
                    s.meetings_done += 1;
                    s.meetings_skipped += 1;
                });
            }
            Err(MeetingError::Failed(e)) => {
                tracing::warn!(error = %e, "compress: meeting failed (originals kept)");
                emit(app, store, |s| {
                    s.meetings_done += 1;
                    s.meetings_failed += 1;
                    s.last_error = Some(e.clone());
                });
            }
        }
    }
    let notice = pass_saved > 0 && {
        let storage = state.storage.lock().unwrap();
        let seen = storage.get_setting(NOTICE_KEY).ok().flatten();
        if seen.as_deref() == Some("shown") {
            false
        } else {
            let _ = storage.set_setting(NOTICE_KEY, "pending");
            true
        }
    };
    emit(app, store, |s| {
        s.state = if store.halted.load(Ordering::SeqCst) {
            "stopped"
        } else {
            "done"
        }
        .into();
        s.paused_for = None;
        s.bytes_done = s.bytes_total;
        if notice {
            s.notice_pending = true;
        }
    });
}

/// Eligibility + lease + compress for one meeting. `Ok(None)` = skipped.
fn compress_one_now(
    app: &tauri::AppHandle,
    state: &AppState,
    id: &str,
) -> Result<Option<MeetingOutcome>, MeetingError> {
    let store = &state.audio_store;
    // Wait for the machine to be free before even starting.
    if !wait_until_idle(app, state, id) {
        return Err(MeetingError::Stopped);
    }
    // Before the storage lock (lock order: see the module docs). A
    // recording always gets a new meeting, so one being recorded is never
    // in the plan (its `ended_at` is unset); this is belt and braces, and a
    // recording that starts later changes nothing the commit relies on.
    if state
        .recording
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|r| r.meeting_id == id)
    {
        return Err(MeetingError::Busy("being recorded".into()));
    }
    // Runs with the storage lock held (before the lease and at the commit):
    // never `recording` or `screen` in here.
    let blocked = |storage: &fly_storage::Storage| -> Result<(), String> {
        if store.cancelled(id) {
            return Err("cancelled".into());
        }
        if store.recently_played(id) {
            return Err("its recording was just played".into());
        }
        if state.pipeline_stage.lock().unwrap().contains_key(id) {
            return Err("transcription running".into());
        }
        if storage
            .transcription_job(id)
            .map_err(|e| e.to_string())?
            .is_some_and(|j| {
                j.status == fly_storage::JOB_QUEUED || j.status == fly_storage::JOB_RUNNING
            })
        {
            return Err("transcription queued".into());
        }
        if storage
            .get_meeting_privacy(id)
            .map_err(|e| e.to_string())?
            .audio_deleted_at
            .is_some()
        {
            return Err("audio deleted".into());
        }
        Ok(())
    };
    {
        let storage = state.storage.lock().unwrap();
        blocked(&storage).map_err(MeetingError::Busy)?;
        if !storage.lease_meeting_folder(id) {
            return Err(MeetingError::Busy("folder in use".into()));
        }
    }
    *store.current.lock().unwrap() = Some(id.to_string());
    struct Release<'a>(&'a AppState, &'a str);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            *self.0.audio_store.current.lock().unwrap() = None;
            self.0
                .storage
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .release_meeting_folder(self.1);
        }
    }
    let _release = Release(state, id);

    // Something else needs the machine (or this meeting was cancelled):
    // abandon the step in flight. Its temp file goes, the WAVs are
    // untouched, FLACs already verified stay for the retry to reuse.
    let mut checkpoint = in_flight_check(state, id);
    let mut on_file_done = |bytes: u64| emit(app, store, |s| s.bytes_done += bytes);
    let mut ctx = MeetingCtx {
        data_dir: &state.data_dir,
        checkpoint: &mut checkpoint,
        may_commit: &blocked,
        storage: &state.storage,
        on_file_done: &mut on_file_done,
        skip_leftover: &|p| store.known_unproven(p),
        free_space: &free_space,
    };
    compress_meeting(&mut ctx, id).map(Some)
}

/// The checkpoint the encoder and verifier call every few ms while a file
/// is in flight: `false` at once when this meeting is cancelled or the job
/// halted, and when the shared rule says yield. The rule itself is asked at
/// most every [`IN_FLIGHT_CHECK`] (its last answer stands in between).
fn in_flight_check<'a>(state: &'a AppState, id: &'a str) -> impl FnMut() -> bool + 'a {
    let mut last: Option<(Instant, bool)> = None;
    move || {
        if state.audio_store.cancelled(id) {
            return false;
        }
        if let Some((at, go)) = last {
            if at.elapsed() < IN_FLIGHT_CHECK {
                return go;
            }
        }
        let go = !state.background_should_yield();
        last = Some((Instant::now(), go));
        go
    }
}

/// Block (status `paused`) while something else needs the machine. `false`
/// when the job must stop instead (halt, or this meeting was cancelled).
fn wait_until_idle(app: &tauri::AppHandle, state: &AppState, id: &str) -> bool {
    let store = &state.audio_store;
    let mut paused = false;
    loop {
        if store.cancelled(id) {
            return false;
        }
        match paused_for(state) {
            None => {
                if paused {
                    emit(app, store, |s| {
                        s.state = "working".into();
                        s.paused_for = None;
                    });
                }
                return true;
            }
            Some(why) => {
                if !paused {
                    paused = true;
                    emit(app, store, |s| {
                        s.state = "paused".into();
                        s.paused_for = Some(why.into());
                    });
                }
                std::thread::sleep(PAUSE_POLL);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

/// What the editor's player should load.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlaybackSource {
    /// Absolute path, for the asset protocol. `None` until a compressed
    /// recording's playback mix is built (`needs_build`).
    pub path: Option<String>,
    /// Call again with `build: true` (on Play) to rebuild the mix.
    pub needs_build: bool,
}

/// What the player gets for a meeting.
#[derive(Debug, PartialEq)]
enum PlaybackPlan {
    /// A stored WAV (or other webview-playable file), streamed as is.
    Stored(PathBuf),
    /// Rebuild into the scratch cache: a compressed two-channel
    /// recording's mix (`system: Some`), or a FLAC decoded to WAV so the
    /// webview never has to decode FLAC (WKWebView can; webkit2gtk only
    /// with GStreamer's flac plugin).
    Build {
        mic: PathBuf,
        system: Option<PathBuf>,
    },
    Nothing,
}

/// Pick the file to play: the stored playback mix, then the old fallback
/// order (mixdown, mic, system). FLAC always goes through the cache.
fn playback_plan(data_dir: &Path, rec: &RecordingRef) -> PlaybackPlan {
    let abs = |p: &Option<String>| p.as_ref().map(|r| data_dir.join(r)).filter(|p| p.exists());
    let playable = |p: PathBuf| {
        if fly_audio::flac::is_flac(&p) {
            PlaybackPlan::Build {
                mic: p,
                system: None,
            }
        } else {
            PlaybackPlan::Stored(p)
        }
    };
    if let Some(p) = abs(&rec.playback_path) {
        return playable(p);
    }
    let (mic, sys) = (abs(&rec.mic_path), abs(&rec.system_path));
    if is_storage_v2(rec) {
        if let (Some(mic), Some(sys)) = (mic.clone(), sys.clone()) {
            return PlaybackPlan::Build {
                mic,
                system: Some(sys),
            };
        }
    }
    match abs(&rec.mixed_path).or(mic).or(sys) {
        Some(p) => playable(p),
        None => PlaybackPlan::Nothing,
    }
}

fn evict_playback_cache(data_dir: &Path, keep: &Path) {
    let root = fly_storage::scratch::scratch_root(data_dir);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .map(|e| e.path().join(PLAYBACK_CACHE_FILE))
        .filter_map(|p| {
            let m = std::fs::metadata(&p).ok()?;
            Some((m.modified().ok()?, m.len(), p))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut total = 0u64;
    for (_, len, p) in files {
        total += len;
        if total > PLAYBACK_CACHE_BUDGET && p != keep {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// The file the player should stream for a meeting. A compressed
/// two-channel recording has no stored playback mix: with `build: false`
/// (opening the note) a cached rebuild is returned if there is one, else
/// `needs_build`; `build: true` (pressing Play) rebuilds it, a few seconds
/// per half hour of audio. `None` when the meeting has no audio left.
#[tauri::command]
pub async fn prepare_playback(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    build: bool,
) -> Result<Option<PlaybackSource>, String> {
    let store = &state.audio_store;
    store
        .recent_playback
        .lock()
        .unwrap()
        .insert(meeting_id.clone(), Instant::now());
    // The player may stream a WAV the job is about to replace: stop it.
    if store.current().as_deref() == Some(meeting_id.as_str()) {
        store.cancel.lock().unwrap().insert(meeting_id.clone());
    }
    let rec = {
        let storage = state.storage.lock().unwrap();
        let m = storage
            .get_meeting(&meeting_id)
            .map_err(|e| e.to_string())?;
        match m.recording {
            Some(r) => r,
            None => return Ok(None),
        }
    };
    let data_dir = state.data_dir.clone();
    let (mic, sys) = match playback_plan(&data_dir, &rec) {
        PlaybackPlan::Stored(p) => {
            return Ok(Some(PlaybackSource {
                path: Some(p.to_string_lossy().into_owned()),
                needs_build: false,
            }))
        }
        PlaybackPlan::Nothing => return Ok(None),
        PlaybackPlan::Build { mic, system } => (mic, system),
    };
    let cache_dir = fly_storage::scratch::meeting_scratch_dir(&data_dir, &meeting_id);
    let cache = cache_dir.join(PLAYBACK_CACHE_FILE);
    let newest_source = std::iter::once(&mic)
        .chain(sys.as_ref())
        .filter_map(|p| mtime(p))
        .max();
    let is_fresh = || {
        std::fs::metadata(&cache).is_ok_and(|m| m.len() > 44 && m.modified().ok() >= newest_source)
    };
    if !build && !is_fresh() {
        return Ok(Some(PlaybackSource {
            path: None,
            needs_build: true,
        }));
    }
    let _one_at_a_time = store.playback_lock.lock().await;
    if !is_fresh() {
        let (cache_c, dir_c) = (cache.clone(), cache_dir.clone());
        tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            std::fs::create_dir_all(&dir_c).map_err(|e| e.to_string())?;
            let part = dir_c.join("playback.part.wav");
            match &sys {
                // both channels: the mix capture would have stored
                Some(sys) => fly_audio::derive::regenerate(
                    DerivedKind::Playback,
                    Some(&mic),
                    Some(sys),
                    &part,
                    &mut || true,
                )
                .map(|_| ())
                .map_err(|e| e.to_string())?,
                // one FLAC: its own samples, as WAV
                None => fly_audio::flac::decode_to_wav(&mic, &part)
                    .map(|_| ())
                    .map_err(|e| e.to_string())?,
            }
            std::fs::rename(&part, &cache_c).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("couldn't prepare the recording for playback: {e}"))?;
    } else {
        // LRU: a hit counts as fresh use.
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&cache) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
    }
    evict_playback_cache(&data_dir, &cache);
    // Only this file joins the webview's asset scope (recordings/ and
    // attachments/ are allowed at startup; scratch as a whole is not).
    let _ = tauri::Manager::asset_protocol_scope(&app).allow_file(&cache);
    Ok(Some(PlaybackSource {
        path: Some(cache.to_string_lossy().into_owned()),
        needs_build: false,
    }))
}

// ---------------------------------------------------------------------------
// Settings › Storage
// ---------------------------------------------------------------------------

const AUDIO_EXTS: &[&str] = &[
    "wav", "flac", "mp3", "m4a", "aac", "ogg", "oga", "opus", "wma",
];
const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm", "avi"];

/// Disk use of the data folder's media, by kind.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct StorageReport {
    /// Meeting audio under `recordings/` (WAV + FLAC + imported audio).
    pub audio_bytes: u64,
    /// The part of `audio_bytes` still stored as uncompressed WAV.
    pub wav_bytes: u64,
    /// Videos copied in by media import (`recordings/`).
    pub imported_video_bytes: u64,
    pub imported_video_files: usize,
    /// Screen recordings (`attachments/<note>/screen-*.mp4`).
    pub screen_recording_bytes: u64,
    pub screen_recording_files: usize,
    /// Other attachments (files, other videos).
    pub other_attachment_bytes: u64,
    pub compress: CompressStatus,
    /// The opt-in video job (video_store.rs).
    pub videos: crate::video_store::VideoJobStatus,
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Walk `recordings/` and `attachments/` (two levels: folder → files).
pub fn measure(data_dir: &Path) -> StorageReport {
    let mut r = StorageReport::default();
    let files_in = |sub: &str| -> Vec<(PathBuf, u64)> {
        let mut out = Vec::new();
        let Ok(dirs) = std::fs::read_dir(data_dir.join(sub)) else {
            return out;
        };
        for d in dirs.flatten() {
            let Ok(ft) = d.file_type() else { continue };
            if ft.is_file() {
                if let Ok(m) = d.metadata() {
                    out.push((d.path(), m.len()));
                }
                continue;
            }
            if !ft.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(d.path()) else {
                continue;
            };
            for f in files.flatten() {
                if f.file_type().is_ok_and(|t| t.is_file()) {
                    if let Ok(m) = f.metadata() {
                        out.push((f.path(), m.len()));
                    }
                }
            }
        }
        out
    };
    for (p, len) in files_in("recordings") {
        let ext = ext_of(&p);
        if AUDIO_EXTS.contains(&ext.as_str()) {
            r.audio_bytes += len;
            if ext == "wav" {
                r.wav_bytes += len;
            }
        } else if VIDEO_EXTS.contains(&ext.as_str()) {
            r.imported_video_bytes += len;
            r.imported_video_files += 1;
        }
    }
    for (p, len) in files_in("attachments") {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with("screen-") && VIDEO_EXTS.contains(&ext_of(&p).as_str()) {
            r.screen_recording_bytes += len;
            r.screen_recording_files += 1;
        } else {
            r.other_attachment_bytes += len;
        }
    }
    r
}

/// Settings › Storage: sizes by kind plus the compression job's status.
#[tauri::command]
pub async fn storage_report(state: tauri::State<'_, AppState>) -> Result<StorageReport, String> {
    let data_dir = state.data_dir.clone();
    let mut report = tauri::async_runtime::spawn_blocking(move || measure(&data_dir))
        .await
        .map_err(|e| e.to_string())?;
    report.compress = state.audio_store.status();
    report.videos = crate::video_store::status_for(&state);
    Ok(report)
}

/// The one-time "recordings compressed" notice was seen.
#[tauri::command]
pub fn dismiss_compress_notice(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state
        .storage
        .lock()
        .unwrap()
        .set_setting(NOTICE_KEY, "shown")
        .map_err(|e| e.to_string())?;
    state.audio_store.status.lock().unwrap().notice_pending = false;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Delete all my data" halts the job; a refused or failed wipe resumes
    /// it instead of leaving it stopped until the next launch.
    #[test]
    fn a_halted_job_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::init_with(
            dir.path().to_path_buf(),
            std::sync::Arc::new(fly_secrets::MemorySecretStore::default()),
        )
        .unwrap();
        assert!(halt(&state, Duration::from_secs(1)));
        assert!(state.audio_store.cancelled("any"));
        resume(&state);
        assert!(!state.audio_store.cancelled("any"));
        assert!(
            *state.audio_store.wake.0.lock().unwrap(),
            "woken for a pass"
        );
    }

    /// The shared rule (#112) decides; it's asked at most every
    /// IN_FLIGHT_CHECK while a file is in flight, a cancel stops at once.
    #[test]
    fn the_in_flight_check_follows_the_shared_rule_every_half_second() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::init_with(
            dir.path().to_path_buf(),
            std::sync::Arc::new(fly_secrets::MemorySecretStore::default()),
        )
        .unwrap();
        let mut go = in_flight_check(&state, "m1");
        assert!(go());
        assert_eq!(paused_for(&state), None);
        let ai = state.foreground_ai();
        assert_eq!(paused_for(&state), Some("ai"));
        assert!(go(), "the last answer stands inside the window");
        std::thread::sleep(IN_FLIGHT_CHECK + Duration::from_millis(100));
        assert!(!go(), "asked again: a foreground AI request runs");
        drop(ai);
        assert!(!go(), "still the last answer");
        std::thread::sleep(IN_FLIGHT_CHECK + Duration::from_millis(100));
        assert!(go());
        // a pipeline stage (not a queued "waiting" job) also yields
        state
            .pipeline_stage
            .lock()
            .unwrap()
            .insert("m2".into(), crate::scheduler::WAITING_STAGE.into());
        assert_eq!(paused_for(&state), None);
        state
            .pipeline_stage
            .lock()
            .unwrap()
            .insert("m2".into(), "transcribing".into());
        assert_eq!(paused_for(&state), Some("transcribing"));
        state.pipeline_stage.lock().unwrap().clear();
        state.audio_store.cancel.lock().unwrap().insert("m1".into());
        assert!(!go(), "a cancel doesn't wait for the window");
    }

    #[test]
    fn free_space_is_known_for_the_temp_dir_and_blocks_only_when_short() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_space(dir.path()).is_some_and(|b| b > 0));
        let none = |_: &Path| None;
        let short = |_: &Path| Some(FREE_SPACE_MARGIN);
        assert!(
            ensure_room(dir.path(), u64::MAX, &none).is_ok(),
            "unknown never blocks"
        );
        assert!(ensure_room(dir.path(), 1, &short).is_err());
        assert!(ensure_room(dir.path(), 0, &short).is_ok());
    }

    #[test]
    fn the_player_never_gets_a_flac() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let m = data.join("recordings/m");
        std::fs::create_dir_all(&m).unwrap();
        let wav = |n: &str| {
            fly_audio::mix::write_wav_mono_16(&m.join(n), &[0.1, -0.2, 0.3], 16_000).unwrap()
        };
        for n in [
            "recording.mic.wav",
            "recording.system.wav",
            "recording.playback.wav",
        ] {
            wav(n);
        }
        let flac = |n: &str| {
            let src = m.join("tmp.wav");
            fly_audio::mix::write_wav_mono_16(&src, &[0.1, -0.2, 0.3], 16_000).unwrap();
            fly_audio::flac::encode_pcm16_wav_to_flac(&src, &m.join(n), &mut || true).unwrap();
            std::fs::remove_file(src).unwrap();
        };
        for n in [
            "recording.mic.flac",
            "recording.system.flac",
            "recording.mixed.flac",
        ] {
            flac(n);
        }
        let p = |n: &str| Some(format!("recordings/m/{n}"));
        let rec =
            |mic: &str, sys: Option<&str>, mixed: Option<&str>, play: Option<&str>| RecordingRef {
                mic_path: p(mic),
                system_path: sys.and_then(p),
                mixed_path: mixed.and_then(p),
                playback_path: play.and_then(p),
                duration_ms: 0,
            };
        // before compression: the stored WAV mix
        assert_eq!(
            playback_plan(
                data,
                &rec(
                    "recording.mic.wav",
                    Some("recording.system.wav"),
                    None,
                    Some("recording.playback.wav")
                )
            ),
            PlaybackPlan::Stored(m.join("recording.playback.wav"))
        );
        // compressed two-channel: rebuilt mix
        assert_eq!(
            playback_plan(
                data,
                &rec(
                    "recording.mic.flac",
                    Some("recording.system.flac"),
                    None,
                    None
                )
            ),
            PlaybackPlan::Build {
                mic: m.join("recording.mic.flac"),
                system: Some(m.join("recording.system.flac"))
            }
        );
        // compressed mic-only (playback follows the mic): decoded, not streamed
        assert_eq!(
            playback_plan(
                data,
                &rec("recording.mic.flac", None, None, Some("recording.mic.flac"))
            ),
            PlaybackPlan::Build {
                mic: m.join("recording.mic.flac"),
                system: None
            }
        );
        // compressed import (mixed only): decoded
        let import = RecordingRef {
            mic_path: None,
            ..rec("x", None, Some("recording.mixed.flac"), None)
        };
        assert_eq!(
            playback_plan(data, &import),
            PlaybackPlan::Build {
                mic: m.join("recording.mixed.flac"),
                system: None
            }
        );
        // nothing left
        let gone = RecordingRef {
            mic_path: p("missing.wav"),
            ..import.clone()
        };
        assert_eq!(
            playback_plan(
                data,
                &RecordingRef {
                    mixed_path: None,
                    ..gone
                }
            ),
            PlaybackPlan::Nothing
        );
    }

    #[test]
    fn twins_and_temps() {
        assert_eq!(
            flac_twin("recordings/a b/recording.mic.wav"),
            "recordings/a b/recording.mic.flac"
        );
        assert_eq!(
            temp_for(Path::new("x/recording.mic.flac")),
            Path::new("x/.recording.mic.compressing.flac")
        );
        let v2 = RecordingRef {
            mic_path: Some("recordings/a/recording.mic.FLAC".into()),
            system_path: None,
            mixed_path: None,
            playback_path: None,
            duration_ms: 0,
        };
        assert!(is_storage_v2(&v2));
        assert!(!is_storage_v2(&RecordingRef {
            mic_path: Some("recordings/a/recording.mic.wav".into()),
            ..v2
        }));
    }
}
