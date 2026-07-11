//! Offline accuracy harness for real recordings: runs the REAL per-channel
//! pipeline (whisper.cpp + sherpa-onnx) over a meeting's WAVs and reports the
//! trust metrics — consecutive-repetition runs (hallucination loops), distinct
//! speaker count, and word counts. `#[ignore]`d: heavy, needs artifacts and a
//! recording on disk.
//!
//! Score an already-exported transcript JSON (no pipeline run):
//!   LOOMA_HARNESS_SCORE_JSON=path\to\meeting.json \
//!     cargo test -p looma-app --test accuracy_harness -- --ignored --nocapture
//!
//! Run the pipeline over a recording folder (recording.mic.wav +
//! recording.system.wav), optionally trimmed for fast iteration:
//!   LOOMA_HARNESS_DIR=path\to\recording-folder \
//!   LOOMA_HARNESS_MODEL=ggml-large-v3-turbo-q5_0 \
//!   LOOMA_HARNESS_MAX_SECS=300 \
//!     cargo test -p looma-app --test accuracy_harness -- --ignored --nocapture
//!
//! Score against a human-grade reference transcript (Fathom .txt export) —
//! adds per-channel WER, speaker-attributed WER, attribution error, and the
//! cross-talk duplication rate to any of the modes above:
//!   LOOMA_HARNESS_REFERENCE=path\to\fathom-export.txt \
//!   LOOMA_HARNESS_REF_SELF=Swarnav          # substring of the ref speaker who is "You"
//!   LOOMA_HARNESS_XTALK_MS=500              # dup window (default 500)
//!
//! Cloud-reference mode (diagnostic only, no local pipeline): transcribe the
//! mic and system channels with Groq to separate model quality from audio
//! quality. The key comes from the environment ONLY — never a file/setting:
//!   GROQ_API_KEY=... LOOMA_HARNESS_GROQ=1 LOOMA_HARNESS_DIR=... \
//!   LOOMA_HARNESS_GROQ_MODEL=whisper-large-v3 \
//!   LOOMA_HARNESS_GROQ_CACHE=path\to\cache-dir   # optional per-chunk response cache
//!     cargo test -p looma-app --test accuracy_harness -- --ignored --nocapture

use std::path::Path;

use looma_core::repeat::loop_token;
use looma_core::{RecordingRef, Transcript};

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

fn report(t: &Transcript) {
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

    let metrics = serde_json::json!({
        "segments": t.segments.len(),
        "speakers_listed": t.speakers.len(),
        "speaker_keys": keys.len(),
        "system_speakers": non_unknown,
        "words_total": mic.len() + system.len(),
        "words_mic": mic.len(),
        "words_system": system.len(),
        "worst_reps_mic": worst["worst_reps_mic"],
        "worst_reps_system": worst["worst_reps_system"],
    });
    eprintln!("HARNESS_METRICS_JSON: {metrics}");
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

/// The reference transcript: interned speaker names + one flat token stream.
struct Reference {
    speakers: Vec<String>,
    /// (normalized token, speaker index)
    tokens: Vec<(String, usize)>,
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
    Reference { speakers, tokens }
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

/// Pick the reference speaker who is "You": LOOMA_HARNESS_REF_SELF substring
/// override, else the ref speaker whose words are best covered by the mic
/// channel (best-effort — echo makes both sides match the mic, so prefer the
/// explicit override on echo-suspect recordings).
fn detect_self(reference: &Reference, mic: &[HypWord]) -> usize {
    if let Ok(hint) = std::env::var("LOOMA_HARNESS_REF_SELF") {
        let hint = hint.to_ascii_lowercase();
        if let Some(idx) = reference
            .speakers
            .iter()
            .position(|s| s.to_ascii_lowercase().contains(&hint))
        {
            return idx;
        }
        panic!("LOOMA_HARNESS_REF_SELF={hint:?} matches no reference speaker");
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

/// Full reference scoring block: per-channel WER, merged + speaker-attributed
/// WER, attribution error, cross-talk duplication, per-channel bleed.
fn score_against_reference(t: &Transcript, ref_path: &str) {
    let reference = parse_fathom_reference(ref_path);
    let mic = hyp_words(t, |k| k == "mic");
    let system = hyp_words(t, |k| k != "mic");
    let merged = hyp_words(t, |_| true);

    let self_idx = detect_self(&reference, &mic);
    let ref_you: Vec<(String, usize)> = reference
        .tokens
        .iter()
        .filter(|(_, s)| *s == self_idx)
        .cloned()
        .collect();
    let ref_others: Vec<(String, usize)> = reference
        .tokens
        .iter()
        .filter(|(_, s)| *s != self_idx)
        .cloned()
        .collect();

    eprintln!("== reference scoring vs {ref_path} ==");
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
    let sys_vs_all = score_channel(&reference.tokens, &system);
    eprintln!("mic    vs ref[you]:    {}", mic_vs_you.line());
    eprintln!("mic    vs ref[others]: {}", mic_vs_others.line());
    eprintln!("mic    vs ref[all]:    {}", mic_vs_all.line());
    eprintln!("system vs ref[others]: {}", sys_vs_others.line());
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
        let best = if *key == "mic" {
            self_idx
        } else {
            counts
                .iter()
                .enumerate()
                .max_by_key(|(_, c)| **c)
                .map(|(i, _)| i)
                .unwrap_or(0)
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
    let window_ms: u64 = std::env::var("LOOMA_HARNESS_XTALK_MS")
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

    let metrics = serde_json::json!({
        "ref_words": n,
        "ref_words_you": ref_you.len(),
        "wer_mic_vs_you": mic_vs_you.wer(),
        "wer_mic_vs_others": mic_vs_others.wer(),
        "wer_mic_vs_all": mic_vs_all.wer(),
        "wer_system_vs_others": sys_vs_others.wer(),
        "wer_system_vs_all": sys_vs_all.wer(),
        "wer_merged": merged_wer.wer(),
        "sa_wer_merged": sa_wer,
        "attribution_error": attr_err,
        "xtalk_dup_windowed": dup_windowed as f64 / n as f64,
        "xtalk_dup_any": dup_any as f64 / n as f64,
        "bleed_others_on_mic": others_on_mic as f64 / ref_others.len().max(1) as f64,
        "bleed_you_on_system": you_on_sys as f64 / ref_you.len().max(1) as f64,
        "xtalk_window_ms": window_ms,
    });
    eprintln!("HARNESS_REFERENCE_METRICS_JSON: {metrics}");
}

fn maybe_score_reference(t: &Transcript) {
    if let Ok(ref_path) = std::env::var("LOOMA_HARNESS_REFERENCE") {
        score_against_reference(t, &ref_path);
    }
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
) -> Vec<looma_core::Word> {
    let (samples, rate) = looma_audio::mix::read_wav_mono(src).expect("read channel wav");
    let samples = looma_audio::mix::resample_linear(&samples, rate, 16_000);
    let take = max_secs
        .map(|s| ((s * 16_000) as usize).min(samples.len()))
        .unwrap_or(samples.len());
    let samples = &samples[..take];
    let total_ms = samples.len() as u64 / 16; // 16 samples per ms at 16 kHz
    let cache_dir = std::env::var("LOOMA_HARNESS_GROQ_CACHE").ok().map(|d| {
        let d = std::path::PathBuf::from(d);
        std::fs::create_dir_all(&d).expect("create groq cache dir");
        d
    });

    let mut words: Vec<looma_core::Word> = Vec::new();
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
                looma_audio::mix::write_wav_mono_16(&wav, &samples[a..b], 16_000)
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
        let raw = looma_asr::groq::parse_groq_verbose_json(&body).expect("parse groq response");
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
        .expect("LOOMA_HARNESS_GROQ is set but GROQ_API_KEY is not in the environment");
    let model =
        std::env::var("LOOMA_HARNESS_GROQ_MODEL").unwrap_or_else(|_| "whisper-large-v3".into());
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

    let align_opts = looma_core::AlignOptions::default();
    let mut segments =
        looma_core::align::segments_from_single_speaker(&mic_words, "mic", &align_opts);
    segments.extend(looma_core::align::segments_from_single_speaker(
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
            looma_core::Speaker {
                key: "mic".into(),
                label: "You".into(),
            },
            looma_core::Speaker {
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
            let (samples, rate) =
                looma_audio::mix::read_wav_mono(dst).map_err(|e| e.to_string())?;
            Ok(samples.len() as u64 * 1000 / rate as u64)
        }
        Some(secs) => {
            let (samples, rate) =
                looma_audio::mix::read_wav_mono(src).map_err(|e| e.to_string())?;
            let take = (secs * rate as u64).min(samples.len() as u64) as usize;
            looma_audio::mix::write_wav_mono_16(dst, &samples[..take], rate)
                .map_err(|e| e.to_string())?;
            Ok(take as u64 * 1000 / rate as u64)
        }
    }
}

#[test]
#[ignore = "offline accuracy harness; needs artifacts + a recording, see file docs"]
fn accuracy_harness() {
    // surface pipeline logs (e.g. collapsed-loop warnings) on stderr
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "looma_app_lib=debug,looma_asr=debug".into()),
        )
        .try_init();

    // ---- score-only mode: metrics for an exported transcript JSON ----
    if let Ok(json_path) = std::env::var("LOOMA_HARNESS_SCORE_JSON") {
        let raw = std::fs::read_to_string(&json_path).expect("read score json");
        let t: Transcript = serde_json::from_str(&raw).expect("parse transcript json");
        report(&t);
        maybe_score_reference(&t);
        return;
    }

    let Ok(rec_dir) = std::env::var("LOOMA_HARNESS_DIR") else {
        eprintln!("SKIP: set LOOMA_HARNESS_DIR or LOOMA_HARNESS_SCORE_JSON");
        return;
    };
    let rec_dir = std::path::PathBuf::from(rec_dir);
    let mic_src = rec_dir.join("recording.mic.wav");
    let sys_src = rec_dir.join("recording.system.wav");
    assert!(mic_src.exists(), "missing {}", mic_src.display());
    assert!(sys_src.exists(), "missing {}", sys_src.display());

    let model =
        std::env::var("LOOMA_HARNESS_MODEL").unwrap_or_else(|_| "ggml-large-v3-turbo-q5_0".into());
    let max_secs = std::env::var("LOOMA_HARNESS_MAX_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());

    // ---- Groq cloud-reference mode: no local pipeline, both channels ----
    if std::env::var("LOOMA_HARNESS_GROQ").is_ok_and(|v| !v.is_empty() && v != "0") {
        let t = groq_reference_transcript(&rec_dir, max_secs);
        if let Ok(out) = std::env::var("LOOMA_HARNESS_OUT_JSON") {
            std::fs::write(&out, serde_json::to_string_pretty(&t).unwrap()).unwrap();
            eprintln!("transcript written to {out}");
        }
        report(&t);
        maybe_score_reference(&t);
        return;
    }

    // ---- artifacts, hardlinked from the real data dir like the golden E2E ----
    let real_data = dirs::data_dir().unwrap().join("FlyOnTheWall");
    let needed = [
        "bin/whisper/Release/whisper-cli.exe".to_string(),
        "bin/sherpa/sherpa-onnx-v1.13.3-win-x64-shared-MD-Release/bin/sherpa-onnx-offline-speaker-diarization.exe".to_string(),
        "models/diarize/sherpa-onnx-pyannote-segmentation-3-0/model.onnx".to_string(),
        "models/diarize/campplus.onnx".to_string(),
        format!("models/asr/{model}.bin"),
    ];
    if let Some(missing) = needed.iter().find(|p| !real_data.join(p).exists()) {
        panic!(
            "artifact not installed: {}",
            real_data.join(missing).display()
        );
    }

    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    for sub in ["bin/whisper", "bin/sherpa", "models/diarize"] {
        link_tree(&real_data.join(sub), &data_dir.join(sub)).unwrap();
    }
    // GPU modes need the Vulkan build in place (no download inside the test)
    if std::env::var("LOOMA_HARNESS_GPU").is_ok_and(|v| !v.is_empty() && v != "0") {
        let vulkan = real_data.join("bin/whisper-vulkan");
        assert!(
            vulkan.join("Release/whisper-cli.exe").exists(),
            "LOOMA_HARNESS_GPU set but whisper-bin-vulkan is not installed"
        );
        link_tree(&vulkan, &data_dir.join("bin/whisper-vulkan")).unwrap();
    }
    std::fs::create_dir_all(data_dir.join("models/asr")).unwrap();
    std::fs::hard_link(
        real_data.join(format!("models/asr/{model}.bin")),
        data_dir.join(format!("models/asr/{model}.bin")),
    )
    .unwrap();

    let state = looma_app_lib::state::AppState::init_with(
        data_dir.clone(),
        std::sync::Arc::new(looma_secrets::MemorySecretStore::default()),
    )
    .unwrap();

    let meeting_id = {
        let storage = state.storage.lock().unwrap();
        storage.set_setting("asr.tier", "light").unwrap();
        storage.set_setting("asr.model_id", &model).unwrap();
        // Deterministic engine selection: CPU by default. LOOMA_HARNESS_GPU=1
        // forces the Vulkan build (verdict pre-seeded so no benchmark runs;
        // pick the GPU with GGML_VK_VISIBLE_DEVICES). LOOMA_HARNESS_GPU=bench
        // enables GPU with NO verdict, exercising the real in-pipeline
        // benchmark + gate exactly as a user's machine would.
        match std::env::var("LOOMA_HARNESS_GPU").ok().as_deref() {
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
        let dur_mic =
            stage_channel(&mic_src, &meet_dir.join("recording.mic.wav"), max_secs).unwrap();
        let dur_sys =
            stage_channel(&sys_src, &meet_dir.join("recording.system.wav"), max_secs).unwrap();
        storage
            .end_meeting(
                &meeting.id,
                &RecordingRef {
                    mic_path: Some(format!("recordings/{}/recording.mic.wav", meeting.id)),
                    system_path: Some(format!("recordings/{}/recording.system.wav", meeting.id)),
                    mixed_path: None,
                    duration_ms: dur_mic.max(dur_sys),
                },
            )
            .unwrap();
        meeting.id
    };

    let started = std::time::Instant::now();
    let on_stage = |p: looma_app_lib::pipeline::PipelineProgress| {
        eprintln!(
            "[{:>6.1}s] stage: {} {}",
            started.elapsed().as_secs_f32(),
            p.stage,
            p.detail.unwrap_or_default()
        )
    };
    let on_model = |p: looma_app_lib::models::ModelProgress| eprintln!("model: {}", p.stage);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let transcript = runtime
        .block_on(looma_app_lib::pipeline::run_with(
            &state,
            &on_stage,
            &on_model,
            &meeting_id,
        ))
        .expect("pipeline should succeed");
    eprintln!("pipeline took {:.1}s", started.elapsed().as_secs_f32());

    // keep the produced transcript for spot-checking against the audio
    if let Ok(out) = std::env::var("LOOMA_HARNESS_OUT_JSON") {
        std::fs::write(&out, serde_json::to_string_pretty(&transcript).unwrap()).unwrap();
        eprintln!("transcript written to {out}");
    }

    report(&transcript);
    maybe_score_reference(&transcript);
}
