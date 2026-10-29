//! Passages: the retrieval unit Ask cites from.
//!
//! A transcript passage is a speaker-turn window of ~160 words that knows
//! which segments it spans, so every line can be cited by segment id. The
//! windowing mirrors `fly_storage::embeddings::chunk_transcript` exactly
//! (same turns, same 160-word target, same 30-word tail merge), so a
//! vector hit — which carries only its chunk's `start_ms` under the current
//! schema — maps back onto the same window and its segment ids. A test pins
//! the two together.
//!
//! Note passages are `chunk_note`'s paragraph windows (no segment ids).

use std::collections::{HashMap, HashSet};

use fly_core::Transcript;

/// Same constants as fly-storage's chunker (kept in lockstep by test).
const TARGET_WORDS: usize = 160;
const MIN_TAIL_WORDS: usize = 30;

/// RRF constant (as in fly-storage's hybrid search).
const RRF_K: f32 = 60.0;

/// One transcript window: indices into `Transcript::segments` (non-empty
/// segments only, in order).
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub segments: Vec<usize>,
    pub start_ms: u64,
    pub end_ms: u64,
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Speaker-turn windows over a transcript (see module docs).
pub fn transcript_windows(t: &Transcript) -> Vec<Window> {
    struct Turn {
        key: String,
        segs: Vec<usize>,
        words: usize,
    }
    let mut turns: Vec<Turn> = Vec::new();
    for (i, seg) in t.segments.iter().enumerate() {
        let text = seg.text.trim();
        if text.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some(last) if last.key == seg.speaker_key => {
                last.segs.push(i);
                last.words += word_count(text);
            }
            _ => turns.push(Turn {
                key: seg.speaker_key.clone(),
                segs: vec![i],
                words: word_count(text),
            }),
        }
    }

    let mut windows: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_words = 0usize;
    for turn in &turns {
        cur.extend(&turn.segs);
        cur_words += turn.words;
        if cur_words >= TARGET_WORDS {
            windows.push(std::mem::take(&mut cur));
            cur_words = 0;
        }
    }
    if !cur.is_empty() {
        match windows.last_mut() {
            Some(last) if cur_words < MIN_TAIL_WORDS => last.extend(cur),
            _ => windows.push(cur),
        }
    }
    windows
        .into_iter()
        .map(|segs| Window {
            start_ms: t.segments[segs[0]].start_ms,
            end_ms: segs
                .iter()
                .map(|&i| t.segments[i].end_ms)
                .max()
                .unwrap_or(0),
            segments: segs,
        })
        .collect()
}

/// A window as the embedded chunk text (`Label: text` per turn) — used to
/// check alignment with the storage chunker.
pub fn window_text(t: &Transcript, w: &Window) -> String {
    let mut out = String::new();
    let mut last_key: Option<&str> = None;
    for &i in &w.segments {
        let seg = &t.segments[i];
        let text = seg.text.trim();
        if last_key == Some(seg.speaker_key.as_str()) {
            out.push(' ');
            out.push_str(text);
        } else {
            if last_key.is_some() {
                out.push('\n');
            }
            out.push_str(&t.label_for(&seg.speaker_key));
            out.push_str(": ");
            out.push_str(text);
            last_key = Some(seg.speaker_key.as_str());
        }
    }
    out
}

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "did", "do", "does", "for", "from", "how", "i",
    "in", "is", "it", "me", "my", "of", "on", "or", "our", "say", "said", "that", "the", "this",
    "to", "was", "we", "were", "what", "when", "where", "which", "who", "why", "with", "you",
    "about", "they", "their", "there", "us",
];

/// Lowercase alphanumeric terms of a query, minus stopwords.
pub fn query_terms(q: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    q.split(|c: char| !c.is_alphanumeric())
        .map(|t| t.to_lowercase())
        .filter(|t| !t.is_empty() && !STOPWORDS.contains(&t.as_str()))
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// How many distinct query terms a text contains (a term also matches as
/// a word prefix when ≥ 4 chars: "budget" finds "budgets").
pub fn lexical_score(terms: &[String], text: &str) -> usize {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    terms
        .iter()
        .filter(|t| {
            words
                .iter()
                .any(|w| w == *t || (t.chars().count() >= 4 && w.starts_with(t.as_str())))
        })
        .count()
}

/// Where a passage lives: `(owner, window index)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PassageKey {
    /// Transcript window of a meeting.
    Transcript(String, usize),
    /// Paragraph chunk of a note.
    Note(String, usize),
}

impl PassageKey {
    /// The meeting/note a per-owner cap applies to.
    pub fn owner(&self) -> &str {
        match self {
            PassageKey::Transcript(id, _) | PassageKey::Note(id, _) => id,
        }
    }
}

/// Which retrieval lists found a passage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Keyword,
    Semantic,
}

/// Reciprocal-rank fusion of ranked passage lists, then a per-owner cap so
/// one long meeting can't fill the whole result. Ties keep first-seen
/// order (lists are passed keyword-first).
pub fn fuse(
    lists: &[(Via, Vec<PassageKey>)],
    per_owner: usize,
    limit: usize,
) -> Vec<(PassageKey, f32, Vec<Via>)> {
    let mut score: HashMap<PassageKey, (usize, f32, Vec<Via>)> = HashMap::new();
    let mut order = 0usize;
    for (via, list) in lists {
        let mut seen = HashSet::new();
        for (rank, key) in list.iter().enumerate() {
            if !seen.insert(key) {
                continue;
            }
            let c = 1.0 / (RRF_K + rank as f32 + 1.0);
            let e = score.entry(key.clone()).or_insert_with(|| {
                order += 1;
                (order, 0.0, Vec::new())
            });
            e.1 += c;
            if !e.2.contains(via) {
                e.2.push(*via);
            }
        }
    }
    let mut all: Vec<(PassageKey, usize, f32, Vec<Via>)> = score
        .into_iter()
        .map(|(k, (o, s, v))| (k, o, s, v))
        .collect();
    all.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.1.cmp(&b.1)));
    let mut per: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for (k, _, s, v) in all {
        let n = per.entry(k.owner().to_string()).or_default();
        if *n >= per_owner {
            continue;
        }
        *n += 1;
        out.push((k, s, v));
        if out.len() >= limit {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_core::{Speaker, TranscriptSegment};

    pub(crate) fn transcript(lines: &[(&str, &str)]) -> Transcript {
        Transcript {
            meeting_id: "m".into(),
            language: None,
            engine: "test".into(),
            segments: lines
                .iter()
                .enumerate()
                .map(|(i, (key, text))| TranscriptSegment {
                    id: format!("s{i}"),
                    speaker_key: key.to_string(),
                    start_ms: i as u64 * 1000,
                    end_ms: i as u64 * 1000 + 900,
                    text: text.to_string(),
                    words: vec![],
                })
                .collect(),
            speakers: vec![
                Speaker {
                    key: "mic".into(),
                    label: "You".into(),
                },
                Speaker {
                    key: "spk_0".into(),
                    label: "Dana".into(),
                },
            ],
        }
    }

    fn long_transcript() -> Transcript {
        let mut lines = Vec::new();
        for i in 0..60 {
            let key = if (i / 3) % 2 == 0 { "mic" } else { "spk_0" };
            let text = if i % 7 == 3 {
                "   "
            } else {
                "we talked about the quarterly budget and the hiring plan for next year"
            };
            lines.push((key, text));
        }
        lines.push(("spk_0", "short tail"));
        let lines: Vec<(&str, &str)> = lines.iter().map(|(k, t)| (*k, *t)).collect();
        transcript(&lines)
    }

    #[test]
    fn windows_match_the_storage_chunker_exactly() {
        for t in [
            long_transcript(),
            transcript(&[("mic", "hello"), ("spk_0", "hi")]),
            transcript(&[]),
        ] {
            let chunks = fly_storage::embeddings::chunk_transcript(&t);
            let windows = transcript_windows(&t);
            assert_eq!(chunks.len(), windows.len());
            for (c, w) in chunks.iter().zip(&windows) {
                assert_eq!(c.start_ms, Some(w.start_ms));
                assert_eq!(c.end_ms, Some(w.end_ms));
                let ids: Vec<&str> = w
                    .segments
                    .iter()
                    .map(|&i| t.segments[i].id.as_str())
                    .collect();
                assert_eq!(c.segment_ids, ids);
            }
        }
    }

    #[test]
    fn polished_chunks_span_the_raw_windows() {
        // A polish drops words (and can empty a line); chunk spans still
        // follow the raw windows, so a vector hit maps onto exactly one.
        let t = long_transcript();
        let mut cleaned = t.clone();
        for s in &mut cleaned.segments {
            s.text = "budget and hiring".into();
        }
        cleaned.segments[0].text = String::new();
        let chunks = fly_storage::embeddings::chunk_transcript_variant(&t, Some(&cleaned));
        let windows = transcript_windows(&t);
        assert_eq!(chunks.len(), windows.len());
        for (c, w) in chunks.iter().zip(&windows) {
            let ids: Vec<&str> = w
                .segments
                .iter()
                .map(|&i| t.segments[i].id.as_str())
                .collect();
            assert_eq!(c.segment_ids, ids);
        }
    }

    #[test]
    fn windows_skip_empty_segments_and_cover_the_rest_once() {
        let t = long_transcript();
        let windows = transcript_windows(&t);
        let covered: Vec<usize> = windows.iter().flat_map(|w| w.segments.clone()).collect();
        let expected: Vec<usize> = (0..t.segments.len())
            .filter(|&i| !t.segments[i].text.trim().is_empty())
            .collect();
        assert_eq!(covered, expected);
        assert!(windows.iter().all(|w| w.end_ms >= w.start_ms));
    }

    #[test]
    fn terms_and_scoring() {
        assert_eq!(
            query_terms("What did Dana say about the Q3 budget?"),
            vec!["dana", "q3", "budget"]
        );
        let terms = query_terms("budget hiring");
        assert_eq!(lexical_score(&terms, "The budgets were fine."), 1);
        assert_eq!(lexical_score(&terms, "Budget and hiring."), 2);
        // Short terms match whole words only.
        assert_eq!(lexical_score(&query_terms("q3"), "q30 numbers"), 0);
    }

    #[test]
    fn fuse_rewards_agreement_and_caps_per_owner() {
        let t = |m: &str, i| PassageKey::Transcript(m.into(), i);
        let lists = vec![
            (
                Via::Keyword,
                vec![t("a", 0), t("a", 1), t("a", 2), t("b", 0)],
            ),
            (Via::Semantic, vec![t("b", 0), t("a", 3)]),
        ];
        let fused = fuse(&lists, 2, 10);
        assert_eq!(fused[0].0, t("b", 0), "found by both lists");
        assert_eq!(fused[0].2, vec![Via::Keyword, Via::Semantic]);
        let from_a = fused.iter().filter(|(k, _, _)| k.owner() == "a").count();
        assert_eq!(from_a, 2, "per-owner cap");
        assert_eq!(fuse(&lists, 3, 1).len(), 1, "limit");
    }
}
