# Meeting context for AI readers (`fly-context`)

> Layer: crate. Described at main `82b7edf` (v2.4.0).
> In flight: **PR #92** (draft, v2.5) rewires this crate onto storage schema v3:
> - `vector_search` filters Keep local (and orphan chunks) inside SQL before its limit. Unscoped searches then ask for 200 hits instead of every hit.
> - Vector hits map to windows by the chunk's `segment_ids`, falling back to `start_ms`.
> - `list_meetings` uses `list_meetings_filtered(&MeetingFilter)`, with date, folder and Keep-local filters in SQL.
> - The in-memory Keep-local sets stay as defense in depth.
> - The chunker lockstep test compares `segment_ids` / `end_ms` instead of text, because chunk text becomes label-free.
> - `MIGRATION.md` is rewritten to record the landed v3 schema.
>
> **Dormant**: no app or MCP caller yet. The Ask harness PR 3.3 will wire it.

## Purpose
Typed, read-only access to the user's meetings for AI readers: the Ask harness, Enhance, and eventually the MCP server. It offers six "tool" functions with JSON schemas, serializable results, and compact model-facing text that cites transcript lines as `[seg:ID]`. A `Context` is built with a `Visibility`. Under `ShareableOnly` (any cloud model or external AI tool), Keep-local meetings and notes are excluded on every path, and a failed Keep-local lookup fails closed. The crate has no LLM, keys or network.

## Files it owns
- `crates/fly-context/Cargo.toml`: deps fly-core, fly-storage, serde(_json), thiserror, chrono. Dev-dep: tempfile. No LLM, key or network crate.
- `crates/fly-context/src/lib.rs`: `Context`, `Visibility`, `QueryVector`, `ContextError`, the Keep-local `Hidden` sets and by-id guards.
- `crates/fly-context/src/passages.rs`: speaker-turn windows (kept in lockstep with `fly_storage::embeddings::chunk_transcript`), query terms, lexical scoring, RRF fusion with a per-owner cap.
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
| Helpers | `passages::{transcript_windows, window_text, query_terms, lexical_score, fuse, Window, PassageKey, Via}`, `render::{clock, segment_line, passages, segments, meetings, event, note, items}` |

The six tools (`tools.rs:137-313`):
| Tool | Arguments (bounds clamped in code) | Keep-local enforcement under `ShareableOnly` |
|---|---|---|
| `search_passages` | `query` (required, ≤1,000 chars), `meeting_ids` (≤50), `folder_id`, `since`, `until`, `limit` 1-20 (default 8) | FTS with `exclude_keep_local` in SQL (`tools.rs:633-640`), then the `Hidden` set re-checked per meeting and note (`:680,714`). Vector hits: on main `vector_search` ignores the flag, so the crate asks for every hit, drops Keep-local and orphan chunks, then caps at 200 (`:737-753`). |
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
| `notes_fts`, `transcripts_fts` | FTS5 | read (`search_split`) |
| `embedding_chunks` | owner_kind, owner_id, text, start_ms, model, embedding | read (`vector_search`) |
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
- **Outbound:** [fly-storage](fly-storage.md): `Storage`, `SearchFilter`, `ItemFilter`, `SearchHitKind`, `embeddings::{chunk_transcript, chunk_note}`, and `keep_local_note_ids` / `keep_local_meeting_ids` / `meeting_keep_local` / `note_keep_local`. [fly-core](fly-core.md): `Meeting`, `Transcript`, `MeetingItem`, `ItemKind`, `BlockOrigin`. Also chrono, serde(_json), thiserror.

## Invariants
- **No LLM, no keys, no network** (`lib.rs:4-6`, `Cargo.toml:7-9`). This keeps it safe to share with the MCP binary (spec §2 item 5).
- **Under `ShareableOnly`, Keep-local notes and meetings never appear on any path.**
  - The set lookups propagate errors, so they fail closed (`lib.rs:129-137`).
  - The by-id guards use storage predicates that answer "keep local" on any lookup error (`lib.rs:161-176`; `crates/fly-storage/src/privacy.rs:209-241`).
  - Sources: THREAT_MODEL T10; spec §4.2a #13. Tests: `keep_local_is_excluded_on_every_path`, `each_path_hides_keep_local_on_its_own`.
- **Tool output carries no trace of excluded meetings:** no ids, titles, dates or counts. A window holding only Keep-local meetings looks like an empty window, so a client can't binary-search their dates (`lib.rs:139-145`; test `tool_output_carries_no_keep_local_metadata`).
- **`Visibility::All` is only for a model running on this machine** (`lib.rs:45-47`). The caller chooses it. fly-llm's `is_local()` is true even for a remote Ollama (`crates/fly-llm/src/openai_compat.rs:68-76`), so base the choice on `build_provider`'s on-device rule (`src-tauri/src/llm_commands.rs:300`).
- **Filter the vector candidate pool before capping it,** so Keep-local or out-of-scope chunks can't crowd out shareable ones (`tools.rs:737-748`; tests `keep_local_chunks_cannot_starve_the_vector_pool`, `out_of_scope_chunks_cannot_starve_a_scoped_search`). On main this works because the crate asks `vector_search` for every hit, since `vector_search` ignores `exclude_keep_local` (`crates/fly-storage/src/embeddings.rs:391-417`).
- **Transcript windows mirror fly-storage's chunker exactly** (same turns, 160-word target, 30-word tail merge), so a vector hit's `start_ms` maps back to its segment ids (`passages.rs:1-19`; test `windows_match_the_storage_chunker_exactly`).
- **Arguments reject unknown fields.** Strict schemas carry no numeric or length keywords, and bounds live in descriptions and are clamped in code (`tools.rs:1-13,319-430`; test `schemas_are_strict_compatible_and_args_round_trip`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-context/src/passages.rs` | 4 | chunker lockstep, window coverage, query terms / lexical score, RRF fusion and per-owner cap |
| unit | `crates/fly-context/src/render.rs` | 1 | `clock()` formatting |
| integration (runs in CI) | `crates/fly-context/tests/context.rs` | 13 | every tool on a real SQLite fixture (one shareable meeting, a folder-local one, an inherited sub-folder one, a note-flagged 1:1). Keep-local exclusion per path and in aggregate, no metadata leak, vector-pool starvation, schemas, argument limits. Semantic search uses a deterministic hashed bag-of-words embedder. |

**Known gaps:**
- **The fail-closed path is never exercised.** No test makes `keep_local_note_ids` / `keep_local_meeting_ids` fail and checks that a call errors instead of returning unfiltered content.
- **`get_items` relies only on the SQL filter** (`tools.rs:1099`), with no defense-in-depth post-filter. A regression in `query_items`'s Keep-local CTE is caught only by this crate's fixture tests.
- **Model-proposed arguments aren't checked against an Ask scope or the retrieved set** (spec §4.2a #13). Any meeting id the visibility allows is readable, and no test covers this.
- **`UnknownTool` echoes the model-supplied name unbounded** (`tools.rs:1182-1189`). fly-llm's loop caps it at 64 chars.
- **No test pins `tool_defs()` schemas against fly-llm's `strict_schema` / `closed_schema` transforms or a real provider's strict validator.** This cross-crate contract is unpinned.
- **`list_meetings`' `folder_id` filter is untested.** In `scoped_search_and_date_filters`, the `until` assertion passes vacuously if the search returns nothing (`tests/context.rs:388-391`).
- **Cost isn't bounded by any test.** `list_meetings` and `excluded_keep_local_meetings` load every meeting per call, and search loads whole transcripts per hit.

## Changing it safely
- Run `cargo test -p fly-context`, plus `cargo test -p fly-storage` when you touch chunking, search or privacy SQL.
- Change `passages::transcript_windows` together with `fly_storage::embeddings::chunk_transcript`; the lockstep test fails otherwise. PR #92 replaces the text check with `segment_ids`.
- **New tool:** add it to `tool_defs()`, `call()` and `render`, and add it to the call lists in `keep_local_is_excluded_on_every_path` and `tool_output_carries_no_keep_local_metadata`.
- Never put excluded counts, ids or "not searched" wording in a tool result. Only the user-facing trace may use `excluded_keep_local_meetings()`.
- Before wiring to a cloud model or the MCP, `vector_search` must filter Keep local in SQL (`MIGRATION.md`, "Read paths"). PR #92 does that; keep the by-id guards regardless.
- If you rebase onto PR #92, check that `list_meetings`' folder semantics still match (main matches `note.folder_id` exactly, `tools.rs:948-953`). Whether the SQL filter includes sub-folders is unverified.
