//! Long-lived app state managed by Tauri.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use looma_audio::cpal_backend::CpalAudioCapture;
use looma_audio::AudioCapture;
use looma_secrets::{KeychainSecretStore, SecretStore};
use looma_storage::Storage;

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
}

impl AppState {
    pub fn init() -> anyhow::Result<Self> {
        Self::init_with(default_data_dir(), Arc::new(KeychainSecretStore::new()))
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
        })
    }

    /// Shared handle for components that hold the secret store themselves
    /// (e.g. calendar providers refreshing tokens).
    pub fn secrets_arc(&self) -> Arc<dyn SecretStore> {
        self.secrets.clone()
    }
}

/// User-visible, portable data directory (spec §10): everything Looma
/// stores lives under here.
fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Looma")
}
