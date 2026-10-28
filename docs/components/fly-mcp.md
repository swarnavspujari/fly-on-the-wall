# MCP server (`fly-mcp`)

> Layer: crate. Described at main `82b7edf` (v2.4.0).
> In flight: PR #92 (draft, v2.5) bumps the storage schema `user_version` 2→3 and reworks the fly-storage search / items / notes filters this crate calls (`search_split`, `query_items`, `list_notes_filtered`). `Storage::open_existing` refuses any version but its own, so a v2.4 server still running against a v3 database answers every tool call with the "Open Fly on the Wall once…" error until its client restarts it. No open PR touches `crates/fly-mcp` itself.

## Purpose
A stdio MCP server (binary `flyonthewall-mcp`) that lets external AI clients (Claude Desktop, Claude Code, Cursor, VS Code) read the user's meetings. It leads with a deterministic "context layer" (`get_context` series briefings, typed extracted items with provenance) and exposes notes and transcripts for verification. It is read-only except for one write, `set_speaker_label`. It never holds API keys and never calls an LLM. Every result is filtered for **Keep local**.

## Files it owns
- `crates/fly-mcp/Cargo.toml` — `[[bin]] flyonthewall-mcp`; deps are only fly-core, fly-storage, serde, serde_json, anyhow, tracing, dirs, chrono.
- `crates/fly-mcp/src/main.rs` — stdin/stdout loop, `--data-dir <path>` (default `<data_dir>`), logs to stderr, exits on stdin EOF.
- `crates/fly-mcp/src/lib.rs` — JSON-RPC framing and both protocol eras (`dispatch`, `handle_modern`), `Server`, `LazyServer`, the 13 tools, the Keep-local guards, input and result caps, unit tests.
- `crates/fly-mcp/src/context.rs` — series detection (`normalize_title`, `detect_series`, `series_matching`) and the `build_briefing` renderer.
- `crates/fly-mcp/tests/stdio.rs` — spawns the real binary: legacy handshake and the stateless 2026-07-28 dialect.
- `crates/fly-mcp/tests/stdio_lifecycle.rs` — the server must exit within 5 s of stdin EOF.
- `scripts/mcp-smoke.mjs` — manual smoke that calls every tool over stdio. Its default data dir is `<data_dir>`; the one write re-sets an existing label.

Packaging, the Settings snippet and CI staging are in [app-mcp-sidecar](app-mcp-sidecar.md).

## Surface area
### Public API
No other crate depends on fly-mcp (the app ships the binary; it does not link the crate).
- **Servers** `Server::new(Storage)`, `Server::handle_message(&str) -> Option<String>` (`lib.rs:48-61`). `LazyServer::new(PathBuf)`, `is_open()`, `handle_message()` (`lib.rs:197-248`). `None` means a notification (no reply).
- **Constants** `SERVER_NAME = "flyonthewall"`, `SERVER_VERSION` (crate version), `MODERN_PROTOCOL_VERSION = "2026-07-28"` (`lib.rs:28-31`), `DB_UNAVAILABLE_MESSAGE` (`lib.rs:188`), `KEEP_LOCAL_MEETING_MESSAGE`, `KEEP_LOCAL_NOTE_MESSAGE` (`lib.rs:904-908`), `MAX_SPEAKER_LABEL_CHARS = 100`, `MAX_QUERY_CHARS = 1_000`, `MAX_RESULT_BYTES = 1_000_000` (`lib.rs:915-923`).
- **Context** `context::{normalize_title, Series, detect_series, series_matching, build_briefing}` (`context.rs:17-295`).

**JSON-RPC methods** (`lib.rs:70-171`)

| Method | Era | Notes |
|---|---|---|
| `initialize` | legacy | Echoes the client's `protocolVersion` (default `2024-11-05`), returns `serverInfo` + `instructions` (`lib.rs:173-185`). |
| `ping`, `tools/list`, `tools/call` | both | Modern requests must carry `_meta["io.modelcontextprotocol/protocolVersion"] = "2026-07-28"` and `_meta[".../clientCapabilities"]`; modern results gain `resultType: "complete"` and `_meta[".../serverInfo"]` (`lib.rs:1041-1047`). Modern `tools/list` adds `ttlMs: 3600000`, `cacheScope: "public"`. |
| `server/discover` | modern only | Without the modern `_meta` it is `-32602` (`lib.rs:94-102`). |
| notifications (no `id`) | both | No reply (`lib.rs:81-82`). |

Errors: `-32700` parse, `-32601` unknown method, `-32602` bad params, `-32022` unsupported protocol version with `data.supported` / `data.requested` (`lib.rs:128-138`). A tool failure is a normal result with `isError: true`.

**Tools** (`tools_call` `lib.rs:251-277`, schemas `lib.rs:1049-1200`). `limit` is clamped to 1-100 (`lib.rs:1004-1009`); dates are RFC 3339 or `YYYY-MM-DD` (`lib.rs:1012-1028`); `query` is capped at 1,000 chars (`lib.rs:954-963`); every result is cut at 1 MB (`lib.rs:966-980`).

| Tool | Arguments | Reads | Keep-local handling |
|---|---|---|---|
| `get_context` | `query` (req), `since` | `list_meetings(None, 2000)`, series detection, then `search(query, 20)` as a full-text fallback; briefing reads transcripts (speaker labels), `get_meeting_items` counts and `query_items` (limit 500) (`lib.rs:761-781`, `context.rs:143-295`) | Hidden notes dropped before series detection (`lib.rs:745-759`), from fallback hits (`lib.rs:769-773`), and again in `build_briefing` (`context.rs:149-158`); items use `exclude_keep_local` |
| `whats_changed` | `series` (req), `since` (req) | Same briefing, restricted to meetings started at/after `since` (`lib.rs:783-798`) | Same as `get_context` |
| `open_items` | `scope` (`"all"` or a series name) | `query_items` (limit 200), then keeps action items and commitments whose status isn't `done` (`lib.rs:800-844`) | `exclude_keep_local: true` in SQL; series built without hidden meetings |
| `query_items` | `type`, `owner`, `status`, `since`, `limit` (default 50) | `query_items` (`lib.rs:710-741`) | `exclude_keep_local: true` |
| `get_meeting_items` | `meeting_id` | `get_meeting_items` (`lib.rs:687-708`) | `guard_meeting` refuses |
| `search_notes` | `query` (req), `folder_id`, `since`, `until`, `limit` (default 20), `cursor` (`"<note>:<transcript>"`) | `search_split` FTS over notes and transcripts (`lib.rs:279-363`) | `exclude_keep_local: true` inside SQL, so hidden rows never fill a page or shape a cursor |
| `list_folders` | none | `list_folders`, `keep_local_folder_ids` (`lib.rs:365-398`) | Local-only folders and descendants omitted; a visible folder never names a hidden parent |
| `get_note` | `note_id` | `get_note` → markdown, `updated`, `meeting_id`, attachment names and relative paths (`lib.rs:450-473`) | `guard_note` refuses |
| `get_transcript` | `meeting_id`, `with_segment_ids` | `get_transcript`, polished variant preferred (`get_cleaned_transcript`), `get_meeting` for header/attendees (`lib.rs:479-548`) | `guard_meeting` refuses |
| `get_transcripts` | `meeting_ids` (1-20), `with_segment_ids` | Same rendering per id; stops before the 1 MB cap and lists skipped ids (`lib.rs:550-593`) | Refused ids render the refusal text in place |
| `get_meeting` | `meeting_id` | `get_meeting`, transcript presence (`lib.rs:595-616`) | `guard_meeting` refuses |
| `list_recent` | `limit` (default 10), `folder_id`, `since`, `until`, `cursor` (`"<rfc3339>\|<id>"`) | `list_notes_filtered(…, exclude_keep_local = true)` (`lib.rs:618-658`) | Excluded inside SQL |
| `set_speaker_label` | `meeting_id`, `speaker_key`, `label` (all req) | `get_transcript`; then **writes** via `Storage::relabel_speaker` (`lib.rs:848-899`) | `guard_meeting` refuses; only existing speaker keys; label cleaned (control/bidi/zero-width chars → spaces, ≤100 chars) (`lib.rs:929-951`) |

`guard_meeting` (`lib.rs:426-448`) refuses when the meeting's own flag or its note's effective flag is set, and refuses an unknown id that still has orphaned transcript or item rows. All Keep-local lookups fail closed (`lib.rs:910-912`, `context.rs:152-154`).

### Tauri commands
None (crates expose no commands). The app's `mcp_config` command lives in [app-mcp-sidecar](app-mcp-sidecar.md).

### Events
None.

### Settings keys
None read or written.

### Database
Opened by `LazyServer::ensure_open` (`lib.rs:220-233`) with `Storage::open_existing` (`crates/fly-storage/src/lib.rs:231-256`): read-write connection flags, 5 s busy timeout, `foreign_keys=ON`, `secure_delete=ON`, persistent WAL, no migration, no column repair, no template seeding. The schema must be exactly `SCHEMA_VERSION` (2) with every required table and column (`crates/fly-storage/src/lib.rs:302-335`); otherwise each tool call returns `DB_UNAVAILABLE_MESSAGE` plus the reason, and the open is retried on the next message.

| Table | Columns | Read / write |
|---|---|---|
| `notes`, `notes_fts` | id, title, folder_id, meeting_id, scratchpad, blocks_json, attachments_json, updated_at | Read |
| `meetings` | id, title, note_id, attendees, started_at, ended_at, recording, keep_local | Read |
| `folders` | id, name, parent_id, keep_local | Read |
| `meeting_items` | all columns | Read |
| `transcripts` | meeting_id, json, cleaned_json | Read; **written** by `set_speaker_label` (both variants, `crates/fly-storage/src/transcripts.rs:151-175`) |
| `transcripts_fts` | meeting_id, body | Read; rewritten by `set_speaker_label` (`crates/fly-storage/src/transcripts.rs:268-291`) |
| `embedding_chunks` | the transcript's chunk rows | Re-synced by `set_speaker_label` (labels are in chunk text) |
| every table | — | Created, migrated and seeded only when **no database file exists yet**: `ensure_open` falls back to the full `Storage::open` (`lib.rs:227-230`) |

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
- **Outbound:** [fly-storage](fly-storage.md) (`Storage`, `ItemFilter`, `SearchFilter`, `StorageError`, the Keep-local CTE), [fly-core](fly-core.md) (`Meeting`, `MeetingItem`, `ItemKind`, `Attendee`, `Transcript`), serde_json, chrono, dirs.
- **Extraction is not here.** `meeting_items` rows are produced by the app ([app-llm](app-llm.md) `extract_meeting_items` / backfill); this server only reads them (DECISIONS.md 2026-07-13).

## Invariants
- **One write only, `set_speaker_label`.** DECISIONS.md "M6 — MCP server" 2026-07-02 (read-only by design) and 2026-07-13 (one write tool). Enforced only by the dispatcher's tool match (`lib.rs:254-269`); the connection is opened read-write (`crates/fly-storage/src/lib.rs:239-242`) because the write needs it, so there is no `query_only` backstop. The tool list is pinned by `initialize_and_list_tools` (`lib.rs:1270-1287`).
- **Never holds keys or calls a cloud LLM.** Spec §2 item 5; DECISIONS.md 2026-07-13 ("the server holds no API keys and makes no LLM calls"). Enforced structurally: `crates/fly-mcp/Cargo.toml:11-19` has no fly-secrets, fly-llm, reqwest or tokio, and neither does fly-storage / fly-core. No test or cargo-deny rule guards it.
- **Keep-local meetings and notes are never returned.** docs/SECURITY_AUDIT_2026-10.md Round 3 ("MCP server"); enforced at `lib.rs:400-448`, `lib.rs:300`, `lib.rs:631`, `lib.rs:726`, `lib.rs:752-756`, `lib.rs:825`, `context.rs:149-158`, `context.rs:244`. Lookup errors refuse the whole result. **Doc drift:** docs/THREAT_MODEL.md T10 still lists "The MCP server can still return keep-local meetings to its client" as residual risk, and Settings › Other AI apps says "It can read every note, including notes you keep local" (`frontend/src/components/SettingsModal.tsx:2209-2212`). Both predate or ignore commit `a5e1995` (2026-10-23) and over-warn.
- **The app owns the schema.** The server never migrates, repairs or seeds an existing database (`crates/fly-storage/src/lib.rs:221-256`; SECURITY_AUDIT Round 3). Exception: with no database file at all it runs the full `Storage::open` (`lib.rs:224-230`).
- **stdout is the protocol channel.** Logs go to stderr (`main.rs:24-29`, `lib.rs:210-211`).
- **The server dies with its client.** It exits cleanly on stdin EOF (`main.rs:33-45`) so it can't block an installer (DECISIONS.md 2026-07-13; `tests/stdio_lifecycle.rs`). The NSIS hook `taskkill`s it as a backstop (`src-tauri/installer-hooks.nsi`).
- **Stateless in both eras; legacy replies byte-identical.** DECISIONS.md 2026-08-09; `lib.rs:10-17`, `lib.rs:103-111`.
- **Deterministic context.** No LLM at query time; every briefing line cites `meeting:` and `segments:` (`context.rs:1-6`, `context.rs:106-128`).
- **Bounded output.** Query ≤1,000 chars, result ≤1 MB, `get_transcripts` ≤20 ids and whole transcripts only, speaker label ≤100 chars (`lib.rs:914-980`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-mcp/src/lib.rs` | 29 | Both protocol eras, tool list pin, every tool's happy path, write validation and sanitising, caps, cursor paging completeness, Keep-local refusals and filtering (direct, lists, search, items, briefings, folders, orphans), lazy open and recovery |
| unit | `crates/fly-mcp/src/context.rs` | 4 | Title normalization, series grouping by title + attendee overlap, all-token matching |
| integration (runs in CI) | `crates/fly-mcp/tests/stdio.rs` | 2 | The real binary over stdio, legacy and 2026-07-28 dialects |
| integration (runs in CI) | `crates/fly-mcp/tests/stdio_lifecycle.rs` | 1 | Exit within 5 s of stdin EOF |
| CI job | `.github/workflows/ci.yml` `sidecar` | — | Release-profile build answers `tools/list` (Linux) |
| manual | `scripts/mcp-smoke.mjs` | — | Every tool against a real data dir |

**Known gaps:**
- No test proves the read tools never write. The connection is read-write and read-only rests on code paths; a regression in a fly-storage "read" (a heal or backfill on read) would go unnoticed. Risk: silent mutation of user data by an external client.
- Schema-version refusal is not tested through the server. `lazy_server_serves_errors_until_the_database_opens` uses a garbage file; the version check itself is tested only in fly-storage. Risk: PR #92's v3 bump vs. a running v2.4 server.
- `set_speaker_label` side effects (mirror files, `transcripts_fts`, `embedding_chunks`, cleaned variant) are not asserted here. Only the rendered label is.
- The first-run `Storage::open` fallback (creates `<data_dir>` and a database from the MCP process) has no test, and it bypasses the app's legacy data-dir migration (see Changing it safely).
- Nothing guards spec §2 item 5 at build time (for example a `cargo tree -p fly-mcp` check for reqwest / fly-secrets).
- `get_context_falls_back_to_full_text_when_title_does_not_match` only checks for a `# Context:` header, not which meetings were routed.

## Changing it safely
- Run `cargo test -p fly-mcp`. The stdio tests spawn the debug binary via `CARGO_BIN_EXE_flyonthewall-mcp`, so they need no staging.
- **Adding a tool:** update `tools_call`, `tools_list`, the pinned name list in `initialize_and_list_tools`, the count `13` in `modern_requests_are_stateless_and_self_describing`, `INSTRUCTIONS`, and `scripts/mcp-smoke.mjs`. Give it a Keep-local guard or SQL filter and add it to the `KeepLocalFixture` tests (`lib.rs:1872-2107`).
- **Adding a write:** don't, without a new DECISIONS.md entry. If you must, mirror the app command's validation and Keep-local refusal and assert the no-write path.
- **Storage schema bumps** (PR #92): the server refuses mismatched schemas by design. Ship the app and binary together; a client holding an old server process needs a restart. The NSIS hook kills running servers on install.
- **Never write to stdout** outside a reply. Use `eprintln!`.
- **Never add** fly-secrets, fly-llm, reqwest or any network crate (spec §2 item 5).
- **Text output is a contract** for clients (`list_recent_text_is_pinned_and_omits_app_status`). Change wording deliberately.
- **First-run fallback trap:** `Storage::open` from the MCP creates `<data_dir>`. The app's one-time pre-rebrand migration only moves a legacy dir when the new one doesn't exist yet (`src-tauri/src/state.rs:118-139`), so a server started before the app's first post-rebrand launch would strand that data (inferred; narrow window).
- When this crate's Keep-local behavior changes, update docs/THREAT_MODEL.md T10 and the Settings "What it can see" copy together.
