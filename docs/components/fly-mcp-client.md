# Connector client (`fly-mcp-client`)

> Layer: crate. New in the connectors beta (spec §4.3).
> In flight: the Ask harness wires `ExternalTools` per turn (PR 3.3). Until then, connector calls run only from Settings › Connectors › Try.

## Purpose
Lets FOTW's own Ask agent call tools on MCP servers the user connects (HubSpot, Notion, GitHub, a local program). Built on the official `rmcp` SDK, pinned to exactly `3.8.0`, with only its client features compiled. FOTW holds private meeting data, reads input anyone in a meeting can shape, and with connectors gains an outbound channel, so every rule below is enforced here, not in the UI.

The MCP **server** FOTW ships to other AI apps is a different crate ([fly-mcp](fly-mcp.md)); neither depends on the other.

## Files it owns
- `crates/fly-mcp-client/src/config.rs` — `ServerConfig` (name, transport, auth), validation, and `hash()`, the value an approval is bound to.
- `src/registry.rs` — persisted state: servers, approvals, tool pins, permission plans, the call log, keychain key names.
- `src/pin.rs` — tool definitions pinned by hash, `reconcile` against a live listing, diffs, the read/write rule, model-facing tool names.
- `src/hub.rs` — `ConnectorHub` (one per app: connections, settings operations) and `ConnectorTurn` (one per Ask turn: the call-time rules).
- `src/fence.rs` — what a result looks like when the model sees it.
- `src/schema.rs` — a small JSON Schema validator (arguments and structured results).
- `src/netguard.rs` — https only, private addresses blocked (URL, DNS, redirects); the guarded OAuth HTTP client.
- `src/oauth.rs` — browser sign-in (PKCE, loopback redirect) and token refresh.
- `src/secrets.rs` — keychain storage: access and refresh tokens apart, long values chunked.
- `src/session.rs` — rmcp connections over stdio and Streamable HTTP; refuses sampling, roots and elicitation.
- `src/sanitize.rs` — removes characters that hide text from a reader.
- `src/fixture.rs`, `src/bin/fixture.rs` — a hostile MCP server, compiled only with the `test-support` feature.

## Surface area
### Public API
| Item | Used by |
|---|---|
| `ConnectorHub::live(store, secrets)` and its settings operations (`check_config`, `add_approved`, `remove`, `sync_tools`, `plan_permissions`, `apply_permissions`, `accept_change`, `set_env_secret`, `sign_in`, `sign_out`, `call_log`, `clear_call_log`, `close_all`) | [app-connectors](app-connectors.md) |
| `ConnectorHub::turn(TurnOptions, Arc<dyn CallApprover>) -> ConnectorTurn`; `ConnectorTurn` implements `fly_llm::external_tools::ExternalTools` | [app-connectors](app-connectors.md) (`turn_for_ask`, `test_connector_tool`) |
| `CallApprover` (shows the confirm card; must be cancel-safe), `CallProposal`, `Decision` | [app-connectors](app-connectors.md) |
| `ConnectorStore` (load/save registry and log) | [app-connectors](app-connectors.md) backs it with backend-only settings |
| `Registry::secret_keys()` | "Delete all my data" ([app-privacy](app-privacy.md)) |

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
None.

### Network egress
| Host | When | Gate |
|---|---|---|
| The https URL of each web connector the user added | Refresh tools, Try, Ask calls the user clicked Run on | Native-dialog approval bound to the config hash; https only; private, loopback, link-local and reserved addresses blocked; no redirects on the MCP transport |
| The connector's OAuth authorization server (discovered) | Sign in, token refresh | Same network rules on every request and redirect hop; discovered endpoints checked before use |
| `127.0.0.1:<ephemeral>` (inbound) | Sign-in redirect | One-shot listener; rmcp checks `state` and the PKCE verifier |

### External processes
- A stdio connector's program, started without a shell, with a cleared environment plus a short allowlist (PATH, home and temp folders, locale) and the variables the user stored for it. stderr is discarded; no console window on Windows. Stopped when its connection drops or the app exits.
- The system browser, through the app's opener, for sign-in.

### Secrets and keychain entries
Service `com.flyonthewall.app`, per connector id `c…`:
| Key | Meaning |
|---|---|
| `connector.<id>.access` (+ `.0`–`.7` chunks) | Access token and non-secret token metadata |
| `connector.<id>.refresh` (+ chunks) | Refresh token, stored on its own |
| `connector.<id>.client` (+ chunks) | Client secret (from registration or entered by the user) |
| `connector.<id>.env.<NAME>` | A stdio connector's variable |

Values longer than 1,000 characters are split across numbered entries (Windows Credential Manager caps an entry at about 1,280). Values are never logged.

## Dependencies
- **Inbound:** `src-tauri` ([app-connectors](app-connectors.md), [app-privacy](app-privacy.md)).
- **Outbound:** [fly-llm](fly-llm.md) (`ToolSpec`, `ExternalTools`), [fly-secrets](fly-secrets.md), `rmcp =3.8.0` (client, async-rw, Streamable HTTP client over reqwest, auth), `reqwest`, `tokio`, `url`, `http`, `sha2`.

## Invariants
Spec §4.3's day-one rules, and where each is enforced:
- **Approval is bound to a hash of the server config.** `ServerEntry::is_approved` compares the stored hash with `config.hash()` on every use; a changed config offers nothing and can't be synced.
- **Each tool definition is pinned by hash.** New tools start off and are writes. A changed definition turns the tool off, clears its read-only mark and keeps the new version as a pending change with a field diff; the model keeps seeing only the pinned definition. Every call re-lists tools live first and refuses a tool that changed or vanished. Accepting a change needs the exact pending hash the user reviewed.
- **Annotations only make a tool stricter.** A tool is a read only when the user marked it read-only and the server doesn't say `readOnlyHint: false` or `destructiveHint: true`; a server can't mark anything read-only.
- **Exact, editable arguments on every call.** `ConnectorTurn::call` validates the model's arguments against the pinned schema, shows them on the confirm card (`CallApprover`), re-validates the edited arguments, and sends exactly those.
- **A connector result can't trigger another proposal in the same turn.** The turn is sealed when a call is sent; every later proposal is refused without a card.
- **Results are fenced, schema-validated and size-capped.** Non-text content becomes a placeholder; a tool with an output schema must return matching structured content or the result is discarded; hidden characters are removed; text is capped (3,500 chars by default) and wrapped in a `<connector_result>` fence that says it's data, with fence-like text defused. Transport and connection errors reach the model as fixed messages.
- **Tool text the model sees is cleaned.** Descriptions and titles lose hidden characters and are capped; a schema with hidden or over-long text anywhere else is refused; annotation-only keys (`x-*`, `examples`, `$comment`) are dropped.
- **https only, private addresses blocked**, including during OAuth discovery ([`netguard`](../../crates/fly-mcp-client/src/netguard.rs)).
- **No sampling, roots or elicitation.** No client capabilities are declared; such requests get "method not found"; multi-round `input_required` and task results are refused.
- **Keep-local content blocks all connectors.** A turn built with `keep_local_in_scope` offers nothing, and `mark_keep_local()` refuses every later call.
- **Tier S gets no tools; budgets cap proposals** (M: 3, C: 8 per turn), on top of the harness's own loop budget.
- **The call log holds allowlisted fields only** (time, connector, tool, kind, outcome, refusal code, duration, result size, truncation, schema check, whether arguments were edited) — never arguments or results.
- Unreadable stored state yields an empty registry (fail closed).

## Tests
`cargo test -p fly-mcp-client` (the crate's tests enable `test-support` through a self dev-dependency):
- Unit tests in every module (config hash, network rules, pinning and diffs, schema validation, fencing, chunked secrets).
- `tests/rules.rs` — each call-time rule against the hostile fixture over an in-memory pipe: prompt injection hidden in descriptions, results that try to close the fence, a 2 MB result, structured content that breaks its schema, a tool swapped mid-session, sampling/roots/elicitation probes, Keep local, tiers and budgets, a voided approval, a stale permission plan, removal clearing the keychain, and a log with no content.
- `tests/stdio.rs` — the real fixture program over stdio: clean environment, and a live tool swap caught.
- `tests/oauth.rs` — a local OAuth fixture: https-only outside tests, a discovery redirect to a link-local address, private endpoints in metadata, a private authorization server, a redirect hop to a private address, and a full PKCE sign-in that stores access and refresh tokens apart.

## Changing it safely
- Bumping `rmcp` is a reviewed change: re-run all three test files, re-check `session.rs` (client capabilities, handler refusals, protocol version) and `netguard.rs`'s use of `OAuthHttpClient`.
- Anything new a server can send that reaches the model must go through `sanitize`, a cap and the fence.
- Never let a model-reachable path write the registry; trust grants stay behind the app's native dialogs.
- `NetPolicy::local_test` exists only with `test-support`; the app never enables that feature.
