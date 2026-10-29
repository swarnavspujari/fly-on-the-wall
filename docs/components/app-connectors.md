# Connector commands (`app-connectors`)

> Layer: backend feature (src-tauri). New in the connectors beta (spec §4.3).
> In flight: the Ask harness (PR 3.3) calls `turn_for_ask` per Ask turn; until it lands, calls run only from Settings › Connectors › Try.

## Purpose
Wires [fly-mcp-client](fly-mcp-client.md) to the app: stores the connector registry and call log in backend-only settings, asks for every trust grant in a native dialog, carries confirm cards to the webview and the user's answers back, and hands the Ask harness a per-turn tool set. Turning the beta on only shows the feature; it grants nothing.

## Files it owns
- `src-tauri/src/connectors.rs` — the 14 commands, `ConnectorsState` (the hub, pending proposals, one-dialog-at-a-time lock), the settings-backed store, the approver behind the confirm card, `turn_for_ask`, `stored_secret_keys`.

## Surface area
### Public API
| Item | Used by |
|---|---|
| `connectors::init(&AppHandle)` | `lib.rs` setup, right after `AppState` is managed |
| `connectors::shutdown(&AppHandle)` | `lib.rs` `RunEvent::Exit` (stops stdio connector programs) |
| `connectors::turn_for_ask(&AppHandle, turn_id, tier, keep_local_in_scope) -> Option<Arc<dyn ExternalTools>>` — `None` while the beta is off; tier `"S"` and a Keep-local scope get no tools | The Ask harness (PR 3.3) |
| `connectors::stored_secret_keys(&Storage) -> Vec<String>` | `privacy::perform_delete_all` (read before the database is emptied) |
| `REGISTRY_KEY`, `CALL_LOG_KEY`, `BETA_KEY` | tests, [app-privacy](app-privacy.md) |
| `llm_commands::ask_consent_natively` (now `pub(crate)`) | the trust-grant dialogs here |

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `connectors_overview` | — | `Overview { beta, connectors: ConnectorView[] }` (tools with pinned-hash prefix, status, diff, kind; env names with set/not set; never a secret) | `ui-connectors` via `api.connectorsOverview` |
| `add_connector` | `config: { name, transport, auth, clientSecret? }` | `Result<SaveResult { saved, connector }, String>`; `saved: false` when the native dialog was declined | `ui-connectors` via `api.addConnector` |
| `remove_connector` | `id` | `Result<(), String>`; also deletes the connector's keychain entries and closes its connection | `ui-connectors` via `api.removeConnector` |
| `sync_connector_tools` | `id` | `Result<ConnectorView, String>`; new tools arrive off, changed ones are turned off with a diff | `ui-connectors` via `api.syncConnectorTools` |
| `save_connector_tools` | `id, changes: { tool, enabled, readOnly }[]` | `Result<SaveResult, String>`; native dialog when the change turns tools on or marks them read-only; the applied change is re-planned and must match what the dialog showed | `ui-connectors` via `api.saveConnectorTools` |
| `accept_connector_tool_change` | `id, tool, pendingHash` | `Result<SaveResult, String>`; native dialog; refused if the pending hash isn't the one reviewed | `ui-connectors` via `api.acceptConnectorToolChange` |
| `sign_in_connector` | `id` | `Result<ConnectorView, String>`; opens the system browser and waits up to 5 minutes | `ui-connectors` via `api.signInConnector` |
| `sign_out_connector` | `id` | `Result<ConnectorView, String>`; deletes the tokens | `ui-connectors` via `api.signOutConnector` |
| `set_connector_env` | `id, name, value` | `Result<ConnectorView, String>`; only names declared in the approved config; empty value clears | `ui-connectors` via `api.setConnectorEnv` |
| `connector_call_log` | — | `CallLogEntry[]` (allowlisted fields, oldest first) | `ui-connectors` via `api.connectorCallLog` |
| `clear_connector_call_log` | — | `Result<(), String>` | `ui-connectors` via `api.clearConnectorCallLog` |
| `pending_connector_calls` | — | `CallProposal[]` waiting for an answer | `ui-connectors` (`ConnectorCallCard.tsx`) via `api.pendingConnectorCalls` |
| `decide_connector_call` | `proposalId, run, args?` | `Result<DecideResult { ok, error }, String>`; edited arguments are validated against the pinned schema (`ok: false` keeps the card open); a call that may change data also needs a yes in a native dialog showing the final arguments | `ui-connectors` (`ConnectorCallCard.tsx`) via `api.decideConnectorCall` |
| `test_connector_tool` | `id, tool, args` | `Result<TestResult { ok, text }, String>`; runs one enabled tool through a full `ConnectorTurn` (same rules and confirm card) | `ui-connectors` via `api.testConnectorTool` |

### Events
| Event | Payload | When |
|---|---|---|
| `connectors:proposal` | `CallProposal` (server, destination, tool, kind, destructive, exact args, pinned input schema) | A call waits for the user |
| `connectors:settled` | `{ proposalId, outcome: "ran" \| "declined" \| "withdrawn" \| "timed_out" }` | The card should close (answered, the turn was dropped, or 10 minutes passed) |
| `connectors:changed` | `{ id }` | Stored state changed outside a Settings command (a tool turned off mid-turn because it changed) |

### Settings keys
| Key | Read / write | Webview-settable? | Meaning |
|---|---|---|---|
| `connectors.registry` | R/W (this module only) | No (not `consent.`/`capture.`/`ui.`; test `connector_state_is_backend_only`) | Servers, approvals (config hash), tool pins |
| `connectors.call_log` | R/W (this module only) | No | Last 200 calls, allowlisted fields |
| `ui.beta.connectors` | R | Yes (UI preference) | Shows the feature and lets `turn_for_ask` offer tools; grants nothing by itself |

### Database
| Table | Columns | Read / write |
|---|---|---|
| `settings` | `key`, `value` | R/W (the keys above) |

### Files and directories written
None.

### Network egress
Through [fly-mcp-client](fly-mcp-client.md): the https URL of each web connector and its OAuth server, only after the user's approval and, for calls, a click on Run. Stdio connectors run locally. Inbound `127.0.0.1:<ephemeral>` during sign-in.

### External processes
- Stdio connector programs (see [fly-mcp-client](fly-mcp-client.md)); stopped on exit.
- The system browser, through `tauri-plugin-opener`, for sign-in.

### Secrets and keychain entries
`connector.<id>.*` entries (see [fly-mcp-client](fly-mcp-client.md)). Removing a connector deletes its entries; "Delete all my data" deletes every connector's entries ([app-privacy](app-privacy.md)).

## Dependencies
- **Inbound:** `lib.rs` (registration, `init`, `shutdown`), `privacy.rs` (`stored_secret_keys`), the Ask harness (`turn_for_ask`).
- **Outbound:** [fly-mcp-client](fly-mcp-client.md), [fly-llm](fly-llm.md) (`ExternalTools`), [fly-storage](fly-storage.md) (settings), [fly-secrets](fly-secrets.md) via `AppState.secrets`, `llm_commands::ask_consent_natively` (rfd), `tauri-plugin-opener`.

## Invariants
- **Trust is granted only behind a native dialog.** Adding a server, turning tools on, marking them read-only and accepting a changed definition each need a click on the dialog's allow button; the default button declines. The webview can't write `connectors.*` settings. Taking trust away needs no dialog.
- **Approval is bound to the exact configuration** (its hash, checked on every use by fly-mcp-client).
- **Every call shows a confirm card** with the exact arguments; edited arguments are re-validated here and again in the turn. Calls to tools that may change data also need the native dialog with the final arguments.
- **A withdrawn card resolves as declined** (the approver's drop guard), and an unanswered card times out after 10 minutes.
- **Dialog text from servers is cleaned and capped** before it reaches a native dialog (`clean_capped`).
- **Delete all my data** lists connector keychain keys from the registry before the database is emptied, then deletes them.
- **rmcp logs at `warn`** by default (`logging.rs`), since it logs an OAuth authorization code at debug.

## Tests
- `connectors::tests` — backend-only keys, `stored_secret_keys`, cleaned dialog text with the safe default button.
- `privacy::tests::delete_all_empties_db_content_and_secrets` — connector tokens, chunks and stdio variables are gone after delete-all.
- The call-time rules themselves are tested in [fly-mcp-client](fly-mcp-client.md).
- Gaps: the native dialogs and the browser sign-in need a real-app check (see the PR).

## Changing it safely
- New connector state the UI needs goes through a command here, never a webview-writable setting.
- Any new trust grant gets a native dialog, built from cleaned, capped text.
- Mirror every command in `frontend/src/devMock.ts` and list it here.
