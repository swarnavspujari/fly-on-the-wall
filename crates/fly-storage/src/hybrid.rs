//! Hybrid retrieval: fuse FTS and vector rankings with reciprocal rank
//! fusion (RRF), then group chunk/row hits by note so the result list is
//! "most relevant meetings", each carrying its best snippet.
//!
//! Pure functions — the async embedding call lives in src-tauri; storage
//! hands this module already-ranked lists.

use std::collections::HashMap;

use crate::embeddings::VectorHit;
use crate::SearchHit;
#[cfg(test)]
use crate::SearchHitKind;

/// Standard RRF constant: softens the gap between neighboring ranks so one
/// list can't dominate on rank-1 alone.
const RRF_K: f32 = 60.0;

/// Fuse ranked lists into one note-grouped result list.
///
/// Inputs are the two FTS lists (notes, transcripts — each rank-ordered by
/// bm25) and the vector list (rank-ordered by cosine). Per list, only a
/// note's best-ranked hit contributes; scores sum across lists, so a note
/// that both keyword- and semantically-matches outranks single-source hits.
/// The returned snippet comes from the strongest contributing list: FTS
/// first (its snippets carry `[[match]]` highlights), vector otherwise.
pub fn fuse(
    fts_notes: &[SearchHit],
    fts_transcripts: &[SearchHit],
    vector: &[VectorHit],
    limit: usize,
) -> Vec<SearchHit> {
    struct Group {
        score: f32,
        /// (contribution, hit) of the single best hit seen so far — the
        /// snippet/deep-link the result row shows.
        best: (f32, SearchHit),
    }
    let mut groups: HashMap<String, Group> = HashMap::new();
    // Tie-break: preserve first-seen order (FTS notes → FTS transcripts →
    // vector), so equal-scored results stay stable across passes.
    let mut order: Vec<String> = Vec::new();

    let mut absorb = |list: Vec<SearchHit>, weight: f32| {
        let mut seen_note: std::collections::HashSet<String> = Default::default();
        for (rank, hit) in list.into_iter().enumerate() {
            // per list, only the best-ranked hit of a note counts
            if !seen_note.insert(hit.note_id.clone()) {
                continue;
            }
            let contribution = weight / (RRF_K + rank as f32 + 1.0);
            match groups.get_mut(&hit.note_id) {
                Some(g) => {
                    g.score += contribution;
                    if contribution > g.best.0 {
                        g.best = (contribution, hit);
                    }
                }
                None => {
                    order.push(hit.note_id.clone());
                    groups.insert(
                        hit.note_id.clone(),
                        Group {
                            score: contribution,
                            best: (contribution, hit),
                        },
                    );
                }
            }
        }
    };

    absorb(fts_notes.to_vec(), 1.0);
    absorb(fts_transcripts.to_vec(), 1.0);
    absorb(vector.iter().map(vector_hit_to_search_hit).collect(), 1.0);

    let mut fused: Vec<(usize, SearchHit, f32)> = order
        .into_iter()
        .enumerate()
        .filter_map(|(i, note_id)| groups.remove(&note_id).map(|g| (i, g.best.1, g.score)))
        .collect();
    fused.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
    fused
        .into_iter()
        .take(limit)
        .map(|(_, hit, _)| hit)
        .collect()
}

fn vector_hit_to_search_hit(v: &VectorHit) -> SearchHit {
    SearchHit {
        kind: v.kind,
        note_id: v.note_id.clone(),
        meeting_id: v.meeting_id.clone(),
        title: v.title.clone(),
        snippet: v.snippet.clone(),
        start_ms: v.start_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(note: &str, kind: SearchHitKind, snippet: &str) -> SearchHit {
        SearchHit {
            kind,
            note_id: note.into(),
            meeting_id: None,
            title: format!("title-{note}"),
            snippet: snippet.into(),
            start_ms: None,
        }
    }

    fn vhit(note: &str, score: f32) -> VectorHit {
        VectorHit {
            kind: SearchHitKind::Transcript,
            note_id: note.into(),
            meeting_id: Some(format!("m-{note}")),
            title: format!("title-{note}"),
            snippet: format!("vector snippet {note}"),
            start_ms: Some(1000),
            score,
        }
    }

    #[test]
    fn note_in_both_lists_outranks_single_source() {
        let fts = vec![hit("a", SearchHitKind::Note, "[[a]]")];
        let vector = vec![vhit("b", 0.9), vhit("a", 0.8)];
        let fused = fuse(&fts, &[], &vector, 10);
        assert_eq!(fused[0].note_id, "a", "a matched both lists");
        assert_eq!(fused[1].note_id, "b");
    }

    #[test]
    fn groups_deduplicate_by_note_and_keep_best_snippet() {
        // same note hit as note-FTS and transcript-FTS and vector
        let fts_n = vec![hit("a", SearchHitKind::Note, "[[keyword]] context")];
        let fts_t = vec![hit(
            "a",
            SearchHitKind::Transcript,
            "[[keyword]] in transcript",
        )];
        let vector = vec![vhit("a", 0.99)];
        let fused = fuse(&fts_n, &fts_t, &vector, 10);
        assert_eq!(fused.len(), 1);
        // rank-1 contributions tie across lists; the first one seen (FTS
        // note snippet, which carries highlights) wins
        assert!(fused[0].snippet.contains("[["));
    }

    #[test]
    fn vector_only_results_surface_with_vector_snippet() {
        let fused = fuse(&[], &[], &[vhit("only", 0.7)], 10);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].snippet, "vector snippet only");
        assert_eq!(fused[0].meeting_id.as_deref(), Some("m-only"));
        assert_eq!(fused[0].start_ms, Some(1000));
    }

    #[test]
    fn empty_vector_list_degrades_to_grouped_fts() {
        let fts_n = vec![hit("a", SearchHitKind::Note, "s1")];
        let fts_t = vec![
            hit("b", SearchHitKind::Transcript, "s2"),
            hit("a", SearchHitKind::Transcript, "s3"),
        ];
        let fused = fuse(&fts_n, &fts_t, &[], 10);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].note_id, "a"); // contributes from both lists
    }

    #[test]
    fn limit_is_applied_after_grouping() {
        let vector: Vec<VectorHit> = (0..20)
            .map(|i| vhit(&format!("n{i}"), 1.0 - i as f32 * 0.01))
            .collect();
        assert_eq!(fuse(&[], &[], &vector, 5).len(), 5);
    }
}
