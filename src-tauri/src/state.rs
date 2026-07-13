//! Long-lived app state managed by Tauri.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fly_audio::cpal_backend::CpalAudioCapture;
use fly_audio::AudioCapture;
use fly_secrets::{KeychainSecretStore, SecretStore};
use fly_storage::Storage;

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
    /// At most one screen capture at a time.
    pub screen: Mutex<Option<ActiveScreenRecording>>,
    /// Nudges the transcription queue worker (job enqueued, recording
    /// stopped). The worker also polls, so a missed nudge only delays it.
    pub jobs_notify: tokio::sync::Notify,
    /// Managed `ollama serve` child (None when a user-run server is used or
    /// the ollama provider isn't active). Killed on app exit (ollama.rs).
    pub ollama: Mutex<Option<std::process::Child>>,
}

impl AppState {
    pub fn init() -> anyhow::Result<Self> {
        let data_dir = default_data_dir();
        // Move a pre-rebrand data dir into place before the DB is opened.
        migrate_from_legacy_data_dir(&data_dir)?;
        let secrets = Arc::new(KeychainSecretStore::new());
        // Best-effort: copy secrets out of the pre-rebrand keychain service. A
        // locked or absent keyring must never block launch, and keys can be
        // re-entered, so failures here are swallowed, not fatal.
        copy_secrets(
            &KeychainSecretStore::with_service(LEGACY_KEYCHAIN_SERVICE),
            secrets.as_ref(),
            &migrated_secret_keys(),
        );
        Self::init_with(data_dir, secrets)
    }

    /// Composition with explicit data dir + secret store — used by tests.
    pub fn init_with(data_dir: PathBuf, secrets: Arc<dyn SecretStore>) -> anyhow::Result<Self> {
        let storage = Storage::open(&data_dir)?;
        tracing::info!(dir = %data_dir.display(), "storage ready");
        Ok(Self {
            storage: Mutex::new(storage),
            data_dir,
            audio: Box::new(CpalAudioCapture::new()),
            recording: Mutex::new(None),
            secrets,
            pipeline_stage: Mutex::new(HashMap::new()),
            screen: Mutex::new(None),
            jobs_notify: tokio::sync::Notify::new(),
            ollama: Mutex::new(None),
        })
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
const LEGACY_KEYCHAIN_SERVICE: &str = "com.looma.notetaker";
/// Current data-dir name under the OS data directory.
const DATA_DIR: &str = "FlyOnTheWall";

/// User-visible, portable data directory (spec §10): everything the app stores
/// lives under here.
fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DATA_DIR)
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
