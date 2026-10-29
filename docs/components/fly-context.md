# Meeting context for AI readers (`fly-context`)

> Layer: crate. Described at main `ccca11b` (storage schema v3, #92 merged) plus the memory PR (branch `claude/memory-mcp-f24b7d`).
> In flight:
> - **search-indexing (v2.5):** reworks `search_passages` internals in `tools.rs` / `passages.rs` (IDF keyword coverage, similarity floor, a `per_meeting` cap and a higher `limit` on `SearchPassagesArgs`).
> - **Ask harness (PR 3.3, crate `fly-ask`):** the second caller. It consumes `memory_prefetch`, the memory tools and `fan_out` described here.

## Purpose
Typed, read-only access to the user's meetings for AI readers: the MCP server today, and the Ask harness and Enhance next. It offers:
- **Nine model-callable tools:** six read tools and three memory tools, each with a JSON schema, a serializable result, and compact model-facing text that cites transcript lines as `[seg:ID]`.
- **Memory:** the structured knowledge the app already extracted across meetings. That means recurring series, people (who owns or said what), decisions, action items and their status, commitments, open questions, key figures, and what changed since last time. Every fact carries its meeting id and segment ids. It is served as tools, as a deterministic per-question prefetch, and as the MCP server's context layer.
- **Fan-out:** a broad question split into sub-queries, searched in parallel within a budget (Ask tier C).

A `Context` is built with a `Visibility`. Under `ShareableOnly` (any cloud model or external AI tool), Keep-local meetings and notes are excluded on every path, and a failed Keep-local lookup fails closed. The crate has no LLM, keys or network.

## Files it owns
- `crates/fly-context/Cargo.toml`: deps fly-core, fly-storage, serde(_json), thiserror, chrono. Dev-deps: tempfile, rusqlite (to break the Keep-local lookup in a test). No LLM, key or network crate.
- `crates/fly-context/src/lib.rs`:
  - `Context`, `Visibility`, `QueryVector`, `ContextError`;
  - the Keep-local `Hidden` sets and by-id guards;
  - the crate-level `tool_defs()` / `call()` over every tool.
- `crates/fly-context/src/tools.rs`: the six read tools (definitions, arguments, results, implementations) and their dispatcher.
- `crates/fly-context/src/memory.rs`:
  - series detection (moved here from fly-mcp);
  - people matching;
  - briefing, open-items and what-changed assembly;
  - `memory_prefetch`;
  - the three memory tools;
  - the `memory::mcp` entry points.
- `crates/fly-context/src/fanout.rs`: `decompose`, `fan_out`, `FanoutBudget`, `Readers` (`Mutex<Storage>`, `OpenPerWorker`).
- `crates/fly-context/src/passages.rs`: speaker-turn windows (kept in lockstep with `fly_storage::embeddings::chunk_transcript`), query terms, lexical scoring, RRF fusion with a per-owner cap.
- `crates/fly-context/src/render.rs`: model-facing text.
  - Read tools use `[seg:ID] Speaker (m:ss): text`.
  - Memory and fan-out renderers pass meeting content through `safe()`, which defuses fence tags and `[seg:`.
- `crates/fly-context/tests/context.rs`: the read tools against a real temporary SQLite database.
- `crates/fly-context/tests/memory.rs`: memory and fan-out against a real temporary database.

## Surface area
### Public API
| Group | Items |
|---|---|
| Context | `Context::new(&Storage, Visibility)`, `visibility()`, `excluded_keep_local_meetings()` (for the *user's* Ask trace only, never tool output) |
| Types | `Visibility::{All, ShareableOnly}`, `QueryVector { vector, model }` (embedding computed by the caller), `ContextError::{InvalidArgs, NotFound, KeepLocal, UnknownTool, Storage}`, `KEEP_LOCAL_MEETING`, `KEEP_LOCAL_NOTE`, `Result<T>` |
| Read tools | `search_passages(&SearchPassagesArgs, Option<QueryVector>)`, `read_segments`, `list_meetings`, `get_event`, `get_note`, `get_items` |
| Memory | `get_briefing(&GetBriefingArgs)`, `open_items(&OpenItemsArgs)`, `whats_changed(&WhatsChangedArgs)`, `memory_prefetch(question, &MemoryPrefetch) -> Option<Briefing>`; results `Briefing`, `MemoryFact`, `CitedSegment`, `TimelineEntry`, `OpenItems`, `MatchedBy`; `Briefing::retain_meetings` / `cited`, the same on `OpenItems`; `memory::{normalize_title, detect_series, series_matching, Series}`; `memory::mcp::{get_context, whats_changed, open_items}` |
| Fan-out | `decompose(question, max_sub_queries) -> Vec<String>` (original first), `fan_out(&impl Readers, Visibility, queries, base: &SearchPassagesArgs, vectors, &FanoutBudget) -> Fanout`, `FanoutBudget::TIER_C`, `Fanout`, `FannedPassage`, `Readers` (impls for `Mutex<Storage>` and `OpenPerWorker`) |
| Tool plumbing | `tool_defs() -> Vec<ToolDef { name, description, input_schema, strict_input_schema }>` (all nine), `call(&Context, name, &Value, Option<QueryVector>) -> ToolOutput { json, text }`, name constants (`tools::SEARCH_PASSAGES` …, `memory::GET_BRIEFING`, `OPEN_ITEMS`, `WHATS_CHANGED`), `MAX_QUERY_CHARS = 1000`, `MAX_MEETING_IDS = 50` |
| Helpers | `passages::{transcript_windows, window_text, query_terms, lexical_score, fuse, Window, PassageKey, Via}`, `render::{clock, segment_line, passages, segments, meetings, event, note, items, briefing, briefing_within, open_items, fanout, safe}` |

**The nine tools.** Bounds are clamped in code; strict schemas carry none.

| Tool | Arguments | Keep-local enforcement under `ShareableOnly` |
|---|---|---|
| `search_passages` | `query` (required, ≤1,000 chars), `meeting_ids` (≤50), `folder_id`, `since`, `until`, `limit` 1-20 (default 8) | FTS and `vector_search` filter Keep local inside SQL, before their limits. The `Hidden` set is re-checked per meeting, note and vector hit. |
| `read_segments` | `meeting_id`, `around_segment_id` (required), `before` / `after` 0-20 (default 3), `prefer_polished` | `guard_meeting` |
| `list_meetings` | `since`, `until`, `folder_id`, `title_contains`, `attendee`, `limit` 1-50 (default 20) | `list_meetings_filtered` with `exclude_keep_local` in SQL, plus the `Hidden` post-filter |
| `get_event` | `meeting_id` | `guard_meeting` |
| `get_note` | exactly one of `note_id` / `meeting_id`; `kind` scratchpad / enhanced / both | `guard_meeting` + `guard_note` |
| `get_items` | `meeting_ids` (≤50), `kind`, `owner`, `status` open/done, `since`, `limit` 1-200 (default 50) | `exclude_keep_local` in SQL only (no post-filter) |
| `get_briefing` | `query` (required: a series, project, customer, topic or person), `person`, `since` | Every memory path. See [Memory](#memory). |
| `open_items` | `scope` (`"all"` or a series name), `owner`, `limit` 1-100 (default 30) | Every memory path |
| `whats_changed` | `series` (required), `since` (default: the series' newest meeting that has extracted items) | Every memory path |

### Memory
What "memory" means here is the knowledge the app has already extracted, offered as cited context. It is not free-form model memory. No model runs at query time.
- **Series:** meetings grouped by normalized title ("Weekly Sync 2026-07-01" → `weekly sync`) and attendee overlap (Jaccard ≥ 0.34 with the group's first meeting).
- **People:** attendee names and emails plus item owners of visible meetings.
  - `PersonKey::matches` treats "Dana" ~ "Dana Lee" ~ `dana.lee@…`, but "Dana Lee" ≁ "Dana Kim".
  - A bare first name shared by two different full names is ambiguous, so no person is chosen.
  - A first name that is also a common word ("Will", "May") counts only when capitalized mid-sentence.
- **Facts (`MemoryFact`):** one per `meeting_items` row.
  - `said_by` is the speaker's *current* label, so names set with `set_speaker_label` or in the app flow in on the next read.
  - `CitedSegment`s carry the cited segments as `SegmentView`s from the raw transcript.
  - Ids a re-transcribe removed don't resolve. The model-facing renderers leave out a fact with no resolvable id: it can't be cited.
- **Routing:**
  - `get_briefing`: a series whose name holds every query word, else a known person the query names, else series holding meetings whose transcript or notes match the query (FTS, 20 hits).
  - `memory_prefetch` (tier S, no model tool call) returns `Some` only when the question names a known person or names a series or project. "I" / "my" maps to the user's own "You" when "You" owns something.
    - A series matches on every word of its name. An all-generic name such as "weekly sync" must appear as written.
    - A project matches on one distinctive question word found in series names ("launch" → `launch weekly` + `launch retro`, newest 3).
    - "What changed …" narrows a series to its newest meeting with items.
    - Router-supplied `people` / `since` / `until` win.
  - Ask-facing calls cap at the newest 12 meetings and 40 facts (`DEFAULT_MAX_MEETINGS`, `DEFAULT_MAX_FACTS`). `facts_omitted` counts the cut.
- **Cost:** each meeting in a briefing loads its transcript once per call (participants, `said_by`, evidence). A person briefing reads at most the 30 newest candidate meetings.
- **The MCP entry points** (`memory::mcp::*`) keep fly-mcp's historical routing: no person routing, no caps, and no evidence for `open_items`. Their text is rendered by fly-mcp.

### Using it from the Ask harness
**Tier S (no model tool calls):**
1. Call `ctx.memory_prefetch(question, &MemoryPrefetch { people, since, until, max_facts, max_meetings })`.
   - Pass the router's validated `people` and dates.
   - A `0` cap means the defaults: 40 facts and 12 meetings.
2. If it returns `Some(b)`, call `b.retain_meetings(|m| resolved.allows_meeting(m))`.
3. Register every `c` in `b.evidence` with `evidence::info(&c.segment, &c.meeting_id, &c.meeting_title, Some(c.started_at))`.
4. Add `render::briefing_within(&b, item_budget)` to the prompt.

**Tiers M/C:** `get_briefing`, `open_items` and `whats_changed` arrive through `fly_context::tool_defs()`. `agent.rs` dispatches by name, so it needs one arm per tool:
1. Parse `GetBriefingArgs` / `OpenItemsArgs` / `WhatsChangedArgs` and call the method.
2. Call `retain_meetings`.
3. Register `evidence`.
4. Render with `render::briefing_within(&b, observation_chars)` or `render::open_items(&o)`.

**What the renderers guarantee the claim check:**
- Every printed `[seg:ID]` is in `evidence`.
- A fact whose sources are gone is not printed.
- Each fact is followed by its cited segments verbatim (`> Speaker: "…"`, at most 2, each clipped at a word boundary to ~220 chars).

Facts are extraction paraphrases ("Launch budget is $40k" from "forty thousand dollars"). The footer tells the model to take numbers, names and quotes from the source lines, and the claim check compares against the segments.

**Tier C:** `decompose(question, 4)`, then `fan_out(&readers, visibility, &queries, &scope_args, &vectors, &budget)`, with `budget.max_searches` set to what is left of the tier's tool calls. `render::fanout(&f)` prints every passage's lines with `[seg:ID]`, so register those segments too.

### Fan-out (tier C)
- **`decompose`:**
  - Splits on sentences, independent clauses ("… and who owns the checklist") and noun lists ("pricing, hiring and the launch" share their head).
  - Shares a last word only after capitalized names ("the Acme and Beta renewals").
  - Keeps the original first and returns at most 4 sub-queries.
  - A cloud model may supply its own sub-queries instead.
- **`fan_out`:**
  - Runs at most `max_searches` searches (duplicates and blanks cost nothing), `max_parallel` at a time, on scoped threads. Each thread builds its own `Context` with the caller's `Visibility`.
  - Any failed search fails the whole call.
  - Merges by RRF (k = 60) after a coverage pass that gives each query its best passage, then applies `max_passages`, `per_meeting` and `max_chars`.
  - Under `ShareableOnly`, re-checks the merged set against the Keep-local sets (fail closed).
- **Readers:**
  - `Mutex<Storage>` (the app's handle) runs searches one at a time.
  - `OpenPerWorker(data_dir)` opens a read connection per search with `Storage::open_existing`, which never migrates, so searches run in parallel under WAL.
- **`FanoutBudget::TIER_C`:** 5 searches, 8 passages per search, 16 merged, 3 per meeting, 24k chars, 4 at a time.

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
| `meetings` | id, title, note_id, attendees_json, started_at, ended_at, recording_json, attendees_confirmed, keep_local | read (`get_meeting`, `list_meetings_filtered`, Keep-local sets) |
| `notes` | id, title, folder_id, scratchpad, blocks, meeting_id | read (`get_note`, Keep-local CTE) |
| `folders` | id, parent_id, keep_local | read (Keep-local CTE) |
| `transcripts` | meeting_id, transcript JSON, `cleaned_json` | read (`get_transcript`, `get_cleaned_transcript`) |
| `notes_fts`, `transcripts_fts` | FTS5 | read (`search_split`, `search_filtered`) |
| `embedding_chunks` | owner_kind, owner_id, segment_ids, start_ms, model, embedding | read (`vector_search`) |
| `meeting_items` | kind, text, owner, status, speaker_key, segment_ids_json, meeting_id | read (`query_items`, `get_meeting_items`) |

### Files and directories written
None. Tests use a `tempfile` directory.

### Network egress
None. Semantic search takes a `QueryVector` the caller already computed.

| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None | n/a | n/a |

### External processes
None.

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:**
  - [fly-mcp](fly-mcp.md) uses it for `get_context` / `whats_changed` / `open_items` (`memory::mcp::*`) and for its `search_passages` tool, always under `ShareableOnly`.
  - The Ask harness (`fly-ask`, PR 3.3) is next.
  - `fly-app` doesn't depend on it directly.
- **Outbound:**
  - [fly-storage](fly-storage.md): `Storage`, `SearchFilter`, `ItemFilter`, `MeetingFilter`, `SearchHitKind`, `embeddings::{chunk_transcript, chunk_note}`, `Storage::open_existing` (fan-out readers), and `keep_local_note_ids` / `keep_local_meeting_ids` / `meeting_keep_local` / `note_keep_local`.
  - [fly-core](fly-core.md): `Meeting`, `Attendee`, `Transcript`, `MeetingItem`, `ItemKind`, `BlockOrigin`, `enhance::fence_safe`.
  - chrono, serde(_json), thiserror.

## Invariants
- **No LLM, no keys, no network** (`lib.rs:4-6`, `Cargo.toml`). This keeps it safe to share with the MCP binary (spec §2 item 5).
- **Under `ShareableOnly`, Keep-local notes and meetings never appear on any path.**
  - The set lookups propagate errors, so they fail closed.
  - The by-id guards use storage predicates that answer "keep local" on any lookup error.
  - Memory reads its meeting list, items and full-text routing with `exclude_keep_local` in SQL, re-checks every meeting against the `Hidden` sets, and keeps an item only when its meeting is visible.
  - People are learned only from visible meetings, so a name that appears only in a Keep-local meeting matches nothing.
  - Sources: THREAT_MODEL T10; spec §4.2a #13. Tests: `keep_local_is_excluded_on_every_path`, `each_path_hides_keep_local_on_its_own`, `keep_local_never_reaches_memory_under_shareable_only`, `a_failed_keep_local_lookup_fails_closed_on_every_memory_path`.
- **Tool output carries no trace of excluded meetings:** no ids, titles, dates or counts. A window holding only Keep-local meetings looks like an empty window, so a client can't binary-search their dates (test `tool_output_carries_no_keep_local_metadata`).
- **Memory comes only from the user's own meetings.** That means `meeting_items`, transcripts and meeting rows written by the app's pipeline and extraction.
  - Connector (external MCP server) results never feed memory, and nothing here writes.
  - Spec §4.3 fences connector output. A future "pinned facts" store would need its own marker and its own consent, not `[seg:]`.
- **Every memory fact keeps its provenance** (meeting id + segment ids).
  - The model-facing text cites only ids that resolve in the current transcript.
  - It never prints a literal `[seg:` from meeting content or a placeholder marker.
  - Tests: `series_briefing_cites_every_fact_and_shows_current_speaker_names`, `meeting_content_cannot_plant_citations_in_memory_text`.
- **`Visibility::All` is only for a model running on this machine.**
  - The caller chooses it.
  - fly-llm's `is_local()` is true even for a remote Ollama, so base the choice on `build_provider`'s on-device rule.
- **Filter the vector candidate pool before capping it,** so Keep-local or out-of-scope chunks can't crowd out shareable ones (tests `keep_local_chunks_cannot_starve_the_vector_pool`, `out_of_scope_chunks_cannot_starve_a_scoped_search`).
- **Transcript windows mirror fly-storage's chunker exactly** (same turns, 160-word target, 30-word tail merge). Test: `windows_match_the_storage_chunker_exactly`.
- **Arguments reject unknown fields.** Strict schemas carry no numeric or length keywords; bounds live in descriptions and are clamped in code (test `schemas_are_strict_compatible_and_args_round_trip`, all nine tools).
- **Fan-out stays inside its budget.** It never runs more than `max_searches` searches and never returns more than `max_passages` passages or more than `per_meeting` passages per meeting. Tests: `fan_out_respects_every_budget`, `fan_out_runs_searches_in_parallel_with_one_connection_each`.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src/passages.rs` | 4 | chunker lockstep, window coverage, query terms / lexical score, RRF fusion and per-owner cap |
| unit | `src/render.rs` | 1 | `clock()` formatting |
| unit | `src/memory.rs` | 7 | title normalization, series grouping and matching, person keys, people found only when named and known (common-word names, ambiguity), series/project words in a question |
| unit | `src/fanout.rs` | 4 | `decompose`: no split, clauses, noun lists and shared last words, caps and dedupe |
| integration (runs in CI) | `tests/context.rs` | 13 | the six read tools on a real SQLite fixture: Keep-local exclusion per path and in aggregate, no metadata leak, vector-pool starvation, schemas for all nine tools, argument limits |
| integration (runs in CI) | `tests/memory.rs` | 15 | briefings with provenance and current labels, relabel flow, person and narrowed briefings, what-changed default, open items (owner, scope, done), budgeted rendering, citation-planting defense, prefetch routing; Keep local on every memory path, including the tool path and a hidden-only person; fail-closed with a broken Keep-local lookup; fan-out merge/attribution, every budget, real parallelism (`OpenPerWorker`) vs serialized `Mutex<Storage>`, Keep-local exclusion |

**Known gaps:**
- **The fail-closed path is tested for memory and fan-out only.** No test makes the lookups fail for the six read tools.
- **`get_items` relies only on the SQL filter**, with no post-filter. The memory paths add one.
- **Model-proposed arguments aren't checked against an Ask scope** (spec §4.2a #13). The harness does that, with `retain_meetings` for memory results.
- **Prefetch routing is heuristic.**
  - People come from attendees and item owners, not from speaker labels. Loading every transcript's labels per question is too slow, so the Ask router's validated `people` should be passed in.
  - Status is as stated in each meeting. A later meeting saying "done" doesn't close an earlier item.
- **`decompose` is English-only.** It has no semantic check, and a junk sub-query only costs one search.
- **Cost:** `list_meetings` and memory load every meeting row per call; briefings load one transcript per meeting shown.

## Changing it safely
- Run `cargo test -p fly-context`, plus `cargo test -p fly-mcp` when you touch `memory.rs`: fly-mcp's `tests/context_golden.rs` pins the MCP's briefing and open-items text byte for byte. Also run `cargo test -p fly-storage` when you touch chunking, search or privacy SQL.
- Change `passages::transcript_windows` together with `fly_storage::embeddings::chunk_transcript`; the lockstep test fails otherwise.
- **New tool:**
  - Read tools go in `tools.rs`; memory tools go in `memory::tool_defs()` and `memory::call()`. The crate-level `tool_defs()` / `call()` pick both up.
  - Add it to the names in `schemas_are_strict_compatible_and_args_round_trip` and to the Keep-local call lists in both test files.
  - fly-ask's `agent.rs` dispatches tools by name, so it needs an arm too.
- **Memory routing** (`find_person`, `series_in_question`): keep it validated against visible data only, and add a case to the unit tests and to `prefetch_routes_people_series_and_projects_and_nothing_else`.
- Never put excluded counts, ids or "not searched" wording in a tool result. Only the user-facing trace may use `excluded_keep_local_meetings()`.
- Construct `SearchPassagesArgs` with `..Default::default()` outside this crate, so new fields don't break callers.
