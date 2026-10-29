//! "Free up space" for downloaded models and tools (Settings › Transcripts ›
//! More options › Models on this computer): what each artifact takes on
//! disk, whether the current settings use it, and deleting the ones they
//! don't, plus leftovers from downloads and older versions.
//!
//! Rules this module keeps:
//! - "In use" comes from the same settings and code paths that use the
//!   files: the transcription model the pipeline would pick (`hw::
//!   default_model_for_tier`, the Model override, the installed stand-in
//!   `models::best_installed_asr_model` picks while it's missing), live
//!   captions' model, the engines, diarization (always on), the Vulkan build
//!   while the GPU switch is on, Parakeet while `asr.engine` routes to it,
//!   ffmpeg while a screen recording runs or an import waits. Queued
//!   (re-)transcriptions read the same settings when they start, so they
//!   need nothing beyond that. In-use files can't be deleted, and nothing is
//!   deleted while a recording, transcription or install runs.
//! - Only paths built here are deleted: an artifact's install root from the
//!   pinned registry, or a leftover matched by an exact, known pattern in
//!   `bin/`, `models/` or a download temp at the data-dir root. A delete
//!   request names ids and paths that must appear in a fresh scan; a path
//!   from the webview is never used as a path.
//! - Every delete is contained (`remove_contained`): plain-name components
//!   under `bin/` or `models/`, no link or junction anywhere between the
//!   data dir and the target, and the target removed without following
//!   links.
//! - Unknown files are reported as "Not recognised" with their size and
//!   never deleted. `models/ollama` (Ollama's own store, managed under Ask
//!   and Enhance) and the Ollama runtime's install are left out entirely;
//!   only a failed Ollama download (temp or `.partial`) counts as a
//!   leftover.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::models::{self, Artifact, ArtifactKind};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Download temps the current process wrote more recently than this belong
/// to a download that's still running (same age rule as the launch sweep).
const LIVE_TEMP_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// What the saved settings and current activity need. Read once per report
/// ([`read_usage`]); a plain struct so the rules are testable without an app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    /// `asr.tier`, else the machine's recommended tier.
    pub tier: String,
    pub max_quality: bool,
    /// `asr.model_id` (the Model picker), when set.
    pub model_override: Option<String>,
    /// "Made on: Groq" (tier `cloud`): this computer's model only backs it up.
    pub groq_only: bool,
    /// `asr.use_gpu` (default on): the Vulkan build leads the chain.
    pub use_gpu: bool,
    /// `asr.engine` / `FLYONTHEWALL_ASR_ENGINE` routes to Parakeet.
    pub parakeet: bool,
    /// `live.enabled` (default on): live captions use their own model.
    pub live_enabled: bool,
    pub screen_recording: bool,
    /// A staged media import hasn't transcribed yet.
    pub imports_waiting: bool,
    /// The video storage job (#119) is running, or set to keep only the
    /// audio of videos: both re-encode with ffmpeg. Wired in `read_usage`.
    pub video_job: bool,
}

impl Default for Usage {
    fn default() -> Self {
        Self {
            tier: "balanced".into(),
            max_quality: false,
            model_override: None,
            groq_only: false,
            use_gpu: true,
            parakeet: false,
            live_enabled: true,
            screen_recording: false,
            imports_waiting: false,
            video_job: false,
        }
    }
}

impl Usage {
    /// The settings half of [`Usage`], from a settings getter. `recommended`
    /// is the cached hardware tier; `engine_env` the engine env override.
    pub fn from_settings(
        get: &dyn Fn(&str) -> Option<String>,
        recommended: Option<String>,
        engine_env: Option<String>,
    ) -> Self {
        // Exactly pipeline.rs: a saved tier wins even when empty (an empty
        // one then picks large-v3-turbo there too); only a missing one falls
        // back to the machine's recommendation.
        let tier = get("asr.tier")
            .or(recommended)
            .unwrap_or_else(|| "balanced".into());
        // Same precedence as pipeline::use_parakeet: env wins over the
        // setting. Language isn't checked: a non-English meeting only
        // skips Parakeet for that run, the flag still routes to it.
        let engine = engine_env
            .filter(|v| !v.is_empty())
            .or_else(|| get(crate::pipeline::ENGINE_SETTING_KEY));
        Self {
            groq_only: tier == "cloud",
            tier,
            max_quality: get("asr.max_quality").as_deref() == Some("true"),
            model_override: get("asr.model_id").filter(|s| !s.is_empty()),
            use_gpu: get(crate::gpu::USE_GPU_KEY).as_deref() != Some("false"),
            parakeet: engine.as_deref().map(str::trim) == Some("parakeet"),
            live_enabled: get("live.enabled").as_deref() != Some("false"),
            screen_recording: false,
            imports_waiting: false,
            video_job: false,
        }
    }

    /// The whisper model id the pipeline asks for (before any stand-in).
    fn wanted_model(&self) -> String {
        self.model_override.clone().unwrap_or_else(|| {
            crate::hw::default_model_for_tier(&self.tier, self.max_quality).to_string()
        })
    }

    /// What the wanted model does, as "Used for …" wording.
    fn transcription_phrase(&self) -> String {
        if self.model_override.is_some() {
            "transcription (picked under Model)".into()
        } else if self.groq_only {
            "backing up Groq transcription".into()
        } else if self.max_quality {
            "Maximum quality transcription".into()
        } else {
            match self.tier.as_str() {
                "light" => "Fast transcription".into(),
                "balanced" => "Balanced transcription".into(),
                "best" => "Best transcription".into(),
                _ => "transcription".into(),
            }
        }
    }
}

/// Why no delete can run right now (all rows wait for it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    Recording,
    Transcribing,
    Installing,
}

impl Busy {
    pub fn message(self) -> &'static str {
        match self {
            Busy::Recording => "Recording now. You can delete models after it ends.",
            Busy::Transcribing => "Transcribing now. You can delete models when it finishes.",
            Busy::Installing => "A download is finishing. Try again in a moment.",
        }
    }
}

/// One downloadable artifact as Settings lists it.
#[derive(Debug, Clone, Serialize)]
pub struct ArtifactUsage {
    pub id: String,
    pub display: String,
    pub installed: bool,
    /// Bytes on disk now (0 when not installed; links count as 0).
    pub size_on_disk: u64,
    /// Pinned download size, for rows that aren't installed.
    pub download_bytes: u64,
    /// "Used for …" when the current settings use it.
    pub in_use: Option<String>,
    /// What it's for, when installed and not in use.
    pub note: Option<String>,
    pub can_delete: bool,
    /// Why Delete is disabled on an installed row (linked, in use, busy).
    pub blocked: Option<String>,
    /// It sits in (or is) a linked folder: the app doesn't manage it, so it
    /// has no size here and can't be deleted.
    pub linked: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A download temp or a half-finished install.
    UnfinishedDownload,
    /// A superseded install of a tool or model (the pins moved on).
    OldVersion,
    /// A model file matching a known pattern that no current entry uses.
    UnusedModelFile,
    /// A folder an earlier delete moved aside (`<name>.deleting-<ms>`) but
    /// couldn't finish; swept at launch.
    PartlyDeleted,
    /// Anything else: listed with its size, never deleted.
    NotRecognised,
}

/// A file or folder found on disk that isn't a current artifact.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DiskEntry {
    /// Data-dir-relative, `/`-separated. Delete requests send it back and
    /// it's matched against a fresh scan, never used as a path directly.
    pub path: String,
    pub bytes: u64,
    pub kind: EntryKind,
    /// Plain words ("Unfinished download of Whisper medium").
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelStorageReport {
    pub artifacts: Vec<ArtifactUsage>,
    /// Deletable leftovers (unfinished downloads, old versions, unused
    /// model files).
    pub leftovers: Vec<DiskEntry>,
    /// Not recognised: shown with their size, left alone.
    pub unrecognised: Vec<DiskEntry>,
    /// What "Free up" removes: deletable artifacts + leftovers.
    pub reclaimable_bytes: u64,
    /// Set while nothing can be deleted ([`Busy::message`]).
    pub busy: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeleteOutcome {
    pub freed_bytes: u64,
    /// Labels of what went.
    pub removed: Vec<String>,
    /// Requested but not deleted because it's in use now or no longer
    /// there ("Whisper medium: in use").
    pub skipped: Vec<String>,
    /// Deletes that failed ("Whisper medium: <error>").
    pub failed: Vec<String>,
    /// The state afterwards.
    pub report: ModelStorageReport,
}

/// Every artifact this OS knows about: tools, models, engine weights.
fn all_artifacts() -> impl Iterator<Item = &'static Artifact> {
    models::registry().chain(models::engine_models())
}

/// Rows Settings lists: the registry minus the Ollama runtime (that lives
/// under Ask and Enhance), plus engine weights that are on disk (they're
/// hidden otherwise: nothing in Settings selects them).
fn listed_artifacts(data_dir: &Path) -> Vec<&'static Artifact> {
    models::registry()
        .filter(|a| a.id != crate::ollama::ARTIFACT_ID)
        .chain(models::engine_models().filter(|a| models::installed_path(data_dir, a).is_some()))
        .collect()
}

/// What deleting an artifact removes, data-dir-relative and `/`-separated:
/// the file; for tools with versioned installs, `bin/<tool>/<version entry>`;
/// for other archives, the extracted root folder inside `dest` (pyannote
/// shares `models/diarize` with CAM++), or `dest` itself when the probe sits
/// right in it (the Ollama runtime).
pub(crate) fn install_root(a: &Artifact) -> String {
    if models::PRUNABLE_TOOLS.contains(&a.id) {
        let parts: Vec<&str> = a.probe_rel.split('/').collect();
        if parts.len() > 3 && parts[0] == "bin" {
            return parts[..3].join("/");
        }
    }
    match a.kind {
        ArtifactKind::File => a.dest_rel.to_string(),
        ArtifactKind::Archive => {
            let rest = a
                .probe_rel
                .strip_prefix(a.dest_rel)
                .and_then(|r| r.strip_prefix('/'));
            match rest.and_then(|r| r.split_once('/')) {
                Some((first, _)) => format!("{}/{first}", a.dest_rel),
                None => a.dest_rel.to_string(),
            }
        }
    }
}

/// Join noun phrases: "a", "a and b", "a, b and c".
fn join_phrases(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// "Used for …" phrases for one artifact under `usage`. `wanted` is the
/// model the pipeline asks for; `stand_in` the installed model it would use
/// while `wanted` isn't on disk.
fn use_phrases(a: &Artifact, usage: &Usage, wanted: &str, stand_in: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    match a.id {
        models::WHISPER_ENGINE_ID => out.push("transcribing on this computer".into()),
        crate::gpu::VULKAN_TOOL_ID => {
            if usage.use_gpu {
                out.push("transcribing on the graphics card".into());
            }
        }
        "sherpa-bin" | "pyannote-seg" | "campplus-embedding" => {
            // Diarization runs on every transcription (pipeline.rs: the
            // models are always ensured, on every tier).
            out.push("telling speakers apart".into());
        }
        "ffmpeg" => {
            if usage.screen_recording {
                out.push("the screen recording in progress".into());
            }
            if usage.imports_waiting {
                out.push("an import waiting to transcribe".into());
            }
            if usage.video_job {
                out.push("storing videos".into());
            }
        }
        models::PARAKEET_MODEL_ID => {
            if usage.parakeet {
                out.push("the Parakeet engine".into());
            }
        }
        id => {
            if id == wanted {
                out.push(usage.transcription_phrase());
            }
            if stand_in == Some(id) {
                let name = models::artifact(wanted).map_or(wanted, |w| w.display);
                out.push(format!("transcribing until {name} downloads"));
            }
            if usage.live_enabled && id == crate::live::LIVE_MODEL {
                out.push("the live transcript while recording".into());
            }
        }
    }
    out
}

/// What an installed, unused artifact is for.
fn unused_note(a: &Artifact) -> String {
    match a.id {
        crate::gpu::VULKAN_TOOL_ID => {
            "Only used when Use the graphics card is on. It downloads again if you turn it on."
                .into()
        }
        "ffmpeg" => "Used to record your screen, import files and make previews of videos in \
                     your notes. It downloads again the next time one of those needs it."
            .into(),
        _ => "Not used by your current settings. It downloads again if you need it.".into(),
    }
}

/// Build the report: artifact rows, leftovers, not-recognised entries.
/// `own_pid` decides which download temps may still be live.
pub fn build_report(
    data_dir: &Path,
    usage: &Usage,
    busy: Option<Busy>,
    own_pid: u32,
) -> ModelStorageReport {
    let wanted = usage.wanted_model();
    let wanted_installed = models::artifact(&wanted)
        .filter(|a| a.id.starts_with("ggml-"))
        .and_then(|a| models::installed_path(data_dir, a))
        .is_some();
    // While the wanted model isn't on disk (just switched, or an id this
    // version no longer has), the pipeline transcribes with this one when
    // the download can't happen: keep it.
    let stand_in = if wanted_installed {
        None
    } else {
        models::best_installed_asr_model(data_dir, &wanted).map(|(id, _)| id)
    };

    let mut artifacts = Vec::new();
    for a in listed_artifacts(data_dir) {
        let installed = models::installed_path(data_dir, a).is_some();
        let rel = install_root(a);
        // `remove_contained` refuses anything reached through a link, so a
        // linked install is shown as such and kept out of Free up.
        let linked = installed && through_link(data_dir, &rel);
        let size_on_disk = if installed && !linked {
            disk_size(&data_dir.join(&rel))
        } else {
            0
        };
        let phrases = use_phrases(a, usage, &wanted, stand_in);
        let in_use = (!phrases.is_empty()).then(|| format!("Used for {}.", join_phrases(&phrases)));
        let blocked = if !installed {
            None
        } else if linked {
            Some(LINKED.to_string())
        } else if let Some(sentence) = &in_use {
            Some(format!("In use, so it can't be deleted. {sentence}"))
        } else {
            busy.map(|b| b.message().to_string())
        };
        let note = (installed && in_use.is_none()).then(|| {
            if linked {
                LINKED.to_string()
            } else {
                unused_note(a)
            }
        });
        artifacts.push(ArtifactUsage {
            id: a.id.to_string(),
            display: a.display.to_string(),
            installed,
            size_on_disk,
            download_bytes: a.bytes,
            note,
            can_delete: installed && blocked.is_none(),
            in_use,
            blocked,
            linked,
        });
    }

    let (leftovers, unrecognised) = scan_leftovers(data_dir, own_pid);
    let reclaimable_bytes = artifacts
        .iter()
        .filter(|a| a.can_delete)
        .map(|a| a.size_on_disk)
        .chain(leftovers.iter().map(|e| e.bytes))
        .sum();
    ModelStorageReport {
        artifacts,
        leftovers,
        unrecognised,
        reclaimable_bytes,
        busy: busy.map(|b| b.message().to_string()),
    }
}

/// Row text for an install reached through a link.
const LINKED: &str = "Linked folder, not managed by the app.";

/// Marker of a folder a delete moved aside: `<name>.deleting-<unix ms>`.
const DELETING_MARK: &str = ".deleting-";

/// Whether `name` is a folder an earlier delete moved aside.
fn is_deleting_name(name: &str) -> bool {
    name.rsplit_once(DELETING_MARK).is_some_and(|(stem, ms)| {
        !stem.is_empty() && !ms.is_empty() && ms.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Whether `rel`, or any folder between the data dir and it, is a link,
/// junction or other reparse point.
fn through_link(data_dir: &Path, rel: &str) -> bool {
    let mut p = data_dir.to_path_buf();
    rel.split('/').any(|seg| {
        p.push(seg);
        std::fs::symlink_metadata(&p).is_ok_and(|m| is_link(&m))
    })
}

/// `<id>.download.<pid>.<n>` (models.rs `download_tmp_path`) → (id, pid).
fn parse_download_temp(name: &str) -> Option<(&str, u32)> {
    let (id, rest) = name.split_once(".download.")?;
    let (pid, n) = rest.split_once('.')?;
    let pid: u32 = pid.parse().ok()?;
    n.parse::<u64>().ok()?;
    (!id.is_empty() && fly_storage::safe_path::is_plain_name(name)).then_some((id, pid))
}

/// A symlink, junction or other reparse point: never followed, never
/// counted, never deleted as a leftover.
fn is_link(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Bytes under `p` without following links (a link counts as 0).
fn disk_size(p: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    if is_link(&meta) {
        0
    } else if meta.is_dir() {
        std::fs::read_dir(p)
            .map(|entries| entries.flatten().map(|e| disk_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        meta.len()
    }
}

/// A catalog name without its "(Q5, ~540 MB) — …" tail, for leftover
/// labels: an older version shouldn't be named after the current one.
fn base_name(display: &str) -> &str {
    let d = display.split(" — ").next().unwrap_or(display);
    d.split(" (").next().unwrap_or(d).trim()
}

/// Short name for an artifact id (falls back to the id).
fn display_of(id: &str) -> String {
    all_artifacts()
        .find(|a| a.id == id)
        .map_or_else(|| id.to_string(), |a| base_name(a.display).to_string())
}

/// Walk `bin/` and `models/` (plus download temps at the data-dir root) and
/// sort what isn't a current install into leftovers and not-recognised.
fn scan_leftovers(data_dir: &Path, own_pid: u32) -> (Vec<DiskEntry>, Vec<DiskEntry>) {
    let mut leftovers = Vec::new();
    let mut unrecognised = Vec::new();

    // Download temps at the root. Nothing else at the root is looked at.
    if let Ok(entries) = std::fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((id, pid)) = parse_download_temp(&name) else {
                continue;
            };
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if is_link(&meta) || !meta.is_file() {
                continue;
            }
            let fresh = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_none_or(|age| age < LIVE_TEMP_MAX_AGE);
            if pid == own_pid && fresh {
                continue; // a download running right now
            }
            let label = format!("Unfinished download of {}", display_of(id));
            leftovers.push(DiskEntry {
                path: name,
                bytes: meta.len(),
                kind: EntryKind::UnfinishedDownload,
                label,
            });
        }
    }

    let arts: Vec<&'static Artifact> = all_artifacts().collect();
    let scan = Scanner {
        data_dir,
        arts: &arts,
        roots: arts.iter().map(|a| install_root(a)).collect(),
    };
    for top in ["bin", "models"] {
        scan.dir(top, &mut leftovers, &mut unrecognised);
    }
    leftovers.sort_by(|a, b| a.path.cmp(&b.path));
    unrecognised.sort_by(|a, b| a.path.cmp(&b.path));
    (leftovers, unrecognised)
}

struct Scanner<'a> {
    data_dir: &'a Path,
    arts: &'a [&'static Artifact],
    /// Install roots of every known artifact (data-dir-relative).
    roots: Vec<String>,
}

impl Scanner<'_> {
    /// Classify the entries of `rel` (a real directory under the data dir).
    fn dir(&self, rel: &str, leftovers: &mut Vec<DiskEntry>, unrecognised: &mut Vec<DiskEntry>) {
        let abs = self.data_dir.join(rel);
        match std::fs::symlink_metadata(&abs) {
            Ok(m) if m.is_dir() && !is_link(&m) => {}
            _ => return, // missing, a file, or a link: never walked
        }
        let Ok(entries) = std::fs::read_dir(&abs) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = format!("{rel}/{name}");
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            let entry_of = |kind, label: String, bytes| DiskEntry {
                path: path.clone(),
                bytes,
                kind,
                label,
            };
            // Ollama's own model store and its runtime: managed under Ask
            // and Enhance, never here.
            if path == "models/ollama" {
                continue;
            }
            let link = is_link(&meta);
            if let Some(a) = self.arts.iter().find(|a| install_root(a) == path) {
                if a.id == crate::ollama::ARTIFACT_ID {
                    continue;
                }
                // A current install, accounted for in its row, unless the
                // probe is missing: a half-finished install the next
                // download replaces anyway. A link is never a leftover.
                if models::installed_path(self.data_dir, a).is_none() {
                    if link {
                        unrecognised.push(entry_of(
                            EntryKind::NotRecognised,
                            "Not recognised".into(),
                            0,
                        ));
                    } else {
                        leftovers.push(entry_of(
                            EntryKind::UnfinishedDownload,
                            format!("Unfinished install of {}", base_name(a.display)),
                            disk_size(&entry.path()),
                        ));
                    }
                }
                continue;
            }
            if link || !fly_storage::safe_path::is_plain_name(&name) {
                unrecognised.push(entry_of(
                    EntryKind::NotRecognised,
                    "Not recognised".into(),
                    0,
                ));
                continue;
            }
            let prefix = format!("{path}/");
            if meta.is_dir() && self.roots.iter().any(|r| r.starts_with(&prefix)) {
                self.dir(&path, leftovers, unrecognised);
                continue;
            }
            let bytes = disk_size(&entry.path());
            match self.classify(rel, &name, meta.is_dir()) {
                Some((kind, label)) => leftovers.push(entry_of(kind, label, bytes)),
                None => unrecognised.push(entry_of(
                    EntryKind::NotRecognised,
                    "Not recognised".into(),
                    bytes,
                )),
            }
        }
    }

    /// Known leftover patterns for an entry `name` inside `parent`.
    fn classify(&self, parent: &str, name: &str, is_dir: bool) -> Option<(EntryKind, String)> {
        // Before every other rule: in a versioned tool folder it would
        // otherwise read as an older version.
        if is_dir && is_deleting_name(name) {
            return Some((EntryKind::PartlyDeleted, "Partly deleted earlier".into()));
        }
        // `<dest>.partial`: the staging folder of an archive install.
        if let Some(stem) = name.strip_suffix(".partial") {
            let dest = format!("{parent}/{stem}");
            if let Some(a) = self
                .arts
                .iter()
                .find(|a| a.kind == ArtifactKind::Archive && a.dest_rel == dest)
            {
                return Some((
                    EntryKind::UnfinishedDownload,
                    format!("Unfinished install of {}", base_name(a.display)),
                ));
            }
        }
        // Everything else in a versioned tool folder is a superseded install
        // (the same rule the launch sweep prunes by).
        if let Some(a) = self.arts.iter().find(|a| {
            models::PRUNABLE_TOOLS.contains(&a.id)
                && install_root(a).rsplit_once('/').map(|(p, _)| p) == Some(parent)
        }) {
            return Some((
                EntryKind::OldVersion,
                format!("Older version of {}", base_name(a.display)),
            ));
        }
        match parent {
            "models/asr" if !is_dir && name.starts_with("ggml-") && name.ends_with(".bin") => {
                Some((
                    EntryKind::UnusedModelFile,
                    "Speech model this version no longer uses".into(),
                ))
            }
            "models/diarize"
                if is_dir && name.starts_with("sherpa-onnx-pyannote-segmentation-") =>
            {
                Some((
                    EntryKind::OldVersion,
                    "Older version of pyannote segmentation".into(),
                ))
            }
            "models/diarize" if !is_dir && name.ends_with(".onnx") => Some((
                EntryKind::UnusedModelFile,
                "Speaker model this version no longer uses".into(),
            )),
            _ => None,
        }
    }
}

/// Delete `rel` (a `/`-separated data-dir-relative path from a fresh scan)
/// without ever leaving the data dir's `bin/` and `models/` folders (or, for
/// a single download-temp name, the data-dir root): plain-name components
/// only, never inside `models/ollama`, no link or junction on the way or as
/// the target, and nothing followed.
///
/// A folder is first renamed aside (`<name>.deleting-<ms>`), so a file in
/// use (Windows) can't leave a half-deleted install whose probe survives and
/// reads as installed: the rename fails and nothing is touched, or it works
/// and the original path is gone at once (the next use downloads afresh).
/// Whatever the removal then can't finish is swept at the next launch
/// ([`sweep_partly_deleted`]). Returns the bytes freed.
pub(crate) fn remove_contained(data_dir: &Path, rel: &str) -> Result<u64, String> {
    let refuse = || format!("refused to delete {rel}: it isn't inside the models folders");
    let target: PathBuf = if rel.contains('/') {
        fly_storage::safe_path::contained(data_dir, rel, &["bin", "models"], 2)
            .ok_or_else(refuse)?
    } else if parse_download_temp(rel).is_some() {
        data_dir.join(rel)
    } else {
        return Err(refuse());
    };
    if rel == "models/ollama" || rel.starts_with("models/ollama/") {
        return Err(refuse());
    }
    let segments: Vec<&str> = rel.split('/').collect();
    let mut dir = data_dir.to_path_buf();
    for seg in &segments[..segments.len() - 1] {
        dir.push(seg);
        let ok = std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir() && !is_link(&m));
        if !ok {
            return Err(format!(
                "refused to delete {rel}: a folder on the way is a link or missing"
            ));
        }
    }
    let meta = match std::fs::symlink_metadata(&target) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.to_string()),
    };
    if is_link(&meta) {
        return Err(format!(
            "refused to delete {rel}: it's a link, which the app doesn't manage"
        ));
    }
    let bytes = disk_size(&target);
    if !meta.is_dir() {
        // One file: removed whole or not at all.
        models::remove_path(&target).map_err(|e| {
            format!("{rel} is in use by another program, so it wasn't deleted ({e})")
        })?;
        return Ok(bytes);
    }
    let name = segments[segments.len() - 1];
    let aside = if is_deleting_name(name) {
        target.clone()
    } else {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let mut n = 0u32;
        let aside = loop {
            let candidate =
                target.with_file_name(format!("{name}{DELETING_MARK}{}", ms + n as u128));
            if std::fs::symlink_metadata(&candidate).is_err() {
                break candidate;
            }
            n += 1;
        };
        std::fs::rename(&target, &aside).map_err(|e| {
            format!(
                "{rel} is in use by another program, so nothing was deleted. \
                 Close it and try again ({e})"
            )
        })?;
        aside
    };
    models::remove_path(&aside).map_err(|e| {
        format!("{rel} was only partly deleted ({e}). The rest goes the next time the app starts.")
    })?;
    Ok(bytes)
}

/// Finish deletes a file in use interrupted: remove every leftover
/// `<name>.deleting-<ms>` folder. Called once at launch, off the startup
/// path; skipped while an install holds the lock.
pub fn sweep_partly_deleted(data_dir: &Path) {
    let Some(_guard) = models::try_install_lock() else {
        return;
    };
    finish_partly_deleted(data_dir);
}

/// [`sweep_partly_deleted`] without the install lock (the caller holds it).
fn finish_partly_deleted(data_dir: &Path) {
    let (leftovers, _) = scan_leftovers(data_dir, std::process::id());
    for e in leftovers
        .iter()
        .filter(|e| e.kind == EntryKind::PartlyDeleted)
    {
        match remove_contained(data_dir, &e.path) {
            Ok(bytes) => {
                tracing::info!(path = %e.path, bytes, "finished a partly deleted model folder")
            }
            Err(err) => {
                tracing::warn!(path = %e.path, error = %err, "partly deleted model folder still there")
            }
        }
    }
}

/// Delete the requested artifacts (by id) and leftovers (by path), each
/// only if a fresh report still lists it as deletable. Not-recognised
/// entries never match. The caller holds the install lock and has checked
/// that nothing is busy.
pub fn delete_requested(
    data_dir: &Path,
    usage: &Usage,
    ids: &[String],
    paths: &[String],
    own_pid: u32,
) -> DeleteOutcome {
    let before = build_report(data_dir, usage, None, own_pid);
    let mut outcome = DeleteOutcome {
        freed_bytes: 0,
        removed: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
        report: before.clone(),
    };
    let mut targets: Vec<(String, String)> = Vec::new(); // (rel path, label)
    for id in ids {
        match before.artifacts.iter().find(|a| &a.id == id) {
            Some(row) if row.can_delete => {
                if let Some(a) = all_artifacts().find(|a| a.id == id) {
                    targets.push((install_root(a), row.display.clone()));
                }
            }
            Some(row) if !row.installed => outcome
                .skipped
                .push(format!("{}: already removed", row.display)),
            Some(row) if row.linked => outcome
                .skipped
                .push(format!("{}: in a linked folder", row.display)),
            Some(row) => outcome.skipped.push(format!("{}: in use", row.display)),
            None => outcome
                .skipped
                .push(format!("{id}: not a model this app manages")),
        }
    }
    for p in paths {
        match before.leftovers.iter().find(|e| &e.path == p) {
            Some(e) => targets.push((e.path.clone(), e.label.clone())),
            None => outcome.skipped.push(format!("{p}: no longer there")),
        }
    }
    // The same id or path twice is one delete.
    let mut seen = std::collections::HashSet::new();
    targets.retain(|(rel, _)| seen.insert(rel.clone()));
    for (rel, label) in targets {
        match remove_contained(data_dir, &rel) {
            Ok(bytes) => {
                tracing::info!(path = %rel, bytes, "model cleanup: deleted");
                outcome.freed_bytes += bytes;
                outcome.removed.push(label);
            }
            Err(e) => {
                tracing::warn!(path = %rel, error = %e, "model cleanup: delete failed");
                outcome.failed.push(format!("{label}: {e}"));
            }
        }
    }
    outcome.report = build_report(data_dir, usage, None, own_pid);
    outcome
}

/// The saved settings and current activity, from the app state. Locks are
/// taken one at a time (storage, then recording/screen/stage).
fn read_usage(state: &AppState) -> (Usage, Option<Busy>) {
    let mut usage = {
        let storage = state.storage.lock().unwrap();
        let get = |k: &str| storage.get_setting(k).ok().flatten();
        let mut u = Usage::from_settings(
            &get,
            crate::hw::cached(&storage).map(|h| h.recommended_tier),
            std::env::var(crate::pipeline::ENGINE_ENV_VAR).ok(),
        );
        u.imports_waiting = storage
            .staged_imports_waiting()
            .map(|ids| !ids.is_empty())
            // Unreadable: assume an import may need ffmpeg.
            .unwrap_or(true);
        u
    };
    usage.screen_recording = state.screen.lock().unwrap().is_some();
    // CHECK POINT (#119, video storage): ffmpeg is also in use while the
    // video job runs or is set to keep only the audio of videos. Whichever
    // of #121 / #119 merges second sets `usage.video_job` here from the
    // job's "needs ffmpeg" signal; the rule (`use_phrases`) and its test are
    // already in place.
    // TODO(#119): usage.video_job = <video job active || audio-only policy on>;
    let busy = if crate::scheduler::recording_active(state) {
        Some(Busy::Recording)
    } else if state
        .pipeline_stage
        .lock()
        .unwrap()
        .values()
        .any(|s| s != crate::scheduler::WAITING_STAGE)
    {
        Some(Busy::Transcribing)
    } else {
        None
    };
    (usage, busy)
}

/// Settings › Models on this computer: sizes, in-use labels, leftovers.
#[tauri::command]
pub async fn model_storage_report(state: State<'_, AppState>) -> CmdResult<ModelStorageReport> {
    let (usage, busy) = read_usage(&state);
    let busy = busy.or_else(|| {
        models::try_install_lock()
            .is_none()
            .then_some(Busy::Installing)
    });
    let data_dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        build_report(&data_dir, &usage, busy, std::process::id())
    })
    .await
    .map_err(|e| e.to_string())
}

/// Delete models (by `ids`) and leftovers (by `paths`, as listed in the
/// report). Per-row Delete sends one id; Free up sends everything it showed
/// in its confirm. Each is re-checked against a fresh report: anything in
/// use now, or not listed, is skipped. Refused outright while a recording,
/// transcription or install runs.
#[tauri::command]
pub async fn delete_model_files(
    state: State<'_, AppState>,
    ids: Vec<String>,
    paths: Vec<String>,
) -> CmdResult<DeleteOutcome> {
    // Held for the whole delete: no install can start meanwhile.
    let guard = models::try_install_lock().ok_or(Busy::Installing.message())?;
    let (usage, busy) = read_usage(&state);
    if let Some(b) = busy {
        return Err(b.message().into());
    }
    let data_dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        delete_requested(&data_dir, &usage, &ids, &paths, std::process::id())
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const SMALL: &str = "ggml-small-q5_1";
    const MEDIUM: &str = "ggml-medium-q5_0";
    const TURBO: &str = "ggml-large-v3-turbo-q5_0";
    const LARGE: &str = "ggml-large-v3-q5_0";

    /// Lay an artifact's probe (and so its install) down in `dir`.
    fn install(dir: &Path, id: &str, bytes: usize) {
        let a = all_artifacts().find(|a| a.id == id).unwrap();
        let probe = dir.join(a.probe_rel);
        std::fs::create_dir_all(probe.parent().unwrap()).unwrap();
        std::fs::write(&probe, vec![0u8; bytes]).unwrap();
    }

    fn usage_with(pairs: &[(&str, &str)]) -> Usage {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Usage::from_settings(&|k: &str| map.get(k).cloned(), None, None)
    }

    fn row<'a>(r: &'a ModelStorageReport, id: &str) -> &'a ArtifactUsage {
        r.artifacts.iter().find(|a| a.id == id).unwrap()
    }

    fn whisper_models(dir: &Path) {
        for id in [SMALL, MEDIUM, TURBO, LARGE] {
            install(dir, id, 1000);
        }
    }

    fn in_use_ids(r: &ModelStorageReport) -> Vec<&str> {
        r.artifacts
            .iter()
            .filter(|a| a.id.starts_with("ggml-") && a.in_use.is_some())
            .map(|a| a.id.as_str())
            .collect()
    }

    #[test]
    fn each_tier_keeps_its_own_model_and_the_live_one() {
        let tmp = tempfile::tempdir().unwrap();
        whisper_models(tmp.path());
        /// (settings, whisper models in use, phrase one of them says)
        type Case<'a> = (&'a [(&'a str, &'a str)], &'a [&'a str], &'a str);
        let cases: &[Case] = &[
            (&[("asr.tier", "light")], &[SMALL], "Fast transcription"),
            (
                &[("asr.tier", "balanced")],
                &[SMALL, TURBO],
                "Balanced transcription",
            ),
            (
                &[("asr.tier", "best")],
                &[SMALL, TURBO],
                "Best transcription",
            ),
            (
                &[("asr.tier", "best"), ("asr.max_quality", "true")],
                &[SMALL, LARGE],
                "Maximum quality transcription",
            ),
            (
                &[("asr.tier", "light"), ("asr.model_id", MEDIUM)],
                &[SMALL, MEDIUM],
                "picked under Model",
            ),
            (&[("asr.tier", "cloud")], &[SMALL, TURBO], "backing up Groq"),
            (
                &[("asr.tier", "balanced"), ("live.enabled", "false")],
                &[TURBO],
                "Balanced transcription",
            ),
        ];
        for (settings, want, phrase) in cases {
            let r = build_report(tmp.path(), &usage_with(settings), None, 1);
            let mut got = in_use_ids(&r);
            got.sort();
            let mut want = want.to_vec();
            want.sort();
            assert_eq!(got, want, "{settings:?}");
            let main = r
                .artifacts
                .iter()
                .find(|a| a.in_use.as_deref().is_some_and(|s| s.contains(phrase)));
            assert!(main.is_some(), "{settings:?}: no row says {phrase:?}");
            for a in r.artifacts.iter().filter(|a| a.id.starts_with("ggml-")) {
                assert_eq!(a.can_delete, a.in_use.is_none(), "{}", a.id);
            }
        }
        // The live model's reason is named, and joins the tier's for Fast.
        let r = build_report(tmp.path(), &usage_with(&[("asr.tier", "light")]), None, 1);
        assert_eq!(
            row(&r, SMALL).in_use.as_deref(),
            Some("Used for Fast transcription and the live transcript while recording.")
        );
    }

    #[test]
    fn a_missing_wanted_model_keeps_its_stand_in() {
        let tmp = tempfile::tempdir().unwrap();
        // Switched to Maximum quality; large-v3 isn't downloaded yet.
        install(tmp.path(), TURBO, 1000);
        install(tmp.path(), MEDIUM, 1000);
        let u = usage_with(&[("asr.tier", "best"), ("asr.max_quality", "true")]);
        let r = build_report(tmp.path(), &u, None, 1);
        let turbo = row(&r, TURBO);
        assert!(!turbo.can_delete);
        assert!(turbo
            .in_use
            .as_deref()
            .unwrap()
            .contains("until Whisper large-v3"));
        assert!(row(&r, MEDIUM).can_delete, "only the best stand-in stays");
        assert!(!row(&r, LARGE).installed);
    }

    #[test]
    fn engines_diarization_gpu_and_parakeet_follow_settings() {
        let tmp = tempfile::tempdir().unwrap();
        for a in models::registry().filter(|a| a.id != crate::ollama::ARTIFACT_ID) {
            install(tmp.path(), a.id, 10);
        }
        install(tmp.path(), models::PARAKEET_MODEL_ID, 10);
        let on = build_report(tmp.path(), &usage_with(&[]), None, 1);
        for id in ["pyannote-seg", "campplus-embedding"] {
            assert!(row(&on, id).in_use.is_some(), "{id} is always in use");
        }
        if models::artifact("sherpa-bin").is_some() {
            assert!(!row(&on, "sherpa-bin").can_delete);
        }
        if models::artifact(models::WHISPER_ENGINE_ID).is_some() {
            assert!(!row(&on, models::WHISPER_ENGINE_ID).can_delete);
        }
        let parakeet = row(&on, models::PARAKEET_MODEL_ID);
        assert!(
            parakeet.can_delete,
            "listed once installed, unused by default"
        );
        let routed = build_report(
            tmp.path(),
            &usage_with(&[(crate::pipeline::ENGINE_SETTING_KEY, "parakeet")]),
            None,
            1,
        );
        assert!(!row(&routed, models::PARAKEET_MODEL_ID).can_delete);
        // The env override wins over the setting, like the pipeline.
        let env = Usage::from_settings(&|_| None, None, Some("parakeet".into()));
        assert!(env.parakeet);

        if models::artifact(crate::gpu::VULKAN_TOOL_ID).is_some() {
            assert!(
                !row(&on, crate::gpu::VULKAN_TOOL_ID).can_delete,
                "GPU on by default"
            );
            let off = build_report(
                tmp.path(),
                &usage_with(&[(crate::gpu::USE_GPU_KEY, "false")]),
                None,
                1,
            );
            let vulkan = row(&off, crate::gpu::VULKAN_TOOL_ID);
            assert!(vulkan.can_delete);
            assert!(vulkan.note.as_deref().unwrap().contains("graphics card"));
        }
        if models::artifact("ffmpeg").is_some() {
            assert!(row(&on, "ffmpeg").can_delete, "idle ffmpeg is unused");
            let mut busy = usage_with(&[]);
            busy.imports_waiting = true;
            let r = build_report(tmp.path(), &busy, None, 1);
            assert!(row(&r, "ffmpeg")
                .in_use
                .as_deref()
                .unwrap()
                .contains("import"));
            busy.imports_waiting = false;
            busy.screen_recording = true;
            let r = build_report(tmp.path(), &busy, None, 1);
            assert!(!row(&r, "ffmpeg").can_delete);
            busy.screen_recording = false;
            busy.video_job = true;
            let r = build_report(tmp.path(), &busy, None, 1);
            assert_eq!(
                row(&r, "ffmpeg").in_use.as_deref(),
                Some("Used for storing videos.")
            );
            assert!(row(&on, "ffmpeg")
                .note
                .as_deref()
                .unwrap()
                .contains("previews of videos"));
        }
        // The Ollama runtime is never listed here.
        assert!(on
            .artifacts
            .iter()
            .all(|a| a.id != crate::ollama::ARTIFACT_ID));
    }

    #[test]
    fn busy_blocks_every_delete_and_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        whisper_models(tmp.path());
        let r = build_report(
            tmp.path(),
            &usage_with(&[("asr.tier", "light")]),
            Some(Busy::Transcribing),
            1,
        );
        assert!(r.artifacts.iter().all(|a| !a.can_delete));
        assert_eq!(r.busy.as_deref(), Some(Busy::Transcribing.message()));
        assert_eq!(
            row(&r, MEDIUM).blocked.as_deref(),
            Some(Busy::Transcribing.message())
        );
        assert_eq!(r.reclaimable_bytes, 0);
    }

    #[test]
    fn delete_refuses_in_use_and_removes_unused() {
        let tmp = tempfile::tempdir().unwrap();
        whisper_models(tmp.path());
        let u = usage_with(&[("asr.tier", "balanced")]);
        let out = delete_requested(
            tmp.path(),
            &u,
            &[
                TURBO.into(),
                MEDIUM.into(),
                MEDIUM.into(),
                "ollama-bin".into(),
                "nope".into(),
            ],
            &[],
            1,
        );
        assert_eq!(out.removed, vec![row(&out.report, MEDIUM).display.clone()]);
        assert_eq!(out.freed_bytes, 1000);
        assert!(
            out.skipped.iter().any(|s| s.contains("in use")),
            "{:?}",
            out.skipped
        );
        assert_eq!(out.skipped.len(), 3, "{:?}", out.skipped);
        assert!(tmp
            .path()
            .join("models/asr/ggml-large-v3-turbo-q5_0.bin")
            .is_file());
        assert!(!tmp.path().join("models/asr/ggml-medium-q5_0.bin").exists());
        // Re-download works: the row is back to "not installed".
        let medium = row(&out.report, MEDIUM);
        assert!(!medium.installed && !medium.can_delete);
    }

    #[test]
    fn deleting_pyannote_keeps_campplus_next_to_it() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), "pyannote-seg", 10);
        install(tmp.path(), "campplus-embedding", 10);
        assert_eq!(
            install_root(models::artifact("pyannote-seg").unwrap()),
            "models/diarize/sherpa-onnx-pyannote-segmentation-3-0"
        );
        remove_contained(
            tmp.path(),
            "models/diarize/sherpa-onnx-pyannote-segmentation-3-0",
        )
        .unwrap();
        assert!(tmp.path().join("models/diarize/campplus.onnx").is_file());
    }

    #[test]
    fn containment_rejects_crafted_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(data.join("models/asr")).unwrap();
        let outside = tmp.path().join("outside.bin");
        std::fs::write(&outside, b"keep").unwrap();
        for bad in [
            "../outside.bin",
            "models/../../outside.bin",
            "models/asr/../../../outside.bin",
            "/etc/passwd",
            r"C:\Windows\notepad.exe",
            "recordings/x/recording.wav",
            "flyonthewall.db",
            "notes",
            "models",
            "models/ollama",
            "models/ollama/blobs/sha256-x",
            "models/asr/a:b",
            "models/asr/NUL",
            "models/asr/x.",
        ] {
            assert!(
                remove_contained(&data, bad).is_err(),
                "{bad:?} must be refused"
            );
        }
        assert!(outside.is_file());
    }

    /// A link or junction inside `models/` pointing outside: neither the
    /// scan nor a delete ever reaches the target.
    #[test]
    fn links_out_of_models_are_never_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(data.join("models/asr")).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("ggml-old.bin"), vec![0u8; 50]).unwrap();
        std::fs::write(outside.join("precious.txt"), b"keep").unwrap();

        // Junctions (Windows) and symlinks (Unix) need no privilege: a
        // failure here is a broken test, never a reason to skip.
        assert!(make_dir_link(&outside, &data.join("models/asr/linked")));
        // The whole `models/diarize` folder is a link too.
        assert!(make_dir_link(&outside, &data.join("models/diarize")));

        let (leftovers, unrecognised) = scan_leftovers(&data, 1);
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let link = unrecognised
            .iter()
            .find(|e| e.path == "models/asr/linked")
            .expect("the link is listed as not recognised");
        assert_eq!(link.bytes, 0, "a link is never measured through");

        // Deleting through the linked folder is refused…
        assert!(remove_contained(&data, "models/asr/linked/ggml-old.bin").is_err());
        assert!(remove_contained(&data, "models/diarize/campplus.onnx").is_err());
        // …and so is deleting the link itself: links aren't the app's.
        assert!(remove_contained(&data, "models/asr/linked").is_err());
        assert!(data.join("models/asr/linked").exists());
        assert!(outside.join("ggml-old.bin").is_file());
        assert!(outside.join("precious.txt").is_file());

        // A delete request can't name a link: it's never a leftover.
        let out = delete_requested(
            &data,
            &Usage::default(),
            &[],
            &["models/asr/linked".into()],
            1,
        );
        assert!(out.removed.is_empty());
        assert!(data.join("models/asr/linked").exists());
    }

    /// A directory symlink (Unix) or junction (Windows, no privilege
    /// needed). False when the platform refuses.
    fn make_dir_link(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            // Backslashes only: mklink reads "/asr" in "models/asr" as a switch.
            let win = |p: &Path| p.to_string_lossy().replace('/', "\\");
            let out = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(win(link))
                .arg(win(target))
                .output();
            match out {
                Ok(o) if o.status.success() => true,
                other => {
                    eprintln!("mklink failed: {other:?}");
                    false
                }
            }
        }
    }

    #[test]
    fn leftovers_are_known_patterns_and_unknown_files_stay() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        install(d, TURBO, 100);
        install(d, "campplus-embedding", 10);
        let put = |rel: &str, n: usize| {
            let p = d.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, vec![1u8; n]).unwrap();
        };
        // Leftovers.
        put("ggml-medium-q5_0.download.999999.0", 7); // another run's temp
        put("models/asr/ggml-base.en.bin", 11); // a model no longer offered
        put(
            "models/diarize/sherpa-onnx-pyannote-segmentation-2-0/model.onnx",
            13,
        );
        put("models/diarize/old-embedding.onnx", 17);
        put("models/diarize.partial/x/model.onnx", 19);
        // Not recognised (kept).
        put("models/asr/my-notes.txt", 23);
        put("models/whatever/file.bin", 29);
        put("bin/mystery/tool.exe", 31);
        // Never looked at.
        put("flyonthewall.db", 37);
        put("recordings/2026-10-01 Sync/recording.mic.wav", 41);
        put("models/ollama/blobs/sha256-abc", 43);
        // This process's fresh temp: a download in progress.
        let own = std::process::id();
        put(&format!("{SMALL}.download.{own}.3"), 47);

        let (leftovers, unrecognised) = scan_leftovers(d, own);
        let paths = |v: &[DiskEntry]| v.iter().map(|e| e.path.clone()).collect::<Vec<_>>();
        assert_eq!(
            paths(&leftovers),
            vec![
                "ggml-medium-q5_0.download.999999.0",
                "models/asr/ggml-base.en.bin",
                "models/diarize.partial",
                "models/diarize/old-embedding.onnx",
                "models/diarize/sherpa-onnx-pyannote-segmentation-2-0",
            ]
        );
        assert_eq!(
            paths(&unrecognised),
            vec!["bin/mystery", "models/asr/my-notes.txt", "models/whatever"]
        );
        assert_eq!(unrecognised[1].bytes, 23);
        assert_eq!(leftovers[0].label, "Unfinished download of Whisper medium");

        // Free up everything listed, plus the unknown paths smuggled in.
        let mut asked = paths(&leftovers);
        asked.extend(paths(&unrecognised));
        asked.push("flyonthewall.db".into());
        let out = delete_requested(d, &usage_with(&[]), &[], &asked, own);
        assert_eq!(out.removed.len(), 5, "{:?}", out);
        assert_eq!(out.freed_bytes, 7 + 11 + 13 + 17 + 19);
        for kept in [
            "models/asr/my-notes.txt",
            "models/whatever/file.bin",
            "bin/mystery/tool.exe",
            "flyonthewall.db",
            "recordings/2026-10-01 Sync/recording.mic.wav",
            "models/ollama/blobs/sha256-abc",
            "models/asr/ggml-large-v3-turbo-q5_0.bin",
            "models/diarize/campplus.onnx",
        ] {
            assert!(d.join(kept).is_file(), "{kept} must stay");
        }
        assert!(d.join(format!("{SMALL}.download.{own}.3")).is_file());
        assert!(out.report.leftovers.is_empty());
        assert_eq!(out.report.unrecognised.len(), 3);
    }

    #[test]
    fn a_linked_model_counts_once_and_the_ollama_runtime_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("data");
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("medium.bin"), vec![0u8; 64]).unwrap();
        std::fs::create_dir_all(d.join("models/asr")).unwrap();
        let linked = d.join("models/asr/ggml-medium-q5_0.bin");
        #[cfg(unix)]
        let ok = std::os::unix::fs::symlink(outside.join("medium.bin"), &linked).is_ok();
        #[cfg(windows)]
        let ok = std::os::windows::fs::symlink_file(outside.join("medium.bin"), &linked).is_ok();
        if ok {
            let (leftovers, unrecognised) = scan_leftovers(&d, 1);
            assert!(
                leftovers.is_empty() && unrecognised.is_empty(),
                "{unrecognised:?}"
            );
            let r = build_report(&d, &usage_with(&[]), None, 1);
            let medium = row(&r, MEDIUM);
            assert!(medium.linked && !medium.can_delete);
            assert_eq!(medium.size_on_disk, 0, "never measured through");
            assert_eq!(r.reclaimable_bytes, 0);
            let out = delete_requested(&d, &usage_with(&[]), &[MEDIUM.into()], &[], 1);
            assert!(out.removed.is_empty(), "{out:?}");
            assert!(outside.join("medium.bin").is_file());
            assert!(std::fs::symlink_metadata(&linked).is_ok());
        } else {
            // File symlinks need Developer Mode on Windows; the directory
            // junction test covers link handling there.
            eprintln!("note: file symlinks unavailable, skipped the linked-model half");
        }

        if let Some(ollama) = models::artifact(crate::ollama::ARTIFACT_ID) {
            let root = d.join(install_root(ollama));
            std::fs::create_dir_all(root.join("lib")).unwrap();
            std::fs::write(root.join("lib/runner.dll"), b"x").unwrap();
            let (leftovers, unrecognised) = scan_leftovers(&d, 1);
            assert!(leftovers.is_empty(), "{leftovers:?}");
            assert!(unrecognised.is_empty(), "{unrecognised:?}");
        }
    }

    /// A whole `models/asr` linked elsewhere: its models show as linked,
    /// not deletable, with no size and nothing toward Free up.
    #[test]
    fn models_in_a_linked_folder_are_not_managed() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("data");
        let outside = tmp.path().join("asr-elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("ggml-medium-q5_0.bin"), vec![0u8; 500]).unwrap();
        // the Balanced model, so medium isn't the stand-in (in use)
        std::fs::write(outside.join("ggml-large-v3-turbo-q5_0.bin"), vec![0u8; 9]).unwrap();
        std::fs::create_dir_all(d.join("models")).unwrap();
        assert!(make_dir_link(&outside, &d.join("models/asr")));

        let r = build_report(&d, &usage_with(&[("asr.tier", "balanced")]), None, 1);
        let medium = row(&r, MEDIUM);
        assert!(medium.installed && medium.linked && !medium.can_delete);
        assert_eq!(medium.size_on_disk, 0);
        assert_eq!(medium.blocked.as_deref(), Some(LINKED));
        assert_eq!(medium.note.as_deref(), Some(LINKED));
        assert_eq!(r.reclaimable_bytes, 0);
        let out = delete_requested(&d, &usage_with(&[]), &[MEDIUM.into()], &[], 1);
        assert_eq!(
            out.skipped,
            vec![format!("{}: in a linked folder", medium.display)]
        );
        assert!(outside.join("ggml-medium-q5_0.bin").is_file());
    }

    /// A folder goes in one rename first; with a file in it held open
    /// (Windows), nothing at all is deleted, so the probe can never survive
    /// without the rest of the install.
    #[test]
    fn folders_are_renamed_aside_and_a_held_file_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        install(d, "pyannote-seg", 10);
        let root = "models/diarize/sherpa-onnx-pyannote-segmentation-3-0";
        std::fs::write(d.join(root).join("LICENSE"), b"x").unwrap();
        let probe = d.join(root).join("model.onnx");

        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let held = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&probe)
                .unwrap();
            let err = remove_contained(d, root).unwrap_err();
            assert!(err.contains("in use"), "{err}");
            assert!(probe.is_file() && d.join(root).join("LICENSE").is_file());
            let (leftovers, _) = scan_leftovers(d, 1);
            assert!(leftovers.is_empty(), "nothing moved: {leftovers:?}");
            drop(held);
        }

        assert_eq!(remove_contained(d, root).unwrap(), 11);
        assert!(!d.join(root).exists());
        let left: Vec<_> = std::fs::read_dir(d.join("models/diarize"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert!(
            left.is_empty(),
            "no .deleting- folder stays behind: {left:?}"
        );
    }

    #[test]
    fn partly_deleted_folders_are_leftovers_and_swept_at_launch() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        install(d, "campplus-embedding", 10);
        let put = |rel: &str| {
            let p = d.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"rest").unwrap();
        };
        put("models/diarize/sherpa-onnx-pyannote-segmentation-3-0.deleting-1700000000000/a");
        put("models/diarize/notes.deleting-soon/b"); // not our marker
        if let Some(ffmpeg) = models::artifact("ffmpeg") {
            let root = install_root(ffmpeg);
            put(&format!("{root}.deleting-1700000000001/bin/ffmpeg.exe"));
        }
        let (leftovers, unrecognised) = scan_leftovers(d, 1);
        let partly: Vec<&DiskEntry> = leftovers
            .iter()
            .filter(|e| e.kind == EntryKind::PartlyDeleted)
            .collect();
        let expected = if models::artifact("ffmpeg").is_some() {
            2
        } else {
            1
        };
        assert_eq!(partly.len(), expected, "{leftovers:?}");
        assert!(partly.iter().all(|e| e.label == "Partly deleted earlier"));
        assert!(unrecognised
            .iter()
            .any(|e| e.path == "models/diarize/notes.deleting-soon"));

        // The lock-free core: other tests may hold the global install lock,
        // which makes the launch wrapper skip.
        finish_partly_deleted(d);
        let (leftovers, unrecognised) = scan_leftovers(d, 1);
        assert!(leftovers.is_empty(), "{leftovers:?}");
        assert_eq!(unrecognised.len(), 1, "unknown folders stay");
        assert!(d.join("models/diarize/campplus.onnx").is_file());
    }

    #[test]
    fn an_empty_saved_tier_matches_the_pipeline() {
        let get = |k: &str| (k == "asr.tier").then(String::new);
        let u = Usage::from_settings(&get, Some("light".into()), None);
        // pipeline.rs keeps the empty tier: default_model_for_tier("") is turbo
        assert_eq!(u.wanted_model(), TURBO);
        let missing = Usage::from_settings(&|_| None, Some("light".into()), None);
        assert_eq!(missing.wanted_model(), SMALL);
    }

    #[test]
    fn old_tool_versions_and_half_installs_are_leftovers() {
        let Some(sherpa) = models::artifact("sherpa-bin") else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        install(d, "sherpa-bin", 10);
        let root = install_root(sherpa);
        let (tool_dir, _) = root.rsplit_once('/').unwrap();
        std::fs::create_dir_all(d.join(tool_dir).join("sherpa-onnx-v1.0.0/bin")).unwrap();
        std::fs::write(d.join(tool_dir).join("sherpa-onnx-v1.0.0/bin/x"), b"old").unwrap();
        // pyannote extracted, but its probe never landed.
        std::fs::create_dir_all(d.join("models/diarize/sherpa-onnx-pyannote-segmentation-3-0"))
            .unwrap();
        let (leftovers, unrecognised) = scan_leftovers(d, 1);
        assert!(unrecognised.is_empty(), "{unrecognised:?}");
        let kinds: Vec<(String, EntryKind)> =
            leftovers.iter().map(|e| (e.path.clone(), e.kind)).collect();
        assert!(kinds.contains(&(
            format!("{tool_dir}/sherpa-onnx-v1.0.0"),
            EntryKind::OldVersion
        )));
        assert!(kinds.contains(&(
            "models/diarize/sherpa-onnx-pyannote-segmentation-3-0".into(),
            EntryKind::UnfinishedDownload
        )));
        assert!(
            !kinds.iter().any(|(p, _)| *p == root),
            "the current install stays"
        );
    }

    #[test]
    fn reclaimable_counts_deletable_rows_and_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        whisper_models(tmp.path());
        std::fs::write(tmp.path().join("ggml-x.download.999999.1"), vec![0u8; 5]).unwrap();
        let r = build_report(
            tmp.path(),
            &usage_with(&[("asr.tier", "balanced")]),
            None,
            1,
        );
        // medium + large-v3 unused (small: live, turbo: Balanced)
        assert_eq!(r.reclaimable_bytes, 1000 + 1000 + 5);
        assert_eq!(row(&r, MEDIUM).size_on_disk, 1000);
        assert_eq!(row(&r, SMALL).size_on_disk, 1000);
    }

    #[test]
    fn download_temp_names_parse_strictly() {
        assert_eq!(
            parse_download_temp("ggml-small-q5_1.download.123.4"),
            Some(("ggml-small-q5_1", 123))
        );
        assert_eq!(
            parse_download_temp("parakeet-tdt-0.6b-v3-q8_0.download.9.0"),
            Some(("parakeet-tdt-0.6b-v3-q8_0", 9))
        );
        for bad in [
            "flyonthewall.db",
            ".download.1.2",
            "x.download.1",
            "x.download.a.2",
            "x.download.1.2.3",
            "x.download.1.2.",
        ] {
            assert_eq!(parse_download_temp(bad), None, "{bad}");
        }
    }
}
