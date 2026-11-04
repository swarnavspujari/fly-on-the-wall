//! Long-lived app state managed by Tauri.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fly_audio::cpal_backend::CpalAudioCapture;
use fly_audio::AudioCapture;
use fly_secrets::{KeychainSecretStore, SecretStore};
use fly_storage::{OpenRetry, Storage, UpgradeProgress};

use crate::recording::ActiveRecording;
use crate::screen_commands::ActiveScreenRecording;

pub struct AppState {
    /// rusqlite connections are Send but not Sync; all storage access goes
    /// through this mutex. Fine for a single-user desktop app.
    pub storage: Mutex<Storage>,
    pub data_dir: PathBuf,
    /// Platform audio backend — the ONLY place an impl is chosen is here.
    pub audio: Box<dyn AudioCapture>,
    /// At most one capture session at a time.
    pub recording: Mutex<Option<ActiveRecording>>,
    /// OS keychain — every API key/token goes through this, never plaintext.
    pub secrets: Arc<dyn SecretStore>,
    /// meeting_id → current pipeline stage, for running transcriptions.
    pub pipeline_stage: Mutex<HashMap<String, String>>,
    /// Meeting ids whose running pipeline was asked to stop (note deleted,
    /// import cancelled). Engines poll this between batches; the pipeline
    /// clears a meeting's entry when a fresh run starts.
    pub cancel_requests: Mutex<std::collections::HashSet<String>>,
    /// At most one screen capture at a time.
    pub screen: Mutex<Option<ActiveScreenRecording>>,
    /// Nudges the transcription queue worker (job enqueued, recording
    /// stopped). The worker also polls, so a missed nudge only delays it.
    pub jobs_notify: tokio::sync::Notify,
    /// Managed `ollama serve` child (None when a user-run server is used or
    /// the ollama provider isn't active). Killed on app exit (ollama.rs).
    pub ollama: Mutex<Option<std::process::Child>>,
    /// Latest `live:status` per the most recent recording. Events are
    /// fire-and-forget, and the live loop can emit "unavailable" before the
    /// pane has mounted its listener (fast failures at recording start) —
    /// this snapshot lets the pane catch up on mount instead of showing an
    /// optimistic "Listening…" forever.
    pub live_status: Mutex<Option<crate::live::LiveStatus>>,
    /// Meetings whose post-transcription cleanup / item extraction was refused
    /// because the cloud provider has no recorded consent or no API key yet
    /// (`llm_commands::record_consent_skip`).
    /// In memory only: the app's notice offers to run them once the user
    /// allows the provider or adds the key (`llm_commands::retry_consent_skipped`).
    pub consent_skipped: Mutex<std::collections::BTreeSet<String>>,
    /// Meetings whose cleanup / item extraction reached the model and was
    /// stopped short: it took too long, got stuck in a loop, or listed only
    /// part of the meeting (`llm_commands::record_incomplete`). Kept apart
    /// from `consent_skipped` because the notice says something else about
    /// them (some items couldn't be read; the transcript may well be cleaned),
    /// but "Run them now" runs both lists.
    pub ai_incomplete: Mutex<std::collections::BTreeSet<String>>,
    /// Passes (transcript cleanup + item extraction) waiting to run after a
    /// transcription, and the one running. Drained by its own worker so a
    /// slow model never holds up the transcription queue (ai_queue.rs).
    pub ai_queue: crate::ai_queue::AiQueue,
    /// Meetings self-heal restored this launch and set to Keep local because
    /// their manifest didn't record the setting. In memory only: the app's
    /// notice asks the user to review them
    /// (`privacy::recovered_kept_local`).
    pub recovered_kept_local: Mutex<Vec<String>>,
    /// Held while the native cloud-consent dialog is open, so requests show
    /// one dialog at a time, with each provider's recent declines so a
    /// request loop can't keep one up (`llm_commands::request_cloud_consent`).
    pub consent_prompt: tokio::sync::Mutex<crate::llm_commands::ConsentPacing>,
    /// Nudges the embedding worker after a content write (embeddings.rs).
    /// The worker also polls, so a missed nudge only delays indexing.
    pub embed_notify: tokio::sync::Notify,
    /// Recording storage v2: the background "Compress recordings" job's
    /// status and controls, plus playback regeneration (audio_store.rs).
    pub audio_store: crate::audio_store::StoreState,
    /// The opt-in video job: imported videos to audio only, screen
    /// recordings to a visually lossless re-encode (video_store.rs).
    pub video_store: crate::video_store::VideoState,
    /// Foreground AI requests in flight (Ask, Enhance): the embedding
    /// backlog steps aside while any runs. Take it with
    /// [`AppState::foreground_ai`].
    foreground_ai: std::sync::atomic::AtomicUsize,
}

/// What background work (the embedding backlog, audio compression) is
/// stepping aside for, in priority order: live captions (a recording) >
/// transcription and its polish > a user-facing AI request > background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundYield {
    /// An audio or screen recording is running (live captions with it).
    Recording,
    /// A transcription pipeline stage is running (ASR, diarization,
    /// polish). A merely queued job ("waiting") doesn't count.
    Transcription,
    /// A [`ForegroundAi`] guard is alive: a model request is running (Ask,
    /// Enhance, transcript polish, item extraction).
    ForegroundAi,
}

/// Held for the length of a user-facing AI request (see
/// [`AppState::foreground_ai`]); dropping it releases the slot.
pub struct ForegroundAi<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for ForegroundAi<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The running app's version: what the backup step records as the library's
/// last-opened-by and compares against on the next launch.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Open the library the way a launch does: the pre-upgrade backup first (see
/// `fly_storage::backup`), then the open that runs any schema step, then the
/// record of what the backup step did. The backup can't stop the open: every
/// problem in it is an outcome for the notice, never an error here.
/// `on_backup` is called once if a backup is about to be taken.
pub fn open_library(
    data_dir: &Path,
    progress: &mut dyn FnMut(UpgradeProgress),
    on_backup: &mut dyn FnMut(),
) -> anyhow::Result<Storage> {
    open_library_with(
        data_dir,
        APP_VERSION,
        &crate::audio_store::free_space,
        OpenRetry::STARTUP,
        progress,
        on_backup,
    )
}

fn open_library_with(
    data_dir: &Path,
    version: &str,
    free_space: &dyn Fn(&Path) -> Option<u64>,
    retry: OpenRetry,
    progress: &mut dyn FnMut(UpgradeProgress),
    on_backup: &mut dyn FnMut(),
) -> anyhow::Result<Storage> {
    let outcome = fly_storage::backup::before_open(data_dir, version, free_space, on_backup);
    // Retries a transient open failure before it is treated as fatal: twice
    // the startup "database disk image is malformed" error came from an
    // external process interfering at the moment of open and cleared within
    // seconds (see the db-corruption incident). Only after the retries are
    // exhausted does the caller reach the startup error dialog.
    let storage = Storage::open_with_retry_and_progress(data_dir, retry, progress)?;
    fly_storage::backup::after_open(&storage, version, &outcome);
    Ok(storage)
}

impl AppState {
    pub fn init() -> anyhow::Result<Self> {
        let (data_dir, secrets) = Self::prepare()?;
        let storage = open_library(&data_dir, &mut |_| {}, &mut || {})?;
        tracing::info!(dir = %data_dir.display(), "storage ready");
        Ok(Self::with_storage(storage, data_dir, secrets))
    }

    /// [`init`](Self::init) for the background startup open (startup.rs):
    /// the same open, reporting the backup step and the one-time schema
    /// step's progress.
    pub fn init_with_progress(
        progress: &mut dyn FnMut(UpgradeProgress),
        on_backup: &mut dyn FnMut(),
    ) -> anyhow::Result<Self> {
        let (data_dir, secrets) = Self::prepare()?;
        let storage = open_library(&data_dir, progress, on_backup)?;
        tracing::info!(dir = %data_dir.display(), "storage ready");
        Ok(Self::with_storage(storage, data_dir, secrets))
    }

    fn prepare() -> anyhow::Result<(PathBuf, Arc<dyn SecretStore>)> {
        let data_dir = prepared_data_dir()?;
        let secrets = Arc::new(KeychainSecretStore::new());
        // Best-effort: copy secrets out of the pre-rebrand keychain service. A
        // locked or absent keyring must never block launch, and keys can be
        // re-entered, so failures here are swallowed, not fatal.
        copy_secrets(
            &KeychainSecretStore::with_service(LEGACY_KEYCHAIN_SERVICE),
            secrets.as_ref(),
            &migrated_secret_keys(),
        );
        Ok((data_dir, secrets))
    }

    /// Composition with explicit data dir + secret store — used by tests. It
    /// skips the backup step (a launch's [`open_library`] takes it), so a
    /// test's settings table holds only what the test put there.
    pub fn init_with(data_dir: PathBuf, secrets: Arc<dyn SecretStore>) -> anyhow::Result<Self> {
        let storage = Storage::open_with_retry(&data_dir, OpenRetry::STARTUP)?;
        tracing::info!(dir = %data_dir.display(), "storage ready");
        Ok(Self::with_storage(storage, data_dir, secrets))
    }

    fn with_storage(storage: Storage, data_dir: PathBuf, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            storage: Mutex::new(storage),
            data_dir,
            audio: Box::new(CpalAudioCapture::new()),
            recording: Mutex::new(None),
            secrets,
            pipeline_stage: Mutex::new(HashMap::new()),
            cancel_requests: Mutex::new(std::collections::HashSet::new()),
            screen: Mutex::new(None),
            jobs_notify: tokio::sync::Notify::new(),
            ollama: Mutex::new(None),
            live_status: Mutex::new(None),
            consent_skipped: Mutex::new(std::collections::BTreeSet::new()),
            ai_incomplete: Mutex::new(std::collections::BTreeSet::new()),
            ai_queue: Default::default(),
            recovered_kept_local: Mutex::new(Vec::new()),
            consent_prompt: tokio::sync::Mutex::new(Default::default()),
            embed_notify: tokio::sync::Notify::new(),
            audio_store: Default::default(),
            video_store: Default::default(),
            foreground_ai: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Mark a model request (Ask, Enhance, polish, extraction) as running until the
    /// guard drops: background work that competes for the same model
    /// runtime (the embedding backlog) waits meanwhile.
    pub fn foreground_ai(&self) -> ForegroundAi<'_> {
        self.foreground_ai
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ForegroundAi(&self.foreground_ai)
    }

    /// Whether any [`foreground_ai`](Self::foreground_ai) guard is alive.
    pub fn foreground_ai_active(&self) -> bool {
        self.foreground_ai.load(std::sync::atomic::Ordering::SeqCst) > 0
    }

    /// Why background work should step aside right now, if it should (see
    /// [`BackgroundYield`]). The one rule every background job follows: the
    /// embedding backlog and audio compression both call it, between units
    /// of work and while a unit is in flight.
    pub fn background_yield_reason(&self) -> Option<BackgroundYield> {
        if crate::scheduler::recording_active(self) {
            return Some(BackgroundYield::Recording);
        }
        let transcribing = self
            .pipeline_stage
            .lock()
            .unwrap()
            .values()
            .any(|stage| stage != crate::scheduler::WAITING_STAGE);
        if transcribing {
            return Some(BackgroundYield::Transcription);
        }
        if self.foreground_ai_active() {
            return Some(BackgroundYield::ForegroundAi);
        }
        None
    }

    /// Whether background work should step aside right now:
    /// [`background_yield_reason`](Self::background_yield_reason)`.is_some()`.
    pub fn background_should_yield(&self) -> bool {
        self.background_yield_reason().is_some()
    }

    /// Shared handle for components that hold the secret store themselves
    /// (e.g. calendar providers refreshing tokens).
    pub fn secrets_arc(&self) -> Arc<dyn SecretStore> {
        self.secrets.clone()
    }
}

/// Pre-rebrand identifiers, kept only so the one-time migration below can find
/// a user's existing data dir and secrets.
const LEGACY_DATA_DIR: &str = "Looma";
pub(crate) const LEGACY_KEYCHAIN_SERVICE: &str = "com.looma.notetaker";
/// Current data-dir name under the OS data directory.
const DATA_DIR: &str = "FlyOnTheWall";

/// User-visible, portable data directory (spec §10): everything the app stores
/// lives under here.
fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DATA_DIR)
}

/// The data dir with the legacy migration already applied — the ONLY safe
/// way to learn the path before [`AppState::init`]: creating anything inside
/// a fresh `FlyOnTheWall/` first (e.g. `logs/`) would make the migration
/// think it already ran and strand a pre-rebrand user's data. Idempotent.
pub fn prepared_data_dir() -> anyhow::Result<PathBuf> {
    let data_dir = default_data_dir();
    // Move a pre-rebrand data dir into place before the DB is opened.
    migrate_from_legacy_data_dir(&data_dir)?;
    Ok(data_dir)
}

/// Move the pre-rebrand data dir (`<data>/Looma`) into place, once, before the
/// DB is opened (the DB file inside is renamed by `Storage::open`). Propagates
/// errors rather than silently opening a fresh dir over stranded notes.
fn migrate_from_legacy_data_dir(new_dir: &Path) -> anyhow::Result<()> {
    if let Some(base) = dirs::data_dir() {
        move_legacy_dir(&base.join(LEGACY_DATA_DIR), new_dir)?;
    }
    Ok(())
}

/// Rename `legacy` → `new_dir` when `legacy` is a dir and `new_dir` doesn't
/// exist. A fresh install (neither) and an already-migrated install (new dir
/// present) are both no-ops, and an existing new dir is never clobbered.
fn move_legacy_dir(legacy: &Path, new_dir: &Path) -> std::io::Result<()> {
    if new_dir.exists() || !legacy.is_dir() {
        return Ok(());
    }
    std::fs::rename(legacy, new_dir)
}

/// Keychain keys that predate the rebrand and must follow the user to the new
/// service. Keys added later are created directly in the new service.
fn migrated_secret_keys() -> [&'static str; 7] {
    use fly_secrets::keys::*;
    [
        OPENAI_API_KEY,
        ANTHROPIC_API_KEY,
        NIM_API_KEY,
        GROQ_API_KEY,
        GOOGLE_OAUTH_TOKEN,
        MS_OAUTH_TOKEN,
        crate::calendar_commands::GOOGLE_CLIENT_SECRET_KEY,
    ]
}

/// Copy secrets from `old` into `new`, filling only keys `new` lacks — so it's
/// idempotent and never overwrites a value the user set after migrating.
fn copy_secrets(old: &dyn SecretStore, new: &dyn SecretStore, keys: &[&str]) {
    for &key in keys {
        if matches!(new.get(key), Ok(None)) {
            if let Ok(Some(value)) = old.get(key) {
                let _ = new.set(key, &value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_secrets::MemorySecretStore;
    use fly_storage::backup;
    use std::cell::RefCell;
    use std::time::Duration;

    const QUICK_RETRY: OpenRetry = OpenRetry {
        attempts: 2,
        backoff: Duration::from_millis(1),
    };

    /// A library as a v2 build leaves it: just enough schema for the open
    /// to treat it as an upgrade.
    fn v2_library(dir: &Path) {
        let conn = rusqlite::Connection::open(dir.join("flyonthewall.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE notes (id TEXT PRIMARY KEY, title TEXT NOT NULL, folder_id TEXT,
                                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE meetings (id TEXT PRIMARY KEY, title TEXT NOT NULL, note_id TEXT NOT NULL);
             INSERT INTO notes VALUES ('n1', 'Weekly sync', NULL, '2026-01-01', '2026-01-01');
             PRAGMA user_version = 2;",
        )
        .unwrap();
    }

    /// Launch the way `open_library` does, recording which step reported
    /// when.
    fn launch(
        dir: &Path,
        version: &str,
        free_space: &dyn Fn(&Path) -> Option<u64>,
    ) -> (anyhow::Result<Storage>, Vec<&'static str>) {
        let steps = RefCell::new(Vec::new());
        let opened = open_library_with(
            dir,
            version,
            free_space,
            QUICK_RETRY,
            &mut |_| steps.borrow_mut().push("upgrade"),
            &mut || steps.borrow_mut().push("backup"),
        );
        (opened, steps.into_inner())
    }

    #[test]
    fn an_upgrading_launch_saves_one_backup_before_the_schema_step_and_a_second_makes_none() {
        let dir = tempfile::tempdir().unwrap();
        v2_library(dir.path());

        let (opened, steps) = launch(dir.path(), "2.5.0", &|_| Some(u64::MAX));
        let storage = opened.unwrap();
        // the gate hears about the backup first, then the upgrade's progress
        assert_eq!(steps.first(), Some(&"backup"), "{steps:?}");
        assert_eq!(steps.iter().filter(|s| **s == "backup").count(), 1);
        assert!(steps.contains(&"upgrade"), "{steps:?}");

        let backups = backup::list(dir.path());
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].label, "schema2");
        let copy = rusqlite::Connection::open_with_flags(
            &backups[0].path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let v: i64 = copy
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2, "the backup is the library as it was");
        // and the library itself moved on, recording who opened it
        assert_eq!(
            storage
                .get_setting(backup::LAST_RUN_VERSION_KEY)
                .unwrap()
                .as_deref(),
            Some("2.5.0")
        );
        assert_eq!(backup::pending_notice(&storage), None);
        drop(storage);

        // Same version again: no backup step, no new file.
        let (opened, steps) = launch(dir.path(), "2.5.0", &|_| Some(u64::MAX));
        drop(opened.unwrap());
        assert!(!steps.contains(&"backup"), "{steps:?}");
        assert_eq!(backup::list(dir.path()).len(), 1);
    }

    #[test]
    fn a_launch_short_of_disk_space_still_opens_and_leaves_one_notice() {
        let dir = tempfile::tempdir().unwrap();
        v2_library(dir.path());

        let (opened, _) = launch(dir.path(), "2.5.0", &|_| Some(1024));
        let storage = opened.unwrap();
        assert!(backup::list(dir.path()).is_empty());
        assert_eq!(
            backup::pending_notice(&storage),
            Some(backup::NoticeKind::NoSpace)
        );
        // upgraded regardless
        assert_eq!(
            storage
                .get_setting(backup::LAST_RUN_VERSION_KEY)
                .unwrap()
                .as_deref(),
            Some("2.5.0")
        );
    }

    #[test]
    fn a_corrupt_library_still_reaches_the_open_error_without_a_panic_or_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("flyonthewall.db"), b"definitely not sqlite").unwrap();
        let (opened, steps) = launch(dir.path(), "2.5.0", &|_| Some(u64::MAX));
        assert!(opened.is_err(), "the startup dialog is what reports this");
        assert!(!steps.contains(&"backup"), "{steps:?}");
        assert!(backup::list(dir.path()).is_empty());
    }

    #[test]
    fn a_fresh_install_opens_without_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let (opened, steps) = launch(dir.path(), "2.5.0", &|_| Some(u64::MAX));
        let storage = opened.unwrap();
        assert!(steps.is_empty(), "{steps:?}");
        assert!(!dir.path().join(backup::BACKUP_DIR).exists());
        // and compression owes it nothing either
        assert!(!backup::compression_backup_due(&storage));
    }

    #[test]
    fn move_legacy_dir_moves_when_new_absent() {
        let base = tempfile::tempdir().unwrap();
        let legacy = base.path().join("Looma");
        let new_dir = base.path().join("FlyOnTheWall");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("note.md"), b"hi").unwrap();

        move_legacy_dir(&legacy, &new_dir).unwrap();

        assert!(!legacy.exists(), "legacy dir should be moved away");
        assert_eq!(std::fs::read(new_dir.join("note.md")).unwrap(), b"hi");
    }

    #[test]
    fn move_legacy_dir_never_clobbers_existing_new_dir() {
        let base = tempfile::tempdir().unwrap();
        let legacy = base.path().join("Looma");
        let new_dir = base.path().join("FlyOnTheWall");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("old.md"), b"old").unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::write(new_dir.join("current.md"), b"current").unwrap();

        move_legacy_dir(&legacy, &new_dir).unwrap();

        assert!(new_dir.join("current.md").exists());
        assert!(!new_dir.join("old.md").exists(), "must not merge legacy in");
        assert!(
            legacy.exists(),
            "legacy left intact when new dir already exists"
        );
    }

    #[test]
    fn move_legacy_dir_is_noop_on_fresh_install() {
        let base = tempfile::tempdir().unwrap();
        let new_dir = base.path().join("FlyOnTheWall");
        move_legacy_dir(&base.path().join("Looma"), &new_dir).unwrap();
        assert!(!new_dir.exists());
    }

    #[test]
    fn copy_secrets_fills_missing_and_preserves_existing() {
        let old = MemorySecretStore::default();
        let new = MemorySecretStore::default();
        old.set("openai_api_key", "sk-old").unwrap();
        old.set("groq_api_key", "gsk-old").unwrap();
        // A key the user re-entered after migrating must win over the old one.
        new.set("groq_api_key", "gsk-new").unwrap();

        copy_secrets(
            &old,
            &new,
            &["openai_api_key", "groq_api_key", "ms_oauth_token"],
        );

        assert_eq!(
            new.get("openai_api_key").unwrap().as_deref(),
            Some("sk-old")
        );
        assert_eq!(new.get("groq_api_key").unwrap().as_deref(), Some("gsk-new"));
        assert_eq!(new.get("ms_oauth_token").unwrap(), None);
    }
}
