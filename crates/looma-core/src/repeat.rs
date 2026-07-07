//! Consecutive n-gram repetition detection and collapse — the app-side guard
//! against ASR hallucination loops (a decoder stuck emitting one phrase for
//! minutes; seen 1881× in a real meeting). Engine-agnostic: operates on the
//! word stream, so whisper.cpp and cloud engines both pass through it.

use crate::Word;

/// Longest phrase length (in words) the loop detector looks for.
pub const MAX_PERIOD: usize = 12;

/// A detected consecutive repetition of one phrase.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopRun {
    /// Words per repetition unit.
    pub period: usize,
    /// How many times the unit occurs consecutively.
    pub reps: usize,
    /// The repeated phrase (normalized tokens joined with spaces).
    pub phrase: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Comparison token for loop detection: lowercase alphanumerics and
/// apostrophes, so "Boat." and "boat" repeat each other. A token with no
/// alphanumeric content (stray punctuation) keeps its trimmed text.
pub fn loop_token(text: &str) -> String {
    let norm: String = text
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .flat_map(char::to_lowercase)
        .collect();
    if norm.is_empty() {
        text.trim().to_string()
    } else {
        norm
    }
}

/// Longest consecutive run of any single n-gram in `tokens` (1 = no repeat).
pub fn max_consecutive_run(tokens: &[String], n: usize) -> usize {
    let mut best = 1;
    if n == 0 || tokens.len() < n {
        return best;
    }
    let mut i = 0;
    while i + n <= tokens.len() {
        let reps = run_length_at(tokens, i, n);
        best = best.max(reps);
        // A run of r units means no longer run of the same unit starts inside
        // it; skipping to its end is safe for the max and keeps this linear.
        i += if reps > 1 { (reps - 1) * n } else { 1 };
    }
    best
}

/// Consecutive occurrences of the n-gram starting at `i` (including itself).
fn run_length_at(tokens: &[String], i: usize, n: usize) -> usize {
    let mut reps = 1;
    let mut j = i + n;
    while j + n <= tokens.len() && tokens[j..j + n] == tokens[i..i + n] {
        reps += 1;
        j += n;
    }
    reps
}

/// Repeats at which a run of `period`-length phrases counts as a loop.
fn min_reps(period: usize) -> usize {
    if period == 1 {
        4
    } else {
        3
    }
}

/// Leftmost loop with the smallest period, so "a b a b a b a b" collapses as
/// period 2 rather than period 4. Returns (start index, period, reps).
fn first_loop(tokens: &[String]) -> Option<(usize, usize, usize)> {
    for n in 1..=MAX_PERIOD {
        for i in 0..tokens.len().saturating_sub(n * min_reps(n) - 1) {
            let reps = run_length_at(tokens, i, n);
            if reps >= min_reps(n) {
                return Some((i, n, reps));
            }
        }
    }
    None
}

/// Collapse hallucination loops in a word stream: a phrase of 2..=MAX_PERIOD
/// words repeated 3+ times consecutively keeps only its first occurrence; a
/// single word repeated 4+ times keeps two ("no no no" stays, a 50× "so"
/// stutter doesn't). Kept words keep their original timestamps. Returns the
/// surviving words and a report of every collapsed run.
pub fn collapse_loops(mut words: Vec<Word>) -> (Vec<Word>, Vec<LoopRun>) {
    let mut runs = Vec::new();
    loop {
        let tokens: Vec<String> = words.iter().map(|w| loop_token(&w.text)).collect();
        let Some((start, period, reps)) = first_loop(&tokens) else {
            break;
        };
        let keep = if period == 1 { 2 } else { 1 };
        let run_end = start + reps * period;
        runs.push(LoopRun {
            period,
            reps,
            phrase: tokens[start..start + period].join(" "),
            start_ms: words[start].start_ms,
            end_ms: words[run_end - 1].end_ms,
        });
        words.drain(start + keep * period..run_end);
    }
    (words, runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, at: u64) -> Word {
        Word {
            text: text.into(),
            start_ms: at,
            end_ms: at + 100,
        }
    }

    fn words(texts: &[&str]) -> Vec<Word> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| w(t, i as u64 * 200))
            .collect()
    }

    fn texts(words: &[Word]) -> Vec<&str> {
        words.iter().map(|w| w.text.as_str()).collect()
    }

    #[test]
    fn loop_token_normalizes_case_and_punctuation() {
        assert_eq!(loop_token("Boat."), "boat");
        assert_eq!(loop_token("I'm"), "i'm");
        assert_eq!(loop_token("  Test,"), "test");
        // pure punctuation keeps its text instead of collapsing to ""
        assert_eq!(loop_token(" ... "), "...");
    }

    #[test]
    fn max_run_counts_repeats() {
        let toks: Vec<String> = ["a", "b", "b", "b", "c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(max_consecutive_run(&toks, 1), 3);
        let toks: Vec<String> = ["x", "y", "x", "y", "x", "y", "z"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(max_consecutive_run(&toks, 2), 3);
        assert_eq!(max_consecutive_run(&toks, 3), 1);
    }

    #[test]
    fn clean_stream_is_untouched() {
        let input = words(&["thanks", "for", "joining", "the", "call", "today"]);
        let (out, runs) = collapse_loops(input.clone());
        assert_eq!(out, input);
        assert!(runs.is_empty());
    }

    #[test]
    fn phrase_repeated_three_times_keeps_first_occurrence() {
        let input = words(&[
            "so", "before", // prefix
            "I", "think", "we're", "in", "the", "boat.", // 1
            "i", "think", "we're", "in", "the", "boat", // 2 (case/punct differ)
            "I", "think", "we're", "in", "the", "boat.", // 3
            "anyway", // suffix
        ]);
        let (out, runs) = collapse_loops(input);
        assert_eq!(
            texts(&out),
            vec!["so", "before", "I", "think", "we're", "in", "the", "boat.", "anyway"]
        );
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].period, 6);
        assert_eq!(runs[0].reps, 3);
        assert_eq!(runs[0].phrase, "i think we're in the boat");
        // report spans the whole collapsed run in original time
        assert_eq!(runs[0].start_ms, 2 * 200);
        assert_eq!(runs[0].end_ms, 19 * 200 + 100);
    }

    #[test]
    fn kept_words_keep_their_timestamps() {
        let input = words(&["a", "b", "a", "b", "a", "b"]);
        let (out, _) = collapse_loops(input.clone());
        assert_eq!(out, input[..2].to_vec());
        assert_eq!(out[1].start_ms, 200);
    }

    #[test]
    fn genuine_double_phrase_is_untouched() {
        let input = words(&["i", "know,", "i", "know,", "right"]);
        let (out, runs) = collapse_loops(input.clone());
        assert_eq!(out, input);
        assert!(runs.is_empty());
    }

    #[test]
    fn triple_single_word_is_genuine_speech() {
        let input = words(&["no", "no", "no", "please"]);
        let (out, runs) = collapse_loops(input.clone());
        assert_eq!(out, input);
        assert!(runs.is_empty());
    }

    #[test]
    fn single_word_stutter_keeps_two() {
        let input = words(&["ok", "so", "so", "so", "so", "so", "then"]);
        let (out, runs) = collapse_loops(input);
        assert_eq!(texts(&out), vec!["ok", "so", "so", "then"]);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].period, 1);
        assert_eq!(runs[0].reps, 5);
    }

    #[test]
    fn minimal_period_wins_over_multiples() {
        // "a b" x4 must collapse as period 2, not period 4
        let input = words(&["a", "b", "a", "b", "a", "b", "a", "b"]);
        let (out, runs) = collapse_loops(input);
        assert_eq!(texts(&out), vec!["a", "b"]);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].period, 2);
        assert_eq!(runs[0].reps, 4);
    }

    #[test]
    fn two_separate_loops_both_collapse() {
        let mut input = words(&["x", "y", "x", "y", "x", "y"]); // loop 1
        let mut second = words(&["mid", "q", "r", "s", "q", "r", "s", "q", "r", "s", "end"]);
        for w in second.iter_mut() {
            w.start_ms += 10_000;
            w.end_ms += 10_000;
        }
        input.append(&mut second);
        let (out, runs) = collapse_loops(input);
        assert_eq!(texts(&out), vec!["x", "y", "mid", "q", "r", "s", "end"]);
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn giant_loop_collapses_fast() {
        // 8-word phrase x1881 with prefix/suffix — the real mic-channel bug
        let phrase = ["i", "think", "we're", "all", "in", "the", "same", "boat."];
        let mut texts_v: Vec<&str> = vec!["hello", "everyone"];
        for _ in 0..1881 {
            texts_v.extend_from_slice(&phrase);
        }
        texts_v.push("bye");
        let input = words(&texts_v);
        let (out, runs) = collapse_loops(input);
        assert_eq!(out.len(), 2 + 8 + 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].reps, 1881);
        assert_eq!(runs[0].period, 8);
    }
}
