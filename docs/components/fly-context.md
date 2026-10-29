# Meeting context for AI readers (`fly-context`)

> Layer: crate. Described at main `82b7edf` (v2.4.0); `search_passages` refreshed for the v2.5 search-quality PR (keyword ranking, similarity floor, fill-after-cap, lazy hit mapping), on top of schema v3 (#92, merged).
>
> **Dormant**: no app or MCP caller yet. The Ask harness PR 3.3 will wire it.

## Purpose
Typed, read-only access to the user's meetings for AI readers: the Ask harness, Enhance, and eventually the MCP server. It offers six "tool" functions with JSON schemas, serializable results, and compact model-facing text that cites transcript lines as `[seg:ID]`. A `Context` is built with a `Visibility`. Under `ShareableOnly` (any cloud model or external AI tool), Keep-local meetings and notes are excluded on every path, and a failed Keep-local lookup fails closed. The crate has no LLM, keys or network.

## Files it owns
- `crates/fly-context/Cargo.toml`: deps fly-core, fly-storage, serde(_json), thiserror, chrono. Dev-dep: tempfile. No LLM, key or network crate.
- `crates/fly-context/src/lib.rs`: `Context`, `Visibility`, `QueryVector`, `ContextError`, the Keep-local `Hidden` sets and by-id guards.
- `crates/fly-context/src/passages.rs`: speaker-turn windows (kept in lockstep with `fly_storage::embeddings::chunk_transcript`), query terms (`fly_storage::query::query_terms`), IDF-weighted term matching, vector-hit anchors, RRF fusion with a per-owner cap that fills empty slots.
- `crates/fly-context/src/render.rs`: model-facing text renderings (`[seg:ID] Speaker (m:ss): text`).
- `crates/fly-context/src/tools.rs`: the six tools: definitions (plain and strict schemas), argument types, result types, implementations, and the `call()` dispatcher.
- `crates/fly-context/tests/context.rs`: integration tests against a real temporary SQLite database.
- `crates/fly-context/MIGRATION.md`: on main, the *proposed* retrieval schema (v2.5 PR 3.2b, "not landed"). PR #92 rewrites it to describe the landed v3 schema.

## Surface area
### Public API
| Group | Items |
|---|---|
| Context | `Context::new(&Storage, Visibility)`, `visibility()`, `excluded_keep_local_meetings()` (for the *user's* Ask trace only, never tool output; `lib.rs:139-157`) |
| Types | `Visibility::{All, ShareableOnly}`, `QueryVector { vector, model }` (embedding computed by the caller), `ContextError::{InvalidArgs, NotFound, KeepLocal, UnknownTool, Storage}`, `KEEP_LOCAL_MEETING`, `KEEP_LOCAL_NOTE` |
| Tool functions | `search_passages(&SearchPassagesArgs, Option<QueryVector>)`, `read_segments`, `list_meetings`, `get_event`, `get_note`, `get_items` (`tools.rs:614-1102`) |
| Tool plumbing | `tool_defs() -> Vec<ToolDef { name, description, input_schema, strict_input_schema }>`, `call(&Context, name, &Value, Option<QueryVector>) -> ToolOutput { json, text }`, name constants (`SEARCH_PASSAGES` …), `MAX_QUERY_CHARS = 1000`, `MAX_MEETING_IDS = 50` |
| Args / results | `*Args` structs (`deny_unknown_fields`, `null` = absent), `PassageResults`, `Passage`, `SegmentWindow`, `SegmentView`, `MeetingList`, `MeetingSummary`, `EventInfo`, `NoteView`, `NoteKind`, `ItemList` |
| Helpers | `passages::{transcript_windows, window_text, query_terms, matched_terms, lexical_score, weighted_match, resolve_anchor, fuse, Window, PassageKey (Transcript / Note / Chunk), Via}`, `render::{clock, segment_line, passages, segments, meetings, event, note, items}` |

#### How `search_passages` ranks (`tools.rs:632-1021`)
1. **Keyword list (transcripts).** The query's content terms (`fly_storage::query::query_terms`: lowercase words minus stopwords; a term of 4+ characters also matches as a prefix) get BM25 IDF weights from `Storage::segment_term_stats` over the segments in scope. Unscoped, the 40 meetings holding the most IDF mass are loaded (`owner_evidence` / `top_owners`, `tools.rs:1263-1300`); a meeting scope loads exactly those meetings. Each window scores the IDF mass of the terms it holds, counting its speaker labels and the meeting title; a window qualifies at ≥ 40% of the query's total IDF mass (`KEYWORD_MIN_COVERAGE`). Words the library has never seen count in that total, so one shared common word doesn't make a match. Up to 3 windows per meeting; ties within a meeting go to the window whose own text matches more, ties across meetings to the meeting with more keyword evidence, then the newer one (never the random ids, so the ranking is deterministic).
2. **Keyword list (notes).** The same, over note paragraph chunks (`note_term_stats`, title counts).
3. **Semantic list.** `vector_search_floor` with the model's floor (`fly_storage::embeddings::similarity_floor`: 0.35 for `embeddinggemma:300m`, none for unknown models such as test embedders). A meeting-scoped search applies no floor: the meeting is the question's subject. A hit in a meeting the keyword pass already loaded maps to its window now; any other becomes a `PassageKey::Chunk` and is only resolved (transcript parsed) if it makes the result.
4. **Fusion.** Reciprocal-rank fusion, at most 3 passages per meeting or note; slots the cap would leave empty go to capped-out passages.
5. **Result.** `PassageResults` adds `similarity` (per passage, same order: the raw cosine of its best semantic hit, `None` if only keywords found it), `top_similarity` (best cosine of any in-scope chunk, floor or not) and `similarity_floor`. These live on `PassageResults`, not `Passage`, because the eval runner builds `Passage` literals.

Measured on the eval gold set (63 answerable, 15 unanswerable questions, 16 meetings), before → after:

| Path | recall@8 | MRR@8 | coverage |
|---|---|---|---|
| Real `embeddinggemma:300m` (the app) | 0.984 → 0.984 | 0.636 → 0.703 | 0.984 → 0.984 |
| CI's hashed embedder | 0.687 → 0.889 | 0.357 → 0.557 | 0.667 → 0.889 |
| Keyword only (no Ollama) | 0.016 → 0.841 | 0.016 → 0.642 | 0.016 → 0.841 |

Unanswerable questions that still return passages: real embedder 15 → 14 of 15; keyword only 0 → 9 (it found nothing for any question before). Evidence MRR (note passages of the right meeting count) 0.763 → 0.844. Real-embedder top-1 cosine separates answerable from unanswerable at AUROC 0.78, so the floor reduces noise but can't decide "not found" alone: use `top_similarity` as one signal. Without the floor, the same keyword ranking drops real-embedder recall to 0.937.

The six tools (`tools.rs:137-313`):
| Tool | Arguments (bounds clamped in code) | Keep-local enforcement under `ShareableOnly` |
|---|---|---|
| `search_passages` | `query` (required, ≤1,000 chars), `meeting_ids` (≤50), `folder_id`, `since`, `until`, `limit` 1-20 (default 8) | Keyword statistics and candidates come from `segment_term_stats` / `note_term_stats` with `exclude_keep_local` in SQL (`tools.rs:651-658`), then the `Hidden` set is re-checked per meeting and note. Vector hits come from `vector_search_floor`, which filters Keep local in SQL before its limit; a hit is re-checked by note id and meeting id before it counts (`tools.rs:822-922`). |
| `read_segments` | `meeting_id`, `around_segment_id` (required), `before` / `after` 0-20 (default 3), `prefer_polished` | `guard_meeting` (`tools.rs:876`) |
| `list_meetings` | `since`, `until`, `folder_id`, `title_contains`, `attendee`, `limit` 1-50 (default 20) | `Hidden` set post-filter over every meeting (`tools.rs:924-928`) |
| `get_event` | `meeting_id` | `guard_meeting` (`tools.rs:969`) |
| `get_note` | exactly one of `note_id` / `meeting_id`; `kind` scratchpad / enhanced / both | `guard_meeting` + `guard_note` (`tools.rs:1026,1035`) |
| `get_items` | `meeting_ids` (≤50), `kind`, `owner`, `status` open/done, `since`, `limit` 1-200 (default 50) | SQL `exclude_keep_local` in `query_items` only, with no post-filter (`tools.rs:1092-1100`) |

### Tauri commands
None (crates expose no commands).

### Events
None.

### Settings keys
None.

### Database
All access goes through the fly-storage API, read-only.
| Table | Columns | Read / write |
|---|---|---|
| `meetings` | id, title, note_id, attendees_json, started_at, ended_at, recording_json, attendees_confirmed, keep_local | read (`get_meeting`, `list_meetings`, Keep-local sets) |
| `notes` | id, title, folder_id, scratchpad, blocks, meeting_id | read (`get_note`, Keep-local CTE) |
| `folders` | id, parent_id, keep_local | read (Keep-local CTE, `privacy.rs:90-102`) |
| `transcripts` | meeting_id, transcript JSON, `cleaned_json` | read (`get_transcript`, `get_cleaned_transcript`) |
| `notes_fts`, `segments_fts` + `transcript_segments` | FTS5 | read (`note_term_stats`, `segment_term_stats`) |
| `embedding_chunks` | owner_kind, owner_id, text, start_ms, segment_ids_json, model, embedding | read (`vector_search_floor`) |
| `meeting_items` | kind, text, owner, status, segment_ids_json, meeting_id | read (`query_items`, `get_meeting_items`) |

### Files and directories written
None. Tests use a `tempfile` directory.

### Network egress
None. Semantic search takes a `QueryVector` the caller already computed (`lib.rs:4-6,53-60`; `Cargo.toml:7-9`).

| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None | n/a | n/a |

### External processes
None.

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** none in production. No crate depends on it, and it isn't in `[workspace.dependencies]`. The planned caller is the Ask harness (PR 3.3). [fly-mcp](fly-mcp.md) does **not** use it: it has its own context assembly and Keep-local post-filters (`crates/fly-mcp/src/lib.rs:375-446`, `crates/fly-mcp/src/context.rs:153,244`).
- **Outbound:** [fly-storage](fly-storage.md): `Storage`, `SearchFilter`, `ItemFilter`, `SearchHitKind`, `segment_term_stats`, `note_term_stats`, `vector_search_floor`, `query::{query_terms, words, term_matches, TermStats}`, `embeddings::{chunk_transcript, chunk_note, similarity_floor}`, and `keep_local_note_ids` / `keep_local_meeting_ids` / `meeting_keep_local` / `note_keep_local`. [fly-core](fly-core.md): `Meeting`, `Transcript`, `MeetingItem`, `ItemKind`, `BlockOrigin`. Also chrono, serde(_json), thiserror.

## Invariants
- **No LLM, no keys, no network** (`lib.rs:4-6`, `Cargo.toml:7-9`). This keeps it safe to share with the MCP binary (spec §2 item 5).
- **Under `ShareableOnly`, Keep-local notes and meetings never appear on any path.**
  - The set lookups propagate errors, so they fail closed (`lib.rs:129-137`).
  - The by-id guards use storage predicates that answer "keep local" on any lookup error (`lib.rs:161-176`; `crates/fly-storage/src/privacy.rs:209-241`).
  - Sources: THREAT_MODEL T10; spec §4.2a #13. Tests: `keep_local_is_excluded_on_every_path`, `each_path_hides_keep_local_on_its_own`.
- **Tool output carries no trace of excluded meetings:** no ids, titles, dates or counts. A window holding only Keep-local meetings looks like an empty window, so a client can't binary-search their dates (`lib.rs:139-145`; test `tool_output_carries_no_keep_local_metadata`).
- **`Visibility::All` is only for a model running on this machine** (`lib.rs:45-47`). The caller chooses it. fly-llm's `is_local()` is true even for a remote Ollama (`crates/fly-llm/src/openai_compat.rs:68-76`), so base the choice on `build_provider`'s on-device rule (`src-tauri/src/llm_commands.rs:612`).
- **Filter the vector candidate pool before capping it,** so Keep-local or out-of-scope chunks can't crowd out shareable ones (`tools.rs:822-850`; tests `keep_local_chunks_cannot_starve_the_vector_pool`, `out_of_scope_chunks_cannot_starve_a_scoped_search`). `vector_search_floor` filters Keep local in SQL before its limit; a meeting-scoped search asks for every hit and applies the scope itself.
- **Keyword statistics are scoped like the results.** IDF weights and candidate meetings come only from segments and notes the visibility and filters allow, so Keep-local text can't shift the ranking of shareable passages.
- **Transcript windows mirror fly-storage's chunker exactly** (same turns, 160-word target, 30-word tail merge), so a vector hit's `start_ms` maps back to its segment ids (`passages.rs:1-19`; test `windows_match_the_storage_chunker_exactly`).
- **Arguments reject unknown fields.** Strict schemas carry no numeric or length keywords, and bounds live in descriptions and are clamped in code (`tools.rs:1-13,319-430`; test `schemas_are_strict_compatible_and_args_round_trip`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-context/src/passages.rs` | 5 | chunker lockstep, window coverage, query terms / lexical score, IDF-weighted matching, RRF fusion with the per-owner cap and fill |
| unit | `crates/fly-context/src/render.rs` | 1 | `clock()` formatting |
| integration (runs in CI) | `crates/fly-context/tests/context.rs` | 16 | every tool on a real SQLite fixture (one shareable meeting, a folder-local one, an inherited sub-folder one, a note-flagged 1:1). Keep-local exclusion per path and in aggregate, no metadata leak, vector-pool starvation, schemas, argument limits; questions finding keyword passages without vectors, the keyword coverage floor, the similarity floor and `top_similarity`. Semantic search uses a deterministic hashed bag-of-words embedder. |
| eval (separate crate) | `evals/` (PR #103) | — | the gold set above; CI scores it with a hashed embedder, so the real-embedder numbers come from a local run |

**Known gaps:**
- **The fail-closed path is never exercised.** No test makes `keep_local_note_ids` / `keep_local_meeting_ids` fail and checks that a call errors instead of returning unfiltered content.
- **`get_items` relies only on the SQL filter** (`tools.rs:1099`), with no defense-in-depth post-filter. A regression in `query_items`'s Keep-local CTE is caught only by this crate's fixture tests.
- **Model-proposed arguments aren't checked against an Ask scope or the retrieved set** (spec §4.2a #13). Any meeting id the visibility allows is readable, and no test covers this.
- **`UnknownTool` echoes the model-supplied name unbounded** (`tools.rs:1182-1189`). fly-llm's loop caps it at 64 chars.
- **No test pins `tool_defs()` schemas against fly-llm's `strict_schema` / `closed_schema` transforms or a real provider's strict validator.** This cross-crate contract is unpinned.
- **`list_meetings`' `folder_id` filter is untested.** In `scoped_search_and_date_filters`, the `until` assertion passes vacuously if the search returns nothing (`tests/context.rs:388-391`).
- **Cost isn't bounded by any test.** `list_meetings` and `excluded_keep_local_meetings` load every meeting per call. `search_passages` parses up to 40 transcripts for the keyword pass plus those of the semantic hits that make the result. Measured on a synthetic 1,016-meeting library (35k 768-d chunks): see [app-search](app-search.md#cost-at-library-size).
- **The tuned constants are measured on a 16-meeting gold set.** `KEYWORD_MIN_COVERAGE` (0.4), 3 keyword windows per meeting and the 0.35 floor were picked on it with the real embedder; re-measure them on a bigger corpus before trusting them further.

## Changing it safely
- Run `cargo test -p fly-context`, plus `cargo test -p fly-storage` when you touch chunking, search or privacy SQL.
- Change `passages::transcript_windows` together with `fly_storage::embeddings::chunk_transcript`; the lockstep test fails otherwise.
- **Judge retrieval changes with the real embedder,** not only CI's hashed one: the two disagree (a keyword change that raised hashed recall cut real-embedder recall from 0.98 to 0.79 until the similarity floor landed). Run the gold set through `search_passages` with `embeddinggemma:300m` and compare recall, MRR and unanswerables.
- Don't add fields to `Passage`, `SegmentView` or `QueryVector`: the eval runner builds them as struct literals. Put new per-passage data on `PassageResults`.
- **New tool:** add it to `tool_defs()`, `call()` and `render`, and add it to the call lists in `keep_local_is_excluded_on_every_path` and `tool_output_carries_no_keep_local_metadata`.
- Never put excluded counts, ids or "not searched" wording in a tool result. Only the user-facing trace may use `excluded_keep_local_meetings()`.
- Before wiring to a cloud model or the MCP, `vector_search` must filter Keep local in SQL (`MIGRATION.md`, "Read paths"). PR #92 does that; keep the by-id guards regardless.
- If you rebase onto PR #92, check that `list_meetings`' folder semantics still match (main matches `note.folder_id` exactly, `tools.rs:948-953`). Whether the SQL filter includes sub-folders is unverified.
