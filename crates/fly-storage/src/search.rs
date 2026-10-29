//! Full-text search over note bodies and transcripts (FTS5), with snippets.
//! Transcript hits carry a timestamp so the UI can deep-link into the
//! recording position.

use serde::{Deserialize, Serialize};

use crate::query::{term_expr, FtsQuery, TermOwners, TermStats};
use crate::{Result, Storage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchHitKind {
    Note,
    Transcript,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub kind: SearchHitKind,
    pub note_id: String,
    /// For transcript hits: the meeting the matched transcript belongs to.
    #[serde(default)]
    pub meeting_id: Option<String>,
    pub title: String,
    /// FTS5 snippet with the match wrapped in `[[` `]]`.
    pub snippet: String,
    /// For transcript hits: position of the matched segment.
    pub start_ms: Option<u64>,
}

/// Filters + pagination for [`Storage::search_filtered`] (MCP `search_notes`).
#[derive(Debug, Default, Clone)]
pub struct SearchFilter {
    pub folder_id: Option<String>,
    /// Notes: updated at/after; transcripts: meeting started at/after.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    pub limit: usize,
    /// Rank-ordered offset — the cursor for paging deeper into results.
    /// Applied to the note and transcript rankings independently.
    pub offset: usize,
    /// Drop keep-local notes (and transcripts of keep-local meetings)
    /// inside the query, so they never count toward `limit`/`offset` and
    /// paging reveals nothing about them (MCP).
    pub exclude_keep_local: bool,
}

impl Storage {
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_filtered(
            query,
            &SearchFilter {
                limit,
                ..Default::default()
            },
        )
    }

    pub fn search_filtered(&self, query: &str, filter: &SearchFilter) -> Result<Vec<SearchHit>> {
        let (mut notes, mut transcripts) = self.search_split(query, filter)?;
        notes.append(&mut transcripts);
        Ok(notes)
    }

    /// The two FTS rankings (note hits, transcript hits) separately — each
    /// rank-ordered by bm25. Hybrid fusion treats them as independent lists;
    /// [`search_filtered`] concatenates them for the classic API.
    ///
    /// Keyword queries match every word (see [`crate::query`]); when that
    /// finds nothing in either list under `filter`, the query's content
    /// terms are ORed instead, so a question still finds the notes and
    /// meetings sharing its rarest words. The choice depends only on the
    /// query and the filter, never on `offset`, so pages stay consistent.
    pub fn search_split(
        &self,
        query: &str,
        filter: &SearchFilter,
    ) -> Result<(Vec<SearchHit>, Vec<SearchHit>)> {
        let q = FtsQuery::parse(query);
        if q.is_empty() {
            return Ok((vec![], vec![]));
        }
        let expr = q.choose(|all| -> Result<bool> {
            let first = SearchFilter {
                limit: 1,
                offset: 0,
                ..filter.clone()
            };
            Ok(!self.fts_note_hits(all, &first)?.is_empty()
                || !self.fts_transcript_hits(all, &first)?.is_empty())
        })?;
        Ok((
            self.fts_note_hits(expr, filter)?,
            self.fts_transcript_hits(expr, filter)?,
        ))
    }

    /// [`Storage::segment_term_stats`] for notes (title and body): how many
    /// notes are in scope and, per term, which notes hold it. Honors
    /// `filter`'s folder, `updated_at` window and `exclude_keep_local` (in
    /// SQL); `limit`/`offset` don't apply.
    pub fn note_term_stats(&self, terms: &[String], filter: &SearchFilter) -> Result<TermStats> {
        self.read_snapshot(|| self.note_term_stats_in_snapshot(terms, filter))
    }

    fn note_term_stats_in_snapshot(
        &self,
        terms: &[String],
        filter: &SearchFilter,
    ) -> Result<TermStats> {
        let (cte, not_local) = if filter.exclude_keep_local {
            (
                crate::privacy::KEEP_LOCAL_CTE,
                " AND n.id NOT IN (SELECT id FROM local_notes)",
            )
        } else {
            ("", "")
        };
        let scope = format!(
            "(:folder IS NULL OR n.folder_id = :folder)
              AND (:since IS NULL OR n.updated_at >= :since)
              AND (:until IS NULL OR n.updated_at <= :until){not_local}"
        );
        let folder = filter.folder_id.as_deref();
        let since = filter.since.map(|t| t.to_rfc3339());
        let until = filter.until.map(|t| t.to_rfc3339());
        let docs: i64 = self.conn.query_row(
            &format!("{cte}SELECT count(*) FROM notes n WHERE {scope}"),
            rusqlite::named_params! {":folder": folder, ":since": since, ":until": until},
            |r| r.get(0),
        )?;
        let mut stmt = self.conn.prepare(&format!(
            "{cte}SELECT f.note_id, 1 FROM notes_fts f JOIN notes n ON n.id = f.note_id
              WHERE notes_fts MATCH :expr AND {scope}"
        ))?;
        let mut out = Vec::with_capacity(terms.len());
        for term in terms {
            let rows = stmt.query_map(
                rusqlite::named_params! {
                    ":expr": term_expr(term),
                    ":folder": folder,
                    ":since": since,
                    ":until": until,
                },
                |r| Ok((r.get::<_, String>(0)?, 1usize)),
            )?;
            out.push(TermOwners::collect(rows)?);
        }
        Ok(TermStats {
            docs: docs.max(0) as usize,
            terms: out,
        })
    }

    /// Note rows matching an FTS5 expression, bm25-ranked.
    fn fts_note_hits(&self, fts_query: &str, filter: &SearchFilter) -> Result<Vec<SearchHit>> {
        let (cte, not_local) = if filter.exclude_keep_local {
            (
                crate::privacy::KEEP_LOCAL_CTE,
                " AND n.id NOT IN (SELECT id FROM local_notes)",
            )
        } else {
            ("", "")
        };
        let mut stmt = self.conn.prepare(&format!(
            "{cte}SELECT f.note_id, n.title, snippet(notes_fts, 2, '[[', ']]', ' … ', 12)
             FROM notes_fts f JOIN notes n ON n.id = f.note_id
             WHERE notes_fts MATCH ?1
               AND (?2 IS NULL OR n.folder_id = ?2)
               AND (?3 IS NULL OR n.updated_at >= ?3)
               AND (?4 IS NULL OR n.updated_at <= ?4){not_local}
             ORDER BY rank LIMIT ?5 OFFSET ?6"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![
                fts_query,
                filter.folder_id.as_deref(),
                filter.since.map(|t| t.to_rfc3339()),
                filter.until.map(|t| t.to_rfc3339()),
                filter.limit.max(1) as i64,
                filter.offset as i64
            ],
            |r| {
                Ok(SearchHit {
                    kind: SearchHitKind::Note,
                    note_id: r.get(0)?,
                    meeting_id: None,
                    title: r.get(1)?,
                    snippet: r.get(2)?,
                    start_ms: None,
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Transcript rows (one per meeting) matching an FTS5 expression,
    /// bm25-ranked. They join back to the owning note through meetings;
    /// folder filtering goes through that note.
    fn fts_transcript_hits(
        &self,
        fts_query: &str,
        filter: &SearchFilter,
    ) -> Result<Vec<SearchHit>> {
        let (cte, not_local) = if filter.exclude_keep_local {
            (
                crate::privacy::KEEP_LOCAL_CTE,
                " AND m.note_id NOT IN (SELECT id FROM local_notes)",
            )
        } else {
            ("", "")
        };
        let mut stmt = self.conn.prepare(&format!(
            "{cte}SELECT m.note_id, m.id, m.title, snippet(transcripts_fts, 1, '[[', ']]', ' … ', 12)
             FROM transcripts_fts t JOIN meetings m ON m.id = t.meeting_id
             LEFT JOIN notes n ON n.id = m.note_id
             WHERE transcripts_fts MATCH ?1
               AND (?2 IS NULL OR n.folder_id = ?2)
               AND (?3 IS NULL OR m.started_at >= ?3)
               AND (?4 IS NULL OR m.started_at <= ?4){not_local}
             ORDER BY rank LIMIT ?5 OFFSET ?6"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![
                fts_query,
                filter.folder_id.as_deref(),
                filter.since.map(|t| t.to_rfc3339()),
                filter.until.map(|t| t.to_rfc3339()),
                filter.limit.max(1) as i64,
                filter.offset as i64
            ],
            |r| {
                Ok(SearchHit {
                    kind: SearchHitKind::Transcript,
                    note_id: r.get(0)?,
                    meeting_id: Some(r.get(1)?),
                    title: r.get(2)?,
                    snippet: r.get(3)?,
                    start_ms: None,
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use crate::test_storage;

    #[test]
    fn search_finds_notes_by_body_and_title() {
        let (_dir, s) = test_storage();
        let note = s.create_note("Pricing sync", None).unwrap();
        s.update_note_scratchpad(&note.id, "we agreed on usage-based pricing for enterprise")
            .unwrap();
        let _other = s.create_note("Unrelated", None).unwrap();

        let hits = s.search("pricing", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, note.id);
        assert!(hits[0].snippet.contains("[[") || hits[0].title.contains("Pricing"));

        // prefix matching (search-as-you-type)
        let hits = s.search("enterpr", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn keywords_need_every_word_questions_need_one() {
        let (_dir, s) = test_storage();
        let pricing = s.create_note("Pricing sync", None).unwrap();
        s.update_note_scratchpad(&pricing.id, "usage-based pricing for enterprise accounts")
            .unwrap();
        let hiring = s.create_note("Hiring plan", None).unwrap();
        s.update_note_scratchpad(&hiring.id, "two engineers for enterprise support")
            .unwrap();

        // Keywords: AND, as before (only one note has both words).
        let hits = s.search("enterprise pricing", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, pricing.id);
        // A quoted phrase must match as a phrase.
        assert_eq!(s.search("\"enterprise support\"", 10).unwrap().len(), 1);
        assert!(s.search("\"support enterprise\"", 10).unwrap().is_empty());

        // A question no single note answers word for word: before, AND
        // over "what did we decide about …" found nothing at all.
        let hits = s
            .search(
                "What did we decide about pricing for the enterprise tier?",
                10,
            )
            .unwrap();
        assert_eq!(hits.len(), 2, "OR fallback finds both enterprise notes");
        assert_eq!(hits[0].note_id, pricing.id, "more shared terms rank first");
        // Nothing shares a word: still nothing.
        assert!(s
            .search("Who owns the Berlin lease?", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn note_term_stats_respect_scope_and_keep_local() {
        let (_dir, s) = test_storage();
        let a = s.create_note("Budget review", None).unwrap();
        s.update_note_scratchpad(&a.id, "hiring is frozen").unwrap();
        let private = s.create_folder("Private", None).unwrap();
        s.set_folder_keep_local(&private.id, true).unwrap();
        let b = s.create_note("Private", Some(&private.id)).unwrap();
        s.update_note_scratchpad(&b.id, "budget secret").unwrap();
        let terms = vec!["budget".to_string(), "hiring".to_string()];
        let all = s
            .note_term_stats(&terms, &super::SearchFilter::default())
            .unwrap();
        assert_eq!(all.docs, 2);
        assert_eq!(all.terms[0].count, 2, "title counts too");
        let shareable = s
            .note_term_stats(
                &terms,
                &super::SearchFilter {
                    exclude_keep_local: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(shareable.docs, 1);
        assert_eq!(shareable.terms[0].owners.len(), 1);
        assert!(shareable.terms[0].owners.contains_key(&a.id));
    }

    #[test]
    fn fallback_choice_ignores_offset() {
        let (_dir, s) = test_storage();
        for i in 0..3 {
            let n = s.create_note(&format!("n{i}"), None).unwrap();
            s.update_note_scratchpad(&n.id, &format!("budget memo number {i}"))
                .unwrap();
        }
        let n = s.create_note("both", None).unwrap();
        s.update_note_scratchpad(&n.id, "budget and hiring together")
            .unwrap();
        let page = |offset| {
            s.search_filtered(
                "budget hiring",
                &super::SearchFilter {
                    limit: 1,
                    offset,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        // AND matches one note, so every page uses AND: page 2 is empty
        // rather than switching to the broader OR list.
        assert_eq!(page(0).len(), 1);
        assert!(page(1).is_empty());
    }

    #[test]
    fn hostile_query_syntax_does_not_error() {
        let (_dir, s) = test_storage();
        s.create_note("x", None).unwrap();
        for q in [
            "\"unbalanced",
            "AND OR NOT",
            "a*b(c)",
            "   ",
            "col:val",
            "what is NEAR(a b) for x?",
            "x\" OR \"y budget plan",
            "who said ^start - minus * in (the) plan?",
        ] {
            s.search(q, 10).unwrap();
        }
    }

    #[test]
    fn updated_note_reindexes() {
        let (_dir, s) = test_storage();
        let note = s.create_note("draft", None).unwrap();
        s.update_note_scratchpad(&note.id, "first version about apples")
            .unwrap();
        assert_eq!(s.search("apples", 10).unwrap().len(), 1);
        s.update_note_scratchpad(&note.id, "second version about oranges")
            .unwrap();
        assert_eq!(s.search("apples", 10).unwrap().len(), 0);
        assert_eq!(s.search("oranges", 10).unwrap().len(), 1);
    }
}
