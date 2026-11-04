//! Word-timing helpers for the bench-only engines (see bench_engines.rs).
//!
//! Two jobs:
//! - [`glue_sp_tokens`]: turn a transducer's SentencePiece token stream
//!   (sherpa-onnx / NeMo-style `tokens` + per-token start times) into words,
//!   the shape the pipeline's aligner expects.
//! - [`transfer_times`]: give an LLM decoder's untimed words (Granite
//!   Speech, any engine without timestamps) word times by aligning its text
//!   to a timestamped engine's words on the SAME audio chunk. Matched and
//!   substituted words take the anchor word's times; inserted words are
//!   spread over the gap between their timed neighbours by character length.
//!   This is timestamp transfer, not acoustic forced alignment: it can only
//!   be as good as the anchor engine's timing, and words neither engine
//!   agrees on get interpolated times. The harness reports which aligner
//!   produced the times next to every score that depends on them.

use fly_core::Word;

/// SentencePiece word-boundary marker.
const SP_SPACE: char = '\u{2581}';

/// Longest a single word may last when the engine gives no end time and
/// the next word is far away (a pause): keeps a trailing word from
/// swallowing the silence after it.
const MAX_WORD_MS: u64 = 1_200;

/// Glue SentencePiece tokens into words. A token starting with `▁` (or a
/// plain space — some exporters decode the marker) starts a new word.
/// `starts_ms[i]` is token i's start; `durations_ms` (TDT models) gives its
/// length when known. Without durations a word ends where the next word
/// starts, capped at `MAX_WORD_MS` past its last token, and the last word
/// ends one `frame_ms` after its last token.
pub fn glue_sp_tokens(
    tokens: &[String],
    starts_ms: &[u64],
    durations_ms: Option<&[u64]>,
    frame_ms: u64,
) -> Vec<Word> {
    struct Building {
        text: String,
        start_ms: u64,
        last_token_ms: u64,
        last_token_end_ms: Option<u64>,
    }
    let mut out: Vec<Building> = Vec::new();
    for (i, tok) in tokens.iter().enumerate() {
        let Some(&at) = starts_ms.get(i) else { break };
        let end = durations_ms.and_then(|d| d.get(i)).map(|d| at + d.max(&1));
        let new_word = tok.starts_with(SP_SPACE) || tok.starts_with(' ') || out.is_empty();
        let piece = tok.trim_start_matches([SP_SPACE, ' ']);
        if piece.is_empty() {
            // a bare boundary marker: the next token opens the word
            if tok.starts_with(SP_SPACE) || tok.starts_with(' ') {
                out.push(Building {
                    text: String::new(),
                    start_ms: at,
                    last_token_ms: at,
                    last_token_end_ms: end,
                });
            }
            continue;
        }
        match out.last_mut() {
            Some(w) if !new_word || w.text.is_empty() => {
                if w.text.is_empty() {
                    w.start_ms = w.start_ms.min(at);
                }
                w.text.push_str(piece);
                w.last_token_ms = at;
                w.last_token_end_ms = end.or(w.last_token_end_ms);
            }
            _ => out.push(Building {
                text: piece.to_string(),
                start_ms: at,
                last_token_ms: at,
                last_token_end_ms: end,
            }),
        }
    }
    out.retain(|w| !w.text.is_empty());
    let n = out.len();
    (0..n)
        .map(|i| {
            let w = &out[i];
            let next_start = out.get(i + 1).map(|x| x.start_ms);
            let end = match (w.last_token_end_ms, next_start) {
                (Some(e), _) => e,
                (None, Some(ns)) => ns.min(w.last_token_ms + MAX_WORD_MS),
                (None, None) => w.last_token_ms + frame_ms,
            };
            Word {
                text: w.text.clone(),
                start_ms: w.start_ms,
                end_ms: end.max(w.start_ms + 1),
            }
        })
        .collect()
}

/// Comparison key for aligning two engines' words: lowercase alphanumerics
/// only ("Okay," == "okay").
fn key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// How [`transfer_times`] timed the words (reported with the results).
#[derive(Debug, Default, Clone, Copy, PartialEq, serde::Serialize)]
pub struct TransferStats {
    /// Same word as the anchor at the aligned position.
    pub matched: usize,
    /// Different word at the aligned position (anchor times reused).
    pub substituted: usize,
    /// No anchor counterpart: time interpolated between neighbours.
    pub interpolated: usize,
}

/// Time `hyp` (untimed words, in order) from `anchors` (a timestamped
/// engine's words over the same audio, in order) by a minimum-edit
/// alignment of their keys. `span` = (start, end) of the audio the words
/// came from, used when there are no anchors around an inserted run.
pub fn transfer_times(
    hyp: &[String],
    anchors: &[Word],
    span: (u64, u64),
) -> (Vec<Word>, TransferStats) {
    let mut stats = TransferStats::default();
    if hyp.is_empty() {
        return (Vec::new(), stats);
    }
    let hk: Vec<String> = hyp.iter().map(|w| key(w)).collect();
    let ak: Vec<String> = anchors.iter().map(|w| key(&w.text)).collect();
    let (n, m) = (hk.len(), ak.len());
    // Levenshtein with a full back-pointer table; chunks are ≤ a few
    // hundred words, so n·m stays small.
    let mut cost = vec![vec![0u32; m + 1]; n + 1];
    for (i, row) in cost.iter_mut().enumerate() {
        row[0] = i as u32;
    }
    for (j, c) in cost[0].iter_mut().enumerate() {
        *c = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = cost[i - 1][j - 1] + u32::from(hk[i - 1] != ak[j - 1]);
            cost[i][j] = sub.min(cost[i - 1][j] + 1).min(cost[i][j - 1] + 1);
        }
    }
    // backtrace: hyp index → anchor index (match or substitution)
    let mut pair: Vec<Option<usize>> = vec![None; n];
    let mut exact: Vec<bool> = vec![false; n];
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        let same = hk[i - 1] == ak[j - 1];
        if cost[i][j] == cost[i - 1][j - 1] + u32::from(!same) {
            pair[i - 1] = Some(j - 1);
            exact[i - 1] = same;
            i -= 1;
            j -= 1;
        } else if cost[i][j] == cost[i - 1][j] + 1 {
            i -= 1;
        } else {
            j -= 1;
        }
    }

    let mut times: Vec<Option<(u64, u64)>> = pair
        .iter()
        .map(|p| p.map(|j| (anchors[j].start_ms, anchors[j].end_ms)))
        .collect();
    for k in 0..n {
        match (pair[k], exact[k]) {
            (Some(_), true) => stats.matched += 1,
            (Some(_), false) => stats.substituted += 1,
            (None, _) => stats.interpolated += 1,
        }
    }
    // anchors can overlap or run backwards after a substitution; keep the
    // timed words monotonic so interpolation gaps are well-formed
    let mut floor = span.0;
    for t in times.iter_mut().flatten() {
        t.0 = t.0.max(floor);
        t.1 = t.1.max(t.0 + 1);
        floor = t.0;
    }
    // interpolate each untimed run between its timed neighbours, by
    // character length (longer words take longer to say)
    let mut k = 0;
    while k < n {
        if times[k].is_some() {
            k += 1;
            continue;
        }
        let run_end = (k..n).find(|&x| times[x].is_some()).unwrap_or(n);
        let lo = if k == 0 {
            span.0
        } else {
            times[k - 1].map(|t| t.1).unwrap_or(span.0)
        };
        let hi = if run_end == n {
            span.1.max(lo + 1)
        } else {
            times[run_end].map(|t| t.0).unwrap_or(span.1)
        }
        .max(lo + 1);
        let weights: Vec<u64> = (k..run_end)
            .map(|x| hyp[x].chars().count().max(1) as u64)
            .collect();
        let total: u64 = weights.iter().sum();
        let mut at = lo;
        for (x, w) in (k..run_end).zip(weights) {
            let len = (hi - lo) * w / total.max(1);
            times[x] = Some((at, (at + len.max(1)).min(hi.max(at + 1))));
            at += len;
        }
        k = run_end;
    }
    let words = hyp
        .iter()
        .zip(times)
        .map(|(text, t)| {
            let (start_ms, end_ms) = t.expect("every word timed above");
            Word {
                text: text.clone(),
                start_ms,
                end_ms,
            }
        })
        .collect();
    (words, stats)
}

/// Split LLM output text into words the way whisper/parakeet emit them
/// (punctuation stays attached to its word).
pub fn split_text_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(str::to_string)
        .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, s: u64, e: u64) -> Word {
        Word {
            text: text.into(),
            start_ms: s,
            end_ms: e,
        }
    }

    fn toks(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sp_tokens_glue_into_words_with_attached_punctuation() {
        let t = toks(&["▁Hel", "lo", "▁wor", "ld", ".", "▁Yes"]);
        let s = [0, 80, 400, 480, 560, 2_000];
        let words = glue_sp_tokens(&t, &s, None, 80);
        let texts: Vec<_> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["Hello", "world.", "Yes"]);
        assert_eq!((words[0].start_ms, words[0].end_ms), (0, 400));
        // a long pause after "world." caps its end, not the next start
        assert_eq!(words[1].end_ms, 560 + MAX_WORD_MS);
        assert_eq!(words[2].end_ms, 2_080);
    }

    #[test]
    fn sp_tokens_use_durations_and_plain_space_markers() {
        let t = toks(&[" so", " we", "'re"]);
        let s = [100, 300, 400];
        let d = [120, 80, 80];
        let words = glue_sp_tokens(&t, &s, Some(&d), 80);
        assert_eq!(words.len(), 2);
        assert_eq!(words[1].text, "we're");
        assert_eq!((words[1].start_ms, words[1].end_ms), (300, 480));
    }

    #[test]
    fn bare_marker_token_opens_the_next_word() {
        let t = toks(&["▁", "OK", "▁go"]);
        let s = [0, 40, 200];
        let words = glue_sp_tokens(&t, &s, None, 80);
        let texts: Vec<_> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["OK", "go"]);
        assert_eq!(words[0].start_ms, 0);
    }

    #[test]
    fn transfer_copies_matched_and_substituted_times() {
        let anchors = vec![
            w("we", 0, 200),
            w("ship", 200, 500),
            w("the", 500, 600),
            w("build", 600, 900),
        ];
        let hyp = toks(&["We", "shipped", "the", "build."]);
        let (words, stats) = transfer_times(&hyp, &anchors, (0, 1_000));
        assert_eq!(
            stats,
            TransferStats {
                matched: 3,
                substituted: 1,
                interpolated: 0
            }
        );
        assert_eq!((words[1].start_ms, words[1].end_ms), (200, 500));
        assert_eq!(words[3].text, "build.");
        assert_eq!(words[3].start_ms, 600);
    }

    #[test]
    fn inserted_words_are_spread_between_neighbours() {
        let anchors = vec![w("hello", 0, 300), w("there", 1_000, 1_300)];
        let hyp = toks(&["hello", "and", "welcome", "there"]);
        let (words, stats) = transfer_times(&hyp, &anchors, (0, 2_000));
        assert_eq!(stats.interpolated, 2);
        assert!(words[1].start_ms >= 300 && words[2].end_ms <= 1_000);
        assert!(words[1].start_ms < words[2].start_ms);
        // "welcome" (7 chars) gets more time than "and" (3)
        assert!(words[2].end_ms - words[2].start_ms > words[1].end_ms - words[1].start_ms);
    }

    #[test]
    fn no_anchors_spreads_over_the_span() {
        let hyp = toks(&["a", "bb"]);
        let (words, stats) = transfer_times(&hyp, &[], (1_000, 1_300));
        assert_eq!(stats.interpolated, 2);
        assert_eq!(words[0].start_ms, 1_000);
        assert!(words[1].end_ms <= 1_300);
        assert!(words[0].end_ms <= words[1].start_ms);
    }

    #[test]
    fn llm_text_splits_into_words_without_bare_punctuation() {
        assert_eq!(
            split_text_words("Okay — so, we're  done ."),
            ["Okay", "so,", "we're", "done"]
        );
    }
}
