# Connectors (`ui-connectors`)

> Layer: frontend feature. New with connectors (beta); not on main yet. References are by symbol name.
> In flight: the backend side ([app-connectors](app-connectors.md), [fly-mcp-client](fly-mcp-client.md)) lands with it. Ask doesn't yet mount a per-turn card stack or show the Keep-local notice; the app-level stack shows every proposal, and `KEEP_LOCAL_NOTICE` is exported for Ask to use.

## Purpose
Connectors let Ask and Enhance call tools on MCP servers you add: hosted ones from a built-in directory, custom web addresses (browser sign-in, an API key, or none) and local programs. This component is the UI for that feature:

- **Settings › Connectors** (title plus a Beta pill). The "Let Ask use connectors (beta)" switch sits at the top. Under it are the list of your connectors, **Browse connectors** (the directory, with a preview before Connect) and **Add connector ▾** (Custom, Local, Import from Claude Desktop). Each connector has a detail page with About · Tools · Activity and a ⋯ menu (Reconnect, Edit, Disconnect).
- **Call cards.** Every call proposed for a tool on Needs approval waits on a card with its exact details until you decide.

The safety rules live in the backend. The UI asks the backend for every change and shows what comes back. Every trust grant is decided in a native dialog that the backend shows, never in the webview. In the dev mock, the browser's `confirm` stands in for that dialog.

## Files it owns
- `frontend/src/connectorsModel.ts`: pure logic:
  - permissions: order, labels, `loosens`, `allowedPermissions`, groups, `groupValue` ("Mixed"), `planGroupChange`;
  - statuses, badges and the review banner;
  - directory search;
  - the Add custom and Add local forms → `ConnectorInput` (with Edit prefills), `parseCommand` for "Paste a command", and `willRunPreview`;
  - the call cards' argument fields (`schemaFields`, `argsFromFields`) and JSON editing;
  - "Try…": `canTry` and the `argsTemplate` it starts from;
  - activity labels and times;
  - copy constants, including `KEEP_LOCAL_NOTICE` for Ask.
- `frontend/src/components/ConnectorsSection.tsx`: the section body. It holds the beta switch, the list (with loading, error and empty states), the polite live region, navigation between the list and a detail page, and which sheet is open.
- `frontend/src/components/ConnectorDetail.tsx`: the detail page, with its header, ⋯ menu, sign-in state, review banner, About, Tools (groups, three-state controls), Activity and the review dialog.
- `frontend/src/components/ConnectorAdd.tsx`: `BrowseSheet` (search, cards, preview), `CustomSheet` and `LocalSheet` (add or edit), and `ImportSheet`.
- `frontend/src/components/connectorParts.tsx`: shared pieces:
  - `MenuButton`: `role="menu"`, arrow keys, Esc returns focus;
  - `ErrorText`: [Show details] for program output;
  - `SecretField`: "Saved in your keychain" + [Replace];
  - `KVRows`: headers or settings, each with a Secret switch;
  - `useSignIn` and `SignInWaiting`;
  - `Callout`, `ConnectorIcon`.
- `frontend/src/components/ConnectorCallCard.tsx`: `useConnectorProposals`, `useArmed`, `ConnectorCallCard` and `ConnectorCallStack`.
- `frontend/src/components/connectorsTesting.tsx`: test helpers.
- Shared with the app shell:
  - `frontend/src/api.ts`: the command wrappers and the three event-name constants;
  - `frontend/src/types.ts`: the types;
  - `frontend/src/devMock.ts`: the devMock connector fixtures.
- Placement: the `"connectors"` entry in `settingsModel.ts` belongs to [ui-settings](ui-settings.md). `SettingsModal.tsx` renders the section after "Other AI apps", and `App.tsx` mounts one `<ConnectorCallStack />` after `AppNotices`.

## Surface area
### Screens and states
- **List.** Each row shows an icon, the name, a badge (Local, or Custom unless its address is in the directory), the status in words and a switch.
  - Statuses: "Connected · host", "Needs sign-in", "N tools changed — review", "Couldn't start", "Couldn't reach host", "Not checked yet", "Off" and "Not approved — edit it to approve again". Problem states add an icon.
  - Switching off applies at once. Switching on asks in a native dialog, and a no leaves the switch off with no error.
  - Other states: the empty state with [Browse connectors], skeleton rows while loading, and "Couldn't load connectors." [Try again].
- **Browse.**
  - Search, then cards (icon, description, "Made by vendor", [+]).
  - [+] opens a preview first: what it does, its full address, "Sign-in happens in your browser." (OAuth) or a key field (API key), and "You'll see its tools after you approve it. Fly on the Wall doesn't contact <host> before then." Nothing is sent before the native dialog.
  - [Connect] → native dialog → for OAuth, "Finish signing in in your browser… [Cancel]" → the detail page, which lists the tools found, every one on Needs approval. Cancel only hides the wait. A sign-in that finishes later still updates the connector.
  - No results: "Nothing matches '…'." [Add custom connector].
- **Add custom.**
  - Fields: Name and Web address (https only). "How does it sign in?" has three radios with none pre-selected. The API key is a password field.
  - Advanced has request headers (key/value, Secret switch) and the OAuth client ID and secret.
  - A trust note sits above the buttons. [Continue] → native dialog (the full address, headers and secret names) → the detail page.
  - Errors: form errors stay on the form. A server that doesn't answer, or wants a browser sign-in when "No sign-in" was chosen, shows on the detail page after adding ("That address didn't answer like a connector.", "Sign in to this connector first.").
- **Add local.**
  - A warning stays visible throughout. Fields: Name; Program (npx, uvx, or Choose a file…, which opens the backend's native file picker); one argument per row; and Settings (environment) as key/value rows with a Secret switch.
  - "Paste a command" fills the rows and never runs anything. The monospace "Will run:" preview shows secret values as ••••. The native dialog shows the program's full path, found on PATH once at approval (a program that isn't found is an error before the dialog).
  - Errors are the backend's words. Text after "\n---\n" (program output, secrets scrubbed by the backend) goes under [Show details].
- **Import from Claude Desktop.**
  - "We found N connectors in Claude Desktop." Every checkbox starts unchecked. Details show the command and masked setting names ("will move to your keychain").
  - [Review N] steps through them one at a time ("1 of 2"), each with its own native dialog. Each step notes that a key written into the command or address isn't moved to the keychain (Claude Desktop configs sometimes hold one there): skip it and add the connector by hand with the key as a secret.
  - If none is found: "No Claude Desktop setup found on this computer."
- **Detail.**
  - The header shows the status and the ⋯ menu. Reconnect signs in again for OAuth connectors; for the others it checks the tools.
  - **About**: sign-in, address or "Will run", headers or settings (secret ones as "Saved in your keychain"), approval and config hash, and when the tools were last checked.
  - **Tools**: "Read-only tools (n)" and "Tools that can change or delete (n)". Each group has a menu button announced as, for example, "Read-only tools: Mixed".
    - Each tool row has a three-part radiogroup labelled "Permission for <tool>" (Always allow · Needs approval · Blocked).
    - Changed, new, missing and unusable tools can only be Blocked. Unlabelled tools carry "Not labelled — treated as a change tool".
    - Always allow on a change tool shows the meeting warning, with an icon.
    - **Try…** (tools that aren't Blocked and have status `ok`) opens an inline details editor prefilled from the input schema (required properties with empty values). [Run] calls `test_connector_tool`, and the returned text shows in a `<pre>` (plain text, styled ok or error). The panel says the call goes through the same approval card as Ask, or that it runs without asking on Always allow. Until the Ask harness lands, this is the only way to make a call in the real app.
  - **Review**: the banner "N tools changed since you approved them. They're blocked until you review." [Review] opens a dialog.
    - It steps one tool at a time, showing "Was:" and "Now:" for each changed field, or the description and inputs of a new tool. "Show unchanged" reveals the rest.
    - [Keep blocked] just closes. [Approve changes] calls `approve_connector_tool`, and the backend asks in a native dialog.
  - **Activity**: Time · Tool · Result (Allowed by you / Allowed automatically / Denied / Blocked / Failed, with the reason underneath), a "Problems only" filter, the footnote "Only tool names and outcomes are kept — never meeting text.", and an in-app confirm before clearing.
  - **Disconnect**: an in-app confirm, "Disconnect <name>? Your sign-in is removed from this computer."
- **Call cards.**
  - A read card says "Ask wants to look something up in <server> · <tool>". It has fields generated from the input schema (string, number, integer, boolean, string enums) with a "Show as code" toggle; otherwise it shows the JSON editor. Its buttons are [Deny] [Allow once] and "Always allow this tool…".
  - A write card has an amber edge plus the text "Can change data" (and "Destructive"), each with an icon. It says "Ask wants to change something in …" and has [Deny] [Review and send…], which leads to the backend's native confirm. It has no Always allow.
  - After a decision a card collapses to "Allowed once · 2:14 PM", "Always allowed · …" or "Denied" for 4 s.
  - `ok: false, error: null` (a no in the native dialog) keeps the card open with no error text.

### Public API
| Export | File | What it does |
|---|---|---|
| `ConnectorsSection` (default) | `ConnectorsSection.tsx` | No props. Reads `connectors_overview` on mount and on every `connectors:changed`, and the directory once the beta is on. |
| `ConnectorDetail` (default) | `ConnectorDetail.tsx` | One connector's page. The section renders it; it isn't used elsewhere. |
| `BrowseSheet`, `CustomSheet`, `LocalSheet`, `ImportSheet` | `ConnectorAdd.tsx` | The add flows (and the forms that edit a connector) as `Modal`s. |
| `useConnectorProposals(turnId?)` | `ConnectorCallCard.tsx` | Subscribes to `connectors:proposal` / `connectors:settled`, then reads `pending_connector_calls`. Remembers settled ids, so a stale pending list can't bring a card back. Returns `{ proposals, markSettled }`. |
| `ConnectorCallStack`, `ConnectorCallCard` | `ConnectorCallCard.tsx` | Bottom-right, z-index 58 (above Settings at 55, below in-app dialogs at 60). One card at a time; a polite live region announces each one. |
| `useArmed`, `ARM_DELAY_MS` (700), `COLLAPSE_MS` (4000) | `ConnectorCallCard.tsx` | See the arm delay below. |
| `KEEP_LOCAL_NOTICE` | `connectorsModel.ts` | For Ask: "Connectors were off for this question because it includes a Keep-local meeting, which never leaves this computer." |

**Arm delay (review finding):** Allow once, Review and send… and "Always allow this tool…" stay disabled for 700 ms after a card mounts, and again whenever the visible card changes to another proposal (`useArmed` is keyed by the proposal id). A double click on one card therefore can't approve the next one. Deny is never delayed.

**Focus:** a card moves focus to Deny only when nobody is typing in an input, textarea or contenteditable; otherwise it is only announced. Esc on a focused card means Deny. Opening a detail page focuses its heading, and Back returns focus to the row.

### Tauri commands
| Command | Wrapper (`api.ts`) | Called from | Native dialog (backend) |
|---|---|---|---|
| `connectors_overview` | `connectorsOverview` | section mount, `connectors:changed`, after changes | — |
| `connectors_directory` | `connectorsDirectory` | section (badges), Browse | — |
| `add_connector` | `addConnector` | Browse, Add custom, Add local | Yes (add, or "Run a program on this computer?") |
| `update_connector` | `updateConnector` | ⋯ › Edit | Yes |
| `set_connector_enabled` | `setConnectorEnabled` | list switch | When turning on |
| `remove_connector` | `removeConnector` | ⋯ › Disconnect (after `useConfirm`) | — |
| `sync_connector_tools` | `syncConnectorTools` | ⋯ › Reconnect, About › Check again | — |
| `set_connector_tool_permissions` | `setConnectorToolPermissions` | tool controls, group menus | When loosening |
| `approve_connector_tool` | `approveConnectorTool` | Review › Approve changes | Yes |
| `sign_in_connector` / `sign_out_connector` | `signInConnector` / `signOutConnector` | Browse, Add custom, detail | — (opens the system browser) |
| `claude_desktop_candidates` / `import_claude_desktop_connector` | `claudeDesktopCandidates` / `importClaudeDesktopConnector` | Import | On each import |
| `pick_connector_program` | `pickConnectorProgram` | Add local › Choose a file… | Native file picker |
| `connector_call_log` / `clear_connector_call_log` | `connectorCallLog` / `clearConnectorCallLog` | Activity | Clearing: "Clear connector activity?" (no in-app confirm) |
| `pending_connector_calls` | `pendingConnectorCalls` | `useConnectorProposals` | — |
| `decide_connector_call` | `decideConnectorCall` | call cards (`deny`, `allow_once`, `always_allow`) | `always_allow` on a read tool; `allow_once` on a write tool |
| `test_connector_tool` | `testConnectorTool` | Tools › Try… | Always its card (Always allow too), then the native confirm for a write |
| `get_app_setting` / `set_app_setting` | `getAppSetting` / `setAppSetting` | the beta switch only | — |

devMock mirrors all 19 connector commands in memory:
- **Fixtures.**
  - "Acme CRM" is a web connector with OAuth. Its tools cover read, write, destructive, unlabelled, changed-with-diff, new and unusable.
  - "Docs search" is a local `npx -y @example/docs-server` with a secret `DOCS_TOKEN`.
  - "Team wiki" is a web connector with an API key, a secret header, and its switch off.
  - Also four synthetic directory entries and two Claude Desktop candidates.
- **Dialogs.** Native dialogs are stood in for by one `mockNativeDialog` helper behind an `eslint-disable`.
- **Toggles.** `fotwMockSignIn = "cancel" | "offline"` and `fotwMockClaudeImport = "none"`. A local connector with a `--crash` argument stops right away, with details; the program `missing-program` isn't found at approval; bare program names are pinned to `/usr/local/bin/<name>`. A host containing `not-a-connector` is added, then doesn't answer like a connector.
- **QA hook.** `fotwMockConnectorCall(tool)` puts a card on screen for any Acme CRM tool.
- **Settings.** Mock `set_app_setting` refuses `connectors.*`, like the backend.
- **Tests.** Tests reset the fixtures with `resetMockConnectors()`.

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `connectors:proposal` (`CONNECTORS_PROPOSAL_EVENT`) | Listens: `useConnectorProposals` | `ConnectorCallProposal` | Backend approver, when a call is proposed |
| `connectors:settled` (`CONNECTORS_SETTLED_EVENT`) | Listens: `useConnectorProposals` (drops the card), Activity tab (re-reads) | `ConnectorSettled` | Backend, on a decision, a withdrawal or a timeout |
| `connectors:changed` (`CONNECTORS_CHANGED_EVENT`) | Listens: `ConnectorsSection` (re-reads the overview) | `{ id }` | Backend, when a connector's state changes |

### Settings keys
| Key | Read / write | Meaning |
|---|---|---|
| `ui.beta.connectors` | read (through `connectors_overview.beta`, or `get_app_setting` as a fallback) and write (`"1"` / `"0"`) | The "Let Ask use connectors (beta)" switch. It's the only setting this component writes. |
| `connectors.*` | never | Backend-owned connector state and trust. `set_app_setting` refuses these keys. |

localStorage keys: none.

### Database, files, network, processes
None from the webview. The backend stores connectors and the call log, makes every connection and starts local programs. The only link the section renders is its own "Learn more" (MCP), which the app's link guard opens in the system browser.

### Secrets and keychain entries
None read. API keys, client secrets and secret header or setting values are typed into password fields and sent once to the backend, which keeps them in the OS keychain. The UI only ever shows "Saved in your keychain" with [Replace], never a value or a Reveal toggle. On Add, every secret row needs a value; on Edit, an empty value keeps the stored one.

## Dependencies
- **Inbound:** `SettingsModal` (renders `ConnectorsSection`), `App` (mounts `ConnectorCallStack`).
- **Outbound:**
  - `ui.tsx`: `SettingsGroup`, `SettingsRow`, `Switch`, `Segmented` (the three-state control: a radiogroup with roving tabindex and arrow keys), `Checkbox`, `Input`, `Select`, `Badge`, `Button`, `Modal`, `Avatar`, `useEscapeLayer`;
  - `confirm.tsx` (`useConfirm`), `api.ts`, `types.ts`, `@tauri-apps/api/event` (`listen`), `lucide-react`.

## Invariants
- **Trust is granted only in native dialogs.** Adding or turning on a connector, loosening a tool's permission, approving a changed or new tool, Always allow from a card, and sending a write call are each confirmed by the backend. The UI calls the command and treats `saved: false` (or `ok: false, error: null`) as "nothing changed": no error, the old value stays. Tightening applies at once. Nothing in the webview fakes, skips or pre-answers a native dialog.
- **Nothing is pre-checked.** No sign-in choice and no import checkbox start selected. New tools start on Needs approval.
- **Server text is plain text.** Names, titles, descriptions, diffs, schemas, field labels, proposal arguments and error output render only as React text nodes or `<pre>` text: no markdown, no `dangerouslySetInnerHTML`, no links. Names from the server never index a plain object (Maps, own-property checks, `Object.fromEntries`).
- **Write and changed states never rely on colour alone.** Each has an icon and words.
- **The arm delay** on a card's allowing buttons is 700 ms after the card mounts or after the visible card changes.
- **The UI writes no connector settings;** the only one it writes is `ui.beta.connectors`.
- **No browser dialogs:** in-app questions use `useConfirm`. The devMock stand-in is the only `window.confirm`.
- **`api.ts` is the only `invoke` seam.**

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest | `frontend/src/connectorsModel.test.ts` | 35 | Keep-local copy; details split; permissions (loosening, allowed values, groups, Mixed, group plans, chips); statuses, badges, directory search, directory input; Add custom (https, sign-in choice, headers, secrets, API key, OAuth, edit prefill); Add local (program, file, args, settings, preview with ••••, Paste a command, edit prefill); card fields (simple schemas, fallback to code, values ⇄ args, `__proto__`); JSON editing; decided labels; proposal merge; activity results and times; Try… availability and the args template (`__proto__` stays a key) |
| vitest (jsdom, mocked `listen`, fake timers) | `frontend/src/components/ConnectorCallCard.test.tsx` | 17 | read card words and focus; the 700 ms arm delay on mount and when the visible card changes (double click); null vs edited args; Show as code; the code fallback; Always allow; a native no keeps the card quietly; Deny/Esc collapse and removal; a failed deny; not stealing focus while typing; hostile text; write card markers and no Always allow; events, turn filter, races, unlisten |
| vitest (jsdom, devMock) | `frontend/src/components/ConnectorsSection.test.tsx` | 18 | beta switch writes only the flag; list rows, badges, statuses, Add menu; switch on asks and off applies at once; empty, error and retry; hostile text; detail heading focus, tabs, groups, chips, blocked-only states, Back focus; tighten vs loosen (declined keeps the value, announced); write Always allow warning; group menu; review (keep blocked, approve declined then accepted, both tools); About; sign-in wait and Cancel; sign-in error; Activity rows, filter and clear; Disconnect copy; Edit prefill and save; Try… through the approval card (allowed and denied) and on Always allow (no card) |
| vitest (jsdom, devMock) | `frontend/src/components/ConnectorAdd.test.tsx` | 13 | Browse search, no results, preview without contact, Connect declined then accepted; OAuth wait and Cancel; sign-in error; API-key directory entry; Add custom validation, native dialog, bad address on the detail page, API key and secret header; Add local preview and run dialog; Paste a command and Choose a file…; program crash with Show details; Claude Desktop import steps and not-found |
| vitest (other docs) | `SettingsModal.test.tsx`, `settingsModel.test.ts` | — | the eighth section, its title, order and search |

**Known gaps:**
- No webview test can see the backend's native dialogs. The tests stand in for them with devMock's `window.confirm`, so their wording, default buttons and Esc handling are covered only by backend tests.
- devMock's argument check is simpler than the backend's schema validation.
- The App-level stack is exercised through the card tests, not by booting the whole App with a proposal.

## Changing it safely
- Run `npm --prefix frontend test` (use `--maxWorkers=1` if workers time out), `npm --prefix frontend run lint`, `npx --prefix frontend tsc -b` and `npm --prefix frontend run format:check`.
- Keep server text as text. A markdown renderer, a link or `dangerouslySetInnerHTML` in these files is a prompt-injection or phishing path.
- Don't add a path that grants trust without the backend's native dialog, and don't write `connectors.*` settings.
- Keep the arm delay on every button that allows a call, and key it by the proposal id.
- When the backend contract changes, update `types.ts`, `api.ts` and devMock together.
- If Ask gets its own `ConnectorCallStack turnId={…}`, show each proposal in one place only.
