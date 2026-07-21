//! GPU transcription selection. The CPU whisper build stays the shipped,
//! validated default; a machine moves post-meeting ASR to the pinned Vulkan
//! build (Windows) only when `asr.use_gpu` is on (default) AND a one-time
//! benchmark on this machine measured the GPU faster than the CPU on real
//! speech from the recording being transcribed. The verdict is persisted per
//! (machine, model); any GPU failure — during the benchmark or mid-pipeline —
//! flips it back to CPU and surfaces on `pipeline:progress`.
//!
//! macOS needs none of this machinery: standard whisper.cpp builds (incl.
//! brew's) default to Metal already, so there the setting only gates a
//! `-ng` (force CPU) flag — see pipeline.rs.

use serde::{Deserialize, Serialize};

/// Setting: GPU transcription opt-out. Default ON ("false" disables); the
/// benchmark gate below decides whether ON actually uses the GPU.
pub const USE_GPU_KEY: &str = "asr.use_gpu";
/// Setting: JSON-serialized [`GpuBench`] verdict of this machine.
pub const BENCH_KEY: &str = "asr.gpu_bench";
/// Registry id of the pinned Vulkan whisper build (models.rs, Windows only).
pub const VULKAN_TOOL_ID: &str = "whisper-bin-vulkan";

/// Persisted per-machine GPU-vs-CPU verdict. Invalidated by a model change
/// (different model, different speed profile) or by toggling the setting
/// off→on in Settings (the user's "try again" gesture).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuBench {
    /// "gpu" or "cpu" — which engine post-meeting transcription uses.
    pub verdict: String,
    /// "benchmark" | "gpu-error: …" | "runtime-failure: …"
    pub reason: String,
    pub gpu_secs: Option<f64>,
    pub cpu_secs: Option<f64>,
    /// Model the measurement ran with.
    pub model_id: String,
}

pub fn enabled(storage: &fly_storage::Storage) -> bool {
    storage
        .get_setting(USE_GPU_KEY)
        .ok()
        .flatten()
        .map(|v| v != "false")
        .unwrap_or(true)
}

pub fn stored(storage: &fly_storage::Storage) -> Option<GpuBench> {
    storage
        .get_setting(BENCH_KEY)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str(&json).ok())
}

pub fn store(storage: &fly_storage::Storage, bench: &GpuBench) {
    match serde_json::to_string(bench) {
        Ok(json) => {
            if let Err(e) = storage.set_setting(BENCH_KEY, &json) {
                tracing::warn!(error = %e, "could not persist GPU benchmark verdict");
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not serialize GPU benchmark verdict"),
    }
}

/// A GPU engine failed mid-pipeline (after winning the benchmark). Pin this
/// machine back to CPU so the failure isn't retried every meeting; toggling
/// the Settings switch off→on clears the verdict for another attempt.
pub fn record_runtime_failure(storage: &fly_storage::Storage, model_id: &str, error: &str) {
    let mut reason = format!("runtime-failure: {error}");
    reason.truncate(300);
    store(
        storage,
        &GpuBench {
            verdict: "cpu".into(),
            reason,
            gpu_secs: None,
            cpu_secs: None,
            model_id: model_id.into(),
        },
    );
}

/// Whether this Mac's hardware is Apple Silicon, asked of the kernel at
/// runtime (`hw.optional.arm64`), not the compiler: the shipped binary is
/// universal, so `cfg!(target_arch)` reports whichever slice happens to be
/// executing (an x86_64 slice under Rosetta still runs on an arm64 machine,
/// and vice versa never holds a GPU truth). Intel-era Macs don't have the
/// key at all — sysctl fails there, and any failure is treated as Intel
/// because CPU is the engine that works everywhere (the Intel smoke test
/// showed Metal on AMD GPUs silently corrupting output, not crashing).
#[cfg(target_os = "macos")]
pub fn is_apple_silicon() -> bool {
    static IS_ARM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS_ARM.get_or_init(|| {
        extern "C" {
            fn sysctlbyname(
                name: *const std::os::raw::c_char,
                oldp: *mut std::os::raw::c_void,
                oldlenp: *mut usize,
                newp: *mut std::os::raw::c_void,
                newlen: usize,
            ) -> std::os::raw::c_int;
        }
        let mut val: i32 = 0;
        let mut len = std::mem::size_of::<i32>();
        let rc = unsafe {
            sysctlbyname(
                c"hw.optional.arm64".as_ptr(),
                &mut val as *mut i32 as *mut _,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        let arm = rc == 0 && val == 1;
        tracing::info!(apple_silicon = arm, "runtime CPU architecture detected");
        arm
    })
}

#[cfg(target_os = "windows")]
pub use windows::{plan, PlanRequest};

#[cfg(target_os = "windows")]
mod windows {
    use std::path::{Path, PathBuf};

    use fly_asr::{TranscribeOptions, TranscriptionEngine};

    use super::GpuBench;
    use crate::models;
    use crate::state::AppState;

    /// Speech the benchmark sample aims for. Long enough that decode time
    /// dominates the GPU's one-time startup (shader/model upload), short
    /// enough to stay a "one-time ~a minute or two" cost on slow CPUs.
    const BENCH_SPEECH_TARGET_MS: u64 = 60_000;
    /// Below this much detected speech the sample proves nothing — skip
    /// benchmarking (stay on CPU, no verdict) and try on a later meeting.
    const BENCH_MIN_SPEECH_MS: u64 = 10_000;

    pub struct PlanRequest<'a> {
        pub cpu_exe: &'a Path,
        pub model_path: &'a Path,
        pub model_id: &'a str,
        pub threads: usize,
        pub opts: &'a TranscribeOptions,
        /// Original recording wav to cut the benchmark sample from.
        pub sample_src: &'a Path,
    }

    /// Decide whether this transcription should run on the Vulkan build.
    /// Returns its exe path only when a valid verdict says the GPU is faster
    /// here. Every "no" is silent-but-logged CPU; a fresh benchmark narrates
    /// itself through `notify` (one-line details on `pipeline:progress`).
    pub async fn plan(
        state: &AppState,
        on_model: models::ProgressSink<'_>,
        notify: &(dyn Fn(String) + Send + Sync),
        req: PlanRequest<'_>,
    ) -> Option<PathBuf> {
        let bench = {
            let storage = state.storage.lock().unwrap();
            super::stored(&storage)
        };
        if let Some(b) = bench.filter(|b| b.model_id == req.model_id) {
            if b.verdict != "gpu" {
                tracing::debug!(reason = %b.reason, "GPU verdict: staying on CPU");
                return None;
            }
            // verdict says GPU — resolve the pinned build (no PATH fallback:
            // a PATH `whisper-cli` is the CPU one)
            return match models::ensure(on_model, &state.data_dir, super::VULKAN_TOOL_ID).await {
                Ok(exe) => Some(exe),
                Err(e) => {
                    tracing::warn!(error = %e, "Vulkan whisper build unavailable — using CPU");
                    None
                }
            };
        }

        // No (valid) verdict — benchmark once on this machine.
        let gpu_exe = match models::ensure(on_model, &state.data_dir, super::VULKAN_TOOL_ID).await {
            Ok(exe) => exe,
            Err(e) => {
                // Likely offline; don't persist a verdict for a transient
                // condition — CPU now, benchmark on a later meeting.
                tracing::warn!(error = %e, "Vulkan whisper build unavailable — using CPU");
                return None;
            }
        };

        let sample = match build_bench_sample(req.sample_src) {
            Ok(Some(s)) => s,
            Ok(None) => {
                tracing::info!("not enough speech for a GPU benchmark — CPU this time");
                return None;
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not build GPU benchmark sample — using CPU");
                return None;
            }
        };
        notify(format!(
            "one-time GPU speed test on this machine ({} s of speech)…",
            sample.speech_ms / 1000
        ));

        let time_run = |exe: PathBuf| {
            let engine = fly_asr::whisper_cpp::WhisperCppEngine {
                exe,
                model: req.model_path.to_path_buf(),
                threads: req.threads,
                force_cpu: false,
                // a timing sample must never resume (or write) checkpoints
                resume: false,
            };
            let wav = sample.wav.clone();
            let opts = req.opts.clone();
            async move {
                let started = std::time::Instant::now();
                engine
                    .transcribe(&wav, &opts)
                    .await
                    .map(|_| started.elapsed().as_secs_f64())
            }
        };

        let gpu_secs = match time_run(gpu_exe.clone()).await {
            Ok(secs) => secs,
            Err(e) => {
                let verdict = GpuBench {
                    verdict: "cpu".into(),
                    reason: format!("gpu-error: {e}"),
                    gpu_secs: None,
                    cpu_secs: None,
                    model_id: req.model_id.into(),
                };
                let storage = state.storage.lock().unwrap();
                super::store(&storage, &verdict);
                notify("GPU test failed — staying on CPU (details in logs)".into());
                tracing::warn!(error = %e, "GPU benchmark run failed — pinned to CPU");
                sample.cleanup();
                return None;
            }
        };
        let cpu_secs = match time_run(req.cpu_exe.to_path_buf()).await {
            Ok(secs) => secs,
            Err(e) => {
                // The validated CPU path failing here is not a GPU verdict —
                // persist nothing and let the pipeline proceed (and report)
                // on the default CPU engine.
                tracing::warn!(error = %e, "CPU benchmark run failed — no verdict stored");
                sample.cleanup();
                return None;
            }
        };
        sample.cleanup();

        let gpu_wins = gpu_secs < cpu_secs;
        let verdict = GpuBench {
            verdict: if gpu_wins { "gpu" } else { "cpu" }.into(),
            reason: "benchmark".into(),
            gpu_secs: Some(gpu_secs),
            cpu_secs: Some(cpu_secs),
            model_id: req.model_id.into(),
        };
        {
            let storage = state.storage.lock().unwrap();
            super::store(&storage, &verdict);
        }
        notify(format!(
            "GPU {gpu_secs:.0} s vs CPU {cpu_secs:.0} s — {} from now on",
            if gpu_wins {
                "transcribing on the GPU"
            } else {
                "GPU is slower on this machine, staying on CPU"
            }
        ));
        tracing::info!(gpu_secs, cpu_secs, gpu_wins, "GPU benchmark verdict stored");
        gpu_wins.then_some(gpu_exe)
    }

    struct BenchSample {
        wav: PathBuf,
        speech_ms: u64,
    }

    impl BenchSample {
        fn cleanup(&self) {
            let _ = std::fs::remove_file(&self.wav);
        }
    }

    /// Cut a speech-only 16 kHz sample from the recording: VAD the audio and
    /// stitch detected speech up to the target. Ok(None) = too little speech.
    fn build_bench_sample(src: &Path) -> Result<Option<BenchSample>, String> {
        use fly_audio::vad::{detect_speech_spans, stitch_spans, VadConfig};

        let (samples, rate) = fly_audio::mix::read_wav_mono(src).map_err(|e| e.to_string())?;
        let samples = fly_audio::mix::resample_linear(&samples, rate, 16_000);
        let spans = detect_speech_spans(&samples, 16_000, &VadConfig::default());

        let mut take = Vec::new();
        let mut speech_ms = 0u64;
        for span in spans {
            take.push(span);
            speech_ms += span.end_ms.saturating_sub(span.start_ms);
            if speech_ms >= BENCH_SPEECH_TARGET_MS {
                break;
            }
        }
        if speech_ms < BENCH_MIN_SPEECH_MS {
            return Ok(None);
        }
        let (chunk, _map) = stitch_spans(&samples, 16_000, &take).map_err(|e| e.to_string())?;
        // one benchmark at a time per app instance — pid is unique enough
        let wav =
            std::env::temp_dir().join(format!("flyonthewall-gpu-bench-{}.wav", std::process::id()));
        fly_audio::mix::write_wav_mono_16(&wav, &chunk, 16_000).map_err(|e| e.to_string())?;
        Ok(Some(BenchSample { wav, speech_ms }))
    }
}
