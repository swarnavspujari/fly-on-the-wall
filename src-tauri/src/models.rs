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
///
/// Engine `dest_rel`s carry the version (`bin/whisper/v1.9.5`): an install is
/// recognized purely by its probe file existing, so an unversioned path would
/// leave every existing install on the old engine forever. Bumping the
/// version dir makes the next transcribe fetch the new build; the old one is
/// simply no longer referenced.
#[cfg(target_os = "windows")]
const TOOLS: &[Artifact] = &[
    Artifact {
        id: "whisper-bin",
        // v1.9.5 (d1be6fd) fixes CVE-2026-110810. Upstream attaches the
        // Windows binaries to the `b5454` release of that same commit (the
        // v1.9.5 release itself has no assets); digest = GitHub asset digest,
        // re-hashed locally 2026-10-23.
        display: "whisper.cpp CLI (CPU, v1.9.5)",
        url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip",
        sha256: "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb",
        bytes: 8_928_640,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/whisper/v1.9.5",
        probe_rel: "bin/whisper/v1.9.5/Release/whisper-cli.exe",
    },
    // Upstream publishes no Vulkan Windows binary (only CPU/BLAS/CUDA-only),
    // so this is whisper.cpp v1.9.5 (d1be6fd) built with GGML_VULKAN by
    // .github/workflows/build-whisper-sidecar.yml (run 40054818817) and
    // hosted as a tools release on this repo — one cross-vendor GPU build for
    // NVIDIA/AMD/Intel. Selection is gated by a per-machine benchmark
    // (gpu.rs); the CPU entry above stays the default and is never touched.
    // Pin re-hashed locally from the hosted asset 2026-10-23.
    Artifact {
        id: "whisper-bin-vulkan",
        display: "whisper.cpp CLI (Vulkan GPU, v1.9.5)",
        url: "https://github.com/swarnavspujari/fly-on-the-wall/releases/download/tools-whisper-vulkan-v1.9.5/whisper-bin-x64-vulkan-v1.9.5.zip",
        sha256: "15b9797efe64cf69a8cdd2089f2116e9fa83165f732b9baacd65c56496ba1a1c",
        bytes: 17_360_733,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/whisper-vulkan/v1.9.5",
        probe_rel: "bin/whisper-vulkan/v1.9.5/Release/whisper-cli.exe",
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
        // BtbN keeps month-end autobuilds long-term (dailies are pruned after
        // ~2 weeks), so pins always use a month-end tag. Digest = GitHub
        // asset digest, re-hashed locally 2026-10-23.
        display: "ffmpeg (n8.1, screen capture + media import)",
        url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/ffmpeg-n8.1.3-9-g29e619e767-win64-gpl-shared-8.1.zip",
        sha256: "dfe81c3aa0a546ee81980b1b824486dd0fe0a1d452f00cb55135379a74be7bcf",
        bytes: 90_079_410,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/ffmpeg",
        probe_rel: "bin/ffmpeg/ffmpeg-n8.1.3-9-g29e619e767-win64-gpl-shared-8.1/bin/ffmpeg.exe",
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
        url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/ffmpeg-n8.1.3-9-g29e619e767-linux64-gpl-shared-8.1.tar.xz",
        sha256: "76649c19a7a4615b367251ba4a8adc643f65831278a00e7a0c38398f7f06484b",
        bytes: 71_911_604,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/ffmpeg",
        probe_rel: "bin/ffmpeg/ffmpeg-n8.1.3-9-g29e619e767-linux64-gpl-shared-8.1/bin/ffmpeg",
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
    // v1.9.5 (d1be6fd), workflow run 40054818817. Pin verified 2026-10-23 by
    // hashing the hosted asset independently of the workflow's summary; the
    // build asserted both x86_64 + arm64 slices (lipo).
    Artifact {
        id: "whisper-bin",
        display: "whisper.cpp CLI (macOS universal, v1.9.5)",
        url: "https://github.com/swarnavspujari/fly-on-the-wall/releases/download/tools-whisper-v1.9.5/whisper-bin-macos-universal2-v1.9.5.tar.bz2",
        sha256: "b5d4fc49705683c61034650d18b9ec2688a28dab73eb3f959d3e24af5abfa012",
        bytes: 3_141_122,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/whisper/v1.9.5",
        probe_rel: "bin/whisper/v1.9.5/whisper-cli",
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
/// path (lib.rs). Also retries pruning superseded tool installs (a prune
/// that failed because an old binary was still running), skipped while an
/// install holds the install lock.
pub fn sweep_stale_downloads(data_dir: &Path, max_age: std::time::Duration) {
    if let Ok(_guard) = INSTALL_LOCK.try_lock() {
        for a in TOOLS {
            if installed_path(data_dir, a).is_some() {
                prune_superseded(data_dir, a);
            }
        }
    }
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
            install_archive(&tmp, data_dir, a, extract_cap(a.bytes.max(downloaded))).await?;
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

/// Serializes archive installs within the process. Extraction goes through a
/// fixed `<dest>.partial` staging dir, and two tasks (live captions + a
/// post-meeting transcribe on a fresh machine) can race to install the same
/// engine; holding this while staging/promoting means the loser simply finds
/// the winner's install. Downloads happen before it, so a slow download never
/// blocks anyone. Installs are seconds long, so one global lock is enough.
static INSTALL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Install a downloaded, SHA-verified archive for `a`, all or nothing:
/// extract into `<dest>.partial` (a stale one from a crash is removed
/// first), then move its top-level entries into `dest` — the entry holding
/// the probe file last, so the probe never exists before everything it
/// needs. A failed extraction leaves `dest` untouched and removes the
/// staging dir. The downloaded archive is always deleted. Afterwards,
/// superseded installs of the same tool are pruned (best effort).
async fn install_archive(
    archive: &Path,
    data_dir: &Path,
    a: &Artifact,
    max_bytes: u64,
) -> Result<(), String> {
    let _guard = INSTALL_LOCK.lock().await;
    let result = async {
        if installed_path(data_dir, a).is_some() {
            return Ok(()); // another task installed it while we downloaded
        }
        let dest = data_dir.join(a.dest_rel);
        let staging = staging_dir(&dest);
        remove_path(&staging)
            .map_err(|e| format!("extraction failed: cannot clear staging dir: {e}"))?;
        std::fs::create_dir_all(&staging).map_err(|e| format!("extraction failed: {e}"))?;
        if let Err(e) = extract_archive(archive, &staging, a.url, max_bytes).await {
            let _ = remove_path(&staging);
            return Err(e);
        }
        let probe_entry = Path::new(a.probe_rel)
            .strip_prefix(a.dest_rel)
            .ok()
            .and_then(|p| p.components().next())
            .map(|c| c.as_os_str().to_os_string());
        let promoted = tokio::task::spawn_blocking({
            let (staging, dest) = (staging.clone(), dest.clone());
            move || promote_staging(&staging, &dest, probe_entry.as_deref())
        })
        .await
        .map_err(|e| format!("install task failed: {e}"))?;
        if let Err(e) = promoted {
            let _ = remove_path(&staging);
            return Err(format!("install failed: {e}"));
        }
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_file(archive).await;
    if result.is_ok() && installed_path(data_dir, a).is_some() {
        prune_superseded(data_dir, a);
    }
    result
}

/// `<dest>.partial`, next to `dest`.
fn staging_dir(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    dest.with_file_name(name)
}

/// Remove a file, directory tree or link at `p` without following links
/// (`remove_dir_all` doesn't traverse symlinks/junctions). Missing is fine.
fn remove_path(p: &Path) -> std::io::Result<()> {
    let meta = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() {
        // A directory symlink/junction on Windows needs remove_dir.
        std::fs::remove_file(p).or_else(|_| std::fs::remove_dir(p))
    } else if meta.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

/// Move every top-level entry of `staging` into `dest` (replacing same-name
/// entries — that's the repair path for a broken earlier install), the
/// `last` entry (the one containing the probe) after all others, then drop
/// the empty staging dir.
fn promote_staging(
    staging: &Path,
    dest: &Path,
    last: Option<&std::ffi::OsStr>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    let mut names = std::fs::read_dir(staging)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort_by_key(|n| Some(n.as_os_str()) == last);
    for name in names {
        let to = dest.join(&name);
        remove_path(&to)?;
        std::fs::rename(staging.join(&name), &to)?;
    }
    std::fs::remove_dir(staging)
}

/// Tools whose installs live in a versioned subdirectory of `bin/<tool>/`
/// (the whisper engines' `v1.9.5`, ffmpeg's / sherpa's archive root dir), so
/// every other entry there is a superseded or legacy install. Ollama is
/// absent on purpose: its archive root IS `bin/ollama`.
const PRUNABLE_TOOLS: &[&str] = &["whisper-bin", "whisper-bin-vulkan", "ffmpeg", "sherpa-bin"];

/// `(bin/<tool>, <current entry>)` for a prunable artifact, from its probe
/// path `bin/<tool>/<current entry>/...`.
fn prune_plan(a: &Artifact) -> Option<(PathBuf, &'static str)> {
    if !PRUNABLE_TOOLS.contains(&a.id) {
        return None;
    }
    let parts: Vec<&'static str> = a.probe_rel.split('/').collect();
    match parts.as_slice() {
        ["bin", tool, keep, _, ..] => Some((Path::new("bin").join(tool), *keep)),
        _ => None,
    }
}

/// Delete everything in `bin/<tool>/` except the current install of `a`:
/// older version dirs (whose binaries may carry fixed CVEs, e.g. whisper.cpp
/// v1.9.1's CVE-2026-110810), the pre-versioning `Release/` layout, stale
/// `.partial` staging dirs. Strictly inside the data dir's `bin/`: bails if
/// `bin` or `bin/<tool>` is itself a link, and removes links found inside
/// without following them. Best effort — a file in use (Windows) just logs
/// a warning and is retried at the next startup sweep.
fn prune_superseded(data_dir: &Path, a: &Artifact) {
    let Some((root_rel, keep)) = prune_plan(a) else {
        return;
    };
    let real_dir = |p: &Path| {
        std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
    };
    let root = data_dir.join(&root_rel);
    if !real_dir(&data_dir.join("bin")) || !real_dir(&root) || !root.join(keep).is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == keep {
            continue;
        }
        match remove_path(&entry.path()) {
            Ok(()) => tracing::info!(
                artifact = a.id,
                entry = %name.to_string_lossy(),
                "removed superseded tool install"
            ),
            Err(e) => tracing::warn!(
                artifact = a.id,
                entry = %name.to_string_lossy(),
                error = %e,
                "could not remove superseded tool install"
            ),
        }
    }
}

/// Upper bound on what one archive may extract to: `EXTRACT_MAX_RATIO` × its
/// pinned download size, but never below `EXTRACT_MIN_CAP`. Every pinned
/// archive today expands well under 5× (the Vulkan whisper zip is ~3.3×), so
/// this only trips on a decompression bomb. Inputs are SHA-256-verified
/// before extraction; the cap is defense in depth.
const EXTRACT_MAX_RATIO: u64 = 10;
const EXTRACT_MIN_CAP: u64 = 1 << 30; // 1 GiB

fn extract_cap(archive_bytes: u64) -> u64 {
    archive_bytes
        .saturating_mul(EXTRACT_MAX_RATIO)
        .max(EXTRACT_MIN_CAP)
}

fn too_large(max_bytes: u64) -> String {
    format!("extraction failed: archive expands past the {max_bytes}-byte limit")
}

/// Extract zip/tar.bz2/tar.xz fully in-process — no external tools. Windows'
/// bundled bsdtar delegates bzip2 to an external binary most machines lack,
/// so shelling out breaks .tar.bz2 artifacts on clean installs. The format
/// comes from `src_name` (the artifact URL) because the downloaded temp file
/// carries a `{id}.download.{pid}.{n}` name, not the archive's extension.
/// At most `max_bytes` of file data is written (see [`extract_cap`]).
async fn extract_archive(
    archive: &Path,
    dest: &Path,
    src_name: &str,
    max_bytes: u64,
) -> Result<(), String> {
    let archive = archive.to_path_buf();
    let dest = dest.to_path_buf();
    let name = src_name.to_ascii_lowercase();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&archive)
            .map_err(|e| format!("extraction failed: cannot open archive: {e}"))?;
        let reader = std::io::BufReader::new(file);
        if name.ends_with(".zip") {
            extract_zip(reader, &dest, max_bytes)
        } else if name.ends_with(".tar.bz2") {
            extract_tar(bzip2::read::BzDecoder::new(reader), &dest, max_bytes)
        } else if name.ends_with(".tgz") || name.ends_with(".tar.gz") {
            extract_tar(flate2::read::GzDecoder::new(reader), &dest, max_bytes)
        } else if name.ends_with(".tar.xz") {
            extract_tar(xz2::read::XzDecoder::new(reader), &dest, max_bytes)
        } else {
            Err(format!(
                "extraction failed: unsupported archive format: {name}"
            ))
        }
    })
    .await
    .map_err(|e| format!("extraction task failed: {e}"))?
}

/// Zip extraction done entry by entry instead of `ZipArchive::extract`, so
/// the rules are ours and can't drift with the crate:
/// - every entry path must be plain relative components — absolute paths,
///   drive prefixes and any `..` / `.` are rejected (not just ones that
///   escape `dest`);
/// - symlink entries are rejected;
/// - files are created with default permissions; on Unix the only mode bit
///   honored is "executable" (→ 0o755), so setuid/world-writable bits in an
///   archive never reach disk;
/// - total bytes actually written are capped at `max_bytes` (the header's
///   declared sizes are not trusted).
fn extract_zip<R: std::io::Read + std::io::Seek>(
    reader: R,
    dest: &Path,
    max_bytes: u64,
) -> Result<(), String> {
    use std::io::Read;
    let fail = |e: &dyn std::fmt::Display| format!("extraction failed: {e}");
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| fail(&e))?;
    let mut written: u64 = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| fail(&e))?;
        let raw = entry
            .name()
            .map(|n| n.into_owned())
            .unwrap_or_else(|_| format!("<entry {i}>"));
        if entry.is_symlink() {
            return Err(format!("extraction failed: symlink entry rejected: {raw}"));
        }
        let rel = entry
            .name()
            .ok()
            .and_then(|n| safe_zip_path(&n))
            // Belt and braces: the crate's own check must agree too.
            .filter(|_| entry.enclosed_name().is_some())
            .ok_or_else(|| format!("extraction failed: unsafe entry path rejected: {raw}"))?;
        let out = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| fail(&e))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| fail(&e))?;
        }
        let mut file = std::fs::File::create(&out).map_err(|e| fail(&e))?;
        let budget = max_bytes - written;
        // Read one byte past the budget so an over-limit entry is detected
        // rather than silently truncated.
        let n = std::io::copy(&mut (&mut entry).take(budget.saturating_add(1)), &mut file)
            .map_err(|e| fail(&e))?;
        if n > budget {
            drop(file);
            let _ = std::fs::remove_file(&out);
            return Err(too_large(max_bytes));
        }
        written += n;
        #[cfg(unix)]
        if entry.unix_mode().is_some_and(|m| m & 0o111 != 0) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| fail(&e))?;
        }
    }
    Ok(())
}

/// Validate a zip entry's RAW name (not a crate-simplified one — zip 9's
/// `enclosed_name` rewrites `a/../b` to `b` and strips a leading `/` rather
/// than rejecting them). Accepts only `/`- or `\`-separated plain names:
/// no leading separator, no drive/stream colon, no empty, `.` or `..`
/// component. A single trailing separator (directory entry) is allowed.
fn safe_zip_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw
        .strip_suffix('/')
        .or_else(|| raw.strip_suffix('\\'))
        .unwrap_or(raw);
    if trimmed.is_empty() {
        return None;
    }
    let mut out = PathBuf::new();
    for part in trimmed.split(['/', '\\']) {
        // Same per-component rules as data-dir paths from IPC/manifests:
        // also rejects reserved device names, trailing dots/spaces,
        // `<>:"|?*` and control characters.
        if !fly_storage::safe_path::is_plain_name(part) {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// A tar symlink target we allow: relative and descending only — plain-name
/// components, no `..`, no root/drive. Such a link can only point at its own
/// directory's contents, so no chain of them can lead outside the
/// destination. Every link in the pinned archives (sherpa / ollama / ffmpeg
/// versioned shared libraries) is a bare same-directory file name.
fn safe_link_target(target: &Path) -> bool {
    target
        .to_str()
        .is_some_and(|t| !t.is_empty() && t.split('/').all(fly_storage::safe_path::is_plain_name))
}

/// Tar extraction with the same byte cap. Entry paths are vetted by the
/// `tar` crate's `unpack_in` (tar 0.4.46: `..` entries and writes through a
/// link leading outside `dest` are refused; absolute paths are re-rooted
/// inside `dest`); an entry it refuses fails the whole extraction instead of
/// being silently skipped. Hard links are rejected outright (none of the pinned archives
/// has one); symlinks must pass [`safe_link_target`] — the pinned
/// sherpa/ollama/ffmpeg tarballs need them for versioned shared libraries.
/// Directories are applied last, like `Archive::unpack`, so a read-only
/// directory can't block the files inside it.
fn extract_tar<R: std::io::Read>(reader: R, dest: &Path, max_bytes: u64) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("extraction failed: {e}");
    std::fs::create_dir_all(dest).map_err(|e| fail(&e))?;
    let dest = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());
    let mut archive = tar::Archive::new(reader);
    let mut declared: u64 = 0;
    let mut dirs = Vec::new();
    for entry in archive.entries().map_err(|e| fail(&e))? {
        let mut entry = entry.map_err(|e| fail(&e))?;
        declared = declared.saturating_add(entry.size());
        if declared > max_bytes {
            return Err(too_large(max_bytes));
        }
        let name = entry
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "<unreadable name>".into());
        let kind = entry.header().entry_type();
        if kind.is_hard_link() {
            return Err(format!(
                "extraction failed: hard link entry rejected: {name}"
            ));
        }
        if kind.is_symlink() {
            let target = entry.link_name().map_err(|e| fail(&e))?;
            if !target.as_deref().is_some_and(safe_link_target) {
                return Err(format!(
                    "extraction failed: symlink entry escapes the archive: {name}"
                ));
            }
        }
        if kind.is_dir() {
            dirs.push((name, entry));
        } else if !entry.unpack_in(&dest).map_err(|e| fail(&e))? {
            return Err(format!(
                "extraction failed: unsafe entry path rejected: {name}"
            ));
        }
    }
    for (name, mut dir) in dirs {
        if !dir.unpack_in(&dest).map_err(|e| fail(&e))? {
            return Err(format!(
                "extraction failed: unsafe entry path rejected: {name}"
            ));
        }
    }
    Ok(())
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
        extract_archive(&archive, &dest, src_name, EXTRACT_MIN_CAP)
            .await
            .unwrap();
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
        let err = extract_archive(
            &archive,
            tmp.path(),
            "https://example.com/model.7z",
            EXTRACT_MIN_CAP,
        )
        .await
        .unwrap_err();
        assert!(err.contains("unsupported archive format"), "{err}");
    }

    /// Build a zip in memory; `entries` are (name, data).
    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    /// Extract `bytes` as `src_name` into a fresh dest; returns (tempdir,
    /// dest, result) so callers can inspect what did or didn't land.
    async fn try_extract(
        bytes: Vec<u8>,
        src_name: &str,
        max_bytes: u64,
    ) -> (tempfile::TempDir, PathBuf, Result<(), String>) {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("fixture.download");
        std::fs::write(&archive, bytes).unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let res = extract_archive(&archive, &dest, src_name, max_bytes).await;
        (tmp, dest, res)
    }

    #[tokio::test]
    async fn zip_rejects_parent_dir_entries() {
        for name in ["../evil.txt", "inner/../../evil.txt", "inner/../ok.txt"] {
            let (tmp, _dest, res) = try_extract(
                zip_with(&[(name, b"x")]),
                "https://example.com/a.zip",
                EXTRACT_MIN_CAP,
            )
            .await;
            let err = res.expect_err(name);
            assert!(err.contains("unsafe entry path"), "{name}: {err}");
            assert!(!tmp.path().join("evil.txt").exists());
        }
    }

    #[tokio::test]
    async fn zip_rejects_absolute_entries() {
        for name in ["/abs.txt", "C:/abs.txt", "\\\\server\\share\\abs.txt"] {
            let (_tmp, _dest, res) = try_extract(
                zip_with(&[(name, b"x")]),
                "https://example.com/a.zip",
                EXTRACT_MIN_CAP,
            )
            .await;
            let err = res.expect_err(name);
            assert!(err.contains("unsafe entry path"), "{name}: {err}");
        }
    }

    #[tokio::test]
    async fn zip_rejects_symlink_entries() {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zw.add_symlink(
            "link",
            "/etc/passwd",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        let (_tmp, dest, res) = try_extract(
            zw.finish().unwrap().into_inner(),
            "https://example.com/a.zip",
            EXTRACT_MIN_CAP,
        )
        .await;
        let err = res.unwrap_err();
        assert!(err.contains("symlink entry rejected"), "{err}");
        assert!(std::fs::symlink_metadata(dest.join("link")).is_err());
    }

    #[tokio::test]
    async fn zip_enforces_extracted_size_cap() {
        let big = vec![0u8; 64 * 1024]; // compresses to almost nothing
        let bytes = zip_with(&[("a.bin", &big[..]), ("b.bin", &big[..])]);
        // Fits exactly…
        let (_t, dest, res) =
            try_extract(bytes.clone(), "https://example.com/a.zip", 128 * 1024).await;
        res.unwrap();
        assert_eq!(
            std::fs::metadata(dest.join("b.bin")).unwrap().len(),
            64 * 1024
        );
        // …one byte less and the second entry trips the cap.
        let (_t, dest, res) = try_extract(bytes, "https://example.com/a.zip", 128 * 1024 - 1).await;
        let err = res.unwrap_err();
        assert!(err.contains("limit"), "{err}");
        assert!(
            !dest.join("b.bin").exists(),
            "over-limit file must not be left behind"
        );
    }

    #[tokio::test]
    async fn tar_enforces_extracted_size_cap() {
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 6);
        enc.write_all(&tar_bytes()).unwrap(); // one 5-byte file
        let bytes = enc.finish().unwrap();
        let (_t, _d, res) = try_extract(bytes.clone(), "https://example.com/a.tar.xz", 5).await;
        res.unwrap();
        let (_t, _d, res) = try_extract(bytes, "https://example.com/a.tar.xz", 4).await;
        assert!(res.unwrap_err().contains("limit"));
    }

    /// Only the executable bit survives from an archive's Unix mode.
    #[cfg(unix)]
    #[tokio::test]
    async fn zip_applies_only_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let base = zip::write::SimpleFileOptions::default();
        zw.start_file("tool", base.unix_permissions(0o4777))
            .unwrap();
        zw.write_all(b"#!/bin/sh\n").unwrap();
        zw.start_file("data.txt", base.unix_permissions(0o666))
            .unwrap();
        zw.write_all(b"d").unwrap();
        let (_t, dest, res) = try_extract(
            zw.finish().unwrap().into_inner(),
            "https://example.com/a.zip",
            EXTRACT_MIN_CAP,
        )
        .await;
        res.unwrap();
        let mode = |p: &str| {
            std::fs::metadata(dest.join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        };
        assert_eq!(mode("tool"), 0o755);
        assert_eq!(
            // Group-write may come from the process umask; the archive's
            // 0o666 must not add world-write or any special bit.
            mode("data.txt") & 0o7002,
            0,
            "no setuid/setgid/sticky or world-write bits"
        );
    }

    #[test]
    fn safe_zip_path_accepts_only_plain_relative_names() {
        assert_eq!(
            safe_zip_path("Release/whisper-cli.exe"),
            Some(PathBuf::from("Release").join("whisper-cli.exe"))
        );
        assert_eq!(safe_zip_path("Release/"), Some(PathBuf::from("Release")));
        assert_eq!(
            safe_zip_path("Release\\ggml.dll"),
            Some(PathBuf::from("Release").join("ggml.dll"))
        );
        for bad in [
            "",
            "/",
            "/abs",
            "\\abs",
            "C:/x",
            "C:x",
            "a/../b",
            "../a",
            "a/./b",
            "a//b",
            "a/b/..",
            "file.txt:ads",
            "a\0b",
            // fly_storage::safe_path rules: device names, trailing dot/space,
            // wildcard/redirect chars, control chars.
            "NUL",
            "Release/con",
            "Release/COM1.dll",
            "Release/lpt9.txt",
            "Release/x.",
            "Release/x ",
            "Release/ x",
            "Release/a<b",
            "Release/a>b",
            "Release/a|b",
            "Release/a?b",
            "Release/a*b",
            "Release/a\"b",
            "Release/a\u{1}b",
        ] {
            assert_eq!(safe_zip_path(bad), None, "{bad:?} must be rejected");
        }
    }

    // ---- tar link entries ----

    /// A tar holding one link entry `name` → `target` of `kind`.
    fn tar_with_link(kind: tar::EntryType, name: &str, target: &str) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(kind);
        h.set_size(0);
        h.set_mode(0o777);
        b.append_link(&mut h, name, target).unwrap();
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 6);
        enc.write_all(&b.into_inner().unwrap()).unwrap();
        enc.finish().unwrap()
    }

    #[tokio::test]
    async fn tar_rejects_hard_links() {
        for target in ["../../outside.txt", "/etc/passwd", "inner/ok.txt"] {
            let bytes = tar_with_link(tar::EntryType::Link, "inner/link", target);
            let (tmp, dest, res) =
                try_extract(bytes, "https://example.com/a.tar.xz", EXTRACT_MIN_CAP).await;
            let err = res.expect_err(target);
            assert!(err.contains("hard link entry rejected"), "{target}: {err}");
            assert!(std::fs::symlink_metadata(dest.join("inner/link")).is_err());
            drop(tmp);
        }
    }

    #[tokio::test]
    async fn tar_rejects_symlinks_escaping_the_destination() {
        for target in [
            "../../outside.txt",
            "..",
            "/etc/passwd",
            "sub/../../x",
            "C:/Windows/System32",
            "\\\\server\\share",
        ] {
            let bytes = tar_with_link(tar::EntryType::Symlink, "inner/link", target);
            let (_tmp, dest, res) =
                try_extract(bytes, "https://example.com/a.tar.xz", EXTRACT_MIN_CAP).await;
            let err = res.expect_err(target);
            assert!(err.contains("symlink entry escapes"), "{target:?}: {err}");
            assert!(std::fs::symlink_metadata(dest.join("inner/link")).is_err());
        }
    }

    #[test]
    fn safe_link_targets_are_descending_plain_names() {
        for ok in [
            "libonnxruntime.1.17.1.dylib",
            "libavutil.so.60.26.103",
            "sub/lib.so",
        ] {
            assert!(safe_link_target(Path::new(ok)), "{ok}");
        }
        for bad in [
            "", "..", "../x", "a/../b", "/abs", "./x", "a//b", "C:x", "NUL",
        ] {
            assert!(!safe_link_target(Path::new(bad)), "{bad:?}");
        }
    }

    /// The real archives' shape — a versioned library plus a same-directory
    /// symlink to it — still extracts (creating symlinks needs no privilege
    /// on Unix, unlike Windows).
    #[cfg(unix)]
    #[tokio::test]
    async fn tar_accepts_same_directory_library_symlinks() {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(5);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "pkg/lib/libx.so.1.2", &b"probe"[..])
            .unwrap();
        let mut l = tar::Header::new_gnu();
        l.set_entry_type(tar::EntryType::Symlink);
        l.set_size(0);
        b.append_link(&mut l, "pkg/lib/libx.so", "libx.so.1.2")
            .unwrap();
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 6);
        enc.write_all(&b.into_inner().unwrap()).unwrap();
        let (_t, dest, res) = try_extract(
            enc.finish().unwrap(),
            "https://example.com/a.tar.xz",
            EXTRACT_MIN_CAP,
        )
        .await;
        res.unwrap();
        assert_eq!(
            std::fs::read(dest.join("pkg/lib/libx.so")).unwrap(),
            b"probe"
        );
    }

    // ---- staged install + pruning ----

    /// A Windows-whisper-shaped artifact at a fake version, for exercising
    /// install/prune without the network.
    const FAKE_ENGINE: Artifact = Artifact {
        id: "whisper-bin",
        display: "test engine",
        url: "https://example.com/whisper-bin-x64.zip",
        sha256: "",
        bytes: 0,
        kind: ArtifactKind::Archive,
        dest_rel: "bin/whisper/v9.9.9",
        probe_rel: "bin/whisper/v9.9.9/Release/whisper-cli.exe",
    };

    fn put(path: &Path, data: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn staging_dir_sits_next_to_dest() {
        let d = Path::new("data").join("bin/whisper/v1.9.5");
        assert_eq!(
            staging_dir(&d),
            Path::new("data").join("bin/whisper/v1.9.5.partial")
        );
    }

    /// The broken-install bug: an archive whose probe extracts before a bad
    /// entry used to leave the probe on disk, so the half-install looked
    /// complete forever. Now nothing lands in dest, the staging dir is gone
    /// and the downloaded archive is deleted.
    #[tokio::test]
    async fn failed_extraction_leaves_no_partial_install() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let archive = data.join("whisper-bin.download.1.0");
        std::fs::write(
            &archive,
            zip_with(&[
                ("Release/whisper-cli.exe", b"exe"),
                ("Release/../../evil.dll", b"x"),
            ]),
        )
        .unwrap();
        let err = install_archive(&archive, data, &FAKE_ENGINE, EXTRACT_MIN_CAP)
            .await
            .unwrap_err();
        assert!(err.contains("unsafe entry path"), "{err}");
        assert!(
            installed_path(data, &FAKE_ENGINE).is_none(),
            "probe must not exist"
        );
        assert!(!data.join("bin/whisper/v9.9.9").exists());
        assert!(!data.join("bin/whisper/v9.9.9.partial").exists());
        assert!(!archive.exists(), "temp archive must be removed on failure");
    }

    #[tokio::test]
    async fn install_clears_stale_staging_and_repairs_broken_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        // A crash leftover in staging, and a broken earlier install whose
        // Release/ lacks the probe but has a stale file.
        put(
            &data.join("bin/whisper/v9.9.9.partial/Release/junk.dll"),
            b"j",
        );
        put(&data.join("bin/whisper/v9.9.9/Release/stale.dll"), b"s");
        let archive = data.join("whisper-bin.download.1.1");
        std::fs::write(
            &archive,
            zip_with(&[
                ("Release/ggml.dll", b"dll"),
                ("Release/whisper-cli.exe", b"exe"),
            ]),
        )
        .unwrap();
        install_archive(&archive, data, &FAKE_ENGINE, EXTRACT_MIN_CAP)
            .await
            .unwrap();
        let rel = data.join("bin/whisper/v9.9.9/Release");
        assert_eq!(std::fs::read(rel.join("whisper-cli.exe")).unwrap(), b"exe");
        assert_eq!(std::fs::read(rel.join("ggml.dll")).unwrap(), b"dll");
        assert!(!rel.join("stale.dll").exists(), "broken install replaced");
        assert!(
            !rel.join("junk.dll").exists(),
            "stale staging not merged in"
        );
        assert!(!data.join("bin/whisper/v9.9.9.partial").exists());
        assert!(!archive.exists());
    }

    #[tokio::test]
    async fn install_prunes_superseded_and_legacy_engine_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        put(
            &data.join("bin/whisper/Release/whisper-cli.exe"),
            b"v1.9.1 legacy",
        );
        put(
            &data.join("bin/whisper/v1.9.5/Release/whisper-cli.exe"),
            b"older",
        );
        put(&data.join("bin/whisper/stray.txt"), b"x");
        put(
            &data.join("bin/whisper-vulkan/Release/whisper-cli.exe"),
            b"other tool",
        );
        put(&data.join("bin/sherpa/keep/me"), b"other tool");
        let archive = data.join("whisper-bin.download.1.2");
        std::fs::write(&archive, zip_with(&[("Release/whisper-cli.exe", b"new")])).unwrap();
        install_archive(&archive, data, &FAKE_ENGINE, EXTRACT_MIN_CAP)
            .await
            .unwrap();
        let left: Vec<String> = std::fs::read_dir(data.join("bin/whisper"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["v9.9.9"], "only the current version remains");
        // Other tools' dirs are not this artifact's to prune.
        assert!(data
            .join("bin/whisper-vulkan/Release/whisper-cli.exe")
            .exists());
        assert!(data.join("bin/sherpa/keep/me").exists());
    }

    #[test]
    fn prune_plans_only_versioned_tool_layouts() {
        let (root, keep) = prune_plan(&FAKE_ENGINE).unwrap();
        assert_eq!((root, keep), (Path::new("bin").join("whisper"), "v9.9.9"));
        // Ollama's archive root IS bin/ollama — pruning would delete lib/.
        let ollama = Artifact {
            id: "ollama-bin",
            probe_rel: "bin/ollama/ollama.exe",
            dest_rel: "bin/ollama",
            ..FAKE_ENGINE
        };
        assert!(prune_plan(&ollama).is_none());
        // A prunable id with too shallow a probe is refused too.
        let shallow = Artifact {
            probe_rel: "bin/whisper/whisper-cli",
            ..FAKE_ENGINE
        };
        assert!(prune_plan(&shallow).is_none());
        // Every real tool pin that is prunable has a versioned layout.
        for a in TOOLS.iter().filter(|a| PRUNABLE_TOOLS.contains(&a.id)) {
            assert!(prune_plan(a).is_some(), "{}", a.id);
        }
    }

    #[test]
    fn prune_does_nothing_without_the_current_install() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        put(
            &data.join("bin/whisper/Release/whisper-cli.exe"),
            b"only copy",
        );
        prune_superseded(data, &FAKE_ENGINE);
        assert!(data.join("bin/whisper/Release/whisper-cli.exe").exists());
    }

    /// Links inside bin/<tool> are removed as links — their targets survive —
    /// and a linked bin/<tool> itself is never walked.
    #[cfg(unix)]
    #[test]
    fn prune_never_follows_links() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let outside = tmp.path().join("outside");
        put(&outside.join("precious.txt"), b"keep");
        put(
            &data.join("bin/whisper/v9.9.9/Release/whisper-cli.exe"),
            b"exe",
        );
        std::os::unix::fs::symlink(&outside, data.join("bin/whisper/old")).unwrap();
        prune_superseded(&data, &FAKE_ENGINE);
        assert!(std::fs::symlink_metadata(data.join("bin/whisper/old")).is_err());
        assert!(
            outside.join("precious.txt").exists(),
            "link target untouched"
        );

        // bin/<tool> as a link to somewhere else: not pruned at all.
        let data2 = tmp.path().join("data2");
        put(&outside.join("v9.9.9/Release/whisper-cli.exe"), b"exe");
        std::fs::create_dir_all(data2.join("bin")).unwrap();
        std::os::unix::fs::symlink(&outside, data2.join("bin/whisper")).unwrap();
        prune_superseded(&data2, &FAKE_ENGINE);
        assert!(outside.join("precious.txt").exists());
    }

    /// Network E2E: every tool archive pinned for THIS OS (except the 1.5 GB
    /// Ollama runtime) downloads, matches its SHA-256 pin and extracts to its
    /// probe through the app's own `ensure_with` path. Run after bumping pins:
    ///   cargo test -p fly-app --lib pinned_tool_archives -- --ignored
    #[tokio::test]
    #[ignore = "downloads every pinned tool archive (~150 MB on Windows); run with --ignored"]
    async fn pinned_tool_archives_download_verify_and_extract() {
        let tmp = tempfile::tempdir().unwrap();
        let noop = |_p: ModelProgress| {};
        let archives = TOOLS
            .iter()
            .filter(|a| a.kind == ArtifactKind::Archive && a.id != "ollama-bin");
        for a in archives {
            let probe = ensure_with(&noop, tmp.path(), a.id, DownloadEffort::Full)
                .await
                .unwrap_or_else(|e| panic!("{}: {e}", a.id));
            assert_eq!(probe, tmp.path().join(a.probe_rel), "{}", a.id);
            assert!(probe.is_file(), "{}: probe missing", a.id);
            eprintln!("ok: {} ({})", a.id, a.url);
        }
    }

    #[test]
    fn extract_cap_scales_with_archive_size() {
        assert_eq!(extract_cap(1), EXTRACT_MIN_CAP);
        assert_eq!(extract_cap(1_502_730_186), 15_027_301_860);
        assert_eq!(extract_cap(u64::MAX), u64::MAX);
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
