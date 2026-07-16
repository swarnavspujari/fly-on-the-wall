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
/// Linux either) — `ensure_tool` falls back to the same tool on PATH there.
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
const TOOLS: &[Artifact] = &[Artifact {
    id: "sherpa-bin",
    display: "sherpa-onnx diarization CLI (v1.13.3)",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.3/sherpa-onnx-v1.13.3-osx-universal2-shared.tar.bz2",
    sha256: "2317b975f42f5edf3e69068809dec456c068b68e48d091e6b578e7a977227361",
    bytes: 56_024_420,
    kind: ArtifactKind::Archive,
    dest_rel: "bin/sherpa",
    probe_rel: "bin/sherpa/sherpa-onnx-v1.13.3-osx-universal2-shared/bin/sherpa-onnx-offline-speaker-diarization",
}];

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

/// Locate an executable on PATH (used where upstream publishes no binary for
/// this OS — e.g. whisper-cli and ffmpeg on macOS, whisper-cli on Linux).
pub fn find_on_path(names: &[&str]) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
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
    if let Some(a) = artifact(id) {
        if let Some(installed) = installed_path(data_dir, a) {
            return Ok(installed);
        }
    }
    if let Some(found) = find_on_path(path_names) {
        return Ok(found);
    }
    if artifact(id).is_some() {
        return ensure(progress, data_dir, id).await;
    }
    Err(format!("{} is not installed — {}", path_names[0], guidance))
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

/// Best already-installed whisper model, if any — the registry lists ASR
/// models smallest→largest, so the last installed one wins. Lets a meeting
/// still transcribe when the wanted model can't be downloaded (offline, CDN
/// outage) but another model is sitting on disk.
pub fn best_installed_asr_model(data_dir: &Path) -> Option<(&'static str, PathBuf)> {
    registry()
        .filter(|a| a.id.starts_with("ggml-"))
        .filter_map(|a| installed_path(data_dir, a).map(|p| (a.id, p)))
        .last()
}

/// Every URL worth trying for an artifact, in order: the pinned primary,
/// then mirrors derivable from the host. Hugging Face files are also served
/// by hf-mirror.com under the same path — useful when HF's Xet CDN bridge is
/// rejecting downloads (a known intermittent failure).
fn candidate_urls(url: &str) -> Vec<String> {
    let mut v = vec![url.to_string()];
    if let Some(rest) = url.strip_prefix("https://huggingface.co/") {
        v.push(format!("https://hf-mirror.com/{rest}"));
    }
    v
}

/// One download attempt from one URL into `tmp`, streaming progress and
/// verifying the SHA-256 pin. Returns bytes downloaded; on any failure the
/// temp file is removed so the next attempt starts clean.
async fn download_and_verify(
    progress: ProgressSink<'_>,
    a: &Artifact,
    tmp: &Path,
    url: &str,
) -> Result<u64, String> {
    let cleanup = |e: String| async move {
        let _ = tokio::fs::remove_file(tmp).await;
        Err::<u64, String>(e)
    };
    let client = reqwest::Client::new();
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
            "checksum mismatch (expected {}, got {digest})",
            a.sha256
        ))
        .await;
    }
    Ok(downloaded)
}

/// Ensure an artifact is installed; returns the probe path. Reports
/// progress through the sink while downloading/extracting.
pub async fn ensure(
    progress: ProgressSink<'_>,
    data_dir: &Path,
    id: &str,
) -> Result<PathBuf, String> {
    let a = artifact(id).ok_or_else(|| format!("unknown artifact {id}"))?;
    if let Some(path) = installed_path(data_dir, a) {
        return Ok(path);
    }

    let tmp = data_dir.join(format!("{}.download", a.id));
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
    let candidates = candidate_urls(a.url);
    let mut errors: Vec<String> = Vec::new();
    let mut downloaded: u64 = 0;
    let mut fetched = false;
    'sources: for url in &candidates {
        for attempt in 0..2u8 {
            if attempt > 0 || !errors.is_empty() {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            }
            match download_and_verify(progress, a, &tmp, url).await {
                Ok(bytes) => {
                    downloaded = bytes;
                    fetched = true;
                    break 'sources;
                }
                Err(e) => {
                    tracing::warn!(artifact = a.id, url, attempt, error = %e, "download attempt failed");
                    errors.push(e);
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
            "download failed for {} after trying {} source(s) (last error: {last}).{hf_hint}",
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
/// is named `{id}.download`.
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
        let gh = candidate_urls("https://github.com/k2-fsa/sherpa-onnx/releases/download/x.tar.bz2");
        assert_eq!(gh.len(), 1);
    }

    /// The registry lists ASR models smallest→largest, so the best installed
    /// model wins; a dir with no ggml weights yields None.
    #[test]
    fn best_installed_prefers_largest() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(best_installed_asr_model(tmp.path()).is_none());
        for id in ["ggml-small-q5_1", "ggml-large-v3-q5_0"] {
            let a = artifact(id).unwrap();
            let p = tmp.path().join(a.probe_rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
        }
        let (id, _) = best_installed_asr_model(tmp.path()).unwrap();
        assert_eq!(id, "ggml-large-v3-q5_0");
    }
}
