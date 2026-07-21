//! Chunk store for semantic search: transcripts, scratchpads, and enhanced
//! blocks are split into retrieval-sized chunks whose vectors live in the
//! `embedding_chunks` table.
//!
//! Vector storage is raw little-endian f32 BLOBs scanned brute-force with a
//! dot product: at this app's scale (hundreds of meetings → low tens of
//! thousands of chunks, a few MB of vectors) a full scan is single-digit
//! milliseconds, so an ANN index or a native SQLite vector extension would
//! add a dependency without buying anything.
//!
//! Chunking runs synchronously on every content write (pure string work);
//! embedding happens later, off-process-critical-path, in src-tauri's
//! embedding worker. A freshly (re)chunked row has `embedding = NULL` —
//! "pending" — and unchanged chunks keep their vector across a re-sync, so
//! a one-line edit re-embeds one chunk, not the whole meeting.

use fly_core::{Note, Transcript};
use serde::{Deserialize, Serialize};

use crate::search::SearchFilter;
use crate::{Result, Storage};

/// Soft word target per chunk; windows close at the first turn/paragraph
/// boundary past this.
const CHUNK_TARGET_WORDS: usize = 160;
/// A trailing chunk smaller than this merges into its predecessor rather
/// than standing alone (tiny chunks embed poorly and pollute results).
const CHUNK_MIN_TAIL_WORDS: usize = 30;
/// Display snippet length for vector hits (chars, cut on a word boundary).
const SNIPPET_CHARS: usize = 160;

/// One chunk of owner text, pre-embedding.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// Display/snippet text — exactly what was on screen, no prompt wrapping.
    pub text: String,
    /// For transcript chunks: start of the window (deep-link position).
    pub start_ms: Option<u64>,
}

/// A chunk pending embedding, handed to the embedding worker.
#[derive(Debug, Clone)]
pub struct PendingChunk {
    pub id: i64,
    /// Owning note/meeting title — context the embedder folds into the
    /// document prompt.
    pub title: String,
    pub text: String,
}

/// One vector-search hit (chunk granularity; callers group by note).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorHit {
    /// "note" | "transcript" — mirrors SearchHitKind.
    pub kind: crate::SearchHitKind,
    pub note_id: String,
    pub meeting_id: Option<String>,
    pub title: String,
    pub snippet: String,
    pub start_ms: Option<u64>,
    /// Cosine similarity in [-1, 1] (vectors are stored normalized).
    pub score: f32,
}

// ---------------------------------------------------------------------------
// Chunkers (pure)
// ---------------------------------------------------------------------------

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Split a transcript into speaker-turn windows: consecutive segments merge
/// into turns, turns pack into ~CHUNK_TARGET_WORDS windows that only break
/// at turn boundaries, and each line carries its speaker label so names are
/// part of what gets embedded.
pub fn chunk_transcript(t: &Transcript) -> Vec<Chunk> {
    // 1. segments → turns (same speaker, contiguous)
    struct Turn {
        key: String,
        line: String,
        start_ms: u64,
        words: usize,
    }
    let mut turns: Vec<Turn> = Vec::new();
    for seg in &t.segments {
        let text = seg.text.trim();
        if text.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some(last) if last.key == seg.speaker_key => {
                last.line.push(' ');
                last.line.push_str(text);
                last.words += word_count(text);
            }
            _ => turns.push(Turn {
                key: seg.speaker_key.clone(),
                line: format!("{}: {}", t.label_for(&seg.speaker_key), text),
                start_ms: seg.start_ms,
                words: word_count(text),
            }),
        }
    }

    // 2. turns → windows
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut cur = String::new();
    let mut cur_words = 0usize;
    let mut cur_start = 0u64;
    for turn in &turns {
        if cur.is_empty() {
            cur_start = turn.start_ms;
        } else {
            cur.push('\n');
        }
        cur.push_str(&turn.line);
        cur_words += turn.words;
        if cur_words >= CHUNK_TARGET_WORDS {
            chunks.push(Chunk {
                text: std::mem::take(&mut cur),
                start_ms: Some(cur_start),
            });
            cur_words = 0;
        }
    }
    if !cur.is_empty() {
        if cur_words < CHUNK_MIN_TAIL_WORDS && !chunks.is_empty() {
            let last = chunks.last_mut().unwrap();
            last.text.push('\n');
            last.text.push_str(&cur);
        } else {
            chunks.push(Chunk {
                text: cur,
                start_ms: Some(cur_start),
            });
        }
    }
    chunks
}

/// Split a note (scratchpad + enhanced blocks) into paragraph-packed chunks.
/// The title itself is not a chunk — it rides along as embedding context on
/// every chunk (and a note that is only a title has nothing to embed).
pub fn chunk_note(note: &Note) -> Vec<Chunk> {
    let mut sources: Vec<&str> = vec![&note.scratchpad];
    for b in &note.blocks {
        sources.push(&b.markdown);
    }

    let mut chunks: Vec<Chunk> = Vec::new();
    let mut cur = String::new();
    let mut cur_words = 0usize;
    for source in sources {
        for para in source.split("\n\n") {
            let para = para.trim();
            if para.is_empty() {
                continue;
            }
            if cur.is_empty() {
                cur.push_str(para);
            } else {
                cur.push_str("\n\n");
                cur.push_str(para);
            }
            cur_words += word_count(para);
            if cur_words >= CHUNK_TARGET_WORDS {
                chunks.push(Chunk {
                    text: std::mem::take(&mut cur),
                    start_ms: None,
                });
                cur_words = 0;
            }
        }
    }
    if !cur.is_empty() {
        if cur_words < CHUNK_MIN_TAIL_WORDS && !chunks.is_empty() {
            let last = chunks.last_mut().unwrap();
            last.text.push_str("\n\n");
            last.text.push_str(&cur);
        } else {
            chunks.push(Chunk {
                text: cur,
                start_ms: None,
            });
        }
    }
    chunks
}

/// First ~SNIPPET_CHARS of a chunk, cut on a word boundary.
fn snippet_of(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.len() <= SNIPPET_CHARS {
        return flat;
    }
    let mut cut = SNIPPET_CHARS;
    while !flat.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &flat[..cut];
    let head = head.rsplit_once(' ').map(|(h, _)| h).unwrap_or(head);
    format!("{head} …")
}

// ---------------------------------------------------------------------------
// Vector <-> BLOB
// ---------------------------------------------------------------------------

/// L2-normalize, then encode as little-endian f32 bytes. Normalizing at
/// write time turns query-time cosine into a plain dot product.
pub fn encode_vector(v: &[f32]) -> Vec<u8> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    let inv = if norm > 0.0 { 1.0 / norm } else { 0.0 };
    v.iter().flat_map(|x| (x * inv).to_le_bytes()).collect()
}

fn decode_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

// ---------------------------------------------------------------------------
// Storage integration
// ---------------------------------------------------------------------------

impl Storage {
    /// Re-chunk a note into `embedding_chunks`. Unchanged chunks (same text
    /// + title) keep their embedding; changed ones reset to pending.
    pub(crate) fn sync_note_chunks(&self, note: &Note) -> Result<()> {
        let chunks = chunk_note(note);
        self.sync_chunks("note", &note.id, &note.title, &chunks)
    }

    /// Re-chunk a transcript. The owning meeting's title (the note's title
    /// is snapshotted there) becomes the embedding context.
    pub(crate) fn sync_transcript_chunks(&self, t: &Transcript) -> Result<()> {
        let title = self
            .get_meeting(&t.meeting_id)
            .map(|m| m.title)
            .unwrap_or_default();
        let chunks = chunk_transcript(t);
        self.sync_chunks("transcript", &t.meeting_id, &title, &chunks)
    }

    pub(crate) fn delete_chunks(&self, owner_kind: &str, owner_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM embedding_chunks WHERE owner_kind = ?1 AND owner_id = ?2",
            (owner_kind, owner_id),
        )?;
        Ok(())
    }

    fn sync_chunks(
        &self,
        owner_kind: &str,
        owner_id: &str,
        title: &str,
        chunks: &[Chunk],
    ) -> Result<()> {
        // Existing rows, so an unchanged chunk can carry its vector over.
        let mut stmt = self.conn.prepare(
            "SELECT chunk_index, title, text, embedding, model FROM embedding_chunks
             WHERE owner_kind = ?1 AND owner_id = ?2",
        )?;
        type Row = (i64, String, String, Option<Vec<u8>>, Option<String>);
        let existing: Vec<Row> = stmt
            .query_map((owner_kind, owner_id), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<std::result::Result<_, _>>()?;

        let unchanged = existing.len() == chunks.len()
            && existing
                .iter()
                .zip(chunks)
                .all(|((_, t, txt, _, _), c)| t == title && *txt == c.text);
        if unchanged {
            return Ok(());
        }

        // Content-keyed vector reuse: an edit that shifts chunk boundaries
        // for one window must not re-embed the rest.
        let mut by_content: std::collections::HashMap<(String, String), (Vec<u8>, String)> =
            std::collections::HashMap::new();
        for (_, t, txt, emb, model) in existing {
            if let (Some(emb), Some(model)) = (emb, model) {
                by_content.insert((t, txt), (emb, model));
            }
        }

        self.conn.execute(
            "DELETE FROM embedding_chunks WHERE owner_kind = ?1 AND owner_id = ?2",
            (owner_kind, owner_id),
        )?;
        let mut insert = self.conn.prepare(
            "INSERT INTO embedding_chunks
             (owner_kind, owner_id, chunk_index, title, text, start_ms, embedding, model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (i, c) in chunks.iter().enumerate() {
            let reuse = by_content.get(&(title.to_string(), c.text.clone()));
            insert.execute((
                owner_kind,
                owner_id,
                i as i64,
                title,
                &c.text,
                c.start_ms.map(|v| v as i64),
                reuse.map(|(emb, _)| emb.clone()),
                reuse.map(|(_, m)| m.clone()),
            ))?;
        }
        Ok(())
    }

    /// Chunks with no vector yet (or a vector from another model), oldest
    /// first — the embedding worker's work queue.
    pub fn pending_embedding_chunks(&self, model: &str, limit: usize) -> Result<Vec<PendingChunk>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, text FROM embedding_chunks
             WHERE embedding IS NULL OR model IS NOT ?1
             ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map((model, limit as i64), |r| {
            Ok(PendingChunk {
                id: r.get(0)?,
                title: r.get(1)?,
                text: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Number of chunks still waiting for a vector from `model`.
    pub fn embedding_backlog(&self, model: &str) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM embedding_chunks WHERE embedding IS NULL OR model IS NOT ?1",
            [model],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Store vectors produced by the embedding worker (normalized here).
    pub fn store_chunk_embeddings(&self, model: &str, vectors: &[(i64, Vec<f32>)]) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare("UPDATE embedding_chunks SET embedding = ?1, model = ?2 WHERE id = ?3")?;
        for (id, v) in vectors {
            stmt.execute((encode_vector(v), model, id))?;
        }
        Ok(())
    }

    /// Ensure every note and transcript has chunk rows — the startup
    /// backfill for content written before this feature existed. Idempotent
    /// and cheap when everything is already chunked (`sync_chunks` no-ops on
    /// unchanged content).
    pub fn backfill_embedding_chunks(&self) -> Result<usize> {
        let note_ids: Vec<String> = self
            .conn
            .prepare("SELECT id FROM notes")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for id in &note_ids {
            let note = self.get_note(id)?;
            self.sync_note_chunks(&note)?;
        }
        let meeting_ids: Vec<String> = self
            .conn
            .prepare("SELECT meeting_id FROM transcripts")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for id in &meeting_ids {
            if let Some(t) = self.get_transcript(id)? {
                self.sync_transcript_chunks(&t)?;
            }
        }
        Ok(note_ids.len() + meeting_ids.len())
    }

    /// Brute-force cosine scan over embedded chunks, honoring the same
    /// folder/date semantics as FTS: note chunks filter on the note's
    /// folder/updated_at, transcript chunks on the owning note's folder and
    /// the meeting's started_at. `query` need not be normalized.
    pub fn vector_search(
        &self,
        query: &[f32],
        model: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        let qnorm = decode_vector(&encode_vector(query));
        let folder = filter.folder_id.as_deref();
        let since = filter.since.map(|t| t.to_rfc3339());
        let until = filter.until.map(|t| t.to_rfc3339());

        let mut stmt = self.conn.prepare(
            "SELECT c.owner_kind,
                    COALESCE(n.id, m.note_id, ''),
                    CASE c.owner_kind WHEN 'transcript' THEN c.owner_id END,
                    COALESCE(n.title, m.title, ''),
                    c.text, c.start_ms, c.embedding
             FROM embedding_chunks c
             LEFT JOIN notes n  ON c.owner_kind = 'note' AND n.id = c.owner_id
             LEFT JOIN meetings m ON c.owner_kind = 'transcript' AND m.id = c.owner_id
             LEFT JOIN notes mn ON mn.id = m.note_id
             WHERE c.embedding IS NOT NULL AND c.model = ?1
               AND (c.owner_kind != 'note' OR n.id IS NOT NULL)
               AND (?2 IS NULL OR COALESCE(n.folder_id, mn.folder_id) = ?2)
               AND (?3 IS NULL OR COALESCE(m.started_at, n.updated_at) >= ?3)
               AND (?4 IS NULL OR COALESCE(m.started_at, n.updated_at) <= ?4)",
        )?;
        let rows = stmt.query_map(rusqlite::params![model, folder, &since, &until], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Vec<u8>>(6)?,
            ))
        })?;

        let mut hits: Vec<VectorHit> = Vec::new();
        for row in rows {
            let (kind, note_id, meeting_id, title, text, start_ms, blob) = row?;
            let v = decode_vector(&blob);
            if v.len() != qnorm.len() {
                continue; // vector from an older/mismatched model dimension
            }
            hits.push(VectorHit {
                kind: if kind == "note" {
                    crate::SearchHitKind::Note
                } else {
                    crate::SearchHitKind::Transcript
                },
                note_id,
                meeting_id,
                title,
                snippet: snippet_of(&text),
                start_ms: start_ms.map(|v| v as u64),
                score: dot(&qnorm, &v),
            });
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit);
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_storage;
    use fly_core::{Speaker, Transcript, TranscriptSegment};

    fn seg(id: &str, key: &str, start_ms: u64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: id.into(),
            speaker_key: key.into(),
            start_ms,
            end_ms: start_ms + 1000,
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
            speakers: vec![
                Speaker {
                    key: "mic".into(),
                    label: "You".into(),
                },
                Speaker {
                    key: "spk_0".into(),
                    label: "Karim".into(),
                },
            ],
        }
    }

    #[test]
    fn transcript_chunks_carry_speaker_labels_and_start_ms() {
        let t = transcript(
            "m1",
            vec![
                seg("s1", "mic", 0, "let's talk pricing"),
                seg("s2", "mic", 1000, "specifically the fuel bid"),
                seg("s3", "spk_0", 2000, "I think we should go lower"),
            ],
        );
        let chunks = chunk_transcript(&t);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0]
            .text
            .contains("You: let's talk pricing specifically the fuel bid"));
        assert!(chunks[0].text.contains("Karim: I think we should go lower"));
        assert_eq!(chunks[0].start_ms, Some(0));
    }

    #[test]
    fn long_transcript_splits_at_turn_boundaries() {
        let long_a = "alpha ".repeat(120); // 120 words
        let long_b = "beta ".repeat(120);
        let t = transcript(
            "m1",
            vec![
                seg("s1", "mic", 0, long_a.trim()),
                seg("s2", "spk_0", 5000, long_b.trim()),
                seg("s3", "mic", 9000, &"gamma ".repeat(50)),
            ],
        );
        let chunks = chunk_transcript(&t);
        assert!(chunks.len() >= 2, "got {} chunks", chunks.len());
        // each chunk starts at a turn boundary with its speaker label
        for c in &chunks {
            assert!(c.text.starts_with("You:") || c.text.starts_with("Karim:"));
        }
        assert_eq!(chunks[0].start_ms, Some(0));
        assert!(chunks[1].start_ms.unwrap() >= 5000);
    }

    #[test]
    fn note_chunks_pack_paragraphs_and_skip_empty() {
        let mut note = fly_core::Note {
            id: "n1".into(),
            title: "Budget".into(),
            folder_id: None,
            meeting_id: None,
            scratchpad: "first para about budgets\n\nsecond para about hiring".into(),
            blocks: vec![fly_core::NoteBlock::user("an enhanced block")],
            attachments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let chunks = chunk_note(&note);
        assert_eq!(chunks.len(), 1); // small — all packed together
        assert!(chunks[0].text.contains("budgets"));
        assert!(chunks[0].text.contains("enhanced block"));

        note.scratchpad = String::new();
        note.blocks = vec![];
        assert!(chunk_note(&note).is_empty(), "empty note → no chunks");
    }

    #[test]
    fn encode_normalizes_and_dot_is_cosine() {
        let blob = encode_vector(&[3.0, 4.0]);
        let v = decode_vector(&blob);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
        assert!((dot(&v, &v) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn sync_pending_store_search_roundtrip() {
        let (_dir, s) = test_storage();
        let note = s.create_note("Fuel pricing", None).unwrap();
        s.update_note_scratchpad(&note.id, "we discussed the fuel bid with Karim")
            .unwrap();

        // chunk exists and is pending
        let pending = s.pending_embedding_chunks("test-model", 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].title, "Fuel pricing");
        assert_eq!(s.embedding_backlog("test-model").unwrap(), 1);

        // store a vector; backlog drains
        s.store_chunk_embeddings("test-model", &[(pending[0].id, vec![1.0, 0.0])])
            .unwrap();
        assert_eq!(s.embedding_backlog("test-model").unwrap(), 0);

        // similar query vector finds it, orthogonal one scores lower
        let hits = s
            .vector_search(&[0.9, 0.1], "test-model", &SearchFilter::default(), 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, note.id);
        assert!(hits[0].score > 0.9);
        assert!(hits[0].snippet.contains("fuel bid"));

        // a different model sees only backlog, no hits
        assert_eq!(s.embedding_backlog("other-model").unwrap(), 1);
        assert!(s
            .vector_search(&[1.0, 0.0], "other-model", &SearchFilter::default(), 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn unchanged_resync_keeps_vectors_changed_resets_to_pending() {
        let (_dir, s) = test_storage();
        let note = s.create_note("n", None).unwrap();
        s.update_note_scratchpad(&note.id, "original body text here")
            .unwrap();
        let pending = s.pending_embedding_chunks("m", 10).unwrap();
        s.store_chunk_embeddings("m", &[(pending[0].id, vec![1.0, 0.0])])
            .unwrap();

        // no-op write: vector survives
        s.update_note_scratchpad(&note.id, "original body text here")
            .unwrap();
        assert_eq!(s.embedding_backlog("m").unwrap(), 0);

        // real edit: chunk back to pending
        s.update_note_scratchpad(&note.id, "completely different content now")
            .unwrap();
        assert_eq!(s.embedding_backlog("m").unwrap(), 1);
    }

    #[test]
    fn deleting_note_removes_its_chunks() {
        let (_dir, s) = test_storage();
        let note = s.create_note("gone", None).unwrap();
        s.update_note_scratchpad(&note.id, "some body").unwrap();
        assert_eq!(s.embedding_backlog("m").unwrap(), 1);
        s.delete_note(&note.id).unwrap();
        assert_eq!(s.embedding_backlog("m").unwrap(), 0);
    }

    #[test]
    fn transcript_chunks_sync_on_save_and_filters_apply() {
        let (_dir, s) = test_storage();
        let folder = s.create_folder("Sales", None).unwrap();
        let note = s.create_note("Airline deal", Some(&folder.id)).unwrap();
        let meeting = s.create_meeting("Airline deal", &note.id, &[]).unwrap();
        let t = transcript(
            &meeting.id,
            vec![seg("s1", "spk_0", 0, "the fuel bid needs a sharper price")],
        );
        s.save_transcript(&t).unwrap();

        let pending = s.pending_embedding_chunks("m", 10).unwrap();
        // note has no body → only the transcript chunk
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].title, "Airline deal");
        s.store_chunk_embeddings("m", &[(pending[0].id, vec![1.0, 0.0])])
            .unwrap();

        let all = s
            .vector_search(&[1.0, 0.0], "m", &SearchFilter::default(), 10)
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].note_id, note.id);
        assert_eq!(all[0].meeting_id.as_deref(), Some(meeting.id.as_str()));
        assert_eq!(all[0].kind, crate::SearchHitKind::Transcript);

        // folder filter: matches through the owning note
        let in_folder = s
            .vector_search(
                &[1.0, 0.0],
                "m",
                &SearchFilter {
                    folder_id: Some(folder.id.clone()),
                    ..Default::default()
                },
                10,
            )
            .unwrap();
        assert_eq!(in_folder.len(), 1);
        let other = s
            .vector_search(
                &[1.0, 0.0],
                "m",
                &SearchFilter {
                    folder_id: Some("nope".into()),
                    ..Default::default()
                },
                10,
            )
            .unwrap();
        assert!(other.is_empty());

        // date filter: transcript hits gate on meeting started_at
        let future = s
            .vector_search(
                &[1.0, 0.0],
                "m",
                &SearchFilter {
                    since: Some(chrono::Utc::now() + chrono::Duration::days(1)),
                    ..Default::default()
                },
                10,
            )
            .unwrap();
        assert!(future.is_empty());
    }

    #[test]
    fn backfill_chunks_existing_content() {
        let (_dir, s) = test_storage();
        let note = s.create_note("legacy", None).unwrap();
        s.update_note_scratchpad(&note.id, "pre-feature content")
            .unwrap();
        // simulate pre-feature state: wipe the chunk rows
        s.conn.execute("DELETE FROM embedding_chunks", []).unwrap();
        assert_eq!(s.embedding_backlog("m").unwrap(), 0);

        s.backfill_embedding_chunks().unwrap();
        assert_eq!(s.embedding_backlog("m").unwrap(), 1);
        // second run is a no-op
        s.backfill_embedding_chunks().unwrap();
        assert_eq!(s.embedding_backlog("m").unwrap(), 1);
    }
}
