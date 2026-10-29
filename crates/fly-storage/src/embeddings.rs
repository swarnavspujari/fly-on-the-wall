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
//!
//! Since schema v3, transcript chunks are label-free (speaker names are
//! display data that a rename changes; a rename now re-embeds nothing),
//! record their exact span (`segment_ids_json`, `start_ms`, `end_ms`), and
//! embed each segment's polished text when the polished transcript has it
//! (`variant`).

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
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Chunk {
    /// Display/snippet text — exactly what was on screen, no prompt wrapping.
    /// Transcript chunks: segment texts, one line per speaker turn, no labels.
    pub text: String,
    /// For transcript chunks: start of the window (deep-link position).
    pub start_ms: Option<u64>,
    /// For transcript chunks: end of the window's last segment.
    pub end_ms: Option<u64>,
    /// For transcript chunks: the segments in the window, in order.
    pub segment_ids: Vec<String>,
    /// For transcript chunks: at least one segment's text came from the
    /// polished transcript.
    pub polished: bool,
}

impl Chunk {
    fn variant(&self) -> &'static str {
        if self.polished {
            "polished"
        } else {
            "raw"
        }
    }
}

/// Something whose content is chunked for semantic search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkOwner {
    /// A note (scratchpad + enhanced blocks), by note id.
    Note(String),
    /// A meeting's transcript, by meeting id.
    Transcript(String),
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
    /// Transcript hits: the chunk's segments, in order. Empty for notes, and
    /// for a chunk an older build wrote (span unknown until re-chunked).
    #[serde(default)]
    pub segment_ids: Vec<String>,
    #[serde(default)]
    pub end_ms: Option<u64>,
    /// The whole chunk text (`snippet` is its first ~160 chars).
    #[serde(default)]
    pub text: String,
}

/// [`Storage::vector_search_floor`]'s result.
#[derive(Debug, Clone, Default)]
pub struct VectorResults {
    /// Hits at or above the floor, best first, at most `limit`.
    pub hits: Vec<VectorHit>,
    /// The best cosine of any in-scope chunk, floor or not (`None` when no
    /// chunk in scope is embedded with this model).
    pub best_score: Option<f32>,
}

/// The app's embedding model (EmbeddingGemma 300M via Ollama): the same tag
/// as `fly_core::embed_prompt::EMBED_MODEL`, which the app and the eval use.
pub const EMBEDDINGGEMMA: &str = fly_core::embed_prompt::EMBED_MODEL;

/// Cosine below which a chunk embedded by `model` isn't a match. With no
/// floor every query "finds" the nearest chunks however unrelated, so a
/// question nothing answers still returns passages, and every keyword
/// candidate also collects a semantic vote. Measured per model on the
/// retrieval gold set (see `docs/components/fly-context.md`); `None` (no
/// floor) for a model nobody has measured.
pub fn similarity_floor(model: &str) -> Option<f32> {
    match model {
        EMBEDDINGGEMMA => Some(EMBEDDINGGEMMA_FLOOR),
        _ => None,
    }
}

/// EmbeddingGemma's floor, for the app's prompts (document
/// `title: … | text: …`, query `task: search result | query: …`).
const EMBEDDINGGEMMA_FLOOR: f32 = 0.35;

// ---------------------------------------------------------------------------
// Chunkers (pure)
// ---------------------------------------------------------------------------

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Split a transcript into speaker-turn windows: consecutive segments merge
/// into turns, and turns pack into ~CHUNK_TARGET_WORDS windows that only
/// break at turn boundaries. Text is label-free (one line per turn); see
/// [`chunk_transcript_variant`] for the polished-text form.
pub fn chunk_transcript(t: &Transcript) -> Vec<Chunk> {
    chunk_transcript_variant(t, None)
}

/// [`chunk_transcript`] over the text the index should hold: each segment
/// of `raw` uses the text of the `cleaned` segment with the same id when
/// there is one (polished), else its own. Ids, speakers and times always
/// come from `raw`, so a stale polished variant from an earlier run (other
/// ids) contributes nothing.
///
/// Boundaries come from the RAW text (which segments are non-empty, how
/// many words each turn has), so polishing a transcript changes chunk text
/// but never which segments a chunk spans: the spans stay identical to the
/// raw windows, and `sync_chunks` can keep each chunk's vector through a
/// polish instead of re-embedding the meeting.
pub fn chunk_transcript_variant(raw: &Transcript, cleaned: Option<&Transcript>) -> Vec<Chunk> {
    let polished: std::collections::HashMap<&str, &str> = cleaned
        .map(|c| {
            c.segments
                .iter()
                .map(|s| (s.id.as_str(), s.text.as_str()))
                .collect()
        })
        .unwrap_or_default();

    // 1. segments → turns (same speaker, contiguous)
    struct Turn {
        key: String,
        line: String,
        start_ms: u64,
        end_ms: u64,
        words: usize,
        ids: Vec<String>,
        polished: bool,
    }
    let mut turns: Vec<Turn> = Vec::new();
    for seg in &raw.segments {
        let raw_text = seg.text.trim();
        if raw_text.is_empty() {
            continue;
        }
        // A polish may empty a filler-only segment: it keeps its place in
        // the span, it just adds no text.
        let (text, is_polished) = match polished.get(seg.id.as_str()) {
            Some(p) => (p.trim(), true),
            None => (raw_text, false),
        };
        match turns.last_mut() {
            Some(last) if last.key == seg.speaker_key => {
                if !text.is_empty() {
                    if !last.line.is_empty() {
                        last.line.push(' ');
                    }
                    last.line.push_str(text);
                }
                last.words += word_count(raw_text);
                last.end_ms = last.end_ms.max(seg.end_ms);
                last.ids.push(seg.id.clone());
                last.polished |= is_polished;
            }
            _ => turns.push(Turn {
                key: seg.speaker_key.clone(),
                line: text.to_string(),
                start_ms: seg.start_ms,
                end_ms: seg.end_ms,
                words: word_count(raw_text),
                ids: vec![seg.id.clone()],
                polished: is_polished,
            }),
        }
    }

    // 2. turns → windows
    fn append(c: &mut Chunk, turn: &Turn) {
        if c.segment_ids.is_empty() {
            c.start_ms = Some(turn.start_ms);
        }
        if !turn.line.is_empty() {
            if !c.text.is_empty() {
                c.text.push('\n');
            }
            c.text.push_str(&turn.line);
        }
        c.end_ms = Some(c.end_ms.unwrap_or(0).max(turn.end_ms));
        c.segment_ids.extend(turn.ids.iter().cloned());
        c.polished |= turn.polished;
    }
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut cur = Chunk::default();
    let mut cur_words = 0usize;
    for turn in &turns {
        append(&mut cur, turn);
        cur_words += turn.words;
        if cur_words >= CHUNK_TARGET_WORDS {
            chunks.push(std::mem::take(&mut cur));
            cur_words = 0;
        }
    }
    if !cur.segment_ids.is_empty() {
        match chunks.last_mut() {
            Some(last) if cur_words < CHUNK_MIN_TAIL_WORDS => {
                if !cur.text.is_empty() {
                    if !last.text.is_empty() {
                        last.text.push('\n');
                    }
                    last.text.push_str(&cur.text);
                }
                last.end_ms = last.end_ms.max(cur.end_ms);
                last.segment_ids.extend(cur.segment_ids);
                last.polished |= cur.polished;
            }
            _ => chunks.push(cur),
        }
    }
    // A window a polish emptied entirely has nothing to embed.
    chunks.retain(|c| !c.text.is_empty());
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
                    ..Default::default()
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
                ..Default::default()
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
    let (chunks, _tail) = blob.as_chunks::<4>();
    chunks.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

#[cfg(test)]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Dot product of `a` with a stored little-endian f32 BLOB of the same
/// length, without decoding it into a new vector.
fn dot_blob(a: &[f32], blob: &[u8]) -> f32 {
    let (chunks, _tail) = blob.as_chunks::<4>();
    a.iter()
        .zip(chunks)
        .map(|(x, b)| x * f32::from_le_bytes(*b))
        .sum()
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

    /// Re-chunk a transcript (`t` is the raw variant; the stored polished
    /// variant, if any, supplies text by segment id). The owning meeting's
    /// title (the note's title is snapshotted there) becomes the embedding
    /// context.
    pub(crate) fn sync_transcript_chunks(&self, t: &Transcript) -> Result<()> {
        let title = self
            .get_meeting(&t.meeting_id)
            .map(|m| m.title)
            .unwrap_or_default();
        // An unreadable polished variant must not block the raw write.
        let cleaned = self.get_cleaned_transcript(&t.meeting_id).ok().flatten();
        let chunks = chunk_transcript_variant(t, cleaned.as_ref());
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
            "SELECT chunk_index, title, text, embedding, model,
                    segment_ids_json, end_ms, variant
             FROM embedding_chunks
             WHERE owner_kind = ?1 AND owner_id = ?2 ORDER BY chunk_index",
        )?;
        struct Row {
            title: String,
            text: String,
            embedding: Option<Vec<u8>>,
            model: Option<String>,
            span: (String, Option<i64>, String),
        }
        let existing: Vec<Row> = stmt
            .query_map((owner_kind, owner_id), |r| {
                Ok(Row {
                    title: r.get(1)?,
                    text: r.get(2)?,
                    embedding: r.get(3)?,
                    model: r.get(4)?,
                    span: (r.get(5)?, r.get(6)?, r.get(7)?),
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        let span_of = |c: &Chunk| -> Result<(String, Option<i64>, String)> {
            Ok((
                serde_json::to_string(&c.segment_ids)?,
                c.end_ms.map(|v| v as i64),
                c.variant().to_string(),
            ))
        };
        let spans: Vec<(String, Option<i64>, String)> =
            chunks.iter().map(span_of).collect::<Result<_>>()?;

        let unchanged = existing.len() == chunks.len()
            && existing
                .iter()
                .zip(chunks.iter().zip(&spans))
                .all(|(row, (c, span))| {
                    row.title == title && row.text == c.text && &row.span == span
                });
        if unchanged {
            return Ok(());
        }

        // Content-keyed vector reuse: an edit that shifts chunk boundaries
        // for one window must not re-embed the rest.
        let mut by_content: std::collections::HashMap<(String, String), (Vec<u8>, String)> =
            std::collections::HashMap::new();
        // Span-keyed reuse through a polish: the same segments (and title)
        // whose text changed only between raw and polished keep their
        // vector. A raw-to-raw change on the same ids is a real edit and
        // re-embeds.
        let mut by_span: std::collections::HashMap<(String, String), (Vec<u8>, String, bool)> =
            std::collections::HashMap::new();
        for row in existing {
            if let (Some(emb), Some(model)) = (row.embedding, row.model) {
                let polished = row.span.2 == "polished";
                by_span.insert(
                    (row.title.clone(), row.span.0.clone()),
                    (emb.clone(), model.clone(), polished),
                );
                by_content.insert((row.title, row.text), (emb, model));
            }
        }

        self.conn.execute(
            "DELETE FROM embedding_chunks WHERE owner_kind = ?1 AND owner_id = ?2",
            (owner_kind, owner_id),
        )?;
        let mut insert = self.conn.prepare(
            "INSERT INTO embedding_chunks
             (owner_kind, owner_id, chunk_index, title, text, start_ms, embedding, model,
              segment_ids_json, end_ms, variant)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?;
        for (i, (c, (ids, end_ms, variant))) in chunks.iter().zip(spans).enumerate() {
            let reuse: Option<(&Vec<u8>, &String)> = by_content
                .get(&(title.to_string(), c.text.clone()))
                .map(|(emb, model)| (emb, model))
                .or_else(|| {
                    let (emb, model, was_polished) =
                        by_span.get(&(title.to_string(), ids.clone()))?;
                    (*was_polished || c.polished).then_some((emb, model))
                });
            insert.execute(rusqlite::params![
                owner_kind,
                owner_id,
                i as i64,
                title,
                &c.text,
                c.start_ms.map(|v| v as i64),
                reuse.map(|(emb, _)| emb.clone()),
                reuse.map(|(_, m)| m.clone()),
                ids,
                end_ms,
                variant,
            ])?;
        }
        Ok(())
    }

    /// Chunks with no vector yet (or a vector from another model), most
    /// recently written first — the embedding worker's work queue. Every
    /// (re)chunk inserts fresh rows, so a meeting that was just transcribed
    /// or a note that was just edited is embedded ahead of an older backlog
    /// (such as the one-time re-embed after the v3 upgrade).
    pub fn pending_embedding_chunks(&self, model: &str, limit: usize) -> Result<Vec<PendingChunk>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, text FROM embedding_chunks
             WHERE embedding IS NULL OR model IS NOT ?1
             ORDER BY id DESC LIMIT ?2",
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

    /// Whether any chunk waits for a vector from `model`: the worker's
    /// per-batch check. It looks from the newest row down and stops at the
    /// first pending one, where [`embedding_backlog`](Self::embedding_backlog)
    /// counts every row (each embedded row's vector sits in overflow pages
    /// that a scan walks).
    pub fn has_pending_embeddings(&self, model: &str) -> Result<bool> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM embedding_chunks
                  WHERE embedding IS NULL OR model IS NOT ?1
                  ORDER BY id DESC LIMIT 1",
                [model],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
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

    /// Store vectors for chunks handed out by
    /// [`pending_embedding_chunks`](Self::pending_embedding_chunks) (normalized
    /// here), but only where the row still holds the text that was embedded.
    /// Embedding runs with no storage lock held, so a chunk can be re-chunked
    /// meanwhile, and SQLite may give the new row the old id: a vector of the
    /// old text must not land on it. Returns how many were stored; the rest
    /// stay pending.
    pub fn store_pending_embeddings(
        &self,
        model: &str,
        vectors: &[(PendingChunk, Vec<f32>)],
    ) -> Result<usize> {
        let mut stmt = self.conn.prepare(
            "UPDATE embedding_chunks SET embedding = ?1, model = ?2
              WHERE id = ?3 AND title = ?4 AND text = ?5",
        )?;
        let mut stored = 0;
        for (chunk, v) in vectors {
            stored += stmt.execute(rusqlite::params![
                encode_vector(v),
                model,
                chunk.id,
                &chunk.title,
                &chunk.text
            ])?;
        }
        Ok(stored)
    }

    /// Every note and transcript that should have chunk rows, oldest first
    /// (notes by `updated_at`, meetings by `started_at`): the startup pass
    /// walks them a few at a time ([`sync_owner_chunks`](Self::sync_owner_chunks)),
    /// releasing the storage lock in between. Oldest first because a
    /// re-chunk gives the owner's new rows the next ids and the embedding
    /// queue takes the highest ids first: the newest owner is embedded first.
    pub fn chunk_owners(&self) -> Result<Vec<ChunkOwner>> {
        let mut owners: Vec<(String, ChunkOwner)> = Vec::new();
        let mut stmt = self.conn.prepare("SELECT id, updated_at FROM notes")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (id, at) = row?;
            owners.push((at, ChunkOwner::Note(id)));
        }
        let mut stmt = self.conn.prepare(
            "SELECT t.meeting_id, COALESCE(m.started_at, '')
               FROM transcripts t LEFT JOIN meetings m ON m.id = t.meeting_id",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (id, at) = row?;
            owners.push((at, ChunkOwner::Transcript(id)));
        }
        owners.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(owners.into_iter().map(|(_, o)| o).collect())
    }

    /// Re-chunk one owner (a no-op when its content is unchanged). An owner
    /// deleted since [`chunk_owners`](Self::chunk_owners) listed it is skipped.
    pub fn sync_owner_chunks(&self, owner: &ChunkOwner) -> Result<()> {
        match owner {
            ChunkOwner::Note(id) => match self.get_note(id) {
                Ok(note) => self.sync_note_chunks(&note),
                Err(crate::StorageError::NotFound(_)) => Ok(()),
                Err(e) => Err(e),
            },
            ChunkOwner::Transcript(id) => match self.get_transcript(id)? {
                Some(t) => self.sync_transcript_chunks(&t),
                None => Ok(()),
            },
        }
    }

    /// Ensure every note and transcript has chunk rows — the startup
    /// backfill for content written before this feature existed. Idempotent
    /// and cheap when everything is already chunked (`sync_chunks` no-ops on
    /// unchanged content). The app walks [`chunk_owners`](Self::chunk_owners)
    /// itself to release its lock between owners; this is the one-call form.
    pub fn backfill_embedding_chunks(&self) -> Result<usize> {
        let owners = self.chunk_owners()?;
        for owner in &owners {
            self.sync_owner_chunks(owner)?;
        }
        Ok(owners.len())
    }

    /// Brute-force cosine scan over embedded chunks, honoring the same
    /// folder/date semantics as FTS: note chunks filter on the note's
    /// folder/updated_at, transcript chunks on the owning note's folder and
    /// the meeting's started_at. `query` need not be normalized.
    ///
    /// Every filter, `filter.exclude_keep_local` included, is applied inside
    /// the query, before `limit` — so hidden chunks can't crowd shareable
    /// ones out of the top `limit`. Chunks whose owner row is gone are never
    /// returned; under `exclude_keep_local`, neither is a transcript chunk
    /// whose meeting has no note row (its privacy can't be checked).
    pub fn vector_search(
        &self,
        query: &[f32],
        model: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        Ok(self
            .vector_search_floor(query, model, filter, limit, None)?
            .hits)
    }

    /// [`vector_search`](Self::vector_search) that also drops hits scoring
    /// below `floor` (before `limit`) and reports the best score in scope
    /// whether or not it passed: a "nothing close" signal for callers.
    ///
    /// Ranks on cached, quantized vectors, then re-scores the best
    /// candidates exactly from the stored vectors (through the same scoped
    /// SQL, so `best_score` only ever comes from a chunk in scope) and reads
    /// text and spans for the `limit` winners alone, re-applying every
    /// filter. All of it runs on one read snapshot. Scores are exact for
    /// the first 256 candidates; a caller asking for more (a meeting-scoped
    /// search takes every hit) gets approximate scores (|error| ≈ 1e-3)
    /// past them.
    pub fn vector_search_floor(
        &self,
        query: &[f32],
        model: &str,
        filter: &SearchFilter,
        limit: usize,
        floor: Option<f32>,
    ) -> Result<VectorResults> {
        self.read_snapshot(|| self.vector_search_in_snapshot(query, model, filter, limit, floor))
    }

    fn vector_search_in_snapshot(
        &self,
        query: &[f32],
        model: &str,
        filter: &SearchFilter,
        limit: usize,
        floor: Option<f32>,
    ) -> Result<VectorResults> {
        let qnorm = decode_vector(&encode_vector(query));
        // Rank on the cached, quantized vectors, over the chunks whose owner
        // this search may see (decided by the database on every call), then
        // re-score exactly: the best candidate always (so `best_score` is
        // exact), and those within the quantization margin of the floor, a
        // few past `limit` (so the error can't reorder hits across the cut).
        let matrix = self.vector_matrix(model, qnorm.len())?;
        let visible = self.visible_owners(&matrix.owners, filter)?;
        let mut approx: Vec<(f32, i64)> = (0..matrix.len())
            .filter(|&i| visible[matrix.owner_of[i] as usize])
            .map(|i| (matrix.score(i, &qnorm), matrix.ids[i]))
            .collect();
        approx.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let margin = floor.map(|f| f - QUANT_MARGIN);
        let candidates: Vec<(f32, i64)> = approx
            .into_iter()
            .enumerate()
            .filter(|(rank, (s, _))| *rank == 0 || margin.is_none_or(|m| *s >= m))
            .take(limit.saturating_add(RESCORE_EXTRA))
            .map(|(_, c)| c)
            .collect();
        // A caller asking for every hit (a meeting-scoped search) keeps
        // approximate scores past the first RESCORE_MAX.
        let head: Vec<i64> = candidates
            .iter()
            .take(RESCORE_MAX)
            .map(|(_, id)| *id)
            .collect();
        let mut scored = self.exact_scores(&qnorm, model, filter, &head)?;
        let best_score = scored.first().map(|(s, _)| *s);
        scored.extend(candidates.into_iter().skip(RESCORE_MAX));
        if let Some(f) = floor {
            scored.retain(|(s, _)| *s >= f);
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(limit);
        if scored.is_empty() {
            return Ok(VectorResults {
                hits: vec![],
                best_score,
            });
        }

        let ids: Vec<i64> = scored.iter().map(|(_, id)| *id).collect();
        let (sql, params) = chunk_scope_sql(
            "c.id, c.owner_kind,
             COALESCE(n.id, m.note_id, ''),
             CASE c.owner_kind WHEN 'transcript' THEN c.owner_id END,
             COALESCE(n.title, m.title, ''),
             c.text, c.start_ms, c.segment_ids_json, c.end_ms",
            model,
            filter,
            &ids,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut by_id: std::collections::HashMap<i64, VectorHit> = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                let kind: String = r.get(1)?;
                let text: String = r.get(5)?;
                let ids: String = r.get(7)?;
                Ok((
                    r.get::<_, i64>(0)?,
                    VectorHit {
                        kind: if kind == "note" {
                            crate::SearchHitKind::Note
                        } else {
                            crate::SearchHitKind::Transcript
                        },
                        note_id: r.get(2)?,
                        meeting_id: r.get(3)?,
                        title: r.get(4)?,
                        snippet: snippet_of(&text),
                        start_ms: r.get::<_, Option<i64>>(6)?.map(|v| v.max(0) as u64),
                        score: 0.0,
                        segment_ids: serde_json::from_str(&ids).unwrap_or_default(),
                        end_ms: r.get::<_, Option<i64>>(8)?.map(|v| v.max(0) as u64),
                        text,
                    },
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let hits = scored
            .into_iter()
            .filter_map(|(score, id)| {
                by_id.remove(&id).map(|mut h| {
                    h.score = score;
                    h
                })
            })
            .collect();
        Ok(VectorResults { hits, best_score })
    }

    /// For each owner, whether `filter` lets a search see its chunks: the
    /// owner-level form of [`chunk_scope_sql`]'s conditions (note chunks on
    /// the note's folder and `updated_at`; transcript chunks on the meeting's
    /// note's folder and `started_at`; under `exclude_keep_local` only
    /// shareable owners, and a meeting only while its note exists; an owner
    /// kind it doesn't know only with no filter at all). The winners' read
    /// re-applies the chunk-level SQL, so a mismatch could only hide hits,
    /// never leak them.
    fn visible_owners(
        &self,
        owners: &[(crate::cache::OwnerKind, String)],
        filter: &SearchFilter,
    ) -> Result<Vec<bool>> {
        use crate::cache::OwnerKind;
        let (cte, note_local, meeting_local) = if filter.exclude_keep_local {
            (
                crate::privacy::KEEP_LOCAL_CTE,
                " AND n.id NOT IN (SELECT id FROM local_notes)",
                " AND mn.id IS NOT NULL AND m.note_id NOT IN (SELECT id FROM local_notes)",
            )
        } else {
            ("", "", "")
        };
        let params = rusqlite::params![
            filter.folder_id.as_deref(),
            filter.since.map(|t| t.to_rfc3339()),
            filter.until.map(|t| t.to_rfc3339())
        ];
        let ids = |sql: String| -> Result<std::collections::HashSet<String>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params, |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        };
        let notes = ids(format!(
            "{cte}SELECT n.id FROM notes n
              WHERE (?1 IS NULL OR n.folder_id = ?1)
                AND (?2 IS NULL OR n.updated_at >= ?2)
                AND (?3 IS NULL OR n.updated_at <= ?3){note_local}"
        ))?;
        let meetings = ids(format!(
            "{cte}SELECT m.id FROM meetings m LEFT JOIN notes mn ON mn.id = m.note_id
              WHERE (?1 IS NULL OR mn.folder_id = ?1)
                AND (?2 IS NULL OR m.started_at >= ?2)
                AND (?3 IS NULL OR m.started_at <= ?3){meeting_local}"
        ))?;
        let unfiltered = !filter.exclude_keep_local
            && filter.folder_id.is_none()
            && filter.since.is_none()
            && filter.until.is_none();
        Ok(owners
            .iter()
            .map(|(kind, id)| match kind {
                OwnerKind::Note => notes.contains(id),
                OwnerKind::Transcript => meetings.contains(id),
                OwnerKind::Other => unfiltered,
            })
            .collect())
    }
}

impl Storage {
    /// Exact cosines (from the stored f32 vectors) for `ids`, best first.
    /// Exact scores of `ids` that are still in scope under `filter`.
    fn exact_scores(
        &self,
        qnorm: &[f32],
        model: &str,
        filter: &SearchFilter,
        ids: &[i64],
    ) -> Result<Vec<(f32, i64)>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let (sql, params) = chunk_scope_sql("c.id, c.embedding", model, filter, ids);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
        let mut out: Vec<(f32, i64)> = Vec::with_capacity(ids.len());
        while let Some(r) = rows.next()? {
            let rusqlite::types::ValueRef::Blob(blob) = r.get_ref(1)? else {
                continue;
            };
            if blob.len() == qnorm.len() * 4 {
                out.push((dot_blob(qnorm, blob), r.get::<_, i64>(0)?));
            }
        }
        out.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        Ok(out)
    }
}

/// Quantized scores this close under the floor are re-scored exactly.
const QUANT_MARGIN: f32 = 0.01;
/// Candidates re-scored past `limit`, so quantization error can't reorder
/// hits across the cut.
const RESCORE_EXTRA: usize = 16;
/// At most this many candidates are re-scored exactly (every unscoped
/// search fits: 200 + RESCORE_EXTRA).
const RESCORE_MAX: usize = 256;

/// `SELECT {columns}` over the embedded chunks of `model` that pass
/// `filter` (see [`Storage::vector_search`]), restricted to `ids` when
/// given. Returns the SQL and its positional parameters.
fn chunk_scope_sql(
    columns: &str,
    model: &str,
    filter: &SearchFilter,
    ids: &[i64],
) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    let (cte, shareable) = if filter.exclude_keep_local {
        (
            crate::privacy::KEEP_LOCAL_CTE,
            format!(
                " AND (c.owner_kind != 'transcript' OR mn.id IS NOT NULL) AND {}",
                crate::privacy::SHAREABLE_CHUNK
            ),
        )
    } else {
        ("", String::new())
    };
    let only = if ids.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        format!(" AND c.id IN ({})", list.join(","))
    };
    let text = |v: Option<String>| v.map(Value::Text).unwrap_or(Value::Null);
    let params = vec![
        Value::Text(model.to_string()),
        text(filter.folder_id.clone()),
        text(filter.since.map(|t| t.to_rfc3339())),
        text(filter.until.map(|t| t.to_rfc3339())),
    ];
    (
        format!(
            "{cte}SELECT {columns}
             FROM embedding_chunks c
             LEFT JOIN notes n  ON c.owner_kind = 'note' AND n.id = c.owner_id
             LEFT JOIN meetings m ON c.owner_kind = 'transcript' AND m.id = c.owner_id
             LEFT JOIN notes mn ON mn.id = m.note_id
             WHERE c.embedding IS NOT NULL AND c.model = ?1
               AND (c.owner_kind != 'note' OR n.id IS NOT NULL)
               AND (c.owner_kind != 'transcript' OR m.id IS NOT NULL)
               AND (?2 IS NULL OR COALESCE(n.folder_id, mn.folder_id) = ?2)
               AND (?3 IS NULL OR COALESCE(m.started_at, n.updated_at) >= ?3)
               AND (?4 IS NULL OR COALESCE(m.started_at, n.updated_at) <= ?4){shareable}{only}"
        ),
        params,
    )
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
    fn transcript_chunks_are_label_free_with_exact_spans() {
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
        assert_eq!(
            chunks[0].text,
            "let's talk pricing specifically the fuel bid\nI think we should go lower"
        );
        assert_eq!(chunks[0].start_ms, Some(0));
        assert_eq!(chunks[0].end_ms, Some(3000));
        assert_eq!(chunks[0].segment_ids, vec!["s1", "s2", "s3"]);
        assert!(!chunks[0].polished);

        // Polished text by segment id; ids/times from raw; unknown ids ignored.
        let mut cleaned = t.clone();
        cleaned.segments[2].text = "I think we should go lower.".into();
        cleaned.segments[0].id = "stale".into();
        let p = chunk_transcript_variant(&t, Some(&cleaned));
        assert!(p[0].text.ends_with("go lower."));
        assert!(p[0].polished);
        assert_eq!(p[0].segment_ids, chunks[0].segment_ids);
    }

    #[test]
    fn polishing_changes_text_but_never_spans() {
        // Long enough for several windows; the polish drops filler words,
        // which would shift polished-word-count boundaries.
        let mut segs = Vec::new();
        for i in 0..40 {
            let key = if (i / 4) % 2 == 0 { "mic" } else { "spk_0" };
            segs.push(seg(
                &format!("s{i}"),
                key,
                i as u64 * 1000,
                "um so uh we talked about the launch budget and you know the timeline",
            ));
        }
        segs.push(seg("s40", "spk_0", 40_000, "um"));
        let t = transcript("m1", segs);
        let mut cleaned = t.clone();
        for s in &mut cleaned.segments {
            s.text = "We talked about the launch budget and the timeline.".into();
        }
        cleaned.segments[40].text = String::new(); // a filler-only line
        let raw = chunk_transcript(&t);
        let polished = chunk_transcript_variant(&t, Some(&cleaned));
        assert!(raw.len() > 2, "several windows");
        assert_eq!(raw.len(), polished.len());
        for (r, p) in raw.iter().zip(&polished) {
            assert_eq!(r.segment_ids, p.segment_ids);
            assert_eq!((r.start_ms, r.end_ms), (p.start_ms, p.end_ms));
            assert!(p.polished && !p.text.contains("um so"));
        }
        assert!(
            !polished.iter().any(|c| c.text.contains("\n\n")),
            "no blank lines"
        );
    }

    #[test]
    fn a_polish_keeps_vectors_a_raw_edit_does_not() {
        let (_dir, s) = test_storage();
        let note = s.create_note("Launch", None).unwrap();
        let meeting = s.create_meeting("Launch", &note.id, &[]).unwrap();
        let t = transcript(
            &meeting.id,
            vec![
                seg("s1", "mic", 0, "um so the launch budget is ten thousand"),
                seg("s2", "spk_0", 1000, "uh and the date moves to may"),
            ],
        );
        s.save_transcript(&t).unwrap();
        let pending = s.pending_embedding_chunks("m", 10).unwrap();
        assert_eq!(pending.len(), 1);
        s.store_chunk_embeddings("m", &[(pending[0].id, vec![1.0, 0.0])])
            .unwrap();

        // The polish lands: text changes, the vector stays.
        let mut cleaned = t.clone();
        cleaned.segments[0].text = "The launch budget is ten thousand.".into();
        cleaned.segments[1].text = "And the date moves to May.".into();
        s.save_cleaned_transcript(&cleaned).unwrap();
        assert_eq!(
            s.embedding_backlog("m").unwrap(),
            0,
            "polish re-embeds nothing"
        );
        let hits = s
            .vector_search(&[1.0, 0.0], "m", &SearchFilter::default(), 10)
            .unwrap();
        assert!(hits[0].text.starts_with("The launch budget"));

        // A real edit of the raw text on a raw chunk re-embeds.
        let other = s.create_note("Other", None).unwrap();
        let m2 = s.create_meeting("Other", &other.id, &[]).unwrap();
        let mut t2 = transcript(&m2.id, vec![seg("x1", "mic", 0, "first words")]);
        s.save_transcript(&t2).unwrap();
        let p = s.pending_embedding_chunks("m", 10).unwrap();
        s.store_chunk_embeddings("m", &[(p[0].id, vec![0.0, 1.0])])
            .unwrap();
        t2.segments[0].text = "corrected words".into();
        s.save_transcript(&t2).unwrap();
        assert_eq!(s.embedding_backlog("m").unwrap(), 1);
    }

    #[test]
    fn the_queue_hands_out_the_newest_writes_first() {
        let (_dir, s) = test_storage();
        let old = s.create_note("Old", None).unwrap();
        s.update_note_scratchpad(&old.id, "old body").unwrap();
        let new = s.create_note("New", None).unwrap();
        s.update_note_scratchpad(&new.id, "new body").unwrap();
        let q = s.pending_embedding_chunks("m", 10).unwrap();
        assert_eq!(q[0].title, "New");
        // Editing the old note puts it back at the front.
        s.update_note_scratchpad(&old.id, "old body, edited")
            .unwrap();
        let q = s.pending_embedding_chunks("m", 10).unwrap();
        assert_eq!(q[0].title, "Old");
    }

    #[test]
    fn a_vector_for_replaced_text_is_never_stored() {
        let (_dir, s) = test_storage();
        let note = s.create_note("n", None).unwrap();
        s.update_note_scratchpad(&note.id, "first").unwrap();
        let handed_out = s.pending_embedding_chunks("m", 10).unwrap();
        // Re-chunked while the worker embedded it: SQLite reuses the max id
        // for the new row.
        s.update_note_scratchpad(&note.id, "second").unwrap();
        let now = s.pending_embedding_chunks("m", 10).unwrap();
        assert_eq!(now[0].id, handed_out[0].id, "same id, new text");
        let stored = s
            .store_pending_embeddings("m", &[(handed_out[0].clone(), vec![1.0, 0.0])])
            .unwrap();
        assert_eq!(stored, 0);
        assert_eq!(s.embedding_backlog("m").unwrap(), 1);
        let stored = s
            .store_pending_embeddings("m", &[(now[0].clone(), vec![1.0, 0.0])])
            .unwrap();
        assert_eq!(stored, 1);
        assert_eq!(s.embedding_backlog("m").unwrap(), 0);
    }

    #[test]
    fn chunk_owners_oldest_first_and_deleted_owners_skip() {
        let (_dir, s) = test_storage();
        let a = s.create_note("a", None).unwrap();
        let b = s.create_note("b", None).unwrap();
        s.update_note_scratchpad(&a.id, "touched last").unwrap();
        let owners = s.chunk_owners().unwrap();
        assert_eq!(owners.last(), Some(&ChunkOwner::Note(a.id.clone())));
        assert!(owners.contains(&ChunkOwner::Note(b.id.clone())));
        s.delete_note(&b.id).unwrap();
        s.sync_owner_chunks(&ChunkOwner::Note(b.id)).unwrap();
        s.sync_owner_chunks(&ChunkOwner::Transcript("gone".into()))
            .unwrap();
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
        // each chunk starts at a turn boundary
        assert_eq!(chunks[0].segment_ids, vec!["s1", "s2"]);
        assert_eq!(chunks[1].segment_ids, vec!["s3"]);
        assert!(!chunks[0].text.contains(':'), "no labels");
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
    fn floor_drops_weak_hits_but_reports_the_best_score() {
        let (_dir, s) = test_storage();
        let mut ids = Vec::new();
        for (title, v) in [("close", [1.0, 0.1]), ("far", [0.1, 1.0])] {
            let n = s.create_note(title, None).unwrap();
            s.update_note_scratchpad(&n.id, &format!("{title} body"))
                .unwrap();
            let p = s.pending_embedding_chunks("m", 10).unwrap();
            let chunk = p.iter().find(|c| c.title == title).unwrap();
            s.store_chunk_embeddings("m", &[(chunk.id, v.to_vec())])
                .unwrap();
            ids.push(n.id);
        }
        let q = [1.0, 0.0];
        let f = SearchFilter::default();
        let all = s.vector_search_floor(&q, "m", &f, 10, None).unwrap();
        assert_eq!(all.hits.len(), 2);
        assert!(all.hits[0].score > all.hits[1].score);
        // Exact scores (re-scored from the stored vectors).
        assert!((all.hits[0].score - 1.0 / 1.01f32.sqrt()).abs() < 1e-6);
        let floored = s.vector_search_floor(&q, "m", &f, 10, Some(0.5)).unwrap();
        assert_eq!(floored.hits.len(), 1);
        assert_eq!(floored.hits[0].note_id, ids[0]);
        assert!((floored.best_score.unwrap() - all.hits[0].score).abs() < 1e-6);
        // Nothing passes: no hits, but the best score still says how close.
        let none = s.vector_search_floor(&q, "m", &f, 10, Some(0.999)).unwrap();
        assert!(none.hits.is_empty());
        assert!(none.best_score.is_some_and(|b| b < 0.999 && b > 0.9));
        // A keep-local owner is out before the limit, and out of the best
        // score too.
        let private = s.create_folder("Private", None).unwrap();
        s.set_folder_keep_local(&private.id, true).unwrap();
        s.move_note(&ids[0], Some(&private.id)).unwrap();
        let shareable = s
            .vector_search_floor(
                &q,
                "m",
                &SearchFilter {
                    exclude_keep_local: true,
                    ..Default::default()
                },
                1,
                None,
            )
            .unwrap();
        assert_eq!(shareable.hits.len(), 1);
        assert_eq!(shareable.hits[0].note_id, ids[1]);
        assert!(shareable.best_score.unwrap() < 0.2);
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
