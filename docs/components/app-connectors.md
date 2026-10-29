# Connector commands (`app-connectors`)

> Layer: backend feature (src-tauri). New in the connectors beta (spec §4.3).
> In flight: the Ask harness (PR 3.3) calls `turn_for_ask` per Ask turn; until it lands, calls run only from Settings › Connectors › Try.

## Purpose
Wires [fly-mcp-client](fly-mcp-client.md) to the app: stores the connector registry and call log in backend-only settings, asks for every trust grant in a native dialog, carries confirm cards to the webview and the user's answers back, reads (never writes) Claude Desktop's config for import, and hands the Ask harness a per-turn tool set. Turning the beta on only shows the feature; it grants nothing.

## Files it owns
- `src-tauri/src/connectors.rs` — the 19 commands, `ConnectorsState` (the hub, pending proposals, one-dialog-at-a-time lock, dialog pacing), the settings-backed store, the native-dialog copy, the approver behind the confirm card, `turn_for_ask`, `stored_secret_keys`.

## Surface area
### Public API
| Item | Used by |
|---|---|
| `connectors::init(&AppHandle)` | `lib.rs` setup, right after `AppState` is managed |
| `connectors::shutdown(&AppHandle)` | `lib.rs` `RunEvent::Exit` (stops local connector programs) |
| `connectors::turn_for_ask(&AppHandle, turn_id, tier, keep_local_in_scope) -> Option<Arc<dyn ExternalTools>>` — `None` while the beta is off; tier `"S"` and a Keep-local scope get no tools | The Ask harness (PR 3.3) |
| `connectors::stored_secret_keys(&Storage, &dyn SecretStore) -> Vec<String>`: keys from the registry, from ids still readable in a registry that no longer parses, and from the keychain's connector index | `privacy::perform_delete_all` (read before the database is emptied) |
| `connectors::close_all(&AppHandle)` | `privacy::delete_all_data` (stops connector programs and connections first) |
| `REGISTRY_KEY`, `CALL_LOG_KEY`, `BETA_KEY` | tests, [app-privacy](app-privacy.md) |
| `llm_commands::ask_consent_natively` (`pub(crate)`) | every dialog here |

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `connectors_overview` | — | `Overview { beta, connectors }`: per connector its config without secret values, status, review count, sign-in state, which secrets are set (never values), and its tools (group, label, permission, status, pinned hash, review hash, diff) | `ui-connectors` via `api.connectorsOverview` |
| `connectors_directory` | — | The built-in list of hosted connectors | `ui-connectors` (Browse) via `api.connectorsDirectory` |
| `add_connector` | `input: { config, secrets }` | `Result<SaveResult { saved, connector, error }, String>`; a bare program name is pinned to its full path first; native dialog ("Add … connector?" / "Run a program on this computer?") showing the whole config; only then any contact: it lists tools unless a browser sign-in is needed | `ui-connectors` via `api.addConnector` |
| `update_connector` | `id, input` | `Result<SaveResult, String>`; program pinned as for add; native dialog; the approval moves to the new config's hash; empty secret values keep what's stored | `ui-connectors` (⋯ › Edit) via `api.updateConnector` |
| `set_connector_enabled` | `id, enabled` | `Result<SaveResult, String>`; off at once, on after a native dialog | `ui-connectors` (row switch) via `api.setConnectorEnabled` |
| `remove_connector` | `id` | `Result<(), String>`; also closes its connection and deletes its keychain entries (any it can't delete stay in the keychain index for "Delete all my data") | `ui-connectors` (⋯ › Disconnect) via `api.removeConnector` |
| `sync_connector_tools` | `id` | `Result<ConnectorView, String>`; changed tools are blocked with a diff, new tools blocked for review | `ui-connectors` via `api.syncConnectorTools` |
| `set_connector_tool_permissions` | `id, changes: { tool, permission }[]` | `Result<SaveResult, String>`; tightening applies at once; loosening after a native dialog (`saved: false` when declined); the applied loosening must equal what the dialog showed | `ui-connectors` (Tools tab) via `api.setConnectorToolPermissions` |
| `approve_connector_tool` | `id, tool, reviewHash` | `Result<SaveResult, String>`; native dialog; refused if `reviewHash` isn't the one reviewed; the tool goes to Needs approval | `ui-connectors` (review) via `api.approveConnectorTool` |
| `sign_in_connector` | `id` | `Result<SaveResult, String>`; opens the system browser, waits up to 5 minutes, then lists tools | `ui-connectors` via `api.signInConnector` |
| `sign_out_connector` | `id` | `Result<ConnectorView, String>`; deletes the tokens | `ui-connectors` via `api.signOutConnector` |
| `claude_desktop_candidates` | — | `Result<{ found, candidates }, String>`: servers in Claude Desktop's config, names only | `ui-connectors` (Import) via `api.claudeDesktopCandidates` |
| `import_claude_desktop_connector` | `key` | `Result<SaveResult, String>`; native dialog per server; its settings move to the keychain; permissions aren't imported | `ui-connectors` (Import) via `api.importClaudeDesktopConnector` |
| `pick_connector_program` | — | `Result<Option<String>, String>`: a native file picker for "Choose a file…" | `ui-connectors` (Add local) via `api.pickConnectorProgram` |
| `connector_call_log` | — | `CallLogEntry[]` (allowlisted fields, oldest first) | `ui-connectors` (Activity) via `api.connectorCallLog` |
| `clear_connector_call_log` | — | `Result<bool, String>`: native dialog first; `false` when the user kept it | `ui-connectors` (Activity) via `api.clearConnectorCallLog` |
| `pending_connector_calls` | — | `CallProposal[]` waiting for an answer | `ui-connectors` (`ConnectorCallCard.tsx`) via `api.pendingConnectorCalls` |
| `decide_connector_call` | `proposalId, action: "deny" \| "allow_once" \| "always_allow", args?` | `Result<DecideResult { ok, error }, String>`; edited arguments are checked (object, no hidden characters, size, pinned schema; `ok: false` keeps the card open); a change tool's call needs the native confirm with the final arguments (refused, not cut, when too long to show); `always_allow` is for read-only tools only and needs the native dialog | `ui-connectors` (`ConnectorCallCard.tsx`) via `api.decideConnectorCall` |
| `test_connector_tool` | `id, tool, args` | `Result<TestResult { ok, text }, String>`; runs one tool through a full `ConnectorTurn` with the same rules, and always shows the card (Always allow too: these arguments come from the webview); a change tool also needs the native confirm | `ui-connectors` (Try) via `api.testConnectorTool` |

Errors are strings. Program output (a local connector that stopped at once) comes after a `\n---\n` line, with every secret value of that connector replaced by dots.

### Events
| Event | Payload | When |
|---|---|---|
| `connectors:proposal` | `CallProposal` (server, destination, tool, kind, destructive, exact args, pinned input schema) | A Needs-approval call waits for the user |
| `connectors:settled` | `{ proposalId, outcome: "ran" \| "declined" \| "withdrawn" \| "timed_out" }` | The card should close (answered, the turn was dropped, or 10 minutes passed) |
| `connectors:changed` | `{ id }` | Stored state changed outside a Settings command (a tool blocked mid-turn because it changed) |

### Native dialogs
Every dialog goes through `ask_consent_natively` (rfd), one at a time, with the safe answer as the default button. Server-provided text is cleaned and capped; the configuration in add, edit and turn-on dialogs is shown in full (configs that can't be are refused). Each kind of dialog is paced like cloud consent: after two declines in a row it stays quiet for a minute, and the command says so instead of asking. Call arguments show pretty-printed up to 40 lines, compact beyond that, and are refused (never cut) past 3,000 characters.
| Trust grant | Title | Buttons (default first) |
|---|---|---|
| Add web connector | Add "<name>" connector? | Cancel / Add connector |
| Add local connector (and import) | Run a program on this computer? | Cancel / Run and add |
| Edit | Save changes to "<name>"? | Cancel / Save changes |
| Turn a connector back on | Turn on "<name>"? | Cancel / Turn on |
| Always allow (one tool) | Let the AI use "<tool>" without asking? | Keep asking / Always allow |
| Blocked → Needs approval (one tool) | Let the AI ask to use "<tool>"? | Keep blocked / Allow with approval |
| Several tools at once | Change permissions for N tools in <name>? | Keep as is / Change permissions |
| Changed or new tool | Approve changes to "<tool>"? | Keep blocked / Approve |
| Call to a change tool | Send this to "<name>"? | Don't send / Send |
| Clear the activity log | Clear connector activity? | Keep it / Clear activity |

### Settings keys
| Key | Read / write | Webview-settable? | Meaning |
|---|---|---|---|
| `connectors.registry` | R/W (this module only) | No: `connectors.` is outside the read allowlist and in `BACKEND_ONLY` (`commands.rs`; test `webview_cannot_touch_connector_state`) | Servers, approvals (config hash), on/off, tool permissions |
| `connectors.call_log` | R/W (this module only) | No | Last 200 calls, allowlisted fields |
| `ui.beta.connectors` | R | Yes (UI preference) | Shows the feature and lets `turn_for_ask` offer tools; grants nothing by itself |

### Database
| Table | Columns | Read / write |
|---|---|---|
| `settings` | `key`, `value` | R/W (the keys above) |

### Files and directories written
None. `claude_desktop_candidates` and `import_claude_desktop_connector` read Claude Desktop's `claude_desktop_config.json` (the standard config folder, plus the Windows Store package location), at most 1 MB, and never write it.

### Network egress
Through [fly-mcp-client](fly-mcp-client.md): the https address of each web connector and its OAuth server, only after approval. Local connectors run on this computer. Inbound `127.0.0.1:<ephemeral>` during sign-in.

### External processes
- Local connector programs (see [fly-mcp-client](fly-mcp-client.md)); never started before approval; their whole process tree stops on disconnect, turn-off, removal, "Delete all my data" and exit.
- The system browser, through `tauri-plugin-opener`, for sign-in.
- Native file picker (rfd) for "Choose a file…".

### Secrets and keychain entries
`connector.<id>.*` entries (see [fly-mcp-client](fly-mcp-client.md)). Secret values arrive from forms (or an import) and go straight to the keychain; views report only whether each is set. Removing a connector deletes its entries; "Delete all my data" deletes every connector's entries ([app-privacy](app-privacy.md)).

## Dependencies
- **Inbound:** `lib.rs` (registration, `init`, `shutdown`), `privacy.rs` (`stored_secret_keys`), `commands.rs` (`BACKEND_ONLY`), the Ask harness (`turn_for_ask`).
- **Outbound:** [fly-mcp-client](fly-mcp-client.md), [fly-llm](fly-llm.md) (`ExternalTools`), [fly-storage](fly-storage.md) (settings), [fly-secrets](fly-secrets.md) via `AppState.secrets`, `llm_commands::ask_consent_natively` (rfd), rfd file dialog, `tauri-plugin-opener`, `dirs`.

## Invariants
- **Trust is granted only behind a native dialog**: adding, editing or turning on a connector, loosening a permission, approving a changed or new tool, sending a call to a change tool, Always allow from a card (read-only tools only), and clearing the activity log. The webview can't read or write `connectors.*` settings. Taking trust away needs no dialog.
- **Nothing reaches a connector before its add dialog.** There's no pre-approval check, so these commands can't be used as a silent outbound channel.
- **After a card is answered, `decide_connector_call` reads the tool's current kind**: a tool that is a change tool now (or was when proposed) gets the native confirm.
- **Approval is bound to the exact configuration** (its hash, checked on every use by fly-mcp-client).
- **Needs approval shows a confirm card** with the exact arguments; edited arguments are re-checked here and again in the turn. A change tool's call also needs the native confirm, which shows the final arguments in full or refuses.
- **A withdrawn card resolves as declined** (the approver's drop guard); an unanswered card times out after 10 minutes; a card is answered once.
- **Secret values never reach the webview**, and program output shown to it is scrubbed of them.
- **Delete all my data** closes every connector connection first, lists connector keychain keys (registry, a damaged registry's ids, the keychain index) before the database is emptied, then deletes them; failures are reported.
- **rmcp logs at `warn`** by default (`logging.rs`), since it logs an OAuth authorization code at debug.

## Tests
- `connectors::tests` — backend-only keys, `stored_secret_keys` (registry, damaged registry, keychain index), the dialog copy (full address, headers, program path) and safe defaults, long arguments shown compact and too-long ones refused, scrubbed program output.
- `commands::security_tests::webview_cannot_touch_connector_state`.
- `privacy::tests::delete_all_empties_db_content_and_secrets` — connector tokens, chunks and settings are gone after delete-all.
- The call-time rules themselves are tested in [fly-mcp-client](fly-mcp-client.md).
- Gaps: native dialogs, the file picker and browser sign-in need a real-app check (see the PR).

## Changing it safely
- New connector state the UI needs goes through a command here, never a webview-writable setting.
- Any new trust grant gets a native dialog, built from cleaned, capped text, with the safe answer as the default.
- Mirror every command in `frontend/src/devMock.ts` and list it here.
