# Connectors (`ui-connectors`)

> Layer: frontend feature. New with connectors (beta); not on main yet. References are by symbol name.
> In flight: the backend side (`src-tauri/src/connectors.rs`, `crates/fly-mcp-client`) lands with it. Ask does not yet mount a per-turn card stack; the app-level stack shows every proposal.

## Purpose
Connectors let Ask call tools on MCP servers the user adds: web servers (HTTPS, optionally with OAuth sign-in) and local programs (stdio). This component is the UI for that feature. It has two parts:

- **Settings › Connectors (beta)** (`ConnectorsSection`): the beta switch; one card per connector with its destination, approval, sign-in and local-program variables; the tool list with per-tool On and Read-only toggles, pinned definition hashes, review of changed tool definitions, and a "Try a tool" tester; the add form; and the call log.
- **The confirm card** (`ConnectorCallCard` / `ConnectorCallStack`): every call that Ask, or the Try tester, proposes waits on this card. The card shows the exact arguments, which you can edit, until you choose Run or Don't run.

The safety rules live in the backend. The UI asks the backend for every change and displays what it returns. Trust grants are decided in native dialogs that the backend shows, not in the webview.

## Files it owns
- `frontend/src/connectorsModel.ts`: pure logic. The add form maps to `ConnectorConfigInput` with validation messages. The per-tool draft maps to the `save_connector_tools` payload, and `grantsTrust` says whether that save opens a native dialog. Also args-JSON editing, the Try tester's args template, proposal merging, call-log labels and relative time. Also holds `CONNECTORS_BETA_KEY`.
- `frontend/src/components/ConnectorsSection.tsx`: the Settings section body (default export), with `ConnectorCard`, `EnvRow`, `ToolRow`, `TryTool`, `AddConnector` and `CallLog` inside it.
- `frontend/src/components/ConnectorCallCard.tsx`: `ConnectorCallCard`, `ConnectorCallStack` and `useConnectorProposals`.
- Shared with the app shell: the command wrappers and the three event-name constants in `frontend/src/api.ts`, the types in `frontend/src/types.ts`, and the devMock connector fixtures in `frontend/src/devMock.ts`. The `"connectors"` section entry is in `frontend/src/settingsModel.ts` (owned by [ui-settings](ui-settings.md)). `SettingsModal.tsx` renders the section after "Other AI apps". `App.tsx` mounts one `<ConnectorCallStack />` after `AppNotices`.

## Surface area
### Public API
| Export | File | What it does |
|---|---|---|
| `ConnectorsSection` (default) | `ConnectorsSection.tsx` | No props. Reads `connectors_overview` on mount and again on every `connectors:changed`. When the overview can't be read, it falls back to `get_app_setting` for the switch. With the beta off it shows only the intro and the switch. |
| `useConnectorProposals(turnId?)` | `ConnectorCallCard.tsx` | Subscribes to `connectors:proposal` and `connectors:settled` first, then reads `pending_connector_calls`, so a proposal emitted between the two isn't lost. Returns `{ proposals, markSettled }`. Settled ids are remembered, so a pending list read before a settle can't bring a card back. `turnId` filters to one Ask turn. |
| `ConnectorCallCard` | `ConnectorCallCard.tsx` | One proposal. Props `proposal`, `onSettled(proposalId)`. |
| `ConnectorCallStack` | `ConnectorCallCard.tsx` | Fixed bottom-right (z-index 58: above Settings at 55 and notices at 50, below in-app dialogs at 60). Shows one card at a time, oldest first, plus "N more calls waiting", so a new proposal never takes focus from the card you're deciding. Optional `turnId`. |
| `CONNECTORS_BETA_KEY`, `SYSTEM_DIALOG_NOTE`, `validateAddDraft`, `splitArgs`, `splitList`, `toolPermissionChanges`, `grantsTrust`, `effectiveKind`, `kindLabel`, `toolStatusLabel`, `prettyArgs`, `parseArgs`, `sameArgs`, `argsTemplate`, `mergeProposals`, `outcomeLabel`, `refusalLabel`, `formatDuration`, `formatChars`, `newestFirst`, `relativeTime` | `connectorsModel.ts` | Pure helpers, unit-tested. |

Behaviour details:
- **Add form.** Web addresses must be `https://`, have a host and carry no user name or password. Names are 1–40 characters. Program arguments split on whitespace; "double quotes" keep spaces together, with no escapes, so Windows paths stay as typed. Environment variable names must match `[A-Za-z_][A-Za-z0-9_]*`. Scopes split on spaces and commas. A client secret needs a client ID. Local programs never sign in. Validation messages show before anything is sent. After a successful `add_connector`, the section runs `sync_connector_tools` for the new connector; its tools arrive off.
- **Tool toggles** are a local draft (a `Map` keyed by tool name, because names come from the server). Save sends only the changed tools. When the draft turns a tool on or marks one read-only, the save bar says "You'll confirm in a system dialog." The Read-only checkbox is disabled when `canMarkReadOnly` is false; it shows a tooltip and the line "The server says this tool can change data…". A tool that isn't `ok` can be turned off but not on. The kind badge reflects the draft: "Read" only when marked read-only and allowed.
- **Changed tools** show "Changed — review". Review change lists each changed field as before/after `<pre>` blocks. Accept change calls `accept_connector_tool_change` with the full `pendingHash`; accepting doesn't turn the tool on. Keep it off closes the review, and if the tool is on, turns it off (no dialog, since that grants nothing).
- **Try a tool** (only for tools that are on and `ok`) pre-fills the arguments with the schema's required properties and an empty value of each type; server defaults are ignored. Run calls `test_connector_tool`. That call goes through the same confirm card, and the result text shows in a `<pre>`.
- **Sign-in** shows "Waiting for you to sign in in your browser…" while `sign_in_connector` is pending (the backend waits up to 5 minutes).
- **Variables** (local programs) use a password field. The UI only ever shows Set or Not set. Clear sends an empty value.
- **Remove** asks in-app first (`useConfirm`, danger tone). The question says the connector's keychain entries are deleted too.
- **Call log** is collapsed behind Show. It lists entries newest first: time, connector, tool (+ "edited"), kind, outcome (with the refusal reason), duration, and result size ("cut" when truncated). It re-reads on `connectors:settled` while open. Clear log asks in-app first. The log holds no arguments or results.
- **Confirm card**: role `alertdialog`, labelled by its heading ("<server> wants to run <title or tool>") and described by "Sends to <destination>". When a title is shown, the real tool name is shown under it. The card has a kind badge, a Destructive warning row, and a monospace arguments textarea that shows parse errors inline. Run is disabled while the JSON is invalid. Arguments that are unchanged, or equal after parsing, are sent as `args: null`; edited ones as the parsed object. Focus starts on Don't run. Esc means Don't run, through an Esc layer at depth 501, so it answers the card before Settings or a sheet (the in-app confirm, at 1000, still wins). Both buttons are disabled while a decision is in flight, and Esc is ignored then. A write call adds "You'll confirm once more in a system dialog." `ok: false` keeps the card open with the backend's error. Don't run always closes the card, even if the command fails, because declining is the safe direction.

### Tauri commands
| Command | Wrapper (`api.ts`) | Called from | Native dialog (backend) |
|---|---|---|---|
| `connectors_overview` | `connectorsOverview` | section mount, `connectors:changed`, after the switch, add, remove | — |
| `add_connector` | `addConnector` | add form | Yes; `{ saved: false }` on decline |
| `remove_connector` | `removeConnector` | Remove (after `useConfirm`) | — |
| `sync_connector_tools` | `syncConnectorTools` | Refresh tools; right after an add | — |
| `save_connector_tools` | `saveConnectorTools` | tools Save; Keep it off | When the change grants trust |
| `accept_connector_tool_change` | `acceptConnectorToolChange` | Accept change | Yes |
| `sign_in_connector` / `sign_out_connector` | `signInConnector` / `signOutConnector` | sign-in row | — (opens the system browser) |
| `set_connector_env` | `setConnectorEnv` | variable rows | — |
| `connector_call_log` / `clear_connector_call_log` | `connectorCallLog` / `clearConnectorCallLog` | call log | — |
| `pending_connector_calls` | `pendingConnectorCalls` | `useConnectorProposals` on mount | — |
| `decide_connector_call` | `decideConnectorCall` | confirm card | For a write call, after Run |
| `test_connector_tool` | `testConnectorTool` | Try a tool | as for the call it proposes |
| `get_app_setting` / `set_app_setting` | `getAppSetting` / `setAppSetting` | the beta switch only | — |

devMock mirrors every connector command in memory, with a web "Acme CRM" (OAuth; tools covering read, write, destructive, changed-with-diff, unusable and no-longer-offered) and a local-program "Local docs" with one variable. Its stand-in for each native dialog is the browser's `confirm`, through one `mockNativeDialog` helper behind an `eslint-disable`, the same pattern as the consent stand-in. `test_connector_tool` emits `connectors:proposal` through `fotwMockEmit` and resolves when `decide_connector_call` settles it; it then logs the call and emits `connectors:settled`. Mock `set_app_setting` refuses `connectors.*` keys. Tests reset the fixtures with `resetMockConnectors()`.

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `connectors:proposal` (`CONNECTORS_PROPOSAL_EVENT`) | Listens: `useConnectorProposals` | `ConnectorCallProposal` | Backend approver, when a call is proposed |
| `connectors:settled` (`CONNECTORS_SETTLED_EVENT`) | Listens: `useConnectorProposals` (drops the card), `CallLog` (re-reads while open) | `ConnectorSettled` (`ran`, `declined`, `withdrawn`, `timed_out`) | Backend, on a decision, a withdrawal or a timeout |
| `connectors:changed` (`CONNECTORS_CHANGED_EVENT`) | Listens: `ConnectorsSection` (re-reads the overview) | `{ id }` | Backend, when a connector's state changes |

### Settings keys
| Key | Read / write | Meaning |
|---|---|---|
| `ui.beta.connectors` | read (via `connectors_overview.beta`, or `get_app_setting` as a fallback) and write (`"1"` / `"0"`) | Let Ask use connectors. The only setting this component writes. |
| `connectors.*` | never | Backend-owned connector state and trust. `set_app_setting` refuses these keys. |

localStorage keys: none.

### Database
None directly. The call log and connector registry are backend state.

### Files and directories written
None.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None from the webview | The backend connects to servers and runs programs. | Every call needs Run on the card; trust grants need the native dialog. |

### External processes
None from the webview. Local-program connectors are started by the backend.

### Secrets and keychain entries
None read. A client secret (add form) and variable values (variable rows) are typed into password fields and sent once to the backend, which keeps them in the OS keychain. They are never returned or shown again.

## Dependencies
- **Inbound:** `SettingsModal` (renders `ConnectorsSection` in the `connectors` section), `App` (mounts `ConnectorCallStack`).
- **Outbound:** `ui.tsx` (`SettingsGroup`, `SettingsRow`, `Switch`, `Segmented`, `Checkbox`, `Input`, `Badge`, `Button`, `useEscapeLayer`, `EscapeLayerScope`, `useDialogFocus`), `confirm.tsx` (`useConfirm`), `api.ts`, `types.ts`, `@tauri-apps/api/event` (`listen`), `lucide-react`.

## Invariants
- **Trust is granted only in native dialogs.** Adding a server, turning a tool on, marking one read-only, accepting a changed definition, and running a write call are each confirmed by the backend in a native dialog. The UI calls the command and shows `{ saved: false }` as a quiet "Not saved." (status, not an error), keeping the draft. It never fakes, skips or pre-answers that dialog, and never asks for it with `useConfirm`.
- **The UI writes no connector settings.** The only setting it writes is `ui.beta.connectors`. Connector state changes only through the connector commands.
- **Server text is plain text.** Tool names, titles, descriptions, diff fields, results, destinations and proposal arguments are attacker-controllable. They render only as React text nodes or `<pre>` text: no markdown, no `dangerouslySetInnerHTML`, no links. Tool names are never used to index a plain object (`ToolDraft` is a `Map`, and `argsTemplate` builds with `Object.fromEntries`).
- **Nothing runs without Run.** The card starts on Don't run, Esc means Don't run, and a failed decline still closes the card.
- **Secrets are write-only.** Password fields, cleared after a save. The UI shows Set or Not set, never a value.
- **`api.ts` is the only `invoke` seam.** Components call the wrappers, never `invoke`.
- **No browser dialogs.** In-app questions use `useConfirm`. The devMock stand-ins are the only `window.confirm` use.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest | `frontend/src/connectorsModel.test.ts` | 22 | add-form validation (https only, name length, credentials in URL, OAuth scopes / client secret, stdio args and variable names), argument splitting, list splitting, permission diff (only changed tools, forbidden read-only dropped, `__proto__` / `constructor` names), `grantsTrust`, kinds and status labels, args parse / pretty / compare, schema template (types, nullable unions, a `__proto__` property), proposal merge, outcome and refusal labels, formatting, relative time |
| vitest (jsdom, mocked `listen`) | `frontend/src/components/ConnectorCallCard.test.tsx` | 13 | heading, destination, kind, focus on Don't run; hostile text rendered literally; write and destructive notes; `args: null` vs edited object; invalid JSON blocks Run; backend `ok: false` keeps the card; busy state and Esc; Don't run / Esc / failed decline; events add and settle cards without duplicates; turn filter; settle-before-pending race; listen-before-pending and unlisten on unmount |
| vitest (jsdom, devMock) | `frontend/src/components/ConnectorsSection.test.tsx` | 17 | beta switch writes only `ui.beta.connectors`, and devMock refuses `connectors.*`; card contents and badges; draft → Save → native decline "Not saved." → accept; turning off asks nothing; Discard; read-only grants trust; diff review and accept with the full pending hash; Keep it off; hostile overview rendered as text; Remove via `useConfirm`; add (web and local) with validation, decline, success and auto-sync; variables; sign-in waiting state; refresh errors; Try a tool through the card (ran, declined, write declined at the native check); call log order, labels and Clear |
| vitest (other docs) | `SettingsModal.test.tsx`, `settingsModel.test.ts` | — | the eighth section, its order, and search (`mcp` now matches "Other AI apps" and "Connectors (beta)") |

**Known gaps:**
- No webview test can see the backend's native dialogs. Tests stand in for them with devMock's `window.confirm`, so the dialog wording, its default button and its Esc handling are covered only by backend tests.
- devMock's tool-argument check (required properties, basic JSON types) is much simpler than the backend's schema validation.
- `App.tsx`'s stack is exercised only through `ConnectorsSection.test.tsx`'s Try flow and the card tests, not by booting the whole App with a proposal.

## Changing it safely
- Run `npm --prefix frontend test`, `npm --prefix frontend run lint`, `npx --prefix frontend tsc -b` and `npm --prefix frontend run format:check`.
- Keep server text as text. A markdown renderer, a link or `dangerouslySetInnerHTML` anywhere in these files is a prompt-injection or phishing path.
- Don't add a UI path that grants trust without the backend's native dialog, and don't write `connectors.*` settings.
- When the backend contract changes, update `types.ts`, `api.ts` and devMock together, as for any command.
- If Ask gets its own `ConnectorCallStack turnId={…}`, make sure each proposal is shown in one place only (two stacks would show two cards for one call).
