# Storage (`fly-storage`)

> Layer: crate. Described at main `82b7edf` (v2.4.0).
> In flight: **PR #92** (draft, v2.5) changes this crate heavily: schema v3 (`user_version` 2→3), new `transcript_segments` + `segments_fts` tables (per-segment FTS, polished text when present), new `embedding_chunks` columns `segment_ids_json` / `end_ms` / `variant` (label-free chunk text), `citation_locators` with re-anchoring inside `save_transcript`, `list_meetings_filtered(&MeetingFilter)`, and a keep-local filter inside the SQL of `vector_search`. New files `citations.rs`, `segments.rs`; touches `embeddings`, `hybrid`, `items`, `lib`, `meetings`, `migrations`, `notes`, `privacy`, `search`, `transcripts`. Everything below describes main before #92, except the search parts refreshed for the v2.5 search-quality PR (`query.rs`, `cache.rs`, the FTS fallback, term statistics, `vector_search_floor`); #92 has since merged (see `SCHEMA_V3.md`).

## Purpose
The app's persistence layer. A bundled SQLite file (WAL, FTS5, `secure_delete`) is the index. Markdown/JSON mirrors on disk are the copy the user owns: one folder per meeting, note mirrors, a portability manifest and `note.json` that let a copied folder rebuild its meeting on another machine (self-heal). It also owns hybrid search (FTS5 + brute-force vectors fused with RRF), the per-note/per-folder Keep-local flags, audio retention, the in-place database erase used by "Delete all my data", path containment, and the machine-local pipeline scratch folder.

## Files it owns
- `crates/fly-storage/src/lib.rs` — `Storage`, `open` / `open_with_progress` / `open_existing` / `open_with_retry` / `open_with_retry_and_progress`, the read-only `pending_upgrade` peek, pragmas, baseline schema, column repair, `user_version` gate.
- `crates/fly-storage/src/migrations.rs` — `looma.db` rename, v2 human-readable layout migration, the v3 index/locator step (`to_v3`, see `SCHEMA_V3.md`) and its throttled progress reports (`UpgradeProgress`).
- `crates/fly-storage/src/atomic.rs` — crash-safe mirror writes (`write_atomic`), `rename_with_retry`.
- `crates/fly-storage/src/safe_path.rs` — lexical path containment (`is_plain_name`, `contained`).
- `crates/fly-storage/src/scratch.rs` — per-meeting scratch outside the data dir; startup sweep.
- `crates/fly-storage/src/naming.rs` — `<YYYY-MM-DD> <title>` disk names, sanitizing, dedup.
- `crates/fly-storage/src/notes.rs` — note CRUD, FTS row, markdown mirror, `note.json`, list views.
- `crates/fly-storage/src/folders.rs` — folder tree, folder-path names and Keep local flags for manifests.
- `crates/fly-storage/src/meetings.rs` — meeting rows, meeting folders, renames, `purge_meeting`, `contained_recordings_dir`.
- `crates/fly-storage/src/transcripts.rs` — raw + polished transcripts, mirrors (md/json/vtt), speaker undo snapshot.
- `crates/fly-storage/src/query.rs` — query terms, stopwords, the prefix rule, `FtsQuery` (AND form + OR fallback), `TermStats` / IDF.
- `crates/fly-storage/src/search.rs` — FTS5 search (`search_split` with the fallback), `note_term_stats`, `SearchFilter`.
- `crates/fly-storage/src/cache.rs` — in-memory, i8-quantized copy of the embedding vectors, dropped by any database write.
- `crates/fly-storage/src/hybrid.rs` — RRF fusion of FTS and vector rankings.
- `crates/fly-storage/src/embeddings.rs` — chunkers, `embedding_chunks` store, `vector_search` / `vector_search_floor`, `similarity_floor`.
- `crates/fly-storage/src/items.rs` — extracted meeting items, `query_items`, `list_meetings`.
- `crates/fly-storage/src/jobs.rs` — transcription job queue.
- `crates/fly-storage/src/attachments.rs` — attachment copies and `attachment_abs_path`.
- `crates/fly-storage/src/settings.rs` — key/value settings table.
- `crates/fly-storage/src/templates.rs` — Enhance templates + built-in seeds.
- `crates/fly-storage/src/privacy.rs` — Keep local, participants notified, retention, `erase_all_content`.
- `crates/fly-storage/src/recovery.rs` — manifest v3, `note.json`, self-heal, portability backfill.
- `crates/fly-storage/tests/storage_safety.rs`, `crates/fly-storage/tests/migration_v2.rs`, `crates/fly-storage/tests/migration_v3.rs` — integration tests.

## Surface area
### Public API
| Group | Items |
|---|---|
| Open / lifecycle | `Storage::open` (creates + migrates + seeds, `lib.rs:227`), `Storage::open_with_progress(dir, &mut dyn FnMut(UpgradeProgress))` (the same open, reporting the v3 step, `lib.rs:234`), `Storage::open_with_retry` + `OpenRetry::STARTUP` (6 × 2.5 s, `lib.rs:264`, `:105`), `Storage::open_with_retry_and_progress` (`lib.rs:271`), `Storage::pending_upgrade(dir) -> Option<PendingUpgrade { from_version, transcripts }>` (read-only: will the next open run the v3 step, and how big is it; `lib.rs:284`, `:114`), `UpgradeProgress { stage, done, total }` + `UpgradeStage` (`migrations.rs:40-62`), `Storage::open_existing` (second-process read/write open, never migrates, `lib.rs:324`), `Storage::EXISTING_BUSY_TIMEOUT` (5 s), `data_dir()`, `StorageError`, `Result` |
| Notes | `create_note`, `get_note`, `list_notes_in_folder`, `list_recent_notes`, `list_notes_filtered` (cursor + `exclude_keep_local`), `update_note_title`, `update_note_scratchpad`, `update_note_blocks`, `edit_note_block`, `keep_note_block`, `move_note`, `delete_note`, `note_mirror_abs`, `NoteSummary` |
| Folders | `create_folder`, `list_folders`, `rename_folder`, `move_folder`, `delete_folder` |
| Meetings | `create_meeting`, `update_attendees`, `end_meeting`, `set_meeting_started_at`, `get_meeting`, `get_meeting_for_note`, `meetings_for_note`, `allocate_meeting_dir`, `purge_meeting`, `list_meetings`, free fns `recording_dir_rel`, `contained_recordings_dir` |
| Transcripts | `save_transcript`, `get_transcript`, `save_cleaned_transcript`, `get_cleaned_transcript`, `clear_cleaned_transcript`, `relabel_speaker`, `transcript_speakers` (speaker legends only; SQLite extracts `$.speakers`, skipping rows that don't parse), `edit_segment_text`, `save_speaker_snapshot` / `get_speaker_snapshot` / `clear_speaker_snapshot`, `revert_speaker_assignment`, `SpeakerSnapshot` |
| Search | `search`, `search_filtered`, `search_split`, `search_segments`, `SearchFilter`, `SearchHit`, `SearchHitKind`, `SegmentHit`; `query::{query_terms, content_terms, KEYWORD_MAX_CONTENT_WORDS, words, fold_word_into, term_matches, term_expr, is_stopword, STOPWORDS, FtsQuery, TermStats (idf, coverage_weights), TermOwners}`, `segment_term_stats`, `note_term_stats`; `hybrid::fuse`; `embeddings::{chunk_transcript, chunk_note, encode_vector, similarity_floor, EMBEDDINGGEMMA}`, `pending_embedding_chunks` (most recently written first), `embedding_backlog`, `store_chunk_embeddings`, `store_pending_embeddings` (stores only where the row still holds the embedded text), `chunk_owners` / `sync_owner_chunks` / `ChunkOwner` (the startup pass, owner by owner), `backfill_embedding_chunks`, `vector_search`, `vector_search_floor`, `PendingChunk`, `VectorHit`, `VectorResults` |
| Items | `replace_meeting_items`, `get_meeting_items`, `has_meeting_items`, `query_items` + `ItemFilter`, `mark_extracted`, `meetings_missing_items` |
| Jobs | `enqueue_transcription`, `next_transcription_job`, `transcription_job`, `queued_transcription_ids`, `mark_transcription_running` / `_done` / `_failed`, `requeue_transcription`, `delete_transcription_job`, `reset_running_transcriptions`, `TranscriptionJob`, `JOB_QUEUED/RUNNING/DONE/FAILED` |
| Attachments | `add_attachment`, `add_attachment_in_place` (a file directly in `attachments/<note_id>/`, or directly in the `recordings/<folder>/` of one of this note's own meetings, e.g. an imported video; nothing else), `remove_attachment` (deletes only inside `attachments/<note_id>/`; an in-place file elsewhere is unlinked, never deleted; a meeting-folder rename rewrites the paths of attachments listed in place there), `attachment_abs_path`, `note_attachments_dir` (the contained `attachments/<note_id>/`, or `None`) |
| Settings / templates | `get_setting`, `set_setting`, `delete_setting`, `delete_settings_with_prefix`; `list_templates`, `get_template`, `upsert_template`, `delete_template`, `seed_builtin_templates` |
| Privacy | `get_meeting_privacy` + `MeetingPrivacy`, `meeting_keep_local`, `note_keep_local` (both fail closed), `set_note_keep_local`, `set_folder_keep_local`, `local_only_folders`, `keep_local_folder_ids` / `_note_ids` / `_meeting_ids`, `pending_embedding_chunks_shareable`, `embedding_backlog_shareable`, `set_participants_notified`, `audio_retention_days`, `set_audio_retention_days`, `retention_candidates`, `meetings_with_audio`, `delete_meeting_audio` + `AudioPurge`, `erase_all_content` + `EraseOutcome`, `AUDIO_RETENTION_SETTING`, `RETENTION_CHOICES` |
| Recovery / portability | `self_heal_recordings` + `HealReport`, `stash_recording_manifest`, `RecordingManifest`, `PortableNote`, `write_recording_manifest`, `write_note_mirror` (not re-exported at root), `MANIFEST_VERSION` (3), `NOTE_MIRROR_VERSION` (2, `note.json` versions on its own), `RECORDING_MANIFEST`, `NOTE_MIRROR`; `RecordingManifest::recorded_keep_local`; `HealReport::kept_local_unconfirmed` (resurrected meetings set to Keep local because the manifest didn't say) |
| Paths / files | `rename_with_retry`; `safe_path::{is_plain_name, contained}`; `naming::{sanitize_title, disk_label, strip_date_prefix, unique_path}`; `scratch::{scratch_root, meeting_scratch_dir, prepare_meeting_scratch, SCRATCH_DIR, LIVE_TMP_DIR}`, `Storage::scratch_dir` |

`pub(crate)` only (not callable from the app): `sweep_stale_scratch` (runs inside `self_heal_recordings`, `recovery.rs:192`), `remove_all_scratch`, `checkpoint_after_delete`, `refresh_recording_manifest`.

### Tauri commands
None (crates expose no commands). Commands that wrap this crate are documented in [app-notes-library](app-notes-library.md), [app-search](app-search.md), [app-recording](app-recording.md), [app-pipeline](app-pipeline.md), [app-llm](app-llm.md), [app-import](app-import.md), [app-settings-consent](app-settings-consent.md) and [app-privacy](app-privacy.md).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| None | — | — | — |

### Settings keys
The `settings` table is a generic key/value store (`settings.rs:8-43`); keys are owned by their callers. The only key this crate defines:

| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `privacy.audio_retention_days` | R/W (`privacy.rs:450-468`) | No (`privacy.` isn't an allowed prefix, `src-tauri/src/commands.rs:59-66`) | Retention period in days; only 7/30/90/365 are honored, anything else reads as "keep forever" (`privacy.rs:454-455`) |

`delete_settings_with_prefix` (`settings.rs:35`) is used by the app to void `consent.cloud_llm.*` (`src-tauri/src/llm_commands.rs:102`).

### Database
`flyonthewall.db`, `user_version` = 3 (`SCHEMA_VERSION`, `lib.rs:503`; the v3 tables and the cross-version contract are in `crates/fly-storage/SCHEMA_V3.md`, not yet in the table below). `migrate` (`lib.rs:512-544`) runs v2 when `from < 2`, and the v3 step when `runs_v3_step(from, fresh)` (`lib.rs:508-510`: `from < 3 || from > SCHEMA_VERSION`, never on a fresh file), in one transaction, reporting `UpgradeProgress`. Schema in `create_baseline` (`lib.rs:416-552`); columns added after first release are back-filled by `repair_missing_columns` (`lib.rs:123-177`, `559-569`). No foreign key ties `meetings.note_id`, `transcripts`, `meeting_items`, `transcription_jobs` or `embedding_chunks` to their owners; deletes clean them up by hand (`notes.rs:263`, `meetings.rs:98`).

| Table | Columns | Read / write |
|---|---|---|
| `folders` | `id` TEXT PK, `name` TEXT, `parent_id` TEXT → `folders(id)` ON DELETE CASCADE, `created_at` TEXT, `keep_local` INTEGER 0/1 | R/W `folders.rs`, `privacy.rs`; created by heal with the manifest's per-folder Keep local (`folders.rs` `ensure_folder_path`) |
| `notes` | `id` TEXT PK, `title`, `folder_id` → `folders(id)` ON DELETE SET NULL, `meeting_id`, `scratchpad`, `blocks_json` (default `[]`), `attachments_json` (default `[]`), `created_at`, `updated_at`, `disk_path` (data-dir-relative mirror) | R/W `notes.rs`, `attachments.rs`, `recovery.rs`, `migrations.rs` |
| `meetings` | `id` TEXT PK, `title`, `note_id`, `attendees_json`, `attendees_confirmed` 0/1, `started_at`, `ended_at`, `recording_json` (relative paths; the source of truth for the folder), `keep_local` 0/1, `participants_notified` 0/1, `audio_deleted_at` | R/W `meetings.rs`, `privacy.rs`, `recovery.rs`, `migrations.rs` |
| `transcripts` | `meeting_id` TEXT PK, `json` (raw), `cleaned_json` (polished, never overwrites raw), `speaker_undo_json` (one-level re-diarize undo) | R/W `transcripts.rs` |
| `templates` | `id` TEXT PK, `name`, `system_prompt`, `structure_hint`, `built_in` 0/1 | R/W `templates.rs` (5 seeds, `templates.rs:83`) |
| `settings` | `key` TEXT PK, `value` TEXT | R/W `settings.rs` |
| `transcription_jobs` | `meeting_id` TEXT PK, `status` (`queued`/`running`/`done`/`failed`), `attempts`, `last_error`, `created_at`, `updated_at` | R/W `jobs.rs`; read by `scratch.rs:342`, `privacy.rs:486`, `notes.rs:55` |
| `meeting_items` | `id` TEXT PK, `meeting_id`, `kind` (`_none` = "ran, found nothing"), `text`, `owner`, `status`, `speaker_key`, `segment_ids_json`, `created_at`, `extracted_by`; indexes `idx_meeting_items_meeting`, `idx_meeting_items_kind` | R/W `items.rs` |
| `notes_fts` (FTS5) | `note_id` UNINDEXED, `title`, `body` (scratchpad + block markdown) | R/W `notes.rs:320-327`, `search.rs` |
| `transcripts_fts` (FTS5) | `meeting_id` UNINDEXED, `body` (`Label: text` lines) | R/W `transcripts.rs:268-283`, `search.rs` |
| `embedding_chunks` | `id` INTEGER PK, `owner_kind` (`note`/`transcript`), `owner_id`, `chunk_index`, `title`, `text`, `start_ms`, `embedding` BLOB (normalized LE f32), `model`; UNIQUE(`owner_kind`,`owner_id`,`chunk_index`); index `idx_embedding_chunks_owner` | R/W `embeddings.rs`, `privacy.rs:306-337` |

FTS5 shadow tables (`*_data`, `*_idx`, `*_content`, `*_docsize`, `*_config`) are created by SQLite; `erase_all_content` deletes through the virtual tables and skips the shadows (`privacy.rs:392-403`).

### Files and directories written
Under `<data_dir>` (`%APPDATA%\FlyOnTheWall` on Windows):

| Path | Written by | Notes |
|---|---|---|
| `flyonthewall.db`, `-wal`, `-shm` | `lib.rs:242-249` | Persistent WAL (`SQLITE_FCNTL_PERSIST_WAL`, `lib.rs:371-392`), `journal_size_limit` 16 MiB, WAL truncated on close. Legacy `looma.db*` renamed on open (`migrations.rs:228`). `pending_upgrade` opens the file read-only and writes nothing. |
| `notes/`, `transcripts/`, `attachments/`, `recordings/` | created on every `open` (`lib.rs:238-241`) | |
| `notes/<date> <title>[ (n)].md` | `notes.rs:329` (atomic) | Path stored in `notes.disk_path`; renamed on title edit; removed on delete. |
| `notes/_unlinked/` | v2 migration only (`migrations.rs:237`) | Mirrors with no row; never deleted. |
| `attachments/<note_id>/<file>[ (n)]` | `attachments.rs:11-50` | Copies; folder removed on note delete. Every path goes through `note_attachments_dir` / `safe_path::contained`. |
| `recordings/<date> <title>[ (n)]/` | `allocate_meeting_dir` (`meetings.rs:272`) | Renamed on title/date edit unless a transcription is running (`meetings.rs:285-361`); removed by `purge_meeting`. Media files inside are written by the app (recording, import). |
| `…/transcript.md`, `.json`, `.vtt` | `transcripts.rs:286-289` (atomic) | Fallback for meetings without a folder: `transcripts/<meeting_id>.{md,json,vtt}` (`transcripts.rs:299-319`). |
| `…/transcript.cleaned.md`, `.json`, `.vtt` | `transcripts.rs:108-111` (atomic) | Removed by `clear_cleaned_transcript`. |
| `…/recording.manifest.json` | `recovery.rs:129`, `680-718`, `meetings.rs:190` (atomic) | Manifest v3: ids, `ended_at`, recording paths, title, `started_at`, attendees, confirmed flag, folder path names, `keep_local` (the meeting's own flag) and `folder_keep_local` (one per folder name). Rewritten when Keep local changes. |
| `…/note.json` | `recovery.rs:122`, `notes.rs:331-343` (atomic) | Scratchpad + blocks with provenance. |
| `…/.<name>.<pid><n>.tmp` | `atomic.rs:156-168` | Transient; removed on failure. A process killed between `sync_all` and rename leaves one behind (nothing sweeps them). |
| `recordings/_unlinked/<name>/` | v2 migration (`migrations.rs:197`) | Self-heal moves folders with a manifest back out (`recovery.rs:614`). |
| Deletes inside meeting folders | `privacy.rs:533` (media by extension), `scratch.rs:304-320` (legacy `*.16k.wav` in pipeline format, `*.asrpart*.json[.tmp]`, `live-tmp/`), `migrations.rs:145` | |

Outside `<data_dir>`: `<scratch_root>/<meeting id>/` (`%LOCALAPPDATA%\FlyOnTheWall\scratch` by default; `<data_dir>/scratch` when the data dir isn't under the OS data dir, e.g. tests) — created by `prepare_meeting_scratch` (`scratch.rs:104`), holds 16 kHz copies, ASR checkpoints and `live-tmp/`; removed per meeting by purge/retention and wholesale by `erase_all_content` (`privacy.rs:414`). Non-plain ids map to `id-<fnv64>` (`scratch.rs:77-93`).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None | — | — |

### External processes
None.

### Secrets and keychain entries
None. The `settings` table is documented as non-secret (`settings.rs:1`); keys live in [fly-secrets](fly-secrets.md).

## Dependencies
- **Inbound:** the app crate `fly-app` (every command module plus `state.rs`, `scheduler.rs`, `pipeline.rs`, `embeddings.rs`, `privacy.rs`, `lib.rs:156` self-heal); `fly-mcp` (opens with `open_existing`, `crates/fly-mcp/src/lib.rs:227`; reads, plus `relabel_speaker` for `set_speaker_label`, `lib.rs:889`, which writes transcript mirrors from a second process); `fly-context` (reads only; used by fly-mcp and the Ask harness).
- **Outbound:** `fly-core` (Note, Meeting, Transcript, RecordingRef, Attendee, MeetingItem), `rusqlite` 0.40 with `bundled` (SQLite with FTS5), `serde`/`serde_json`, `chrono`, `dirs` (scratch root), `tracing`, `thiserror`. Dev: `tempfile`.

## Invariants
- Recursive deletes of meeting folders stay inside `recordings/<folder>`: `contained_recordings_dir` on `purge_meeting` (`meetings.rs:124`), `delete_meeting_audio` (`privacy.rs:538`), retention age check (`privacy.rs:505`), scratch sweep (`scratch.rs:305`). THREAT_MODEL T3/T7.
- Manifest recording paths are never trusted: self-heal rebases every path onto the scanned folder (file name only) and rejects non-plain names before moving anything (`recovery.rs:439-460`, `592-635`). THREAT_MODEL T7 follow-up.
- Ids read from manifests are validated with `safe_path::is_safe_id` (1-64 lowercase ASCII letters, digits, `-`, `_`, and a plain name, so no reserved device names) before anything is read, moved or written; a bad id rejects the whole folder. Lowercase only, so two ids never name one folder on a case-insensitive file system; the app's own ids are lowercase UUIDs. Paths are built only from ids that pass it (`note_attachments_dir`; the scratch dir and legacy transcript name hash anything else). THREAT_MODEL T7.
- Every path built from stored or manifest data must pass a containment check before a write, rename or delete: attachments via `note_attachments_dir`, note mirrors under `notes/`, meeting folders, transcript and `note.json` mirrors via `contained_recordings_dir`; the legacy `transcripts/<id>` fallback uses the scratch id mapping. THREAT_MODEL T3/T7.
- Attachment paths from IPC must be `attachments/…` or `recordings/…` with plain components (`attachments.rs:70-79`, `safe_path.rs:40-58`). THREAT_MODEL T3.
- Deletes are transactions, `secure_delete=ON`, and a WAL `TRUNCATE` checkpoint follows (`lib.rs:202`, `notes.rs:267-276`, `meetings.rs:107-120`, `notes.rs:475-479`). THREAT_MODEL T11.
- Mirrors are replaced atomically (temp in the same folder, `sync_all`, rename, Unix dir fsync); a lock that never clears falls back to an in-place (non-atomic) write so the mirror doesn't go stale (`atomic.rs:66-120`). `atomic.rs` module docs.
- A fresh (table-less) database never runs the v2 orphan sweep, so a lost index never mass-parks folders (`lib.rs:516`, `529-534`). DECISIONS.md "Storage naming & startup latency"; 2026-07-13 incident.
- `pending_upgrade` and `migrate` decide with the same `runs_v3_step` (`lib.rs:508-510`), so the app's choice to open in the background always matches what `open` will do. The peek reads the file `open` will open (`flyonthewall.db`, else a `looma.db` it would rename; `migrations.rs:242-247`), read-only, and creates or renames nothing (`pending_upgrade_agrees_with_open_and_writes_nothing`).
- The v3 step stays one transaction and a full, idempotent rebuild with progress reporting on. The trigger rebuild runs per transcript (`UPDATE … WHERE meeting_id = ?`, the same trigger firing as a table-wide `UPDATE`). Reports are throttled to one per 100 ms per stage, plus every stage start and a final `done == total` after the commit (`migrations.rs:153-222`). A progress callback runs inside the transaction and must not touch the database.
- Backfill never overwrites a mirror that may be newer: manifests only gain missing fields, except Keep local, where `true` from the file or the database wins (fail closed); `note.json` is replaced only when its `updated_at` is older; unreadable/foreign/future files are left alone (`recovery.rs:331-414`).
- `open_existing` (MCP) never creates, migrates, repairs or seeds, and refuses any schema that isn't exactly `SCHEMA_VERSION` (3) with every expected column (`lib.rs:324-434`). Spec §2 item 5 (MCP holds no keys; reads the app's DB).
- Keep local fails closed: unknown meeting or lookup error answers `true` (`privacy.rs:209-241`). Bulk readers filter inside SQL before `LIMIT` via `KEEP_LOCAL_CTE` (`privacy.rs:90-102`; used in `search.rs:84-92`, `notes.rs:144-151`, `items.rs:89-100`, `privacy.rs:306-337`). `vector_search` / `vector_search_floor` apply it in SQL before the limit and floor (#92); the vector cache (`cache.rs`) never decides visibility, each search asks SQL for the ids it may see and re-applies every filter when it reads the winners. `segment_term_stats` / `note_term_stats` scope their statistics the same way, so keep-local text never weighs on shareable ranking. **Not** applied in `list_meetings` (`items.rs:180`) or `search()` / `search_filtered` without the flag. Spec §2 item 1 (enforcement in the app), THREAT_MODEL T10.
- A re-recording into a keep-local note inherits the flag (`meetings.rs:73`, `privacy.rs:266-275`).
- Keep local survives self-heal and a copied folder: heal recreates the meeting with the manifest's own flag, flags folders it creates, and sets the meeting's own flag when the manifest says it was kept local but the folder it lands in isn't (a folder reused by name). A manifest without the flags (v1/v2, or stashed with the database down) heals as kept local. Attaching to an existing row can only add the flag. `set_note_keep_local` / `set_folder_keep_local` rewrite the affected manifests. Meetings kept local only because the manifest didn't say are listed in `HealReport::kept_local_unconfirmed`; the app asks the user to review them ([app-privacy](app-privacy.md)).
- A v2 build (v2.4) leaves a v3 manifest alone in its backfill, but its live edits and heal rebase rewrite it as v2 without the Keep local fields; heal then keeps the meeting local. Running an older build after v2.5 can only make a recovered meeting more local.
- Calendar-seeded attendees start unconfirmed; only the editor's save sets `attendees_confirmed` (`meetings.rs:42-91`). Spec §2 item 7.
- Raw transcript `json` is never overwritten by polish; a new transcription must clear the cleaned variant (`transcripts.rs:96-129`).
- Retention deletes media only (extension allow-list, plain files, symlinks not followed), keeps transcripts/notes/manifest, stamps `audio_deleted_at` only when nothing failed, and requires both `ended_at` and file mtimes past the cutoff (`privacy.rs:44-47`, `479-582`).
- FTS input is always sanitized: every user word reaches FTS5 inside double quotes, so no input can inject syntax (`query.rs`, test `hostile_and_degenerate_input`). The AND form quotes each token (minus stopwords once a query has more than two content words; a shorter keyword search keeps every word, as before) and prefix-stars the last; when it matches nothing under the filter, the OR form (content terms, 4+ characters as prefixes, quoted phrases still required) runs instead. Control characters are dropped and typographic double quotes delimit phrases. The choice ignores the page offset. DECISIONS.md M1 "FTS5 MATCH input is sanitized" and the v2.5 search entry.
- The vector cache is valid only while `PRAGMA data_version` (other connections' commits) and this connection's count of `embedding_chunks` row changes (an update hook, so it covers every write path, trigger or not) are unchanged; the segment-range cache uses `total_changes()` instead (`cache.rs`, tests `only_vector_writes_drop_the_vector_cache`, `any_write_drops_the_cache`). `ReadCache` has no `Default`: every `Storage` constructor builds it with `ReadCache::watching(&conn)`, which installs the hook. Ranking uses i8-quantized vectors; reported scores are exact for the first 256 candidates (re-read from the stored f32 vectors through the same scoped SQL, so `best_score` only ever comes from a chunk in scope) and approximate past them.
- `vector_search_floor`, `segment_term_stats` and `note_term_stats` each read on one snapshot (`read_snapshot`, a savepoint): a commit by another connection can't land between their scope check, cache stamp and reads.
- Meeting folders are found through `recording_json`, never derived from titles; a running transcription blocks the folder rename (`meetings.rs:293-302`). DECISIONS.md "Title-edit policy".
- Scratch is keyed by meeting id, outside the meeting folder; old in-folder scratch is deleted only for finished meetings and only with exact pipeline names in the pipeline's WAV format (`scratch.rs:179-258`, `289-336`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-storage/src/*.rs` | 188 (184 off Windows: 4 `#[cfg(windows)]` in `atomic.rs`) | open/open_existing/retry, migrations, atomic writes, containment, naming, notes/folders/meetings/transcripts/items/jobs/attachments/settings/templates, FTS + RRF + vectors, keep-local, retention, erase, self-heal, scratch |
| integration (runs in CI) | `crates/fly-storage/tests/storage_safety.rs` | 10 | fresh-DB no-park, backfill no-clobber, edits refresh manifests, persistent WAL, scratch independence/sweep, erase removes scratch |
| integration (runs in CI) | `crates/fly-storage/tests/migration_v2.rs` | 1 | v1 → v2 layout on a synthetic v1 fixture |
| integration (runs in CI) | `crates/fly-storage/tests/migration_v3.rs` | 9 (+2 ignored) | v2 → v3 step, rerun no-op, rollback on failure, older-build round trip, keep-local filters; `pending_upgrade` vs `open` on fresh / zero-byte / v2 / v3 / newer-stamped / `looma.db` files (and that the peek writes nothing); progress runs through all four stages, monotonic and throttled, to `done == total`. Ignored: `v3_step_timing` (release-build benchmark over a synthetic 1,000-meeting × 300-segment v2 library, per-stage split) and `write_v2_library_for_manual_check` (writes that library to `FOTW_V2_LIBRARY_DIR` for a manual first-launch check) |
| integration (other crates) | `crates/fly-mcp`, `crates/fly-context` tests | — | `exclude_keep_local` on `search_split` / `list_notes_filtered` is exercised there, not in this crate (inferred) |
| env-gated | `src-tauri/tests/search_eval.rs` | 1 | hybrid vs FTS hit rate; returns early (passes) when Ollama with the embedding model isn't reachable, so CI checks nothing |

**Known gaps:**
- `similarity_floor` (0.35 for `embeddinggemma:300m`) is calibrated on #103's gold set for the app's prompts and question-length queries; nothing re-checks it when the prompts or model change. The Search UI applies none; fly-context applies it only to queries of 4+ content words.
- Containment tests cover deletes, attachments, the portability backfill, manifest rebasing and manifest ids (`recovery.rs` `manifest_with_unsafe_ids_is_rejected`, `unsafe_note_ids_never_reach_outside_the_app_folders`). The fuzz corpus of hostile names isn't run through every path-taking API yet.
- No test pins `secure_delete`, `foreign_keys` or `journal_mode` pragmas, or that deleted text is gone from the file (T11).
- Migration tests use hand-written SQL fixtures, not a file made by an older build; the "rerun after a mid-migration crash" promise (`migrations.rs:2-4`) is untested.
- Atomic-write tests simulate a failing write through a seam; a kill between `sync_all` and rename (leftover `.tmp`) isn't covered.
- Note delete and meeting purge are separate transactions in the app (`src-tauri/src/commands.rs:298-314`); an orphaned meeting keeps searchable transcript text and no test or sweep covers it.

## Changing it safely
- Run `cargo test -p fly-storage` (and `-p fly-mcp -p fly-context`, which read through it), then `cargo clippy --workspace --all-targets -D warnings`.
- Schema change: bump `SCHEMA_VERSION` and add an `if from < N` step in `migrate`; add new columns to BOTH `create_baseline` and `EXPECTED_COLUMNS`; add new tables to `REQUIRED_TABLES`. `open_existing` rejects any other version, so the MCP binary and the app must ship from the same build.
- A new slow step: give it a condition helper that `pending_upgrade` shares (as `runs_v3_step` is), and report through the step's `Reporter` so the app's upgrade screen moves. Re-measure with `v3_step_timing` and re-weigh the `UNITS_*` constants if the stage split changes.
- New readers that hand data to AI tools must filter keep-local inside SQL (`KEEP_LOCAL_CTE` / `SHAREABLE_CHUNK`) before `LIMIT`; never post-filter a truncated list.
- New recursive deletes go through `contained_recordings_dir`, and any id or path that came from a manifest or `note.json` is checked with `safe_path` before use; new mirror files go through `write_atomic`; new manifest fields are `Option` with `#[serde(default)]` so v1/v2 files still parse.
- New files outside `<data_dir>` must be added to `erase_all_content` (the restart wipe only clears the data dir). New tables are erased automatically (`sqlite_master` scan).
- Search changes: judge them with the real embedder on the eval gold set, not only the hashed CI embedder (see [fly-context](fly-context.md#how-search_passages-ranks)). A new write path needs nothing for the vector cache (the update hook sees every `embedding_chunks` row change); a new `Storage` constructor must build its cache with `ReadCache::watching`.
- The repo is public: test fixtures use synthetic titles and names only.
