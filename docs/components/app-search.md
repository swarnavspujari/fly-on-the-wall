# Search and embedding indexer (`app-search`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: **PR #92** (draft, v2.5, storage schema v3). Adds `transcript_segments` + `segments_fts` (per-segment FTS, polished text when present); `segment_ids_json` / `end_ms` / `variant` columns on `embedding_chunks` with label-free chunk text; and a keep-local filter inside SQL for `Storage::vector_search`, which today ignores `SearchFilter.exclude_keep_local`. Chunk text changes there, so every chunk re-embeds after that migration (inferred). The commands below keep their signatures (inferred from the PR description).

## Purpose
Search-as-you-type over notes and transcripts. `search` is plain SQLite FTS5. `search_semantic` fuses FTS with a vector search (reciprocal rank fusion, grouped by note). Its vectors come from a background worker that chunks every note and transcript and embeds the chunks through Ollama's `/api/embed` with `embeddinggemma:300m`. Everything semantic is best-effort: no Ollama, no model or a dead server means FTS-only, never an error.

## Files it owns
- `src-tauri/src/commands.rs` (part) — `search` (651-659), `search_semantic` (661-702).
- `src-tauri/src/embeddings.rs` — the indexer (`spawn`), `embed_raw`, `embed_query`, prompts, model constant.
- Storage halves live in [fly-storage](fly-storage.md): `search.rs` (`search`, `search_split`, `SearchFilter`), `embeddings.rs` (chunking, queue, `vector_search`), `hybrid.rs` (`fuse`), `privacy.rs:303-338` (shareable queue).

## Surface area
### Public API
`embeddings.rs`:

| Item | Line | Used by |
|---|---|---|
| `EMBED_MODEL = "embeddinggemma:300m"` | 24 | worker, `search_semantic`, `tests/search_eval.rs` |
| `doc_prompt(title, text)` → `"title: {title} \| text: {text}"`; `query_prompt(q)` → `"task: search result \| query: {q}"` | 37-46 | worker, `embed_query`, `search_eval` |
| `embed_raw(root, &[String]) -> Result<Vec<Vec<f32>>, String>`: `POST {root}/api/embed`, 300 s timeout, count-checked | 49-77 | worker, `embed_query`, `search_eval` |
| `embed_query(&AppState, q) -> Option<Vec<f32>>`: only if `GET {root}/api/version` answers; never spawns a server or pulls | 83-96 | `search_semantic` |
| `spawn(AppHandle)`: the background indexer (called once, from `startup::finish`, `startup.rs:252`) | 293-371 | app setup |
| `yield_reason(&AppState) -> Option<YieldTo>`, `rest_after(Duration)`, `embed_next_batch(&Mutex<Storage>, &impl Embedder, shareable_only, size)`, `Embedder` trait (crate-private; tested with a fake embedder) | 168-262 | the worker, tests |

#### Indexer loop (`embeddings.rs:145-371`)
1. Once at start, on a blocking thread: the chunk backfill (`Storage::chunk_owners`, newest first, then `sync_owner_chunks` for 8 owners per storage-lock hold). Idempotent; a library already chunked costs one JSON parse per owner and never holds the lock for long.
2. Loop. **Yield** first (`yield_reason`): while a recording or screen recording runs (live captions), while a transcription job is running (any `pipeline_stage` other than `"waiting"`, which includes the polish), or while a foreground AI request holds `AppState::foreground_ai()` (Ask, Enhance), the worker logs `embedding index paused` once and checks back every 5 s or on a nudge. Priority: live captions > transcription > Ask / Enhance > backlog.
3. `root = ollama::root_url` (`llm.ollama.base_url`, else `http://localhost:11434`). If the root is **not** loopback, count only the *shareable* backlog (keep-local owners excluded); otherwise the full backlog.
4. Backlog 0 → wait for `embed_notify` or 30 s.
5. `ensure_ready`: `ollama::ensure_running` (may spawn the managed `ollama serve`), then `GET /api/tags`; if the model is missing, **pull it** (`POST /api/pull`, progress on `model:progress`). Failure → wait 60 s or a nudge.
6. `embed_next_batch`: re-derive remote/local from the URL actually used, take the 16 **most recently written** pending chunks (shareable queue when remote) under the lock, embed `doc_prompt(title, text)` with no lock held, then `store_pending_embeddings`, which stores a vector only where the row still holds the text that was embedded (a chunk re-chunked meanwhile, possibly under the same id, stays pending). Failure → sleep 60 s.
7. **Rest** after each batch for as long as it took (at least 250 ms): a large backlog (the one-time re-embed after the v3 upgrade, a model change) uses at most about half of what the embedding runtime can do.

New content first: chunk ids grow with every (re)chunk, so the queue is "newest write first" and a meeting that was just transcribed is embedded ahead of any backlog, as soon as its transcription and polish are done.

Polish never re-embeds: chunk boundaries come from the raw text, so polishing changes a chunk's text but not its span, and `sync_chunks` keeps the vector of a chunk whose span and title are unchanged when the change is between raw and polished text. Combined with the yield (the worker waits out the polish), a new meeting is embedded once.

The storage lock is never held across HTTP. `embed_notify` is nudged by `update_note_scratchpad` (`commands.rs:256`), Enhance/block edits (`llm_commands.rs:738,756`) and each finished transcription (`scheduler.rs:171`).

#### `search_semantic` (`commands.rs:666-702`)
`SearchFilter { limit: 30, ..Default }`, so `exclude_keep_local = false`. Runs `search_split` (FTS notes, FTS transcripts), then `embed_query`. With a vector it calls `vector_search(…, EMBED_MODEL, &filter, 30)`; a failure there is logged and treated as empty. Then `hybrid::fuse`. Keep-local notes **are** returned. That's correct for the user's own UI. But `vector_search` would ignore `exclude_keep_local` even if it were set (`crates/fly-storage/src/embeddings.rs:391-418`). fly-context post-filters it itself (`crates/fly-context/src/tools.rs:738-749`); PR #92 moves the filter into SQL.

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `search` | `query: String` | `Result<Vec<SearchHit>, String>` (FTS, limit 30, notes then transcripts) | `ui-app-shell` search box (`App.tsx:472`, 200 ms debounce), via `api.search` |
| `search_semantic` | `query: String` | `Result<Vec<SearchHit>, String>` (fused, grouped by note, limit 30) | `ui-app-shell` (`App.tsx:480`, 450 ms debounce; replaces the FTS list; errors ignored; stale responses dropped by `searchSeq`), via `api.searchSemantic` |

devMock mirrors both (`devMock.ts:1203-1208`). It returns the same two canned hits for every query, and `search_semantic` returns the first one. Mock data has no keep-local or empty-result case.

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `model:progress` | emitted by the indexer while pulling `embeddinggemma:300m` (`embeddings.rs:132-139`) | `ModelProgress` | `App.tsx:450` (global model-progress UI) |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `llm.ollama.base_url` | read (via `ollama::root_url`, `ollama.rs:32-45`) | no | where chunks and queries are embedded; non-loopback → shareable queue only |

### Database
| Table | Columns | Read / write |
|---|---|---|
| `notes_fts`, `transcripts_fts` | `note_id/meeting_id`, `title`, `body` | read (`search`, `search_split`) |
| `embedding_chunks` | `id, owner_kind, owner_id, chunk_index, title, text, start_ms, embedding, model` (`crates/fly-storage/src/lib.rs:535-546`) | worker: backfill rows, read pending, write `embedding`/`model`; `search_semantic`: read |
| `notes`, `meetings`, `folders` | `folder_id`, `updated_at`, `started_at`, `keep_local` | read (joins; shareable-queue CTE) |

### Files and directories written
- `<data_dir>/models/ollama/` gets the embedding model when the **managed** Ollama pulls it (`OLLAMA_MODELS`, `ollama.rs:106`). A user-run Ollama stores it in its own model folder.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `http://localhost:11434` or `llm.ollama.base_url` (can be another machine): `/api/version`, `/api/tags`, `/api/embed` | worker whenever a backlog exists; `search_semantic` on every debounced query | No consent, even when the chosen LLM provider is a cloud one. Keep-local chunks go only to a loopback root (shareable queue otherwise). **Query text always goes to the configured root.** |
| same root `/api/pull`, and from there the Ollama model registry (inferred) | first indexing pass without the model, automatically | None. About 600 MB downloaded with no click, if Ollama is installed or running. |

### External processes
- `ollama serve` (managed) may be spawned by the worker through `ollama::ensure_running` when the root is the default and Ollama is installed ([app-ollama](app-ollama.md)). The query path never spawns.

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `startup.rs:252` (`embeddings::spawn`), `lib.rs:212-213` (registration), `commands.rs:256`, `llm_commands.rs:738,756`, `scheduler.rs:171` (`embed_notify`), `tests/search_eval.rs`.
- **Outbound:** [fly-storage](fly-storage.md) (`search_split`, `vector_search`, `hybrid::fuse`, chunk queue incl. `*_shareable`), [app-ollama](app-ollama.md) (`root_url`, `server_alive`, `ensure_running`, `pull_model`), [app-llm](app-llm.md) (`is_loopback_url`), `AppState::{embed_notify, foreground_ai_active, pipeline_stage}`, `scheduler::recording_active`.

## Invariants
- Semantic search degrades to FTS and never fails a search or blocks the UI (`embeddings.rs:10-13`; `commands.rs:683-694`).
- The query path never spawns a server or pulls a model (`embeddings.rs:79-85`).
- Keep-local notes and transcripts are never sent to an Ollama on another machine for indexing; the decision is made from the URL actually used (THREAT_MODEL T10; `embeddings.rs:168-207`, `crates/fly-storage/src/privacy.rs:303-338`).
- The storage lock is never held across an HTTP call, and the startup pass holds it for 8 owners at a time (test `new_content_is_embedded_before_the_backlog_and_the_lock_is_free`).
- The backlog yields to recording, transcription (incl. polish) and foreground AI (`yield_reason`); a vector is stored only on a row still holding the embedded text (`store_pending_embeddings`).
- Ask / Enhance hold `AppState::foreground_ai()` for their whole run; a new foreground AI command should too.
- The `model` column records which model produced each vector, so a model change re-queues everything (`crates/fly-storage/src/lib.rs:531-534`).
- Not an invariant today: `vector_search` honouring `exclude_keep_local`. Any non-UI caller must post-filter until PR #92 lands.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/embeddings.rs` | 5 | newest-first batches with the lock free during embedding (fake embedder), a chunk rewritten mid-embed stays pending, the startup pass, yielding to transcription and foreground AI, pacing |
| unit (crate) | `crates/fly-storage/src/embeddings.rs:495-716` | 9 | chunking, pending/store/search round trip, re-sync, filters, backfill |
| unit (crate) | `crates/fly-storage/src/hybrid.rs:129-185` | 5 | RRF fusion, grouping, FTS-only degrade, limit |
| unit (crate) | `crates/fly-storage/src/privacy.rs:834` | 1 | shareable queue skips keep-local owners |
| env-gated | `src-tauri/tests/search_eval.rs` | 1 | 22 natural-language queries, FTS vs hybrid; asserts only hybrid ≥ FTS; **skips (passes) without a local Ollama + `embeddinggemma`**, which is every CI runner |

**Known gaps:**
- CI never exercises a real embedding path: `search_eval` early-returns there, and the app crate has no test for `search_semantic` or `embed_query`. The worker's loop itself (Ollama readiness, remote root) is untested; its steps are.
- `yield_reason`'s recording branch reuses `scheduler::recording_active`; no unit test builds an `ActiveRecording`. Checked in the real app instead.
- No test pins "degrade to FTS": Ollama down, model missing, `vector_search` error.
- No test covers the remote-root decision in the worker (`embeddings.rs:172,198`): a keep-local chunk reaching a non-loopback root would go unnoticed.
- No test that `vector_search` respects `exclude_keep_local`. It doesn't, by design until PR #92. Neither `search_eval` nor fly-storage sets the flag.
- `search_eval`'s "22/22 hybrid vs 4/22 FTS" headline is printed, not asserted; there is no committed absolute floor (spec §4.2a item 2).
- No test for the automatic model pull or its `model:progress` events.

## Changing it safely
- Run `cargo test -p fly-storage` (chunking, fusion, shareable queue) and, with Ollama + `embeddinggemma:300m` running locally, `cargo test -p fly-app --test search_eval -- --nocapture`. Read the printed table; a pass in CI means nothing.
- Changing `doc_prompt` / `query_prompt` / chunking changes every vector's meaning. Bump or re-queue (the `model` column only re-queues on a model-name change), then re-run `search_eval`.
- Any new caller that serves results to an external tool (MCP, agentic Ask) must set `exclude_keep_local` **and** post-filter vector hits until PR #92 lands.
- Keep `search` cheap: it is a synchronous command (`commands.rs:651-652`), so it runs on the main thread per keystroke (see `commands.rs:36-40`).
- devMock: keep `search` / `search_semantic` returning the `SearchHit` shape (`kind`, `note_id`, `title`, `snippet` with `[[match]]`, `start_ms`).
