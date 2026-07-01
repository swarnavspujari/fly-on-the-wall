//! Long-lived app state managed by Tauri.

use std::path::PathBuf;
use std::sync::Mutex;

use looma_storage::Storage;

pub struct AppState {
    /// rusqlite connections are Send but not Sync; all storage access goes
    /// through this mutex. Fine for a single-user desktop app.
    pub storage: Mutex<Storage>,
    pub data_dir: PathBuf,
}

impl AppState {
    pub fn init() -> anyhow::Result<Self> {
        let data_dir = default_data_dir();
        let storage = Storage::open(&data_dir)?;
        tracing::info!(dir = %data_dir.display(), "storage ready");
        Ok(Self {
            storage: Mutex::new(storage),
            data_dir,
        })
    }
}

/// User-visible, portable data directory (spec §10): everything Looma
/// stores lives under here.
fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Looma")
}
