# Connector client (`fly-mcp-client`)

> Layer: crate. New in the connectors beta (spec §4.3).
> In flight: the Ask harness wires `ExternalTools` per turn (PR 3.3). Until then, connector calls run only from Settings › Connectors › Try.

## Purpose
Lets FOTW's own Ask agent call tools on MCP servers the user connects: hosted servers (Notion, Linear, HubSpot, GitHub…), custom web servers and local programs. Built on the official `rmcp` SDK, pinned to exactly `3.8.0`, with only its client features compiled. FOTW holds private meeting data, reads input anyone in a meeting can shape, and with connectors gains an outbound channel, so every rule below is enforced here, not in the UI.

The MCP **server** FOTW ships to other AI apps is a different crate ([fly-mcp](fly-mcp.md)); neither depends on the other.

## Files it owns
- `crates/fly-mcp-client/src/config.rs` — `ServerConfig` (name, transport with headers or settings, auth: none / OAuth / API key), validation, and `hash()`, the value an approval is bound to. Secret values are never part of it.
- `src/registry.rs` — persisted state: servers (approval, on/off switch, last problem), tool permissions, permission plans, the call log, keychain key names.
- `src/pin.rs` — tool definitions pinned by hash, `Permission` (Always allow / Needs approval / Blocked), `reconcile` against a live listing, review of changed and new tools, diffs, grouping, model-facing tool names.
- `src/hub.rs` — `ConnectorHub` (one per app: connections, add/edit, permissions, secrets) and `ConnectorTurn` (one per Ask turn: the call-time rules).
- `src/fence.rs` — what a result (or error) looks like when the model sees it.
- `src/schema.rs` — a small JSON Schema validator (arguments and structured results).
- `src/netguard.rs` — https only, private addresses blocked (URL, DNS, redirects); the guarded OAuth HTTP client.
- `src/oauth.rs` — browser sign-in (PKCE, loopback redirect, issuer check), the authorization server pinned at sign-in, and token refresh.
- `src/secrets.rs` — keychain storage: access and refresh tokens apart, long values chunked, and the keychain's own index of connector keys.
- `src/session.rs` — rmcp connections over stdio and Streamable HTTP; program lookup; a local program's whole process tree (`ProcessTree`); refuses sampling, roots and elicitation.
- `src/directory.rs` — the built-in list of hosted connectors.
- `src/claude_import.rs` — reads (never writes) Claude Desktop's config and turns its servers into connector forms.
- `src/sanitize.rs` — removes characters that hide text from a reader.
- `src/fixture.rs`, `src/bin/fixture.rs` — a hostile MCP server, compiled only with the `test-support` feature.

## Surface area
### Public API
| Item | Used by |
|---|---|
| `ConnectorHub::live(store, secrets)` and its operations (`check_config`, `pin_program` (a bare program name looked up once and pinned to its full path, before the dialog), `add_approved`, `update_approved`, `set_enabled`, `remove` (returns keychain entries it couldn't delete), `sync_tools`, `plan_permissions`, `apply_tightening`, `apply_loosening`, `accept_change`, `secret_status`, `sign_in`, `sign_out`, `call_log`, `clear_call_log`, `close_all`) | [app-connectors](app-connectors.md) |
| `ConnectorHub::turn(TurnOptions, Arc<dyn CallApprover>) -> ConnectorTurn`; `ConnectorTurn` implements `fly_llm::external_tools::ExternalTools` | [app-connectors](app-connectors.md) (`turn_for_ask`, `test_connector_tool`) |
| `CallApprover` (shows the confirm card; must be cancel-safe), `CallProposal`, `Decision`, `NewSecrets`, `args_problem` | [app-connectors](app-connectors.md) |
| `ConnectorStore` (load/save registry and log) | [app-connectors](app-connectors.md) backs it with backend-only settings |
| `Registry::secret_keys()`, `Registry::ids_in_raw` (ids still readable in a registry that no longer parses), `registry::slot_keys_for_id`, `secrets::indexed_keys` | "Delete all my data" ([app-privacy](app-privacy.md)) |
| `directory::directory()`, `claude_import::{candidates, to_config}` | [app-connectors](app-connectors.md) |

`fly_llm::external_tools::ExternalTools` lives in [fly-llm](fly-llm.md) so the Ask harness doesn't depend on this crate.

### Tauri commands
None (see [app-connectors](app-connectors.md)).

### Events
None.

### Settings keys
None directly; the app persists the registry through `ConnectorStore`.

### Database
None.

### Files and directories written
None. `claude_import` only parses text the app read.

### Network egress
| Host | When | Gate |
|---|---|---|
| The https address of a web connector the user typed or picked | Only after the add dialog: the first tool listing, refresh tools, Try, Ask calls the user allowed or that run on Always allow. Nothing is sent before approval. A switched-off connector doesn't connect or sign in | A native-dialog approval bound to the config hash. https only; private, loopback, link-local and reserved addresses blocked; no redirects on the MCP transport |
| The connector's OAuth authorization server | Sign in (discovered, then pinned with the connector); token refresh uses the pinned server, never a new discovery | Same network rules on every request and redirect hop; endpoints checked before use |
| `127.0.0.1:<ephemeral>` (inbound) | Sign-in redirect | One-shot listener; a connection gets 5 s to send its request; rmcp checks `state`, the PKCE verifier and the reply's issuer (`iss`, RFC 9207) |

### External processes
- A local connector's program, only after the user approved it. A bare name like `npx` is looked up on PATH once, before the approval dialog (with PATHEXT on Windows, so `npx` finds `npx.cmd`; relative PATH entries are skipped), and the config stores that full path; the dialog shows it. Relative paths are refused, and a connection refuses a program that isn't a full path to a file. Started without a shell, with a cleared environment plus a short allowlist (PATH, home and temp folders, locale) and the connector's settings. No console window on Windows. The last 4 KB of its error output is kept in memory to explain a failed start (secrets are scrubbed before the UI sees it). Its whole process tree (a Job Object on Windows, a process group on Unix) is stopped when its connection drops: turned off, edited, removed, "Delete all my data", or the app exits.
- The system browser, through the app's opener, for sign-in.

### Secrets and keychain entries
Service `com.flyonthewall.app`, per connector id `c…`:
| Key | Meaning |
|---|---|
| `connector.<id>.access` (+ `.0`–`.7` chunks) | Access token and non-secret token metadata |
| `connector.<id>.refresh` (+ chunks) | Refresh token, stored on its own |
| `connector.<id>.client` (+ chunks) | OAuth client secret (from registration or entered by the user) |
| `connector.<id>.apikey` (+ chunks) | API key or token, sent as a bearer token |
| `connector.<id>.header.<name>` | A secret request header |
| `connector.<id>.env.<NAME>` | A secret setting of a local program |
| `connector.index` (+ `.0`–`.31` chunks) | The keychain's own list of connector key names by connector id, so "Delete all my data" finds them without the registry. An id leaves it once all its keys are deleted |

Values longer than 1,000 characters are split across numbered entries (Windows Credential Manager caps an entry at about 1,280). Values are never logged and never returned to the webview.

## Dependencies
- **Inbound:** `src-tauri` ([app-connectors](app-connectors.md), [app-privacy](app-privacy.md)).
- **Outbound:** [fly-llm](fly-llm.md) (`ToolSpec`, `ExternalTools`), [fly-secrets](fly-secrets.md), `rmcp =3.8.0` (client, async-rw, Streamable HTTP client over reqwest, auth), `reqwest`, `tokio`, `url`, `http`, `sha2`.

## Invariants
Spec §4.3's day-one rules, with the permission model the user chose (per tool: Always allow / Needs approval / Blocked):
- **Approval is bound to a hash of the server config.** `ServerEntry::is_approved` compares the stored hash with `config.hash()` on every use; a changed config offers nothing and can't be synced. Editing a connector re-approves it in a native dialog; an edit that points it somewhere new (another address, program line or settings, transport or kind of sign-in, `ServerConfig::moves_from`) deletes every stored secret and token and starts its tools over.
- **Tool definitions are pinned by hash.** Tools in the first listing after approval start on Needs approval. A changed definition is Blocked and kept as a pending change with a field diff; a tool that appears later is Blocked until reviewed; the model only ever sees reviewed definitions. Every call re-lists tools live first and refuses a tool that changed or vanished. Approving a change needs the exact hash the user reviewed and lands on Needs approval, never Always allow.
- **Labels group tools; they never grant.** A tool is read-only only when the server labels it read-only and not destructive; unlabelled tools are change tools. The label also decides whether a Needs-approval call needs the app's second, native confirm (change tools do). Labels never grant Always allow, and any label change is a definition change (Blocked until reviewed). A server could mislabel a change tool as read-only; its calls still need the card click with the exact arguments.
- **Loosening needs the user.** Raising a permission (Blocked → Needs approval → Always allow) is planned here and applied only after the app's native dialog; the applied plan must equal the one shown. Lowering applies at once.
- **Needs approval shows exact, editable arguments.** `ConnectorTurn::call` checks the model's arguments (object, no hidden characters, ≤ 16,000 chars, pinned schema), shows them on the confirm card (`CallApprover`), checks the edited arguments again, and sends exactly those. Always allow skips the card only; every other rule still applies.
- **A connector result can't trigger another proposal in the same turn.** The turn is sealed when a call is sent, or when a connection or listing error comes back; `sealed()` tells the harness, and every later proposal is refused without a card.
- **Results and errors are fenced, link-stripped, schema-validated and size-capped.** Non-text content becomes a placeholder; a tool with an output schema must return matching structured content or the result is discarded; hidden characters are removed; links become `[link: host]` (scheme links even mid-word, `//host/…` and `www.host…`); text is capped (3,500 chars by default) and wrapped in a `<connector_result>` fence that says it's data, with fence-like text defused. Errors after contact with the server go through the same fence; transport errors reach the model as fixed text.
- **Tool text the model sees is cleaned.** Descriptions and titles lose hidden characters and links, are capped, and are quoted as the service's own words; a schema with hidden or over-long text anywhere else is refused; annotation-only keys (`x-*`, `examples`, `$comment`) are dropped.
- **https only, private addresses blocked**, including during OAuth discovery ([`netguard`](../../crates/fly-mcp-client/src/netguard.rs)). Request headers the app sets itself (`Authorization`, `Host`, MCP session headers…) can't be configured.
- **No sampling, roots or elicitation.** No client capabilities are declared; such requests get "method not found"; multi-round `input_required` and task results are refused.
- **Keep-local content blocks all connectors.** A turn built with `keep_local_in_scope` offers nothing, and `mark_keep_local()` refuses every later call, Always allow included.
- **Tier S gets no tools; budgets cap proposals** (M: 3, C: 8 per turn), on top of the harness's own loop budget.
- **Nothing connects before approval.** There is no pre-approval check: the add dialog comes first, then the first listing. A switched-off connector neither syncs nor signs in.
- **The approval dialog shows the whole config.** Name, full address, the program's full path and every argument, and every non-secret setting or header with its value, never cut. A config that can't be shown that way is refused: hidden or control characters anywhere, an address over 512 characters, a program line over 1,000, a shown value over 200 (make it a secret instead), or a relative program path.
- **Local programs' settings can't change how they load or where they reach.** Neither plain nor secret settings may use a name the program inherits (`PATH`, `HOME`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `TEMP`, `TMP`…) or one that preloads code, moves config, points a runtime at another package registry, sets a proxy or loosens TLS trust (`LD_*`, `DYLD_*`, `NODE_*`, `NPM_*`, `PIP_*`, `UV_*`, `PYTHON*`, `GIT_*`, `XDG_*`, `ELECTRON_*`, `*_PROXY`, `JAVA_TOOL_OPTIONS`, `DOTNET_STARTUP_HOOKS`, `SSL_CERT_FILE`…).
- **After the card, the call is checked again.** A card can wait minutes: Keep local, the connector's state and the tool's current permission and kind are read again before anything is sent. `TurnOptions::force_approval` (Settings › Try) shows the card even on Always allow.
- **Logs record an error's kind, never its text** (a server's error can echo the arguments).
- **The call log holds allowlisted fields only** (time, connector, tool, kind, outcome, who allowed it, refusal code, duration, result size, truncation, schema check, whether arguments were edited) — never arguments or results.
- **Claude Desktop import reads, never writes**, skips FOTW's own server, and hands the webview names only; values go to the keychain.
- Unreadable stored state yields an empty registry (fail closed).

## Tests
`cargo test -p fly-mcp-client` (the crate's tests enable `test-support` through a self dev-dependency):
- Unit tests in every module: config hash and validation (headers, settings), network rules, pinning, review and diffs, permission plans, schema validation, fencing and link stripping, chunked secrets, the directory, Claude Desktop parsing.
- `tests/rules.rs` — the call-time rules against the hostile fixture over an in-memory pipe: nothing connects before approval or while switched off, Try always shows the card, a permission change or Keep local while the card waits, links in descriptions, Needs approval, Always allow, Blocked and switched-off connectors, label grouping, stale loosening plans, prompt injection hidden in descriptions, hidden characters and oversized arguments, results that try to close the fence or carry links, a 2 MB result, structured content that breaks its schema, server and connection errors (fenced and sealing), a tool swapped mid-session, a tool added after approval, sampling/roots/elicitation probes, Keep local, tiers and budgets, a voided approval, secrets kept out of the registry and removed with the connector, and a log with no content.
- `tests/stdio.rs` — the real fixture program over stdio: clean environment with settings, a live tool swap, a missing program, a bare program name that was never pinned, a program that stops at once (its last words captured), and a grandchild process that stops with the connector.
- `tests/oauth.rs` — a local OAuth fixture: https-only outside tests, a discovery redirect to a link-local address, private endpoints in metadata, a private authorization server, a redirect hop to a private address, and a full PKCE sign-in that stores access and refresh tokens apart.

## Changing it safely
- Bumping `rmcp` is a reviewed change: re-run all three test files, re-check `session.rs` (client capabilities, handler refusals, protocol version) and `netguard.rs`'s use of `OAuthHttpClient`.
- Anything new a server can send that reaches the model must go through `sanitize`, a cap and the fence.
- Never let a model-reachable path write the registry; loosening stays behind the app's native dialogs.
- `NetPolicy::local_test` exists only with `test-support`; the app never enables that feature.
