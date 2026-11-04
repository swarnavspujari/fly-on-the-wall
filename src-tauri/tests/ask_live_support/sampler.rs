//! Machine and Ollama resource sampling for the Ask bench: CPU, RAM, the
//! Ollama processes' memory, VRAM (`nvidia-smi`) and where the loaded model
//! sits (`/api/ps`: how much of it is on the GPU).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sysinfo::{ProcessesToUpdate, System};

/// Processes whose CPU is the build's, not the model's: a sign another
/// session was compiling while a number was taken.
const BUILD_PROCS: &[&str] = &[
    "rustc", "cargo", "link.exe", "lld", "cl.exe", "cc1", "cmake",
];

pub struct Sampler {
    stop: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<Value>,
}

#[derive(Default)]
struct Acc {
    n: u32,
    machine_cpu: f64,
    ollama_cpu: f64,
    build_samples: u32,
    peak_ram_used: u64,
    ram_total: u64,
    peak_ollama_rss: u64,
    vram_first: Option<u64>,
    vram_peak: u64,
    vram_total: u64,
    gpu_util_sum: f64,
    gpu_n: u32,
    ps: Vec<(u64, u64, u64)>,
}

fn nvidia_smi() -> Option<(u64, u64, f64)> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.used,memory.total,utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text.lines().next()?.split(',').map(str::trim);
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

/// `(size, size_vram, context_length)` of the first loaded model.
async fn ollama_ps(client: &reqwest::Client, upstream: &str) -> Option<(u64, u64, u64)> {
    let v: Value = client
        .get(format!("{upstream}/api/ps"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let m = v["models"].as_array()?.first()?;
    Some((
        m["size"].as_u64()?,
        m["size_vram"].as_u64().unwrap_or(0),
        m["context_length"].as_u64().unwrap_or(0),
    ))
}

impl Sampler {
    /// Sample every 500 ms until [`Sampler::finish`].
    pub fn start(upstream: &str) -> Sampler {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let upstream = upstream.to_string();
        let task = tokio::spawn(async move {
            let client = reqwest::Client::new();
            let mut sys = System::new();
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            sys.refresh_processes(ProcessesToUpdate::All, true);
            let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
            let mut acc = Acc::default();
            let mut tick = 0u32;
            while !flag.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(500)).await;
                tick += 1;
                sys.refresh_cpu_usage();
                sys.refresh_memory();
                sys.refresh_processes(ProcessesToUpdate::All, true);
                acc.n += 1;
                acc.machine_cpu += f64::from(sys.global_cpu_usage());
                acc.ram_total = sys.total_memory();
                acc.peak_ram_used = acc.peak_ram_used.max(sys.used_memory());
                let (mut rss, mut cpu, mut building) = (0u64, 0f64, false);
                for p in sys.processes().values() {
                    let name = p.name().to_string_lossy().to_lowercase();
                    if name.contains("ollama") || name.contains("llama-server") {
                        rss += p.memory();
                        cpu += f64::from(p.cpu_usage());
                    }
                    building |= BUILD_PROCS.iter().any(|b| name.starts_with(b));
                }
                acc.peak_ollama_rss = acc.peak_ollama_rss.max(rss);
                acc.ollama_cpu += cpu / cores;
                acc.build_samples += u32::from(building);
                if tick.is_multiple_of(2) {
                    if let Ok(Some((used, total, util))) =
                        tokio::task::spawn_blocking(nvidia_smi).await
                    {
                        acc.vram_first.get_or_insert(used);
                        acc.vram_peak = acc.vram_peak.max(used);
                        acc.vram_total = total;
                        acc.gpu_util_sum += util;
                        acc.gpu_n += 1;
                    }
                }
                if tick.is_multiple_of(4) {
                    if let Some(ps) = ollama_ps(&client, &upstream).await {
                        acc.ps.push(ps);
                    }
                }
            }
            let n = f64::from(acc.n.max(1));
            let machine = acc.machine_cpu / n;
            let ollama = acc.ollama_cpu / n;
            let mb = |b: u64| (b as f64 / 1_048_576.0).round();
            let gb = |b: u64| (b as f64 / 1_073_741_824.0 * 10.0).round() / 10.0;
            let gpu_frac = |(size, vram, _): &(u64, u64, u64)| *vram as f64 / (*size).max(1) as f64;
            let min_frac = acc.ps.iter().map(gpu_frac).fold(f64::NAN, f64::min);
            let last = acc.ps.last().copied();
            json!({
                "samples": acc.n,
                "machine_cpu_avg_pct": (machine * 10.0).round() / 10.0,
                "ollama_cpu_avg_pct": (ollama * 10.0).round() / 10.0,
                "other_cpu_avg_pct": ((machine - ollama).max(0.0) * 10.0).round() / 10.0,
                "build_processes_seen_pct": (f64::from(acc.build_samples) / n * 1000.0).round() / 10.0,
                "peak_ram_used_gb": gb(acc.peak_ram_used),
                "ram_total_gb": gb(acc.ram_total),
                "peak_ollama_rss_mb": mb(acc.peak_ollama_rss),
                "vram_baseline_used_mb": acc.vram_first,
                "vram_peak_used_mb": (acc.gpu_n > 0).then_some(acc.vram_peak),
                "vram_total_mb": (acc.gpu_n > 0).then_some(acc.vram_total),
                "gpu_util_avg_pct": (acc.gpu_n > 0).then(|| (acc.gpu_util_sum / f64::from(acc.gpu_n) * 10.0).round() / 10.0),
                "model_size_mb": last.map(|l| mb(l.0)),
                "model_size_vram_mb": last.map(|l| mb(l.1)),
                "model_gpu_fraction_min": (!min_frac.is_nan()).then(|| (min_frac * 1000.0).round() / 1000.0),
                "model_fully_on_gpu": (!acc.ps.is_empty()).then(|| acc.ps.iter().all(|p| gpu_frac(p) >= 0.999)),
                "context_length_loaded": acc.ps.iter().map(|p| p.2).max(),
            })
        });
        Sampler { stop, task }
    }

    pub async fn finish(self) -> Value {
        self.stop.store(true, Ordering::Relaxed);
        self.task.await.unwrap_or(Value::Null)
    }
}
