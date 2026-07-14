//! Re-diarization: map a fresh set of diarization turns onto an EXISTING
//! transcript without touching its text.
//!
//! Segments keep their ids, boundaries, words, and text — only the
//! per-segment `speaker_key` is reassigned. That invariant is what lets the
//! polished (cleaned) variant survive a re-diarize: raw and cleaned share
//! segment ids, so both get the same key updates and neither loses a word.
//! Note-block and meeting-item provenance citations keep resolving for the
//! same reason.

use std::collections::HashMap;

use crate::model::{Speaker, SpeakerTurn, Transcript, TranscriptSegment};

/// Result of reassigning segments to new speaker turns.
pub struct Reassignment {
    /// Old key → new key per segment id (only segments that changed).
    pub changed: Vec<String>,
    /// The rebuilt speaker list (labels carried over where stable).
    pub speakers: Vec<Speaker>,
}

/// Fallback key for segments overlapping no turn (matches align.rs).
pub const UNKNOWN_KEY: &str = "spk_unknown";

/// Reassign every segment's speaker key from `turns`, in place.
/// Segments whose key equals `protected_key` (the pre-labeled mic channel)
/// are never touched. Returns the ids of segments whose key changed.
pub fn reassign_segment_speakers(
    segments: &mut [TranscriptSegment],
    turns: &[SpeakerTurn],
    protected_key: Option<&str>,
) -> Vec<String> {
    let mut changed = Vec::new();
    for seg in segments.iter_mut() {
        if protected_key.is_some_and(|k| seg.speaker_key == k) {
            continue;
        }
        let new_key = speaker_for_span(seg.start_ms, seg.end_ms, turns);
        if new_key != seg.speaker_key {
            changed.push(seg.id.clone());
            seg.speaker_key = new_key;
        }
    }
    changed
}

/// The turn speaker overlapping `[start, end)` the most; when nothing
/// overlaps, the nearest turn within one second, else the unknown fallback —
/// the same policy the word aligner uses.
fn speaker_for_span(start_ms: u64, end_ms: u64, turns: &[SpeakerTurn]) -> String {
    let mut totals: HashMap<&str, u64> = HashMap::new();
    for t in turns {
        let s = start_ms.max(t.start_ms);
        let e = end_ms.min(t.end_ms);
        if e > s {
            *totals.entry(t.speaker_key.as_str()).or_default() += e - s;
        }
    }
    if let Some((key, _)) = totals
        .into_iter()
        .max_by_key(|&(key, overlap)| (overlap, std::cmp::Reverse(key.to_string())))
    {
        return key.to_string();
    }
    let mid = (start_ms + end_ms) / 2;
    turns
        .iter()
        .map(|t| {
            // distance from mid to the turn's interval (0 when inside it)
            let dist = t
                .start_ms
                .saturating_sub(mid)
                .max(mid.saturating_sub(t.end_ms));
            (dist, t)
        })
        .min_by_key(|(dist, _)| *dist)
        .filter(|(dist, _)| *dist <= 1000)
        .map(|(_, t)| t.speaker_key.clone())
        .unwrap_or_else(|| UNKNOWN_KEY.to_string())
}

/// A label the app generated ("Speaker 3", "Unknown", or the raw key) —
/// anything else is a user assignment worth preserving.
pub fn is_generic_label(key: &str, label: &str) -> bool {
    label == key
        || label == "Unknown"
        || label
            .strip_prefix("Speaker ")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Fraction of a cluster's duration that must map onto a single counterpart
/// (in both directions) for its identity to count as stable across the
/// re-diarize.
const STABLE_SHARE: f64 = 0.8;

/// Rebuild the speaker list after reassignment.
///
/// `old_keys` is segment id → speaker key BEFORE reassignment; `segments`
/// carry the new keys. A custom label (user assignment/rename) is carried to
/// a new key only when the old and new clusters map onto each other with
/// ≥ 80% of their duration in both directions — anything murkier is CLEARLY
/// reset to a generic "Speaker N" so the user re-assigns it deliberately.
/// `protected` keys (mic) keep their old label verbatim.
pub fn rebuild_speakers(
    old_keys: &HashMap<String, String>,
    old_speakers: &[Speaker],
    segments: &[TranscriptSegment],
    protected_key: Option<&str>,
) -> Vec<Speaker> {
    // duration matrices old→new and new→old
    let mut old_to_new: HashMap<&str, HashMap<&str, u64>> = HashMap::new();
    let mut new_to_old: HashMap<&str, HashMap<&str, u64>> = HashMap::new();
    for seg in segments {
        let Some(old) = old_keys.get(&seg.id) else {
            continue;
        };
        let dur = seg.end_ms.saturating_sub(seg.start_ms).max(1);
        *old_to_new
            .entry(old.as_str())
            .or_default()
            .entry(seg.speaker_key.as_str())
            .or_default() += dur;
        *new_to_old
            .entry(seg.speaker_key.as_str())
            .or_default()
            .entry(old.as_str())
            .or_default() += dur;
    }
    let dominant = |m: &HashMap<&str, u64>| -> Option<(String, f64)> {
        let total: u64 = m.values().sum();
        let (k, v) = m
            .iter()
            .max_by_key(|&(k, v)| (*v, std::cmp::Reverse(k.to_string())))?;
        Some(((*k).to_string(), *v as f64 / total.max(1) as f64))
    };

    let old_label = |key: &str| -> Option<&str> {
        old_speakers
            .iter()
            .find(|s| s.key == key)
            .map(|s| s.label.as_str())
    };

    let mut speakers: Vec<Speaker> = Vec::new();
    if let Some(pk) = protected_key {
        if segments.iter().any(|s| s.speaker_key == pk) || old_label(pk).is_some() {
            speakers.push(Speaker {
                key: pk.to_string(),
                label: old_label(pk).unwrap_or("You").to_string(),
            });
        }
    }

    let mut next = 1;
    for seg in segments {
        let key = seg.speaker_key.as_str();
        if speakers.iter().any(|s| s.key == key) {
            continue;
        }
        // Stable identity: the new cluster is dominated by one old cluster
        // AND that old cluster is dominated by this new one, both ≥ 80%.
        let carried: Option<String> = new_to_old.get(key).and_then(|m| {
            let (old, share_back) = dominant(m)?;
            if share_back < STABLE_SHARE {
                return None;
            }
            let (fwd, share_fwd) = dominant(old_to_new.get(old.as_str())?)?;
            if fwd != key || share_fwd < STABLE_SHARE {
                return None;
            }
            let label = old_label(&old)?;
            (!is_generic_label(&old, label)).then(|| label.to_string())
        });
        let label = match carried {
            Some(label) => label,
            None if key == UNKNOWN_KEY => "Unknown".to_string(),
            None => {
                let l = format!("Speaker {next}");
                next += 1;
                l
            }
        };
        speakers.push(Speaker {
            key: key.to_string(),
            label,
        });
    }
    speakers
}

/// Apply new turns to a transcript: reassign segment keys (mic protected) and
/// rebuild the speaker list, preserving stable assignments. `turns` may be
/// empty for the "just you" case handled by the caller.
pub fn apply_turns(
    transcript: &mut Transcript,
    turns: &[SpeakerTurn],
    protected_key: Option<&str>,
) -> Reassignment {
    let old_keys: HashMap<String, String> = transcript
        .segments
        .iter()
        .map(|s| (s.id.clone(), s.speaker_key.clone()))
        .collect();
    let old_speakers = transcript.speakers.clone();
    let changed = reassign_segment_speakers(&mut transcript.segments, turns, protected_key);
    let speakers = rebuild_speakers(
        &old_keys,
        &old_speakers,
        &transcript.segments,
        protected_key,
    );
    transcript.speakers = speakers.clone();
    Reassignment { changed, speakers }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(id: &str, key: &str, start_s: u64, end_s: u64) -> TranscriptSegment {
        TranscriptSegment {
            id: id.into(),
            speaker_key: key.into(),
            start_ms: start_s * 1000,
            end_ms: end_s * 1000,
            text: format!("text {id}"),
            words: vec![],
        }
    }

    fn turn(key: &str, start_s: u64, end_s: u64) -> SpeakerTurn {
        SpeakerTurn {
            speaker_key: key.into(),
            start_ms: start_s * 1000,
            end_ms: end_s * 1000,
        }
    }

    fn spk(key: &str, label: &str) -> Speaker {
        Speaker {
            key: key.into(),
            label: label.into(),
        }
    }

    #[test]
    fn segments_keep_ids_text_and_boundaries() {
        let mut t = Transcript {
            meeting_id: "m".into(),
            language: None,
            engine: "e".into(),
            segments: vec![seg("a", "spk_0", 0, 10), seg("b", "spk_1", 10, 20)],
            speakers: vec![spk("spk_0", "Speaker 1"), spk("spk_1", "Speaker 2")],
        };
        let before: Vec<_> = t
            .segments
            .iter()
            .map(|s| (s.id.clone(), s.text.clone(), s.start_ms, s.end_ms))
            .collect();
        // new diarization merges both into one cluster
        apply_turns(&mut t, &[turn("spk_0", 0, 20)], None);
        let after: Vec<_> = t
            .segments
            .iter()
            .map(|s| (s.id.clone(), s.text.clone(), s.start_ms, s.end_ms))
            .collect();
        assert_eq!(before, after, "only speaker keys may change");
        assert!(t.segments.iter().all(|s| s.speaker_key == "spk_0"));
        assert_eq!(t.speakers.len(), 1);
    }

    #[test]
    fn mic_segments_are_never_reassigned() {
        let mut t = Transcript {
            meeting_id: "m".into(),
            language: None,
            engine: "e".into(),
            segments: vec![seg("a", "mic", 0, 10), seg("b", "spk_0", 10, 20)],
            speakers: vec![spk("mic", "You"), spk("spk_0", "Speaker 1")],
        };
        // turns claim the whole timeline for spk_5
        let r = apply_turns(&mut t, &[turn("spk_5", 0, 20)], Some("mic"));
        assert_eq!(t.segments[0].speaker_key, "mic");
        assert_eq!(t.segments[1].speaker_key, "spk_5");
        assert_eq!(r.changed, vec!["b".to_string()]);
        assert_eq!(t.label_for("mic"), "You");
        assert_eq!(t.label_for("spk_5"), "Speaker 1");
    }

    #[test]
    fn stable_cluster_keeps_its_attendee_assignment() {
        // spk_0 (assigned "Priya") keeps its span; a phantom spk_1 merges in.
        let mut t = Transcript {
            meeting_id: "m".into(),
            language: None,
            engine: "e".into(),
            segments: vec![
                seg("a", "spk_0", 0, 100),
                seg("b", "spk_0", 100, 200),
                seg("c", "spk_1", 200, 210),
            ],
            speakers: vec![spk("spk_0", "Priya"), spk("spk_1", "Speaker 2")],
        };
        let turns = [turn("spk_0", 0, 210)];
        apply_turns(&mut t, &turns, None);
        // all segments now spk_0, and 200/210 of its duration came from the
        // old spk_0 (> 80%) whose duration went 100% here → label carried
        assert!(t.segments.iter().all(|s| s.speaker_key == "spk_0"));
        assert_eq!(t.label_for("spk_0"), "Priya");
    }

    #[test]
    fn unstable_cluster_is_clearly_reset() {
        // Old "Priya" (spk_0) and "Jordan" (spk_1) get merged 50/50 into one
        // new cluster — neither identity survives, label resets to generic.
        let mut t = Transcript {
            meeting_id: "m".into(),
            language: None,
            engine: "e".into(),
            segments: vec![seg("a", "spk_0", 0, 100), seg("b", "spk_1", 100, 200)],
            speakers: vec![spk("spk_0", "Priya"), spk("spk_1", "Jordan")],
        };
        apply_turns(&mut t, &[turn("spk_0", 0, 200)], None);
        assert_eq!(t.label_for("spk_0"), "Speaker 1");
    }

    #[test]
    fn split_cluster_resets_the_lost_half() {
        // Old spk_0 ("Priya") splits into spk_0 + spk_1 60/40: forward
        // dominance fails, so BOTH new clusters reset.
        let mut t = Transcript {
            meeting_id: "m".into(),
            language: None,
            engine: "e".into(),
            segments: vec![seg("a", "spk_0", 0, 60), seg("b", "spk_0", 60, 100)],
            speakers: vec![spk("spk_0", "Priya")],
        };
        apply_turns(
            &mut t,
            &[turn("spk_0", 0, 60), turn("spk_1", 60, 100)],
            None,
        );
        assert_eq!(t.label_for("spk_0"), "Speaker 1");
        assert_eq!(t.label_for("spk_1"), "Speaker 2");
    }

    #[test]
    fn segment_with_no_overlap_snaps_or_falls_back() {
        let turns = [turn("spk_0", 0, 10)];
        // within 1s of the turn → snaps
        assert_eq!(speaker_for_span(10_500, 10_900, &turns), "spk_0");
        // far away → unknown
        assert_eq!(speaker_for_span(60_000, 61_000, &turns), UNKNOWN_KEY);
    }

    #[test]
    fn generic_labels_are_detected() {
        assert!(is_generic_label("spk_0", "Speaker 1"));
        assert!(is_generic_label("spk_0", "Speaker 12"));
        assert!(is_generic_label("spk_unknown", "Unknown"));
        assert!(is_generic_label("spk_3", "spk_3"));
        assert!(!is_generic_label("spk_0", "Priya Kapoor"));
        assert!(!is_generic_label("spk_0", "Speaker one"));
    }
}
