# MCP server (`fly-mcp`)

> Layer: crate. Described at main `ccca11b` plus the memory PR (branch `claude/memory-mcp-f24b7d`). That PR moves the context layer's logic into [fly-context](fly-context.md) and adds `search_passages` (v2.5 row 3.6).
> Storage schema v3 (#92) is merged. `Storage::open_existing` refuses any version but its own, so a v2.4 server still running against a v3 database answers every tool call with the "Open Fly on the Wall once…" error until its client restarts it.

## Purpose
A stdio MCP server (binary `flyonthewall-mcp`) that lets external AI clients (Claude Desktop, Claude Code, Cursor, VS Code) read the user's meetings.
- It leads with a deterministic "context layer": `get_context` series briefings, and typed extracted items with provenance. This layer is the same "memory" code Ask uses (`fly_context::memory`).
- It adds passage-level search (`search_passages`) and exposes notes and transcripts for verification.
- It is read-only except for one write, `set_speaker_label`.
- It never holds API keys and never calls an LLM.
- Every result is filtered for **Keep local**.

## Files it owns
- `crates/fly-mcp/Cargo.toml` — `[[bin]] flyonthewall-mcp`; deps are only fly-core, fly-storage, fly-context (path dep; itself only fly-core + fly-storage), serde, serde_json, anyhow, tracing, dirs, chrono.
- `crates/fly-mcp/src/main.rs` — stdin/stdout loop, `--data-dir <path>` (default `<data_dir>`), logs to stderr, exits on stdin EOF.
- `crates/fly-mcp/src/lib.rs` — JSON-RPC framing and both protocol eras (`dispatch`, `handle_modern`), `Server`, `LazyServer`, the 14 tools, the Keep-local guards, input and result caps, unit tests.
- `crates/fly-mcp/src/context.rs` — **text only**: renders `fly_context::memory` results (`render_briefing`, `render_whats_changed`, `render_open_items`, `render_item_line`) in the wording clients already parse. It re-exports the series helpers (`normalize_title`, `detect_series`, `series_matching`, `Series`) from fly-context.
- `crates/fly-mcp/tests/context_golden.rs` — pins `get_context` / `whats_changed` / `open_items` text byte for byte (fixed dates, ids redacted), including a Keep-local meeting in the series.
- `crates/fly-mcp/tests/stdio.rs` — spawns the real binary: legacy handshake (including `search_passages`) and the stateless 2026-07-28 dialect.
- `crates/fly-mcp/tests/stdio_lifecycle.rs` — the server must exit within 5 s of stdin EOF.
- `scripts/mcp-smoke.mjs` — manual smoke that calls every tool over stdio. Its default data dir is `<data_dir>`; the one write re-sets an existing label.

Packaging, the Settings snippet and CI staging are in [app-mcp-sidecar](app-mcp-sidecar.md).

## Surface area
### Public API
No other crate depends on fly-mcp (the app ships the binary; it does not link the crate).
- **Servers** `Server::new(Storage)`, `Server::handle_message(&str) -> Option<String>`. `LazyServer::new(PathBuf)`, `is_open()`, `handle_message()`. `None` means a notification (no reply).
- **Constants** `SERVER_NAME = "flyonthewall"`, `SERVER_VERSION` (crate version), `MODERN_PROTOCOL_VERSION = "2026-07-28"`, `DB_UNAVAILABLE_MESSAGE`, `KEEP_LOCAL_MEETING_MESSAGE`, `KEEP_LOCAL_NOTE_MESSAGE`, `MAX_SPEAKER_LABEL_CHARS = 100`, `MAX_QUERY_CHARS = 1_000`, `MAX_RESULT_BYTES = 1_000_000`.
- **Context** `context::{normalize_title, Series, detect_series, series_matching}` (re-exported from `fly_context::memory`) and the text renderers `context::{render_briefing, render_whats_changed, render_open_items, render_item_line}`.

**JSON-RPC methods** (`dispatch`, `handle_modern`)

| Method | Era | Notes |
|---|---|---|
| `initialize` | legacy | Echoes the client's `protocolVersion` (default `2024-11-05`), returns `serverInfo` + `instructions` (`initialize`). |
| `ping`, `tools/list`, `tools/call` | both | Modern requests must carry `_meta["io.modelcontextprotocol/protocolVersion"] = "2026-07-28"` and `_meta[".../clientCapabilities"]`; modern results gain `resultType: "complete"` and `_meta[".../serverInfo"]` (`modernize`). Modern `tools/list` adds `ttlMs: 3600000`, `cacheScope: "public"`. |
| `server/discover` | modern only | Without the modern `_meta` it is `-32602` (`dispatch`). |
| notifications (no `id`) | both | No reply (`dispatch`). |

Errors: `-32700` parse, `-32601` unknown method, `-32602` bad params, `-32022` unsupported protocol version with `data.supported` / `data.requested` (`handle_modern`). A tool failure is a normal result with `isError: true`.

**Tools** (`tools_call`; schemas in `tools_list`). `limit` is clamped to 1-100 (`arg_limit`), except `search_passages` (1-20, clamped by fly-context); dates are RFC 3339 or `YYYY-MM-DD`: a bare `since` date means from the start of that day (`arg_time`), and a bare `until` date means through its end, on every tool that takes one (`arg_until`, and fly-context for `search_passages`); `query` is capped at 1,000 chars (`arg_query`); every result is cut at 1 MB (`cap_result`).

| Tool | Arguments | Reads | Keep-local handling |
|---|---|---|---|
| `get_context` | `query` (req), `since` | `fly_context::memory::mcp::get_context`: every visible meeting (`list_meetings_filtered`), series detection, then `search_filtered(query, 20)` as a full-text fallback; the briefing reads transcripts (speaker labels, `said by`), `get_meeting_items` counts and `query_items` (limit 500). Rendered by `context::render_briefing`. | `Context` under `ShareableOnly`: Keep local filtered in SQL (meetings, fallback hits, items) and again by the fail-closed `Hidden` sets; an item is kept only when its meeting is visible. A failed lookup refuses the call. |
| `whats_changed` | `series` (req), `since` (req) | Same briefing, restricted to meetings started at/after `since` (`memory::mcp::whats_changed`) | Same as `get_context` |
| `open_items` | `scope` (`"all"` or a series name) | `memory::mcp::open_items`: `query_items` (limit 200), then keeps action items and commitments whose status isn't `done` | Same as `get_context` |
| `query_items` | `type`, `owner`, `status`, `since`, `limit` (default 50) | `query_items` (`tool_query_items`) | `exclude_keep_local: true` |
| `get_meeting_items` | `meeting_id` | `get_meeting_items` (`tool_get_meeting_items`) | `guard_meeting` refuses |
| `search_notes` | `query` (req), `folder_id`, `since`, `until`, `limit` (default 20), `cursor` (`"<note>:<transcript>"`) | `search_split` FTS over notes and transcripts (`tool_search`) | `exclude_keep_local: true` inside SQL, so hidden rows never fill a page or shape a cursor |
| `search_passages` | `query` (req), `meeting_ids` (≤50), `folder_id`, `since`, `until`, `limit` (1-20, default 8) | `fly_context::Context::search_passages`, keyword only (no embedder in this process); rendered by `fly_context::render::passages` (`[seg:ID] Speaker (m:ss): text`) plus a pointer to `get_transcript` | `ShareableOnly`: Keep local inside the FTS SQL and by the `Hidden` sets; a `meeting_ids` scope naming a hidden meeting reads like an empty result |
| `list_folders` | none | `list_folders`, `keep_local_folder_ids` (`tool_list_folders`) | Local-only folders and descendants omitted; a visible folder never names a hidden parent |
| `get_note` | `note_id` | `get_note` → markdown, `updated`, `meeting_id`, attachment names and relative paths (`tool_get_note`) | `guard_note` refuses |
| `get_transcript` | `meeting_id`, `with_segment_ids` | `get_transcript`, polished variant preferred (`get_cleaned_transcript`), `get_meeting` for header/attendees (`render_transcript`) | `guard_meeting` refuses |
| `get_transcripts` | `meeting_ids` (1-20), `with_segment_ids` | Same rendering per id; stops before the 1 MB cap and lists skipped ids (`tool_get_transcripts`) | Refused ids render the refusal text in place |
| `get_meeting` | `meeting_id` | `get_meeting`, transcript presence (`tool_get_meeting`) | `guard_meeting` refuses |
| `list_recent` | `limit` (default 10), `folder_id`, `since`, `until`, `cursor` (`"<rfc3339>\|<id>"`) | `list_notes_filtered(…, exclude_keep_local = true)` (`tool_list_recent`) | Excluded inside SQL |
| `set_speaker_label` | `meeting_id`, `speaker_key`, `label` (all req) | `get_transcript`; then **writes** via `Storage::relabel_speaker` (`tool_set_speaker_label`) | `guard_meeting` refuses; only existing speaker keys; label cleaned (control/bidi/zero-width chars → spaces, ≤100 chars) (`clean_speaker_label`) |

`guard_meeting` refuses when the meeting's own flag or its note's effective flag is set, and refuses an unknown id that still has orphaned transcript or item rows. All Keep-local lookups fail closed (`keep_local_lookup_error`, and `context_error` for fly-context's storage errors).

### Tauri commands
None (crates expose no commands). The app's `mcp_config` command lives in [app-mcp-sidecar](app-mcp-sidecar.md).

### Events
None.

### Settings keys
None read or written.

### Database
Opened by `LazyServer::ensure_open` with `Storage::open_existing` (`crates/fly-storage/src/lib.rs:231-256`): read-write connection flags, 5 s busy timeout, `foreign_keys=ON`, `secure_delete=ON`, persistent WAL, no migration, no column repair, no template seeding. The schema must be exactly `SCHEMA_VERSION` (2) with every required table and column (`crates/fly-storage/src/lib.rs:302-335`); otherwise each tool call returns `DB_UNAVAILABLE_MESSAGE` plus the reason, and the open is retried on the next message.

| Table | Columns | Read / write |
|---|---|---|
| `notes`, `notes_fts` | id, title, folder_id, meeting_id, scratchpad, blocks_json, attachments_json, updated_at | Read |
| `meetings` | id, title, note_id, attendees, started_at, ended_at, recording, keep_local | Read |
| `folders` | id, name, parent_id, keep_local | Read |
| `meeting_items` | all columns | Read |
| `transcripts` | meeting_id, json, cleaned_json | Read; **written** by `set_speaker_label` (both variants, `crates/fly-storage/src/transcripts.rs:151-175`) |
| `transcripts_fts` | meeting_id, body | Read; rewritten by `set_speaker_label` (`crates/fly-storage/src/transcripts.rs:268-291`) |
| `embedding_chunks` | the transcript's chunk rows | Re-synced by `set_speaker_label` (labels are in chunk text) |
| every table | — | Created, migrated and seeded only when **no database file exists yet**: `ensure_open` falls back to the full `Storage::open` (`ensure_open`) |

### Files and directories written
- By `set_speaker_label`: the transcript mirrors `transcript{,.cleaned}.{md,json,vtt}` next to the recording, or `transcripts/<meeting_id>{,.cleaned}.*` when the meeting has no recording folder (`crates/fly-storage/src/transcripts.rs:286-322`).
- First run only (no database yet): `<data_dir>` itself, `notes/`, `transcripts/`, `attachments/`, `recordings/`, and `flyonthewall.db` (`crates/fly-storage/src/lib.rs:190-209`).
- SQLite `-wal` / `-shm` files (persistent; the server is often the last connection to close).

### Network egress
None. No network crate is in the dependency closure (fly-core and fly-storage have none). Everything a tool returns goes to the **MCP client's** model provider; that transfer is the client's, not this process's.

### External processes
None spawned. The binary itself is spawned by the MCP client (for example Claude Desktop) and lives as long as the client keeps its stdin open.

### Secrets and keychain entries
None. fly-secrets is not a dependency (spec §2 item 5).

## Dependencies
- **Inbound:** MCP clients over stdio. The app ships the binary as a Tauri `externalBin` and prints a config snippet ([app-mcp-sidecar](app-mcp-sidecar.md)). No Rust crate depends on fly-mcp.
- **Outbound:**
  - [fly-context](fly-context.md): `Context` under `Visibility::ShareableOnly`; `memory::mcp::{get_context, whats_changed, open_items}`; `search_passages`; `render::passages`. It owns the context layer's logic, which Ask shares.
  - [fly-storage](fly-storage.md): `Storage`, `ItemFilter`, `SearchFilter`, `StorageError`, the Keep-local CTE.
  - [fly-core](fly-core.md): `Meeting`, `MeetingItem`, `ItemKind`, `Attendee`, `Transcript`.
  - serde_json, chrono, dirs.
- **Extraction is not here.** `meeting_items` rows are produced by the app ([app-llm](app-llm.md) `extract_meeting_items` / backfill); this server only reads them (DECISIONS.md 2026-07-13).

## Invariants
- **One write only, `set_speaker_label`.** DECISIONS.md "M6 — MCP server" 2026-07-02 (read-only by design) and 2026-07-13 (one write tool). Enforced only by the dispatcher's tool match (`tools_call`); the connection is opened read-write (`crates/fly-storage/src/lib.rs:239-242`) because the write needs it, so there is no `query_only` backstop. The tool list is pinned by `initialize_and_list_tools`.
- **Never holds keys or calls a cloud LLM.** Spec §2 item 5; DECISIONS.md 2026-07-13 ("the server holds no API keys and makes no LLM calls"). Enforced structurally: `crates/fly-mcp/Cargo.toml` has no fly-secrets, fly-llm, reqwest or tokio, and neither do fly-context / fly-storage / fly-core. No test or cargo-deny rule guards it.
- **Keep-local meetings and notes are never returned.** docs/SECURITY_AUDIT_2026-10.md Round 3 ("MCP server"); enforced by the by-id guards (`guard_note`, `guard_meeting`), `exclude_keep_local` in `search_notes` / `list_recent` / `query_items`, and, for the context layer and `search_passages`, by fly-context under `ShareableOnly` (SQL filters plus the fail-closed `Hidden` sets). Lookup errors refuse the whole result. **Doc drift:** docs/THREAT_MODEL.md T10 still lists "The MCP server can still return keep-local meetings to its client" as residual risk, and Settings › Other AI apps says "It can read every note, including notes you keep local" (`frontend/src/components/SettingsModal.tsx:2209-2212`). Both predate or ignore commit `a5e1995` (2026-10-23) and over-warn.
- **The app owns the schema.** The server never migrates, repairs or seeds an existing database (`crates/fly-storage/src/lib.rs:221-256`; SECURITY_AUDIT Round 3). Exception: with no database file at all it runs the full `Storage::open` (`ensure_open`).
- **stdout is the protocol channel.** Logs go to stderr (`main.rs:24-29`, `LazyServer::new`).
- **The server dies with its client.** It exits cleanly on stdin EOF (`main.rs:33-45`) so it can't block an installer (DECISIONS.md 2026-07-13; `tests/stdio_lifecycle.rs`). The NSIS hook `taskkill`s it as a backstop (`src-tauri/installer-hooks.nsi`).
- **Stateless in both eras; legacy replies byte-identical.** DECISIONS.md 2026-08-09; the module docs, `dispatch`.
- **Deterministic context.** No LLM at query time; every briefing line cites `meeting:` and `segments:` (`fly_context::memory`, rendered by `context.rs`).
- **Context-layer text is a contract.** `tests/context_golden.rs` pins it byte for byte. The shared memory code may change only if these pins still pass, or the change is deliberate and the pins are updated with it.
- **Bounded output.** Query ≤1,000 chars, result ≤1 MB, `get_transcripts` ≤20 ids and whole transcripts only, speaker label ≤100 chars (the `MAX_*` constants, `arg_query`, `cap_result`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-mcp/src/lib.rs` | 33 | Fail-closed context layer and `search_passages` when the Keep-local lookup breaks, one unreadable transcript in a series, a bare `until` date covering the whole day on every tool; both protocol eras, tool list pin, every tool's happy path (including `search_passages` and its argument errors), write validation and sanitising, caps, cursor paging completeness, Keep-local refusals and filtering (direct, lists, search, passages, items, briefings, folders, orphans), lazy open and recovery |
| integration (runs in CI) | `crates/fly-mcp/tests/context_golden.rs` | 3 | `get_context` (series, `since`, full-text fallback, misses), `whats_changed` and `open_items` text pinned byte for byte, with a Keep-local meeting in the series |
| integration (runs in CI) | `crates/fly-mcp/tests/stdio.rs` | 2 | The real binary over stdio, legacy (including `search_passages`) and 2026-07-28 dialects |
| integration (runs in CI) | `crates/fly-mcp/tests/stdio_lifecycle.rs` | 1 | Exit within 5 s of stdin EOF |
| CI job | `.github/workflows/ci.yml` `sidecar` | — | Release-profile build answers `tools/list` (Linux) |
| manual | `scripts/mcp-smoke.mjs` | — | Every tool against a real data dir |

**Known gaps:**
- No test proves the read tools never write. The connection is read-write and read-only rests on code paths; a regression in a fly-storage "read" (a heal or backfill on read) would go unnoticed. Risk: silent mutation of user data by an external client.
- Schema-version refusal is not tested through the server. `lazy_server_serves_errors_until_the_database_opens` uses a garbage file; the version check itself is tested only in fly-storage. Risk: PR #92's v3 bump vs. a running v2.4 server.
- `set_speaker_label` side effects (mirror files, `transcripts_fts`, `embedding_chunks`, cleaned variant) are not asserted here. Only the rendered label is.
- The first-run `Storage::open` fallback (creates `<data_dir>` and a database from the MCP process) has no test, and it bypasses the app's legacy data-dir migration (see Changing it safely).
- Nothing guards spec §2 item 5 at build time (for example a `cargo tree -p fly-mcp` check for reqwest / fly-secrets).
- The context-layer tools don't route person queries. `get_context("Dana")` still falls back to full-text routing; Ask's `get_briefing` resolves people. Adding person routing here would change replies, so it waits for a deliberate pin update.

## Changing it safely
- Run `cargo test -p fly-mcp`. The stdio tests spawn the debug binary via `CARGO_BIN_EXE_flyonthewall-mcp`, so they need no staging.
- **Adding a tool:** update `tools_call`, `tools_list`, the pinned name list in `initialize_and_list_tools`, the count `14` in `modern_requests_are_stateless_and_self_describing`, `INSTRUCTIONS`, and `scripts/mcp-smoke.mjs`. Give it a Keep-local guard or SQL filter and add it to the `KeepLocalFixture` tests (`KeepLocalFixture`).
- **Adding a write:** don't, without a new DECISIONS.md entry. If you must, mirror the app command's validation and Keep-local refusal and assert the no-write path.
- **Storage schema bumps** (PR #92): the server refuses mismatched schemas by design. Ship the app and binary together; a client holding an old server process needs a restart. The NSIS hook kills running servers on install.
- **Never write to stdout** outside a reply. Use `eprintln!`.
- **Never add** fly-secrets, fly-llm, reqwest or any network crate (spec §2 item 5).
- **Text output is a contract** for clients (`list_recent_text_is_pinned_and_omits_app_status`, `tests/context_golden.rs`). Change wording deliberately.
- **Context-layer logic lives in fly-context** (`fly_context::memory`, shared with Ask). Change it there, then run both `cargo test -p fly-context` and `cargo test -p fly-mcp`; the golden pins catch any change to MCP replies. `context.rs` here is wording only.
- **First-run fallback trap:** `Storage::open` from the MCP creates `<data_dir>`. The app's one-time pre-rebrand migration only moves a legacy dir when the new one doesn't exist yet (`src-tauri/src/state.rs:123-144`), so a server started before the app's first post-rebrand launch would strand that data (inferred; narrow window).
- When this crate's Keep-local behavior changes, update docs/THREAT_MODEL.md T10 and the Settings "What it can see" copy together.
