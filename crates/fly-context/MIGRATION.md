# Storage schema v3: retrieval index + citation locators (v2.5 PR 3.2b)

This implements spec §4.2 "Retrieval fixes" and §4.2a #6 (citation locators) in `fly-storage` as `user_version` **3**. It's a data-format change: it ships with v2.5 and must not merge before v2.4 is tagged.

Code: `crates/fly-storage/src/{segments,citations,embeddings,migrations}.rs`. Tests: `crates/fly-storage/tests/migration_v3.rs`, plus unit tests in those modules.

## What changed and why

| Before (v2) | After (v3) |
| --- | --- |
| `transcripts_fts` has one row per meeting, so a keyword hit names a meeting, not a place in it. | **`transcript_segments`** holds one row per segment (id, speaker key, `start_ms`/`end_ms`, text, `variant`). **`segments_fts`** is an external-content FTS5 index over its text, so the text is stored once. `Storage::search_segments` returns segment hits ranked by bm25, with keep-local filtered in SQL. `transcripts_fts` is unchanged, so `search_notes`, the MCP and Home search behave exactly as before. |
| Chunk text embeds speaker labels (`Dana: …`), so a rename re-embeds the whole meeting. | Chunks are **label-free**: one line per turn. A rename re-embeds nothing. |
| Chunks store only `start_ms`, and readers re-run the chunker to find the span. | `embedding_chunks` gains `segment_ids_json`, `end_ms` and `variant`. `VectorHit` carries `segment_ids`, `end_ms` and the full `text`. |
| The polished transcript is never indexed. | Each segment indexes and embeds its **polished** text when the polished transcript has a segment with the same id, otherwise its raw text. Matching by id means a stale polish left over from an earlier run (old ids) is never used. |
| `vector_search` ignores `exclude_keep_local` and truncates before callers filter. | The keep-local filter (`KEEP_LOCAL_CTE` + `SHAREABLE_CHUNK`) and an orphan guard run **inside SQL, before the limit** (§4.2a #13). Chunks whose meeting or note row is gone are never returned. Under `exclude_keep_local`, a transcript chunk whose meeting has no note row is dropped too, because its privacy can't be checked. |
| `list_meetings` has no keep-local or `until` filter. | `list_meetings_filtered(&MeetingFilter { since, until, folder_id, exclude_keep_local, limit })`, all applied in SQL. |
| Citations are bare segment ids, and re-transcribing silently breaks every one. | **`citation_locators`** (see below). |

### Triggers keep the segment index in step, not Rust write paths

`transcript_segments`, `segments_fts` and the locator clean-up are maintained by SQLite triggers on `transcripts`. The triggers are stored in the database file, so **any** build that writes transcripts keeps the index consistent, including an older build that knows nothing about v3. They use only core SQLite (built-in JSON functions and FTS5, both present in every released build's bundled SQLite). They are written so they can't fail on a stored row: `json_valid` guards and no constraints a malformed segment could break. A trigger error would fail the older build's write.

**`LIMIT -1` in the polished-text subquery is load-bearing.** Without it, SQLite re-parses the polished JSON once per segment: 42 s for a 3,000-segment transcript. With it, the subquery is built once and joined through an automatic index, in about 30 ms. Test: `long_polished_transcript_indexes_quickly`.

### Citation locators

Table: `citation_locators(owner_kind, owner_id, note_id, meeting_id, seg_id, start_ms, end_ms, text_hash, status)`, with primary key `(owner_kind, owner_id, seg_id)`.

**Written automatically:**
- `update_note_blocks` records locators for AI blocks.
- `replace_meeting_items` records locators for items.
- Ask inserts can call `record_citations` (`owner_kind = 'ask_insert'`, reserved).
- A pair that's already recorded keeps its hash, so later drift is visible.

**Text hash:** the first 16 hex characters of SHA-256 over the text, lowercased, with whitespace collapsed.

**Status:**
- **Computed when read** (`citation_locators()`): `changed` if the segment is gone, or if its text matches the stored hash in neither the indexed (polished) variant nor the raw one.
- **Changes that are stored:**
  - the update trigger marks a citation `changed` when its segment id disappears;
  - re-anchoring sets `reanchored`.

**Re-anchoring** happens in `save_transcript`, while the old transcript is still readable. For each cited segment the new transcript no longer has:
1. Take candidates overlapping the old span padded by 2 s.
2. Score each as 0.5 × time IoU + 0.5 × token Jaccard (old text vs new).
3. If the best score is ≥ 0.6, repoint the locator (`reanchored`). Then rewrite the id in `meeting_items.segment_ids_json` and in the note's `BlockOrigin::Ai.source_segment_ids`, so the existing "zoom in" keeps working. This doesn't bump the note's `updated_at`, and its `note.json` mirror is refreshed.
4. Otherwise the locator stays `changed`.

Re-diarize keeps ids, so it never triggers this.

## The v3 step (`migrations::to_v3`)

The step runs in **one transaction**. A crash or error rolls it back completely: the version isn't stamped and the next open runs it again.

1. Rebuild `transcript_segments` through the triggers (`UPDATE transcripts SET json = json`), after dropping rows whose transcript is gone.
2. Re-chunk every transcript: label-free, with spans, polished where it exists. Chunk text changes once, so **the whole corpus re-embeds once**. The existing embedding worker picks up the pending rows, and keyword search works throughout. Note chunks don't change and keep their vectors. Orphan transcript chunks are dropped.
3. Record locators for every AI block and item from the current transcripts, so they start `ok`. Then drop locators whose owner or meeting is gone, and mark citations of vanished segments `changed`.

Unreadable rows are logged and skipped, never fatal: a failing step would stop the app opening. In tests, a debug build takes about 1 s for 9,000 segments. A fresh database gets the schema and is stamped 3 without running the step.

## When an older app opens a v3 database

Every released build (v1.0.3 through v2.3.0, and v2.4 as it stands) has `SCHEMA_VERSION = 2`, stamps `user_version` unconditionally at the end of `migrate`, and bundles the same SQLite (rusqlite 0.40). Replayed in `an_older_build_writing_to_a_v3_file_stays_safe_and_heals`:

- **It opens normally and stamps the file back to 2.** It creates or alters nothing that conflicts: the new columns have defaults and the new tables are ignored by its code.
- **Its transcript writes keep the segment index correct** (triggers). Its meeting deletes remove that meeting's segments, their keyword-index entries and its locators, so deleted text doesn't linger in the new index. Its re-transcribes mark affected citations `changed`; it can't re-anchor, which is the honest state.
- **Its "Delete all my data" erases the new tables too.** It deletes every table listed in `sqlite_master`.
- **Cost, not data loss:**
  - its startup chunk backfill rewrites transcript chunks in its labelled format, so the corpus re-embeds under the old build and again after returning to v3;
  - its new AI blocks and items get no locators until v3 reopens.
- **Next v3 open:** the version reads 2, so the v3 step runs again and heals everything. It re-chunks, reconciles deletes and records the missing locators. A second open after that is a no-op.
- **MCP binaries:**
  - an old MCP server refuses a v3 file ("newer than this build supports — update Fly on the Wall") until the old app has stamped it back to 2;
  - a v3 MCP server refuses a v2-stamped file ("open the app once to upgrade it") until the v3 app has reopened it.
  - Neither ever writes the schema.

**Rule for future steps** (in the `SCHEMA_VERSION` doc comment): every step must be a full, idempotent rebuild of what it owns, because a downgrade round-trip re-runs it.

## Not done here (follow-ups)

- **fly-context keyword path:** still localizes meeting-level FTS hits in memory. Switching it to `search_segments` and adding `meeting_items` as a third ranked list (§4.2) is a fly-context change with no schema impact. The vector path already uses the segment ids and the in-SQL filter.
- **Embedding worker priority:** the one-time re-embed runs at the worker's normal pace. Spec §4.2a #3 wants it lowest priority and throttled (captions > Ask > backfill). The worker isn't in this PR.
- **`segments_by_ids`** for the citation validator: `indexed_segment` exists `pub(crate)`. Expose it when a caller needs it.
- **Retrieval gold set** (§4.2 eval 1, recall@8 ≥ 0.85) in CI against the v3 schema.
- **Retiring `transcripts_fts`:** a later release.

## Deviations from the proposal in 3.2a

- The segment index is a plain table plus external-content FTS5, maintained by triggers, instead of a standalone FTS table written by Rust. This gives safety under older builds, indexed per-meeting deletes and no duplicated text.
- The polished-vs-raw choice is made per segment id, not per meeting.
- `citation_locators` gains `note_id`, for note-scoped clean-up.
- Text drift is detected when locators are read, not written by `edit_segment_text`. Edits then need no extra write path, and older builds' edits are caught too.
- Re-anchoring rewrites block ids without touching the note's `updated_at`.
