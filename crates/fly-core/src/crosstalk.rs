//! Cross-talk de-duplication for the two-channel (mic + system-loopback) path.
//!
//! On a headset-less recording the built-in mic naturally re-captures the
//! far-end voice played through the laptop speakers, so the mic channel
//! transcribes BOTH sides of the call (all labelled "You") and the same words
//! are transcribed again on the system loopback. Merged unchanged, every
//! far-end word is double-counted and mis-attributed.
//!
//! This module splits the two raw word streams into a clean "you" stream (the
//! near speaker) and a single "far" stream (the far-end), keeping each
//! duplicated far-end word exactly once from whichever channel is the better
//! source. The far stream is handed to the normal diarization aligner
//! afterwards, so the far-end still splits into its real speakers.
//!
//! Echo is detected as a *run* of the same words appearing on both channels
//! within a short window: genuine talk-over (DIFFERENT words on the two
//! channels at the same instant) has no cross-channel token match and survives
//! untouched.
//!
//! Known, bounded limitations (measured residual is small — it lives inside the
//! ~3% attribution error, so none warrant the complexity of real AEC yet):
//! - An ISOLATED far-end echo word (a run of 1, below `min_run`) is not removed,
//!   so a lone echoed backchannel can survive on both channels. `min_run` is a
//!   deliberate precision/recall trade — a run of 1 is usually a coincidence
//!   (both speakers said "yeah"), and dropping it would eat real "you" speech.
//! - The greedy 1:1 token matcher can leave one word of an echo run unmatched
//!   when the mic holds more copies of a token than the system within the
//!   window, splitting the run so a boundary word survives.
//! - A near speaker who mirrors 2+ of the far-end's exact words inside the
//!   window can be misread as echo (see `CrosstalkOptions::window_ms`).

use crate::repeat::loop_token;
use crate::Word;

#[derive(Debug, Clone)]
pub struct CrosstalkOptions {
    /// A mic word and a system word with the same token whose starts differ by
    /// at most this are echo candidates (the acoustic echo delay is small; the
    /// window is generous to absorb ASR timestamp jitter).
    pub window_ms: u64,
    /// Minimum length of a run of consecutive twinned mic words for it to count
    /// as echo. A run of 1 is treated as a coincidence (a shared backchannel
    /// like "yeah", or genuine talk-over of a common word) and left alone.
    pub min_run: usize,
    /// Which channel keeps the far-end copy of a duplicated run. `true` keeps
    /// the mic's copy (re-attributed to the far-end speaker) and drops the
    /// system copy; `false` keeps the system copy and drops the mic echo.
    pub keep_mic_far_end: bool,
    /// Safety gate: only de-duplicate when at least this fraction of the
    /// far-end (system) words are echoed into the mic. A headset recording has
    /// no acoustic echo, so cross-channel matches are rare and coincidental —
    /// below this floor the split is a no-op, protecting genuine simultaneous
    /// same-phrase speech ("I know", "exactly") from being deleted on clean
    /// recordings that never had the cross-talk problem in the first place.
    pub min_echo_ratio: f64,
}

impl Default for CrosstalkOptions {
    fn default() -> Self {
        // Production values, chosen by measurement against the human-verified
        // reference. `keep_mic_far_end: false` (keep the system copy) is the E1
        // Diagnostic-0 result: on the built-in-mic recording the system loopback
        // rendered the far-end's words with fewer substitutions than the
        // acoustic-echo copy in the mic. `window_ms: 1500` is wide because the
        // two channels are transcribed independently, so whisper assigns the
        // SAME echoed word timestamps that differ by up to ~1 s between channels
        // — a measured sweep showed tighter windows miss echo twins and roughly
        // halve the de-dup (merged WER 27% at 1500 ms vs 36% at 700 ms). The
        // cost of the wide window is that a near speaker who mirrors 2+ of the
        // far-end's exact words within it can be misread as echo; that residual
        // is bounded (it lives inside the ~3% attribution error). `min_echo_ratio`
        // skips clean/headset recordings, where any such match would be spurious.
        Self {
            window_ms: 1_500,
            min_run: 2,
            keep_mic_far_end: false,
            min_echo_ratio: 0.15,
        }
    }
}

/// The two de-duplicated word streams. `you_words` is the near speaker (→ the
/// "mic" key); `far_words` is every far-end word, each kept once, ready for the
/// diarization aligner.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelSplit {
    pub you_words: Vec<Word>,
    pub far_words: Vec<Word>,
}

/// Split the mic and system word streams into a near ("you") and far-end
/// stream with cross-talk echo removed. Words are assumed sorted by start time.
pub fn split_crosstalk(mic: &[Word], sys: &[Word], opts: &CrosstalkOptions) -> ChannelSplit {
    let mic_tok: Vec<String> = mic.iter().map(|w| loop_token(&w.text)).collect();
    let sys_tok: Vec<String> = sys.iter().map(|w| loop_token(&w.text)).collect();

    // 1. Greedy time-windowed token matching: pair each mic word with at most
    //    one, closest-in-time, unused system word carrying the same token.
    //    Both streams are sorted, so a single forward-sliding window is linear.
    let mut sys_used = vec![false; sys.len()];
    let mut mic_match: Vec<Option<usize>> = vec![None; mic.len()];
    let mut lo = 0usize;
    for (i, mw) in mic.iter().enumerate() {
        while lo < sys.len() && sys[lo].start_ms + opts.window_ms < mw.start_ms {
            lo += 1;
        }
        let mut best: Option<(usize, u64)> = None;
        let mut k = lo;
        while k < sys.len() && sys[k].start_ms <= mw.start_ms + opts.window_ms {
            if !sys_used[k] && sys_tok[k] == mic_tok[i] {
                let d = sys[k].start_ms.abs_diff(mw.start_ms);
                if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                    best = Some((k, d));
                }
            }
            k += 1;
        }
        if let Some((k, _)) = best {
            sys_used[k] = true;
            mic_match[i] = Some(k);
        }
    }

    // 2. Echo = a run of `min_run`+ CONSECUTIVE matched mic words. A lone match
    //    (a shared backchannel, a coincidental common word, talk-over) is not.
    let mut mic_echo = vec![false; mic.len()];
    let mut i = 0;
    while i < mic.len() {
        if mic_match[i].is_some() {
            let run_start = i;
            while i < mic.len() && mic_match[i].is_some() {
                i += 1;
            }
            if i - run_start >= opts.min_run {
                mic_echo[run_start..i].fill(true);
            }
        } else {
            i += 1;
        }
    }

    // 3. A system word is echo only if its partner mic word is an echo word.
    let mut sys_echo = vec![false; sys.len()];
    for (i, m) in mic_match.iter().enumerate() {
        if mic_echo[i] {
            sys_echo[m.expect("echo implies a match")] = true;
        }
    }

    // Safety gate: if barely any of the far-end is echoed into the mic, this is
    // a clean/headset recording with no cross-talk problem — the few matches are
    // coincidental (both speakers said "yeah"), so leave BOTH streams untouched
    // rather than risk deleting genuine simultaneous same-phrase speech.
    if !sys.is_empty() {
        let echoed = sys_echo.iter().filter(|&&e| e).count();
        if (echoed as f64) < opts.min_echo_ratio * sys.len() as f64 {
            return ChannelSplit {
                you_words: mic.to_vec(),
                far_words: sys.to_vec(),
            };
        }
    }

    // 4. Assemble. `you` is always the mic minus its echo. The far-end is
    //    sourced per the keep-direction: the mic's (cleaner-timed) echo copy
    //    plus any system word the mic never echoed, or the whole system channel.
    let you_words: Vec<Word> = mic
        .iter()
        .zip(&mic_echo)
        .filter(|(_, echo)| !**echo)
        .map(|(w, _)| w.clone())
        .collect();

    let mut far_words: Vec<Word> = if opts.keep_mic_far_end {
        let from_mic = mic
            .iter()
            .zip(&mic_echo)
            .filter(|(_, echo)| **echo)
            .map(|(w, _)| w.clone());
        let residual_sys = sys
            .iter()
            .zip(&sys_echo)
            .filter(|(_, echo)| !**echo)
            .map(|(w, _)| w.clone());
        from_mic.chain(residual_sys).collect()
    } else {
        sys.to_vec()
    };
    far_words.sort_by_key(|w| (w.start_ms, w.end_ms));

    ChannelSplit {
        you_words,
        far_words,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: u64, end: u64) -> Word {
        Word {
            text: text.into(),
            start_ms: start,
            end_ms: end,
        }
    }

    fn texts(ws: &[Word]) -> Vec<&str> {
        ws.iter().map(|x| x.text.as_str()).collect()
    }

    // Core-logic tests disable the echo-ratio gate (min_echo_ratio: 0.0) so they
    // exercise the matching/run logic directly; the gate has its own tests.
    fn opts(min_run: usize, keep_mic: bool) -> CrosstalkOptions {
        CrosstalkOptions {
            window_ms: 1_500,
            min_run,
            keep_mic_far_end: keep_mic,
            min_echo_ratio: 0.0,
        }
    }

    #[test]
    fn no_crosstalk_is_a_noop_in_both_directions() {
        // Disjoint vocabularies: nothing is echo, so you=mic and far=sys
        // regardless of keep-direction (the backward-compatible property).
        let mic = vec![w("hello", 0, 300), w("world", 350, 700)];
        let sys = vec![w("goodbye", 1_000, 1_300), w("now", 1_350, 1_700)];
        for keep_mic in [false, true] {
            let split = split_crosstalk(&mic, &sys, &opts(2, keep_mic));
            assert_eq!(texts(&split.you_words), vec!["hello", "world"]);
            assert_eq!(texts(&split.far_words), vec!["goodbye", "now"]);
        }
    }

    #[test]
    fn echo_run_keep_system_drops_mic_copy() {
        // Far-end "the quick fox" echoes into the mic alongside a You word.
        let mic = vec![
            w("hi", 0, 200),
            w("the", 1_050, 1_200),
            w("quick", 1_350, 1_500),
            w("fox", 1_650, 1_800),
        ];
        let sys = vec![
            w("the", 1_000, 1_150),
            w("quick", 1_300, 1_450),
            w("fox", 1_600, 1_750),
        ];
        let split = split_crosstalk(&mic, &sys, &opts(2, false));
        assert_eq!(texts(&split.you_words), vec!["hi"]);
        // far-end kept from the SYSTEM channel (system timestamps)
        assert_eq!(texts(&split.far_words), vec!["the", "quick", "fox"]);
        assert_eq!(split.far_words[0].start_ms, 1_000);
    }

    #[test]
    fn echo_run_keep_mic_keeps_mic_copy() {
        let mic = vec![
            w("hi", 0, 200),
            w("the", 1_050, 1_200),
            w("quick", 1_350, 1_500),
            w("fox", 1_650, 1_800),
        ];
        let sys = vec![
            w("the", 1_000, 1_150),
            w("quick", 1_300, 1_450),
            w("fox", 1_600, 1_750),
        ];
        let split = split_crosstalk(&mic, &sys, &opts(2, true));
        assert_eq!(texts(&split.you_words), vec!["hi"]);
        // far-end kept from the MIC channel (mic timestamps), system dropped
        assert_eq!(texts(&split.far_words), vec!["the", "quick", "fox"]);
        assert_eq!(split.far_words[0].start_ms, 1_050);
    }

    #[test]
    fn talk_over_survives() {
        // You says "yeah" (unique token) in the middle of a far-end echo run.
        let mic = vec![
            w("the", 1_050, 1_200),
            w("yeah", 1_260, 1_340),
            w("fox", 1_650, 1_800),
        ];
        let sys = vec![w("the", 1_000, 1_150), w("fox", 1_600, 1_750)];
        // "the" and "fox" are matched but the run is broken by "yeah", so with
        // min_run=2 neither reaches a run of 2 → nothing is echo here.
        let split = split_crosstalk(&mic, &sys, &opts(2, false));
        assert!(
            texts(&split.you_words).contains(&"yeah"),
            "the interjection must survive as You"
        );
    }

    #[test]
    fn keep_mic_reattributes_run_and_keeps_system_residual() {
        // mic echoes "the" and "fox" (a consecutive run); the far-end also said
        // "quick", which only the system captured.
        let mic = vec![
            w("hello", 0, 300),
            w("the", 1_000, 1_150),
            w("fox", 1_600, 1_750),
        ];
        let sys = vec![
            w("the", 1_000, 1_150),
            w("quick", 1_300, 1_450),
            w("fox", 1_600, 1_750),
        ];
        let split = split_crosstalk(&mic, &sys, &opts(2, true));
        assert_eq!(texts(&split.you_words), vec!["hello"]);
        // far = mic's the+fox (kept) merged with the system-only "quick"
        assert_eq!(texts(&split.far_words), vec!["the", "quick", "fox"]);
    }

    #[test]
    fn min_run_2_keeps_a_lone_coincidental_match() {
        // A single shared backchannel "okay" is not a run → left as You, and
        // the system copy also survives (min_run>1 accepts this to protect
        // genuine coincidental matches).
        let mic = vec![w("okay", 1_000, 1_200)];
        let sys = vec![w("okay", 1_100, 1_300)];
        let split = split_crosstalk(&mic, &sys, &opts(2, false));
        assert_eq!(texts(&split.you_words), vec!["okay"]);
        assert_eq!(texts(&split.far_words), vec!["okay"]);
    }

    #[test]
    fn min_run_1_dedups_even_a_lone_match() {
        let mic = vec![w("okay", 1_000, 1_200)];
        let sys = vec![w("okay", 1_100, 1_300)];
        let split = split_crosstalk(&mic, &sys, &opts(1, false));
        assert!(split.you_words.is_empty());
        assert_eq!(texts(&split.far_words), vec!["okay"]);
    }

    #[test]
    fn outside_window_is_not_a_match() {
        let mic = vec![w("report", 0, 300), w("budget", 500, 800)];
        let sys = vec![w("report", 5_000, 5_300), w("budget", 5_500, 5_800)];
        let split = split_crosstalk(&mic, &sys, &opts(2, false));
        // 5 s apart → not echo → both channels keep their words
        assert_eq!(texts(&split.you_words), vec!["report", "budget"]);
        assert_eq!(texts(&split.far_words), vec!["report", "budget"]);
    }

    #[test]
    fn far_words_are_sorted_by_start() {
        let mic = vec![
            w("a", 0, 100),
            w("one", 1_000, 1_100),
            w("three", 3_000, 3_100),
        ];
        let sys = vec![
            w("one", 1_000, 1_100),
            w("two", 2_000, 2_100),
            w("three", 3_000, 3_100),
        ];
        let split = split_crosstalk(&mic, &sys, &opts(2, true));
        let starts: Vec<u64> = split.far_words.iter().map(|w| w.start_ms).collect();
        assert!(
            starts.windows(2).all(|p| p[0] <= p[1]),
            "far_words must be sorted"
        );
    }

    #[test]
    fn empty_inputs_produce_empty_output() {
        let split = split_crosstalk(&[], &[], &CrosstalkOptions::default());
        assert!(split.you_words.is_empty() && split.far_words.is_empty());
    }

    #[test]
    fn low_echo_recording_is_left_untouched_by_the_gate() {
        // A near-clean (headset-like) recording: 20 far-end words, only a lone
        // 2-word coincidence echoed. 2/20 = 0.10 < the 0.15 default floor, so
        // the split must be a NO-OP — no genuine You words are ever deleted.
        let sys: Vec<Word> = (0..20u64)
            .map(|i| w(&format!("s{i}"), i * 1_000, i * 1_000 + 300))
            .collect();
        let mic = vec![
            w("hello", 0, 300),
            w("there", 400, 700),
            w("s3", 3_050, 3_300),
            w("s4", 4_050, 4_300),
            w("bye", 9_000, 9_300),
        ];
        let split = split_crosstalk(&mic, &sys, &CrosstalkOptions::default());
        assert_eq!(split.you_words.len(), mic.len(), "no You words dropped");
        assert_eq!(split.far_words.len(), sys.len(), "system left intact");
    }

    #[test]
    fn high_echo_recording_is_deduped_under_the_default_gate() {
        // Whole far-end echoed (ratio 1.0 >= 0.15): the gate lets de-dup run.
        let mic = vec![
            w("hi", 0, 200),
            w("the", 1_050, 1_200),
            w("quick", 1_350, 1_500),
            w("fox", 1_650, 1_800),
        ];
        let sys = vec![
            w("the", 1_000, 1_150),
            w("quick", 1_300, 1_450),
            w("fox", 1_600, 1_750),
        ];
        let split = split_crosstalk(&mic, &sys, &CrosstalkOptions::default());
        assert_eq!(texts(&split.you_words), vec!["hi"]);
        assert_eq!(texts(&split.far_words), vec!["the", "quick", "fox"]);
    }

    #[test]
    fn tokens_match_case_and_punctuation_insensitively() {
        let mic = vec![w("The", 1_050, 1_200), w("Fox.", 1_650, 1_800)];
        let sys = vec![w("the", 1_000, 1_150), w("fox", 1_600, 1_750)];
        let split = split_crosstalk(&mic, &sys, &opts(2, false));
        assert!(
            split.you_words.is_empty(),
            "case/punct differences still echo"
        );
        assert_eq!(texts(&split.far_words), vec!["the", "fox"]);
    }
}
