//! Artifact manager: sidecar binaries and model weights, downloaded on first
//! use into the data dir with streaming progress + SHA-256 verification.
//! Nothing here is ever bundled in git or the installer (docs/MODELS.md).

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

/// Progress sink — the tauri layer forwards these to `model:progress`
/// events; tests pass a no-op.
pub type ProgressSink<'a> = &'a (dyn Fn(ModelProgress) + Send + Sync);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// Single file stored at `dest_rel`.
    File,
    /// Archive extracted into `dest_rel`; `probe_rel` proves extraction.
    Archive,
}

pub struct Artifact {
    pub id: &'static str,
    pub display: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub kind: ArtifactKind,
    /// Destination (file path or extraction dir), relative to the data dir.
    pub dest_rel: &'static str,
    /// For archives: file inside dest that proves a complete install and is
    /// the path callers actually want. For files: same as dest_rel.
    pub probe_rel: &'static str,
}

/// Platform tool binaries. Checksums pinned from upstream release digests
/// (GitHub asset `digest` fields), same method as the original Windows pins.
/// whisper.cpp and ffmpeg publish no macOS binaries (and whisper.cpp none for
/// Linux either) — macOS gets a self-built whisper engine hosted on this
/// repo (below); elsewhere `ensure_tool` falls back to the tool on PATH.
#[cfg(target_os = "windows")]
const TOOLS: &[Artifact] = &[
    Artifact {
        id: "whisper-bin",
        display: "whisper.cpp CLI (CPU, v1.9.1)",
        url: "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.1/whisper-bin-x64.zip",
        sha256: "7d8be46ecd31828e1eb7a2ecdd0d6b314feafd82163038ab6092594b0a063539",
        bytes: 7_982_101,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/whisper",
        probe_rel: "bin/whisper/Release/whisper-cli.exe",
    },
    // Upstream publishes no Vulkan Windows binary (only CPU/BLAS/CUDA-only),
    // so this is whisper.cpp v1.9.1 (f049fff) built with -DGGML_VULKAN=1 and
    // hosted as a tools release on this repo — one cross-vendor GPU build for
    // NVIDIA/AMD/Intel. Selection is gated by a per-machine benchmark
    // (gpu.rs); the CPU entry above stays the default and is never touched.
    Artifact {
        id: "whisper-bin-vulkan",
        display: "whisper.cpp CLI (Vulkan GPU, v1.9.1)",
        url: "https://github.com/swarnavspujari/fly-on-the-wall/releases/download/tools-whisper-vulkan-v1.9.1/whisper-bin-x64-vulkan-v1.9.1.zip",
        sha256: "9dbd3ab65394a26784d79ae495de36311925f1f489a6e0e905841b6076799973",
        bytes: 23_632_146,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/whisper-vulkan",
        probe_rel: "bin/whisper-vulkan/Release/whisper-cli.exe",
    },
    Artifact {
        id: "sherpa-bin",
        display: "sherpa-onnx diarization CLI (v1.13.3)",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.3/sherpa-onnx-v1.13.3-win-x64-shared-MD-Release.tar.bz2",
        sha256: "6491877a599a4c5a33e5568c8a22f86fc376dc25a2bc49b95373bbf0dd0a12c8",
        bytes: 19_413_897,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/sherpa",
        probe_rel: "bin/sherpa/sherpa-onnx-v1.13.3-win-x64-shared-MD-Release/bin/sherpa-onnx-offline-speaker-diarization.exe",
    },
    Artifact {
        id: "ffmpeg",
        display: "ffmpeg (n8.1, screen capture + media import)",
        url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-06-30-13-34/ffmpeg-n8.1.2-21-gce3c09c101-win64-gpl-shared-8.1.zip",
        sha256: "ec51253085a831b517e68cb7a1e46d13fcc8324f5e61ac0b3fd73c56af41ca21",
        bytes: 79_279_847,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/ffmpeg",
        probe_rel: "bin/ffmpeg/ffmpeg-n8.1.2-21-gce3c09c101-win64-gpl-shared-8.1/bin/ffmpeg.exe",
    },
    // Local LLM runtime for the "ollama" provider (Enhance / Ask / polish).
    // Strictly opt-in from Settings — 1.5 GB (bundles CUDA runners); the app
    // manages `ollama serve` itself (see ollama.rs). The zip's root holds
    // ollama.exe + lib/ollama/ (verified against the release's central
    // directory), digest from the GitHub asset like the pins above.
    Artifact {
        id: "ollama-bin",
        display: "Ollama (local AI runtime, v0.31.2)",
        url: "https://github.com/ollama/ollama/releases/download/v0.31.2/ollama-windows-amd64.zip",
        sha256: "6988b58d2223ae3f9d5766b46b0be614dec36524b80317159718b5adf3006f3b",
        bytes: 1_502_730_186,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/ollama",
        probe_rel: "bin/ollama/ollama.exe",
    },
];

#[cfg(target_os = "linux")]
const TOOLS: &[Artifact] = &[
    Artifact {
        id: "sherpa-bin",
        display: "sherpa-onnx diarization CLI (v1.13.3)",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.3/sherpa-onnx-v1.13.3-linux-x64-shared.tar.bz2",
        sha256: "3e6aa632a30b7047f389e337e342eb08ea6c5661717645fd072e7d0ebf9d57fb",
        bytes: 27_211_051,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/sherpa",
        probe_rel: "bin/sherpa/sherpa-onnx-v1.13.3-linux-x64-shared/bin/sherpa-onnx-offline-speaker-diarization",
    },
    Artifact {
        id: "ffmpeg",
        display: "ffmpeg (n8.1, screen capture + media import)",
        url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-06-30-13-34/ffmpeg-n8.1.2-21-gce3c09c101-linux64-gpl-shared-8.1.tar.xz",
        sha256: "23f5d4c8e6fdc24fbbfcbbb8e83a727154f1ef70830b108ac7fd131856777405",
        bytes: 62_123_996,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/ffmpeg",
        probe_rel: "bin/ffmpeg/ffmpeg-n8.1.2-21-gce3c09c101-linux64-gpl-shared-8.1/bin/ffmpeg",
    },
];

#[cfg(target_os = "macos")]
const TOOLS: &[Artifact] = &[
    // Upstream whisper.cpp publishes no macOS binary, so we build it ourselves
    // (static, universal, Metal embedded — see scripts/build-whisper-sidecar.sh
    // and .github/workflows/build-whisper-sidecar.yml) and host it as a tools
    // release on this repo, like the Windows Vulkan build above. This lets
    // `ensure_tool` auto-download the engine on first transcribe, exactly like
    // Windows, instead of requiring a `whisper-cli` on PATH (e.g. `brew
    // install whisper-cpp`). While no managed copy is installed yet, a
    // brew/PATH build still wins (resolution: installed → PATH → download).
    // Pin verified 2026-07-16 by hashing the hosted asset independently of
    // the workflow's summary; fat header confirmed x86_64 + arm64 slices.
    Artifact {
        id: "whisper-bin",
        display: "whisper.cpp CLI (macOS universal, v1.9.1)",
        url: "https://github.com/swarnavspujari/fly-on-the-wall/releases/download/tools-whisper-v1.9.1/whisper-bin-macos-universal2-v1.9.1.tar.bz2",
        sha256: "f9a4bcae555dd3d14f0a8795aad63b8a7a006d59f705bca94e83ef2215805070",
        bytes: 2_388_157,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/whisper",
        probe_rel: "bin/whisper/whisper-cli",
    },
    // v1.13.x macOS builds bundle an onnxruntime compiled against SDK 15.5
    // whose CoreML EP hard-references MLComputePlan (macOS 14.4+): dyld
    // aborts on every macOS from the app's 12.0 floor through 14.3
    // (reproduced on this repo's 14.3 Apple Silicon smoke machine). The
    // v1.12.34 release publishes an onnxruntime-1.17.1 variant with
    // minos 11.0 that runs and diarizes correctly there — verified on real
    // audio with the pinned pyannote+CAM++ models. Digest matches the
    // GitHub asset digest (sha256 re-verified locally 2026-07-21).
    Artifact {
        id: "sherpa-bin",
        display: "sherpa-onnx diarization CLI (v1.12.34, onnxruntime 1.17.1)",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.12.34/sherpa-onnx-v1.12.34-onnxruntime-1.17.1-osx-universal2-shared.tar.bz2",
        sha256: "86ed07a11d2a15fc1615e1ad0a69514a8bee556c1b9a85c7ab3822c66f57bdf7",
        bytes: 42_657_343,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/sherpa",
        probe_rel: "bin/sherpa/sherpa-onnx-v1.12.34-onnxruntime-1.17.1-osx-universal2-shared/bin/sherpa-onnx-offline-speaker-diarization",
    },
    // Local LLM runtime for the "ollama" provider — same managed, opt-in
    // story as the Windows entry (ollama.rs `can_install` keys off this).
    // The CLI tarball ships the arm64 Metal/MLX runners: verified on the
    // Apple Silicon smoke machine — `ollama serve` reports inference
    // compute library=Metal (Apple M3 Pro), so pulled models run on the
    // GPU. Digest matches the GitHub asset digest (re-hashed locally
    // 2026-07-21). Tarball root holds `ollama` + its dylibs.
    Artifact {
        id: "ollama-bin",
        display: "Ollama (local AI runtime, v0.31.2)",
        url: "https://github.com/ollama/ollama/releases/download/v0.31.2/ollama-darwin.tgz",
        sha256: "d72381baa260f6ce014c8e942e605eac76cac5313fcb3401eaf5495f659cfd6d",
        bytes: 128_859_228,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/ollama",
        probe_rel: "bin/ollama/ollama",
    },
];

/// OS-independent model weights. Checksums pinned from upstream release
/// digests / HF LFS metadata, re-verified locally on 2026-07-01.
const MODELS: &[Artifact] = &[
    Artifact {
        id: "pyannote-seg",
        display: "pyannote segmentation 3.0 (ONNX)",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2",
        sha256: "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488",
        bytes: 6_958_444,
        kind: ArtifactKind::Archive,
        dest_rel: "models/diarize",
        probe_rel: "models/diarize/sherpa-onnx-pyannote-segmentation-3-0/model.onnx",
    },
    Artifact {
        id: "campplus-embedding",
        display: "3D-Speaker CAM++ speaker embedding (ONNX)",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx",
        sha256: "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2",
        bytes: 28_281_164,
        kind: ArtifactKind::File,
        dest_rel: "models/diarize/campplus.onnx",
        probe_rel: "models/diarize/campplus.onnx",
    },
    Artifact {
        id: "ggml-small-q5_1",
        display: "Whisper small (Q5, ~190 MB) — Light tier",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin",
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
        bytes: 190_085_487,
        kind: ArtifactKind::File,
        dest_rel: "models/asr/ggml-small-q5_1.bin",
        probe_rel: "models/asr/ggml-small-q5_1.bin",
    },
    Artifact {
        id: "ggml-medium-q5_0",
        display: "Whisper medium (Q5, ~540 MB)",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium-q5_0.bin",
        sha256: "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f",
        bytes: 539_212_467,
        kind: ArtifactKind::File,
        dest_rel: "models/asr/ggml-medium-q5_0.bin",
        probe_rel: "models/asr/ggml-medium-q5_0.bin",
    },
    Artifact {
        id: "ggml-large-v3-turbo-q5_0",
        display: "Whisper large-v3-turbo (Q5, ~574 MB) — Balanced/Best default",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        bytes: 574_041_195,
        kind: ArtifactKind::File,
        dest_rel: "models/asr/ggml-large-v3-turbo-q5_0.bin",
        probe_rel: "models/asr/ggml-large-v3-turbo-q5_0.bin",
    },
    Artifact {
        id: "ggml-large-v3-q5_0",
        display: "Whisper large-v3 (Q5, ~1 GB) — maximum quality",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-q5_0.bin",
        sha256: "d75795ecff3f83b5faa89d1900604ad8c780abd5739fae406de19f23ecd98ad1",
        bytes: 1_081_140_203,
        kind: ArtifactKind::File,
        dest_rel: "models/asr/ggml-large-v3-q5_0.bin",
        probe_rel: "models/asr/ggml-large-v3-q5_0.bin",
    },
];

/// Every artifact this OS can manage (tools first, then model weights).
pub fn registry() -> impl Iterator<Item = &'static Artifact> {
    TOOLS.iter().chain(MODELS.iter())
}

pub fn artifact(id: &str) -> Option<&'static Artifact> {
    registry().find(|a| a.id == id)
}

/// Artifact id for the whisper.cpp engine and the executable name(s) to look
/// for on PATH. Kept here so readiness checks and the transcription pipeline
/// resolve the exact same binary.
pub const WHISPER_ENGINE_ID: &str = "whisper-bin";
pub const WHISPER_CLI_NAMES: &[&str] = &["whisper-cli"];

/// Markers for failures only a person can fix, mirroring
/// `fly_asr::REJECTED_MARKER`: the pipeline reduces errors to strings before
/// they reach the scheduler, which matches on these substrings to fail the
/// job once (with its actionable notice) instead of burning every retry on
/// an identical outcome. Sharing the constant between the producers below
/// and `scheduler::permanent_failure` keeps a message reword from silently
/// decoupling them. The frontend's notice selector (pipelineNotice.ts)
/// matches the same phrases — change them in both places or not at all.
pub const ENGINE_MISSING_MARKER: &str = "is not installed";
pub const DOWNLOAD_FAILED_MARKER: &str = "download failed for";

/// True when a tool binary is already usable without downloading anything — a
/// managed install (this OS ships an artifact for it) or the same tool on
/// PATH. This is the non-downloading half of `ensure_tool`, used to report
/// engine readiness to the UI so weights-present-but-engine-missing is a
/// visible state rather than a transcribe-time surprise.
pub fn tool_installed(data_dir: &Path, id: &str, path_names: &[&str]) -> bool {
    if let Some(a) = artifact(id) {
        if installed_path(data_dir, a).is_some() {
            return true;
        }
    }
    find_on_path(path_names).is_some()
}

/// Locate an executable on PATH (used where upstream publishes no binary for
/// this OS — e.g. whisper-cli and ffmpeg on macOS, whisper-cli on Linux).
/// On macOS the standard Homebrew prefixes are probed as well: apps launched
/// from Finder don't inherit brew's PATH, and without this the "brew install
/// whisper-cpp" remedy our own error message suggests would never resolve.
pub fn find_on_path(names: &[&str]) -> Option<PathBuf> {
    // An unset PATH (some Finder/launchd spawns) must not skip the brew
    // probes below — they exist for exactly those launches.
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let path_dirs = std::env::split_paths(&path_var).collect::<Vec<_>>();
    #[cfg(target_os = "macos")]
    let extra_dirs = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
    #[cfg(not(target_os = "macos"))]
    let extra_dirs: [PathBuf; 0] = [];
    for dir in path_dirs.iter().chain(extra_dirs.iter()) {
        for name in names {
            for candidate in [dir.join(name), dir.join(format!("{name}.exe"))] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Resolve a tool binary: an already-installed managed copy wins, then the
/// same tool on PATH, then a managed download; otherwise fail with
/// person-actionable guidance.
pub async fn ensure_tool(
    progress: ProgressSink<'_>,
    data_dir: &Path,
    id: &str,
    path_names: &[&str],
    guidance: &str,
) -> Result<PathBuf, String> {
    ensure_tool_with(
        progress,
        data_dir,
        id,
        path_names,
        guidance,
        DownloadEffort::Full,
    )
    .await
}

/// [`ensure_tool`] with an explicit download effort for the managed-download
/// leg (installed copies and PATH hits are unaffected).
pub async fn ensure_tool_with(
    progress: ProgressSink<'_>,
    data_dir: &Path,
    id: &str,
    path_names: &[&str],
    guidance: &str,
    effort: DownloadEffort,
) -> Result<PathBuf, String> {
    if let Some(a) = artifact(id) {
        if let Some(installed) = installed_path(data_dir, a) {
            return Ok(installed);
        }
    }
    if let Some(found) = find_on_path(path_names) {
        return Ok(found);
    }
    if artifact(id).is_some() {
        return ensure_with(progress, data_dir, id, effort).await;
    }
    Err(format!(
        "{} {ENGINE_MISSING_MARKER} — {}",
        path_names[0], guidance
    ))
}

pub fn installed_path(data_dir: &Path, a: &Artifact) -> Option<PathBuf> {
    let probe = data_dir.join(a.probe_rel);
    probe.exists().then_some(probe)
}

#[derive(Clone, serde::Serialize)]
pub struct ModelProgress {
    pub id: String,
    pub downloaded: u64,
    pub total: u64,
    pub stage: String, // downloading | verifying | extracting | done | error
    pub error: Option<String>,
}

/// Best already-installed whisper model to stand in for `wanted_id` — the
/// registry lists ASR models smallest→largest, so among installed models we
/// prefer the largest that is NOT bigger than the wanted one (a Light-tier
/// machine whose small model is missing must not get pushed onto a leftover
/// large-v3 the tier system steered away from). Only when nothing at-or-below
/// the wanted size is installed does a larger model win — a slow rescue still
/// beats losing the meeting. Lets transcription proceed when the wanted model
/// can't be downloaded (offline, CDN outage) but another model is on disk.
pub fn best_installed_asr_model(
    data_dir: &Path,
    wanted_id: &str,
) -> Option<(&'static str, PathBuf)> {
    let asr: Vec<&'static Artifact> = registry().filter(|a| a.id.starts_with("ggml-")).collect();
    let wanted_idx = asr.iter().position(|a| a.id == wanted_id);
    let installed: Vec<(usize, &'static Artifact, PathBuf)> = asr
        .iter()
        .enumerate()
        .filter_map(|(i, a)| installed_path(data_dir, a).map(|p| (i, *a, p)))
        .collect();
    let at_or_below = installed
        .iter()
        .rfind(|(i, _, _)| wanted_idx.is_none_or(|w| *i <= w));
    at_or_below
        .or_else(|| installed.first())
        .map(|(_, a, p)| (a.id, p.clone()))
}

/// Remove stale download temp files (`{id}.download.{pid}.{n}`) left by a
/// crashed or killed run — per-attempt unique names mean nothing else ever
/// opens them, and a multi-GB model orphan must not sit on disk forever.
/// Age-gated: a LIVE download writes continuously, so its mtime stays fresh;
/// anything untouched for `max_age` is safely dead. Called off the startup
/// path (lib.rs).
pub fn sweep_stale_downloads(data_dir: &Path, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let is_temp = name.to_string_lossy().contains(".download.");
        if !is_temp || !entry.path().is_file() {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if stale {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {
                    tracing::info!(file = %entry.path().display(), "removed stale download temp")
                }
                Err(e) => {
                    tracing::warn!(file = %entry.path().display(), error = %e, "could not remove stale download temp")
                }
            }
        }
    }
}

/// Opt-out for the hf-mirror.com fallback (docs/MODELS.md): set
/// `FLYONTHEWALL_NO_HF_MIRROR` to anything but "0" to only ever contact
/// huggingface.co. Default is mirror on — integrity is decided by the
/// SHA-256 pins either way.
pub const NO_HF_MIRROR_ENV: &str = "FLYONTHEWALL_NO_HF_MIRROR";

fn hf_mirror_disabled() -> bool {
    std::env::var_os(NO_HF_MIRROR_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Every URL worth trying for an artifact, in order: the pinned primary,
/// then mirrors derivable from the host. Hugging Face files are also served
/// by hf-mirror.com under the same path — useful when HF's Xet CDN bridge is
/// rejecting downloads (a known intermittent failure).
fn candidate_urls(url: &str) -> Vec<String> {
    candidate_urls_with(url, !hf_mirror_disabled())
}

fn candidate_urls_with(url: &str, allow_mirror: bool) -> Vec<String> {
    let mut v = vec![url.to_string()];
    if allow_mirror {
        if let Some(rest) = url.strip_prefix("https://huggingface.co/") {
            v.push(format!("https://hf-mirror.com/{rest}"));
        }
    }
    v
}

/// Error prefix shared by the producer in `download_and_verify` and the
/// `retry_same_url` predicate, so a message reword can't silently decouple
/// them.
const CHECKSUM_MISMATCH_PREFIX: &str = "checksum mismatch";

/// Whether a failed attempt is worth retrying against the SAME url. A
/// checksum mismatch is deterministic for a given source — the host is
/// simply serving a file that isn't the pinned one — so retrying it cannot
/// succeed and would only cost another full download; skip straight to the
/// next source. Transient failures (network, HTTP 5xx, Xet 403) get one
/// retry.
fn retry_same_url(error: &str) -> bool {
    !error.starts_with(CHECKSUM_MISMATCH_PREFIX)
}

/// One download attempt from one URL into `tmp`, streaming progress and
/// verifying the SHA-256 pin. Returns bytes downloaded; on any failure the
/// temp file is removed so the next attempt starts clean.
async fn download_and_verify(
    progress: ProgressSink<'_>,
    client: &reqwest::Client,
    a: &Artifact,
    tmp: &Path,
    url: &str,
) -> Result<u64, String> {
    let cleanup = |e: String| async move {
        let _ = tokio::fs::remove_file(tmp).await;
        Err::<u64, String>(e)
    };
    let resp = match async {
        client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("download failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("download failed: {e}"))
    }
    .await
    {
        Ok(r) => r,
        Err(e) => return cleanup(e).await,
    };
    let total = resp.content_length().unwrap_or(a.bytes);

    let mut hasher = Sha256::new();
    let mut file = match tokio::fs::File::create(tmp).await {
        Ok(f) => f,
        Err(e) => return cleanup(e.to_string()).await,
    };
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => return cleanup(format!("download interrupted: {e}")).await,
        };
        hasher.update(&chunk);
        if let Err(e) = file.write_all(&chunk).await {
            return cleanup(e.to_string()).await;
        }
        downloaded += chunk.len() as u64;
        if last_emit.elapsed().as_millis() > 200 {
            progress(ModelProgress {
                id: a.id.into(),
                downloaded,
                total,
                stage: "downloading".into(),
                error: None,
            });
            last_emit = std::time::Instant::now();
        }
    }
    if let Err(e) = file.flush().await {
        return cleanup(e.to_string()).await;
    }
    drop(file);

    progress(ModelProgress {
        id: a.id.into(),
        downloaded,
        total,
        stage: "verifying".into(),
        error: None,
    });
    let digest = hex::encode(hasher.finalize());
    if digest != a.sha256 {
        return cleanup(format!(
            "{CHECKSUM_MISMATCH_PREFIX} (expected {}, got {digest})",
            a.sha256
        ))
        .await;
    }
    Ok(downloaded)
}

/// How hard [`ensure`] tries when an artifact actually needs downloading.
#[derive(Clone, Copy, PartialEq)]
pub enum DownloadEffort {
    /// Every candidate URL, two attempts each, with backoff — post-meeting
    /// transcription wants the meeting rescued with patience to spare.
    Full,
    /// One attempt at the primary URL only. The live-caption loop wants a
    /// prompt "unavailable" verdict when offline, not ~12 s of retries;
    /// the next meeting retries anyway.
    SingleAttempt,
}

/// Ensure an artifact is installed; returns the probe path. Reports
/// progress through the sink while downloading/extracting.
pub async fn ensure(
    progress: ProgressSink<'_>,
    data_dir: &Path,
    id: &str,
) -> Result<PathBuf, String> {
    ensure_with(progress, data_dir, id, DownloadEffort::Full).await
}

/// A temp path no other download can be writing to. Concurrent `ensure`
/// calls for the SAME artifact (e.g. the live-caption loop and a pipeline
/// both resolving whisper-bin) used to share one `{id}.download` file: both
/// writers interleaved into it, and since the SHA-256 is computed over each
/// download's network stream — not the file on disk — the first to finish
/// could verify its own clean stream yet rename/extract the interleaved
/// bytes. Process id + an in-process counter make every attempt's file its
/// own; a crashed run leaves at most a small stale `{id}.download.{pid}.{n}`
/// file, which no later run ever opens.
fn download_tmp_path(data_dir: &Path, id: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    data_dir.join(format!("{id}.download.{}.{n}", std::process::id()))
}

/// [`ensure`] with an explicit download effort.
pub async fn ensure_with(
    progress: ProgressSink<'_>,
    data_dir: &Path,
    id: &str,
    effort: DownloadEffort,
) -> Result<PathBuf, String> {
    let a = artifact(id).ok_or_else(|| format!("unknown artifact {id}"))?;
    if let Some(path) = installed_path(data_dir, a) {
        return Ok(path);
    }

    let tmp = download_tmp_path(data_dir, a.id);
    if let Some(parent) = tmp.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }

    // ---- download with streaming progress, over every candidate URL ----
    // Any candidate is safe to try: acceptance is decided by the SHA-256 pin,
    // so a wrong or compromised mirror can only fail the checksum, never
    // install bad bytes. Two attempts per URL absorbs transient CDN errors
    // (Hugging Face's Xet bridge intermittently returns 403 AccessDenied)
    // without hammering a host that is down.
    let (candidates, attempts_per_url) = match effort {
        DownloadEffort::Full => (candidate_urls(a.url), 2u8),
        DownloadEffort::SingleAttempt => (vec![a.url.to_string()], 1u8),
    };
    // One shared client: retries and mirror hops reuse the connection pool
    // instead of paying a fresh TLS handshake per attempt. Bounded connect +
    // per-chunk read: with no timeouts an offline machine sits in TCP connect
    // limbo for the OS default (observed 60+ s on macOS) before any caller —
    // the live loop's prompt "unavailable" verdict most of all — hears about
    // it. No overall request timeout: model downloads legitimately run for
    // minutes.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .read_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let mut errors: Vec<String> = Vec::new();
    let mut downloaded: u64 = 0;
    let mut fetched = false;
    'sources: for url in &candidates {
        for attempt in 0..attempts_per_url {
            // Back off only before re-hitting the SAME url (protecting a
            // struggling host); switching to a different source proceeds
            // immediately — an offline machine shouldn't stack 4 s sleeps
            // between hosts that all refuse instantly.
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            }
            match download_and_verify(progress, &client, a, &tmp, url).await {
                Ok(bytes) => {
                    downloaded = bytes;
                    fetched = true;
                    break 'sources;
                }
                Err(e) => {
                    tracing::warn!(artifact = a.id, url, attempt, error = %e, "download attempt failed");
                    let retry = retry_same_url(&e);
                    errors.push(e);
                    if !retry {
                        continue 'sources;
                    }
                }
            }
        }
    }
    if !fetched {
        let last = errors.last().cloned().unwrap_or_default();
        let hf_hint = if a.url.starts_with("https://huggingface.co/") {
            " Hugging Face's CDN sometimes rejects downloads temporarily — retry later, \
             pick an already-installed model in Settings, or enable Groq cloud transcription."
        } else {
            ""
        };
        let msg = format!(
            "{DOWNLOAD_FAILED_MARKER} {} after trying {} source(s) (last error: {last}).{hf_hint}",
            a.display,
            candidates.len(),
        );
        progress(ModelProgress {
            id: a.id.into(),
            downloaded: 0,
            total: a.bytes,
            stage: "error".into(),
            error: Some(msg.clone()),
        });
        return Err(msg);
    }
    let total = a.bytes.max(downloaded);

    // ---- install ----
    let dest = data_dir.join(a.dest_rel);
    match a.kind {
        ArtifactKind::File => {
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            tokio::fs::rename(&tmp, &dest)
                .await
                .map_err(|e| e.to_string())?;
        }
        ArtifactKind::Archive => {
            progress(ModelProgress {
                id: a.id.into(),
                downloaded,
                total,
                stage: "extracting".into(),
                error: None,
            });
            tokio::fs::create_dir_all(&dest)
                .await
                .map_err(|e| e.to_string())?;
            extract_archive(&tmp, &dest, a.url).await?;
            let _ = tokio::fs::remove_file(&tmp).await;
        }
    }

    let probe = data_dir.join(a.probe_rel);
    if !probe.exists() {
        return Err(format!(
            "artifact {} installed but expected file missing: {}",
            a.id,
            probe.display()
        ));
    }
    progress(ModelProgress {
        id: a.id.into(),
        downloaded,
        total,
        stage: "done".into(),
        error: None,
    });
    Ok(probe)
}

/// Extract zip/tar.bz2/tar.xz fully in-process — no external tools. Windows'
/// bundled bsdtar delegates bzip2 to an external binary most machines lack,
/// so shelling out breaks .tar.bz2 artifacts on clean installs. The format
/// comes from `src_name` (the artifact URL) because the downloaded temp file
/// carries a `{id}.download.{pid}.{n}` name, not the archive's extension.
async fn extract_archive(archive: &Path, dest: &Path, src_name: &str) -> Result<(), String> {
    let archive = archive.to_path_buf();
    let dest = dest.to_path_buf();
    let name = src_name.to_ascii_lowercase();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&archive)
            .map_err(|e| format!("extraction failed: cannot open archive: {e}"))?;
        let reader = std::io::BufReader::new(file);
        if name.ends_with(".zip") {
            zip::ZipArchive::new(reader)
                .and_then(|mut z| z.extract(&dest))
                .map_err(|e| format!("extraction failed: {e}"))
        } else if name.ends_with(".tar.bz2") {
            tar::Archive::new(bzip2::read::BzDecoder::new(reader))
                .unpack(&dest)
                .map_err(|e| format!("extraction failed: {e}"))
        } else if name.ends_with(".tgz") || name.ends_with(".tar.gz") {
            tar::Archive::new(flate2::read::GzDecoder::new(reader))
                .unpack(&dest)
                .map_err(|e| format!("extraction failed: {e}"))
        } else if name.ends_with(".tar.xz") {
            tar::Archive::new(xz2::read::XzDecoder::new(reader))
                .unpack(&dest)
                .map_err(|e| format!("extraction failed: {e}"))
        } else {
            Err(format!(
                "extraction failed: unsupported archive format: {name}"
            ))
        }
    })
    .await
    .map_err(|e| format!("extraction task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A tar stream holding `inner/dir/probe.txt`, mirroring the nested
    /// layout of the real artifacts (archive root dir + probe file below it).
    fn tar_bytes() -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let data = b"probe";
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        b.append_data(&mut header, "inner/dir/probe.txt", &data[..])
            .unwrap();
        b.into_inner().unwrap()
    }

    /// Extract `bytes` (written to a `{id}.download`-style temp name, like
    /// `ensure` does) and assert the probe file appears under dest.
    async fn assert_extracts(bytes: Vec<u8>, src_name: &str, probe: &str) {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("fixture.download");
        std::fs::write(&archive, bytes).unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        extract_archive(&archive, &dest, src_name).await.unwrap();
        let probe = dest.join(probe);
        assert!(probe.is_file(), "missing probe file {}", probe.display());
        assert_eq!(std::fs::read(&probe).unwrap(), b"probe");
    }

    #[tokio::test]
    async fn extracts_tar_bz2_in_process() {
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        enc.write_all(&tar_bytes()).unwrap();
        assert_extracts(
            enc.finish().unwrap(),
            "https://example.com/sherpa-onnx-v1.13.3.tar.bz2",
            "inner/dir/probe.txt",
        )
        .await;
    }

    #[tokio::test]
    async fn extracts_tgz_in_process() {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&tar_bytes()).unwrap();
        assert_extracts(
            enc.finish().unwrap(),
            "https://example.com/ollama-darwin.tgz",
            "inner/dir/probe.txt",
        )
        .await;
    }

    #[tokio::test]
    async fn extracts_tar_xz_in_process() {
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 6);
        enc.write_all(&tar_bytes()).unwrap();
        assert_extracts(
            enc.finish().unwrap(),
            "https://example.com/ffmpeg-n8.1.tar.xz",
            "inner/dir/probe.txt",
        )
        .await;
    }

    #[tokio::test]
    async fn extracts_zip_in_process() {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zw.start_file("inner/dir/probe.txt", opts).unwrap();
        zw.write_all(b"probe").unwrap();
        assert_extracts(
            zw.finish().unwrap().into_inner(),
            "https://example.com/whisper-bin-x64.zip",
            "inner/dir/probe.txt",
        )
        .await;
    }

    #[tokio::test]
    async fn rejects_unknown_archive_format() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("fixture.download");
        std::fs::write(&archive, b"junk").unwrap();
        let err = extract_archive(&archive, tmp.path(), "https://example.com/model.7z")
            .await
            .unwrap_err();
        assert!(err.contains("unsupported archive format"), "{err}");
    }

    /// Concurrent downloads of the same artifact must never share a temp
    /// file — interleaved writers plus stream-side hashing could install
    /// corrupt bytes. Every attempt gets its own path, under the data dir,
    /// still recognizably named after the artifact.
    #[test]
    fn download_tmp_paths_are_unique_per_attempt() {
        let dir = Path::new("data");
        let a = download_tmp_path(dir, "whisper-bin");
        let b = download_tmp_path(dir, "whisper-bin");
        assert_ne!(a, b, "two attempts must not share a temp file");
        for p in [&a, &b] {
            assert_eq!(p.parent(), Some(dir));
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                name.starts_with("whisper-bin.download."),
                "temp name should stay attributable to its artifact: {name}"
            );
        }
    }

    /// The opt-out drops the mirror and leaves only the pinned primary.
    #[test]
    fn candidate_urls_opt_out_disables_mirror() {
        let url = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/x.bin";
        assert_eq!(candidate_urls_with(url, false), vec![url.to_string()]);
        assert_eq!(candidate_urls_with(url, true).len(), 2);
    }

    /// A checksum mismatch (the exact message download_and_verify produces)
    /// must not be retried against the same URL — the source is
    /// deterministically serving the wrong file; move to the next source.
    /// Transient errors keep their same-URL retry.
    #[test]
    fn checksum_mismatch_advances_to_next_source() {
        assert!(!retry_same_url(
            "checksum mismatch (expected abc123, got def456)"
        ));
        assert!(retry_same_url("download failed: connection reset"));
        assert!(retry_same_url(
            "download interrupted: unexpected end of stream"
        ));
    }

    /// Hugging Face URLs gain the hf-mirror.com fallback; everything else
    /// (GitHub-hosted tools) stays single-source.
    #[test]
    fn candidate_urls_mirror_only_for_hf() {
        let hf = candidate_urls("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/x.bin");
        assert_eq!(
            hf,
            vec![
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/x.bin",
                "https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/x.bin",
            ]
        );
        let gh =
            candidate_urls("https://github.com/k2-fsa/sherpa-onnx/releases/download/x.tar.bz2");
        assert_eq!(gh.len(), 1);
    }

    /// The stand-in for a missing model prefers the largest installed model
    /// AT OR BELOW the wanted one — a Light-tier machine must not get pushed
    /// onto a leftover large-v3. Only when everything installed is bigger
    /// does the smallest larger model win (slow rescue beats no rescue).
    #[test]
    fn best_installed_stays_at_or_below_the_wanted_model() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(best_installed_asr_model(tmp.path(), "ggml-small-q5_1").is_none());
        for id in ["ggml-small-q5_1", "ggml-large-v3-q5_0"] {
            let a = artifact(id).unwrap();
            let p = tmp.path().join(a.probe_rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
        }
        // Wanted large → the largest installed wins.
        let (id, _) = best_installed_asr_model(tmp.path(), "ggml-large-v3-q5_0").unwrap();
        assert_eq!(id, "ggml-large-v3-q5_0");
        // Wanted small (Light tier) → the leftover large model must NOT win.
        let (id, _) = best_installed_asr_model(tmp.path(), "ggml-small-q5_1").unwrap();
        assert_eq!(id, "ggml-small-q5_1");
        // Wanted medium, only small + large installed → small (at-or-below).
        let (id, _) = best_installed_asr_model(tmp.path(), "ggml-medium-q5_0").unwrap();
        assert_eq!(id, "ggml-small-q5_1");
    }

    /// Only larger models installed → the smallest of them stands in.
    #[test]
    fn best_installed_falls_up_only_when_nothing_smaller_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let a = artifact("ggml-large-v3-q5_0").unwrap();
        let p = tmp.path().join(a.probe_rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
        let (id, _) = best_installed_asr_model(tmp.path(), "ggml-small-q5_1").unwrap();
        assert_eq!(id, "ggml-large-v3-q5_0");
    }

    /// Stale download temps (dead pids) are reclaimed; fresh ones survive.
    #[test]
    fn sweep_removes_stale_download_temps_only() {
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp.path().join("ggml-small-q5_1.download.9999.0");
        let fresh = tmp.path().join("whisper-bin.download.1234.1");
        let unrelated = tmp.path().join("notes.md");
        for f in [&stale, &fresh, &unrelated] {
            std::fs::write(f, b"x").unwrap();
        }
        // Age 0 → everything qualifying is stale; the non-temp file survives.
        sweep_stale_downloads(tmp.path(), std::time::Duration::ZERO);
        assert!(!stale.exists() && !fresh.exists());
        assert!(unrelated.exists(), "non-temp files are never touched");

        // A just-written temp survives a real (1 h) age gate.
        std::fs::write(&fresh, b"x").unwrap();
        sweep_stale_downloads(tmp.path(), std::time::Duration::from_secs(3600));
        assert!(fresh.exists());
    }

    /// A managed install (probe file present) reports ready even when the tool
    /// is nowhere on PATH — this is the branch that distinguishes a real,
    /// runnable engine from bare weights.
    #[test]
    fn tool_installed_sees_managed_probe() {
        let tmp = tempfile::tempdir().unwrap();
        // sherpa-bin exists in TOOLS on every OS; fabricate its probe file.
        let a = artifact("sherpa-bin").expect("sherpa-bin artifact");
        let probe = tmp.path().join(a.probe_rel);
        std::fs::create_dir_all(probe.parent().unwrap()).unwrap();
        std::fs::write(&probe, b"x").unwrap();
        // A name that won't resolve on PATH, so this asserts the managed hit only.
        assert!(tool_installed(
            tmp.path(),
            "sherpa-bin",
            &["fly-nonexistent-binary-xyz"]
        ));
    }

    /// No managed artifact and nothing on PATH → not installed. This is the
    /// macOS/Linux state that must surface as "engine missing" rather than a
    /// transcribe-time crash.
    #[test]
    fn tool_installed_false_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!tool_installed(
            tmp.path(),
            "fly-unknown-artifact",
            &["fly-nonexistent-binary-xyz"]
        ));
    }
}
