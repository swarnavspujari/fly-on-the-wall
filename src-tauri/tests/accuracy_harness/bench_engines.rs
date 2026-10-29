//! Bench-only ASR engines for the accuracy harness (v2.5 engine choice).
//! NOTHING here ships in the app: these engines run only through
//! `pipeline::run_with_asr_engine`, the harness seam that swaps the ASR step
//! and keeps the shipped prep, loop guard, diarization and alignment.
//!
//! Every runtime and model lives OUTSIDE the repo under one bench folder,
//! `FLYONTHEWALL_HARNESS_BENCH_DIR` (layout in docs/data/asr/README.md,
//! "Bench engines"). A missing runtime/model → SKIP, never a download.
//!
//! Engine names (`FLYONTHEWALL_HARNESS_ENGINES`):
//! - `nemotron-live`     nemotron-speech-streaming-en-0.6b (sherpa-onnx int8),
//!   the whole channel streamed in 1.12 s chunks like a live call; also
//!   reports first-partial latency.
//! - `nemotron-offline`  the same model over the shipped VAD batches.
//! - `nemotron35-live`   nemotron-3.5-asr-streaming-0.6b (OpenMDW successor).
//! - `parakeet-sherpa`   parakeet-tdt-0.6b-v3 int8 via sherpa-onnx (TDT
//!   durations; also the timing anchor for decoders without timestamps).
//! - `cohere`            Cohere Transcribe (sherpa-onnx int8).
//! - `granite`           IBM Granite Speech 5.0 2B GGUF via llama.cpp's
//!   llama-server (LLM decoder, no timestamps: word times transferred from
//!   parakeet-sherpa on the same piece).
//! - `nemocpp-parakeet`  parakeet-tdt-0.6b-v3 q8_0 via NVIDIA NeMo-Speech.cpp
//!   (alternative runtime to the shipped parakeet-cli path).
//! - `nemocpp-parakeet-whole` the same, the whole channel in one request
//!   (NeMo-Speech.cpp's own long-form chunking, not the shipped VAD batches).
//! - `nemocpp-nemotron`  nemotron-speech-streaming-en q8_0 via NeMo-Speech.cpp
//!   at 1.12 s right context (its offline/file path).
//!
//! The server-based ones live in bench_servers.rs.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fly_asr::{AsrError, RawTranscript, TranscribeOptions, TranscriptionEngine};
use fly_core::Word;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::bench_align::{self, TransferStats};

// ---------------------------------------------------------------------------
// Per-run side channel: engine stats the transcript can't carry
// ---------------------------------------------------------------------------

static STATS: Mutex<Option<serde_json::Map<String, serde_json::Value>>> = Mutex::new(None);

/// Merge one stat into the current run's `bench` block: numbers add up,
/// arrays concatenate, anything else is replaced.
pub fn stat(key: &str, value: serde_json::Value) {
    let mut g = STATS.lock().unwrap();
    let map = g.get_or_insert_with(serde_json::Map::new);
    match (map.get_mut(key), value) {
        (Some(serde_json::Value::Number(a)), serde_json::Value::Number(b)) => {
            let sum = a.as_f64().unwrap_or(0.0) + b.as_f64().unwrap_or(0.0);
            *map.get_mut(key).unwrap() = serde_json::json!(sum);
        }
        (Some(serde_json::Value::Array(a)), serde_json::Value::Array(b)) => a.extend(b),
        (_, v) => {
            map.insert(key.to_string(), v);
        }
    }
}

/// Drain the current run's stats (call once per pipeline run).
pub fn take_stats() -> Option<serde_json::Value> {
    STATS.lock().unwrap().take().map(serde_json::Value::Object)
}

// ---------------------------------------------------------------------------
// Bench folder layout
// ---------------------------------------------------------------------------

pub const BENCH_DIR_ENV: &str = "FLYONTHEWALL_HARNESS_BENCH_DIR";

const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

/// Every engine name this module builds.
pub const BENCH_ENGINES: &[&str] = &[
    "nemotron-live",
    "nemotron-offline",
    "nemotron35-live",
    "parakeet-sherpa",
    "cohere",
    "granite",
    "nemocpp-parakeet",
    "nemocpp-parakeet-whole",
    "nemocpp-nemotron",
];

pub fn is_bench_engine(name: &str) -> bool {
    BENCH_ENGINES.contains(&name)
}

pub struct BenchDir {
    pub root: PathBuf,
}

impl BenchDir {
    pub fn from_env() -> Result<Self, String> {
        let root = std::env::var(BENCH_DIR_ENV)
            .map(PathBuf::from)
            .map_err(|_| {
                format!("{BENCH_DIR_ENV} is not set (bench engines live outside the repo)")
            })?;
        if !root.is_dir() {
            return Err(format!(
                "{BENCH_DIR_ENV}={} is not a folder",
                root.display()
            ));
        }
        Ok(Self { root })
    }

    fn need(&self, rel: &str) -> Result<PathBuf, String> {
        let p = self.root.join(rel);
        if p.exists() {
            Ok(p)
        } else {
            Err(format!("bench artifact missing: {}", p.display()))
        }
    }

    /// The bench venv's interpreter (sherpa-onnx ≥ 1.13.4 from PyPI).
    fn python(&self) -> Result<PathBuf, String> {
        self.need(if cfg!(windows) {
            "venv/Scripts/python.exe"
        } else {
            "venv/bin/python"
        })
    }
}

/// The worker script ships in the repo (scripts/asr-bench/).
fn worker_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .join("scripts/asr-bench/sherpa_worker.py")
}

fn bench_threads(default: usize) -> usize {
    std::env::var("FLYONTHEWALL_HARNESS_BENCH_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
        .max(1)
}

/// Build the named bench engine. `gpu` asks for its GPU path where the
/// runtime has one (sherpa-onnx's onnxruntime has no Vulkan provider).
pub fn build(
    name: &str,
    gpu: bool,
    threads: usize,
) -> Result<Box<dyn TranscriptionEngine>, String> {
    let dir = BenchDir::from_env()?;
    let threads = bench_threads(threads);
    let sherpa = |label: &'static str,
                  kind: WorkerKind,
                  model_rel: &str,
                  mode: Mode|
     -> Result<Box<dyn TranscriptionEngine>, String> {
        if gpu {
            return Err(format!(
                "{label}: sherpa-onnx has no Vulkan provider (CPU only in this bench)"
            ));
        }
        // live captions run beside the call: 2 threads by default (the
        // "CPU ≤ 25 % during a call" budget), FLYONTHEWALL_HARNESS_LIVE_THREADS
        let threads = match mode {
            Mode::Stream { .. } => std::env::var("FLYONTHEWALL_HARNESS_LIVE_THREADS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2usize)
                .max(1),
            Mode::Batched { .. } => threads,
        };
        Ok(Box::new(SherpaEngine {
            label,
            python: dir.python()?,
            kind,
            model_dir: dir.need(model_rel)?,
            threads,
            mode,
            anchor: None,
        }))
    };
    match name {
        "nemotron-live" => sherpa(
            "nemotron-live",
            WorkerKind::OnlineTransducer,
            "models/nemotron-streaming-en-1120ms",
            Mode::Stream { chunk_ms: 1120 },
        ),
        "nemotron-offline" => sherpa(
            "nemotron-offline",
            WorkerKind::OnlineTransducer,
            "models/nemotron-streaming-en-1120ms",
            Mode::Batched { max_piece_ms: None },
        ),
        "nemotron35-live" => sherpa(
            "nemotron35-live",
            WorkerKind::OnlineTransducer,
            "models/nemotron35-streaming-1120ms",
            Mode::Stream { chunk_ms: 1120 },
        ),
        "parakeet-sherpa" => sherpa(
            "parakeet-sherpa",
            WorkerKind::OfflineTransducer,
            "models/parakeet-tdt-0.6b-v3-int8",
            Mode::Batched { max_piece_ms: None },
        ),
        "cohere" => {
            let mut e = SherpaEngine {
                label: "cohere",
                python: dir.python()?,
                kind: WorkerKind::Cohere,
                model_dir: dir.need("models/cohere-transcribe-int8")?,
                threads,
                // encoder-decoder trained on ≤ 30 s utterances
                mode: Mode::Batched {
                    max_piece_ms: Some(30_000),
                },
                anchor: None,
            };
            e.anchor = Some(Box::new(SherpaEngine {
                label: "parakeet-sherpa",
                python: dir.python()?,
                kind: WorkerKind::OfflineTransducer,
                model_dir: dir.need("models/parakeet-tdt-0.6b-v3-int8")?,
                threads,
                mode: Mode::Batched { max_piece_ms: None },
                anchor: None,
            }));
            Ok(Box::new(e))
        }
        "nemocpp-parakeet" | "nemocpp-parakeet-whole" | "nemocpp-nemotron" => {
            let device = if gpu { "vulkan" } else { "cpu" };
            let exe = dir.need(&format!("bin/nemo-speech-{device}/bin/nemo-speech{EXE}"))?;
            let (gguf, extra): (&str, &[&str]) = if name.starts_with("nemocpp-parakeet") {
                ("parakeet-tdt-0.6b-v3.q8_0.gguf", &[])
            } else {
                // 1.12 s chunks, the same operating point as nemotron-live.
                // --no-warmup: v0.2.0's warmup trips GGML_ASSERT(ne3 == ne13)
                // in the cache-aware encoder on Vulkan (upstream issue #48);
                // real requests decode fine.
                (
                    "nemotron-speech-streaming-en-0.6b.q8_0.gguf",
                    &["--asr.streaming.rnnt_right_context", "13", "--no-warmup"],
                )
            };
            let model = dir.need(&format!("models/nemo-speech/{gguf}"))?;
            let mut args: Vec<String> = [
                "serve",
                "--no-ui",
                "--port",
                "{port}",
                "--read-timeout",
                "1800",
                "--write-timeout",
                "1800",
                "--device",
                device,
                "--asr.backend.threads",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            args.push(threads.to_string());
            args.push("--asr-model".into());
            args.push(model.to_string_lossy().into_owned());
            args.extend(extra.iter().map(|s| s.to_string()));
            let label: &'static str = match name {
                "nemocpp-parakeet" => "nemocpp-parakeet",
                "nemocpp-parakeet-whole" => "nemocpp-parakeet-whole",
                _ => "nemocpp-nemotron",
            };
            Ok(Box::new(crate::bench_servers::ServerEngine {
                label,
                model_tag: gguf.trim_end_matches(".gguf").to_string(),
                exe,
                args,
                ready_path: "/ready",
                // NeMo-Speech.cpp splits > 400 s itself; batches are ≤ 120 s
                max_piece_ms: None,
                // -whole: the channel in one request, NeMo-Speech's own
                // long-form chunking instead of the shipped VAD batches
                whole_file: name == "nemocpp-parakeet-whole",
                flavor: crate::bench_servers::ServerFlavor::NemoSpeech,
                anchor: None,
            }))
        }
        "granite" => {
            let device = if gpu { "vulkan" } else { "cpu" };
            let exe = dir.need(&format!("bin/llama-{device}/llama-server{EXE}"))?;
            // IBM recommends Q8_0 (lower quants lose accuracy past ~1 min)
            let quant = std::env::var("FLYONTHEWALL_HARNESS_GRANITE_QUANT")
                .unwrap_or_else(|_| "Q8_0".into());
            let gguf = format!("granite-speech-5.0-2b-{quant}.gguf");
            let model = dir.need(&format!("models/granite-speech-5.0-2b/{gguf}"))?;
            let mmproj = dir.need("models/granite-speech-5.0-2b/mmproj-model-f16.gguf")?;
            // IBM: segments of up to ~2 min; 60 s keeps well inside that
            let piece_secs: u64 = std::env::var("FLYONTHEWALL_HARNESS_GRANITE_PIECE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60);
            let mut args: Vec<String> = vec![
                "-m".into(),
                model.to_string_lossy().into_owned(),
                "--mmproj".into(),
                mmproj.to_string_lossy().into_owned(),
                "--port".into(),
                "{port}".into(),
                "--host".into(),
                "127.0.0.1".into(),
                "-c".into(),
                "8192".into(),
                "-t".into(),
                threads.to_string(),
                "--temp".into(),
                "0".into(),
                "--no-webui".into(),
                "-ngl".into(),
                if gpu { "99" } else { "0" }.into(),
            ];
            if !gpu {
                args.push("--no-mmproj-offload".into());
            }
            Ok(Box::new(crate::bench_servers::ServerEngine {
                label: "granite",
                model_tag: gguf.trim_end_matches(".gguf").to_string(),
                exe,
                args,
                ready_path: "/health",
                max_piece_ms: Some(piece_secs * 1000),
                whole_file: false,
                flavor: crate::bench_servers::ServerFlavor::GraniteChat,
                anchor: Some(Box::new(SherpaEngine {
                    label: "parakeet-sherpa",
                    python: dir.python()?,
                    kind: WorkerKind::OfflineTransducer,
                    model_dir: dir.need("models/parakeet-tdt-0.6b-v3-int8")?,
                    threads,
                    mode: Mode::Batched { max_piece_ms: None },
                    anchor: None,
                })),
            }))
        }
        other => Err(format!("unknown bench engine {other}")),
    }
}

// ---------------------------------------------------------------------------
// sherpa-onnx engines (persistent Python worker, see scripts/asr-bench)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum WorkerKind {
    OnlineTransducer,
    OfflineTransducer,
    Cohere,
}

impl WorkerKind {
    fn arg(self) -> &'static str {
        match self {
            WorkerKind::OnlineTransducer => "online-transducer",
            WorkerKind::OfflineTransducer => "offline-transducer",
            WorkerKind::Cohere => "cohere",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Mode {
    /// The whole channel, fed in `chunk_ms` pieces like a live capture.
    Stream { chunk_ms: u64 },
    /// The shipped VAD batches (≤ 120 s speech each); `max_piece_ms` cuts
    /// each batch further at quiet points for short-context models.
    Batched { max_piece_ms: Option<u64> },
}

/// Capture buffer size the live simulation feeds (a mic callback delivers
/// ~100–200 ms at a time); the model's chunk is baked into its export.
const LIVE_FEED_MS: u64 = 160;

/// FastConformer frame (10 ms hop × 8 subsampling): the timestamp grid.
const NEMO_FRAME_MS: u64 = 80;

pub struct SherpaEngine {
    pub label: &'static str,
    pub python: PathBuf,
    pub kind: WorkerKind,
    pub model_dir: PathBuf,
    pub threads: usize,
    pub mode: Mode,
    /// Timestamped engine whose words time this one's (when it has none).
    pub anchor: Option<Box<SherpaEngine>>,
}

/// One persistent worker process: model loaded once, JSON lines both ways.
pub struct Worker {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

impl Worker {
    pub async fn spawn(
        mut cmd: tokio::process::Command,
        what: &str,
    ) -> Result<(Self, f64), AsrError> {
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS, as the shipped sidecars
            cmd.creation_flags(0x0800_0000 | 0x0000_4000);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| AsrError::Engine(format!("failed to launch {what}: {e}")))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
        let mut w = Worker {
            child,
            stdin,
            stdout,
        };
        let ready = w.read_reply(what).await?;
        let load = ready["load_secs"].as_f64().unwrap_or(f64::NAN);
        Ok((w, load))
    }

    async fn read_reply(&mut self, what: &str) -> Result<serde_json::Value, AsrError> {
        loop {
            let line = self
                .stdout
                .next_line()
                .await
                .map_err(|e| AsrError::Engine(format!("{what}: read: {e}")))?
                .ok_or_else(|| AsrError::Engine(format!("{what} exited (see its stderr above)")))?;
            // tolerate library chatter on stdout: replies are JSON objects
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                if v.is_object() {
                    return Ok(v);
                }
            }
        }
    }

    pub async fn request(
        &mut self,
        req: serde_json::Value,
        what: &str,
    ) -> Result<serde_json::Value, AsrError> {
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| AsrError::Engine(format!("{what}: write: {e}")))?;
        self.stdin.flush().await.ok();
        let rep = self.read_reply(what).await?;
        if rep["ok"].as_bool() != Some(true) {
            return Err(AsrError::Engine(format!(
                "{what}: {}",
                rep["error"].as_str().unwrap_or("worker error")
            )));
        }
        Ok(rep)
    }

    pub async fn close(mut self) {
        drop(self.stdin);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), self.child.wait()).await;
    }
}

/// A temp wav for one request; removed on drop.
pub struct TempWav(pub PathBuf);

impl TempWav {
    pub fn write(samples: &[f32], rate: u32) -> Result<Self, AsrError> {
        let f = tempfile::Builder::new()
            .prefix("fotw-bench-")
            .suffix(".wav")
            .tempfile()
            .map_err(AsrError::Io)?;
        let (_, path) = f.keep().map_err(|e| AsrError::Io(e.error))?;
        fly_audio::mix::write_wav_mono_16(&path, samples, rate)
            .map_err(|e| AsrError::Engine(format!("write chunk wav: {e}")))?;
        Ok(TempWav(path))
    }
}

impl Drop for TempWav {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn ms(v: &serde_json::Value) -> Vec<u64> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_f64())
                .map(|s| (s * 1000.0).round().max(0.0) as u64)
                .collect()
        })
        .unwrap_or_default()
}

/// Words from one worker reply (times relative to the request's wav).
/// `None` when the engine returned text but no token timestamps.
pub fn reply_words(rep: &serde_json::Value) -> Option<Vec<Word>> {
    let tokens: Vec<String> = rep["tokens"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let starts = ms(&rep["starts"]);
    if tokens.is_empty() || starts.len() != tokens.len() {
        return None;
    }
    let durations = rep["durations"].as_array().map(|_| ms(&rep["durations"]));
    Some(bench_align::glue_sp_tokens(
        &tokens,
        &starts,
        durations.as_deref().filter(|d| d.len() == tokens.len()),
        NEMO_FRAME_MS,
    ))
}

impl SherpaEngine {
    pub async fn spawn_worker(&self) -> Result<Worker, AsrError> {
        let mut cmd = tokio::process::Command::new(&self.python);
        cmd.arg("-I")
            .arg(worker_script())
            .arg("--kind")
            .arg(self.kind.arg())
            .arg("--model-dir")
            .arg(&self.model_dir)
            .arg("--threads")
            .arg(self.threads.to_string());
        let (w, load) = Worker::spawn(cmd, self.label).await?;
        stat(
            &format!("{}_load_secs", self.label),
            serde_json::json!(load),
        );
        stat(
            &format!("{}_threads", self.label),
            serde_json::json!(self.threads),
        );
        Ok(w)
    }

    /// Decode one prepared chunk; words relative to the chunk.
    async fn decode_piece(
        &self,
        worker: &mut Worker,
        anchor: Option<&mut Worker>,
        samples: &[f32],
        rate: u32,
    ) -> Result<Vec<Word>, AsrError> {
        let wav = TempWav::write(samples, rate)?;
        let rep = worker
            .request(serde_json::json!({"wav": wav.0}), self.label)
            .await?;
        stat(
            &format!("{}_decode_secs", self.label),
            rep["decode_secs"].clone(),
        );
        if let Some(words) = reply_words(&rep) {
            return Ok(words);
        }
        // no timestamps: time the text from the anchor engine's words
        let text = rep["text"].as_str().unwrap_or_default();
        let hyp = bench_align::split_text_words(text);
        let span = (0, samples.len() as u64 * 1000 / rate.max(1) as u64);
        let anchors = match (anchor, &self.anchor) {
            (Some(aw), Some(a)) => {
                let arep = aw
                    .request(serde_json::json!({"wav": wav.0}), a.label)
                    .await?;
                stat("anchor_decode_secs", arep["decode_secs"].clone());
                reply_words(&arep).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let (words, st) = bench_align::transfer_times(&hyp, &anchors, span);
        record_transfer(st, self.anchor.as_ref().map(|a| a.label));
        Ok(words)
    }

    async fn transcribe_stream(
        &self,
        wav_path: &Path,
        chunk_ms: u64,
    ) -> Result<RawTranscript, AsrError> {
        let mut worker = self.spawn_worker().await?;
        let rep = worker
            .request(
                serde_json::json!({"stream": wav_path, "feed_ms": LIVE_FEED_MS}),
                self.label,
            )
            .await?;
        worker.close().await;
        let words = reply_words(&rep).unwrap_or_default();
        stat(
            &format!("{}_decode_secs", self.label),
            rep["decode_secs"].clone(),
        );
        for k in [
            "chunk_compute_p50_s",
            "chunk_compute_p95_s",
            "chunk_compute_max_s",
        ] {
            stat(&format!("live_{k}"), rep[k].clone());
        }
        stat("live_chunk_ms", serde_json::json!(chunk_ms));
        stat("live_feed_ms", serde_json::json!(LIVE_FEED_MS));
        if let Some(emits) = rep["emits"].as_array() {
            let emits: Vec<(f64, f64, f64)> = emits
                .iter()
                .filter_map(|e| {
                    let a = e.as_array()?;
                    Some((
                        a.first()?.as_f64()?,
                        a.get(1)?.as_f64()?,
                        a.get(2)?.as_f64()?,
                    ))
                })
                .collect();
            if let Ok((samples, rate)) = fly_audio::mix::read_wav_mono(wav_path) {
                stat("live_latency", live_latency(&emits, &samples, rate));
            }
        }
        Ok(RawTranscript {
            language: Some("en".into()),
            words: fly_asr::drop_non_speech_spans(words),
            segments: vec![],
        })
    }
}

pub fn record_transfer(st: TransferStats, anchor: Option<&str>) {
    stat(
        "timing",
        serde_json::json!(format!(
            "transfer from {}",
            anchor.unwrap_or("none (spread over the chunk)")
        )),
    );
    stat("transfer_matched", serde_json::json!(st.matched));
    stat("transfer_substituted", serde_json::json!(st.substituted));
    stat("transfer_interpolated", serde_json::json!(st.interpolated));
}

/// Silence before a speech onset for it to count as "starting to talk".
const ONSET_GAP_MS: u64 = 1_500;

/// Live first-partial latency: for every VAD speech onset that follows
/// ≥ 1.5 s of silence, the time from the onset until the first token at or
/// after it appears on screen = audio fed when it was emitted − onset +
/// that chunk's compute time (audio arrives in real time; decoding the
/// chunk then takes `compute`). Also every token's own emission delay.
pub fn live_latency(emits: &[(f64, f64, f64)], samples: &[f32], rate: u32) -> serde_json::Value {
    let spans =
        fly_audio::vad::detect_speech_spans(samples, rate, &fly_audio::vad::VadConfig::default());
    let mut onsets = Vec::new();
    let mut last_end: Option<u64> = None;
    for s in &spans {
        if last_end.is_none_or(|e| s.start_ms.saturating_sub(e) >= ONSET_GAP_MS) {
            onsets.push(s.start_ms as f64 / 1000.0);
        }
        last_end = Some(s.end_ms);
    }
    let mut first_partial: Vec<f64> = Vec::new();
    for (i, &on) in onsets.iter().enumerate() {
        let next_on = onsets.get(i + 1).copied().unwrap_or(f64::INFINITY);
        // first emission whose token starts inside this talk spurt
        if let Some((_, fed, dt)) = emits
            .iter()
            .filter(|(t, _, _)| *t >= on - 0.3 && *t < next_on)
            .min_by(|a, b| a.1.total_cmp(&b.1))
        {
            first_partial.push((fed - on).max(0.0) + dt);
        }
    }
    let token_delay: Vec<f64> = emits
        .iter()
        .map(|(t, fed, dt)| (fed - t).max(0.0) + dt)
        .collect();
    let q = |v: &[f64], p: f64| -> Option<f64> {
        if v.is_empty() {
            return None;
        }
        let mut s = v.to_vec();
        s.sort_by(f64::total_cmp);
        Some(s[((s.len() - 1) as f64 * p).round() as usize])
    };
    serde_json::json!({
        "onsets": onsets.len(),
        "onsets_with_text": first_partial.len(),
        "first_partial_p50_s": q(&first_partial, 0.5),
        "first_partial_p90_s": q(&first_partial, 0.9),
        "first_partial_max_s": q(&first_partial, 1.0),
        "token_delay_p50_s": q(&token_delay, 0.5),
        "token_delay_p90_s": q(&token_delay, 0.9),
    })
}

#[async_trait::async_trait]
impl TranscriptionEngine for SherpaEngine {
    fn id(&self) -> &'static str {
        self.label
    }

    fn is_local(&self) -> bool {
        true
    }

    fn model_tag(&self) -> Option<String> {
        self.model_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    }

    async fn transcribe(
        &self,
        wav_path: &Path,
        opts: &TranscribeOptions,
    ) -> fly_asr::Result<RawTranscript> {
        self.transcribe_with_progress(wav_path, opts, &|_| {}, &|| false)
            .await
    }

    async fn transcribe_with_progress(
        &self,
        wav_path: &Path,
        opts: &TranscribeOptions,
        on_progress: fly_asr::TranscribeProgressFn<'_>,
        cancel: fly_asr::CancelFn<'_>,
    ) -> fly_asr::Result<RawTranscript> {
        let max_piece = match self.mode {
            Mode::Stream { chunk_ms } => return self.transcribe_stream(wav_path, chunk_ms).await,
            Mode::Batched { max_piece_ms } => max_piece_ms,
        };
        let worker = tokio::sync::Mutex::new(self.spawn_worker().await?);
        let anchor = match &self.anchor {
            Some(a) => Some(tokio::sync::Mutex::new(a.spawn_worker().await?)),
            None => None,
        };
        let raw = fly_asr::batch::transcribe_batched(
            self.label,
            &self.model_dir,
            wav_path,
            opts,
            false,
            Some(fly_asr::batch::MAX_BATCH_SPEECH_MS),
            on_progress,
            cancel,
            |chunk, rate| {
                let worker = &worker;
                let anchor = &anchor;
                async move {
                    let mut w = worker.lock().await;
                    let mut a = match anchor {
                        Some(m) => Some(m.lock().await),
                        None => None,
                    };
                    let mut words = Vec::new();
                    for (from, to) in pieces(&chunk, rate, max_piece) {
                        let off = from as u64 * 1000 / rate as u64;
                        let got = self
                            .decode_piece(&mut w, a.as_deref_mut(), &chunk[from..to], rate)
                            .await?;
                        words.extend(got.into_iter().map(|mut x| {
                            x.start_ms += off;
                            x.end_ms += off;
                            x
                        }));
                    }
                    Ok(RawTranscript {
                        language: Some("en".into()),
                        words,
                        segments: vec![],
                    })
                }
            },
        )
        .await?;
        worker.into_inner().close().await;
        if let Some(a) = anchor {
            a.into_inner().close().await;
        }
        Ok(RawTranscript {
            words: fly_asr::drop_non_speech_spans(raw.words),
            ..raw
        })
    }
}

/// Sample ranges of `chunk` no longer than `max_ms`, cut at quiet points
/// (fly-asr's own splitter); the whole chunk when there is no cap.
pub fn pieces(chunk: &[f32], rate: u32, max_ms: Option<u64>) -> Vec<(usize, usize)> {
    let len_ms = chunk.len() as u64 * 1000 / rate.max(1) as u64;
    let Some(max_ms) = max_ms.filter(|m| len_ms > *m) else {
        return vec![(0, chunk.len())];
    };
    let whole = fly_audio::vad::SpeechSpan {
        start_ms: 0,
        end_ms: len_ms,
    };
    let per_ms = rate as usize / 1000;
    fly_asr::batch::split_long_spans(&[whole], chunk, rate, max_ms)
        .into_iter()
        .map(|s| {
            (
                (s.start_ms as usize * per_ms).min(chunk.len()),
                (s.end_ms as usize * per_ms).min(chunk.len()),
            )
        })
        .filter(|(a, b)| b > a)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_respect_the_cap_and_tile_the_chunk() {
        let rate = 16_000;
        let chunk = vec![0.3f32; 70 * rate as usize];
        let p = pieces(&chunk, rate, Some(30_000));
        assert!(p.len() >= 3);
        assert_eq!(p.first().unwrap().0, 0);
        assert_eq!(p.last().unwrap().1, chunk.len());
        assert!(p.windows(2).all(|w| w[0].1 == w[1].0));
        assert!(p.iter().all(|(a, b)| (b - a) <= 30 * rate as usize));
        assert_eq!(pieces(&chunk, rate, None), vec![(0, chunk.len())]);
    }

    #[test]
    fn live_latency_measures_onset_to_first_text() {
        // 1 s silence, 2 s "speech", 3 s silence, 2 s speech
        let rate = 16_000u32;
        let mut s = vec![0.0f32; 8 * rate as usize];
        for x in &mut s[rate as usize..3 * rate as usize] {
            *x = 0.5;
        }
        for x in &mut s[6 * rate as usize..8 * rate as usize] {
            *x = 0.5;
        }
        // tokens at 1.1 s (seen when 2.24 s had been fed) and 6.2 s (7.84 s)
        let emits = [(1.1, 2.24, 0.1), (1.5, 2.24, 0.1), (6.2, 7.84, 0.2)];
        let v = live_latency(&emits, &s, rate);
        assert_eq!(v["onsets"], 2);
        // second spurt: onset 6.0 s → first text at 7.84 s + 0.2 s compute
        // (the VAD may pad the onset a little earlier)
        let worst = v["first_partial_max_s"].as_f64().unwrap();
        assert!((2.0..2.5).contains(&worst), "{v}");
    }
}
