//! Contention benchmark (spec v2.5 row 3.3b, §4.2a #3): does a recording
//! lose audio while the app does heavy background work?
//!
//! It records with the REAL capture backend (`fly_audio::cpal_backend`) for
//! `FOTW_BENCH_SECS` while a quiet marker tone (`fly_audio::testtone`, 1 kHz,
//! about -30 dBFS, level stepping once a second) plays through the default
//! output, and — all at once —
//! - `embed`: a full re-embed: batches of 16 chunks through the app's
//!   `embeddings::embed_raw` against the local Ollama, back to back;
//! - `asr`: a queued transcription: the real `pipeline::run_with`
//!   (whisper.cpp + VAD + sherpa diarization, CPU) over a ~4 min meeting,
//!   restarted whenever it finishes (so diarization runs during the
//!   recording too, and a cancel at the end lands within one meeting);
//! - `ask`: an Ask every 30 s through the shipped prompt
//!   (`llm_commands::build_ask_system`) and the Ollama provider;
//! - `disk`: ~50 MB/s of file writes with periodic fsync on the same volume;
//! - live captions over the recording, through the app's own
//!   `live::LiveCaptioner`.
//!
//! Then it checks the captured system channel against the tone (gaps longer
//! than 10 ms, padded or removed), the mic's sample count against the
//! recording's duration (drift), Ask latency (p50/p95) and caption lag.
//! Report-only: it never fails on the numbers; it writes them as JSON.
//!
//! **Local only, never in CI:** it plays sound for the whole run, needs
//! Ollama with `embeddinggemma:300m` and `llama3.1`, and downloads
//! whisper/sherpa artifacts into its own scratch data dir on first run (the
//! real `%APPDATA%\FlyOnTheWall` is never read or written).
//!
//! ```text
//! cargo test -p fly-app --release --test contention_bench -- --ignored --nocapture
//! ```
//! Environment:
//! - `FOTW_BENCH_SECS`   recording length, default 600
//! - `FOTW_BENCH_LOAD`   comma list of embed,asr,ask,disk (default all; `none`)
//! - `FOTW_BENCH_DATA`   scratch data dir, default `~/fotw-bench` (keep it
//!   short: sherpa breaks on long Windows paths)
//! - `FOTW_BENCH_OUT`    results JSON path, default
//!   `<data>/results/contention-<UTC time>.json` (outside the repo)
//! - `FOTW_BENCH_LABEL`  free text stored in the results (e.g. "before")
//! - `FOTW_BENCH_ASK_EVERY_SECS` default 30
//! - `FOTW_BENCH_DISK_MBPS`      disk-load write rate, default 50
//! - `FOTW_BENCH_DISK_MAX_GB`    disk-load cap per run, default 30 (the load
//!   stops writing there; 50 MB/s for 10 min is 30 GB)
//! - `FOTW_BENCH_INDUCE_GAP_AT_SECS` (Windows) freeze the bench process
//!   for `FOTW_BENCH_INDUCE_GAP_MS` (default 1000) at that point of the
//!   recording — the tone keeps playing from a separate player process, so
//!   the capture really loses audio and must report it
//! - `FOTW_BENCH_KEEP_AUDIO` keep the recording under `<data>/rec/`
//!
//! Disk hygiene (the disk is shared with builds): the disk load writes one
//! 512 MB file at a time, opened delete-on-close on Windows, so the OS removes
//! it even if the run is killed. Everything else the run writes — the
//! recording, queued-meeting copies, caption chunks, the player's files — is
//! removed when the run ends (panics included) and again at the next start in
//! case a run was killed. Kept on purpose: the downloaded engines/models and
//! the small scratch database under `<data>/app`, and the results JSON.
//!
//! The evals runner's `local_bench` can call this test and read the JSON.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fly_app_lib::live::{ChunkOutcome, LiveCaptioner};
use fly_app_lib::state::AppState;
use fly_audio::testtone::{self, TonePlayer, ToneSpec};
use fly_audio::{AudioCapture, CaptureConfig};
use fly_core::{RecordingRef, Transcript};
use fly_llm::{ChatMessage, ChatRequest, LLMProvider, ThinkingMode};
use serde_json::{json, Value};

const OLLAMA: &str = "http://127.0.0.1:11434";
const ASK_MODEL: &str = "llama3.1";
const LIVE_MODEL: &str = "ggml-small-q5_1";
const LIVE_TICK: Duration = Duration::from_millis(5_000);
/// Gaps at or under this are below what a listener notices; the spec target
/// counts the longer ones.
const GAP_REPORT_MS: f64 = 10.0;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    Some(sorted[idx.min(sorted.len() - 1)])
}

fn stats(mut v: Vec<f64>) -> Value {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    json!({
        "count": v.len(),
        "p50_ms": percentile(&v, 50.0),
        "p95_ms": percentile(&v, 95.0),
        "max_ms": v.last().copied(),
    })
}

fn machine() -> Value {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
    json!({
        "cpu": sys.cpus().first().map(|c| c.brand().trim().to_string()),
        "logical_cpus": sys.cpus().len(),
        "ram_gb": (sys.total_memory() as f64 / 1024f64.powi(3) * 10.0).round() / 10.0,
        "os": sysinfo::System::long_os_version(),
    })
}

fn git_sha() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// A ~4 min two-voice meeting for the ASR load: the committed TTS fixture
/// repeated. Built once into the scratch dir.
fn long_meeting_wav(dir: &Path) -> (PathBuf, u64) {
    let out = dir.join("queued-meeting.wav");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/meeting-fixture.wav");
    let (one, rate) = fly_audio::mix::read_wav_mono(&fixture).expect("fixture wav");
    let reps = 9;
    let duration_ms = one.len() as u64 * reps * 1000 / rate as u64;
    if !out.exists() {
        let mut all = Vec::with_capacity(one.len() * reps as usize);
        for _ in 0..reps {
            all.extend_from_slice(&one);
        }
        fly_audio::mix::write_wav_mono_16(&out, &all, rate).expect("write long meeting");
    }
    (out, duration_ms)
}

/// A fresh meeting whose mixed track is (a hard link to) `wav` — a new
/// meeting each run, so nothing is resumed from an earlier run's checkpoint.
fn queue_meeting(state: &AppState, wav: &Path, duration_ms: u64, link: bool) -> String {
    let storage = state.storage.lock().unwrap();
    let note = storage.create_note("Bench queued meeting", None).unwrap();
    let meeting = storage
        .create_meeting("Bench queued meeting", &note.id, &[])
        .unwrap();
    let rel = format!("recordings/bench-{}", meeting.id);
    let dir = state.data_dir.join(&rel);
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("recording.mixed.wav");
    if !link || std::fs::hard_link(wav, &target).is_err() {
        std::fs::copy(wav, &target).unwrap();
    }
    storage
        .end_meeting(
            &meeting.id,
            &RecordingRef {
                mic_path: None,
                system_path: None,
                mixed_path: Some(format!("{rel}/recording.mixed.wav")),
                playback_path: None,
                duration_ms,
            },
        )
        .unwrap();
    meeting.id
}

/// The tone source: in this process, or — when the bench freezes itself
/// to induce a gap — a child process (this test binary, `tone_player_child`).
enum Player {
    InProcess(TonePlayer),
    Child {
        child: std::process::Child,
        stop_file: PathBuf,
        stats_file: PathBuf,
    },
}

impl Player {
    fn start(tone: ToneSpec, separate: bool, dir: &Path) -> Self {
        if !separate {
            return Player::InProcess(TonePlayer::start(tone).expect("tone player"));
        }
        let stop_file = dir.join("player.stop");
        let stats_file = dir.join("player.json");
        let _ = std::fs::remove_file(&stop_file);
        let _ = std::fs::remove_file(&stats_file);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["tone_player_child", "--ignored", "--exact", "--nocapture"])
            .env("FOTW_BENCH_PLAYER_STOP", &stop_file)
            .env("FOTW_BENCH_PLAYER_STATS", &stats_file)
            .spawn()
            .expect("tone player process");
        Player::Child {
            child,
            stop_file,
            stats_file,
        }
    }

    fn finish(self) -> Value {
        match self {
            Player::InProcess(p) => json!({
                "process": "bench",
                "rate": p.rate(),
                "frames": p.frames_played(),
                "max_callback_gap_ms": p.max_callback_gap_ms(),
            }),
            Player::Child {
                mut child,
                stop_file,
                stats_file,
            } => {
                let _ = std::fs::write(&stop_file, b"stop");
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_secs(15) {
                    if let Ok(Some(_)) = child.try_wait() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = child.kill();
                std::fs::read_to_string(&stats_file)
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or(Value::Null)
            }
        }
    }
}

/// Freeze every thread of this process (capture callbacks and the writer
/// included) for `ms`, from a helper process. Windows only.
fn freeze_self(ms: u64) {
    if !cfg!(target_os = "windows") {
        eprintln!("FOTW_BENCH_INDUCE_GAP_AT_SECS is Windows-only; ignored");
        return;
    }
    let script = format!(
        "Add-Type -Namespace Fotw -Name Nt -MemberDefinition '\
         [DllImport(\"ntdll.dll\")] public static extern int NtSuspendProcess(IntPtr h); \
         [DllImport(\"ntdll.dll\")] public static extern int NtResumeProcess(IntPtr h);'; \
         $p = Get-Process -Id {pid}; [Fotw.Nt]::NtSuspendProcess($p.Handle) | Out-Null; \
         Start-Sleep -Milliseconds {ms}; [Fotw.Nt]::NtResumeProcess($p.Handle) | Out-Null",
        pid = std::process::id()
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        eprintln!("freezing the bench process failed: {status:?}");
    }
}

/// A file for the disk load: on Windows FILE_FLAG_DELETE_ON_CLOSE, so the
/// OS deletes it when the handle closes — also when the process is killed.
fn open_scratch_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const DELETE: u32 = 0x0001_0000;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        o.access_mode(GENERIC_WRITE | DELETE)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
    }
    o.open(path)
}

/// Stop processes this bench started that are still running. Sidecars are
/// spawned without kill-on-drop, so dropping their task leaves them behind.
fn stop_own_children() {
    let Ok(me) = sysinfo::get_current_pid() else {
        return;
    };
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    for (pid, proc_) in sys.processes() {
        if proc_.parent() == Some(me) {
            eprintln!(
                "stopping leftover child {} ({pid})",
                proc_.name().to_string_lossy()
            );
            proc_.kill();
        }
    }
}

/// Everything a run writes besides the model cache, the scratch database and
/// the results: the disk load, caption chunks, the player's files, the
/// queued-meeting copies and (unless kept) the recordings.
fn remove_scratch(root: &Path, data_dir: &Path, recordings: bool) {
    for sub in ["disk-load", "live-tmp", "player.stop", "player.json"] {
        let p = root.join(sub);
        let _ = std::fs::remove_dir_all(&p);
        let _ = std::fs::remove_file(&p);
    }
    if recordings {
        let _ = std::fs::remove_dir_all(root.join("rec"));
    }
    if let Ok(entries) = std::fs::read_dir(data_dir.join("recordings")) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("bench-") {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

/// Runs [`remove_scratch`] when the bench ends, however it ends.
struct ScratchGuard {
    root: PathBuf,
    data_dir: PathBuf,
    recordings: bool,
}

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        remove_scratch(&self.root, &self.data_dir, self.recordings);
    }
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    embed_batches: AtomicU64,
    embed_errors: AtomicU64,
    asr_runs: AtomicU64,
    asr_errors: Mutex<Vec<String>>,
    ask_ms: Mutex<Vec<f64>>,
    ask_errors: Mutex<Vec<String>>,
    ask_overruns: AtomicU64,
    caption_lag_ms: Mutex<Vec<f64>>,
    caption_failures: AtomicU64,
    caption_ticks: AtomicU64,
    caption_tick_max_ms: AtomicU64,
    disk_mb: AtomicU64,
    cpu_samples: Mutex<Vec<f64>>,
}

#[test]
#[ignore = "local contention bench: plays a tone for ~10 min and loads the machine; run with --ignored"]
fn contention_bench() {
    let secs: u64 = env_or("FOTW_BENCH_SECS", "600")
        .parse()
        .expect("FOTW_BENCH_SECS");
    let loads_raw = env_or("FOTW_BENCH_LOAD", "embed,asr,ask,disk");
    let loads: Vec<String> = loads_raw
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && s != "none")
        .collect();
    let want = |l: &str| loads.iter().any(|x| x == l);
    let ask_every: u64 = env_or("FOTW_BENCH_ASK_EVERY_SECS", "30").parse().unwrap();
    let disk_mbps: u64 = env_or("FOTW_BENCH_DISK_MBPS", "50").parse().unwrap();
    let disk_max_mb: u64 = env_or("FOTW_BENCH_DISK_MAX_GB", "30")
        .parse::<u64>()
        .unwrap()
        * 1024;
    let induce_at: Option<u64> = std::env::var("FOTW_BENCH_INDUCE_GAP_AT_SECS")
        .ok()
        .and_then(|v| v.parse().ok());
    let induce_ms: u64 = env_or("FOTW_BENCH_INDUCE_GAP_MS", "1000").parse().unwrap();
    let home = dirs::home_dir().expect("home dir");
    let bench_root = PathBuf::from(env_or(
        "FOTW_BENCH_DATA",
        &home.join("fotw-bench").to_string_lossy(),
    ));
    let data_dir = bench_root.join("app");
    // whatever a killed earlier run left behind
    let keep_audio = std::env::var("FOTW_BENCH_KEEP_AUDIO").is_ok();
    remove_scratch(&bench_root, &data_dir, !keep_audio);
    std::fs::create_dir_all(&data_dir).unwrap();
    // ...and whatever this one writes, however it ends (Drop runs on panic)
    let _cleanup = ScratchGuard {
        root: bench_root.clone(),
        data_dir: data_dir.clone(),
        recordings: !keep_audio,
    };
    let started_utc = chrono::Utc::now();
    let out_path = PathBuf::from(env_or(
        "FOTW_BENCH_OUT",
        &bench_root
            .join("results")
            .join(format!(
                "contention-{}.json",
                started_utc.format("%Y%m%dT%H%M%SZ")
            ))
            .to_string_lossy(),
    ));
    eprintln!(
        "contention bench: {secs}s, loads [{}], data {}",
        loads.join(","),
        bench_root.display()
    );

    let state = Arc::new(
        AppState::init_with(
            data_dir.clone(),
            Arc::new(fly_secrets::MemorySecretStore::default()),
        )
        .expect("scratch app state"),
    );
    {
        let storage = state.storage.lock().unwrap();
        // CPU transcription: the worst case for contention
        storage.set_setting("asr.tier", "light").unwrap();
        storage.set_setting("asr.model_id", LIVE_MODEL).unwrap();
        storage.set_setting("asr.use_gpu", "false").unwrap();
        storage.set_setting("asr.use_groq", "false").unwrap();
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let on_model = |p: fly_app_lib::models::ModelProgress| {
        if p.stage != "downloading" {
            eprintln!("model: {} {}", p.id, p.stage)
        }
    };

    // ---------------------------------------------------------- warm-up
    let (whisper_exe, live_model) = rt.block_on(async {
        let exe = fly_app_lib::models::ensure_tool_with(
            &on_model,
            &data_dir,
            fly_app_lib::models::WHISPER_ENGINE_ID,
            fly_app_lib::models::WHISPER_CLI_NAMES,
            "the bench needs whisper.cpp",
            fly_app_lib::models::DownloadEffort::Full,
        )
        .await
        .expect("whisper.cpp");
        let model = fly_app_lib::models::ensure_with(
            &on_model,
            &data_dir,
            LIVE_MODEL,
            fly_app_lib::models::DownloadEffort::Full,
        )
        .await
        .expect("live model");
        (exe, model)
    });
    let (long_wav, long_ms) = long_meeting_wav(&bench_root);
    let mut setup_errors: Vec<String> = Vec::new();
    if want("asr") {
        // one short run downloads sherpa + the diarization models up front
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/meeting-fixture.wav");
        let id = queue_meeting(&state, &fixture, 27_540, false);
        let on_stage = |_: fly_app_lib::pipeline::PipelineProgress| {};
        if let Err(e) = rt.block_on(fly_app_lib::pipeline::run_with(
            &state, &on_stage, &on_model, &id,
        )) {
            setup_errors.push(format!("asr warm-up: {e}"));
        }
    }
    if want("embed") || want("ask") {
        let ok = rt.block_on(fly_app_lib::embeddings::embed_raw(
            OLLAMA,
            &["warm-up".to_string()],
        ));
        if let Err(e) = ok {
            setup_errors.push(format!("ollama embed warm-up: {e}"));
        }
    }
    for e in &setup_errors {
        eprintln!("SETUP: {e}");
    }

    let shared = Arc::new(Shared::default());

    // ---------------------------------------------------------- loads
    let mut tasks = Vec::new();
    if want("embed") {
        let shared = shared.clone();
        let state = state.clone();
        tasks.push(rt.spawn(async move {
            let filler = "We walked through the quarterly plan, the hiring pipeline, the \
                          vendor renewal and the open risks on the migration; each owner \
                          gave a date and the group agreed to revisit the budget next week. ";
            let mut n: u64 = 0;
            while !shared.stop.load(Ordering::Relaxed) {
                let inputs: Vec<String> = (0..16)
                    .map(|i| {
                        n += 1;
                        fly_app_lib::embeddings::doc_prompt(
                            &format!("Bench meeting {}", n / 40),
                            &format!("chunk {n} {i}: {}", filler.repeat(6)),
                        )
                    })
                    .collect();
                match fly_app_lib::embeddings::embed_raw(OLLAMA, &inputs).await {
                    Ok(v) => {
                        shared.embed_batches.fetch_add(1, Ordering::Relaxed);
                        // the worker commits each batch to SQLite
                        let bytes: usize = v.iter().map(|e| e.len() * 4).sum();
                        let _ = state
                            .storage
                            .lock()
                            .unwrap()
                            .set_setting("bench.embed_progress", &format!("{n}:{bytes}"));
                    }
                    Err(_) => {
                        shared.embed_errors.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }));
    }
    let asr_meeting: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    if want("asr") {
        let shared = shared.clone();
        let state = state.clone();
        let current = asr_meeting.clone();
        let wav = long_wav.clone();
        let data_dir = data_dir.clone();
        tasks.push(rt.spawn(async move {
            let on_stage = |_: fly_app_lib::pipeline::PipelineProgress| {};
            let on_model = |_: fly_app_lib::models::ModelProgress| {};
            while !shared.stop.load(Ordering::Relaxed) {
                let id = queue_meeting(&state, &wav, long_ms, true);
                *current.lock().unwrap() = Some(id.clone());
                let res = fly_app_lib::pipeline::run_with(&state, &on_stage, &on_model, &id).await;
                match res {
                    Ok(_) => {
                        shared.asr_runs.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) if e.contains(fly_asr::CANCELLED_MARKER) => {}
                    Err(e) => {
                        shared.asr_errors.lock().unwrap().push(e);
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
                // keep the scratch dir from growing run over run
                let _ = std::fs::remove_dir_all(data_dir.join(format!("recordings/bench-{id}")));
            }
        }));
    }
    if want("ask") {
        let shared = shared.clone();
        let state = state.clone();
        tasks.push(rt.spawn(async move {
            let fixture =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/bench/budget.transcript.json");
            let transcript: Transcript =
                serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
            let note = {
                let storage = state.storage.lock().unwrap();
                let note = storage.create_note("Budget review", None).unwrap();
                storage
                    .update_note_scratchpad(&note.id, "- forecast due Friday\n- marketing ask")
                    .unwrap()
            };
            let provider =
                fly_llm::openai_compat::OpenAiCompatProvider::ollama(None, ASK_MODEL.to_string());
            let profile = fly_core::prompt_profile::profile_for(ASK_MODEL);
            let system =
                fly_app_lib::llm_commands::build_ask_system(&note, Some(&transcript), profile);
            let questions = [
                "What did we decide about the marketing budget?",
                "Who owns the updated forecast, and when is it due?",
                "Summarize the open risks in three bullets.",
                "Was anything left unresolved?",
            ];
            let mut tick = tokio::time::interval(Duration::from_secs(ask_every));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut i = 0usize;
            loop {
                tick.tick().await;
                if shared.stop.load(Ordering::Relaxed) {
                    break;
                }
                let t0 = Instant::now();
                let res = provider
                    .chat(ChatRequest {
                        effort: profile.effort.ask.map(String::from),
                        messages: vec![
                            ChatMessage::system(system.clone()),
                            ChatMessage::user(questions[i % questions.len()].to_string()),
                        ],
                        temperature: Some(0.3),
                        max_tokens: Some(profile.max_tokens.ask.unwrap_or(2048)),
                        thinking: if profile.thinking_disabled() {
                            ThinkingMode::Disabled
                        } else {
                            ThinkingMode::Default
                        },
                        format: None,
                    })
                    .await;
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                if ms > (ask_every * 1000) as f64 {
                    shared.ask_overruns.fetch_add(1, Ordering::Relaxed);
                }
                match res {
                    Ok(_) => shared.ask_ms.lock().unwrap().push(ms),
                    Err(e) => shared.ask_errors.lock().unwrap().push(e.to_string()),
                }
                i += 1;
            }
        }));
    }
    let mut threads = Vec::new();
    if want("disk") {
        let shared = shared.clone();
        let disk_mbps = disk_mbps as f64;
        let dir = bench_root.join("disk-load");
        threads.push(std::thread::spawn(move || {
            use std::io::Write;
            let _ = std::fs::create_dir_all(&dir);
            let buf = vec![0x5au8; 8 << 20];
            let target_mb_s = disk_mbps;
            let t0 = Instant::now();
            let capped = |shared: &Shared| shared.disk_mb.load(Ordering::Relaxed) >= disk_max_mb;
            while !shared.stop.load(Ordering::Relaxed) && !capped(&shared) {
                // one 512 MB file at a time, gone as soon as it's closed
                let Ok(mut f) = open_scratch_file(&dir.join("load.bin")) else {
                    break;
                };
                for k in 0..64 {
                    if shared.stop.load(Ordering::Relaxed) || capped(&shared) {
                        break;
                    }
                    if f.write_all(&buf).is_err() {
                        break;
                    }
                    let mb = shared.disk_mb.fetch_add(8, Ordering::Relaxed) + 8;
                    if k % 8 == 7 {
                        let _ = f.sync_all();
                    }
                    let ahead = mb as f64 / target_mb_s - t0.elapsed().as_secs_f64();
                    if ahead > 0.0 {
                        std::thread::sleep(Duration::from_secs_f64(ahead));
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
        }));
    }
    {
        let shared = shared.clone();
        threads.push(std::thread::spawn(move || {
            let mut sys = sysinfo::System::new();
            sys.refresh_cpu_usage();
            while !shared.stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(2));
                sys.refresh_cpu_usage();
                shared
                    .cpu_samples
                    .lock()
                    .unwrap()
                    .push(sys.global_cpu_usage() as f64);
            }
        }));
    }
    // let the loads ramp up before recording starts
    std::thread::sleep(Duration::from_secs(if loads.is_empty() { 1 } else { 20 }));

    // ---------------------------------------------------------- record
    let tone = ToneSpec::default();
    // an induced gap freezes this process: the tone must keep playing
    let player = Player::start(tone, induce_at.is_some(), &bench_root);
    std::thread::sleep(Duration::from_secs(2));
    let capture = fly_audio::cpal_backend::CpalAudioCapture::new();
    let capture_warnings_at_start = capture.capture_warnings();
    let rec_dir = bench_root
        .join("rec")
        .join(started_utc.format("%Y%m%dT%H%M%SZ").to_string());
    let rec_started = Instant::now();
    let session = capture
        .start(CaptureConfig {
            mic_device_id: None,
            capture_system: true,
            out_dir: rec_dir.clone(),
            base_name: "recording".into(),
        })
        .expect("capture start");

    let live_task = {
        let shared = shared.clone();
        let engine = fly_app_lib::live::live_engine(whisper_exe.clone(), live_model.clone());
        let mut captioner = LiveCaptioner::new(&rec_dir, bench_root.join("live-tmp"));
        rt.spawn(async move {
            let never = AtomicBool::new(false);
            while !shared.stop.load(Ordering::Relaxed) {
                tokio::time::sleep(LIVE_TICK).await;
                let t0 = Instant::now();
                let outcomes = captioner.tick(&engine, &never).await;
                shared.caption_ticks.fetch_add(1, Ordering::Relaxed);
                shared
                    .caption_tick_max_ms
                    .fetch_max(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
                for outcome in outcomes {
                    match outcome {
                        ChunkOutcome::Text { end_ms, .. } => {
                            let now_ms = rec_started.elapsed().as_secs_f64() * 1000.0;
                            shared
                                .caption_lag_ms
                                .lock()
                                .unwrap()
                                .push(now_ms - end_ms as f64);
                        }
                        ChunkOutcome::Failed(_) => {
                            shared.caption_failures.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        })
    };

    let mut warnings_seen: Vec<String> = Vec::new();
    let deadline = rec_started + Duration::from_secs(secs);
    let mut last_report = Instant::now();
    let mut induced = false;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1000));
        if let Some(at) = induce_at {
            if !induced && rec_started.elapsed() >= Duration::from_secs(at) {
                induced = true;
                eprintln!("[{at:>5}s] freezing this process for {induce_ms} ms");
                freeze_self(induce_ms);
            }
        }
        for w in session.warnings() {
            if !warnings_seen.contains(&w) {
                eprintln!(
                    "[{:>5.0}s] warning: {w}",
                    rec_started.elapsed().as_secs_f64()
                );
                warnings_seen.push(w);
            }
        }
        if last_report.elapsed() > Duration::from_secs(60) {
            last_report = Instant::now();
            eprintln!(
                "[{:>5.0}s] recording… embed batches {}, asr runs {}, asks {}, disk {} MB, caption ticks {} chunks {}",
                rec_started.elapsed().as_secs_f64(),
                shared.embed_batches.load(Ordering::Relaxed),
                shared.asr_runs.load(Ordering::Relaxed),
                shared.ask_ms.lock().unwrap().len(),
                shared.disk_mb.load(Ordering::Relaxed),
                shared.caption_ticks.load(Ordering::Relaxed),
                shared.caption_lag_ms.lock().unwrap().len(),
            );
        }
    }
    let output = session.stop().expect("capture stop");
    let player = player.finish();

    // ---------------------------------------------------------- teardown
    shared.stop.store(true, Ordering::Relaxed);
    if let Some(id) = asr_meeting.lock().unwrap().clone() {
        state.cancel_requests.lock().unwrap().insert(id);
    }
    // Sidecars aren't killed when their future is dropped, so let every load
    // finish its current unit (the pipeline honours the cancel between
    // batches; a ~4 min meeting bounds that) — never leave a whisper running
    // into the next measurement — then shut the runtime down without waiting
    // on anything stuck.
    rt.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(120), live_task).await;
        for t in tasks {
            let _ = tokio::time::timeout(Duration::from_secs(420), t).await;
        }
    });
    rt.shutdown_timeout(Duration::from_secs(5));
    // a sidecar still running (a live-caption whisper starved past the wait
    // above) would outlive the run, load the next one and hold its output
    stop_own_children();
    for t in threads {
        let _ = t.join();
    }

    // ---------------------------------------------------------- analyse
    let system = output.system_path.as_ref().map(|p| {
        let (f, rate) = fly_audio::mix::read_wav_mono(p).expect("system wav");
        // read_wav_mono scales by i16::MAX: this recovers the exact samples
        let samples: Vec<i16> = f
            .iter()
            .map(|v| (v * i16::MAX as f32).round() as i16)
            .collect();
        (samples, rate)
    });
    let (tone_report, system_json) = match &system {
        Some((samples, rate)) => {
            let rep = testtone::analyze(samples, *rate, &tone);
            let lost_ms: f64 = rep.gaps.iter().map(|g| g.ms).sum();
            // silence: a gap filled with zeros (a recorder loss, or the
            // engine stalling); removed: audio missing with no fill;
            // inserted: extra non-silent audio — the render side repeating
            // a period, which the speakers played too
            let by_kind: serde_json::Map<String, Value> = ["silence", "removed", "inserted"]
                .iter()
                .map(|k| {
                    let n = rep
                        .gaps
                        .iter()
                        .filter(|g| g.kind == *k && g.ms > GAP_REPORT_MS)
                        .count();
                    (k.to_string(), json!(n))
                })
                .collect();
            let j = json!({
                "rate": rate,
                "frames": samples.len(),
                "expected_frames": output.duration_ms * *rate as u64 / 1000,
                "tone": &rep,
                "gaps_over_10ms": rep.gaps_longer_than(GAP_REPORT_MS),
                "gaps_over_10ms_by_kind": by_kind,
                "gap_ms_total": lost_ms,
            });
            (Some(rep), j)
        }
        None => (None, Value::Null),
    };
    let mic_json = output.mic_path.as_ref().map(|p| {
        let (f, rate) = fly_audio::mix::read_wav_mono(p).expect("mic wav");
        let frames = f.len() as u64;
        let expected = output.duration_ms * rate as u64 / 1000;
        let drift_pct = (frames as f64 - expected as f64) / expected.max(1) as f64 * 100.0;
        json!({
            "rate": rate,
            "frames": frames,
            "expected_frames": expected,
            "drift_frames": frames as i64 - expected as i64,
            "drift_pct": drift_pct,
        })
    });
    let gaps_over = tone_report
        .as_ref()
        .map(|r| r.gaps_longer_than(GAP_REPORT_MS));
    let drift_pct = mic_json.as_ref().and_then(|m| m["drift_pct"].as_f64());
    let cpu = shared.cpu_samples.lock().unwrap().clone();
    let cpu_avg = if cpu.is_empty() {
        None
    } else {
        Some(cpu.iter().sum::<f64>() / cpu.len() as f64)
    };

    let results = json!({
        "bench": "contention",
        "schema": 1,
        "label": std::env::var("FOTW_BENCH_LABEL").ok(),
        "git_sha": git_sha(),
        "started_at": started_utc.to_rfc3339(),
        "machine": machine(),
        "config": {
            "secs": secs,
            "loads": loads,
            "ask_every_secs": ask_every,
            "disk_mbps": disk_mbps,
            "disk_max_gb": disk_max_mb / 1024,
            "tone": tone,
            "asr": {"tier": "light", "model": LIVE_MODEL, "gpu": false,
                    "threads": fly_app_lib::pipeline::sidecar_threads()},
        },
        "setup_errors": setup_errors,
        "capture": {
            "duration_ms": output.duration_ms,
            "warnings_at_start": capture_warnings_at_start,
            // the capture's own accounting (absent before it existed)
            "report": serde_json::to_value(&output).ok().and_then(|v| v.get("health").cloned()),
            "session_warnings": warnings_seen,
            "mic": mic_json,
            "system": system_json,
        },
        "player": player,
        "induced_gap": induce_at.map(|at| json!({"at_secs": at, "ms": induce_ms})),
        "ask": {
            "latency": stats(shared.ask_ms.lock().unwrap().clone()),
            "errors": shared.ask_errors.lock().unwrap().clone(),
            "overruns": shared.ask_overruns.load(Ordering::Relaxed),
        },
        "captions": {
            "lag": stats(shared.caption_lag_ms.lock().unwrap().clone()),
            "failures": shared.caption_failures.load(Ordering::Relaxed),
            "ticks": shared.caption_ticks.load(Ordering::Relaxed),
            "slowest_tick_ms": shared.caption_tick_max_ms.load(Ordering::Relaxed),
        },
        "load": {
            "embed_batches": shared.embed_batches.load(Ordering::Relaxed),
            "embed_errors": shared.embed_errors.load(Ordering::Relaxed),
            "asr_runs_completed": shared.asr_runs.load(Ordering::Relaxed),
            "asr_errors": shared.asr_errors.lock().unwrap().clone(),
            "disk_mb_written": shared.disk_mb.load(Ordering::Relaxed),
            "cpu_avg_pct": cpu_avg,
            "cpu_max_pct": cpu.iter().cloned().fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x)))),
        },
        "targets": {
            "gaps_over_10ms": {"target": 0, "actual": gaps_over,
                               "pass": gaps_over.map(|g| g == 0)},
            "mic_drift_pct": {"target_abs_below": 0.1, "actual": drift_pct,
                              "pass": drift_pct.map(|d| d.abs() < 0.1)},
        },
    });
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&out_path, serde_json::to_string_pretty(&results).unwrap()).unwrap();
    eprintln!(
        "\n{}",
        serde_json::to_string_pretty(&results["targets"]).unwrap()
    );
    eprintln!(
        "ask {} · captions {} · load {}",
        results["ask"]["latency"], results["captions"]["lag"], results["load"]
    );
    eprintln!("results: {}", out_path.display());
    // the recording itself (kept only on request) and every other scratch
    // file go when `_cleanup` drops, right after this
}

/// Re-analyse a kept system-channel WAV (`FOTW_BENCH_KEEP_AUDIO=1` keeps
/// them under `<data>/rec/`): `FOTW_BENCH_WAV=<path>`.
#[test]
#[ignore = "offline helper for the contention bench"]
fn analyze_kept_recording() {
    let path = std::env::var("FOTW_BENCH_WAV").expect("set FOTW_BENCH_WAV");
    let (f, rate) = fly_audio::mix::read_wav_mono(Path::new(&path)).expect("wav");
    let samples: Vec<i16> = f
        .iter()
        .map(|v| (v * i16::MAX as f32).round() as i16)
        .collect();
    let rep = testtone::analyze(&samples, rate, &ToneSpec::default());
    eprintln!("{}", serde_json::to_string_pretty(&rep).unwrap());
}

/// The tone source for an induced-gap run (spawned by the bench; a no-op
/// when run by hand).
#[test]
#[ignore = "child process of contention_bench"]
fn tone_player_child() {
    let (Ok(stop), Ok(stats)) = (
        std::env::var("FOTW_BENCH_PLAYER_STOP"),
        std::env::var("FOTW_BENCH_PLAYER_STATS"),
    ) else {
        return;
    };
    let player = TonePlayer::start(ToneSpec::default()).expect("tone player");
    while !Path::new(&stop).exists() {
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = json!({
        "process": "child",
        "rate": player.rate(),
        "frames": player.frames_played(),
        "max_callback_gap_ms": player.max_callback_gap_ms(),
    });
    let _ = std::fs::write(stats, out.to_string());
}
