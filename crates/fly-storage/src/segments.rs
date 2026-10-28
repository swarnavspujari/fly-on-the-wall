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

use serde::{Deserialize, Serialize};

use crate::search::{sanitize_fts_query, SearchFilter};
use crate::{Result, Storage};

/// The rows `transcript_segments` holds for one `transcripts` row, given SQL
/// expressions for its meeting id, raw JSON and polished JSON. Used by the
/// triggers (`new.*`).
///
/// `LIMIT -1` is load-bearing: it stops SQLite from flattening the polished
/// subquery into the join, which would re-parse the polished JSON once per
/// raw segment (measured: 42 s for a 3,000-segment transcript). With it the
/// subquery is materialized once and joined through an automatic index
/// (~30 ms). Triggers can't use a CTE, hence this form.
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
                             json_extract(value, '$.text') AS text
                        FROM json_each(CASE WHEN json_valid({cleaned}) THEN {cleaned} ELSE '{{}}' END,
                                       '$.segments')
                       LIMIT -1) c
             ON c.id = json_extract(r.value, '$.id')"
    )
}

/// DDL for the segment index: table, external-content FTS5 index, and the
/// triggers that keep both in step with `transcripts`. Idempotent.
pub(crate) fn schema_sql() -> String {
    let rows = segment_rows_select("new.meeting_id", "new.json", "new.cleaned_json");
    format!(
        r#"
        CREATE TABLE IF NOT EXISTS transcript_segments (
            id          INTEGER PRIMARY KEY,
            meeting_id  TEXT NOT NULL,
            seq         INTEGER,          -- position in the transcript
            segment_id  TEXT NOT NULL,
            speaker_key TEXT NOT NULL,
            start_ms    INTEGER,
            end_ms      INTEGER,
            variant     TEXT NOT NULL,    -- 'raw' | 'polished'
            text        TEXT NOT NULL
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

        CREATE TRIGGER IF NOT EXISTS transcript_segments_fts_insert
        AFTER INSERT ON transcript_segments BEGIN
            INSERT INTO segments_fts (rowid, text) VALUES (new.id, new.text);
        END;
        CREATE TRIGGER IF NOT EXISTS transcript_segments_fts_delete
        AFTER DELETE ON transcript_segments BEGIN
            INSERT INTO segments_fts (segments_fts, rowid, text)
            VALUES ('delete', old.id, old.text);
        END;
        CREATE TRIGGER IF NOT EXISTS transcript_segments_fts_update
        AFTER UPDATE ON transcript_segments BEGIN
            INSERT INTO segments_fts (segments_fts, rowid, text)
            VALUES ('delete', old.id, old.text);
            INSERT INTO segments_fts (rowid, text) VALUES (new.id, new.text);
        END;

        CREATE TRIGGER IF NOT EXISTS transcripts_segments_insert
        AFTER INSERT ON transcripts BEGIN
            INSERT INTO transcript_segments
                (meeting_id, seq, segment_id, speaker_key, start_ms, end_ms, variant, text)
            {rows};
        END;

        -- Also marks citations whose segment id the new transcript no longer
        -- has (a re-transcribe makes new ids). The app re-anchors them right
        -- after (citations.rs); an older build leaves them marked, which is
        -- the honest state.
        CREATE TRIGGER IF NOT EXISTS transcripts_segments_update
        AFTER UPDATE OF json, cleaned_json ON transcripts BEGIN
            DELETE FROM transcript_segments WHERE meeting_id = old.meeting_id;
            INSERT INTO transcript_segments
                (meeting_id, seq, segment_id, speaker_key, start_ms, end_ms, variant, text)
            {rows};
            UPDATE citation_locators SET status = 'changed'
             WHERE meeting_id = new.meeting_id AND status != 'changed'
               AND seg_id NOT IN (SELECT segment_id FROM transcript_segments
                                   WHERE meeting_id = new.meeting_id);
        END;

        CREATE TRIGGER IF NOT EXISTS transcripts_segments_delete
        AFTER DELETE ON transcripts BEGIN
            DELETE FROM transcript_segments WHERE meeting_id = old.meeting_id;
            DELETE FROM citation_locators WHERE meeting_id = old.meeting_id;
        END;
        "#
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
    /// Keyword search over individual transcript segments, bm25-ranked.
    /// Honors `filter` like [`Storage::search_split`]: folder via the owning
    /// note, dates on the meeting's start, `limit`/`offset`, and
    /// `exclude_keep_local` inside the query (before the limit, so paging
    /// reveals nothing about hidden meetings). Under `exclude_keep_local` a
    /// meeting whose note row is missing is dropped too: its privacy can't
    /// be checked.
    pub fn search_segments(&self, query: &str, filter: &SearchFilter) -> Result<Vec<SegmentHit>> {
        let fts_query = sanitize_fts_query(query);
        if fts_query.is_empty() {
            return Ok(vec![]);
        }
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
                &fts_query,
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
        // must index nothing rather than abort the write.
        for json in [
            "not json",
            "{}",
            "{\"segments\": 7}",
            "{\"segments\": [{\"text\": 1}]}",
        ] {
            s.conn
                .execute(
                    "INSERT INTO transcripts (meeting_id, json) VALUES ('m-x', ?1)
                     ON CONFLICT(meeting_id) DO UPDATE SET json = excluded.json",
                    [json],
                )
                .unwrap();
        }
        s.conn
            .execute(
                "UPDATE transcripts SET cleaned_json = 'nope' WHERE meeting_id = 'm-x'",
                [],
            )
            .unwrap();
        s.conn
            .execute("DELETE FROM transcripts WHERE meeting_id = 'm-x'", [])
            .unwrap();
        fts_ok(&s);
    }
}
