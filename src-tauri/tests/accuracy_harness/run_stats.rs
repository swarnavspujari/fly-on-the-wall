//! Run-level measurements for the accuracy harness that the transcript
//! itself can't tell you: speed (ASR wall time vs audio), peak memory of
//! the decode sidecars, machine load during the run (so a contended,
//! untrustworthy timing is visible in the results), how many hallucination
//! loops the engine produced BEFORE the pipeline's loop guard removed them,
//! and reference-free structure metrics for fixtures with no word-level
//! reference.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fly_core::Transcript;

// ---------------------------------------------------------------------------
// Peak RSS + machine CPU sampler
// ---------------------------------------------------------------------------

/// Sidecar families whose peak resident memory is reported. Only
/// descendants of this test process count, so another session's
/// whisper-cli on the same machine never pollutes the number.
/// First matching prefix wins, so the diarizer's sidecar is listed before
/// the bench build's sherpa-onnx ASR binaries.
const FAMILIES: &[(&str, &str)] = &[
    ("whisper-cli", "whisper"),
    ("parakeet-cli", "parakeet"),
    ("sherpa-onnx-offline-speaker-diarization", "sherpa"),
    // bench-only engines (bench_engines.rs)
    ("sherpa-onnx", "sherpa-asr"),
    ("python", "sherpa-asr"),
    ("llama-", "llama"),
    ("nemo", "nemo-cpp"),
];

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ResourceStats {
    /// Peak RSS per sidecar family, MB.
    pub peak_rss_mb: std::collections::BTreeMap<String, f64>,
    /// Peak RSS of the harness process itself (in-process VBx/ort), MB.
    pub peak_rss_self_mb: f64,
    /// CPU time per sidecar family, seconds (summed over every process of
    /// the family; sampled, so a process's last < 250 ms can be missed).
    pub cpu_secs: std::collections::BTreeMap<String, f64>,
    /// Logical CPUs on this machine (to turn CPU-seconds into a share).
    pub logical_cpus: usize,
    /// Mean whole-machine CPU % over the run — high values mean other work
    /// competed for the cores and timings are pessimistic.
    pub machine_cpu_avg_pct: f64,
    pub samples: usize,
}

pub struct Sampler {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<ResourceStats>,
}

impl Sampler {
    pub fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            use sysinfo::{ProcessesToUpdate, System};
            let me = sysinfo::get_current_pid().ok();
            let mut sys = System::new();
            let mut stats = ResourceStats::default();
            let mut cpu_sum = 0.0f64;
            // pid → (family, accumulated CPU ms at its latest sample)
            let mut cpu_ms: std::collections::HashMap<sysinfo::Pid, (&str, u64)> =
                std::collections::HashMap::new();
            sys.refresh_cpu_usage();
            stats.logical_cpus = sys.cpus().len();
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(250));
                sys.refresh_cpu_usage();
                sys.refresh_processes(ProcessesToUpdate::All, true);
                cpu_sum += sys.global_cpu_usage() as f64;
                stats.samples += 1;
                for (pid, p) in sys.processes() {
                    let mb = p.memory() as f64 / 1_048_576.0;
                    if Some(*pid) == me {
                        stats.peak_rss_self_mb = stats.peak_rss_self_mb.max(mb);
                        continue;
                    }
                    // children and grandchildren (a venv's python.exe on
                    // Windows is a launcher that spawns the real interpreter)
                    let ours = me.is_some() && {
                        let mut at = p.parent();
                        let mut hops = 0;
                        while at.is_some() && at != me && hops < 3 {
                            at = at.and_then(|q| sys.process(q)).and_then(|q| q.parent());
                            hops += 1;
                        }
                        at.is_some() && at == me
                    };
                    if !ours {
                        continue;
                    }
                    let name = p.name().to_string_lossy().to_ascii_lowercase();
                    if let Some((_, fam)) = FAMILIES.iter().find(|(pre, _)| name.starts_with(pre)) {
                        let e = stats.peak_rss_mb.entry(fam.to_string()).or_default();
                        *e = e.max(mb);
                        let c = cpu_ms.entry(*pid).or_insert((fam, 0));
                        c.1 = c.1.max(p.accumulated_cpu_time());
                    }
                }
            }
            stats.machine_cpu_avg_pct = cpu_sum / stats.samples.max(1) as f64;
            for (fam, ms) in cpu_ms.into_values() {
                *stats.cpu_secs.entry(fam.to_string()).or_default() += ms as f64 / 1000.0;
            }
            stats
        });
        Self { stop, handle }
    }

    pub fn finish(self) -> ResourceStats {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Stage clock: ASR wall time = first "transcribing" → first "diarizing"
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct StageClock {
    inner: Mutex<(Option<Instant>, Option<Instant>)>,
}

impl StageClock {
    pub fn observe(&self, stage: &str) {
        let mut g = self.inner.lock().unwrap();
        match stage {
            "transcribing" if g.0.is_none() => g.0 = Some(Instant::now()),
            "diarizing" if g.0.is_some() && g.1.is_none() => g.1 = Some(Instant::now()),
            _ => {}
        }
    }

    /// Seconds spent in ASR (both channels on a two-track recording).
    pub fn asr_secs(&self) -> Option<f64> {
        let g = self.inner.lock().unwrap();
        Some((g.1? - g.0?).as_secs_f64())
    }
}

// ---------------------------------------------------------------------------
// Hallucination-loop counter (tracing layer)
// ---------------------------------------------------------------------------

/// The pipeline's loop guard (`guard_loops`) logs every repetition run it
/// collapses; counting those events measures loops the ENGINE produced,
/// which the final transcript no longer shows.
pub const LOOP_EVENT: &str = "collapsed ASR repetition loop";

#[derive(Debug, Clone, serde::Serialize)]
pub struct LoopEvent {
    pub channel: String,
    pub reps: u64,
    pub phrase_words: u64,
}

static LOOPS: Mutex<Vec<LoopEvent>> = Mutex::new(Vec::new());

pub struct LoopCounter;

#[derive(Default)]
struct LoopVisitor {
    message: Option<String>,
    channel: Option<String>,
    reps: u64,
    phrase_words: u64,
}

impl tracing::field::Visit for LoopVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = Some(format!("{value:?}")),
            "channel" => self.channel = Some(format!("{value:?}").trim_matches('"').to_string()),
            _ => {}
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.message = Some(value.to_string()),
            "channel" => self.channel = Some(value.to_string()),
            _ => {}
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "reps" => self.reps = value,
            "phrase_words" => self.phrase_words = value,
            _ => {}
        }
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.record_u64(field, value.max(0) as u64);
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LoopCounter {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        let mut v = LoopVisitor::default();
        event.record(&mut v);
        if v.message.as_deref() == Some(LOOP_EVENT) {
            LOOPS.lock().unwrap().push(LoopEvent {
                channel: v.channel.unwrap_or_default(),
                reps: v.reps,
                phrase_words: v.phrase_words,
            });
        }
    }
}

/// Install the harness subscriber: human logs (RUST_LOG or the default
/// filter) plus the loop counter. Idempotent.
pub fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;
    let fmt_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "fly_app_lib=debug,fly_asr=debug".into());
    let loop_filter =
        tracing_subscriber::filter::Targets::new().with_target("fly_app_lib", tracing::Level::WARN);
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(fmt_filter))
        .with(LoopCounter.with_filter(loop_filter))
        .try_init();
}

/// Drain the loops counted since the last call (one pipeline run).
pub fn take_loops() -> Vec<LoopEvent> {
    std::mem::take(&mut *LOOPS.lock().unwrap())
}

pub fn loops_json(loops: &[LoopEvent]) -> serde_json::Value {
    let words: u64 = loops.iter().map(|l| l.reps * l.phrase_words.max(1)).sum();
    serde_json::json!({
        "count": loops.len(),
        "max_reps": loops.iter().map(|l| l.reps).max().unwrap_or(0),
        "words_collapsed": words,
        "by_channel": {
            "mic": loops.iter().filter(|l| l.channel == "mic").count(),
            "system": loops.iter().filter(|l| l.channel == "system").count(),
            "mixed": loops.iter().filter(|l| l.channel == "mixed").count(),
        },
    })
}

// ---------------------------------------------------------------------------
// Reference-free structure metrics
// ---------------------------------------------------------------------------

/// Cross-channel duplicate rate with no reference: share of final mic
/// tokens whose same token also starts on a non-mic channel within
/// `window_ms`, counted only inside runs of ≥ 2 consecutive twinned mic
/// tokens (the echo signature; isolated "yeah" overlaps are coincidence).
/// Longest pause inside one echo run (same scale as the dedup window).
const RUN_GAP_MS: u64 = 1_500;

pub fn reffree_dup_rate(t: &Transcript, window_ms: u64) -> Option<f64> {
    let toks = |mic: bool| {
        let mut v: Vec<(String, u64)> = t
            .segments
            .iter()
            .filter(|s| (s.speaker_key == "mic") == mic)
            .flat_map(|s| {
                s.words
                    .iter()
                    .map(|w| (fly_core::repeat::loop_token(&w.text), w.start_ms))
            })
            .filter(|(tok, _)| !tok.is_empty())
            .collect();
        v.sort_by_key(|(_, at)| *at);
        v
    };
    let mic = toks(true);
    let sys = toks(false);
    if mic.is_empty() || sys.is_empty() {
        return None;
    }
    let twinned: Vec<bool> = mic
        .iter()
        .map(|(tok, at)| {
            let lo = sys.partition_point(|(_, s)| *s + window_ms < *at);
            sys[lo..]
                .iter()
                .take_while(|(_, s)| *s <= at + window_ms)
                .any(|(st, _)| st == tok)
        })
        .collect();
    // a run is consecutive twinned mic tokens with no long pause between them
    let joins = |j: usize| twinned[j] && mic[j].1.saturating_sub(mic[j - 1].1) <= RUN_GAP_MS;
    let mut dup = 0usize;
    let mut i = 0;
    while i < twinned.len() {
        if twinned[i] {
            let j = (i + 1..twinned.len())
                .find(|&j| !joins(j))
                .unwrap_or(twinned.len());
            if j - i >= 2 {
                dup += j - i;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    Some(dup as f64 / mic.len() as f64)
}

/// For a fixture whose far-end speaker count is known (a 1:1: one), the
/// share of far-end transcript time NOT on the top-N diarized clusters —
/// phantom clusters plus "Unknown". 0 = every far-end second went to the
/// right number of speakers. Two-channel recordings only.
pub fn far_end_minority_share(t: &Transcript, expected: usize) -> Option<f64> {
    let mut secs: std::collections::BTreeMap<&str, f64> = Default::default();
    for s in t.segments.iter().filter(|s| s.speaker_key != "mic") {
        *secs.entry(s.speaker_key.as_str()).or_default() +=
            s.end_ms.saturating_sub(s.start_ms) as f64 / 1000.0;
    }
    let total: f64 = secs.values().sum();
    if total <= 0.0 || !t.segments.iter().any(|s| s.speaker_key == "mic") {
        return None;
    }
    let mut clusters: Vec<f64> = secs
        .iter()
        .filter(|(k, _)| **k != "spk_unknown")
        .map(|(_, v)| *v)
        .collect();
    clusters.sort_by(|a, b| b.total_cmp(a));
    let top: f64 = clusters.iter().take(expected).sum();
    Some((total - top) / total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_core::{TranscriptSegment, Word};

    fn seg(key: &str, words: &[(&str, u64)]) -> TranscriptSegment {
        TranscriptSegment {
            id: format!("{key}-{}", words.first().map(|w| w.1).unwrap_or(0)),
            speaker_key: key.into(),
            start_ms: words.first().map(|w| w.1).unwrap_or(0),
            end_ms: words.last().map(|w| w.1 + 300).unwrap_or(0),
            text: words.iter().map(|w| w.0).collect::<Vec<_>>().join(" "),
            words: words
                .iter()
                .map(|(t, at)| Word {
                    text: (*t).into(),
                    start_ms: *at,
                    end_ms: at + 300,
                })
                .collect(),
        }
    }

    fn transcript(segments: Vec<TranscriptSegment>) -> Transcript {
        Transcript {
            meeting_id: "m".into(),
            language: Some("en".into()),
            engine: "test".into(),
            segments,
            speakers: vec![],
        }
    }

    #[test]
    fn dup_rate_counts_twinned_runs_only() {
        let t = transcript(vec![
            seg(
                "mic",
                &[("so", 0), ("the", 400), ("budget", 800), ("yeah", 5_000)],
            ),
            seg("spk_0", &[("the", 450), ("budget", 900), ("yeah", 5_100)]),
        ]);
        // "the budget" is a 2-run → 2 of 4 mic tokens; lone "yeah" ignored
        assert_eq!(reffree_dup_rate(&t, 500), Some(0.5));
    }

    #[test]
    fn minority_share_counts_phantoms_and_unknown() {
        let t = transcript(vec![
            seg("mic", &[("hi", 0)]),
            seg("spk_0", &[("a", 0), ("b", 7_700)]), // 8 s
            seg("spk_3", &[("c", 10_000), ("d", 10_700)]), // 1 s
            seg("spk_unknown", &[("e", 20_000), ("f", 20_700)]), // 1 s
        ]);
        let share = far_end_minority_share(&t, 1).unwrap();
        assert!((share - 0.2).abs() < 1e-9, "{share}");
    }

    #[test]
    fn stage_clock_measures_transcribe_to_diarize() {
        let c = StageClock::default();
        assert!(c.asr_secs().is_none());
        c.observe("ensuring-models");
        c.observe("transcribing");
        c.observe("transcribing");
        c.observe("diarizing");
        assert!(c.asr_secs().unwrap() >= 0.0);
    }
}
