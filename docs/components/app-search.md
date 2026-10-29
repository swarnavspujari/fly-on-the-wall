# Search and embedding indexer (`app-search`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0); the search half refreshed for the v2.5 search-quality PR (FTS fallback for questions, similarity floor, vector cache). Schema v3 (#92) has merged: `vector_search` filters Keep local in SQL, and chunk text is label-free (the corpus re-embeds once). The commands keep their signatures.

## Purpose
Search-as-you-type over notes and transcripts. `search` is SQLite FTS5: a keyword search must match every word (the last as a prefix); when that finds nothing, as with a typed question, any content word matches and bm25 ranks the rows sharing the rarest words first (`fly_storage::query`). `search_semantic` fuses that with a vector search (reciprocal rank fusion, grouped by note). The vector list has no similarity floor: it is ranked, the user decides, and a short keyword search scores low cosines even against the note it is after ("cost cuts" 0.30 vs 0.49 for the same topic asked as a question). Its vectors come from a background worker that chunks every note and transcript and embeds the chunks through Ollama's `/api/embed` with `embeddinggemma:300m`. Everything semantic is best-effort: no Ollama, no model or a dead server means FTS-only, never an error.

## Files it owns
- `src-tauri/src/commands.rs` (part) — `search` (651-660), `search_semantic` (662-715).
- `src-tauri/src/embeddings.rs` — the indexer (`spawn`), `embed_raw`, `embed_query`, prompts, model constant.
- Storage halves live in [fly-storage](fly-storage.md): `query.rs` (terms, stopwords, the AND / OR expressions), `search.rs` (`search`, `search_split`, `SearchFilter`), `embeddings.rs` (chunking, queue, `vector_search_floor`, `similarity_floor`), `cache.rs` (in-memory vectors), `hybrid.rs` (`fuse`), `privacy.rs:303-338` (shareable queue).

## Surface area
### Public API
`embeddings.rs`:

| Item | Line | Used by |
|---|---|---|
| `EMBED_MODEL`, `doc_prompt`, `query_prompt`, re-exported from `fly_core::embed_prompt` (`"embeddinggemma:300m"`, shared with the retrieval eval; `fly_storage::embeddings::EMBEDDINGGEMMA` is the same tag, and its similarity floor lives in fly-storage) | 25 | worker, `search_semantic`, `tests/search_eval.rs` |
| `doc_prompt(title, text)` → `"title: {title} \| text: {text}"`; `query_prompt(q)` → `"task: search result \| query: {q}"` | 37-46 | worker, `embed_query`, `search_eval` |
| `embed_raw(root, &[String]) -> Result<Vec<Vec<f32>>, String>`: `POST {root}/api/embed`, 300 s timeout, count-checked | 49-77 | worker, `embed_query`, `search_eval` |
| `embed_query(&AppState, q) -> Option<Vec<f32>>`: only if `GET {root}/api/version` answers; never spawns a server or pulls | 83-96 | `search_semantic` |
| `spawn(AppHandle)`: the background indexer (called once, `lib.rs:177`) | 151-240 | app setup |

#### Indexer loop (`embeddings.rs:151-240`)
1. Once at start: `Storage::backfill_embedding_chunks` (chunk rows for every note and transcript; idempotent).
2. Loop. `root = ollama::root_url` (`llm.ollama.base_url`, else `http://localhost:11434`). If the root is **not** loopback, count only the *shareable* backlog (keep-local owners excluded); otherwise the full backlog (172-181).
3. Backlog 0 → wait for `embed_notify` or 30 s (183).
4. `ensure_ready` (126-142): `ollama::ensure_running` (may spawn the managed `ollama serve`), then `GET /api/tags`; if the model is missing, **pull it** (`POST /api/pull`, progress on `model:progress`). Failure → wait 60 s or a nudge.
5. Re-derive remote/local from the URL actually used (198), take 16 pending chunks (shareable queue when remote), embed `doc_prompt(title, text)`, store vectors (`store_chunk_embeddings`). Failure → sleep 60 s.

The storage lock is never held across HTTP. `embed_notify` is nudged by `update_note_scratchpad` (`commands.rs:256`), Enhance/block edits (`llm_commands.rs:738,756`) and each finished transcription (`scheduler.rs:171`).

#### `search_semantic` (`commands.rs:662-715`)
`SearchFilter { limit: 30, ..Default }`, so `exclude_keep_local = false`: Keep-local notes **are** returned, which is right for the user's own UI. Runs `search_split` (FTS notes, FTS transcripts, with the AND → OR fallback), then `embed_query`. With a vector it calls `vector_search(…, EMBED_MODEL, &filter, 30)` (no floor); a failure there is logged and treated as empty. Then `hybrid::fuse`.

Measured on the eval gold set (63 questions; a hit = a note of the meeting holding the answer), before → after: **without Ollama** (FTS only) the right meeting is in the top 5 for 2 → 55 questions (MRR 0.03 → 0.77; FTS returned nothing for 53 of them before); **with** `embeddinggemma` 61 → 61 (MRR 0.86 → 0.82: a few answers moved from rank 1 to 2 now that the FTS lists contribute). Every unanswerable question now returns something in both modes (FTS shares a common word), as a search box should.

#### Cost at library size
A synthetic library of 1,016 meetings (34,990 chunks, 768-d vectors), minimum of 5 runs per query, 21 questions, on a shared and busy dev machine (treat as relative):

| Path (p50 / p95, ms) | main `ccca11b` | this change |
|---|---|---|
| `search_passages`, hybrid | 2,465 / 3,922 | 672 / 1,074 |
| `search_passages`, keyword only | 181 / 764 (it found nothing for most questions, so did little work) | 532 / 1,159 |
| `vector_search`, 200 hits | 1,835 / 3,170 | 106 / 355 |
| Search UI (`search_split` + vector + fuse) | 1,978 / 3,288 | 133 / 319 |

Where the time went on main: the vector scan read every 3 KB vector (and walked its overflow pages just to check `model`) per query, and keyword statistics read each matching segment's row. Now vectors come from an in-memory cache and visibility is decided per note and meeting.

What's left:
- **The cache rebuild.** A write to `embedding_chunks` (a stored vector, an edited note or transcript re-chunked) or any commit by another process drops the vector cache, so the next search re-reads every vector (about 1-3 s at 35k chunks on this machine; ~0.1-0.3 s for a few hundred meetings). Other writes (settings, job progress, items) keep it. During a bulk re-embed, when the worker stores a batch every few seconds, searches pay it about as often as main paid its scan.
- **Transcript parsing.** The keyword pass parses up to 40 transcripts per query (most of the remaining keyword-only time); caching parsed windows is the next cut.
- **Brute force.** Every in-scope vector is still scored (~35k dot products); an ANN index is the step past ~100k chunks.

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
| `embedding_chunks` | `id, owner_kind, owner_id, chunk_index, title, text, start_ms, embedding, model, segment_ids_json, end_ms, variant` | worker: backfill rows, read pending, write `embedding`/`model`; `search_semantic`: read (ids for the filter, vectors via the cache, details for the winners) |
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
- **Inbound:** `lib.rs:177` (`embeddings::spawn`), `lib.rs:212-213` (registration), `commands.rs:256`, `llm_commands.rs:738,756`, `scheduler.rs:171` (`embed_notify`), `tests/search_eval.rs`.
- **Outbound:** [fly-storage](fly-storage.md) (`search_split`, `vector_search`, `hybrid::fuse`, chunk queue incl. `*_shareable`), [app-ollama](app-ollama.md) (`root_url`, `server_alive`, `ensure_running`, `pull_model`), [app-llm](app-llm.md) (`is_loopback_url`), `AppState::embed_notify`.

## Invariants
- Semantic search degrades to FTS and never fails a search or blocks the UI (`embeddings.rs:10-13`; `commands.rs:683-694`).
- The query path never spawns a server or pulls a model (`embeddings.rs:79-85`).
- Keep-local notes and transcripts are never sent to an Ollama on another machine for indexing; the decision is made from the URL actually used (THREAT_MODEL T10; `embeddings.rs:168-207`, `crates/fly-storage/src/privacy.rs:303-338`).
- The storage lock is never held across an HTTP call (`embeddings.rs:149-150`).
- The `model` column records which model produced each vector, so a model change re-queues everything (`crates/fly-storage/src/lib.rs:531-534`).
- `vector_search` / `vector_search_floor` honour `exclude_keep_local` inside SQL, before the limit and the floor (#92). The in-memory vector cache never decides visibility: each search asks the database which chunk ids it may see.
- A keyword search that matches anything keeps AND semantics; the OR fallback is chosen from the query and filter only, never the page offset, so MCP `search_notes` cursors stay consistent.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/embeddings.rs`, `commands.rs` (search) | 0 | nothing in the app crate |
| unit (crate) | `crates/fly-storage/src/embeddings.rs` | 10 | chunking, pending/store/search round trip, re-sync, filters, backfill, the floor and exact best score |
| unit (crate) | `crates/fly-storage/src/{query,search,segments,cache}.rs` | 6 + 6 + 2 (search-related) + 2 | stopwords, AND / OR expressions, hostile input, the fallback and its paging, term statistics with keep-local, the vector cache's invalidation and quantization error |
| unit (crate) | `crates/fly-storage/src/hybrid.rs:129-185` | 5 | RRF fusion, grouping, FTS-only degrade, limit |
| unit (crate) | `crates/fly-storage/src/privacy.rs:834` | 1 | shareable queue skips keep-local owners |
| env-gated | `src-tauri/tests/search_eval.rs` | 1 | 22 natural-language queries, FTS vs hybrid; asserts only hybrid ≥ FTS; **skips (passes) without a local Ollama + `embeddinggemma`**, which is every CI runner |

**Known gaps:**
- CI never exercises any semantic path: `search_eval` early-returns there, and the app crate has no test for `search_semantic`, `embed_query` or the worker.
- No test pins "degrade to FTS": Ollama down, model missing, `vector_search` error.
- No test covers the remote-root decision in the worker (`embeddings.rs:172,198`): a keep-local chunk reaching a non-loopback root would go unnoticed.
- `vector_search`'s `exclude_keep_local` is covered in `crates/fly-storage/tests/migration_v3.rs` and the floor test; `search_eval` doesn't set it.
- `search_eval`'s "22/22 hybrid vs 4/22 FTS" headline is printed, not asserted; there is no committed absolute floor (spec §4.2a item 2).
- No test for the automatic model pull or its `model:progress` events.

## Changing it safely
- Run `cargo test -p fly-storage` (chunking, fusion, shareable queue) and, with Ollama + `embeddinggemma:300m` running locally, `cargo test -p fly-app --test search_eval -- --nocapture`. Read the printed table; a pass in CI means nothing.
- Changing `doc_prompt` / `query_prompt` / chunking changes every vector's meaning. Bump or re-queue (the `model` column only re-queues on a model-name change), re-run `search_eval`, and re-measure `similarity_floor` (fly-context's Ask search uses it): it is calibrated for these prompts.
- Any new caller that serves results to an external tool (MCP, agentic Ask) must set `exclude_keep_local`; keep a by-id re-check as defense in depth (fly-context does).
- Keep `search` cheap: it is a synchronous command (`commands.rs:651-652`), so it runs on the main thread per keystroke (see `commands.rs:36-40`).
- devMock: keep `search` / `search_semantic` returning the `SearchHit` shape (`kind`, `note_id`, `title`, `snippet` with `[[match]]`, `start_ms`).
