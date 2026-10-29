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

/// Lowercase content terms of a query (stopwords and lone letters
/// dropped, deduplicated): the same terms the storage keyword index is
/// queried with ([`fly_storage::query::query_terms`]).
pub fn query_terms(q: &str) -> Vec<String> {
    fly_storage::query::query_terms(q)
}

/// Which query terms a text contains (a term also matches as a word
/// prefix when ≥ 4 chars: "budget" finds "budgets").
pub fn matched_terms(terms: &[String], text: &str) -> Vec<bool> {
    let mut hit = vec![false; terms.len()];
    mark_terms(terms, text, &mut hit);
    hit
}

/// Set `hit[i]` for each term `text` holds: one pass over its words,
/// stopping once every term is found.
fn mark_terms(terms: &[String], text: &str, hit: &mut [bool]) {
    let mut left = hit.iter().filter(|h| !**h).count();
    let mut word = String::new();
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        if left == 0 {
            return;
        }
        if w.is_empty() {
            continue;
        }
        fly_storage::query::fold_word_into(w, &mut word);
        for (h, t) in hit.iter_mut().zip(terms) {
            if !*h && fly_storage::query::term_matches(t, &word) {
                *h = true;
                left -= 1;
            }
        }
    }
}

/// Which query terms each window holds, counting what [`window_text`]
/// shows (its lines and its speakers' labels) without building the text:
/// one pass over each line, each label matched once.
pub fn window_matches(terms: &[String], t: &Transcript, windows: &[Window]) -> Vec<Vec<bool>> {
    let mut labels: HashMap<&str, Vec<bool>> = HashMap::new();
    windows
        .iter()
        .map(|w| {
            let mut hit = vec![false; terms.len()];
            for &i in &w.segments {
                let seg = &t.segments[i];
                let label = labels
                    .entry(seg.speaker_key.as_str())
                    .or_insert_with(|| matched_terms(terms, &t.label_for(&seg.speaker_key)));
                for (h, l) in hit.iter_mut().zip(label.iter()) {
                    *h |= *l;
                }
                mark_terms(terms, &seg.text, &mut hit);
            }
            hit
        })
        .collect()
}

/// How many distinct query terms a text contains.
pub fn lexical_score(terms: &[String], text: &str) -> usize {
    matched_terms(terms, text).iter().filter(|m| **m).count()
}

/// The IDF mass of the query terms present in `texts` (`idf[i]` weighs
/// `terms[i]`; each text has a weight, and a term counts once, at the
/// highest weight of a text holding it). Rare terms (names, figures,
/// project words) count for much more than words every meeting uses.
pub fn weighted_match(terms: &[String], idf: &[f32], texts: &[(&str, f32)]) -> f32 {
    let mut best = vec![0f32; terms.len()];
    for (text, weight) in texts {
        if *weight <= 0.0 || text.is_empty() {
            continue;
        }
        for (b, m) in best.iter_mut().zip(matched_terms(terms, text)) {
            if m {
                *b = b.max(*weight);
            }
        }
    }
    best.iter().zip(idf).map(|(b, w)| b * w).sum()
}

/// Where a passage lives: `(owner, window index)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PassageKey {
    /// Transcript window of a meeting.
    Transcript(String, usize),
    /// Paragraph chunk of a note.
    Note(String, usize),
    /// A vector hit in a meeting whose windows haven't been computed:
    /// `(meeting id, anchor)`, the anchor being the chunk's first segment id
    /// (or `@<start_ms>` for a chunk an older build wrote). Resolved onto a
    /// [`PassageKey::Transcript`] window only if it makes the result, so a
    /// search doesn't parse every transcript its vector hits touch.
    Chunk(String, String),
}

impl PassageKey {
    /// The meeting/note a per-owner cap applies to.
    pub fn owner(&self) -> &str {
        match self {
            PassageKey::Transcript(id, _) | PassageKey::Note(id, _) | PassageKey::Chunk(id, _) => {
                id
            }
        }
    }
}

/// The window a vector hit lands on: the one holding the chunk's first
/// segment (chunks may embed polished text, so their boundaries can differ
/// from raw-text windows), or for an anchor `@<start_ms>` (a chunk an older
/// build wrote, without ids) the window starting at, else before, it.
pub fn resolve_anchor(t: &Transcript, windows: &[Window], anchor: &str) -> Option<usize> {
    if let Some(start) = anchor.strip_prefix('@') {
        let start: u64 = start.parse().ok()?;
        return windows
            .iter()
            .position(|w| w.start_ms == start)
            .or_else(|| windows.iter().rposition(|w| w.start_ms <= start));
    }
    let pos = t.segments.iter().position(|s| s.id == anchor)?;
    windows.iter().position(|w| w.segments.contains(&pos))
}

/// Which retrieval lists found a passage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Keyword,
    Semantic,
}

/// Reciprocal-rank fusion of ranked passage lists, then a per-owner cap so
/// one long meeting can't crowd the others out. Ties keep first-seen order
/// (lists are passed keyword-first). With `fill`, slots the cap would leave
/// empty go to the capped-out passages, best first: with nothing left to
/// crowd out, a meeting's next-best window is worth more than an empty
/// slot. Without it the cap is strict.
pub fn fuse(
    lists: &[(Via, Vec<PassageKey>)],
    per_owner: usize,
    limit: usize,
    fill: bool,
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
    let mut capped = Vec::new();
    for (k, _, s, v) in all {
        if out.len() >= limit {
            break;
        }
        let n = per.entry(k.owner().to_string()).or_default();
        if *n >= per_owner {
            capped.push((k, s, v));
            continue;
        }
        *n += 1;
        out.push((k, s, v));
    }
    if fill {
        let room = limit.saturating_sub(out.len());
        out.extend(capped.into_iter().take(room));
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
    fn weighted_match_counts_each_term_once_across_texts() {
        let terms = query_terms("Dana budget hiring");
        let idf = [3.0, 1.0, 0.5];
        // "Dana" in the label line, "budgets" in the body: 3 + 1.
        assert_eq!(
            weighted_match(
                &terms,
                &idf,
                &[("Dana: the budgets", 1.0), ("budget again", 1.0)]
            ),
            4.0
        );
        // A term in a lighter text (a title) counts at that weight, unless
        // a full-weight text has it too.
        assert_eq!(
            weighted_match(&terms, &idf, &[("Hiring plan", 0.5), ("Dana: hi", 1.0)]),
            3.25
        );
        assert_eq!(weighted_match(&terms, &idf, &[("nothing here", 1.0)]), 0.0);
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
        let fused = fuse(&lists, 2, 4, true);
        assert_eq!(fused[0].0, t("b", 0), "found by both lists");
        assert_eq!(fused[0].2, vec![Via::Keyword, Via::Semantic]);
        let from_a = fused.iter().filter(|(k, _, _)| k.owner() == "a").count();
        assert_eq!(from_a, 3, "per-owner cap, then the free slot");
        assert_eq!(fused[1].0.owner(), "a");
        assert_eq!(fused[2].0.owner(), "a");
        assert_eq!(
            fused[3].0,
            t("a", 3),
            "capped-out passages only fill free slots"
        );
        // With other owners to show, the cap holds.
        let mut more = lists.clone();
        more.push((
            Via::Semantic,
            vec![t("c", 0), t("d", 0), t("e", 0), t("f", 0)],
        ));
        let fused = fuse(&more, 2, 6, true);
        assert_eq!(fused.iter().filter(|(k, _, _)| k.owner() == "a").count(), 2);
        assert_eq!(fuse(&lists, 3, 1, true).len(), 1, "limit");
        // Strict: the free slot stays empty.
        let fused = fuse(&lists, 2, 4, false);
        assert_eq!(fused.len(), 3);
        assert_eq!(fused.iter().filter(|(k, _, _)| k.owner() == "a").count(), 2);
    }
}
