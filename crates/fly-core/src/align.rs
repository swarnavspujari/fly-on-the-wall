//! The word↔speaker aligner: merge ASR word timestamps with diarization
//! speaker turns so every word carries a speaker, then group consecutive
//! same-speaker words into transcript segments.
//!
//! This mapping is what makes provenance "zoom-in" exact, so it is pure,
//! deterministic, and unit-tested.

use crate::model::{SpeakerTurn, TranscriptSegment, Word};

#[derive(Debug, Clone)]
pub struct AlignOptions {
    /// Start a new segment when the gap between consecutive words of the same
    /// speaker exceeds this (natural pause → new paragraph).
    pub max_gap_ms: u64,
    /// Speaker key used for words that overlap no diarization turn at all
    /// (e.g. diarizer missed a short interjection).
    pub fallback_speaker: String,
}

impl Default for AlignOptions {
    fn default() -> Self {
        Self {
            max_gap_ms: 2_000,
            fallback_speaker: "spk_unknown".to_string(),
        }
    }
}

/// Assign each word the speaker whose turn overlaps it the most; when nothing
/// overlaps, fall back to the nearest turn within one second, else the
/// configured fallback speaker. Words are assumed sorted by start time.
pub fn align_words_to_speakers(
    words: &[Word],
    turns: &[SpeakerTurn],
    opts: &AlignOptions,
) -> Vec<TranscriptSegment> {
    let mut segments: Vec<TranscriptSegment> = Vec::new();

    for word in words {
        let speaker = speaker_for(word, turns, opts);

        let start_new = match segments.last() {
            None => true,
            Some(seg) => {
                seg.speaker_key != speaker
                    || word.start_ms.saturating_sub(seg.end_ms) > opts.max_gap_ms
            }
        };

        if start_new {
            segments.push(TranscriptSegment {
                id: crate::new_id(),
                speaker_key: speaker,
                start_ms: word.start_ms,
                end_ms: word.end_ms,
                text: word.text.clone(),
                words: vec![word.clone()],
            });
        } else {
            let seg = segments.last_mut().expect("checked above");
            seg.end_ms = seg.end_ms.max(word.end_ms);
            if !seg.text.is_empty() && !word.text.starts_with(|c: char| c.is_ascii_punctuation()) {
                seg.text.push(' ');
            }
            seg.text.push_str(&word.text);
            seg.words.push(word.clone());
        }
    }

    segments
}

/// Group a single known speaker's words into pause-separated segments —
/// used for the mic channel, where the speaker is you by construction
/// (spec §6.4) and no diarization is needed.
pub fn segments_from_single_speaker(
    words: &[Word],
    speaker_key: &str,
    opts: &AlignOptions,
) -> Vec<TranscriptSegment> {
    let all = [SpeakerTurn {
        speaker_key: speaker_key.to_string(),
        start_ms: 0,
        end_ms: words.last().map(|w| w.end_ms + 1).unwrap_or(1),
    }];
    align_words_to_speakers(words, &all, opts)
}

/// Interleave the per-channel segment lists (mic = you, system = them) into
/// one meeting timeline. Channels were recorded against the same pause-aware
/// clock, so start timestamps are directly comparable.
pub fn merge_channel_segments(mut channels: Vec<Vec<TranscriptSegment>>) -> Vec<TranscriptSegment> {
    let mut merged: Vec<TranscriptSegment> = channels.drain(..).flatten().collect();
    merged.sort_by_key(|s| (s.start_ms, s.end_ms));
    merged
}

fn speaker_for(word: &Word, turns: &[SpeakerTurn], opts: &AlignOptions) -> String {
    let mut best: Option<(&SpeakerTurn, u64)> = None;
    for turn in turns {
        let overlap_start = word.start_ms.max(turn.start_ms);
        let overlap_end = word.end_ms.min(turn.end_ms);
        if overlap_end > overlap_start {
            let overlap = overlap_end - overlap_start;
            if best.map(|(_, o)| overlap > o).unwrap_or(true) {
                best = Some((turn, overlap));
            }
        }
    }
    if let Some((turn, _)) = best {
        return turn.speaker_key.clone();
    }

    // No overlap: snap to the nearest turn if it is within 1s.
    let mid = (word.start_ms + word.end_ms) / 2;
    let mut nearest: Option<(&SpeakerTurn, u64)> = None;
    for turn in turns {
        let dist = if mid < turn.start_ms {
            turn.start_ms - mid
        } else {
            mid.saturating_sub(turn.end_ms)
        };
        if nearest.map(|(_, d)| dist < d).unwrap_or(true) {
            nearest = Some((turn, dist));
        }
    }
    match nearest {
        Some((turn, dist)) if dist <= 1_000 => turn.speaker_key.clone(),
        _ => opts.fallback_speaker.clone(),
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

    fn t(key: &str, start: u64, end: u64) -> SpeakerTurn {
        SpeakerTurn {
            speaker_key: key.into(),
            start_ms: start,
            end_ms: end,
        }
    }

    #[test]
    fn groups_consecutive_words_by_speaker() {
        let words = vec![
            w("hello", 0, 400),
            w("there", 450, 800),
            w("hi", 1200, 1400),
            w("back", 1450, 1700),
        ];
        let turns = vec![t("spk_0", 0, 1000), t("spk_1", 1100, 2000)];
        let segs = align_words_to_speakers(&words, &turns, &AlignOptions::default());
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].speaker_key, "spk_0");
        assert_eq!(segs[0].text, "hello there");
        assert_eq!(segs[1].speaker_key, "spk_1");
        assert_eq!(segs[1].text, "hi back");
        assert_eq!(segs[0].words.len(), 2);
    }

    #[test]
    fn word_straddling_two_turns_goes_to_larger_overlap() {
        // Word spans 900..1300; spk_0 covers 100ms of it, spk_1 covers 200ms.
        let words = vec![w("uh", 900, 1300)];
        let turns = vec![t("spk_0", 0, 1000), t("spk_1", 1100, 2000)];
        let segs = align_words_to_speakers(&words, &turns, &AlignOptions::default());
        assert_eq!(segs[0].speaker_key, "spk_1");
    }

    #[test]
    fn long_pause_splits_segment_even_for_same_speaker() {
        let words = vec![w("first", 0, 300), w("second", 5_000, 5_300)];
        let turns = vec![t("spk_0", 0, 6_000)];
        let segs = align_words_to_speakers(&words, &turns, &AlignOptions::default());
        assert_eq!(segs.len(), 2);
        assert!(segs.iter().all(|s| s.speaker_key == "spk_0"));
    }

    #[test]
    fn orphan_word_snaps_to_nearest_turn_within_1s() {
        let words = vec![w("yes", 2_100, 2_200)];
        let turns = vec![t("spk_0", 0, 2_000)];
        let segs = align_words_to_speakers(&words, &turns, &AlignOptions::default());
        assert_eq!(segs[0].speaker_key, "spk_0");
    }

    #[test]
    fn far_orphan_word_gets_fallback_speaker() {
        let words = vec![w("echo", 10_000, 10_200)];
        let turns = vec![t("spk_0", 0, 2_000)];
        let segs = align_words_to_speakers(&words, &turns, &AlignOptions::default());
        assert_eq!(segs[0].speaker_key, "spk_unknown");
    }

    #[test]
    fn empty_inputs_produce_empty_output() {
        let segs = align_words_to_speakers(&[], &[], &AlignOptions::default());
        assert!(segs.is_empty());
    }

    #[test]
    fn single_speaker_channel_segments_by_pause() {
        let words = vec![
            w("hello", 0, 300),
            w("there", 350, 600),
            w("again", 9_000, 9_300),
        ];
        let segs = segments_from_single_speaker(&words, "mic", &AlignOptions::default());
        assert_eq!(segs.len(), 2);
        assert!(segs.iter().all(|s| s.speaker_key == "mic"));
        assert_eq!(segs[0].text, "hello there");
        assert_eq!(segs[1].text, "again");
    }

    #[test]
    fn merge_interleaves_channels_by_time() {
        let mic = segments_from_single_speaker(
            &[w("question", 0, 500), w("answer follow-up", 4_000, 4_500)],
            "mic",
            &AlignOptions::default(),
        );
        let them = segments_from_single_speaker(
            &[w("reply", 1_000, 1_500)],
            "spk_0",
            &AlignOptions::default(),
        );
        let merged = merge_channel_segments(vec![mic, them]);
        let speakers: Vec<&str> = merged.iter().map(|s| s.speaker_key.as_str()).collect();
        assert_eq!(speakers, vec!["mic", "spk_0", "mic"]);
    }
}
