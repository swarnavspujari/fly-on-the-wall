//! Full-text search over note bodies and transcripts (FTS5), with snippets.
//! Transcript hits carry a timestamp so the UI can deep-link into the
//! recording position.

use serde::{Deserialize, Serialize};

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
    pub offset: usize,
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
    pub fn search_split(
        &self,
        query: &str,
        filter: &SearchFilter,
    ) -> Result<(Vec<SearchHit>, Vec<SearchHit>)> {
        let fts_query = sanitize_fts_query(query);
        if fts_query.is_empty() {
            return Ok((vec![], vec![]));
        }
        let limit = filter.limit.max(1) as i64;
        let offset = filter.offset as i64;
        let folder = filter.folder_id.as_deref();
        let since = filter.since.map(|t| t.to_rfc3339());
        let until = filter.until.map(|t| t.to_rfc3339());
        let mut note_hits = Vec::new();
        let mut transcript_hits = Vec::new();

        let mut stmt = self.conn.prepare(
            "SELECT f.note_id, n.title, snippet(notes_fts, 2, '[[', ']]', ' … ', 12)
             FROM notes_fts f JOIN notes n ON n.id = f.note_id
             WHERE notes_fts MATCH ?1
               AND (?2 IS NULL OR n.folder_id = ?2)
               AND (?3 IS NULL OR n.updated_at >= ?3)
               AND (?4 IS NULL OR n.updated_at <= ?4)
             ORDER BY rank LIMIT ?5 OFFSET ?6",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![&fts_query, folder, &since, &until, limit, offset],
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
        for row in rows {
            note_hits.push(row?);
        }

        // Transcript hits join back to the owning note through meetings;
        // folder filtering goes through that note.
        let mut stmt = self.conn.prepare(
            "SELECT m.note_id, m.id, m.title, snippet(transcripts_fts, 1, '[[', ']]', ' … ', 12)
             FROM transcripts_fts t JOIN meetings m ON m.id = t.meeting_id
             LEFT JOIN notes n ON n.id = m.note_id
             WHERE transcripts_fts MATCH ?1
               AND (?2 IS NULL OR n.folder_id = ?2)
               AND (?3 IS NULL OR m.started_at >= ?3)
               AND (?4 IS NULL OR m.started_at <= ?4)
             ORDER BY rank LIMIT ?5 OFFSET ?6",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![&fts_query, folder, &since, &until, limit, offset],
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
        for row in rows {
            transcript_hits.push(row?);
        }

        Ok((note_hits, transcript_hits))
    }
}

/// Turn free-form user input into a safe FTS5 MATCH expression: each token
/// double-quoted (no operator injection / syntax errors), joined with
/// implicit AND, last token gets prefix matching for search-as-you-type.
fn sanitize_fts_query(input: &str) -> String {
    let tokens: Vec<String> = input
        .split_whitespace()
        .map(|t| t.replace('"', ""))
        .filter(|t| !t.is_empty())
        .collect();
    let n = tokens.len();
    tokens
        .into_iter()
        .enumerate()
        .map(|(i, t)| {
            if i + 1 == n {
                format!("\"{t}\"*")
            } else {
                format!("\"{t}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    fn hostile_query_syntax_does_not_error() {
        let (_dir, s) = test_storage();
        s.create_note("x", None).unwrap();
        for q in ["\"unbalanced", "AND OR NOT", "a*b(c)", "   ", "col:val"] {
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
