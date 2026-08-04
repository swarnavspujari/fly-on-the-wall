//! Offline accuracy harness for real recordings: runs the REAL per-channel
//! pipeline (whisper.cpp + sherpa-onnx) over a meeting's WAVs and reports the
//! trust metrics — consecutive-repetition runs (hallucination loops), distinct
//! speaker count, and word counts. `#[ignore]`d: heavy, needs artifacts and a
//! recording on disk.
//!
//! Score an already-exported transcript JSON (no pipeline run):
//!   FLYONTHEWALL_HARNESS_SCORE_JSON=path\to\meeting.json \
//!     cargo test -p fly-app --test accuracy_harness -- --ignored --nocapture
//!
//! Run the pipeline over a recording folder (recording.mic.wav +
//! recording.system.wav, or a single recording.mixed.wav), optionally
//! trimmed for fast iteration:
//!   FLYONTHEWALL_HARNESS_DIR=path\to\recording-folder \
//!   FLYONTHEWALL_HARNESS_MODEL=ggml-large-v3-turbo-q5_0 \
//!   FLYONTHEWALL_HARNESS_MAX_SECS=300 \
//!     cargo test -p fly-app --test accuracy_harness -- --ignored --nocapture
//!
//! Score against a reference transcript — a Fathom .txt export or a
//! Microsoft Teams .vtt transcript (picked by extension) — adds per-channel
//! WER, speaker-attributed WER, attribution error, and the cross-talk
//! duplication rate to any of the modes above:
//!   FLYONTHEWALL_HARNESS_REFERENCE=path\to\fathom-export.txt-or-teams.vtt \
//!   FLYONTHEWALL_HARNESS_REF_SELF=Swarnav          # substring of the ref speaker who is "You"
//!   FLYONTHEWALL_HARNESS_XTALK_MS=500              # dup window (default 500)
//! Teams .vtt references carry utterance END timestamps too, which
//! additionally enables the diarization block: time-weighted DER
//! (speaker-confusion + missed-speech + false-alarm over reference speech
//! time) with and without a 250 ms collar, plus speaker-count accuracy.
//! Teams timestamps are utterance-level, not frame-level — the collared
//! number is the trustworthy one.
//!
//! Diarize-only sweep mode: re-run diarization + word→speaker alignment on
//! top of an existing baseline transcript (ASR is NOT re-run — words come
//! from the baseline), score, and exit. Cheap enough to sweep parameters:
//!   FLYONTHEWALL_HARNESS_DIARIZE_WAV=path\to\channel.wav   # what the diarizer hears
//!   FLYONTHEWALL_HARNESS_BASE_JSON=path\to\baseline.json   # prior OUT_JSON transcript
//!   FLYONTHEWALL_HARNESS_CLUSTER_THRESHOLD=0.9             # optional knobs…
//!   FLYONTHEWALL_HARNESS_NUM_SPEAKERS=2
//!   FLYONTHEWALL_HARNESS_DUST_FLOOR_MS=15000
//!   FLYONTHEWALL_HARNESS_DUST_FRACTION=0.05
//!   FLYONTHEWALL_HARNESS_EMBEDDING=path\to\other-embedding.onnx
//!
//! Every mode: FLYONTHEWALL_HARNESS_RESULTS_JSON=path writes all metrics of
//! the run as one stable-keyed JSON file — the committed baselines under
//! docs/data/diarization/ are exactly these files, so a re-run or sweep
//! shows up as a git diff. FLYONTHEWALL_HARNESS_FIXTURE=name labels the
//! fixture inside the file.
//!
//! Cloud-reference mode (diagnostic only, no local pipeline): transcribe the
//! mic and system channels with Groq to separate model quality from audio
//! quality. The key comes from the environment ONLY — never a file/setting:
//!   GROQ_API_KEY=... FLYONTHEWALL_HARNESS_GROQ=1 FLYONTHEWALL_HARNESS_DIR=... \
//!   FLYONTHEWALL_HARNESS_GROQ_MODEL=whisper-large-v3 \
//!   FLYONTHEWALL_HARNESS_GROQ_CACHE=path\to\cache-dir   # optional per-chunk response cache
//!     cargo test -p fly-app --test accuracy_harness -- --ignored --nocapture

use std::path::{Path, PathBuf};

use fly_core::repeat::loop_token;
use fly_core::{RecordingRef, SpeakerTurn, Transcript};

/// One reportable repetition run: `reps` consecutive occurrences of an
/// `n`-word phrase starting at `start_ms`.
#[derive(Debug, serde::Serialize)]
struct RunReport {
    n: usize,
    reps: usize,
    phrase: String,
    start_ms: u64,
}

fn channel_words(t: &Transcript, mic: bool) -> Vec<(String, u64)> {
    let mut words: Vec<(String, u64)> = t
        .segments
        .iter()
        .filter(|s| (s.speaker_key == "mic") == mic)
        .flat_map(|s| s.words.iter().map(|w| (loop_token(&w.text), w.start_ms)))
        .collect();
    words.sort_by_key(|(_, at)| *at);
    words
}

/// Worst consecutive run per n-gram size (only n with 3+ reps are reported).
fn worst_runs(words: &[(String, u64)]) -> Vec<RunReport> {
    let tokens: Vec<&String> = words.iter().map(|(t, _)| t).collect();
    let mut out = Vec::new();
    for n in 1..=10usize {
        let (mut best, mut best_i) = (1usize, 0usize);
        let mut i = 0;
        while i + n <= tokens.len() {
            let mut reps = 1;
            let mut j = i + n;
            while j + n <= tokens.len() && tokens[j..j + n] == tokens[i..i + n] {
                reps += 1;
                j += n;
            }
            if reps > best {
                (best, best_i) = (reps, i);
            }
            i += if reps > 1 { (reps - 1) * n } else { 1 };
        }
        if best >= 3 {
            out.push(RunReport {
                n,
                reps: best,
                phrase: tokens[best_i..best_i + n]
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                start_ms: words[best_i].1,
            });
        }
    }
    out
}

fn mmss(ms: u64) -> String {
    format!("{:02}:{:02}", ms / 60_000, ms % 60_000 / 1000)
}

fn report(t: &Transcript) -> serde_json::Value {
    let keys: std::collections::BTreeSet<&str> =
        t.segments.iter().map(|s| s.speaker_key.as_str()).collect();
    let non_unknown = keys
        .iter()
        .filter(|k| **k != "spk_unknown" && **k != "mic")
        .count();
    let mic = channel_words(t, true);
    let system = channel_words(t, false);

    eprintln!("== transcript metrics ==");
    eprintln!(
        "segments={} speakers_listed={} speaker_keys={} system_speakers(non-mic, non-unknown)={}",
        t.segments.len(),
        t.speakers.len(),
        keys.len(),
        non_unknown
    );
    eprintln!(
        "words: total={} mic={} system={}",
        mic.len() + system.len(),
        mic.len(),
        system.len()
    );
    let mut worst = serde_json::Map::new();
    for (label, words) in [("mic", &mic), ("system", &system)] {
        let runs = worst_runs(words);
        eprintln!("worst consecutive runs ({label}):");
        if runs.is_empty() {
            eprintln!("  none with 3+ reps");
        }
        for r in &runs {
            eprintln!(
                "  n={} x{} [{}] '{}'",
                r.n,
                r.reps,
                mmss(r.start_ms),
                &r.phrase[..r.phrase.len().min(60)]
            );
        }
        let max_reps = runs.iter().map(|r| r.reps).max().unwrap_or(1);
        worst.insert(format!("worst_reps_{label}"), serde_json::json!(max_reps));
    }

    // per-cluster attributed seconds — the over-splitting signature is one
    // voice spread across several substantial clusters, visible right here
    let mut speaker_seconds: std::collections::BTreeMap<&str, f64> = Default::default();
    for s in &t.segments {
        *speaker_seconds.entry(s.speaker_key.as_str()).or_default() +=
            s.end_ms.saturating_sub(s.start_ms) as f64 / 1000.0;
    }
    let speaker_seconds: serde_json::Map<String, serde_json::Value> = speaker_seconds
        .into_iter()
        .map(|(k, v)| (k.to_string(), serde_json::json!((v * 10.0).round() / 10.0)))
        .collect();

    let metrics = serde_json::json!({
        "segments": t.segments.len(),
        "speakers_listed": t.speakers.len(),
        "speaker_keys": keys.len(),
        "system_speakers": non_unknown,
        "speaker_seconds": speaker_seconds,
        "words_total": mic.len() + system.len(),
        "words_mic": mic.len(),
        "words_system": system.len(),
        "worst_reps_mic": worst["worst_reps_mic"],
        "worst_reps_system": worst["worst_reps_system"],
    });
    eprintln!("HARNESS_METRICS_JSON: {metrics}");
    metrics
}

// ---------------------------------------------------------------------------
// Reference scoring (vs a Fathom .txt export)
// ---------------------------------------------------------------------------

/// Unambiguous non-lexical fillers dropped from BOTH the reference and the
/// hypothesis token streams. The human-verified reference dropped its "um"s,
/// so without this the ASR's fillers score as insertions and the WER is
/// meaningless. Kept deliberately narrow (never a content word) — a bare "a"
/// or "so" is a real word and must survive.
fn is_filler(tok: &str) -> bool {
    matches!(tok, "um" | "uh" | "mm" | "mhm" | "hmm" | "erm")
}

/// Split into lowercase alphanumeric tokens (apostrophes dropped, other
/// punctuation splits): "back-to-back" → [back, to, back], "It's" → [its].
/// Non-lexical fillers (see `is_filler`) are stripped so both sides score
/// against the same filler-free vocabulary.
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            cur.push(c.to_ascii_lowercase());
        } else if c != '\'' && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.retain(|t| !is_filler(t));
    out
}

/// One timed reference utterance. Only Teams .vtt references carry end
/// timestamps; Fathom .txt has turn starts only, so its `turns` stay empty
/// and DER scoring is skipped for it.
struct RefTurn {
    speaker: usize,
    start_ms: u64,
    end_ms: u64,
}

/// The reference transcript: interned speaker names + one flat token stream,
/// plus timed utterances when the format provides them.
struct Reference {
    speakers: Vec<String>,
    /// (normalized token, speaker index)
    tokens: Vec<(String, usize)>,
    /// Timed utterances (empty when the reference has no end timestamps).
    turns: Vec<RefTurn>,
}

fn parse_mmss(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut ms = 0u64;
    for p in &parts {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        ms = ms * 60 + p.parse::<u64>().ok()?;
    }
    Some(ms * 1000)
}

/// Parse a Fathom .txt export: unindented `m:ss - Speaker Name` turn headers
/// followed by indented body lines. `ACTION ITEM:` / `SCREEN SHARING:`
/// annotations end with a `WATCH: <url>` marker; speech can continue after it.
fn parse_fathom_reference(path: &str) -> Reference {
    let raw = std::fs::read_to_string(path).expect("read reference transcript");
    let mut speakers: Vec<String> = Vec::new();
    let mut tokens: Vec<(String, usize)> = Vec::new();
    let mut current: Option<usize> = None;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "---" || trimmed.starts_with("VIEW RECORDING") {
            continue;
        }
        // turn header: unindented "m:ss - Name"
        if !line.starts_with(' ') {
            if let Some((ts, name)) = trimmed.split_once(" - ") {
                if parse_mmss(ts).is_some() {
                    let name = name.trim().to_string();
                    let idx = speakers.iter().position(|s| *s == name).unwrap_or_else(|| {
                        speakers.push(name);
                        speakers.len() - 1
                    });
                    current = Some(idx);
                    continue;
                }
            }
            // title or other unindented metadata — not speech
            continue;
        }
        let Some(speaker) = current else { continue };
        // strip inline annotations up to and including their WATCH url
        let speech =
            if trimmed.starts_with("ACTION ITEM:") || trimmed.starts_with("SCREEN SHARING:") {
                match trimmed.find("WATCH:") {
                    Some(at) => {
                        let rest = trimmed[at + "WATCH:".len()..].trim_start();
                        rest.split_once(char::is_whitespace)
                            .map(|(_, r)| r)
                            .unwrap_or("")
                    }
                    None => "",
                }
            } else {
                trimmed
            };
        for tok in tokenize(speech) {
            tokens.push((tok, speaker));
        }
    }
    assert!(
        !tokens.is_empty(),
        "reference transcript parsed to zero words: {path}"
    );
    Reference {
        speakers,
        tokens,
        turns: Vec::new(),
    }
}

/// "HH:MM:SS.mmm" or "MM:SS.mmm" → ms.
fn parse_vtt_ts(s: &str) -> Option<u64> {
    let (main, frac) = s.trim().split_once('.')?;
    if frac.len() != 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let parts: Vec<&str> = main.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut secs = 0u64;
    for p in parts {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        secs = secs * 60 + p.parse::<u64>().ok()?;
    }
    Some(secs * 1000 + frac.parse::<u64>().ok()?)
}

/// The five entities WebVTT payload text may escape. `&amp;` goes last so a
/// literal "&amp;lt;" doesn't double-decode.
fn decode_vtt_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Parse a Microsoft Teams transcript export (.vtt): cues carry utterance
/// timestamps and `<v Display Name>speech</v>` voice spans. Teams labels
/// each cue from that participant's own audio stream and signed-in identity,
/// so speaker attribution AND utterance timing are ground truth (timing is
/// utterance-level — the DER collar absorbs that).
fn parse_teams_vtt_reference(path: &str) -> Reference {
    let raw = std::fs::read_to_string(path).expect("read reference transcript");
    let mut speakers: Vec<String> = Vec::new();
    let mut tokens: Vec<(String, usize)> = Vec::new();
    let mut turns: Vec<RefTurn> = Vec::new();
    let mut lines = raw.lines().peekable();
    while let Some(line) = lines.next() {
        // cue timing line: "00:00:03.435 --> 00:00:07.772" (+ optional settings)
        let Some((from, to)) = line.split_once("-->") else {
            continue;
        };
        let (Some(start_ms), Some(end_ms)) = (
            parse_vtt_ts(from),
            parse_vtt_ts(to.trim().split_whitespace().next().unwrap_or("")),
        ) else {
            continue;
        };
        // payload: lines until the blank cue separator
        let mut payload = String::new();
        while let Some(next) = lines.peek() {
            if next.trim().is_empty() {
                break;
            }
            if !payload.is_empty() {
                payload.push(' ');
            }
            payload.push_str(lines.next().expect("peeked").trim());
        }
        // voice spans: "<v Name>speech</v>" (Teams: exactly one per cue)
        for span in payload.split("<v ").skip(1) {
            let Some((name, speech)) = span.split_once('>') else {
                continue;
            };
            let name = decode_vtt_entities(name).trim().to_string();
            if name.is_empty() {
                continue;
            }
            let speech = decode_vtt_entities(&speech.replace("</v>", ""));
            let idx = speakers.iter().position(|s| *s == name).unwrap_or_else(|| {
                speakers.push(name);
                speakers.len() - 1
            });
            for tok in tokenize(&speech) {
                tokens.push((tok, idx));
            }
            turns.push(RefTurn {
                speaker: idx,
                start_ms,
                end_ms,
            });
        }
    }
    assert!(!tokens.is_empty(), "teams vtt parsed to zero words: {path}");
    Reference {
        speakers,
        tokens,
        turns,
    }
}

/// Reference format dispatch: Teams .vtt by extension, else Fathom .txt.
fn parse_reference(path: &str) -> Reference {
    if path.to_ascii_lowercase().ends_with(".vtt") {
        parse_teams_vtt_reference(path)
    } else {
        parse_fathom_reference(path)
    }
}

/// One hypothesis token with its timing and the speaker key of its segment.
struct HypWord {
    tok: String,
    start_ms: u64,
    key: String,
}

fn hyp_words(t: &Transcript, keep: impl Fn(&str) -> bool) -> Vec<HypWord> {
    let mut out: Vec<HypWord> = t
        .segments
        .iter()
        .filter(|s| keep(&s.speaker_key))
        .flat_map(|s| {
            s.words.iter().flat_map(|w| {
                tokenize(&w.text).into_iter().map(|tok| HypWord {
                    tok,
                    start_ms: w.start_ms,
                    key: s.speaker_key.clone(),
                })
            })
        })
        .collect();
    out.sort_by_key(|w| w.start_ms);
    out
}

/// Edit operations from a full Levenshtein alignment (unit costs).
enum Op {
    Match(usize, usize),
    Sub,
    Del,
    Ins,
}

/// Global alignment of reference tokens vs hypothesis tokens. O(n·m) with a
/// full u8 traceback matrix — a 26-minute meeting (~4k × ~8k tokens) is ~32 MB
/// and well under a second.
fn align_tokens(r: &[&str], h: &[&str]) -> Vec<Op> {
    let (n, m) = (r.len(), h.len());
    let w = m + 1;
    let mut back = vec![0u8; (n + 1) * w]; // 0=diag-match,1=diag-sub,2=up-del,3=left-ins
    let mut prev: Vec<u32> = (0..=m as u32).collect();
    let mut cur = vec![0u32; m + 1];
    back[1..=m].fill(3);
    for i in 1..=n {
        cur[0] = i as u32;
        back[i * w] = 2;
        for j in 1..=m {
            let (diag_cost, op) = if r[i - 1] == h[j - 1] {
                (0, 0u8)
            } else {
                (1, 1u8)
            };
            let mut best = prev[j - 1] + diag_cost;
            let mut b = op;
            if prev[j] + 1 < best {
                best = prev[j] + 1;
                b = 2;
            }
            if cur[j - 1] + 1 < best {
                best = cur[j - 1] + 1;
                b = 3;
            }
            cur[j] = best;
            back[i * w + j] = b;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let (mut i, mut j) = (n, m);
    let mut ops = Vec::with_capacity(n + m);
    while i > 0 || j > 0 {
        match back[i * w + j] {
            0 => {
                i -= 1;
                j -= 1;
                ops.push(Op::Match(i, j));
            }
            1 => {
                i -= 1;
                j -= 1;
                ops.push(Op::Sub);
            }
            2 => {
                i -= 1;
                ops.push(Op::Del);
            }
            _ => {
                j -= 1;
                ops.push(Op::Ins);
            }
        }
    }
    ops.reverse();
    ops
}

struct WerCounts {
    matches: usize,
    subs: usize,
    dels: usize,
    inss: usize,
    ref_len: usize,
}

impl WerCounts {
    fn wer(&self) -> f64 {
        (self.subs + self.dels + self.inss) as f64 / self.ref_len.max(1) as f64
    }
    fn line(&self) -> String {
        format!(
            "WER={:5.1}%  (S={} D={} I={} match={} / N={})",
            self.wer() * 100.0,
            self.subs,
            self.dels,
            self.inss,
            self.matches,
            self.ref_len
        )
    }
}

fn wer_counts(ops: &[Op], ref_len: usize) -> WerCounts {
    let mut c = WerCounts {
        matches: 0,
        subs: 0,
        dels: 0,
        inss: 0,
        ref_len,
    };
    for op in ops {
        match op {
            Op::Match(..) => c.matches += 1,
            Op::Sub => c.subs += 1,
            Op::Del => c.dels += 1,
            Op::Ins => c.inss += 1,
        }
    }
    c
}

fn score_channel(ref_tokens: &[(String, usize)], hyp: &[HypWord]) -> WerCounts {
    let r: Vec<&str> = ref_tokens.iter().map(|(t, _)| t.as_str()).collect();
    let h: Vec<&str> = hyp.iter().map(|w| w.tok.as_str()).collect();
    wer_counts(&align_tokens(&r, &h), r.len())
}

/// Reference-word index → matched hypothesis-word index.
fn match_map(ref_tokens: &[(String, usize)], hyp: &[HypWord]) -> Vec<Option<usize>> {
    let r: Vec<&str> = ref_tokens.iter().map(|(t, _)| t.as_str()).collect();
    let h: Vec<&str> = hyp.iter().map(|w| w.tok.as_str()).collect();
    let mut map = vec![None; r.len()];
    for op in align_tokens(&r, &h) {
        if let Op::Match(ri, hi) = op {
            map[ri] = Some(hi);
        }
    }
    map
}

/// Pick the reference speaker who is "You": FLYONTHEWALL_HARNESS_REF_SELF substring
/// override, else the ref speaker whose words are best covered by the mic
/// channel (best-effort — echo makes both sides match the mic, so prefer the
/// explicit override on echo-suspect recordings).
fn detect_self(reference: &Reference, mic: &[HypWord]) -> usize {
    if let Ok(hint) = std::env::var("FLYONTHEWALL_HARNESS_REF_SELF") {
        let hint = hint.to_ascii_lowercase();
        if let Some(idx) = reference
            .speakers
            .iter()
            .position(|s| s.to_ascii_lowercase().contains(&hint))
        {
            return idx;
        }
        panic!("FLYONTHEWALL_HARNESS_REF_SELF={hint:?} matches no reference speaker");
    }
    let matched = match_map(&reference.tokens, mic);
    let mut best = (0usize, -1.0f64);
    for (idx, name) in reference.speakers.iter().enumerate() {
        let total = reference.tokens.iter().filter(|(_, s)| *s == idx).count();
        let hits = reference
            .tokens
            .iter()
            .zip(&matched)
            .filter(|((_, s), m)| *s == idx && m.is_some())
            .count();
        let rate = hits as f64 / total.max(1) as f64;
        eprintln!("  self-detect: {name} mic-coverage {:.1}%", rate * 100.0);
        if rate > best.1 {
            best = (idx, rate);
        }
    }
    best.0
}

// ---------------------------------------------------------------------------
// Diarization error rate (vs a timed reference — Teams .vtt only)
// ---------------------------------------------------------------------------

/// Merge into disjoint sorted intervals. Touching intervals fuse — Teams
/// splits one utterance into contiguous cues, and those internal boundaries
/// are artifacts, not speaker changes.
fn union_intervals(mut v: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    v.retain(|(s, e)| e > s);
    v.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(v.len());
    for (s, e) in v {
        match out.last_mut() {
            Some((_, last_e)) if s <= *last_e => *last_e = (*last_e).max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Time-weighted DER components, all in ms. DER = (miss + fa + conf) /
/// ref_speech, the NIST md-eval decomposition: regions are scored with
/// possibly-overlapping speakers on both sides, and hypothesis speakers are
/// mapped to reference speakers by the time-overlap-maximizing assignment.
struct DerScore {
    /// Denominator: Σ region-length × ref-speaker-count over scored regions.
    ref_speech_ms: u64,
    miss_ms: u64,
    fa_ms: u64,
    conf_ms: u64,
}

impl DerScore {
    fn der(&self) -> f64 {
        (self.miss_ms + self.fa_ms + self.conf_ms) as f64 / self.ref_speech_ms.max(1) as f64
    }
    fn frac(&self, ms: u64) -> f64 {
        ms as f64 / self.ref_speech_ms.max(1) as f64
    }
}

/// Score hypothesis speaker spans against timed reference utterances.
/// `collar_ms` is excluded around every merged reference-utterance boundary
/// (±collar), the standard tolerance for imprecise reference timing.
/// Returns the score plus the optimal ref→hyp key mapping.
fn score_der(
    ref_turns: &[RefTurn],
    n_ref: usize,
    hyp_turns: &[SpeakerTurn],
    collar_ms: u64,
) -> (DerScore, Vec<Option<String>>) {
    // per-speaker unioned intervals, both sides
    let mut ref_by: Vec<Vec<(u64, u64)>> = vec![Vec::new(); n_ref];
    for t in ref_turns {
        ref_by[t.speaker].push((t.start_ms, t.end_ms));
    }
    let ref_by: Vec<Vec<(u64, u64)>> = ref_by.into_iter().map(union_intervals).collect();

    let mut hyp_keys: Vec<String> = Vec::new();
    let mut hyp_by: Vec<Vec<(u64, u64)>> = Vec::new();
    for t in hyp_turns {
        let i = hyp_keys
            .iter()
            .position(|k| *k == t.speaker_key)
            .unwrap_or_else(|| {
                hyp_keys.push(t.speaker_key.clone());
                hyp_by.push(Vec::new());
                hyp_keys.len() - 1
            });
        hyp_by[i].push((t.start_ms, t.end_ms));
    }
    let hyp_by: Vec<Vec<(u64, u64)>> = hyp_by.into_iter().map(union_intervals).collect();

    // collar: ±collar around every merged reference boundary is unscored
    let mut excluded: Vec<(u64, u64)> = Vec::new();
    if collar_ms > 0 {
        for &(s, e) in ref_by.iter().flatten() {
            excluded.push((s.saturating_sub(collar_ms), s + collar_ms));
            excluded.push((e.saturating_sub(collar_ms), e + collar_ms));
        }
    }
    let excluded = union_intervals(excluded);

    // elementary timeline: membership is constant between boundaries
    let mut points: Vec<u64> = Vec::new();
    for &(s, e) in ref_by
        .iter()
        .flatten()
        .chain(hyp_by.iter().flatten())
        .chain(excluded.iter())
    {
        points.push(s);
        points.push(e);
    }
    points.sort_unstable();
    points.dedup();
    let active = |ivs: &[(u64, u64)], t: u64| -> bool {
        let i = ivs.partition_point(|&(s, _)| s <= t);
        i > 0 && ivs[i - 1].1 > t
    };

    // pass 1: overlap matrix over scored regions
    let (nr, nh) = (n_ref, hyp_keys.len());
    let mut ov = vec![0u64; nr.max(1) * nh.max(1)];
    let mut regions: Vec<(u64, Vec<usize>, Vec<usize>)> = Vec::new();
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b <= a || active(&excluded, a) {
            continue;
        }
        let rs: Vec<usize> = (0..nr).filter(|&r| active(&ref_by[r], a)).collect();
        let hs: Vec<usize> = (0..nh).filter(|&h| active(&hyp_by[h], a)).collect();
        if rs.is_empty() && hs.is_empty() {
            continue;
        }
        let len = b - a;
        for &r in &rs {
            for &h in &hs {
                ov[r * nh + h] += len;
            }
        }
        regions.push((len, rs, hs));
    }

    // optimal injective ref→hyp assignment maximizing matched time: DP over
    // the ref bitmask, one hyp column at a time. tables[h] = dp after the
    // first h hyp columns; kept whole for the backtrack.
    assert!(nr <= 12, "assignment DP caps at 12 reference speakers");
    let full = 1usize << nr;
    let mut tables: Vec<Vec<u64>> = vec![vec![0; full]];
    for h in 0..nh {
        let prev = tables.last().expect("seeded").clone();
        let mut dp = prev.clone();
        for mask in 0..full {
            for r in 0..nr {
                if mask & (1 << r) == 0 {
                    continue;
                }
                let cand = prev[mask ^ (1 << r)] + ov[r * nh + h];
                if cand > dp[mask] {
                    dp[mask] = cand;
                }
            }
        }
        tables.push(dp);
    }
    let mut mapping: Vec<Option<usize>> = vec![None; nr];
    {
        let mut mask = full - 1;
        for h in (0..nh).rev() {
            if tables[h + 1][mask] == tables[h][mask] {
                continue; // column h contributes nothing at this mask
            }
            for r in 0..nr {
                if mask & (1 << r) != 0
                    && tables[h + 1][mask] == tables[h][mask ^ (1 << r)] + ov[r * nh + h]
                {
                    mapping[r] = Some(h);
                    mask ^= 1 << r;
                    break;
                }
            }
        }
        // a zero-overlap pairing is no pairing
        for r in 0..nr {
            if let Some(h) = mapping[r] {
                if ov[r * nh + h] == 0 {
                    mapping[r] = None;
                }
            }
        }
    }

    // pass 2: NIST decomposition per region under that mapping
    let mut score = DerScore {
        ref_speech_ms: 0,
        miss_ms: 0,
        fa_ms: 0,
        conf_ms: 0,
    };
    for (len, rs, hs) in &regions {
        let (r_n, h_n) = (rs.len() as u64, hs.len() as u64);
        score.ref_speech_ms += len * r_n;
        let matched = rs
            .iter()
            .filter(|&&r| mapping[r].is_some_and(|h| hs.contains(&h)))
            .count() as u64;
        score.miss_ms += len * r_n.saturating_sub(h_n);
        score.fa_ms += len * h_n.saturating_sub(r_n);
        score.conf_ms += len * (r_n.min(h_n).saturating_sub(matched));
    }
    let mapping = mapping
        .into_iter()
        .map(|h| h.map(|h| hyp_keys[h].clone()))
        .collect();
    (score, mapping)
}

/// Hypothesis speaker spans as the user sees them: one span per transcript
/// segment (includes alignment effects, the pre-labeled mic channel, and
/// unknown-fallback spans).
fn transcript_turns(t: &Transcript) -> Vec<SpeakerTurn> {
    t.segments
        .iter()
        .map(|s| SpeakerTurn {
            speaker_key: s.speaker_key.clone(),
            start_ms: s.start_ms,
            end_ms: s.end_ms,
        })
        .collect()
}

/// DER (collared + uncollared) and speaker-count accuracy for one hypothesis
/// span set; `label` names the span source in the log lines ("attributed" =
/// transcript segments, "turns" = raw diarizer output).
fn score_diarization(
    reference: &Reference,
    hyp_turns: &[SpeakerTurn],
    label: &str,
) -> Option<serde_json::Value> {
    if reference.turns.is_empty() {
        eprintln!("(reference has no utterance end timestamps — DER skipped)");
        return None;
    }
    let mut out = serde_json::Map::new();

    // speaker-count accuracy: clusters the product would show as speakers
    let clusters: std::collections::BTreeSet<&str> = hyp_turns
        .iter()
        .map(|t| t.speaker_key.as_str())
        .filter(|k| *k != "mic" && *k != "spk_unknown")
        .collect();
    out.insert(
        "ref_speakers".into(),
        serde_json::json!(reference.speakers.len()),
    );
    out.insert("hyp_clusters".into(), serde_json::json!(clusters.len()));
    eprintln!(
        "speaker count ({label}): predicted {} clusters vs {} reference speakers",
        clusters.len(),
        reference.speakers.len()
    );

    for collar in [250u64, 0] {
        let (s, mapping) = score_der(
            &reference.turns,
            reference.speakers.len(),
            hyp_turns,
            collar,
        );
        eprintln!(
            "DER ({label}, collar {collar}ms): {:5.1}% = confusion {:.1}% + miss {:.1}% + false-alarm {:.1}%  (scored ref speech {:.0}s)",
            s.der() * 100.0,
            s.frac(s.conf_ms) * 100.0,
            s.frac(s.miss_ms) * 100.0,
            s.frac(s.fa_ms) * 100.0,
            s.ref_speech_ms as f64 / 1000.0
        );
        out.insert(
            format!("der_collar_{collar}"),
            serde_json::json!({
                "der": s.der(),
                "confusion": s.frac(s.conf_ms),
                "miss": s.frac(s.miss_ms),
                "false_alarm": s.frac(s.fa_ms),
                "ref_speech_s": s.ref_speech_ms as f64 / 1000.0,
            }),
        );
        if collar == 250 {
            let map: serde_json::Map<String, serde_json::Value> = mapping
                .iter()
                .enumerate()
                .map(|(r, h)| (format!("ref_{r}"), serde_json::json!(h)))
                .collect();
            out.insert("mapping_collar_250".into(), serde_json::Value::Object(map));
        }
    }
    Some(serde_json::Value::Object(out))
}

/// Full reference scoring block: per-channel WER, merged + speaker-attributed
/// WER, attribution error, cross-talk duplication, per-channel bleed.
fn score_against_reference(t: &Transcript, ref_path: &str) -> serde_json::Value {
    let reference = parse_reference(ref_path);
    let mic = hyp_words(t, |k| k == "mic");
    let system = hyp_words(t, |k| k != "mic");
    let merged = hyp_words(t, |_| true);

    // Mixed/import transcripts have no mic channel: the "you"-vs-others
    // splits, bleed, and cross-talk metrics are meaningless there — skipped.
    let self_idx = (!mic.is_empty()).then(|| detect_self(&reference, &mic));
    let ref_you: Vec<(String, usize)> = reference
        .tokens
        .iter()
        .filter(|(_, s)| Some(*s) == self_idx)
        .cloned()
        .collect();
    let ref_others: Vec<(String, usize)> = reference
        .tokens
        .iter()
        .filter(|(_, s)| Some(*s) != self_idx)
        .cloned()
        .collect();

    eprintln!("== reference scoring vs {ref_path} ==");
    let mut m = serde_json::Map::new();
    m.insert(
        "ref_words".into(),
        serde_json::json!(reference.tokens.len()),
    );

    if let Some(self_idx) = self_idx {
        eprintln!(
            "ref: {} words total — {} = \"You\" ({} words), others {} words",
            reference.tokens.len(),
            reference.speakers[self_idx],
            ref_you.len(),
            ref_others.len()
        );
        let mic_vs_you = score_channel(&ref_you, &mic);
        // Diagnostic 0 crux: how well did the MIC transcribe the far-end speaker?
        // Compare this against `system vs ref[others]` to decide which channel's
        // copy of the far-end to keep when de-duplicating cross-talk.
        let mic_vs_others = score_channel(&ref_others, &mic);
        let mic_vs_all = score_channel(&reference.tokens, &mic);
        let sys_vs_others = score_channel(&ref_others, &system);
        eprintln!("mic    vs ref[you]:    {}", mic_vs_you.line());
        eprintln!("mic    vs ref[others]: {}", mic_vs_others.line());
        eprintln!("mic    vs ref[all]:    {}", mic_vs_all.line());
        eprintln!("system vs ref[others]: {}", sys_vs_others.line());
        m.insert("ref_words_you".into(), serde_json::json!(ref_you.len()));
        m.insert("wer_mic_vs_you".into(), serde_json::json!(mic_vs_you.wer()));
        m.insert(
            "wer_mic_vs_others".into(),
            serde_json::json!(mic_vs_others.wer()),
        );
        m.insert("wer_mic_vs_all".into(), serde_json::json!(mic_vs_all.wer()));
        m.insert(
            "wer_system_vs_others".into(),
            serde_json::json!(sys_vs_others.wer()),
        );
    } else {
        eprintln!(
            "ref: {} words total across {} speakers (no mic channel — single-track scoring)",
            reference.tokens.len(),
            reference.speakers.len()
        );
    }
    let sys_vs_all = score_channel(&reference.tokens, &system);
    eprintln!("system vs ref[all]:    {}", sys_vs_all.line());

    // ---- merged transcript: WER + speaker attribution ----
    let r: Vec<&str> = reference.tokens.iter().map(|(t, _)| t.as_str()).collect();
    let h: Vec<&str> = merged.iter().map(|w| w.tok.as_str()).collect();
    let merged_ops = align_tokens(&r, &h);
    let merged_wer = wer_counts(&merged_ops, r.len());
    eprintln!("merged vs ref[all]:    {}", merged_wer.line());

    // hyp speaker key → majority reference speaker over matched words.
    // "mic" is pinned to the local user: that is the product's contract.
    let mut votes: std::collections::BTreeMap<&str, Vec<usize>> = Default::default();
    for op in &merged_ops {
        if let Op::Match(ri, hi) = op {
            let e = votes
                .entry(merged[*hi].key.as_str())
                .or_insert_with(|| vec![0; reference.speakers.len()]);
            e[reference.tokens[*ri].1] += 1;
        }
    }
    let mut mapping: std::collections::BTreeMap<&str, usize> = Default::default();
    for (key, counts) in &votes {
        let best = match (*key, self_idx) {
            ("mic", Some(self_idx)) => self_idx,
            _ => counts
                .iter()
                .enumerate()
                .max_by_key(|(_, c)| **c)
                .map(|(i, _)| i)
                .unwrap_or(0),
        };
        mapping.insert(key, best);
        eprintln!(
            "speaker mapping: {key} → {} (matched-word votes: {})",
            reference.speakers[best],
            counts
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{}={c}", reference.speakers[i]))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let (mut attr_wrong, mut attr_total) = (0usize, 0usize);
    for op in &merged_ops {
        if let Op::Match(ri, hi) = op {
            attr_total += 1;
            if mapping[merged[*hi].key.as_str()] != reference.tokens[*ri].1 {
                attr_wrong += 1;
            }
        }
    }
    let attr_err = attr_wrong as f64 / attr_total.max(1) as f64;
    let sa_wer = (merged_wer.subs + merged_wer.dels + merged_wer.inss + attr_wrong) as f64
        / r.len().max(1) as f64;
    eprintln!(
        "attribution error: {:5.1}% of matched words carry the wrong speaker ({attr_wrong}/{attr_total})",
        attr_err * 100.0
    );
    eprintln!("speaker-attributed WER (merged): {:5.1}%", sa_wer * 100.0);

    // ---- cross-talk duplication: ref words matched on BOTH channels ----
    // (two-channel recordings only — a mixed track has no second channel)
    if let Some(self_idx) = self_idx {
        let window_ms: u64 = std::env::var("FLYONTHEWALL_HARNESS_XTALK_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500);
        let on_mic = match_map(&reference.tokens, &mic);
        let on_sys = match_map(&reference.tokens, &system);
        let (mut dup_windowed, mut dup_any) = (0usize, 0usize);
        let (mut you_on_sys, mut others_on_mic) = (0usize, 0usize);
        for (ri, (mi, si)) in on_mic.iter().zip(&on_sys).enumerate() {
            let speaker = reference.tokens[ri].1;
            if mi.is_some() && speaker != self_idx {
                others_on_mic += 1;
            }
            if si.is_some() && speaker == self_idx {
                you_on_sys += 1;
            }
            if let (Some(mi), Some(si)) = (mi, si) {
                dup_any += 1;
                if mic[*mi].start_ms.abs_diff(system[*si].start_ms) <= window_ms {
                    dup_windowed += 1;
                }
            }
        }
        let n = reference.tokens.len();
        eprintln!(
            "cross-talk duplication: {:5.1}% of ref words on BOTH channels within {window_ms}ms ({dup_windowed}/{n}); {:5.1}% regardless of timing ({dup_any}/{n})",
            dup_windowed as f64 / n as f64 * 100.0,
            dup_any as f64 / n as f64 * 100.0
        );
        eprintln!(
            "bleed: {:5.1}% of ref[others] words appear in MIC channel ({others_on_mic}/{}); {:5.1}% of ref[you] words appear in SYSTEM channel ({you_on_sys}/{})",
            others_on_mic as f64 / ref_others.len().max(1) as f64 * 100.0,
            ref_others.len(),
            you_on_sys as f64 / ref_you.len().max(1) as f64 * 100.0,
            ref_you.len()
        );
        m.insert(
            "xtalk_dup_windowed".into(),
            serde_json::json!(dup_windowed as f64 / n as f64),
        );
        m.insert(
            "xtalk_dup_any".into(),
            serde_json::json!(dup_any as f64 / n as f64),
        );
        m.insert(
            "bleed_others_on_mic".into(),
            serde_json::json!(others_on_mic as f64 / ref_others.len().max(1) as f64),
        );
        m.insert(
            "bleed_you_on_system".into(),
            serde_json::json!(you_on_sys as f64 / ref_you.len().max(1) as f64),
        );
        m.insert("xtalk_window_ms".into(), serde_json::json!(window_ms));
    }

    m.insert(
        "wer_system_vs_all".into(),
        serde_json::json!(sys_vs_all.wer()),
    );
    m.insert("wer_merged".into(), serde_json::json!(merged_wer.wer()));
    m.insert("sa_wer_merged".into(), serde_json::json!(sa_wer));
    m.insert("attribution_error".into(), serde_json::json!(attr_err));

    // ---- diarization: DER over the attributed segment spans ----
    if let Some(d) = score_diarization(&reference, &transcript_turns(t), "attributed") {
        m.insert("diarization".into(), d);
    }

    let metrics = serde_json::Value::Object(m);
    eprintln!("HARNESS_REFERENCE_METRICS_JSON: {metrics}");
    metrics
}

fn maybe_score_reference(t: &Transcript) -> Option<serde_json::Value> {
    std::env::var("FLYONTHEWALL_HARNESS_REFERENCE")
        .ok()
        .map(|ref_path| score_against_reference(t, &ref_path))
}

// ---------------------------------------------------------------------------
// Groq cloud-reference mode (diagnostic: model quality vs audio quality)
// ---------------------------------------------------------------------------

/// Chunk step: Groq's free tier caps uploads at 25 MB, so channels are cut
/// into 10-minute pieces (16 kHz mono FLAC ≈ 10 MB each)…
const GROQ_CHUNK_MS: u64 = 600_000;
/// …with a little overlap so no word is lost on a cut; words whose midpoint
/// falls in the overlap are kept from the earlier chunk only.
const GROQ_OVERLAP_MS: u64 = 15_000;

/// Encode a 16 kHz chunk for upload: FLAC via ffmpeg when available
/// (preferred, ~half the size), else the WAV itself (10 min mono 16 kHz
/// ≈ 19 MB, still under the 25 MB cap).
fn encode_chunk(wav: &Path) -> (std::path::PathBuf, &'static str, &'static str) {
    let flac = wav.with_extension("flac");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(wav)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "flac"])
        .arg(&flac)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        (flac, "audio/flac", "chunk.flac")
    } else {
        eprintln!("ffmpeg unavailable — uploading WAV chunks instead of FLAC");
        (wav.to_path_buf(), "audio/wav", "chunk.wav")
    }
}

/// One Groq transcription call (multipart, temperature 0, verbose_json with
/// word timestamps). Retries transient failures. The API key is read from the
/// GROQ_API_KEY environment variable by the caller — never from disk.
async fn groq_call(api_key: &str, model: &str, media: &Path, mime: &str, name: &str) -> String {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .unwrap();
    for attempt in 1..=4 {
        let bytes = std::fs::read(media).expect("read chunk");
        let form = reqwest::multipart::Form::new()
            .part(
                "file",
                reqwest::multipart::Part::bytes(bytes)
                    .file_name(name.to_string())
                    .mime_str(mime)
                    .unwrap(),
            )
            .text("model", model.to_string())
            .text("temperature", "0")
            .text("language", "en")
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "word");
        let resp = client
            .post("https://api.groq.com/openai/v1/audio/transcriptions")
            .bearer_auth(api_key)
            .multipart(form)
            .send()
            .await;
        match resp {
            Ok(r) => {
                let status = r.status();
                let retry_after = r
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                let body = r.text().await.unwrap_or_default();
                if status.is_success() {
                    return body;
                }
                let transient = status.as_u16() == 429 || status.is_server_error();
                assert!(
                    transient && attempt < 4,
                    "groq returned {status}: {}",
                    body.chars().take(400).collect::<String>()
                );
                let wait = retry_after.unwrap_or(20).min(90);
                eprintln!("groq {status}, retrying in {wait}s (attempt {attempt}/4)");
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            }
            Err(e) => {
                assert!(attempt < 4, "groq request failed: {e}");
                eprintln!("groq network error ({e}), retrying (attempt {attempt}/4)");
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        }
    }
    unreachable!()
}

/// Transcribe one channel via Groq: resample to 16 kHz, chunk, upload, stitch
/// word timestamps back onto the recording clock.
async fn groq_transcribe_channel(
    api_key: &str,
    model: &str,
    src: &Path,
    label: &str,
    max_secs: Option<u64>,
    work: &Path,
) -> Vec<fly_core::Word> {
    let (samples, rate) = fly_audio::mix::read_wav_mono(src).expect("read channel wav");
    let samples = fly_audio::mix::resample_linear(&samples, rate, 16_000);
    let take = max_secs
        .map(|s| ((s * 16_000) as usize).min(samples.len()))
        .unwrap_or(samples.len());
    let samples = &samples[..take];
    let total_ms = samples.len() as u64 / 16; // 16 samples per ms at 16 kHz
    let cache_dir = std::env::var("FLYONTHEWALL_HARNESS_GROQ_CACHE")
        .ok()
        .map(|d| {
            let d = std::path::PathBuf::from(d);
            std::fs::create_dir_all(&d).expect("create groq cache dir");
            d
        });

    let mut words: Vec<fly_core::Word> = Vec::new();
    let mut chunk_start = 0u64;
    while chunk_start < total_ms {
        let chunk_end = (chunk_start + GROQ_CHUNK_MS + GROQ_OVERLAP_MS).min(total_ms);
        let last = chunk_start + GROQ_CHUNK_MS >= total_ms;
        let cache_key = format!("{label}-{model}-{chunk_start}-{chunk_end}-{total_ms}.json");
        let cached = cache_dir
            .as_ref()
            .map(|d| d.join(&cache_key))
            .filter(|p| p.exists());
        let body = match cached {
            Some(p) => {
                eprintln!(
                    "groq {label} chunk @{}s: using cached response",
                    chunk_start / 1000
                );
                std::fs::read_to_string(p).unwrap()
            }
            None => {
                let (a, b) = (
                    (chunk_start * 16) as usize,
                    ((chunk_end * 16) as usize).min(samples.len()),
                );
                let wav = work.join(format!("{label}-{chunk_start}.wav"));
                fly_audio::mix::write_wav_mono_16(&wav, &samples[a..b], 16_000)
                    .expect("write chunk");
                let (media, mime, name) = encode_chunk(&wav);
                let size = std::fs::metadata(&media).map(|m| m.len()).unwrap_or(0);
                eprintln!(
                    "groq {label} chunk @{}s → {} ({:.1} MB)",
                    chunk_start / 1000,
                    media.file_name().unwrap_or_default().to_string_lossy(),
                    size as f64 / 1e6
                );
                assert!(size <= 25_000_000, "chunk exceeds Groq's 25 MB cap");
                let body = groq_call(api_key, model, &media, mime, name).await;
                if let Some(d) = &cache_dir {
                    std::fs::write(d.join(&cache_key), &body).unwrap();
                }
                body
            }
        };
        let raw = fly_asr::groq::parse_groq_verbose_json(&body).expect("parse groq response");
        for mut w in raw.words {
            w.start_ms += chunk_start;
            w.end_ms += chunk_start;
            let mid = (w.start_ms + w.end_ms) / 2;
            // overlap policy: the earlier chunk owns the overlap region
            if last || mid < chunk_start + GROQ_CHUNK_MS {
                words.push(w);
            }
        }
        chunk_start += GROQ_CHUNK_MS;
    }
    words.sort_by_key(|w| w.start_ms);
    eprintln!(
        "groq {label}: {} words over {}s",
        words.len(),
        total_ms / 1000
    );
    words
}

/// Build a two-channel transcript from Groq output (no diarization — this
/// mode isolates ASR/audio quality; the system channel is one speaker key).
fn groq_reference_transcript(rec_dir: &Path, max_secs: Option<u64>) -> Transcript {
    let api_key = std::env::var("GROQ_API_KEY")
        .expect("FLYONTHEWALL_HARNESS_GROQ is set but GROQ_API_KEY is not in the environment");
    let model = std::env::var("FLYONTHEWALL_HARNESS_GROQ_MODEL")
        .unwrap_or_else(|_| "whisper-large-v3".into());
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (mic_words, sys_words) = runtime.block_on(async {
        let mic = groq_transcribe_channel(
            &api_key,
            &model,
            &rec_dir.join("recording.mic.wav"),
            "mic",
            max_secs,
            tmp.path(),
        )
        .await;
        let sys = groq_transcribe_channel(
            &api_key,
            &model,
            &rec_dir.join("recording.system.wav"),
            "system",
            max_secs,
            tmp.path(),
        )
        .await;
        (mic, sys)
    });

    let align_opts = fly_core::AlignOptions::default();
    let mut segments =
        fly_core::align::segments_from_single_speaker(&mic_words, "mic", &align_opts);
    segments.extend(fly_core::align::segments_from_single_speaker(
        &sys_words,
        "spk_0",
        &align_opts,
    ));
    segments.sort_by_key(|s| (s.start_ms, s.end_ms));
    Transcript {
        meeting_id: "groq-harness".into(),
        language: Some("en".into()),
        engine: format!("groq:{model}"),
        segments,
        speakers: vec![
            fly_core::Speaker {
                key: "mic".into(),
                label: "You".into(),
            },
            fly_core::Speaker {
                key: "spk_0".into(),
                label: "System".into(),
            },
        ],
    }
}

/// Recursively hardlink a directory tree (same-volume, instant, no copies).
fn link_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            link_tree(&entry.path(), &target)?;
        } else {
            std::fs::hard_link(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Copy a recording channel into the meeting dir, trimmed to `max_secs`.
fn stage_channel(src: &Path, dst: &Path, max_secs: Option<u64>) -> Result<u64, String> {
    match max_secs {
        None => {
            std::fs::copy(src, dst).map_err(|e| e.to_string())?;
            let (samples, rate) = fly_audio::mix::read_wav_mono(dst).map_err(|e| e.to_string())?;
            Ok(samples.len() as u64 * 1000 / rate as u64)
        }
        Some(secs) => {
            let (samples, rate) = fly_audio::mix::read_wav_mono(src).map_err(|e| e.to_string())?;
            let take = (secs * rate as u64).min(samples.len() as u64) as usize;
            fly_audio::mix::write_wav_mono_16(dst, &samples[..take], rate)
                .map_err(|e| e.to_string())?;
            Ok(take as u64 * 1000 / rate as u64)
        }
    }
}

// ---------------------------------------------------------------------------
// Sweep knobs, artifact resolution, results publishing
// ---------------------------------------------------------------------------

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// Diarization options for the diarize-only sweep mode: the shipped defaults
/// unless overridden from the environment (see file docs).
fn sweep_diarize_options() -> fly_diarize::DiarizeOptions {
    let mut opts = fly_diarize::DiarizeOptions::default();
    if let Some(t) = env_parse::<f32>("FLYONTHEWALL_HARNESS_CLUSTER_THRESHOLD") {
        opts.cluster_threshold = Some(t);
    }
    if let Some(n) = env_parse::<usize>("FLYONTHEWALL_HARNESS_NUM_SPEAKERS") {
        opts.num_speakers = Some(n);
    }
    opts
}

fn sweep_dust() -> (u64, f64) {
    (
        env_parse("FLYONTHEWALL_HARNESS_DUST_FLOOR_MS").unwrap_or(fly_diarize::DUST_FLOOR_MS),
        env_parse("FLYONTHEWALL_HARNESS_DUST_FRACTION").unwrap_or(fly_diarize::DUST_FRACTION),
    )
}

/// The run configuration echoed into the results JSON. `sweep` = the
/// diarize-only mode, where the env knobs actually apply; the pipeline mode
/// always runs the shipped defaults.
fn config_json(mode: &str, sweep: bool) -> serde_json::Value {
    let opts = if sweep {
        sweep_diarize_options()
    } else {
        fly_diarize::DiarizeOptions::default()
    };
    let (floor, fraction) = if sweep {
        sweep_dust()
    } else {
        (fly_diarize::DUST_FLOOR_MS, fly_diarize::DUST_FRACTION)
    };
    let embedding = std::env::var("FLYONTHEWALL_HARNESS_EMBEDDING")
        .ok()
        .filter(|_| sweep)
        .and_then(|p| {
            Path::new(&p)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "campplus.onnx (pinned)".into());
    let sentence_align = sweep
        && std::env::var("FLYONTHEWALL_HARNESS_SENTENCE_ALIGN")
            .is_ok_and(|v| !v.is_empty() && v != "0");
    // pipeline mode always runs the shipped engine (VBx refinement on);
    // diarize-only arms opt in via FLYONTHEWALL_HARNESS_VBX
    let vbx = match mode {
        "pipeline" => serde_json::json!(true),
        "diarize-only" => serde_json::json!(
            std::env::var("FLYONTHEWALL_HARNESS_VBX").is_ok_and(|v| !v.is_empty() && v != "0")
        ),
        _ => serde_json::Value::Null,
    };
    serde_json::json!({
        "mode": mode,
        // f32 → short f64 so committed baselines stay readable (0.9, not 0.899…)
        "cluster_threshold": opts
            .cluster_threshold
            .map(|t| (t as f64 * 10_000.0).round() / 10_000.0),
        "num_speakers": opts.num_speakers,
        "dust_floor_ms": floor,
        "dust_fraction": fraction,
        "embedding": embedding,
        "align": if sentence_align { "sentence" } else { "word" },
        "vbx": vbx,
    })
}

/// Write the run's full metric set to FLYONTHEWALL_HARNESS_RESULTS_JSON (when
/// set). The committed per-fixture baselines under docs/data/diarization/
/// are these files verbatim — a sweep or regression shows up as a git diff.
fn write_results(results: serde_json::Map<String, serde_json::Value>) {
    let Ok(path) = std::env::var("FLYONTHEWALL_HARNESS_RESULTS_JSON") else {
        return;
    };
    let mut all = serde_json::Map::new();
    if let Ok(name) = std::env::var("FLYONTHEWALL_HARNESS_FIXTURE") {
        all.insert("fixture".into(), serde_json::json!(name));
    }
    all.extend(results);
    let body = serde_json::to_string_pretty(&serde_json::Value::Object(all)).expect("serialize");
    std::fs::write(&path, body + "\n").expect("write results json");
    eprintln!("results written to {path}");
}

struct DiarizerPaths {
    exe: PathBuf,
    seg: PathBuf,
    emb: PathBuf,
}

/// Resolve the diarization sidecar + model paths through the models registry
/// (the single source of truth for artifact locations — no hard-coded
/// platform strings). Missing artifacts → SKIP message + None, the same
/// contract as rediarize_e2e.
fn diarizer_paths(real_data: &Path) -> Option<DiarizerPaths> {
    let rel = |id: &str| fly_app_lib::models::artifact(id).map(|a| real_data.join(a.probe_rel));
    let (Some(exe), Some(seg), Some(default_emb)) = (
        rel("sherpa-bin"),
        rel("pyannote-seg"),
        rel("campplus-embedding"),
    ) else {
        eprintln!("SKIP: diarization artifacts not registered for this OS");
        return None;
    };
    let emb = std::env::var("FLYONTHEWALL_HARNESS_EMBEDDING")
        .map(PathBuf::from)
        .unwrap_or(default_emb);
    for p in [&exe, &seg, &emb] {
        if !p.exists() {
            eprintln!("SKIP: artifact not installed: {}", p.display());
            return None;
        }
    }
    Some(DiarizerPaths { exe, seg, emb })
}

/// VBx refinement of the sherpa init clustering via the shipped
/// fly_diarize::refine lab. Returns refined turns + a lab-info JSON. Grid
/// mode (FLYONTHEWALL_HARNESS_VBX_GRID=1, needs a reference) scores every
/// (Fa, Fb, loopP) combo on raw-turn DER and keeps the best.
fn vbx_refine(
    samples_16k: &[f32],
    raw_turns: &[SpeakerTurn],
    emb_model: &Path,
    threads: usize,
) -> (Vec<SpeakerTurn>, serde_json::Value) {
    let lab = fly_diarize::refine::VbxLab::prepare(samples_16k, raw_turns, emb_model, threads)
        .expect("vbx lab prepare");
    eprintln!(
        "vbx: {} subsegments embedded in {:.1}s",
        lab.n_subsegs, lab.embed_secs
    );
    let mut params = fly_diarize::vbx::VbxParams::default();
    if let Some(v) = env_parse("FLYONTHEWALL_HARNESS_VBX_FA") {
        params.fa = v;
    }
    if let Some(v) = env_parse("FLYONTHEWALL_HARNESS_VBX_FB") {
        params.fb = v;
    }
    if let Some(v) = env_parse("FLYONTHEWALL_HARNESS_VBX_LOOPP") {
        params.loop_prob = v;
    }
    let grid =
        std::env::var("FLYONTHEWALL_HARNESS_VBX_GRID").is_ok_and(|v| !v.is_empty() && v != "0");
    let mut info = serde_json::Map::new();
    info.insert("n_subsegs".into(), serde_json::json!(lab.n_subsegs));
    info.insert("embed_secs".into(), serde_json::json!(lab.embed_secs));

    if grid {
        let reference = std::env::var("FLYONTHEWALL_HARNESS_REFERENCE")
            .map(|p| parse_reference(&p))
            .expect("VBX_GRID needs FLYONTHEWALL_HARNESS_REFERENCE to score combos");
        let mut rows = Vec::new();
        let mut best: Option<(f64, (f64, f64, f64), Vec<SpeakerTurn>)> = None;
        for fa in [0.1, 0.3, 1.0] {
            for fb in [4.0, 17.0, 64.0] {
                for lp in [0.9, 0.99] {
                    let combo = fly_diarize::vbx::VbxParams {
                        fa,
                        fb,
                        loop_prob: lp,
                        ..Default::default()
                    };
                    let (turns, iters) = lab.run(&combo);
                    let label = format!("vbx fa={fa} fb={fb} loopP={lp}");
                    let score = score_diarization(&reference, &turns, &label)
                        .expect("vtt reference scores DER");
                    let der = score["der_collar_250"]["der"].as_f64().unwrap_or(1.0);
                    let clusters = score["hyp_clusters"].as_u64().unwrap_or(0);
                    rows.push(serde_json::json!({
                        "fa": fa, "fb": fb, "loop_p": lp, "iters": iters,
                        "der_collar_250": der, "clusters": clusters,
                    }));
                    if best.as_ref().map(|(b, _, _)| der < *b).unwrap_or(true) {
                        best = Some((der, (fa, fb, lp), turns));
                    }
                }
            }
        }
        let (der, (fa, fb, lp), turns) = best.expect("grid ran");
        eprintln!(
            "vbx grid best: fa={fa} fb={fb} loopP={lp} → turn-DER250 {:.1}%",
            der * 100.0
        );
        info.insert("grid".into(), serde_json::json!(rows));
        info.insert(
            "chosen".into(),
            serde_json::json!({"fa": fa, "fb": fb, "loop_p": lp}),
        );
        (turns, serde_json::Value::Object(info))
    } else {
        let (turns, iters) = lab.run(&params);
        info.insert(
            "chosen".into(),
            serde_json::json!({"fa": params.fa, "fb": params.fb, "loop_p": params.loop_prob, "iters": iters}),
        );
        (turns, serde_json::Value::Object(info))
    }
}

/// Diarize-only sweep mode: fresh diarization over one audio channel, words
/// re-attributed from an existing baseline transcript (mic segments pass
/// through untouched — they are pre-labeled by construction), then the full
/// scoring stack. No ASR run, so a parameter sweep costs only diarization.
fn diarize_only(wav: &str) {
    let base_json = std::env::var("FLYONTHEWALL_HARNESS_BASE_JSON").expect(
        "diarize-only mode needs FLYONTHEWALL_HARNESS_BASE_JSON (a prior FLYONTHEWALL_HARNESS_OUT_JSON transcript)",
    );
    let base: Transcript = serde_json::from_str(
        &std::fs::read_to_string(&base_json).expect("read base transcript json"),
    )
    .expect("parse base transcript json");

    let real_data = dirs::data_dir().unwrap().join("FlyOnTheWall");
    let Some(paths) = diarizer_paths(&real_data) else {
        return;
    };

    // words to re-attribute: everything not pre-labeled "mic"
    let mut far_words: Vec<fly_core::Word> = base
        .segments
        .iter()
        .filter(|s| s.speaker_key != "mic")
        .flat_map(|s| s.words.iter().cloned())
        .collect();
    far_words.sort_by_key(|w| (w.start_ms, w.end_ms));
    let mic_segments: Vec<fly_core::TranscriptSegment> = base
        .segments
        .iter()
        .filter(|s| s.speaker_key == "mic")
        .cloned()
        .collect();

    // 16 kHz mono input for the diarizer
    let tmp = tempfile::tempdir().unwrap();
    let (samples, rate) = fly_audio::mix::read_wav_mono(Path::new(wav)).expect("read diarize wav");
    let samples_16k = if rate == 16_000 {
        samples
    } else {
        fly_audio::mix::resample_linear(&samples, rate, 16_000)
    };
    let wav16 = if rate == 16_000 {
        PathBuf::from(wav)
    } else {
        let p = tmp.path().join("diarize.16k.wav");
        fly_audio::mix::write_wav_mono_16(&p, &samples_16k, 16_000).expect("write 16k wav");
        p
    };

    let opts = sweep_diarize_options();
    let (floor, fraction) = sweep_dust();
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let emb_model = paths.emb.clone();
    let engine = fly_diarize::sherpa::SherpaDiarizeEngine {
        exe: paths.exe,
        segmentation_model: paths.seg,
        embedding_model: paths.emb,
        threads,
        // raw sidecar output: the harness applies dust or VBx itself so
        // legacy/refined arms stay separately measurable
        refine: None,
    };
    use fly_diarize::DiarizationEngine as _;
    let started = std::time::Instant::now();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let raw_turns = runtime
        .block_on(engine.diarize(&wav16, &opts))
        .expect("diarize");
    let diarize_secs = started.elapsed().as_secs_f64();
    eprintln!(
        "diarize took {diarize_secs:.1}s: {} raw turns, threshold={:?} num_speakers={:?}",
        raw_turns.len(),
        opts.cluster_threshold,
        opts.num_speakers
    );

    // VBx mode replaces the dust filter: the init clustering (however
    // shattered) is refined instead of pruned.
    let vbx_mode =
        std::env::var("FLYONTHEWALL_HARNESS_VBX").is_ok_and(|v| !v.is_empty() && v != "0");
    let mut vbx_info: Option<serde_json::Value> = None;
    let turns = if vbx_mode {
        let (turns, info) = vbx_refine(&samples_16k, &raw_turns, &emb_model, threads);
        vbx_info = Some(info);
        turns
    } else {
        fly_diarize::drop_dust_clusters_with(raw_turns, floor, fraction)
    };

    let align_opts = fly_core::AlignOptions::default();
    let sentence_align = std::env::var("FLYONTHEWALL_HARNESS_SENTENCE_ALIGN")
        .is_ok_and(|v| !v.is_empty() && v != "0");
    let mut segments = mic_segments;
    segments.extend(if sentence_align {
        fly_core::align::align_words_to_speakers_by_sentence(&far_words, &turns, &align_opts)
    } else {
        fly_core::align::align_words_to_speakers(&far_words, &turns, &align_opts)
    });
    segments.sort_by_key(|s| (s.start_ms, s.end_ms));
    let mut speakers: Vec<fly_core::Speaker> = Vec::new();
    for s in &segments {
        if !speakers.iter().any(|sp| sp.key == s.speaker_key) {
            speakers.push(fly_core::Speaker {
                key: s.speaker_key.clone(),
                label: s.speaker_key.clone(),
            });
        }
    }
    let t = Transcript {
        meeting_id: base.meeting_id.clone(),
        language: base.language.clone(),
        engine: format!("{}+rediarize", base.engine),
        segments,
        speakers,
    };

    if let Ok(out) = std::env::var("FLYONTHEWALL_HARNESS_OUT_JSON") {
        std::fs::write(&out, serde_json::to_string_pretty(&t).unwrap()).unwrap();
        eprintln!("transcript written to {out}");
    }

    let mut results = serde_json::Map::new();
    results.insert("config".into(), config_json("diarize-only", true));
    results.insert("diarize_secs".into(), serde_json::json!(diarize_secs));
    if let Some(info) = vbx_info {
        results.insert("vbx_lab".into(), info);
    }
    results.insert("transcript".into(), report(&t));
    if let Some(r) = maybe_score_reference(&t) {
        results.insert("reference".into(), r);
    }
    // raw diarizer turns scored separately from the attributed segments:
    // separates clustering error from alignment error
    if let Ok(ref_path) = std::env::var("FLYONTHEWALL_HARNESS_REFERENCE") {
        let reference = parse_reference(&ref_path);
        if let Some(d) = score_diarization(&reference, &turns, "turns") {
            results.insert("diarization_turns".into(), d);
        }
    }
    write_results(results);
}

#[test]
#[ignore = "offline accuracy harness; needs artifacts + a recording, see file docs"]
fn accuracy_harness() {
    // surface pipeline logs (e.g. collapsed-loop warnings) on stderr
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fly_app_lib=debug,fly_asr=debug".into()),
        )
        .try_init();

    // ---- score-only mode: metrics for an exported transcript JSON ----
    if let Ok(json_path) = std::env::var("FLYONTHEWALL_HARNESS_SCORE_JSON") {
        let raw = std::fs::read_to_string(&json_path).expect("read score json");
        let t: Transcript = serde_json::from_str(&raw).expect("parse transcript json");
        let mut results = serde_json::Map::new();
        results.insert("config".into(), config_json("score-json", false));
        results.insert("transcript".into(), report(&t));
        if let Some(r) = maybe_score_reference(&t) {
            results.insert("reference".into(), r);
        }
        write_results(results);
        return;
    }

    // ---- diarize-only sweep mode: no ASR, see file docs ----
    if let Ok(wav) = std::env::var("FLYONTHEWALL_HARNESS_DIARIZE_WAV") {
        diarize_only(&wav);
        return;
    }

    let Ok(rec_dir) = std::env::var("FLYONTHEWALL_HARNESS_DIR") else {
        eprintln!("SKIP: set FLYONTHEWALL_HARNESS_DIR, FLYONTHEWALL_HARNESS_SCORE_JSON, or FLYONTHEWALL_HARNESS_DIARIZE_WAV");
        return;
    };
    let rec_dir = std::path::PathBuf::from(rec_dir);
    let mic_src = rec_dir.join("recording.mic.wav");
    let sys_src = rec_dir.join("recording.system.wav");
    let mixed_src = rec_dir.join("recording.mixed.wav");
    let per_channel = mic_src.exists() && sys_src.exists();
    assert!(
        per_channel || mixed_src.exists(),
        "FLYONTHEWALL_HARNESS_DIR needs recording.mic.wav + recording.system.wav, or a recording.mixed.wav"
    );

    let model = std::env::var("FLYONTHEWALL_HARNESS_MODEL")
        .unwrap_or_else(|_| "ggml-large-v3-turbo-q5_0".into());
    let max_secs = std::env::var("FLYONTHEWALL_HARNESS_MAX_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());

    // ---- Groq cloud-reference mode: no local pipeline, both channels ----
    if std::env::var("FLYONTHEWALL_HARNESS_GROQ").is_ok_and(|v| !v.is_empty() && v != "0") {
        let t = groq_reference_transcript(&rec_dir, max_secs);
        if let Ok(out) = std::env::var("FLYONTHEWALL_HARNESS_OUT_JSON") {
            std::fs::write(&out, serde_json::to_string_pretty(&t).unwrap()).unwrap();
            eprintln!("transcript written to {out}");
        }
        let mut results = serde_json::Map::new();
        results.insert("config".into(), config_json("groq", false));
        results.insert("transcript".into(), report(&t));
        if let Some(r) = maybe_score_reference(&t) {
            results.insert("reference".into(), r);
        }
        write_results(results);
        return;
    }

    // ---- artifacts: resolved via the models registry, hardlinked from the
    // real data dir like the golden E2E; missing → skip, not panic ----
    let real_data = dirs::data_dir().unwrap().join("FlyOnTheWall");
    if diarizer_paths(&real_data).is_none() {
        return; // SKIP already printed
    }
    if !fly_app_lib::models::tool_installed(
        &real_data,
        fly_app_lib::models::WHISPER_ENGINE_ID,
        fly_app_lib::models::WHISPER_CLI_NAMES,
    ) {
        eprintln!("SKIP: whisper engine not installed (no managed artifact, nothing on PATH)");
        return;
    }
    let model_rel = fly_app_lib::models::artifact(&model)
        .map(|a| a.probe_rel.to_string())
        .unwrap_or_else(|| format!("models/asr/{model}.bin"));
    if !real_data.join(&model_rel).exists() {
        eprintln!(
            "SKIP: ASR model not installed: {}",
            real_data.join(&model_rel).display()
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    let mut link_subs: Vec<&str> = vec!["models/diarize"];
    for id in ["whisper-bin", "sherpa-bin"] {
        if let Some(a) = fly_app_lib::models::artifact(id) {
            if real_data.join(a.probe_rel).exists() {
                link_subs.push(a.dest_rel);
            }
        }
    }
    // GPU modes need the Vulkan build in place (no download inside the test)
    let gpu = std::env::var("FLYONTHEWALL_HARNESS_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    if gpu {
        match fly_app_lib::models::artifact("whisper-bin-vulkan") {
            Some(a) if real_data.join(a.probe_rel).exists() => link_subs.push(a.dest_rel),
            _ => {
                eprintln!(
                    "SKIP: FLYONTHEWALL_HARNESS_GPU set but whisper-bin-vulkan is not installed"
                );
                return;
            }
        }
    }
    for sub in link_subs {
        link_tree(&real_data.join(sub), &data_dir.join(sub)).unwrap();
    }
    let model_dst = data_dir.join(&model_rel);
    std::fs::create_dir_all(model_dst.parent().expect("model path has a parent")).unwrap();
    std::fs::hard_link(real_data.join(&model_rel), &model_dst).unwrap();

    let state = fly_app_lib::state::AppState::init_with(
        data_dir.clone(),
        std::sync::Arc::new(fly_secrets::MemorySecretStore::default()),
    )
    .unwrap();

    let meeting_id = {
        let storage = state.storage.lock().unwrap();
        storage.set_setting("asr.tier", "light").unwrap();
        storage.set_setting("asr.model_id", &model).unwrap();
        // Deterministic engine selection: CPU by default. FLYONTHEWALL_HARNESS_GPU=1
        // forces the Vulkan build (verdict pre-seeded so no benchmark runs;
        // pick the GPU with GGML_VK_VISIBLE_DEVICES). FLYONTHEWALL_HARNESS_GPU=bench
        // enables GPU with NO verdict, exercising the real in-pipeline
        // benchmark + gate exactly as a user's machine would.
        match std::env::var("FLYONTHEWALL_HARNESS_GPU").ok().as_deref() {
            Some("bench") => {
                storage.set_setting("asr.use_gpu", "true").unwrap();
            }
            Some(v) if !v.is_empty() && v != "0" => {
                storage.set_setting("asr.use_gpu", "true").unwrap();
                storage
                    .set_setting(
                        "asr.gpu_bench",
                        &format!(
                            r#"{{"verdict":"gpu","reason":"forced by accuracy_harness","gpu_secs":null,"cpu_secs":null,"model_id":"{model}"}}"#
                        ),
                    )
                    .unwrap();
            }
            _ => {
                storage.set_setting("asr.use_gpu", "false").unwrap();
            }
        }
        let note = storage.create_note("Accuracy harness", None).unwrap();
        let meeting = storage
            .create_meeting("Accuracy harness", &note.id, &[])
            .unwrap();
        let meet_dir = data_dir.join("recordings").join(&meeting.id);
        std::fs::create_dir_all(&meet_dir).unwrap();
        let recording = if per_channel {
            let dur_mic =
                stage_channel(&mic_src, &meet_dir.join("recording.mic.wav"), max_secs).unwrap();
            let dur_sys =
                stage_channel(&sys_src, &meet_dir.join("recording.system.wav"), max_secs).unwrap();
            RecordingRef {
                mic_path: Some(format!("recordings/{}/recording.mic.wav", meeting.id)),
                system_path: Some(format!("recordings/{}/recording.system.wav", meeting.id)),
                mixed_path: None,
                playback_path: None,
                duration_ms: dur_mic.max(dur_sys),
            }
        } else {
            // single mixed track — the import path: the whole track is diarized
            let dur =
                stage_channel(&mixed_src, &meet_dir.join("recording.mixed.wav"), max_secs).unwrap();
            RecordingRef {
                mic_path: None,
                system_path: None,
                mixed_path: Some(format!("recordings/{}/recording.mixed.wav", meeting.id)),
                playback_path: None,
                duration_ms: dur,
            }
        };
        storage.end_meeting(&meeting.id, &recording).unwrap();
        meeting.id
    };

    let started = std::time::Instant::now();
    let on_stage = |p: fly_app_lib::pipeline::PipelineProgress| {
        eprintln!(
            "[{:>6.1}s] stage: {} {}",
            started.elapsed().as_secs_f32(),
            p.stage,
            p.detail.unwrap_or_default()
        )
    };
    let on_model = |p: fly_app_lib::models::ModelProgress| eprintln!("model: {}", p.stage);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let transcript = runtime
        .block_on(fly_app_lib::pipeline::run_with(
            &state,
            &on_stage,
            &on_model,
            &meeting_id,
        ))
        .expect("pipeline should succeed");
    let pipeline_secs = started.elapsed().as_secs_f64();
    eprintln!("pipeline took {pipeline_secs:.1}s");

    // keep the produced transcript for spot-checking against the audio
    if let Ok(out) = std::env::var("FLYONTHEWALL_HARNESS_OUT_JSON") {
        std::fs::write(&out, serde_json::to_string_pretty(&transcript).unwrap()).unwrap();
        eprintln!("transcript written to {out}");
    }

    let mut results = serde_json::Map::new();
    let mut config = config_json("pipeline", false);
    config["asr_model"] = serde_json::json!(model);
    config["gpu"] = serde_json::json!(gpu);
    config["max_secs"] = serde_json::json!(max_secs);
    results.insert("config".into(), config);
    results.insert("pipeline_secs".into(), serde_json::json!(pipeline_secs));
    results.insert("transcript".into(), report(&transcript));
    if let Some(r) = maybe_score_reference(&transcript) {
        results.insert("reference".into(), r);
    }
    write_results(results);
}

// ---------------------------------------------------------------------------
// Unit tests (not #[ignore]d — pure parsing/scoring math, no artifacts)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod der_tests {
    use super::*;

    fn turn(key: &str, start_ms: u64, end_ms: u64) -> SpeakerTurn {
        SpeakerTurn {
            speaker_key: key.into(),
            start_ms,
            end_ms,
        }
    }

    fn rt(speaker: usize, start_ms: u64, end_ms: u64) -> RefTurn {
        RefTurn {
            speaker,
            start_ms,
            end_ms,
        }
    }

    #[test]
    fn vtt_timestamps_parse() {
        assert_eq!(parse_vtt_ts("00:00:03.435"), Some(3_435));
        assert_eq!(parse_vtt_ts("01:02:03.004"), Some(3_723_004));
        assert_eq!(parse_vtt_ts("02:03.004 "), Some(123_004));
        assert_eq!(parse_vtt_ts("nope"), None);
        assert_eq!(parse_vtt_ts("00:00:03"), None);
    }

    #[test]
    fn teams_vtt_parses_speakers_tokens_and_turns() {
        let vtt = "\
WEBVTT

abc/1-0
00:00:01.000 --> 00:00:03.000
<v Ada Lovelace>Hello there,
this continues.</v>

abc/2-0
00:00:03.000 --> 00:00:04.500
<v Bob>Hi Ada &amp; all.</v>
";
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ref.vtt");
        std::fs::write(&p, vtt).unwrap();
        let r = parse_teams_vtt_reference(p.to_str().unwrap());
        assert_eq!(r.speakers, vec!["Ada Lovelace", "Bob"]);
        assert_eq!(r.turns.len(), 2);
        assert_eq!(r.turns[0].start_ms, 1000);
        assert_eq!(r.turns[0].end_ms, 3000);
        assert_eq!(r.turns[1].speaker, 1);
        let toks: Vec<&str> = r.tokens.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            toks,
            vec!["hello", "there", "this", "continues", "hi", "ada", "all"]
        );
    }

    #[test]
    fn intervals_union_and_fuse() {
        assert_eq!(
            union_intervals(vec![(5, 10), (0, 5), (20, 30), (8, 12), (30, 30)]),
            vec![(0, 12), (20, 30)]
        );
    }

    #[test]
    fn perfect_hypothesis_scores_zero() {
        let refs = [rt(0, 0, 10_000), rt(1, 10_000, 20_000)];
        let hyps = [turn("spk_0", 0, 10_000), turn("spk_1", 10_000, 20_000)];
        let (s, mapping) = score_der(&refs, 2, &hyps, 0);
        assert_eq!(s.miss_ms + s.fa_ms + s.conf_ms, 0);
        assert_eq!(s.ref_speech_ms, 20_000);
        assert_eq!(mapping[0].as_deref(), Some("spk_0"));
        assert_eq!(mapping[1].as_deref(), Some("spk_1"));
    }

    #[test]
    fn one_cluster_for_two_speakers_is_half_confusion() {
        let refs = [rt(0, 0, 10_000), rt(1, 10_000, 20_000)];
        let hyps = [turn("spk_0", 0, 20_000)];
        let (s, _) = score_der(&refs, 2, &hyps, 0);
        assert_eq!(s.conf_ms, 10_000);
        assert_eq!(s.miss_ms, 0);
        assert_eq!(s.fa_ms, 0);
        assert!((s.der() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn empty_hypothesis_is_all_miss() {
        let refs = [rt(0, 0, 10_000)];
        let (s, _) = score_der(&refs, 1, &[], 0);
        assert_eq!(s.miss_ms, 10_000);
        assert!((s.der() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn hyp_speech_outside_ref_is_false_alarm() {
        let refs = [rt(0, 0, 10_000)];
        let hyps = [turn("spk_0", 0, 10_000), turn("spk_0", 30_000, 35_000)];
        let (s, _) = score_der(&refs, 1, &hyps, 0);
        assert_eq!(s.fa_ms, 5_000);
        assert_eq!(s.conf_ms + s.miss_ms, 0);
    }

    #[test]
    fn collar_absorbs_boundary_jitter() {
        // hypothesis switches 200 ms late — inside a 250 ms collar
        let refs = [rt(0, 0, 10_000), rt(1, 10_000, 20_000)];
        let hyps = [turn("spk_0", 0, 10_200), turn("spk_1", 10_200, 20_000)];
        let (strict, _) = score_der(&refs, 2, &hyps, 0);
        assert_eq!(strict.conf_ms, 200);
        let (collared, _) = score_der(&refs, 2, &hyps, 250);
        assert_eq!(collared.conf_ms, 0);
        assert_eq!(collared.miss_ms + collared.fa_ms, 0);
    }

    #[test]
    fn overlapping_reference_speech_needs_both_speakers() {
        // both ref speakers talk 5-10 s; hypothesis only ever has one active
        let refs = [rt(0, 0, 10_000), rt(1, 5_000, 10_000)];
        let hyps = [turn("spk_0", 0, 10_000)];
        let (s, _) = score_der(&refs, 2, &hyps, 0);
        assert_eq!(s.ref_speech_ms, 15_000);
        assert_eq!(s.miss_ms, 5_000); // the second concurrent speaker
    }

    #[test]
    fn mapping_is_globally_optimal_not_greedy() {
        // spk_a covers ref0 60% / ref1 100% of their time; a greedy pass that
        // hands spk_a to ref0 first would strand ref1. Optimal: a→1, b→0.
        let refs = [rt(0, 0, 10_000), rt(1, 10_000, 16_000)];
        let hyps = [turn("spk_a", 4_000, 16_000), turn("spk_b", 0, 4_000)];
        let (s, mapping) = score_der(&refs, 2, &hyps, 0);
        assert_eq!(mapping[0].as_deref(), Some("spk_b"));
        assert_eq!(mapping[1].as_deref(), Some("spk_a"));
        assert_eq!(s.conf_ms, 6_000); // ref0's 4-10 s attributed to spk_a
    }
}
