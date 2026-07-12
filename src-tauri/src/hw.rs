//! Hardware detection → recommended ASR tier (spec §6.2). Auto-picked on
//! first run, always user-overridable in Settings.
//!
//! `detect()` shells out to nvidia-smi (hundreds of ms), so nothing on a
//! command path calls it directly: the result is cached in settings
//! ([`cached`]) and refreshed in the background at each launch (lib.rs) —
//! a hardware change is picked up one launch later.

use serde::{Deserialize, Serialize};

/// Settings key holding the JSON-serialized [`HwInfo`] of the last run.
pub const CACHE_KEY: &str = "hw.cache";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HwInfo {
    pub ram_gb: f64,
    pub cpu_cores: usize,
    pub gpu_name: Option<String>,
    pub vram_mb: Option<u64>,
    pub recommended_tier: String,
}

/// The hardware profile persisted by the last detection run, if any.
pub fn cached(storage: &fly_storage::Storage) -> Option<HwInfo> {
    storage
        .get_setting(CACHE_KEY)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str(&json).ok())
}

/// Run detection and persist the result for [`cached`] readers.
pub fn detect_and_cache(storage: &fly_storage::Storage) -> HwInfo {
    let info = detect();
    if let Ok(json) = serde_json::to_string(&info) {
        if let Err(e) = storage.set_setting(CACHE_KEY, &json) {
            tracing::warn!(error = %e, "could not persist hardware cache");
        }
    }
    info
}

pub fn detect() -> HwInfo {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
    let ram_gb = sys.total_memory() as f64 / 1024.0 / 1024.0 / 1024.0;
    let cpu_cores = sys.cpus().len().max(1);

    // NVIDIA first (nvidia-smi also reports VRAM, which drives the tier
    // recommendation); otherwise fall back to a vendor-neutral name so
    // Settings can show what the Vulkan GPU path (gpu.rs) would run on.
    let (gpu_name, vram_mb) = match detect_nvidia() {
        (Some(name), vram) => (Some(name), vram),
        _ => (detect_any_gpu_name(), None),
    };

    // §6.2: NVIDIA ≥8 GB VRAM (or strong CPU + ≥16 GB RAM) → Best;
    // ≤8 GB RAM → Light; otherwise Balanced. Cloud is opt-in only.
    let recommended_tier = if vram_mb.unwrap_or(0) >= 8_192 || (ram_gb >= 16.0 && cpu_cores >= 12) {
        "best"
    } else if ram_gb <= 8.5 {
        "light"
    } else {
        "balanced"
    };

    HwInfo {
        ram_gb: (ram_gb * 10.0).round() / 10.0,
        cpu_cores,
        gpu_name,
        vram_mb,
        recommended_tier: recommended_tier.to_string(),
    }
}

fn detect_nvidia() -> (Option<String>, Option<u64>) {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output();
    if let Ok(out) = out {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(line) = text.lines().next() {
                let mut parts = line.split(',').map(str::trim);
                let name = parts.next().map(str::to_string);
                let vram = parts.next().and_then(|v| v.parse::<u64>().ok());
                return (name, vram);
            }
        }
    }
    (None, None)
}

/// Vendor-neutral GPU name (Intel/AMD iGPUs have no nvidia-smi). Display
/// only — no VRAM figure, so it never changes the recommended tier. Windows
/// queries WMI through PowerShell (`wmic` is gone on Windows 11); other
/// platforms return None (macOS hardware is uniform enough not to matter).
fn detect_any_gpu_name() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name) -join \"`n\"",
            ])
            // CREATE_NO_WINDOW — no console flash from a GUI process
            .creation_flags(0x0800_0000)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.contains("Microsoft Basic"))
            .map(str::to_string)
    }
    #[cfg(not(target_os = "windows"))]
    None
}

/// Default whisper model for a tier (spec §6.2 + docs/MODELS.md).
pub fn default_model_for_tier(tier: &str, max_quality: bool) -> &'static str {
    if max_quality {
        return "ggml-large-v3-q5_0";
    }
    match tier {
        "light" => "ggml-small-q5_1",
        // large-v3-turbo beats medium on accuracy at similar CPU cost — the
        // sweet spot for both Balanced and Best (medium stays selectable).
        _ => "ggml-large-v3-turbo-q5_0",
    }
}
