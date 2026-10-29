# Storage schema v3: retrieval index + citation locators (v2.5 PR 3.2b)

This implements spec §4.2 "Retrieval fixes" and §4.2a #6 (citation locators) in `fly-storage` as `user_version` **3**. It's a data-format change and ships with v2.5.

Code: `src/{segments,citations,embeddings,migrations}.rs` and `Storage::SCHEMA_VERSION` in `src/lib.rs`, which holds the cross-version contract. Tests: `tests/migration_v3.rs`, plus unit tests in those modules.

## What changed and why

| Before (v2) | After (v3) |
| --- | --- |
| `transcripts_fts` has one row per meeting, so a keyword hit names a meeting, not a place in it. | **`transcript_segments`** holds one row per segment (id, speaker key, `start_ms`/`end_ms`, text, `variant`). **`segments_fts`** is an external-content FTS5 index over its text, so the text is stored once, with FTS5 `secure-delete` on. `Storage::search_segments` returns segment hits ranked by bm25, with keep-local filtered in SQL. `transcripts_fts` is unchanged. |
| Chunk text embeds speaker labels (`Dana: …`), so a rename re-embeds the whole meeting. | Chunks are **label-free**: one line per turn. A rename re-embeds nothing. |
| Chunks store only `start_ms`. | `embedding_chunks` gains `segment_ids_json`, `end_ms` and `variant`. `VectorHit` carries `segment_ids`, `end_ms` and the full `text`. |
| The polished transcript is never indexed. | Each segment indexes and embeds its **polished** text when the polished transcript has a segment with the same id, otherwise its raw text. A stale polish (old ids) is never used. |
| `vector_search` ignores `exclude_keep_local` and truncates before callers filter. | The keep-local filter runs **inside SQL, before the limit** (§4.2a #13). The shared shareable-chunk filter fails closed: it admits only `note` and `transcript` chunks, and a transcript chunk only while its meeting and that meeting's note exist. The same filter guards the remote-embedding queue (`pending_embedding_chunks_shareable`, `embedding_backlog_shareable`). Chunks whose owner row is gone are never returned. `search_segments` and `list_meetings_filtered` fail closed the same way. |
| `list_meetings` has no keep-local or `until` filter. | `list_meetings_filtered(&MeetingFilter { since, until, folder_id, exclude_keep_local, limit })`, all applied in SQL. |
| Citations are bare segment ids, and re-transcribing silently breaks every one. | **`citation_locators`** (see below). |

### Triggers keep the segment index in step

`transcript_segments`, `segments_fts` and the locator clean-up are maintained by SQLite triggers on `transcripts`. The triggers are stored in the database file, so any build that writes transcripts keeps the index consistent, including an older build that knows nothing about v3.

- **Total.** For any stored value they insert what they can and never raise:
  - `json_valid` guards the document, and `type = 'object'` guards each element;
  - every column is COALESCEd or nullable;
  - the trigger-maintained table has no NOT NULL / UNIQUE / FK constraints.
  - Test: `malformed_transcript_json_never_fails_the_write` covers string, number and array elements and non-array `segments`, in both columns.
- **Re-installed on every open.** They're dropped and re-created in one transaction, so a corrected trigger reaches files that already exist (`triggers_are_reinstalled_on_every_open`).
- **`LIMIT -1` in the polished-text subquery is load-bearing.** Without it, SQLite re-parses the polished JSON once per segment: 42 s for a 3,000-segment transcript, against about 30 ms with it (`long_polished_transcript_indexes_quickly`). `GROUP BY` keeps duplicate polished ids from duplicating rows.
- **A damaged index** would now fail the transcript write itself, so:
  - `save_transcript` rebuilds `segments_fts` and retries once on `SQLITE_CORRUPT` (`corrupt_index_is_rebuilt_and_the_save_retried`);
  - the v3 step ends with a rebuild.
- **Secure delete:** a deleted meeting's words leave the index's pages (`deleted_text_leaves_the_index_pages`).

### Citation locators

Table: `citation_locators(owner_kind, owner_id, note_id, meeting_id, seg_id, start_ms, end_ms, text_hash, speaker_key, status)`, with primary key `(owner_kind, owner_id, seg_id)`. `speaker_key` is also listed in `EXPECTED_COLUMNS`, so a file stamped v3 by an earlier v3 build is repaired on open.

- **The cited hash is taken once and never overwritten.**
  - It's the first 16 hex characters of SHA-256 over the RAW segment text, normalized to lowercased alphanumeric words.
  - Casing, spacing and punctuation never count as a change; a different, added or missing word does.
  - A re-polish can't move it (`repolish_never_marks_changed_but_reattribution_does`).
- **Written automatically:**
  - `update_note_blocks` records AI blocks, and `replace_meeting_items` records items.
  - The recording self-heal re-syncs the meetings it restored.
  - Ask inserts can call `record_citations` (`owner_kind = 'ask_insert'`, reserved).
  - A pair that's already recorded keeps its hash.
  - An id that no transcript of the owner's meetings has (a model citing ids that a re-transcribe replaced while it ran) gets a `changed` placeholder (`text_hash = ''`). Readers must still treat a cited id without a locator as changed.
- **Status when read:** `ok` or `reanchored` only if the segment exists, has the cited speaker key, and its raw text hashes to the cited hash. Rows without a speaker key, recorded before raw-text hashing, may match the indexed text instead. Otherwise it's `changed`.
- **Speaker renumbering is followed, not flagged.**
  - Keys are numbered by first appearance, so a re-diarize or re-transcribe routinely renumbers a stable speaker (Karim spk_2 → spk_1).
  - `save_transcript` maps old keys to new keys with the rule `rebuild_speakers` uses: ≥ 80% of the cluster's duration onto one counterpart, in both directions. Weights come from shared segment ids (re-diarize) or from time overlap (re-transcribe).
  - It rewrites the cited `speaker_key` of the stable pairs.
  - When any speaker was renumbered, a cited key whose cluster has no stable image (it split or merged) is rewritten to a sentinel no segment carries, so it reads `changed`. This matters because its number may now belong to someone else: in the case found in review, Karim was renumbered onto Dana's old spk_0, and the cited Dana line had landed in it. The rule also covers lost segments being re-pointed (`unstable_cluster_whose_number_was_taken_reads_changed`, both the re-diarize and re-transcribe variants). It's never NULL, because a NULL key means "recorded before speaker keys" and matches any speaker.
  - A line that genuinely moved to another cluster still reads `changed`, even when the clusters were renumbered too.
  - In a transcript with too little context (a single line), a renumbering and a speaker swap look the same. That case resolves to the stable mapping: the citation reads `ok` / `reanchored`, matching the label `rebuild_speakers` carries over and shows.
- **No visibility filter.** `citation_locators()` answers for any owner. Callers serving an AI reader (Ask, MCP, a future `segments_by_ids`) must gate on the owner's keep-local status first.

**Re-anchoring** happens in `save_transcript`, while the old transcript is still readable. For each cited segment the new transcript no longer has:
1. Take candidates overlapping the old span padded by 2 s.
2. Score each as 0.45 × time IoU + 0.45 × token Jaccard + 0.10 × same speaker key (after the renumbering map). The pointer moves to the best candidate scoring ≥ 0.6. Placeholders (no hash) never move.
3. The status is `reanchored` only when the new raw text hashes to the cited hash and the speaker key matches. Otherwise it's `changed`, still pointing at the best match so zoom-in lands in the right place.
   - "We will ship" → "we will not ship", "four ninety" → "fourteen ninety", and the same words from another speaker are all `changed` (`adversarial_retranscribes_read_as_changed`).
4. When two cited segments land on the same new one for the same owner, one row survives. If either no longer holds, the survivor reads `changed`.
5. Rewrite the id in `meeting_items.segment_ids_json` and in the note's `BlockOrigin::Ai.source_segment_ids`. Nothing else changes: not block markdown, order or origin kind, not user blocks, and not the note's `updated_at`.

**Failure handling:**
- The upsert and the database side of the re-anchor share one savepoint. A re-anchor error rolls back only the re-anchor, through a nested savepoint, and the save still succeeds. A failed `RELEASE` rolls back too, so no savepoint is ever left open on the connection.
- **Fail closed:** when re-anchor planning fails, or the re-anchor rolls back, nothing vouches for the meeting's citations any more. Even citations on surviving ids may carry pre-renumbering keys. So, inside the same savepoint as the upsert, **every** citation of that meeting is marked `changed` and left on its old id (`reanchor_db_error_rolls_back_only_the_reanchor` checks a citation on a surviving id too).
- Note mirrors are refreshed afterwards, and their I/O errors are only logged (`reanchor_mirror_failure_never_fails_the_save`).

## The v3 step (`migrations::to_v3`)

The step runs in **one transaction**. A crash or a fatal error rolls it back completely: the version isn't stamped and the next open runs it again. Version 2 is stamped right after `to_v2`, so a v3 failure never re-runs the file-layout step.

1. Rebuild `transcript_segments` through the triggers (`UPDATE transcripts SET json = json WHERE meeting_id = ?`, one transcript at a time: the same trigger firing for the same rows as a table-wide `UPDATE`, so progress can follow it), after dropping rows whose transcript is gone.
2. Re-chunk every transcript: label-free, with spans, polished where it exists. **The transcript corpus re-embeds once.** Note chunks don't change. Orphan transcript chunks are dropped.
3. Reconcile and record locators.
4. Rebuild `segments_fts`.

A meeting or note that fails in step 2 or 3 is logged and skipped, never fatal. The duration is logged with each stage's share. A fresh database gets the schema and is stamped 3 without running the step. The step also runs when a NEWER-stamped file is opened (see the contract).

**Timing.** About 9,000 segments without word timings take 0.25–0.45 s in a release build and about 1 s in debug. A realistic large library is much slower per segment, because every segment carries its word timings and the triggers parse that JSON. `tests/migration_v3.rs::v3_step_timing` (ignored; run it with `--release -- --ignored --nocapture`) builds 1,000 meetings × 300 segments (14 timed words each, a third polished, 6 AI blocks and 8 items per meeting, 768-dim v2 vectors; 478 MB). Release build on a busy 20-thread Windows machine:

| Stage | Before the locator-lookup fix (2 runs) | After (2 runs) |
| --- | --- | --- |
| 1. Segment rebuild (triggers) | 23.6–24.1 s | 22.0–26.2 s |
| 2. Re-chunk | 6.3–12.9 s | 6.3–6.6 s |
| 3. Locators | 21.5–22.1 s | 7.8–8.0 s |
| 4. Index rebuild + commit | 3.5–4.2 s | 3.8–4.9 s |
| Total | 56–62 s | 41–44 s |

The machine was shared with other builds throughout (the fixture took 6.5–9.5 s to build, against about 4 s when idle), so treat these as upper bounds.

- Stage 1 is mostly the triggers' JSON work. A select-only pass of the trigger's `json_each` + `json_extract` over the same library took 12.4 s by itself. Dropping the per-row `segments_fts` triggers during the rebuild (the closing `rebuild` recreates the index anyway) saved nothing measurable. Making stage 1 cheaper means changing the trigger SELECT in `segments.rs`, which every build runs.
- Stage 3 was quadratic. The per-note and per-meeting "stale owner" lookups (`SELECT DISTINCT owner_id … WHERE owner_kind = ? AND note_id = ?`) used the primary key's `owner_kind` prefix, so each scanned every locator of its kind. They now force the note/meeting index (`+owner_kind`, pinned by `owner_lookups_use_the_note_and_meeting_indexes`), which also speeds up every runtime blocks save.

### First-launch progress

`Storage::pending_upgrade(dir)` says, read-only, whether the next open will run this step (`runs_v3_step`, the same condition `migrate` uses) and how many transcripts it re-indexes. `Storage::open_with_progress` / `open_with_retry_and_progress` report `UpgradeProgress { stage, done, total }` from inside the step. `done` and `total` are units weighted per stage from the benchmark (8 / 2 / 2 / 1 per transcript). Reports are throttled to one per 100 ms within a stage, plus every stage start and a final `done == total` after the commit. The app uses these to open the library off the main thread and show an "Upgrading your library" screen (`src-tauri/src/startup.rs`, `frontend/src/components/LibraryGate.tsx`). Plain `open` behaves as before.

## When an older app opens a v3 database

Every released build (v1.0.3 through v2.4.0) has `SCHEMA_VERSION = 2`, stamps `user_version` unconditionally at the end of `migrate`, and bundles the same SQLite (rusqlite 0.40). This is replayed in `an_older_build_writing_to_a_v3_file_stays_safe_and_heals`, and the review also ran the real v2.4.0 crate against a v3 file.

- **It opens normally and stamps the file back to 2.** It creates or alters nothing that conflicts.
- **Its transcript writes keep the segment index correct** (triggers):
  - its meeting deletes remove that meeting's segments, their index entries and its locators;
  - its re-transcribes mark affected citations `changed`.
- **Its "Delete all my data" erases the new tables too.**
- **Cost, not data loss:**
  - its startup chunk backfill rewrites transcript chunks in its labelled format, so the corpus re-embeds there and again after returning to v3;
  - its new AI blocks and items get no locators until v3 reopens.
- **The next v3 open re-runs the step and heals everything.** A second open after that is a no-op.
- **MCP binaries:**
  - an old MCP server refuses a v3 file ("newer than this build supports — update Fly on the Wall") until the old app has stamped it back to 2;
  - a v3 MCP server refuses a v2-stamped file until the v3 app has reopened it.

## Known follow-ups

- **First-launch progress for large libraries: done** (see [First-launch progress](#first-launch-progress)). The step still takes about 40 s on a 300k-segment library with word timings (release build, busy machine). Stage 1 is the remaining lever: the trigger SELECT's per-segment `json_extract` calls over objects that carry word timings.

- **Re-embed pacing (v2.5 release blocker, tracked separately):**
  - the one-time re-embed runs at the worker's normal priority, oldest chunks first (`ORDER BY id`);
  - polish changes re-embed;
  - the startup chunk pass holds the storage lock.
  - Spec §4.2a #3 wants the backfill at lowest priority and throttled.
- **fly-context keyword path:** switch to `search_segments`, and add `meeting_items` as a third ranked list.
- **FTS5 `secure-delete`** for `notes_fts` and `transcripts_fts`, which still keep deleted terms in their pages.
- **Trigger optimization:** skip the segment rebuild when `$.segments` is unchanged (a relabel currently rebuilds that meeting's rows).
- **Dev tooling:** warn about a stale `flyonthewall-mcp` sidecar.
- **Retrieval gold set** (§4.2 eval 1) in CI; retiring `transcripts_fts`.
