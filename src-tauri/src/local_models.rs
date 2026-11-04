//! Which local model the app defaults to, picked from this computer's
//! memory, and the one-time offer that lets an existing install move to it.
//!
//! The rule and its numbers come from the Ask bench on real local models
//! (docs/ASK-MODEL-BENCH.md, spec v2.5 §4.2 "Models"):
//!
//! * `qwen3.5:9b` is the best model measured that fits a 16 GB computer:
//!   it meets every Ask quality floor for its tier, keeps spoken numbers
//!   right and declines questions the meetings don't answer.
//! * `llama3.1` stays the default for the 12 GB class (and is what every
//!   existing install already uses).
//! * `qwen3.5:4b` is the fallback where `llama3.1` doesn't fit.
//!
//! **Nothing here switches a model on its own.** A new install is *pinned*
//! to the recommendation once, at its first launch; an existing install is
//! pinned to the model it was already using (`llama3.1`); an explicit choice
//! (`llm.ollama.model`) always wins. After that the default never moves: a
//! better model is offered once in Settings, and only the user's click
//! downloads or switches (`use_local_model`).
//!
//! The settings keys here are backend-only (the webview can't read or write
//! `llm.*`): commands are the only way in.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// What every install used before the default depended on memory, so it is
/// what an existing install keeps.
pub const LEGACY_DEFAULT: &str = "llama3.1";

/// The model an install with no explicit choice uses. Written once by
/// [`migrate_default`]; read by every local task (Enhance, Ask, polish,
/// extraction, titles) through [`effective_default`].
const DEFAULT_KEY: &str = "llm.ollama.default_model";
/// The model whose offer the user dismissed. A different recommendation (a
/// computer with more memory, say) is a new offer.
const DISMISSED_KEY: &str = "llm.ollama.offer_dismissed";
/// The explicit choice (Settings, or "use this model" here).
const EXPLICIT_KEY: &str = "llm.ollama.model";

/// Where the bench is written up, for the "how this was measured" link.
pub const BENCH_DOC_URL: &str =
    "https://github.com/swarnavspujari/fly-on-the-wall/blob/main/docs/ASK-MODEL-BENCH.md";

/// Honest label for every number below (docs/ASK-MODEL-BENCH.md §2): one
/// laptop, other work running, so speeds are indicative and quality isn't
/// affected by that.
pub const MEASURED_ON: &str = "Measured on one test laptop (4 GB graphics memory, 32 GB RAM, \
     Ollama 0.40) while other programs were running. Your computer will differ.";

/// What was measured for a model: 60 answerable and 16 unanswerable
/// questions from the evals' synthetic corpus, on main with the claim-check
/// fixes of #133 (docs/ASK-MODEL-BENCH.md §4.1 and §4.3).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Measured {
    /// Of the questions the meetings answer, the share answered with a
    /// source the model was shown ("grounded").
    pub sourced_pct: u8,
    /// Of the questions the meetings don't answer, the share it declined
    /// instead of making something up ("correct refusal").
    pub declines_pct: u8,
    /// Median time per answer, rounded to 5 s. Contended: other sessions
    /// were compiling on the machine, the small models more than the big.
    pub seconds: u8,
    /// How much longer notes, cleanup and action items take after a meeting
    /// than with `llama3.1`, in tenths (`[15, 25]` = 1.5 to 2.5 times), from
    /// the `llm_bench` contract pass. None when it is no slower.
    pub background_slower: Option<[u8; 2]>,
}

/// One model the app offers. The ONE static table of benchmark numbers in
/// product code: the UI reads it through `local_model_advice`.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Candidate {
    pub model: &'static str,
    /// Higher is better quality, as measured. `llama3.1` and `qwen3.5:4b`
    /// are tied (docs/ASK-MODEL-BENCH.md §4.1): the 4B is only ever the
    /// fit-driven fallback, never an "upgrade" offer.
    pub rank: u8,
    /// Download size (the registry's, rounded).
    pub download_bytes: u64,
    /// Memory held while answering, measured at the context window the app
    /// asks for (weights plus KV cache).
    pub memory_gb: f64,
    /// Installed memory (as the OS reports it, in GB) from which this model
    /// is recommended. See [`recommend`] for the arithmetic.
    pub min_ram_gb: f64,
    /// That figure as the computer's marketing class, for display.
    pub ram_class_gb: u8,
    /// The oldest Ollama that has the model: `qwen3.5` landed in 0.17.4 and
    /// its 4B and 9B sizes in 0.17.5 (Ollama release notes). The pull
    /// itself checks too; this only avoids recommending what can't pull.
    pub min_ollama: [u32; 3],
    pub measured: Measured,
}

/// Best first. The pin of the managed Windows runtime (0.40.13) and macOS
/// (0.31.2) are both far past `min_ollama`.
pub const CANDIDATES: &[Candidate] = &[
    Candidate {
        model: "qwen3.5:9b",
        rank: 2,
        download_bytes: 6_600_000_000,
        memory_gb: 6.2,
        // 6.2 GB for the model plus ~9 GB for Windows or macOS, this app,
        // the speech engine (1–3 GB while transcribing), the search model
        // and a browser or call app running during the meeting.
        min_ram_gb: 15.0,
        ram_class_gb: 16,
        min_ollama: [0, 17, 5],
        measured: Measured {
            sourced_pct: 83,
            declines_pct: 94,
            seconds: 30,
            background_slower: Some([15, 25]),
        },
    },
    Candidate {
        model: "llama3.1",
        rank: 1,
        download_bytes: 4_920_000_000,
        memory_gb: 5.8,
        // 5.8 GB plus ~5 GB: enough for the OS and this app; the speech
        // engine and a call app fit only with the memory a 12 GB class has.
        min_ram_gb: 11.0,
        ram_class_gb: 12,
        min_ollama: [0, 0, 0],
        measured: Measured {
            sourced_pct: 62,
            declines_pct: 75,
            seconds: 25,
            background_slower: None,
        },
    },
    Candidate {
        model: "qwen3.5:4b",
        rank: 1,
        download_bytes: 3_330_000_000,
        memory_gb: 3.6,
        // 3.6 GB plus ~3.5 GB: an 8 GB computer with little else open.
        // Below this, nothing measured fits well.
        min_ram_gb: 7.0,
        ram_class_gb: 8,
        min_ollama: [0, 17, 5],
        measured: Measured {
            sourced_pct: 65,
            declines_pct: 56,
            seconds: 20,
            background_slower: None,
        },
    },
];

fn candidate(model: &str) -> Option<&'static Candidate> {
    CANDIDATES
        .iter()
        .find(|c| crate::ollama::same_model(c.model, model))
}

/// Installed memory in GB, as the OS reports it (a "16 GB" computer reads
/// 15.2–16.0). Memory only, on purpose; see [`recommend`]. 0 when the OS
/// won't say.
pub fn total_ram_gb() -> f64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.total_memory() as f64 / 1024.0 / 1024.0 / 1024.0
}

/// A reading below this is "couldn't tell" (no computer has 1 GB; sysinfo
/// reports 0 when it can't read memory). Nothing is pinned or offered on it,
/// and the default stays `llama3.1`. That launch still writes the hardware
/// cache, so the next one counts as an existing install: it pins `llama3.1`
/// and, if the reading is good by then, shows the one-time offer.
pub const RAM_UNKNOWN_BELOW_GB: f64 = 1.0;

pub fn ram_known(ram_gb: f64) -> bool {
    ram_gb >= RAM_UNKNOWN_BELOW_GB
}

fn parse_version(v: &str) -> Option<[u32; 3]> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some([major, minor, patch])
}

/// Whether a running server of `version` has a model that needs `min`. An
/// unknown (server not running), unreadable or `0.0.0` version (what a
/// development build of Ollama reports) counts as yes: the pull says so
/// itself if it can't.
fn supports(version: Option<&str>, min: [u32; 3]) -> bool {
    version
        .and_then(parse_version)
        .is_none_or(|v| v == [0, 0, 0] || v >= min)
}

/// A better model exists for this computer but the running Ollama is too old
/// for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeldBack {
    pub model: String,
    /// The Ollama version the model needs, "0.17.5".
    pub needs_ollama: String,
    pub have_ollama: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    pub model: &'static str,
    pub held_back: Option<HeldBack>,
}

/// The model for a computer with `ram_gb` of memory running Ollama
/// `ollama_version` (None = unknown).
///
/// **Memory is the only input** (plus the Ollama version, which only ever
/// takes a model away). Each model needs its measured footprint plus room
/// for everything else that runs during a meeting (see the table). The
/// cut-offs are therefore 15 GB (the 16 GB class), 11 GB (12 GB) and below.
///
/// * *Graphics memory is not used.* The bench ran on a 4 GB GPU with 24–75%
///   of each model on it and still met the floors; a bigger GPU only makes
///   answers faster, and every computer with 8 GB or more of graphics memory
///   already has 16 GB or more of RAM. A computer without a GPU is slower
///   (the caption under the numbers says yours will differ); its model
///   fits all the same.
/// * *Apple Silicon* shares one pool, so installed memory is the figure to
///   use, and it is the same rule. The numbers understate a Mac's speed: its
///   GPU runs the whole model.
/// * *Linux and Intel Macs* use the same installed-memory figure.
/// * 32 GB and more stay on `qwen3.5:9b`: the larger models weren't run
///   (docs/ASK-MODEL-BENCH.md §9), and `gemma4:12b-it-qat` meets the same
///   floors but is 35% slower and 1.8 GB bigger.
pub fn recommend(ram_gb: f64, ollama_version: Option<&str>) -> Recommendation {
    // Memory unreadable: no claim about this computer; the safe default.
    if !ram_known(ram_gb) {
        return Recommendation {
            model: LEGACY_DEFAULT,
            held_back: None,
        };
    }
    let ideal = CANDIDATES
        .iter()
        .find(|c| ram_gb >= c.min_ram_gb)
        .unwrap_or(&CANDIDATES[CANDIDATES.len() - 1]);
    if supports(ollama_version, ideal.min_ollama) {
        return Recommendation {
            model: ideal.model,
            held_back: None,
        };
    }
    // The running Ollama is too old for the ideal model: the best one it
    // does support, from the ideal downward, and say what was held back.
    let fallback = CANDIDATES
        .iter()
        .filter(|c| c.rank <= ideal.rank && supports(ollama_version, c.min_ollama))
        .find(|c| ram_gb >= c.min_ram_gb)
        .or_else(|| {
            CANDIDATES
                .iter()
                .find(|c| supports(ollama_version, c.min_ollama))
        })
        .unwrap_or(&CANDIDATES[1]);
    let [a, b, c] = ideal.min_ollama;
    Recommendation {
        model: fallback.model,
        held_back: Some(HeldBack {
            model: ideal.model.to_string(),
            needs_ollama: format!("{a}.{b}.{c}"),
            have_ollama: ollama_version.unwrap_or_default().to_string(),
        }),
    }
}

fn get(storage: &fly_storage::Storage, key: &str) -> Option<String> {
    storage
        .get_setting(key)
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
}

/// The model every local task uses when the user hasn't chosen one: the
/// pinned default, else what every install used before this existed.
pub fn effective_default(storage: &fly_storage::Storage) -> String {
    get(storage, DEFAULT_KEY).unwrap_or_else(|| LEGACY_DEFAULT.to_string())
}

/// The user's own pick, if they made one.
pub fn explicit_model(storage: &fly_storage::Storage) -> Option<String> {
    get(storage, EXPLICIT_KEY)
}

/// Whether this library was in use before the default depended on memory:
/// notes, an accepted first-run notice, any provider setting, or a trace of a
/// previous launch (the hardware cache, the v2.4 consent migration). A brand
/// new install has none of them.
fn existing_install(storage: &fly_storage::Storage) -> bool {
    storage
        .list_recent_notes(1)
        .is_ok_and(|notes| !notes.is_empty())
        || [
            "consent.recording_notice_accepted",
            "llm.provider",
            EXPLICIT_KEY,
            "llm.ollama.base_url",
            crate::hw::CACHE_KEY,
            crate::llm_commands::CLOUD_CONSENT_RESET_FLAG,
        ]
        .iter()
        .any(|k| get(storage, k).is_some())
}

/// The Ollama manifest of `model` under a models folder: `llama3.1` is
/// `manifests/registry.ollama.ai/library/llama3.1/latest`, `qwen3.5:9b` is
/// `.../library/qwen3.5/9b`.
fn manifest_of(models_dir: &std::path::Path, model: &str) -> std::path::PathBuf {
    let (name, tag) = model.split_once(':').unwrap_or((model, "latest"));
    models_dir
        .join("manifests")
        .join("registry.ollama.ai")
        .join("library")
        .join(name)
        .join(tag)
}

/// Offered models that are already downloaded under any of `dirs` (best
/// first, as in [`CANDIDATES`]).
pub fn installed_in(dirs: &[std::path::PathBuf]) -> Vec<&'static str> {
    CANDIDATES
        .iter()
        .filter(|c| dirs.iter().any(|d| manifest_of(d, c.model).is_file()))
        .map(|c| c.model)
        .collect()
}

/// Where the Linux package runs Ollama as a service: its own `ollama` user's
/// home, which a desktop session never sees as `~`.
const LINUX_SERVICE_STORE: &str = "/usr/share/ollama/.ollama/models";

/// Ollama's own default stores on this OS: `~/.ollama/models`, and on Linux
/// the service user's folder above.
fn default_stores() -> Vec<std::path::PathBuf> {
    let mut stores = Vec::new();
    if let Some(home) = dirs::home_dir() {
        stores.push(home.join(".ollama").join("models"));
    }
    if cfg!(target_os = "linux") {
        stores.push(std::path::PathBuf::from(LINUX_SERVICE_STORE));
    }
    stores
}

fn custom_store() -> Option<std::path::PathBuf> {
    std::env::var_os("OLLAMA_MODELS")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

/// Where this computer's Ollama keeps models: the app's own folder (the
/// managed server), `OLLAMA_MODELS`, and Ollama's defaults ([`default_stores`]).
fn models_dirs(data_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![data_dir.join("models").join("ollama")];
    dirs.extend(custom_store());
    dirs.extend(default_stores());
    dirs
}

/// Offered models already on this computer's disk (for the first-launch pin).
pub fn installed_on_disk(data_dir: &std::path::Path) -> Vec<&'static str> {
    installed_in(&models_dirs(data_dir))
}

/// Pin the default once, at the first launch that knows about it. Returns
/// the pinned model when this call wrote it.
///
/// * an explicit choice, an existing install, or `llama3.1` already on the
///   disk → [`LEGACY_DEFAULT`] (what it was already running; an explicit
///   choice that is later cleared falls back to it too, never to a model the
///   user never saw);
/// * a brand new install → the recommendation for this computer's memory,
///   unless an offered model is already downloaded (a library wiped with
///   "delete all my data" leaves Ollama's models alone): then that model, the
///   recommended one if it is there, else the best that fits;
/// * memory unreadable → nothing is pinned this launch (`llama3.1` meanwhile).
///   The launch writes the hardware cache, so the next one is an existing
///   install: it pins `llama3.1`, and the offer carries the better model if
///   the reading is good by then.
///
/// `on_disk` is [`installed_on_disk`].
pub fn migrate_default(
    storage: &fly_storage::Storage,
    ram_gb: f64,
    on_disk: &[&str],
) -> Option<String> {
    if get(storage, DEFAULT_KEY).is_some() {
        return None;
    }
    let pick = if explicit_model(storage).is_some()
        || existing_install(storage)
        || on_disk.contains(&LEGACY_DEFAULT)
    {
        LEGACY_DEFAULT
    } else if !ram_known(ram_gb) {
        tracing::warn!("memory unreadable: the default local model isn't pinned this launch");
        return None;
    } else {
        let rec = recommend(ram_gb, None).model;
        if on_disk.is_empty() || on_disk.contains(&rec) {
            rec
        } else {
            on_disk
                .iter()
                .copied()
                .find(|m| candidate(m).is_some_and(|c| ram_gb >= c.min_ram_gb))
                .or_else(|| on_disk.first().copied())
                .unwrap_or(rec)
        }
    };
    match storage.set_setting(DEFAULT_KEY, pick) {
        Ok(()) => {
            tracing::info!(model = pick, ram_gb, "pinned the default local model");
            Some(pick.to_string())
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not pin the default local model");
            None
        }
    }
}

/// A better model than the one an existing install is using.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Offer {
    pub model: String,
    pub download_bytes: u64,
}

/// The one-time offer, if there is one: the user is on Ollama and has not
/// picked a model themselves, the recommendation is better than their
/// default (by measured quality, so the tied 4B is never offered over
/// `llama3.1`), the running Ollama can pull it, and they haven't said "not
/// now" to it.
pub fn offer(
    storage: &fly_storage::Storage,
    ram_gb: f64,
    ollama_version: Option<&str>,
) -> Option<Offer> {
    let provider = get(storage, "llm.provider").unwrap_or_else(|| "ollama".into());
    if provider != "ollama" || explicit_model(storage).is_some() || !ram_known(ram_gb) {
        return None;
    }
    // A custom or remote server is sized by ITS memory, not this computer's:
    // a model sized for this one would be pulled onto the other box.
    if get(storage, "llm.ollama.base_url")
        .is_some_and(|u| !crate::llm_commands::is_loopback_url(&u))
    {
        return None;
    }
    let rec = recommend(ram_gb, ollama_version);
    let current = effective_default(storage);
    let better = match candidate(&current) {
        Some(cur) => candidate(rec.model).is_some_and(|r| r.rank > cur.rank),
        // A default we know nothing about (hand-edited): never offer over it.
        None => false,
    };
    if !better || get(storage, DISMISSED_KEY).as_deref() == Some(rec.model) {
        return None;
    }
    Some(Offer {
        model: rec.model.to_string(),
        download_bytes: candidate(rec.model)?.download_bytes,
    })
}

/// One row of the picker: a model with what was measured for it.
#[derive(Debug, Serialize)]
pub struct ModelRow {
    pub model: &'static str,
    pub download_bytes: u64,
    pub memory_gb: f64,
    pub ram_class_gb: u8,
    /// This computer has the memory the model is recommended from.
    pub fits: bool,
    /// The recommendation for this computer.
    pub recommended: bool,
    /// The running Ollama is older than the model needs ("0.17.5").
    pub needs_newer_ollama: Option<String>,
    pub measured: Measured,
}

/// Everything the Settings screen needs, in one call.
#[derive(Debug, Serialize)]
pub struct Advice {
    pub ram_gb: f64,
    pub ollama_version: Option<String>,
    pub recommended: String,
    pub held_back: Option<HeldBack>,
    /// What an install with no explicit choice uses (pinned).
    pub default_model: String,
    /// The pinned default needs a newer Ollama than the one running (a fresh
    /// install pinned before the version was known): its download would fail.
    pub default_blocked: Option<HeldBack>,
    pub explicit_model: Option<String>,
    pub offer: Option<Offer>,
    pub models: Vec<ModelRow>,
    pub measured_on: &'static str,
    pub doc_url: &'static str,
}

/// `model` needs a newer Ollama than `ollama_version`.
fn blocked(model: &str, ollama_version: Option<&str>) -> Option<HeldBack> {
    let c = candidate(model)?;
    (!supports(ollama_version, c.min_ollama)).then(|| {
        let [a, b, p] = c.min_ollama;
        HeldBack {
            model: c.model.to_string(),
            needs_ollama: format!("{a}.{b}.{p}"),
            have_ollama: ollama_version.unwrap_or_default().to_string(),
        }
    })
}

pub fn advice(storage: &fly_storage::Storage, ram_gb: f64, ollama_version: Option<&str>) -> Advice {
    let rec = recommend(ram_gb, ollama_version);
    Advice {
        ram_gb: (ram_gb * 10.0).round() / 10.0,
        ollama_version: ollama_version.map(str::to_string),
        recommended: rec.model.to_string(),
        held_back: rec.held_back.clone(),
        default_model: effective_default(storage),
        default_blocked: blocked(&effective_default(storage), ollama_version),
        explicit_model: explicit_model(storage),
        offer: offer(storage, ram_gb, ollama_version),
        models: CANDIDATES
            .iter()
            .map(|c| ModelRow {
                model: c.model,
                download_bytes: c.download_bytes,
                memory_gb: c.memory_gb,
                ram_class_gb: c.ram_class_gb,
                fits: !ram_known(ram_gb) || ram_gb >= c.min_ram_gb,
                recommended: c.model == rec.model,
                needs_newer_ollama: (!supports(ollama_version, c.min_ollama)).then(|| {
                    let [a, b, p] = c.min_ollama;
                    format!("{a}.{b}.{p}")
                }),
                measured: c.measured,
            })
            .collect(),
        measured_on: MEASURED_ON,
        doc_url: BENCH_DOC_URL,
    }
}

/// "Not now" on the offer for `model`: never offered again.
pub fn dismiss_offer(storage: &fly_storage::Storage, model: &str) -> CmdResult<()> {
    let c = candidate(model).ok_or_else(|| format!("'{model}' isn't one of the offered models"))?;
    storage
        .set_setting(DISMISSED_KEY, c.model)
        .map_err(|e| e.to_string())
}

/// The user's click on a model: it becomes their explicit choice (always
/// respected from then on). The UI downloads it first, so this is the switch.
pub fn choose_model(storage: &fly_storage::Storage, model: &str) -> CmdResult<()> {
    let c = candidate(model).ok_or_else(|| format!("'{model}' isn't one of the offered models"))?;
    storage
        .set_setting(EXPLICIT_KEY, c.model)
        .map_err(|e| e.to_string())
}

/// Room kept free beyond a download's own size (Ollama stages the blobs,
/// then writes the manifest, and the computer needs air).
pub const PULL_MARGIN_BYTES: u64 = 2_000_000_000;

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
}

/// Whether a pull of `model` has the disk room it needs, given the free
/// bytes where the models go (None = can't tell: not refused) and how much of
/// it an interrupted pull already wrote there ([`partial_bytes`]), which a
/// resumed pull doesn't download again. Only the offered models have a known
/// size; any other name isn't checked.
pub fn check_room(model: &str, free: Option<u64>, resumable: u64) -> CmdResult<()> {
    let (Some(c), Some(free)) = (candidate(model), free) else {
        return Ok(());
    };
    // The partial files on disk may belong to another model's dropped pull
    // (they can't be told apart), so the credit never exceeds this download.
    let left = c.download_bytes.saturating_sub(resumable);
    let need = left.saturating_add(PULL_MARGIN_BYTES);
    if free >= need {
        return Ok(());
    }
    let download = if resumable > 0 {
        format!("{} still to download of {}", gb(left), gb(c.download_bytes))
    } else {
        format!("a {} download", gb(c.download_bytes))
    };
    Err(format!(
        "Not enough free disk space for {}. It needs about {} free ({download} plus {} to spare) and this drive has {}. Free up space (Settings › Your files and privacy shows what the app keeps), or choose a smaller model.",
        c.model,
        gb(need),
        gb(PULL_MARGIN_BYTES),
        gb(free)
    ))
}

/// Bytes an interrupted pull already wrote under `models_dir`, which a resume
/// keeps. Ollama downloads a layer into a sparse `blobs/<digest>-partial`
/// (its length is the whole layer, so it says nothing about progress) and
/// records each chunk's progress in a small `<digest>-partial-<n>` JSON with a
/// `Completed` byte count. Those counts are what the disk really holds.
///
/// A record without its data file beside it is stale and not counted, and
/// anything unreadable counts as nothing (the pull then just has to fit in
/// full).
pub fn partial_bytes(models_dir: &std::path::Path) -> u64 {
    let blobs = models_dir.join("blobs");
    let Ok(entries) = std::fs::read_dir(&blobs) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let (blob, n) = name.rsplit_once("-partial-")?;
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            if !blobs.join(format!("{blob}-partial")).is_file() {
                return None;
            }
            // A record is a few dozen bytes; anything bigger isn't one.
            if e.metadata().ok()?.len() > 4096 {
                return None;
            }
            let record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
            record
                .as_object()?
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("completed"))?
                .1
                .as_u64()
        })
        .fold(0, u64::saturating_add)
}

/// A folder that looks like an Ollama store: it exists and holds `manifests/`
/// (a running server creates it at startup, so a real store has it).
fn is_store(dir: &std::path::Path) -> bool {
    dir.join("manifests").is_dir()
}

/// Where a user-run server keeps its models, as far as this process can tell:
/// `custom` (`OLLAMA_MODELS`) when set, else the first of Ollama's `defaults`
/// that is a store. Only a folder that [`is_store`] is trusted, and a set but
/// unusable `OLLAMA_MODELS` isn't second-guessed by a default (the server
/// likely reads the same variable). None means "can't tell": the pull isn't
/// checked rather than measured against the wrong drive. A relocated store
/// this process can't see (the Ollama app's "Model location", a service's
/// environment) lands here too whenever no visible folder is a store.
fn user_store(
    custom: Option<std::path::PathBuf>,
    defaults: &[std::path::PathBuf],
) -> Option<std::path::PathBuf> {
    match custom {
        Some(dir) => is_store(&dir).then_some(dir),
        None => defaults.iter().find(|d| is_store(d)).cloned(),
    }
}

/// Where a pull lands. The app's own models folder for the server this app
/// manages (authoritative: nothing the user set elsewhere applies to it); a
/// user-run local server's store ([`user_store`]); None for a server on
/// another computer.
fn target_dir(
    local: bool,
    managed: bool,
    data_dir: &std::path::Path,
    custom: Option<std::path::PathBuf>,
    defaults: &[std::path::PathBuf],
) -> Option<std::path::PathBuf> {
    if !local {
        return None;
    }
    if managed {
        return Some(data_dir.join("models").join("ollama"));
    }
    user_store(custom, defaults)
}

/// The folder whose drive a pull fills, None when that isn't known ([`target_dir`]).
pub fn pull_target_dir(state: &AppState, root: &str, managed: bool) -> Option<std::path::PathBuf> {
    target_dir(
        crate::llm_commands::is_loopback_url(root),
        managed,
        &state.data_dir,
        custom_store(),
        &default_stores(),
    )
}

/// A Windows extended-length path (`\\?\C:\x`, what `canonicalize` returns)
/// as the plain `C:\x` that the disk list's mount points are written in. A
/// network path (`\\?\UNC\…`) has no drive to measure and stays as it is.
fn without_verbatim(path: std::path::PathBuf) -> std::path::PathBuf {
    let plain = path
        .to_str()
        .and_then(|s| s.strip_prefix(r"\\?\"))
        .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
        .map(std::path::PathBuf::from);
    plain.unwrap_or(path)
}

/// `dir` with every link, junction and symlink on the way resolved, so the
/// drive that is measured is the one the files land on. A tail that doesn't
/// exist yet (a store the first pull creates) is kept as it is under its
/// nearest existing parent.
fn resolve_dir(dir: &std::path::Path) -> std::path::PathBuf {
    let mut probe = dir;
    loop {
        if let Ok(real) = std::fs::canonicalize(probe) {
            let tail = dir.strip_prefix(probe).unwrap_or(std::path::Path::new(""));
            return without_verbatim(real.join(tail));
        }
        match probe.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => probe = parent,
            _ => return dir.to_path_buf(),
        }
    }
}

/// Free bytes on the drive that `dir` really lives on; None when unknown.
pub fn free_bytes_at(dir: &std::path::Path) -> Option<u64> {
    crate::audio_store::free_space(&resolve_dir(dir))
}

#[tauri::command]
pub async fn local_model_advice(state: State<'_, AppState>) -> CmdResult<Advice> {
    let root = crate::ollama::root_url(&state);
    let version = crate::ollama::server_version(&root).await;
    let storage = state.storage.lock().unwrap();
    Ok(advice(&storage, total_ram_gb(), version.as_deref()))
}

#[tauri::command]
pub fn dismiss_local_model_offer(state: State<'_, AppState>, model: String) -> CmdResult<()> {
    dismiss_offer(&state.storage.lock().unwrap(), &model)
}

#[tauri::command]
pub fn use_local_model(state: State<'_, AppState>, model: String) -> CmdResult<()> {
    choose_model(&state.storage.lock().unwrap(), &model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage() -> (tempfile::TempDir, fly_storage::Storage) {
        let dir = tempfile::tempdir().unwrap();
        let s = fly_storage::Storage::open(dir.path()).unwrap();
        (dir, s)
    }

    #[test]
    fn memory_cutoffs_pick_the_model_at_each_boundary() {
        let pick = |ram| recommend(ram, None).model;
        // 4 and 6 GB computers: nothing measured fits well; the lightest.
        assert_eq!(pick(4.0), "qwen3.5:4b");
        assert_eq!(pick(6.9), "qwen3.5:4b");
        // The 8 GB class (7.4–7.9 reported).
        assert_eq!(pick(7.0), "qwen3.5:4b");
        assert_eq!(pick(7.9), "qwen3.5:4b");
        assert_eq!(pick(10.9), "qwen3.5:4b");
        // The 12 GB class.
        assert_eq!(pick(11.0), "llama3.1");
        assert_eq!(pick(11.7), "llama3.1");
        assert_eq!(pick(14.9), "llama3.1");
        // The 16 GB class (a "16 GB" computer reads 15.2–16.0).
        assert_eq!(pick(15.0), "qwen3.5:9b");
        assert_eq!(pick(15.2), "qwen3.5:9b");
        assert_eq!(pick(16.0), "qwen3.5:9b");
        // 32 GB and up stay on the measured 9B, not gemma4 or a 35B.
        assert_eq!(pick(31.7), "qwen3.5:9b");
        assert_eq!(pick(64.0), "qwen3.5:9b");
        assert_eq!(pick(128.0), "qwen3.5:9b");
    }

    #[test]
    fn the_cutoffs_follow_the_table_not_a_second_copy() {
        for c in CANDIDATES {
            assert_eq!(recommend(c.min_ram_gb, None).model, c.model, "{}", c.model);
            assert!(c.ram_class_gb as f64 >= c.min_ram_gb, "{}", c.model);
            assert!(c.min_ram_gb > c.memory_gb + 3.0, "{}", c.model);
        }
    }

    #[test]
    fn an_old_ollama_takes_qwen_away_and_says_so() {
        // Too old for qwen3.5: the 16 GB class falls back to llama3.1.
        let r = recommend(16.0, Some("0.16.9"));
        assert_eq!(r.model, "llama3.1");
        let h = r.held_back.unwrap();
        assert_eq!(
            (
                h.model.as_str(),
                h.needs_ollama.as_str(),
                h.have_ollama.as_str()
            ),
            ("qwen3.5:9b", "0.17.5", "0.16.9")
        );
        // The low-memory fallback is also qwen: only llama3.1 is left.
        let r = recommend(8.0, Some("0.16.9"));
        assert_eq!(r.model, "llama3.1");
        assert_eq!(r.held_back.unwrap().model, "qwen3.5:4b");
        // Exactly the minimum, newer, and a pre-release of the minimum.
        assert!(recommend(16.0, Some("0.17.5")).held_back.is_none());
        assert!(recommend(16.0, Some("0.40.13")).held_back.is_none());
        assert!(recommend(16.0, Some("v0.31.2")).held_back.is_none());
        assert!(recommend(16.0, Some("0.17.5-rc1")).held_back.is_none());
        assert!(recommend(16.0, Some("0.17.4")).held_back.is_some());
        // Unknown or unreadable: not held back (the pull says so itself).
        assert!(recommend(16.0, None).held_back.is_none());
        assert!(recommend(16.0, Some("dev")).held_back.is_none());
        // llama3.1 never needs a newer Ollama.
        assert!(recommend(12.0, Some("0.1.0")).held_back.is_none());
    }

    #[test]
    fn versions_parse_the_way_ollama_prints_them() {
        assert_eq!(parse_version("0.40.13"), Some([0, 40, 13]));
        assert_eq!(parse_version("v0.31.2"), Some([0, 31, 2]));
        assert_eq!(parse_version("0.40.14-rc0"), Some([0, 40, 14]));
        assert_eq!(parse_version("1.2"), Some([1, 2, 0]));
        assert_eq!(parse_version("dev"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn a_fresh_install_is_pinned_to_the_recommendation_once() {
        let (_d, s) = storage();
        assert_eq!(effective_default(&s), "llama3.1", "before the migration");
        assert_eq!(
            migrate_default(&s, 31.7, &[]).as_deref(),
            Some("qwen3.5:9b")
        );
        assert_eq!(effective_default(&s), "qwen3.5:9b");
        // Once: a later run on a bigger or smaller computer changes nothing.
        assert_eq!(migrate_default(&s, 8.0, &[]), None);
        assert_eq!(effective_default(&s), "qwen3.5:9b");
        // A small computer's fresh install gets the fitting model.
        let (_d, s) = storage();
        migrate_default(&s, 7.7, &[]);
        assert_eq!(effective_default(&s), "qwen3.5:4b");
    }

    #[test]
    fn an_existing_install_keeps_what_it_was_using() {
        // Each sign of a library in use pins llama3.1, whatever the memory.
        let signs: [(&str, &str); 4] = [
            ("consent.recording_notice_accepted", "true"),
            ("llm.provider", "ollama"),
            ("llm.ollama.base_url", "http://localhost:11434"),
            ("llm.provider", "anthropic"),
        ];
        for (k, v) in signs {
            let (_d, s) = storage();
            s.set_setting(k, v).unwrap();
            assert_eq!(
                migrate_default(&s, 64.0, &[]).as_deref(),
                Some("llama3.1"),
                "{k}"
            );
            assert_eq!(effective_default(&s), "llama3.1", "{k}");
        }
        // Notes in the library count too.
        let (_d, s) = storage();
        s.create_note("A note", None).unwrap();
        assert_eq!(migrate_default(&s, 64.0, &[]).as_deref(), Some("llama3.1"));
    }

    #[test]
    fn an_explicit_choice_is_never_overridden() {
        let (_d, s) = storage();
        s.set_setting("llm.ollama.model", "mistral").unwrap();
        // Pinned legacy underneath, so clearing the choice later can't land
        // on a model the user never saw.
        assert_eq!(migrate_default(&s, 64.0, &[]).as_deref(), Some("llama3.1"));
        assert_eq!(explicit_model(&s).as_deref(), Some("mistral"));
        // And no offer, on any memory.
        assert_eq!(offer(&s, 64.0, None), None);
        s.set_setting("llm.ollama.model", "").unwrap();
        assert_eq!(explicit_model(&s), None, "a cleared choice is no choice");
        assert_eq!(effective_default(&s), "llama3.1");
    }

    #[test]
    fn an_existing_install_is_offered_a_better_model_never_switched() {
        let (_d, s) = storage();
        s.set_setting("llm.provider", "ollama").unwrap();
        migrate_default(&s, 32.0, &[]);
        // Never switched: still llama3.1.
        assert_eq!(effective_default(&s), "llama3.1");
        let o = offer(&s, 32.0, Some("0.40.13")).unwrap();
        assert_eq!(o.model, "qwen3.5:9b");
        assert_eq!(o.download_bytes, 6_600_000_000);
        // The offer is the same until the user answers it.
        assert_eq!(offer(&s, 32.0, Some("0.40.13")), Some(o));
        // The 12 GB class has nothing better than llama3.1 to offer.
        assert_eq!(offer(&s, 12.0, None), None);
        // The tied 4B is never offered over llama3.1, even where it fits better.
        assert_eq!(offer(&s, 8.0, None), None);
        // A running Ollama too old for the 9B: nothing to offer, not a failing pull.
        assert_eq!(offer(&s, 32.0, Some("0.16.0")), None);
    }

    #[test]
    fn nothing_is_offered_to_a_fresh_install_or_a_cloud_user() {
        let (_d, s) = storage();
        migrate_default(&s, 32.0, &[]); // pinned to the 9B itself
        assert_eq!(offer(&s, 32.0, None), None);
        let (_d, s) = storage();
        s.set_setting("llm.provider", "anthropic").unwrap();
        migrate_default(&s, 32.0, &[]);
        assert_eq!(
            offer(&s, 32.0, None),
            None,
            "a cloud provider isn't a local model's customer"
        );
    }

    #[test]
    fn dismissing_the_offer_is_for_that_model_only() {
        let (_d, s) = storage();
        s.set_setting("llm.provider", "ollama").unwrap();
        migrate_default(&s, 32.0, &[]);
        assert!(offer(&s, 32.0, None).is_some());
        dismiss_offer(&s, "qwen3.5:9b").unwrap();
        assert_eq!(offer(&s, 32.0, None), None, "dismissed");
        // The default did not move because of the dismissal.
        assert_eq!(effective_default(&s), "llama3.1");
        // A different recommendation is a new offer (a model dismissed for a
        // smaller computer doesn't silence a later one).
        dismiss_offer(&s, "qwen3.5:4b").unwrap();
        assert!(offer(&s, 32.0, None).is_some());
        // Only offered models can be named.
        assert!(dismiss_offer(&s, "rm -rf").is_err());
    }

    #[test]
    fn accepting_makes_an_explicit_choice_that_ends_the_offer() {
        let (_d, s) = storage();
        s.set_setting("llm.provider", "ollama").unwrap();
        migrate_default(&s, 32.0, &[]);
        choose_model(&s, "qwen3.5:9b").unwrap();
        assert_eq!(explicit_model(&s).as_deref(), Some("qwen3.5:9b"));
        assert_eq!(offer(&s, 32.0, None), None);
        // The tag-insensitive spelling is stored as the offered one.
        choose_model(&s, "llama3.1:latest").unwrap();
        assert_eq!(explicit_model(&s).as_deref(), Some("llama3.1"));
        assert!(
            choose_model(&s, "gemma4:12b-it-qat").is_err(),
            "not promoted"
        );
        assert!(choose_model(&s, "").is_err());
    }

    #[test]
    fn the_table_holds_numbers_only_and_every_row_is_sane() {
        assert_eq!(CANDIDATES.len(), 3);
        for c in CANDIDATES {
            let m = c.measured;
            assert!(m.sourced_pct <= 100 && m.declines_pct <= 100, "{}", c.model);
            assert!(
                m.seconds > 0 && m.seconds.is_multiple_of(5),
                "{} is rounded to 5 s",
                c.model
            );
            assert!(c.download_bytes > 1_000_000_000, "{}", c.model);
        }
        // Best first, and "better" means a higher measured rank.
        assert!(CANDIDATES
            .windows(2)
            .all(|w| w[0].min_ram_gb >= w[1].min_ram_gb));
        assert!(MEASURED_ON.contains("one test laptop") && MEASURED_ON.contains("other programs"));
        assert!(BENCH_DOC_URL.ends_with("docs/ASK-MODEL-BENCH.md"));
    }

    #[test]
    fn advice_describes_this_computer_and_marks_the_recommendation() {
        let (_d, s) = storage();
        s.set_setting("llm.provider", "ollama").unwrap();
        migrate_default(&s, 16.0, &[]);
        let a = advice(&s, 15.7, Some("0.40.13"));
        assert_eq!(a.recommended, "qwen3.5:9b");
        assert_eq!(a.default_model, "llama3.1");
        assert_eq!(a.ram_gb, 15.7);
        assert!(a.offer.is_some() && a.explicit_model.is_none());
        let rec: Vec<_> = a.models.iter().filter(|m| m.recommended).collect();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].model, "qwen3.5:9b");
        assert!(a.models.iter().all(|m| m.fits), "15.7 GB holds all three");
        // An 8 GB computer: only the 4B fits, and the 9B says so.
        let a = advice(&s, 7.7, None);
        let fits: Vec<_> = a
            .models
            .iter()
            .filter(|m| m.fits)
            .map(|m| m.model)
            .collect();
        assert_eq!(fits, vec!["qwen3.5:4b"]);
        // An old Ollama is named on the rows it rules out.
        let a = advice(&s, 16.0, Some("0.16.0"));
        let needs: Vec<_> = a
            .models
            .iter()
            .filter(|m| m.needs_newer_ollama.is_some())
            .map(|m| m.model)
            .collect();
        assert_eq!(needs, vec!["qwen3.5:9b", "qwen3.5:4b"]);
        assert_eq!(a.held_back.unwrap().needs_ollama, "0.17.5");
    }

    #[test]
    fn memory_detection_returns_something_plausible() {
        let gb = total_ram_gb();
        assert!((1.0..16_384.0).contains(&gb), "{gb}");
    }

    /// The provider Ask and every other local task get is built from the
    /// resolved default, not a constant: pinned for this install, with an
    /// explicit pick winning (llm_commands.rs build_provider).
    #[test]
    fn every_local_task_gets_the_resolved_model() {
        use crate::llm_commands::{build_provider, DataScope};
        let build = |setup: &dyn Fn(&fly_storage::Storage)| {
            let dir = tempfile::tempdir().unwrap();
            let secrets = std::sync::Arc::new(fly_secrets::MemorySecretStore::default());
            let state =
                crate::state::AppState::init_with(dir.path().to_path_buf(), secrets).unwrap();
            setup(&state.storage.lock().unwrap());
            let p = build_provider(&state, DataScope::NoMeetingData).unwrap();
            (p.id(), p.model().to_string(), state, dir)
        };
        // Nothing saved (a library from before the pin): what it always used.
        let (id, model, _s, _d) = build(&|_| {});
        assert_eq!((id, model.as_str()), ("ollama", "llama3.1"));
        // A fresh install pinned to the 9B.
        let (_, model, s, _d) = build(&|st| {
            migrate_default(st, 31.7, &[]);
        });
        assert_eq!(model, "qwen3.5:9b");
        // The settings screen and the in-use guard agree with the provider.
        assert_eq!(effective_default(&s.storage.lock().unwrap()), "qwen3.5:9b");
        // An existing install on a big computer is pinned to what it ran.
        let (_, model, _s, _d) = build(&|st| {
            st.set_setting("llm.provider", "ollama").unwrap();
            migrate_default(st, 64.0, &[]);
        });
        assert_eq!(model, "llama3.1", "never switched by the pin");
        // An explicit pick beats the pin, on any memory.
        let (_, model, _s, _d) = build(&|st| {
            migrate_default(st, 64.0, &[]);
            st.set_setting("llm.ollama.model", "qwen3.5:4b").unwrap();
        });
        assert_eq!(model, "qwen3.5:4b");
        // Clicking a model (use_local_model) is that explicit pick.
        let (_, model, _s, _d) = build(&|st| {
            st.set_setting("llm.provider", "ollama").unwrap();
            migrate_default(st, 64.0, &[]);
            choose_model(st, "qwen3.5:9b").unwrap();
        });
        assert_eq!(model, "qwen3.5:9b");
    }

    /// The model the app won't let you delete is the one in use: the pinned
    /// default counts, not a constant that no longer matches.
    #[test]
    fn the_delete_guard_protects_the_pinned_default() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = std::sync::Arc::new(fly_secrets::MemorySecretStore::default());
        let state = crate::state::AppState::init_with(dir.path().to_path_buf(), secrets).unwrap();
        migrate_default(&state.storage.lock().unwrap(), 31.7, &[]);
        assert_eq!(
            crate::ollama::selected_model(&state).as_deref(),
            Some("qwen3.5:9b")
        );
        state
            .storage
            .lock()
            .unwrap()
            .set_setting("llm.provider", "anthropic")
            .unwrap();
        assert_eq!(
            crate::ollama::selected_model(&state),
            None,
            "a cloud provider uses none"
        );
    }

    #[test]
    fn a_pull_is_refused_when_the_disk_cannot_hold_it() {
        // 6.6 GB download + 2 GB margin = 8.6 GB for the 9B.
        let need = 6_600_000_000 + PULL_MARGIN_BYTES;
        assert!(check_room("qwen3.5:9b", Some(need), 0).is_ok());
        let err = check_room("qwen3.5:9b", Some(need - 1), 0).unwrap_err();
        assert!(
            err.starts_with("Not enough free disk space for qwen3.5:9b."),
            "{err}"
        );
        for part in [
            "about 8.6 GB free",
            "a 6.6 GB download",
            "2.0 GB to spare",
            "has 8.6 GB",
        ] {
            assert!(err.contains(part), "{part} in {err}");
        }
        assert!(err.contains("choose a smaller model"));
        // Plain words, no codes.
        assert!(!err.contains("bytes") && !err.contains("ENOSPC"));
        // Each offered model's own size.
        assert!(check_room("llama3.1", Some(4_920_000_000 + PULL_MARGIN_BYTES), 0).is_ok());
        assert!(check_room("llama3.1", Some(4_920_000_000), 0).is_err());
        assert!(check_room("qwen3.5:4b", Some(3_330_000_000 + PULL_MARGIN_BYTES), 0).is_ok());
        assert!(check_room("qwen3.5:4b", Some(1_000_000_000), 0).is_err());
        // A tag-insensitive name is the same model.
        assert!(check_room("llama3.1:latest", Some(1), 0).is_err());
        // Can't tell the free space, or not a model we know the size of: not refused.
        assert!(check_room("qwen3.5:9b", None, 0).is_ok());
        assert!(check_room("some-custom-model", Some(1), 0).is_ok());
    }

    /// A pull that dropped at 5 GB of 6.6 only needs the other 1.6 GB (plus
    /// the margin), not the whole download again.
    #[test]
    fn a_resumed_pull_is_only_charged_for_what_is_left() {
        let have = 5_000_000_000;
        // 1.6 GB left + 2.0 GB to spare = 3.6 GB.
        let need = 1_600_000_000 + PULL_MARGIN_BYTES;
        assert!(check_room("qwen3.5:9b", Some(need), have).is_ok());
        // The same drive refuses it from scratch (8.6 GB), which is the old behaviour.
        assert!(check_room("qwen3.5:9b", Some(need), 0).is_err());
        assert!(check_room("qwen3.5:9b", Some(4_000_000_000), have).is_ok());
        // Still refused below what is left, in words that say it is a resume.
        let err = check_room("qwen3.5:9b", Some(need - 1), have).unwrap_err();
        for part in [
            "about 3.6 GB free",
            "1.6 GB still to download of 6.6 GB",
            "2.0 GB to spare",
        ] {
            assert!(err.contains(part), "{part} in {err}");
        }
        // Another model's dropped pull can't make this one free: the credit
        // stops at this download's size.
        assert!(check_room("qwen3.5:9b", Some(PULL_MARGIN_BYTES), u64::MAX).is_ok());
        assert!(check_room("qwen3.5:9b", Some(PULL_MARGIN_BYTES - 1), u64::MAX).is_err());
    }

    fn write(path: &std::path::Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// What Ollama leaves behind when a pull drops: a sparse data file plus a
    /// small JSON record per chunk with its `Completed` bytes.
    #[test]
    fn partial_bytes_add_up_what_ollama_already_wrote() {
        let store = tempfile::tempdir().unwrap();
        let blobs = store.path().join("blobs");
        assert_eq!(partial_bytes(store.path()), 0, "no blobs folder");

        // One layer, three chunks (the key's case doesn't matter).
        write(&blobs.join("sha256-aaa-partial"), "x");
        write(
            &blobs.join("sha256-aaa-partial-0"),
            r#"{"N":0,"Offset":0,"Size":100,"Completed":100}"#,
        );
        write(
            &blobs.join("sha256-aaa-partial-1"),
            r#"{"N":1,"Offset":100,"Size":100,"Completed":40}"#,
        );
        write(
            &blobs.join("sha256-aaa-partial-10"),
            r#"{"n":10,"offset":200,"size":100,"completed":5}"#,
        );
        // A second layer.
        write(&blobs.join("sha256-bbb-partial"), "x");
        write(
            &blobs.join("sha256-bbb-partial-0"),
            r#"{"N":0,"Offset":0,"Size":50,"Completed":10}"#,
        );
        assert_eq!(partial_bytes(store.path()), 100 + 40 + 5 + 10);

        // Not counted: a record whose data file is gone, one that isn't JSON
        // or has no count, a name that isn't a chunk number, a finished blob.
        write(
            &blobs.join("sha256-ccc-partial-0"),
            r#"{"N":0,"Size":9,"Completed":999}"#,
        );
        write(&blobs.join("sha256-ddd-partial"), "x");
        write(&blobs.join("sha256-ddd-partial-0"), "not json");
        write(&blobs.join("sha256-eee-partial"), "x");
        write(&blobs.join("sha256-eee-partial-0"), r#"{"N":0,"Size":9}"#);
        write(&blobs.join("sha256-fff-partial"), "x");
        write(&blobs.join("sha256-fff-partial-x"), r#"{"Completed":777}"#);
        write(&blobs.join("sha256-ggg"), "finished layer");
        assert_eq!(partial_bytes(store.path()), 155);
    }

    /// A store this process can't see is skipped, not measured on the wrong
    /// drive; a folder is trusted only when it has `manifests/`.
    #[test]
    fn a_user_run_store_is_trusted_only_when_it_looks_like_one() {
        let tmp = tempfile::tempdir().unwrap();
        let store = |name: &str| {
            let d = tmp.path().join(name);
            std::fs::create_dir_all(d.join("manifests")).unwrap();
            d
        };
        let (relocated, home) = (store("relocated"), store("home"));
        let bare = tmp.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        let missing = tmp.path().join("missing");

        let homes = std::slice::from_ref(&home);

        // OLLAMA_MODELS wins when it is a store.
        assert_eq!(
            user_store(Some(relocated.clone()), homes),
            Some(relocated.clone())
        );
        // Set but not a store (no folder, or an empty one): skip the check,
        // and don't guess a default the server may not be using.
        assert_eq!(user_store(Some(missing.clone()), homes), None);
        assert_eq!(user_store(Some(bare.clone()), homes), None);
        // Not set: the first default that is a store.
        assert_eq!(
            user_store(None, &[missing.clone(), bare.clone(), home.clone()]),
            Some(home.clone())
        );
        // Nothing visible is a store (a relocated one this process can't
        // see): can't tell.
        assert_eq!(user_store(None, &[missing, bare]), None);
        assert_eq!(user_store(None, &[]), None);
    }

    #[test]
    fn the_managed_store_stays_authoritative() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(other.join("manifests")).unwrap();
        let managed = data.join("models").join("ollama");
        // The app's own folder, even before it exists and whatever the user
        // set for a server of their own.
        assert_eq!(
            target_dir(
                true,
                true,
                &data,
                Some(other.clone()),
                std::slice::from_ref(&other)
            ),
            Some(managed)
        );
        // A user-run local server: its store, or nothing.
        assert_eq!(
            target_dir(true, false, &data, Some(other.clone()), &[]),
            Some(other.clone())
        );
        assert_eq!(target_dir(true, false, &data, None, &[]), None);
        // Another computer: never measured here.
        assert_eq!(
            target_dir(false, true, &data, Some(other.clone()), &[other]),
            None
        );
    }

    #[test]
    fn extended_length_paths_are_written_plain_for_the_disk_list() {
        let plain = |s: &str| without_verbatim(std::path::PathBuf::from(s));
        assert_eq!(
            plain(r"\\?\C:\Users\x\models"),
            std::path::Path::new(r"C:\Users\x\models")
        );
        // No drive to measure on a network path; anything else is untouched.
        assert_eq!(
            plain(r"\\?\UNC\srv\share\m"),
            std::path::Path::new(r"\\?\UNC\srv\share\m")
        );
        assert_eq!(plain(r"D:\models"), std::path::Path::new(r"D:\models"));
        assert_eq!(
            plain("/usr/share/ollama"),
            std::path::Path::new("/usr/share/ollama")
        );
    }

    /// A directory symlink (Unix) or junction (Windows, no privilege needed).
    /// False when the platform refuses.
    fn make_dir_link(target: &std::path::Path, link: &std::path::Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            // Backslashes only: mklink reads "/x" as a switch.
            let win = |p: &std::path::Path| p.to_string_lossy().replace('/', "\\");
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(win(link))
                .arg(win(target))
                .output()
                .is_ok_and(|o| o.status.success())
        }
    }

    /// The store that is a link to another drive is measured where it really
    /// is, and the path handed to the disk list is a plain one.
    #[test]
    fn the_store_is_resolved_through_a_link_before_free_space_is_read() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("big-drive");
        std::fs::create_dir_all(real.join("models").join("manifests")).unwrap();
        let link = tmp.path().join("home-link");
        if !make_dir_link(&real, &link) {
            eprintln!("skipped: this platform refused to make a link");
            return;
        }
        let want = without_verbatim(std::fs::canonicalize(&real).unwrap());

        let resolved = resolve_dir(&link.join("models"));
        assert_eq!(resolved, want.join("models"));
        assert_ne!(resolved, link.join("models"), "the link was followed");
        // A tail that doesn't exist yet is kept under its resolved parent.
        assert_eq!(
            resolve_dir(&link.join("models").join("not").join("yet")),
            want.join("models").join("not").join("yet")
        );
        assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
        // And the link is a store through the same eyes.
        assert!(is_store(&link.join("models")));
    }

    #[test]
    fn the_linux_service_store_is_searched_on_linux_only() {
        let service = std::path::PathBuf::from("/usr/share/ollama/.ollama/models");
        let on_linux = cfg!(target_os = "linux");
        assert_eq!(default_stores().contains(&service), on_linux);
        assert_eq!(
            models_dirs(std::path::Path::new("data")).contains(&service),
            on_linux
        );
    }

    #[test]
    fn unreadable_memory_pins_nothing_and_offers_nothing() {
        assert!(!ram_known(0.0) && !ram_known(0.9) && ram_known(1.0));
        assert_eq!(recommend(0.0, None).model, "llama3.1", "the safe default");
        // A fresh install on an unreadable computer isn't pinned to the 4B
        // forever: nothing is written and the default stays llama3.1.
        let (_d, s) = storage();
        assert_eq!(migrate_default(&s, 0.0, &[]), None);
        assert_eq!(effective_default(&s), "llama3.1");
        // That launch also writes the hardware cache, so the next one is an
        // existing install: it pins llama3.1 (not the recommendation), and
        // the better model comes as the one-time offer, once the reading is
        // good.
        s.set_setting(crate::hw::CACHE_KEY, "x").unwrap();
        assert_eq!(migrate_default(&s, 31.7, &[]).as_deref(), Some("llama3.1"));
        assert_eq!(
            offer(&s, 31.7, None).map(|o| o.model).as_deref(),
            Some("qwen3.5:9b")
        );
        // An existing install needs no reading to keep what it runs.
        let (_d, s) = storage();
        s.set_setting("llm.provider", "ollama").unwrap();
        assert_eq!(migrate_default(&s, 0.0, &[]).as_deref(), Some("llama3.1"));
        // No offer on a reading that isn't one, and no row claims a misfit.
        assert_eq!(offer(&s, 0.0, None), None);
        assert!(advice(&s, 0.0, None).models.iter().all(|m| m.fits));
    }

    #[test]
    fn traces_of_an_earlier_launch_make_an_install_existing() {
        // A v2.4 library with nothing else: the hardware cache is written by
        // every launch, and the v2.4 consent migration by the first v2.4 one.
        for key in [
            crate::hw::CACHE_KEY,
            crate::llm_commands::CLOUD_CONSENT_RESET_FLAG,
        ] {
            let (_d, s) = storage();
            s.set_setting(key, "x").unwrap();
            assert_eq!(
                migrate_default(&s, 64.0, &[]).as_deref(),
                Some("llama3.1"),
                "{key}"
            );
        }
        // llama3.1 already downloaded: that is what it has been running.
        let (_d, s) = storage();
        assert_eq!(
            migrate_default(&s, 64.0, &["llama3.1"]).as_deref(),
            Some("llama3.1")
        );
        assert_eq!(effective_default(&s), "llama3.1");
    }

    #[test]
    fn a_fresh_library_pins_a_model_that_is_already_on_the_disk() {
        // After "delete all my data" Ollama's models are still there.
        let pin = |ram: f64, on_disk: &[&str]| {
            let (_d, s) = storage();
            migrate_default(&s, ram, on_disk);
            effective_default(&s)
        };
        assert_eq!(
            pin(32.0, &[]),
            "qwen3.5:9b",
            "nothing downloaded: the recommendation"
        );
        assert_eq!(
            pin(32.0, &["qwen3.5:4b"]),
            "qwen3.5:4b",
            "what is there, not a 6.6 GB download"
        );
        assert_eq!(
            pin(32.0, &["qwen3.5:9b", "qwen3.5:4b"]),
            "qwen3.5:9b",
            "the recommendation if it is there"
        );
        assert_eq!(
            pin(12.0, &["qwen3.5:9b", "qwen3.5:4b"]),
            "qwen3.5:4b",
            "the best that fits"
        );
        assert_eq!(
            pin(8.0, &["qwen3.5:9b"]),
            "qwen3.5:9b",
            "only what there is"
        );
    }

    #[test]
    fn manifests_on_disk_say_which_offered_models_are_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let put = |root: &std::path::Path, rel: &str| {
            let p = root.join("manifests/registry.ollama.ai/library").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "{}").unwrap();
        };
        assert!(installed_in(&[dir.path().to_path_buf()]).is_empty());
        put(dir.path(), "llama3.1/latest");
        put(other.path(), "qwen3.5/9b");
        put(other.path(), "qwen3.5/27b"); // not an offered model
        let both = [dir.path().to_path_buf(), other.path().to_path_buf()];
        assert_eq!(
            installed_in(&both),
            vec!["qwen3.5:9b", "llama3.1"],
            "best first"
        );
        assert_eq!(installed_in(&both[..1]), vec!["llama3.1"]);
        // A folder that doesn't exist is no model.
        assert!(installed_in(&[std::path::PathBuf::from("/no/such/models")]).is_empty());
    }

    #[test]
    fn another_computers_ollama_is_never_offered_this_computers_model() {
        let (_d, s) = storage();
        s.set_setting("llm.provider", "ollama").unwrap();
        migrate_default(&s, 32.0, &[]);
        assert!(offer(&s, 32.0, None).is_some());
        // Sized by THIS computer's memory, it would land 6.6 GB on the other box.
        s.set_setting("llm.ollama.base_url", "http://gpu-box.example.net:11434")
            .unwrap();
        assert_eq!(offer(&s, 32.0, None), None);
        // Another port on this computer is still this computer.
        s.set_setting("llm.ollama.base_url", "http://127.0.0.1:11500")
            .unwrap();
        assert!(offer(&s, 32.0, None).is_some());
        s.set_setting("llm.ollama.base_url", "http://localhost:11434")
            .unwrap();
        assert!(offer(&s, 32.0, None).is_some());
        // A custom base URL also marks an existing install: no fresh 9B pin.
        let (_d, s) = storage();
        s.set_setting("llm.ollama.base_url", "http://gpu-box.example.net:11434")
            .unwrap();
        assert_eq!(migrate_default(&s, 64.0, &[]).as_deref(), Some("llama3.1"));
    }

    #[test]
    fn a_pinned_default_the_running_ollama_cannot_pull_is_flagged() {
        let (_d, s) = storage();
        migrate_default(&s, 32.0, &[]); // pinned before the version was known
        assert_eq!(effective_default(&s), "qwen3.5:9b");
        let a = advice(&s, 32.0, Some("0.16.9"));
        let b = a.default_blocked.expect("flagged");
        assert_eq!(
            (
                b.model.as_str(),
                b.needs_ollama.as_str(),
                b.have_ollama.as_str()
            ),
            ("qwen3.5:9b", "0.17.5", "0.16.9")
        );
        // Not when it can run, when the version is unknown, or for a dev build.
        assert!(advice(&s, 32.0, Some("0.40.13")).default_blocked.is_none());
        assert!(advice(&s, 32.0, None).default_blocked.is_none());
        assert!(advice(&s, 32.0, Some("0.0.0")).default_blocked.is_none());
        // llama3.1 can always be pulled.
        let (_d, s) = storage();
        s.set_setting("llm.provider", "ollama").unwrap();
        migrate_default(&s, 32.0, &[]);
        assert!(advice(&s, 32.0, Some("0.1.0")).default_blocked.is_none());
    }

    #[test]
    fn a_development_build_of_ollama_is_not_too_old() {
        assert!(supports(Some("0.0.0"), [0, 17, 5]));
        assert!(recommend(16.0, Some("0.0.0")).held_back.is_none());
        assert!(
            !supports(Some("0.0.1"), [0, 17, 5]),
            "a real old version still is"
        );
    }

    #[test]
    fn only_the_9b_is_slower_for_the_work_after_a_meeting() {
        let slower: Vec<_> = CANDIDATES
            .iter()
            .filter_map(|c| c.measured.background_slower.map(|x| (c.model, x)))
            .collect();
        assert_eq!(slower, vec![("qwen3.5:9b", [15, 25])]);
    }

    /// The dev mock and the frontend tests read the same table
    /// (frontend/src/localModels.table.json); this keeps it equal to the
    /// product's. Regenerate with FLY_BLESS_TABLE=1.
    #[test]
    fn the_shared_table_matches_the_product_table() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../frontend/src/localModels.table.json"
        );
        let want = serde_json::to_value(CANDIDATES).unwrap();
        if std::env::var_os("FLY_BLESS_TABLE").is_some() {
            std::fs::write(path, serde_json::to_string_pretty(&want).unwrap() + "\n").unwrap();
            return;
        }
        let got: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("localModels.table.json"))
                .unwrap();
        assert_eq!(
            got, want,
            "frontend/src/localModels.table.json differs from CANDIDATES: FLY_BLESS_TABLE=1 cargo test -p fly-app --lib local_models"
        );
    }
}
