//! Per-segment transcript index (schema v3).
//!
//! `transcripts_fts` holds one row per meeting, so a keyword hit says which
//! meeting matched but not where. `transcript_segments` holds one row per
//! segment (id, speaker key, span, text), and `segments_fts` indexes its
//! text, so a hit points at a segment and its timestamps.
//!
//! **Maintained by SQLite triggers, not by Rust write paths.** The triggers
//! live in the database file, so every build that writes the `transcripts`
//! table keeps this index in step, including an OLDER build that opens a v3
//! file and knows nothing about it (see `Storage::SCHEMA_VERSION`). They use
//! only core SQLite (built-in JSON functions, FTS5) and are written so they
//! can't fail on any row an app ever stored (`json_valid` guards, no
//! constraints that a malformed segment could violate): a trigger error
//! would fail the old build's transcript write.
//!
//! Text variant: each segment indexes its polished text when the polished
//! transcript (`cleaned_json`) has a segment with the same id, else its raw
//! text (`variant` says which). Matching by id means a stale polished variant
//! left over from an earlier transcription run (whose ids no longer exist)
//! is never indexed. Speaker labels are not indexed: they're display names
//! that a rename changes, and speakers are a filter (`speaker_key`).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::query::{term_expr, FtsQuery, TermOwners, TermStats};
use crate::search::SearchFilter;
use crate::{Result, Storage};

/// The rows `transcript_segments` holds for one `transcripts` row, given SQL
/// expressions for its meeting id, raw JSON and polished JSON. Used by the
/// triggers (`new.*`).
///
/// **Total**: it must return rows (possibly none) for ANY stored value,
/// because it runs inside every build's transcript write: an error here
/// fails that write. `json_valid` guards the document; `type = 'object'`
/// guards each element (`json_extract` on a string element raises
/// "malformed JSON"); every column is COALESCEd or nullable.
///
/// `LIMIT -1` is load-bearing: it stops SQLite from flattening the polished
/// subquery into the join, which would re-parse the polished JSON once per
/// raw segment (measured: 42 s for a 3,000-segment transcript). With it the
/// subquery is materialized once and joined through an automatic index
/// (~30 ms). Triggers can't use a CTE, hence this form. `GROUP BY` keeps
/// duplicate polished ids from fanning out into duplicate rows.
fn segment_rows_select(meeting_id: &str, raw: &str, cleaned: &str) -> String {
    format!(
        "SELECT {meeting_id},
                CAST(r.key AS INTEGER),
                COALESCE(json_extract(r.value, '$.id'), ''),
                COALESCE(json_extract(r.value, '$.speaker_key'), ''),
                json_extract(r.value, '$.start_ms'),
                json_extract(r.value, '$.end_ms'),
                CASE WHEN c.text IS NULL THEN 'raw' ELSE 'polished' END,
                COALESCE(c.text, json_extract(r.value, '$.text'), '')
           FROM json_each(CASE WHEN json_valid({raw}) THEN {raw} ELSE '{{}}' END, '$.segments') r
           LEFT JOIN (SELECT json_extract(value, '$.id') AS id,
                             MAX(json_extract(value, '$.text')) AS text
                        FROM json_each(CASE WHEN json_valid({cleaned}) THEN {cleaned} ELSE '{{}}' END,
                                       '$.segments')
                       WHERE type = 'object'
                       GROUP BY 1
                       LIMIT -1) c
             ON c.id = json_extract(r.value, '$.id')
          WHERE r.type = 'object'"
    )
}

/// Every trigger this module owns, dropped and re-created on each open (see
/// [`install`]).
const TRIGGERS: &[&str] = &[
    "transcript_segments_fts_insert",
    "transcript_segments_fts_delete",
    "transcript_segments_fts_update",
    "transcripts_segments_insert",
    "transcripts_segments_update",
    "transcripts_segments_delete",
];

/// Tables and indexes, created once. No NOT NULL / UNIQUE / FK constraints:
/// the triggers write these rows inside other builds' transcript writes,
/// and a constraint violation would fail them (see `Storage::SCHEMA_VERSION`).
const TABLES_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS transcript_segments (
        id          INTEGER PRIMARY KEY,
        meeting_id  TEXT,
        seq         INTEGER,          -- position in the transcript
        segment_id  TEXT,
        speaker_key TEXT,
        start_ms    INTEGER,
        end_ms      INTEGER,
        variant     TEXT,             -- 'raw' | 'polished'
        text        TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_transcript_segments_meeting
        ON transcript_segments(meeting_id, segment_id);

    -- Text lives once, in transcript_segments; the index reads it from
    -- there (external content).
    CREATE VIRTUAL TABLE IF NOT EXISTS segments_fts USING fts5(
        text,
        content = 'transcript_segments',
        content_rowid = 'id',
        tokenize = 'unicode61 remove_diacritics 2'
    );
"#;

fn triggers_sql() -> String {
    let rows = segment_rows_select("new.meeting_id", "new.json", "new.cleaned_json");
    format!(
        r#"
        CREATE TRIGGER transcript_segments_fts_insert
        AFTER INSERT ON transcript_segments BEGIN
            INSERT INTO segments_fts (rowid, text) VALUES (new.id, new.text);
        END;
        CREATE TRIGGER transcript_segments_fts_delete
        AFTER DELETE ON transcript_segments BEGIN
            INSERT INTO segments_fts (segments_fts, rowid, text)
            VALUES ('delete', old.id, old.text);
        END;
        CREATE TRIGGER transcript_segments_fts_update
        AFTER UPDATE ON transcript_segments BEGIN
            INSERT INTO segments_fts (segments_fts, rowid, text)
            VALUES ('delete', old.id, old.text);
            INSERT INTO segments_fts (rowid, text) VALUES (new.id, new.text);
        END;

        CREATE TRIGGER transcripts_segments_insert
        AFTER INSERT ON transcripts BEGIN
            INSERT INTO transcript_segments
                (meeting_id, seq, segment_id, speaker_key, start_ms, end_ms, variant, text)
            {rows};
        END;

        -- Also marks citations whose segment id the new transcript no longer
        -- has (a re-transcribe makes new ids). The app re-anchors them right
        -- after (citations.rs); an older build leaves them marked, which is
        -- the honest state.
        CREATE TRIGGER transcripts_segments_update
        AFTER UPDATE OF json, cleaned_json ON transcripts BEGIN
            DELETE FROM transcript_segments WHERE meeting_id = old.meeting_id;
            INSERT INTO transcript_segments
                (meeting_id, seq, segment_id, speaker_key, start_ms, end_ms, variant, text)
            {rows};
            UPDATE citation_locators SET status = 'changed'
             WHERE meeting_id = new.meeting_id AND status != 'changed'
               AND NOT EXISTS (SELECT 1 FROM transcript_segments ts
                                WHERE ts.meeting_id = new.meeting_id
                                  AND ts.segment_id = citation_locators.seg_id);
        END;

        CREATE TRIGGER transcripts_segments_delete
        AFTER DELETE ON transcripts BEGIN
            DELETE FROM transcript_segments WHERE meeting_id = old.meeting_id;
            DELETE FROM citation_locators WHERE meeting_id = old.meeting_id;
        END;
        "#
    )
}

/// Create the segment index if missing, and (re)install its triggers. Runs
/// on every open, after `citation_locators` exists: dropping and re-creating
/// the triggers in one transaction means a fixed trigger reaches every
/// existing file, not just new ones. Also turns on FTS5 `secure-delete`
/// (SQLite >= 3.42, bundled in every build) so deleted transcript text
/// doesn't linger in the index's pages.
pub(crate) fn install(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(TABLES_SQL)?;
    let tx = conn.unchecked_transaction()?;
    for name in TRIGGERS {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {name};"))?;
    }
    conn.execute_batch(&triggers_sql())?;
    conn.execute(
        "INSERT INTO segments_fts (segments_fts, rank) VALUES ('secure-delete', 1)",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// True for SQLite's "database disk image is malformed" family
/// (`SQLITE_CORRUPT`, incl. `SQLITE_CORRUPT_VTAB` from a damaged FTS index).
pub(crate) fn is_corrupt(e: &crate::StorageError) -> bool {
    matches!(
        e,
        crate::StorageError::Db(rusqlite::Error::SqliteFailure(f, _))
            if f.code == rusqlite::ErrorCode::DatabaseCorrupt
    )
}

/// One keyword hit at segment granularity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SegmentHit {
    pub meeting_id: String,
    pub note_id: String,
    /// Meeting title.
    pub title: String,
    pub segment_id: String,
    pub speaker_key: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    /// `"raw"` or `"polished"`: which text matched.
    pub variant: String,
    /// FTS5 snippet with the match wrapped in `[[` `]]`.
    pub snippet: String,
}

/// One indexed segment, as stored.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IndexedSegment {
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub text: String,
}

impl Storage {
    /// Rebuild `segments_fts` from `transcript_segments` (FTS5 `rebuild`):
    /// the repair for a damaged keyword index, and the last stage of the v3
    /// step.
    pub(crate) fn rebuild_segment_index(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO segments_fts (segments_fts) VALUES ('rebuild')",
            [],
        )?;
        Ok(())
    }

    /// Keyword search over individual transcript segments, bm25-ranked.
    /// Honors `filter` like [`Storage::search_split`]: folder via the owning
    /// note, dates on the meeting's start, `limit`/`offset`, and
    /// `exclude_keep_local` inside the query (before the limit, so paging
    /// reveals nothing about hidden meetings). Under `exclude_keep_local` a
    /// meeting whose note row is missing is dropped too: its privacy can't
    /// be checked.
    ///
    /// Like [`Storage::search_split`], every word must match unless that
    /// finds nothing under `filter`; then the query's content terms are
    /// ORed (see [`crate::query`]).
    pub fn search_segments(&self, query: &str, filter: &SearchFilter) -> Result<Vec<SegmentHit>> {
        let q = FtsQuery::parse(query);
        if q.is_empty() {
            return Ok(vec![]);
        }
        let expr = q.choose(|all| -> Result<bool> {
            let first = SearchFilter {
                limit: 1,
                offset: 0,
                ..filter.clone()
            };
            Ok(!self.segment_hits(all, &first)?.is_empty())
        })?;
        self.segment_hits(expr, filter)
    }

    /// Segment rows matching an FTS5 expression, bm25-ranked.
    fn segment_hits(&self, fts_query: &str, filter: &SearchFilter) -> Result<Vec<SegmentHit>> {
        let (cte, not_local) = if filter.exclude_keep_local {
            (
                crate::privacy::KEEP_LOCAL_CTE,
                " AND n.id IS NOT NULL AND m.note_id NOT IN (SELECT id FROM local_notes)",
            )
        } else {
            ("", "")
        };
        let mut stmt = self.conn.prepare(&format!(
            "{cte}SELECT ts.meeting_id, m.note_id, m.title, ts.segment_id, ts.speaker_key,
                    ts.start_ms, ts.end_ms, ts.variant,
                    snippet(segments_fts, 0, '[[', ']]', ' … ', 12)
               FROM segments_fts
               JOIN transcript_segments ts ON ts.id = segments_fts.rowid
               JOIN meetings m ON m.id = ts.meeting_id
               LEFT JOIN notes n ON n.id = m.note_id
              WHERE segments_fts MATCH ?1
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
                Ok(SegmentHit {
                    meeting_id: r.get(0)?,
                    note_id: r.get(1)?,
                    title: r.get(2)?,
                    segment_id: r.get(3)?,
                    speaker_key: r.get(4)?,
                    start_ms: r.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                    end_ms: r.get::<_, Option<i64>>(6)?.map(|v| v.max(0) as u64),
                    variant: r.get(7)?,
                    snippet: r.get(8)?,
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Keyword statistics for ranking passages over the segment index:
    /// how many segments are in scope, and per term which meetings hold it
    /// (and in how many segments). Terms come from
    /// [`crate::query::query_terms`] and match with its prefix rule.
    /// Honors `filter`'s folder, dates and `exclude_keep_local` (in SQL,
    /// failing closed like [`Storage::search_segments`]); `limit`/`offset`
    /// don't apply.
    pub fn segment_term_stats(&self, terms: &[String], filter: &SearchFilter) -> Result<TermStats> {
        // Scope is decided once per meeting, not per matching segment, and a
        // matching FTS row is mapped to its meeting through the cached rowid
        // ranges: a common term matches tens of thousands of segments in a
        // big library, and reading each text-heavy row (or joining it to its
        // meeting and note) dominated the search.
        let in_scope = self.meetings_in_scope(filter)?;
        let ranges = self.segment_ranges()?;
        let Some(ranges) = ranges.as_ref() else {
            return self.segment_term_stats_joined(terms, &in_scope);
        };
        let scoped: Vec<bool> = ranges
            .meetings
            .iter()
            .map(|m| in_scope.contains(m))
            .collect();
        let docs = ranges
            .counts
            .iter()
            .zip(&scoped)
            .filter(|(_, s)| **s)
            .map(|(n, _)| *n)
            .sum();
        let mut stmt = self
            .conn
            .prepare_cached("SELECT rowid FROM segments_fts WHERE segments_fts MATCH ?1")?;
        let mut out = Vec::with_capacity(terms.len());
        for term in terms {
            let mut per: HashMap<u32, usize> = HashMap::new();
            let mut rows = stmt.query([term_expr(term)])?;
            while let Some(r) = rows.next()? {
                if let Some(m) = ranges.meeting_of(r.get(0)?) {
                    if scoped[m as usize] {
                        *per.entry(m).or_default() += 1;
                    }
                }
            }
            out.push(TermOwners {
                count: per.values().sum(),
                owners: per
                    .into_iter()
                    .map(|(m, n)| (ranges.meetings[m as usize].clone(), n))
                    .collect(),
            });
        }
        Ok(TermStats { docs, terms: out })
    }

    /// [`Storage::segment_term_stats`] by joining each matching row to its
    /// meeting: the fallback when segment rowid ranges overlap.
    fn segment_term_stats_joined(
        &self,
        terms: &[String],
        in_scope: &HashSet<String>,
    ) -> Result<TermStats> {
        let mut docs = 0usize;
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT meeting_id, count(*) FROM transcript_segments GROUP BY meeting_id",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                if let rusqlite::types::ValueRef::Text(m) = r.get_ref(0)? {
                    if in_scope.contains(std::str::from_utf8(m).unwrap_or("")) {
                        docs += r.get::<_, i64>(1)?.max(0) as usize;
                    }
                }
            }
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT ts.meeting_id
               FROM segments_fts
               JOIN transcript_segments ts ON ts.id = segments_fts.rowid
              WHERE segments_fts MATCH ?1",
        )?;
        let mut out = Vec::with_capacity(terms.len());
        for term in terms {
            let mut owners = TermOwners::default();
            let mut rows = stmt.query([term_expr(term)])?;
            while let Some(r) = rows.next()? {
                let rusqlite::types::ValueRef::Text(m) = r.get_ref(0)? else {
                    continue;
                };
                let m = std::str::from_utf8(m).unwrap_or("");
                if !in_scope.contains(m) {
                    continue;
                }
                owners.count += 1;
                match owners.owners.get_mut(m) {
                    Some(n) => *n += 1,
                    None => {
                        owners.owners.insert(m.to_string(), 1);
                    }
                }
            }
            out.push(owners);
        }
        Ok(TermStats { docs, terms: out })
    }

    /// Ids of the meetings `filter` admits: folder via the owning note,
    /// dates on the meeting's start, keep-local in SQL. Under
    /// `exclude_keep_local` a meeting without a note row is out (its privacy
    /// can't be checked), as in [`Storage::search_segments`].
    fn meetings_in_scope(&self, filter: &SearchFilter) -> Result<HashSet<String>> {
        let (cte, not_local) = if filter.exclude_keep_local {
            (
                crate::privacy::KEEP_LOCAL_CTE,
                " AND n.id IS NOT NULL AND m.note_id NOT IN (SELECT id FROM local_notes)",
            )
        } else {
            ("", "")
        };
        let mut stmt = self.conn.prepare(&format!(
            "{cte}SELECT m.id FROM meetings m LEFT JOIN notes n ON n.id = m.note_id
              WHERE (?1 IS NULL OR n.folder_id = ?1)
                AND (?2 IS NULL OR m.started_at >= ?2)
                AND (?3 IS NULL OR m.started_at <= ?3){not_local}"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![
                filter.folder_id.as_deref(),
                filter.since.map(|t| t.to_rfc3339()),
                filter.until.map(|t| t.to_rfc3339())
            ],
            |r| r.get::<_, String>(0),
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// A segment as indexed (polished text when present), if the meeting's
    /// transcript has it.
    pub(crate) fn indexed_segment(
        &self,
        meeting_id: &str,
        segment_id: &str,
    ) -> Result<Option<IndexedSegment>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT start_ms, end_ms, text FROM transcript_segments
                  WHERE meeting_id = ?1 AND segment_id = ?2 ORDER BY seq LIMIT 1",
                (meeting_id, segment_id),
                |r| {
                    Ok(IndexedSegment {
                        start_ms: r.get::<_, Option<i64>>(0)?.map(|v| v.max(0) as u64),
                        end_ms: r.get::<_, Option<i64>>(1)?.map(|v| v.max(0) as u64),
                        text: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_storage;
    use fly_core::{Speaker, Transcript, TranscriptSegment};

    fn seg(id: &str, start_ms: u64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: id.into(),
            speaker_key: "spk_0".into(),
            start_ms,
            end_ms: start_ms + 900,
            text: text.into(),
            words: vec![],
        }
    }

    fn transcript(meeting_id: &str, segs: Vec<TranscriptSegment>) -> Transcript {
        Transcript {
            meeting_id: meeting_id.into(),
            language: Some("en".into()),
            engine: "test".into(),
            segments: segs,
            speakers: vec![Speaker {
                key: "spk_0".into(),
                label: "Dana".into(),
            }],
        }
    }

    fn rows(s: &Storage, meeting_id: &str) -> Vec<(String, String, String)> {
        s.conn
            .prepare(
                "SELECT segment_id, variant, text FROM transcript_segments
                  WHERE meeting_id = ?1 ORDER BY seq",
            )
            .unwrap()
            .query_map([meeting_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    fn fts_ok(s: &Storage) {
        s.conn
            .execute(
                "INSERT INTO segments_fts (segments_fts) VALUES ('integrity-check')",
                [],
            )
            .unwrap();
    }

    #[test]
    fn term_stats_count_segments_per_meeting_within_scope() {
        let (_dir, s) = test_storage();
        let mk = |title: &str, lines: &[&str]| {
            let note = s.create_note(title, None).unwrap();
            let m = s.create_meeting(title, &note.id, &[]).unwrap();
            let segs = lines
                .iter()
                .enumerate()
                .map(|(i, t)| seg(&format!("{title}-{i}"), i as u64 * 1000, t))
                .collect();
            s.save_transcript(&transcript(&m.id, segs)).unwrap();
            (m.id, note.id)
        };
        let (a, _) = mk("a", &["budget review", "the budgets grew", "hiring plan"]);
        let (b, b_note) = mk("b", &["budget secret", "nothing else"]);
        s.set_note_keep_local(&b_note, true).unwrap();
        let terms = vec!["budget".to_string(), "hiring".to_string(), "zz".to_string()];

        let all = s
            .segment_term_stats(&terms, &SearchFilter::default())
            .unwrap();
        assert_eq!(all.docs, 5);
        // "budget" also matches "budgets" (prefix rule).
        assert_eq!(all.terms[0].count, 3);
        assert_eq!(all.terms[0].owners[&a], 2);
        assert_eq!(all.terms[0].owners[&b], 1);
        assert_eq!(all.terms[1].count, 1);
        assert_eq!(all.terms[2].count, 0);
        let idf = all.idf();
        assert!(
            idf[2] > idf[1] && idf[1] > idf[0],
            "rarer terms weigh more: {idf:?}"
        );

        // Keep-local meetings don't count at all: not as owners, not in N.
        let shareable = s
            .segment_term_stats(
                &terms,
                &SearchFilter {
                    exclude_keep_local: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(shareable.docs, 3);
        assert_eq!(shareable.terms[0].count, 2);
        assert!(!shareable.terms[0].owners.contains_key(&b));
        // Dates on the meeting's start.
        let future = s
            .segment_term_stats(
                &terms,
                &SearchFilter {
                    since: Some(chrono::Utc::now() + chrono::Duration::days(1)),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(future.docs, 0);
        assert_eq!(future.terms[0].count, 0);
    }

    #[test]
    fn term_stats_agree_when_rowid_ranges_overlap() {
        let (_dir, s) = test_storage();
        let mut ids = Vec::new();
        for title in ["a", "b"] {
            let note = s.create_note(title, None).unwrap();
            let m = s.create_meeting(title, &note.id, &[]).unwrap();
            let segs = (0..3)
                .map(|i| seg(&format!("{title}{i}"), i * 1000, "budget talk"))
                .collect();
            s.save_transcript(&transcript(&m.id, segs)).unwrap();
            ids.push(m.id);
        }
        let terms = vec!["budget".to_string()];
        let fast = s
            .segment_term_stats(&terms, &SearchFilter::default())
            .unwrap();
        assert!(s.segment_ranges().unwrap().is_some());
        // A row of meeting b inside meeting a's rowid range (no trigger
        // writes this way; the stats must still be right).
        let a_min: i64 = s
            .conn
            .query_row(
                "SELECT min(id) FROM transcript_segments WHERE meeting_id = ?1",
                [&ids[0]],
                |r| r.get(0),
            )
            .unwrap();
        s.conn
            .execute(
                "UPDATE transcript_segments SET meeting_id = ?1 WHERE id = ?2",
                rusqlite::params![&ids[1], a_min + 1],
            )
            .unwrap();
        assert!(s.segment_ranges().unwrap().is_none(), "overlap detected");
        let slow = s
            .segment_term_stats(&terms, &SearchFilter::default())
            .unwrap();
        assert_eq!(slow.docs, fast.docs);
        assert_eq!(slow.terms[0].count, fast.terms[0].count);
        assert_eq!(slow.terms[0].owners[&ids[0]], 2);
        assert_eq!(slow.terms[0].owners[&ids[1]], 4);
    }

    #[test]
    fn segment_search_falls_back_to_any_word() {
        let (_dir, s) = test_storage();
        let note = s.create_note("m", None).unwrap();
        let m = s.create_meeting("m", &note.id, &[]).unwrap();
        s.save_transcript(&transcript(
            &m.id,
            vec![
                seg("s1", 0, "we approved the budget"),
                seg("s2", 1000, "hiring stays frozen"),
            ],
        ))
        .unwrap();
        let f = SearchFilter {
            limit: 10,
            ..Default::default()
        };
        // No single segment holds both words: AND finds nothing, OR both.
        let hits = s
            .search_segments("What about the budget and hiring?", &f)
            .unwrap();
        assert_eq!(hits.len(), 2);
        // Keywords that one segment holds stay precise.
        let hits = s.search_segments("hiring frozen", &f).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].segment_id, "s2");
    }

    #[test]
    fn segments_follow_saves_polish_and_delete() {
        let (_dir, s) = test_storage();
        let note = s.create_note("Airline deal", None).unwrap();
        let m = s.create_meeting("Airline deal", &note.id, &[]).unwrap();
        s.save_transcript(&transcript(
            &m.id,
            vec![
                seg("a", 0, "the fule bid"),
                seg("b", 1000, "ship it friday"),
            ],
        ))
        .unwrap();
        assert_eq!(
            rows(&s, &m.id),
            vec![
                ("a".into(), "raw".into(), "the fule bid".into()),
                ("b".into(), "raw".into(), "ship it friday".into()),
            ]
        );

        // Polish fixes a typo: the polished text is what's indexed.
        s.save_cleaned_transcript(&transcript(
            &m.id,
            vec![
                seg("a", 0, "the fuel bid"),
                seg("b", 1000, "ship it Friday"),
            ],
        ))
        .unwrap();
        assert_eq!(
            rows(&s, &m.id)[0],
            ("a".into(), "polished".into(), "the fuel bid".into())
        );
        let hits = s
            .search_segments(
                "fuel",
                &SearchFilter {
                    limit: 10,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].segment_id, "a");
        assert_eq!(hits[0].start_ms, Some(0));
        assert_eq!(hits[0].end_ms, Some(900));
        assert_eq!(hits[0].variant, "polished");
        assert!(hits[0].snippet.contains("[[fuel]]"), "{}", hits[0].snippet);
        // The raw typo is no longer in the index.
        assert!(s
            .search_segments(
                "fule",
                &SearchFilter {
                    limit: 10,
                    ..Default::default()
                }
            )
            .unwrap()
            .is_empty());

        // A new transcription run (new ids) with the stale polish still
        // present indexes raw text: stale polish never leaks in.
        s.save_transcript(&transcript(&m.id, vec![seg("c", 0, "brand new words")]))
            .unwrap();
        assert_eq!(
            rows(&s, &m.id),
            vec![("c".into(), "raw".into(), "brand new words".into())]
        );
        s.clear_cleaned_transcript(&m.id).unwrap();
        assert_eq!(rows(&s, &m.id).len(), 1);
        fts_ok(&s);

        // Deleting the meeting removes its segments and their index entries.
        s.purge_meeting(&m.id).unwrap();
        assert!(rows(&s, &m.id).is_empty());
        assert!(s
            .search_segments(
                "brand",
                &SearchFilter {
                    limit: 10,
                    ..Default::default()
                }
            )
            .unwrap()
            .is_empty());
        fts_ok(&s);
    }

    #[test]
    fn speaker_labels_are_not_indexed_and_relabel_is_free() {
        let (_dir, s) = test_storage();
        let note = s.create_note("n", None).unwrap();
        let m = s.create_meeting("n", &note.id, &[]).unwrap();
        s.save_transcript(&transcript(&m.id, vec![seg("a", 0, "pricing talk")]))
            .unwrap();
        let before: i64 = s
            .conn
            .query_row("SELECT id FROM transcript_segments", [], |r| r.get(0))
            .unwrap();
        s.relabel_speaker(&m.id, "spk_0", "Karim").unwrap();
        let hits = |q: &str| {
            s.search_segments(
                q,
                &SearchFilter {
                    limit: 10,
                    ..Default::default()
                },
            )
            .unwrap()
            .len()
        };
        assert_eq!(hits("Karim"), 0);
        assert_eq!(hits("Dana"), 0);
        assert_eq!(hits("pricing"), 1);
        // the row was rebuilt (trigger), but content is identical
        let after: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM transcript_segments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, 1);
        assert!(before > 0);
    }

    /// Guards the `LIMIT -1` in `segment_rows_select`: without it a long
    /// polished transcript takes tens of seconds per save.
    #[test]
    fn long_polished_transcript_indexes_quickly() {
        let (_dir, s) = test_storage();
        let note = s.create_note("long", None).unwrap();
        let m = s.create_meeting("long", &note.id, &[]).unwrap();
        let segs: Vec<_> = (0..3_000)
            .map(|i| {
                seg(
                    &format!("s{i}"),
                    i * 1_000,
                    &format!("word {i} and more words here"),
                )
            })
            .collect();
        s.save_transcript(&transcript(&m.id, segs.clone())).unwrap();
        let mut polished = transcript(&m.id, segs);
        for p in &mut polished.segments {
            p.text = p.text.replace("word", "Word");
        }
        let started = std::time::Instant::now();
        s.save_cleaned_transcript(&polished).unwrap();
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_secs(10), "took {took:?}");
        assert_eq!(
            rows(&s, &m.id).iter().filter(|r| r.1 == "polished").count(),
            3_000
        );
    }

    #[test]
    fn malformed_transcript_json_never_fails_the_write() {
        let (_dir, s) = test_storage();
        // What an older build or a hand-edited row might hold: the trigger
        // must index what it can and never abort the write.
        let shapes = [
            "not json",
            "{}",
            "{\"segments\": 7}",
            "{\"segments\": \"abc\"}",
            "{\"segments\": [\"x\"]}",
            "{\"segments\": [\"x\", 3, null, [1], {\"id\": \"ok\", \"text\": \"fine\"}]}",
            "{\"segments\": [{\"text\": 1}]}",
            "{\"segments\": {\"a\": {\"id\": \"o\"}}}",
        ];
        for json in shapes {
            s.conn
                .execute(
                    "INSERT INTO transcripts (meeting_id, json) VALUES ('m-x', ?1)
                     ON CONFLICT(meeting_id) DO UPDATE SET json = excluded.json",
                    [json],
                )
                .unwrap_or_else(|e| panic!("json {json}: {e}"));
        }
        let good = r#"{"segments": [{"id": "ok", "text": "fine"}]}"#;
        s.conn
            .execute(
                "UPDATE transcripts SET json = ?1 WHERE meeting_id = 'm-x'",
                [good],
            )
            .unwrap();
        for cleaned in shapes.iter().chain(&["nope"]) {
            s.conn
                .execute(
                    "UPDATE transcripts SET cleaned_json = ?1 WHERE meeting_id = 'm-x'",
                    [cleaned],
                )
                .unwrap_or_else(|e| panic!("cleaned {cleaned}: {e}"));
        }
        // The object element still indexed, raw text (no polished match).
        assert_eq!(
            rows(&s, "m-x"),
            vec![("ok".into(), "raw".into(), "fine".into())]
        );
        s.conn
            .execute("DELETE FROM transcripts WHERE meeting_id = 'm-x'", [])
            .unwrap();
        fts_ok(&s);
    }

    #[test]
    fn duplicate_polished_ids_never_duplicate_rows() {
        let (_dir, s) = test_storage();
        let note = s.create_note("n", None).unwrap();
        let m = s.create_meeting("n", &note.id, &[]).unwrap();
        s.save_transcript(&transcript(
            &m.id,
            vec![seg("a", 0, "one"), seg("b", 1000, "two")],
        ))
        .unwrap();
        s.save_cleaned_transcript(&transcript(
            &m.id,
            vec![
                seg("a", 0, "One."),
                seg("a", 0, "One!"),
                seg("b", 1000, "Two."),
            ],
        ))
        .unwrap();
        let r = rows(&s, &m.id);
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(r.iter().all(|x| x.1 == "polished"));
    }

    /// Triggers are dropped and re-created on every open, so a corrected
    /// trigger reaches files that already have an old one.
    #[test]
    fn triggers_are_reinstalled_on_every_open() {
        let dir = tempfile::tempdir().unwrap();
        let note_id;
        let mid;
        {
            let s = Storage::open(dir.path()).unwrap();
            let note = s.create_note("n", None).unwrap();
            note_id = note.id.clone();
            mid = s.create_meeting("n", &note.id, &[]).unwrap().id;
            // Simulate a file carrying a stale (here: broken) trigger.
            s.conn
                .execute_batch(
                    "DROP TRIGGER transcripts_segments_insert;
                     CREATE TRIGGER transcripts_segments_insert AFTER INSERT ON transcripts
                     BEGIN SELECT RAISE(ABORT, 'stale trigger'); END;",
                )
                .unwrap();
            assert!(s
                .save_transcript(&transcript(&mid, vec![seg("a", 0, "hello")]))
                .is_err());
        }
        let s = Storage::open(dir.path()).unwrap();
        s.save_transcript(&transcript(&mid, vec![seg("a", 0, "hello")]))
            .unwrap();
        assert_eq!(rows(&s, &mid).len(), 1);
        assert!(!note_id.is_empty());
    }

    /// FTS5 secure-delete: a deleted meeting's words don't linger in the
    /// index's pages (`secure_delete` alone doesn't reach them).
    #[test]
    fn deleted_text_leaves_the_index_pages() {
        let (_dir, s) = test_storage();
        let note = s.create_note("n", None).unwrap();
        let m = s.create_meeting("n", &note.id, &[]).unwrap();
        s.save_transcript(&transcript(&m.id, vec![seg("a", 0, "zanzibarquokka plan")]))
            .unwrap();
        let in_pages = |s: &Storage| -> i64 {
            s.conn
                .query_row(
                    "SELECT COUNT(*) FROM segments_fts_data
                      WHERE instr(block, CAST('zanzibarquokka' AS BLOB)) > 0",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert!(in_pages(&s) > 0, "term is in the index before");
        s.purge_meeting(&m.id).unwrap();
        assert_eq!(in_pages(&s), 0, "term gone from the index pages");
        fts_ok(&s);
    }

    /// A damaged keyword index used to fail the transcript write itself
    /// (for every build, through the triggers). The app rebuilds it from
    /// `transcript_segments` and retries once.
    #[test]
    fn corrupt_index_is_rebuilt_and_the_save_retried() {
        let (_dir, s) = test_storage();
        let note = s.create_note("n", None).unwrap();
        let m = s.create_meeting("n", &note.id, &[]).unwrap();
        s.save_transcript(&transcript(&m.id, vec![seg("a", 0, "alpha beta")]))
            .unwrap();
        s.conn
            .execute(
                "UPDATE segments_fts_data SET block = x'0102030405' WHERE id > 1",
                [],
            )
            .unwrap();
        let broken = s.conn.execute(
            "INSERT INTO segments_fts (segments_fts) VALUES ('integrity-check')",
            [],
        );
        assert!(broken.is_err(), "fixture must actually damage the index");

        s.save_transcript(&transcript(&m.id, vec![seg("b", 0, "gamma delta")]))
            .unwrap();
        fts_ok(&s);
        let hits = s
            .search_segments(
                "gamma",
                &SearchFilter {
                    limit: 5,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
    }
}
