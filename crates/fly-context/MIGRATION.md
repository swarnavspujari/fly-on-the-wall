# Proposed retrieval schema migration (v2.5 PR 3.2b, not landed)

This is the design for spec §4.2 "Retrieval fixes" and §4.2a #6 (citation locators). It **waits for the storage-safety session's `fly-storage` changes** and lands in `fly-storage` as the next `user_version` step after theirs. `fly-context` 3.2a works on today's schema and swaps its in-memory workarounds for these tables once they exist. Each workaround is marked below.

## Why
Today's schema forces `fly-context` into several workarounds:
- **`transcripts_fts` has one row per meeting.** A keyword hit says *which meeting*, not *where*. `search_passages` localizes it by re-chunking the transcript in memory and scoring windows lexically.
- **`embedding_chunks` stores only `start_ms`.** No segment ids, no `end_ms`. A vector hit is mapped back to its window by re-running the chunker and matching `start_ms`. That works only because `fly-context`'s windowing is pinned to `chunk_transcript` by a test.
- **Chunk text embeds speaker labels** (`Dana: …`). A speaker rename changes every chunk of the meeting and re-embeds the whole meeting.
- **The polished transcript is never indexed.** Proper nouns that polish fixed are invisible to keyword search.
- **`vector_search` ignores `SearchFilter.exclude_keep_local`** and truncates before any caller can filter. `fly-context` requests every hit, then filters against `keep_local_note_ids()` (fail-closed), then caps. Today the only caller is the UI's semantic search (`commands.rs:648`), so nothing leaks. But it's a trap for the next AI caller, and the fix below is required before wiring.
- **`list_meetings` has no keep-local or `until` filter.** `fly-context` filters in memory.
- **Citations are bare segment ids.** Re-transcribing builds new ids (`align.rs` `new_id()`), so every citation in Enhanced blocks and items silently dangles.

## Schema (one `user_version` step)

```sql
-- 1. Chunks carry their exact span. Text becomes label-free.
ALTER TABLE embedding_chunks ADD COLUMN segment_ids_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE embedding_chunks ADD COLUMN end_ms INTEGER;
ALTER TABLE embedding_chunks ADD COLUMN variant TEXT NOT NULL DEFAULT 'raw'; -- 'raw' | 'polished'
-- (via EXPECTED_COLUMNS, so repair_missing_columns also heals odd DBs)

-- 2. Per-segment keyword index (labels NOT indexed; speakers are filters).
CREATE VIRTUAL TABLE IF NOT EXISTS segments_fts USING fts5(
    meeting_id  UNINDEXED,
    segment_id  UNINDEXED,
    speaker_key UNINDEXED,
    start_ms    UNINDEXED,
    end_ms      UNINDEXED,
    variant     UNINDEXED,   -- which text is indexed: 'polished' when present, else 'raw'
    text,
    tokenize = 'unicode61 remove_diacritics 2'
);

-- 3. Citation locators (§4.2a #6), one row per (citing thing, segment).
CREATE TABLE IF NOT EXISTS citation_locators (
    owner_kind TEXT NOT NULL,      -- 'note_block' | 'item' | 'ask_insert'
    owner_id   TEXT NOT NULL,      -- NoteBlock.id / MeetingItem.id / insert id
    meeting_id TEXT NOT NULL,
    seg_id     TEXT NOT NULL,
    start_ms   INTEGER NOT NULL,
    end_ms     INTEGER NOT NULL,
    text_hash  TEXT NOT NULL,      -- hex sha256 of whitespace-collapsed, lowercased text, first 16 chars
    status     TEXT NOT NULL DEFAULT 'ok',   -- 'ok' | 'reanchored' | 'changed'
    PRIMARY KEY (owner_kind, owner_id, seg_id)
);
CREATE INDEX IF NOT EXISTS idx_citation_locators_meeting ON citation_locators(meeting_id);
```

`transcripts_fts` (meeting-level) stays as it is. `search_notes`, the MCP and the Home search keep working byte-for-byte, and it can be retired in a later release.

## Write paths (fly-storage)
- **`sync_transcript_chunks`:**
  - chunks with the same turn/160-word windowing, but chunk `text` = segment texts joined per turn, *without* labels;
  - stores `segment_ids_json` and `end_ms`;
  - embeds the **polished** text when a cleaned transcript exists (`variant='polished'`), else raw. Ids and timestamps are identical between the two, so spans don't change.
  - Content-keyed vector reuse keys on `(title, text)` as now. Because labels are gone, **a rename costs zero re-embeds**.
  - Readers render labels at read time from `segment_ids` and the current speakers.
- **`sync_transcript_derived` and `save_cleaned_transcript`:** rewrite the meeting's `segments_fts` rows (polished text when present), so FTS and chunks always agree on the variant.
- **Embedding prompt:** `title: {meeting title} | text: {label-free text}`. The title stays in the embedding context because it's what makes "the airline meeting" match.
- **Citation locators:**
  - written whenever an AI block, item or Ask insert is saved with segment ids, from the transcript variant the model was shown;
  - `edit_segment_text` recomputes the hash for that segment and marks its locators `changed` when the hash moves.
- **Re-anchoring on re-transcribe** (inside `save_transcript`, while the *old* transcript is still readable), for each locator whose `seg_id` is gone from the new transcript:
  1. take candidates overlapping `[start_ms − 2 s, end_ms + 2 s]`;
  2. score each as `0.5 × time-overlap ratio + 0.5 × token-Jaccard(old text, new text)`;
  3. if the best score is ≥ 0.6, repoint the locator (`status='reanchored'`) and rewrite the matching id in `BlockOrigin::Ai.source_segment_ids` and `meeting_items.segment_ids_json`, so existing "zoom in" keeps working;
  4. otherwise mark it `changed`, and the UI shows "source changed — check".

  Re-diarize keeps ids (`pipeline.rs:720`), so it never triggers this. **Required before re-transcribe-with-Parakeet is offered** (spec 4.2a #6).

## Read paths added to fly-storage
- **Required before anything wires `fly-context` to a cloud model or the MCP** (spec §4.2a #13): `vector_search` honors `filter.exclude_keep_local` inside SQL (`KEEP_LOCAL_CTE` + `SHAREABLE_CHUNK`, both already in `privacy.rs`).
  - It must also take an **orphan guard**: a transcript chunk whose meeting row is gone, or a note chunk whose note is gone, has no checkable privacy and is excluded under `exclude_keep_local`.
  - Its `limit` must apply *after* that filter.
  - Until then, `fly-context` asks `vector_search` for every hit, drops keep-local and orphan chunks, and only then caps the candidate pool at 200. That's correct, but it scans and sorts every chunk per query. Tests `keep_local_chunks_cannot_starve_the_vector_pool` and `out_of_scope_chunks_cannot_starve_a_scoped_search` pin the behavior.
  - Alongside this, `VectorHit` gains `segment_ids`, `end_ms` and the full chunk `text`.
- `search_segments(query, &SearchFilter) -> Vec<SegmentHit { meeting_id, segment_id, start_ms, end_ms, snippet }>`, bm25-ranked, keep-local in SQL.
- `list_meetings(&MeetingFilter { since, until, folder_id, exclude_keep_local, limit })`.
- Optionally `segments_by_ids(meeting_id, &[seg_id])` for the citation validator. It avoids parsing the whole transcript JSON per check.

## What fly-context then changes (no API change)
- `search_passages` keyword list: `search_segments` hits grouped into windows by `segment_ids` (no in-memory re-scoring); `meeting_items` added as a third ranked list (spec §4.2).
- Semantic list: windows straight from `VectorHit.segment_ids`. The pinned-windowing test becomes unnecessary.
- Remove the post-filters for `vector_search` and `list_meetings`. Keep the by-id guards; they're cheap and fail closed.

## Backfill and cost
- **Migration step** (idempotent, resumable per meeting, like `to_v2`):
  1. populate `segments_fts` for every transcript;
  2. re-chunk every transcript (new label-free text, which resets vectors to pending);
  3. write locators for existing AI blocks and items from the current transcripts. Their ids are valid now, so all rows start as `ok`.
- **Re-embed:** every transcript chunk changes text once, so the whole corpus re-embeds once. On this machine that's roughly 27 meetings × ~20 chunks, about 600 chunks. It must run in the existing worker at the lowest priority (spec 4.2a #3: captions > Ask > backfill), throttled. FTS-only search keeps working throughout, as it does today when Ollama is down.
- **Disk:** `segments_fts` roughly duplicates transcript text once more (~1–2 MB per 100 meeting-hours), and locators are tiny.

## Tests the migration PR needs
- An old-DB fixture migrates and the rerun is a no-op; a crash between steps resumes cleanly.
- A rename re-embeds 0 chunks; an edit to one segment re-embeds 1 chunk.
- `segments_fts` uses the polished text when it exists and the raw text otherwise, and both agree with the chunk `variant`.
- Re-transcribe fixture: identical speech with new ids gets every locator `reanchored`; changed speech gets `changed`. Block and item ids are rewritten.
- The keep-local exclusion test covers `vector_search`, `search_segments` and `list_meetings` with `exclude_keep_local`.
- The retrieval gold set (spec §4.2 eval 1, recall@8 ≥ 0.85) runs in CI against the migrated schema.
