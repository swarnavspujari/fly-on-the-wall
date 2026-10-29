# Ask (`ui-ask`)

> Layer: frontend feature. Described as of spec v2.5 PR 3.4 (the Ask UI), built against the Ask harness ([fly-ask](fly-ask.md), PR 3.3, #111) and the connectors beta ([ui-connectors](ui-connectors.md), #113). References are to functions and components, not line numbers.
> In flight: the backend asks listed under **Known gaps** (`cancel_ask`, `trace.connectors_off_keep_local`, the connectors wiring with `turn_id` = the ask id). The UI already calls or reads each of them and degrades quietly while they're missing.

## Purpose
Ask a question about the open note, every meeting, or a meeting, person or folder picked with `@`. A composer docks next to the view switcher (and on Home, for every meeting). Answers stream into a sheet over the note, or pinned beside it on wide windows, then show as **checked sentences**: each sentence carries citation chips that open the transcript at its lines; unsourced sentences are marked, and flagged claims (a number, date, name or quote that isn't in the lines it cites) say which claim failed. A collapsed trace pill says how the answer was found; a "runs on" pill says where it ran and which tier the harness chose. Changes the model proposes (`insert_into_note`, `add_action_item`) are buttons that never run on their own. The chat is ephemeral: a note's clears on a note switch, Home's when the app closes, and nothing reaches a note until you click. Every question goes through the backend's one LLM path (`ask_meeting` → `build_provider`), which enforces cloud consent and Keep local. "Runs on" changes the **global** saved LLM provider, the same setting Settings writes, and says so.

## Files it owns
- `frontend/src/components/AskPanel.tsx` — `useAskChat` (turns, streaming, Stop, scope), `applyAskEvent`, `askHistory`, `AskComposer` (scope chip, `@` suggestions, runs-on label, Send/Stop), `AskSheet`, `RunsOn`, `useWideWindow`, `PIN_MIN_WIDTH`, `GLOBAL_PROMPTS`, `OPEN_ASK_EVENT` / `openAsk`.
- `frontend/src/components/AskAnswer.tsx` — `AskTurnView` (one question and its answer: streaming view, trace pill, runs-on pill, sentences and chips, not found / refused / unverified / stopped, sources, proposals, Insert), `OpenSource`, `ProposalState`.
- `frontend/src/components/AskConnectors.tsx` — `AskConnectorCards` (a turn's connector cards, in the turn) and `useInlineAskTurns` (which turns the corner stack leaves to the sheet).
- `frontend/src/components/GlobalAsk.tsx` — Ask over every meeting, docked on Home.
- `frontend/src/askModel.ts` — pure copy and mapping: `describeAskError` (backend error prose → title/fix, now including the harness's model failures), `runsOnPlace`, `QUICK_PROMPTS`, `CLAUDE_MODELS`, `providerName`, `RUNS_ON_HELPER`, `runsOnSwitched`, and the harness copy: `stageCopy`, `traceSummary`, `traceDetails`, `usedMemory`, `toolName`, `tierLabel`, `tierDetail`, `answerRunsOn`, `KEPT_LOCAL_NOT_SEARCHED`, `notFoundCopy`, `UNVERIFIED_COPY`, `flagCopy`, `refusalCopy`, `proposalExpired`, `isCancelled`, `isWideScope`.
- `frontend/src/askScope.ts` — scope choices (`thisScope`, `ALL_SCOPE`, `sameScope`) and `@` mentions (`mentionAt`, `mentionOptions`, `removeMention`, `mentionScope`, `namedSpeakers`).
- `frontend/src/askCitations.ts` — `layoutSentences` (paragraphs and bullets), `citeGroups` (one chip per cited meeting), `citedIds`, `locateLine` (is a cited line still where Ask read it), `worstStatus`, `CITE_STATUS_COPY`, `clock`.

(The cloud-consent disclosure isn't here: the backend's native dialog owns its words; see [ui-confirm-consent](ui-confirm-consent.md).)

## How an answer is shown
1. **Send.** `useAskChat.send` adds a running turn with a fresh ask id, re-reads the saved provider, and calls `ask_meeting(noteId, history, scope, askId)`. `history` holds each earlier answered turn with its `on_device` and cited ids (`askHistory`), so the backend never sends an on-device answer to an off-device model.
2. **Stream.** The chat listens to `ask:event` once and routes each event to its turn by `ask_id` (`applyAskEvent`): `stage` (the status line: "Searching this meeting…", "Writing the answer…", "Checking the answer against the transcript…"), `sources` (the moment the prefetch finishes: "Reading <meeting>…", and the kept-local line), `turn_start` (a new model turn supersedes the last one's frames) and `frame` (the whole text so far, plain, no chips; turn 0 is the final checked text). The streaming block is `aria-busy`; nothing is announced until the answer is ready, and focus never moves.
3. **Answer.** The reply replaces the stream with `AskTurnView`:
   - `answered` / `unverified`: `layoutSentences` groups sentences into paragraphs and bullets. Each sentence renders as inline Markdown (no blocks, no raw HTML, safe links/images) followed by one `CitationChip` per cited meeting ("2 sources"). Unsourced sentences sit on `--warning-soft` with a "No source" tag; flagged ones add a `--warning` underline and "Check: <flagCopy>" (the reason is the sentence's `aria-describedby`). `unverified` adds `UNVERIFIED_COPY` above.
   - `not_found`: its own plain block (`notFoundCopy`), never the reply text (which can carry a kept-local count), with **Search all meetings** for a narrow scope.
   - `refused`: "Ask can't search this" and the harness's sentence.
   - A model failure arrives as a rejected promise: the turn is dropped, the question goes back to the composer, and `describeAskError` gives it its own title (cut off, empty, out of budget, tool steps, couldn't read meetings) with Try again.
   - **Stopped**: "You stopped this answer. Nothing from it was kept." with **Ask again**. Partial (unchecked) text is never kept.
4. **Trace pill** (`TracePill`): collapsed by default, `aria-expanded`; summary like "Searched this meeting · read 6 passages · used memory"; expanded, a mono list (`traceDetails`): searches (keyword / meaning), passages and items read, remembered facts, each tool call as `name · ok|failed`, retries and budget stops, the model (the saved model when sent; the run log names none), the tier in words, where it ran, and timings. Built from counts and enum codes only; a unit test checks no meeting text can reach it.
5. **Runs-on pill** (`RunsOnPill`): "On this computer" (success) / "On another computer" / the vendor (warning), from the run log's `reach` (else `on_device`), plus the tier ("one step" for S, "multi-step" for M and C; the tooltip says how far it could search).
6. **Kept-local line**: for a wide scope where an off-device model left Keep-local meetings out (`sources.kept_local_not_searched` or `trace.kept_local_not_searched`), `KEPT_LOCAL_NOT_SEARCHED`, with **no count**.
7. **Sources** (a `<details>`): each cited meeting and line (time, speaker, snippet), each a button that opens the transcript there.
8. **Proposals** (`ProposalCard`): exactly what would change (the Markdown for a note, or the item text and owner), approved only by a click. `insert_into_note` → `insert_ask_answer(note_id, markdown, segment_ids)`; `add_action_item` → `add_action_item(meeting_id, text, owner, segment_ids)`. Approval state lives in the chat (`chat.proposals`), so a double click or closing and reopening the sheet never approves twice; a failure says nothing changed and offers Try again; an expired or `blocked` proposal is disabled with a reason. `refusals` are said plainly (`refusalCopy`; duplicates aren't).
9. **Insert into note** (a note's Ask only): `insert_ask_answer(noteId, reply.text, citedIds)` adds an AI block with its sources (no more appending plain text to the scratchpad); "Show" opens the Enhanced view.
10. **Connectors** (beta): `AskConnectorCards` shows the turn's pending connector cards (`useConnectorProposals(turnId)` + `ConnectorCallCard` from [ui-connectors](ui-connectors.md)) inside the turn, with the decided summaries, while the sheet shows it. `useInlineAskTurns` tells the app's corner `ConnectorCallStack` (`hideTurnIds`) to skip those turns, so a call never has two cards; closing the sheet hands them back to the corner. `KEEP_LOCAL_NOTICE` shows when `trace.connectors_off_keep_local` is true.

### Citation chips and moved lines
A chip opens the transcript at its lines (`OpenSource`). An Ask answer isn't stored, so it has no citation locator rows (schema v3 writes those for note blocks and items), but each cited line carries the same locator fields: meeting, segment id, start/end, speaker and snippet. `locateLine` applies fly-storage's re-anchoring rule (`citations.rs`) to the meeting's live transcript (`get_transcript`), when the answer arrives and again on every click:
- same id, same words → `ok`;
- same id, other words (an edit) → `changed`;
- id gone → the best match within ±2 s by word coverage: `reanchored` when the words and speaker are the same (the chip says "moved" and opens the new line), else `changed` (it opens the nearest line).

A changed or moved chip gets a warning outline, "· changed" / "· moved", and the reason in its label and in Sources (`CITE_STATUS_COPY`). Once inserted into a note, the block's own locators take over ([ui-editor](ui-editor.md)).

Where a chip goes: a recording of the open note → `Editor.zoom` (switching recording if needed, Transcript view, highlighted; a floating sheet closes so it doesn't cover the line, a pinned one stays); another note's meeting → `App.openSourceAt`, which finds the note by `meeting_id` (the loaded list, else `list_recent_notes(500)`), opens it, and hands the Editor a `pendingZoom` that applies once that note's meetings are loaded.

### Scope and `@`
- **Scope chip** (`ScopeChip`): "This meeting" (or "This note"; `{kind: "note", id}`, the note's every recording plus your notes) or "All meetings" (`{kind: "all"}`), as a `menuitemradio` menu; Esc closes the menu only.
- **`@`** (`mentionAt` at the caret, after a space or at the start): a `listbox` grouped Meetings / People / Folders (`role="group"`), arrow keys move `aria-activedescendant`, Enter or Tab picks, Esc closes the list only. Meetings are recent notes with a meeting (`list_recent_notes(200)`), folders `list_folders`, both read on the first `@`; people are the open meeting's attendees and named speakers. A name that matches nothing is offered last ("Meetings with “…”"). A pick replaces the scope (the harness takes one scope per question): `{kind: "note"}`, `{kind: "folder"}` or `{kind: "person", id: name}`, shows as "@Name" with an × back to the default, and the `@query` text leaves the composer.
- Each question keeps the scope it was asked with; a question asked over something other than the default says "Searched: …".
- Defaults: a note's Ask starts on this note; Home's on all meetings.

### Global entry
- **Home** docks `GlobalAsk` (title "Ask your meetings", its own quick prompts, no Insert). Its chat lives in `App`, so it survives opening a source and coming back.
- **Sidebar** "Ask your meetings" (with the shortcut) and **Ctrl/Cmd+J** dispatch `OPEN_ASK_EVENT`: the open note's Ask (the sidebar entry starts it on All meetings; the shortcut keeps its scope), or Home's.

## Surface area
### Public API
| Export | File | Notes |
|---|---|---|
| `useAskChat(noteId \| null, {hasMeeting}): AskChat` | `AskPanel.tsx` | `turns`, `input`, `busy`, `error {raw, question, scope, provider}`, `inserted`, `proposals` + `setProposal`, `scope` / `defaultScope` / `setScope`, `send(text, scope?)`, `stop`, `retry`, `llm` / `setLlm` / `refreshLlm`, `sheetId`, `announcement`. `null` = Home. |
| `AskTurn`, `applyAskEvent`, `askHistory` | `AskPanel.tsx` | Pure, tested. |
| `AskComposer` | `AskPanel.tsx` | Props `chat, inputRef, open, onOpen, placeholder, people?`. A click, typing or Enter opens the sheet; focus alone doesn't. Enter sends, Shift+Enter adds a line, IME composition is ignored. Grows to 132 px and re-measures on resize. Loads the saved provider on mount and on `fotw:llm-settings-changed` for its label. |
| `AskSheet` | `AskPanel.tsx` | Props `chat, inputRef, hasMeeting, keptLocal, pinned, canPin, onTogglePin, onClose, insertNoteId?, onNoteChanged?, onOpenSource?, onShowInserted?`. |
| `OPEN_ASK_EVENT`, `openAsk(detail)` | `AskPanel.tsx` | `detail.scope = "all"` starts a note's Ask on All meetings. |
| `GLOBAL_PROMPTS`, `useWideWindow()`, `PIN_MIN_WIDTH = 1440` | `AskPanel.tsx` | |
| `AskTurnView`, `OpenSource`, `ProposalState` | `AskAnswer.tsx` | |
| `AskConnectorCards`, `useInlineAskTurns` | `AskConnectors.tsx` | |
| `GlobalAsk` (default) | `GlobalAsk.tsx` | |
| Copy and mapping | `askModel.ts`, `askScope.ts`, `askCitations.ts` | See **Files it owns**. |

`describeAskError` matches backend prose (first match wins). Rows added for the harness's model failures (`fly-ask` `lib.rs`):
| Substring (lowercased) | Title |
|---|---|
| `cut off before it finished` | The answer was cut off |
| `empty answer` (existing empty pattern) | The answer came back empty |
| `used up its budget` | Ask ran out of room before the model answered |
| `tool calls couldn't be understood` | The model didn't follow Ask's steps |
| `couldn't read your meetings` | Ask couldn't read your meetings |

The earlier rows (keep local, consent, no key, auth, Ollama, timeouts, rate limits, 5xx, network) are unchanged; see `askModel.ts` and `askModel.test.ts`.

### Tauri commands
- Calls: `ask_meeting` (`noteId`, `history`, `scope`, `askId`; Home passes `noteId: ""` with an explicit scope), `cancel_ask` (Stop and note switches; requested from the harness, see Known gaps), `insert_ask_answer`, `add_action_item`, `get_transcript` (citation status), `list_recent_notes`, `list_folders` (for `@`, and opening another note's source), `get_llm_settings`, `set_llm_settings` (Runs on), and through `useCloudConsent`: `get_app_setting` (reads `consent.cloud_llm.<id>`) and `request_cloud_consent`. Through `AskConnectorCards`: `pending_connector_calls` and the card's own commands ([ui-connectors](ui-connectors.md)). The pin setting `ui.ask.pinned` is read and written by `Editor.tsx` ([ui-editor](ui-editor.md)).
- Backend owners: [app-llm](app-llm.md) (`ask_meeting`, `insert_ask_answer`, `add_action_item`, and `cancel_ask` once added), [app-pipeline](app-pipeline.md) / storage reads (`get_transcript`), [app-connectors](app-connectors.md), [app-settings-consent](app-settings-consent.md).
- devMock mirrors all of them. `mockAsk` streams the same events (stage → sources → frames → checking → final frame) and resolves with the harness's reply shape; the question picks the case ("zebra" → not found, "action" / "follow-up" → proposals and a refusal, "price" → a flagged number, "crm" / "look up" with a cloud provider → a connector card first, anything else → sourced sentences plus an unsourced one). Toggles: `fotwMockAskError=<case>` (adds `empty`, `truncated`, `budget`), `fotwMockAskReply=unverified|refused`, `fotwMockAskStale=1` (one moved and one changed citation), `fotwMockAskConnectorsOff=1`, `fotwMockAskSlow=1` (to try Stop). `cancel_ask` stops a running mock answer, which then fails with "Ask was cancelled.".

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `ask:event` | listens (`useAskChat`, one listener per chat) | `{ask_id, event: {type: "stage" \| "sources" \| "turn_start" \| "frame", …}}` | `ask_meeting` with an `askId` ([app-llm](app-llm.md)) |
| `connectors:proposal`, `connectors:settled` | listens (`AskConnectorCards` through `useConnectorProposals`) | `ConnectorCallProposal`, `ConnectorSettled` | [app-connectors](app-connectors.md) |
| `fotw:open-ask` (window) | emitted by `openAsk` (Sidebar entry, Ctrl/Cmd+J in `App`); listened by `Editor` and `GlobalAsk` | `{scope?: "all"}` | — |
| `fotw:llm-settings-changed` (window) | listens (`AskComposer`, for its runs-on label); emitted by `api.setLlmSettings` | none | Settings, Runs on |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? | Meaning |
|---|---|---|---|
| `consent.cloud_llm.<provider>` | read via `useCloudConsent`; recorded by the backend's `request_cloud_consent` | No (read-only to the webview) | Allowed cloud provider ([ui-confirm-consent](ui-confirm-consent.md)). |
| LLM provider / model / base URL | write via `set_llm_settings` (`api_key` always `null`, `base_url` sent back unchanged) | n/a (dedicated command) | The global provider for Ask, Enhance, cleanup and action items. |

localStorage keys: none (devMock toggles only).

### Database
None directly. The Ask chat is never stored (DECISIONS.md M4, "Ask chat is ephemeral by design"). Writes happen only through the two clicked commands: `insert_ask_answer` (an AI block on the note, with citation locators recorded by the block write) and `add_action_item` (a `meeting_items` row, `extracted_by = "user:ask"`); both validate again in the backend.

### Files and directories written
None.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| Saved LLM provider (local Ollama, a remote Ollama, or Anthropic / OpenAI / NIM) | each question (`ask_meeting`) | user action + backend consent + Keep local (`build_provider`; wide scopes exclude Keep-local meetings for an off-device model in the harness). The kept-local warning under Runs on is advisory only. |
| A connector's server | only after you approve its card | the connectors beta rules ([ui-connectors](ui-connectors.md)) |

### External processes
None from the webview (the backend may start the managed Ollama).

### Secrets and keychain entries
None. Runs on always sends `api_key: null`. Keys are entered in Settings.

## Dependencies
- **Inbound:** `Editor.tsx` (composer in the dock with `people`, sheet floating or pinned, `insertNoteId`, `onOpenSource` → `zoom` or `onOpenSourceElsewhere`, `pendingZoom`, `OPEN_ASK_EVENT`), `App.tsx` (`useAskChat(null)` for Home, `GlobalAsk`, `openSourceAt`, Ctrl/Cmd+J, `useInlineAskTurns` → `ConnectorCallStack hideTurnIds`), `Sidebar.tsx` (the "Ask your meetings" entry).
- **Outbound:** `api.ts`, `@tauri-apps/api/event` (`listen`), `settingsModel.ts` (`providerLabel`, `providerVendor`), `homeModel.ts` (`isLoopbackUrl`), [ui-confirm-consent](ui-confirm-consent.md) (`useCloudConsent`, `useConfirm`), [ui-connectors](ui-connectors.md) (`useConnectorProposals`, `ConnectorCallCard`, `cardHeading`, `decidedLabel`, `KEEP_LOCAL_NOTICE`), `markdownSafe.tsx` (`SAFE_A`, `SAFE_IMG`), `ui.tsx` (`Sheet`, `Notice`, `Select`, `Button`, `CitationChip`, `useEscapeLayer`), npm `react-markdown`, `remark-gfm`, lucide-react.

## Invariants
- One LLM path: Ask calls only `ask_meeting`. Consent and Keep local are enforced in `build_provider` (spec §2 item 1).
- **Nothing writes without a click.** Proposals are never executed by the UI on arrival; Insert and each proposal call their command once, on a click, and the approval state survives closing the sheet.
- **The trace holds no meeting text.** The trace pill, its details and the runs-on pill are built from counts and enum codes (`traceSummary`, `traceDetails`); the kept-local line never states a count.
- **Unchecked text is never kept.** Streamed frames show while the answer is written; Stop drops them, and only the checked reply becomes an answer (and history).
- A reply for a note you've left, or one you stopped, is dropped (`seq`); a note switch also asks the backend to stop it. Double Enter sends once (`inFlight`).
- **Runs on writes the global provider** and keeps its stale-write rules: it re-reads before every write and writes nothing if that fails; a model pick never switches the provider behind your back; a re-read that started before the last write is ignored; it's disabled while a question runs. A cloud pick asks for consent before anything is saved.
- An error names the provider the question went to, read alongside the question.
- Accessibility: streaming never moves focus and announces only "Answer N is ready." (or the failure / Stop); chips are buttons with labels naming their meeting and status; the scope menu, `@` list and sheet each close on Esc through `useEscapeLayer` (innermost first); buttons that disappear on click hand focus back to the composer.
- Everything rendered from the harness is sanitized there and rendered without raw HTML, remote images or in-app navigation (THREAT_MODEL T1/T2).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest | `frontend/src/components/AskPanel.test.tsx` | 45 | composer and sheet (open/focus, prompts, Enter / Shift+Enter, single send, Insert as an AI block, error mapping + retry, consent "Review and allow", stale reply dropped and cancelled, Esc layering, Runs on rules); harness answers (streaming stages, sources, frames and superseded turns; Stop and Ask again; backend "cancelled"; not found + Search all meetings; model failure copy; unsourced and flagged sentences; unverified; chips opening lines with moved / changed status; trace pill details and no meeting text; runs-on pill local and cloud; kept-local line without a count; proposals once-only, expired, refusals, failure; `@` meetings / people / typed name / folder and Esc; scope menu; follow-up history; Ask over every meeting; composer runs-on label); connectors (a card in its turn and not in the corner, back in the corner when the sheet closes; the Keep-local notice) |
| vitest | `frontend/src/askHarnessModel.test.ts` | 16 | harness failure titles, cancel detection, trace summary and details (no meeting text, no kept-local count), where an answer ran, tier words, stage, not-found, flag and refusal copy, proposal expiry |
| vitest | `frontend/src/askScope.test.ts` | 9 | `mentionAt`, `mentionOptions` (word starts, groups, typed name last, duplicates), `removeMention`, `mentionScope`, scope labels, named speakers |
| vitest | `frontend/src/askCitations.test.ts` | 8 | sentence layout, chip groups, cited ids, `locateLine` ok / changed / reanchored / nearest / none, worst status, clock |
| vitest | `frontend/src/askModel.test.ts` | 10 | `describeAskError` table, `runsOnPlace`, copy |
| vitest | `frontend/src/components/Editor.ask.test.tsx` | 3 | pin + remembered `ui.ask.pinned` (devMock), Esc from the title, dock click-through (class names only) |

**Known gaps:**
- **Backend asks (from the harness, #111):**
  - `cancel_ask(ask_id)`: set the `cancel` flag `AskEnv` already takes, so Stop stops the model (today the app passes `cancel: None`). The UI calls it and ignores a failure; until then Stop only drops the answer.
  - `trace.connectors_off_keep_local: bool` (user-only, like `kept_local_not_searched`): connectors were off because the scope holds a Keep-local meeting. Until then `KEEP_LOCAL_NOTICE` never shows.
  - Connectors wiring (`connectors::turn_for_ask` with `turn_id` = the `ask_id` the UI sends), so `ConnectorCallProposal.turnId` matches the turn; until then connector cards only appear in the dev mock.
  - Nice to have: `note_id` on each `SourceView` (opening a source in an older recording of another note needs it; today it's found by the note's latest `meeting_id`), a people list for `@` across meetings (today: the open meeting's attendees and speakers, plus any typed name), and an optional `note_id` on `ask_meeting` (Home passes `""`).
- **No cross-check against the Rust strings.** `describeAskError` keys on prose in `llm_commands.rs`, `fly-ask` and `fly-llm`; tests and devMock use hand copies, so a Rust wording change silently drops Ask to "Ask couldn't answer". The harness's reply shape is likewise mirrored by hand in `types.ts` and devMock.
- `CLAUDE_MODELS` vs `ANTHROPIC_MODELS` vs the Rust model ids: no test ties them together.
- No test of the Editor's `openAskSource` / `pendingZoom` wiring or App's `openSourceAt` (the AskPanel tests stop at `onOpenSource`); checked by hand in the dev mock.
- `Editor.ask.test.tsx` "the dock lets clicks through" checks Tailwind class strings (jsdom applies no CSS).
- Not tested in the real app (only vitest and the dev mock): it needs #111 (and #113 for connector cards).

## Changing it safely
- Run `npm --prefix frontend test`, lint, prettier and `tsc`.
- When the harness's reply or event shape changes (`fly-ask` `AskAnswer`, `Trace`, `AskEvent`, `SourceView`, `Proposal`), update `types.ts`, `askModel.ts`, devMock's `mockAsk` and the tests in the same PR.
- When a backend error string changes (`llm_commands.rs`, `ollama.rs`, `fly-ask`, `fly-llm`), update `describeAskError`, `askModel.test.ts` / `askHarnessModel.test.ts` and devMock in the same PR. Never reword `CONSENT_REFUSAL_MARK` without updating the TS match.
- Keep the trace built from counts and codes. Never put `trace_lines`, frames or reply text into the trace pill, and never show the kept-local count.
- Keep every write behind a click and once-only (`chat.proposals`, `inserted`).
- Keep every send behind `ask_meeting`. Don't add a client-side LLM call or a second provider path (spec §2 item 1).
- `locateLine` copies fly-storage's re-anchoring thresholds (±2 s, 0.6); change both together.
