# Ask (`ui-ask`)

> Layer: frontend feature. Described at main `82b7edf` (v2.4.0), plus backend-only cloud consent (#102). Line references into `askModel.ts`, `askModel.test.ts`, `api.ts`, `devMock.ts` and `llm_commands.rs` are as of #102.
> In flight: the Ask harness (spec PR 3.3) will wire the dormant `fly-context` / `fly-llm` tool loop behind `ask_meeting`, which may change its error strings, and `describeAskError` matches those strings. PR #92 / #86: none here.

## Purpose
Ask a question about the open note or meeting. A composer docks next to the view switcher. Answers appear in a sheet over the note, or pinned beside it on wide windows. The chat is ephemeral: it clears on a note switch, and nothing reaches the note until you click "Insert into note". Every question goes through the backend's one LLM path (`ask_meeting` → `build_provider`), which enforces cloud consent and Keep local. "Runs on" changes the **global** saved LLM provider, the same setting Settings writes, and says so.

## Files it owns
- `frontend/src/components/AskPanel.tsx` — `useAskChat`, `AskComposer`, `AskSheet`, `RunsOn`, `useWideWindow`, `PIN_MIN_WIDTH`.
- `frontend/src/askModel.ts` — pure copy and mapping: `describeAskError`, `runsOnPlace`, `QUICK_PROMPTS`, `CLAUDE_MODELS`, `providerName`, `RUNS_ON_HELPER`, `runsOnSwitched`. (The cloud-consent disclosure isn't here: the backend's native dialog owns its words; see [ui-confirm-consent](ui-confirm-consent.md).)

## Surface area
### Public API
| Export | File | Notes |
|---|---|---|
| `useAskChat(noteId): AskChat` | `AskPanel.tsx:105-212` | History, input, busy, error `{raw, question, provider}`, inserted set, `send`, `retry`, `llm` (saved settings), `setLlm`, `refreshLlm`, `sheetId`, `announcement`. |
| `AskComposer` | `AskPanel.tsx:217-312` | A click, typing or Enter opens the sheet; focus alone doesn't. Enter sends, Shift+Enter adds a line, IME composition is ignored. A polite live region announces answers. |
| `AskSheet` | `AskPanel.tsx:539-769` | Quick prompts, answers (react-markdown with `SAFE_A` / `SAFE_IMG`), Insert into note, error `Notice` with Try again or "Review and allow <vendor>", Runs on footer, Pin to side. |
| `useWideWindow()`, `PIN_MIN_WIDTH = 1440` | `AskPanel.tsx:50-70` | `matchMedia` gate for pinning. |
| `describeAskError(error, providerId): AskErrorView` | `askModel.ts:49-141` | Maps backend error **prose** to `{title, fix, raw, action?}`. See the table below. |
| `runsOnPlace(info)` | `askModel.ts:146-161` | `local` (loopback Ollama), `remote` ("Off this computer · host:port"), or `cloud` (vendor). Uses `isLoopbackUrl` from `homeModel.ts`. |
| `CLAUDE_MODELS` | `askModel.ts:20-23` | `claude-sonnet-5-5` (default), `claude-haiku-5-5`. A hand copy of `ANTHROPIC_MODELS` in `SettingsModal.tsx:89-92`. The Rust side has its own list (`crates/fly-llm/src/anthropic.rs:27`, `:30`; `crates/fly-core/src/prompt_profile.rs:185-186`). |
| `QUICK_PROMPTS`, `RUNS_ON_HELPER`, `runsOnSwitched`, `providerName` | `askModel.ts:10-29`, `:163-168` | Copy. `providerName` is also used by [ui-confirm-consent](ui-confirm-consent.md) (the skipped-cleanup notice). |

`describeAskError` matches (first match wins), checked against the Rust source at `82b7edf` (`llm_commands.rs` lines as of #102):
| Substring / pattern (lowercased) | Title | Backend source |
|---|---|---|
| `keep local` | This note is kept local | `keep_local_refusal`, `src-tauri/src/llm_commands.rs:514-519` |
| `isn't allowed to receive your meeting content` (`action: "consent"`) | `<Vendor>` isn't confirmed yet | `CONSENT_REFUSAL_MARK`, `llm_commands.rs:435`, `:560` |
| `no api key` | No API key for `<Vendor>` | `llm_commands.rs:621` |
| `authentication failed` | `<Vendor>` didn't accept the API key | `LlmError::Auth`, `crates/fly-llm/src/lib.rs:35` |
| `ollama isn't installed` | The local AI isn't installed | `src-tauri/src/ollama.rs:91` |
| (provider `ollama`) `model .*not found` / `try pulling` / … | The local model isn't downloaded | Ollama's HTTP body via `LlmError::Provider` (not in repo source) |
| `ollama isn't reachable` / `isn't answering yet` / `could not start ollama` | Can't reach the local AI | `fly-llm/src/lib.rs:68`, `ollama.rs:86`, `:131`, `:118` |
| context-length patterns | This meeting is too long for the model | provider HTTP bodies |
| `timed out` / `timeout` | `<Vendor>` took too long to answer | `fly-llm/src/lib.rs:78` |
| `429` / `529` / rate limit / overloaded | `<Vendor>` is busy right now | provider HTTP bodies |
| `5xx` | `<Vendor>` had a problem answering | provider HTTP bodies |
| `can't reach` / `network error` / `error sending request` / `connection` | Can't reach `<Vendor>` | `fly-llm/src/lib.rs:73`, `:39` |
| `no message content` / empty answer | The answer came back empty | `crates/fly-llm/src/openai_compat.rs:693`, `:859` |
| anything else | Ask couldn't answer | — |

### Tauri commands
- Calls: `ask_meeting`, `get_llm_settings`, `set_llm_settings` (Runs on), and through `useCloudConsent`: `get_app_setting` (reads `consent.cloud_llm.<id>`) and `request_cloud_consent` (the backend asks in a native dialog and records a yes). The pin setting `ui.ask.pinned` is read and written by `Editor.tsx` (see [ui-editor](ui-editor.md)).
- Backend owners: `app-llm` (`ask_meeting`), `app-settings-consent` (the rest).
- devMock mirrors all of them. `mockAsk` (`devMock.ts:958-974`) mimics `build_provider` (keep local, then consent, then key). `fotwMockAskError=<case>` forces each backend error. `MOCK_ASK_ERRORS` (`devMock.ts:931-955`) is a **third** hand copy of the backend strings (Rust, `askModel.test.ts`, devMock).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `fotw:llm-settings-changed` (window) | Emitted indirectly: `api.setLlmSettings` fires it after a Runs on write (`api.ts:174-175`). Ask doesn't listen. It re-reads on sheet open, Runs on focus, pointer enter and each send (`AskPanel.tsx:567-569`, `:446`, `:606`, `:158`). | none | skipped-cleanup notice ([ui-confirm-consent](ui-confirm-consent.md)) |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_writable_setting_key`) | Meaning |
|---|---|---|---|
| `consent.cloud_llm.<provider>` | read via `useCloudConsent`; recorded by the backend's `request_cloud_consent` | No (read-only to the webview) | Allowed cloud provider (see [ui-confirm-consent](ui-confirm-consent.md)). |
| LLM provider / model / base URL | write via `set_llm_settings` (`AskPanel.tsx:357-362`; `api_key` always `null`, `base_url` sent back unchanged) | n/a (dedicated command) | The global provider for Ask, Enhance, cleanup and action items. |

localStorage keys: none. devMock toggles: `fotwMockAskError`, `fotwMockConsentReset`, `fotwMockOllama`.

### Database
None directly. The Ask chat is never stored (DECISIONS.md M4, "Ask chat is ephemeral by design"). Insert appends the answer to the note's scratchpad through `Editor.insertFromAsk` → `update_note_scratchpad` (`Editor.tsx:1191-1196`).

### Files and directories written
None.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| Saved LLM provider (local Ollama, a remote Ollama, or Anthropic / OpenAI / NIM) | each question (`ask_meeting`) | user action + backend consent + Keep local (`build_provider`). The kept-local warning under Runs on is advisory only (`AskPanel.tsx:437-444`). |

### External processes
None from the webview (the backend may start the managed Ollama).

### Secrets and keychain entries
None. Runs on always sends `api_key: null` (`AskPanel.tsx:361`). Keys are entered in Settings.

## Dependencies
- **Inbound:** `Editor.tsx:52`, `:991`, `:1563-1602` (composer in the dock, sheet floating or pinned, `keptLocal` from the meeting's privacy).
- **Outbound:** `api.ts`, `settingsModel.ts` (`providerLabel`, `providerVendor`), `homeModel.ts` (`isLoopbackUrl`), [ui-confirm-consent](ui-confirm-consent.md) (`useCloudConsent`, `useConfirm`), `markdownSafe.tsx` (`SAFE_A`, `SAFE_IMG`), `ui.tsx` (`Sheet`, `Notice`, `Select`, `Button`), npm `react-markdown`, `remark-gfm`.

## Invariants
- One LLM path: Ask calls only `ask_meeting`. Consent and Keep local are enforced in `build_provider` (spec §2 item 1; `askModel.ts:1-4`; `AskPanel.tsx:33-38`).
- Nothing is written to the note without a click on Insert (DECISIONS.md M4; `AskPanel.tsx:707-725`).
- **Runs on writes the global provider.** The helper says it also drives Enhance, cleanup and action items (`RUNS_ON_HELPER`, `askModel.ts:164`). A cloud pick asks for consent before anything is saved (`AskPanel.tsx:391-401`).
- **Stale-write rules.** Runs on re-reads the saved settings before every write, and writes nothing if they can't be read (`AskPanel.tsx:378-389`). A model pick never switches the provider behind your back: if Settings moved off Anthropic, it asks "Switch to Anthropic?" first (`:403-425`). A re-read that started before the last write is ignored (`writes` counter, `:130-146`). Runs on is disabled while a question runs (`:630`, `disabled={busy}`).
- An error names the provider the question went to, read alongside the question, not whatever Runs on shows later (`AskPanel.tsx:155-158`, `:176-182`).
- A reply for a note you've left is dropped (`seq`, `AskPanel.tsx:119-128`, `:170`, `:177`). Double Enter sends once (`inFlight` ref, `:115-117`, `:150-151`).
- Answers render without raw HTML and without remote images, and links open externally (THREAT_MODEL T1/T2; `ASK_MD`, `AskPanel.tsx:40`, `:694`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest | `frontend/src/components/AskPanel.test.tsx` | 25 | composer open/focus, quick prompts, Enter / Shift+Enter, single send, Insert-only writes, error mapping + retry, consent "Review and allow" (a decline records nothing and doesn't re-ask; an allow, recorded by the stand-in backend, re-asks), stale reply dropped, Esc layering, Runs on (lists providers, consent before cloud, declining consent keeps the sheet and focus, local without asking, stale-sheet rules, failed save, unreadable settings, pointer re-read, disabled while busy, kept-local warning, remote Ollama, pin offer) |
| vitest | `frontend/src/askModel.test.ts` | 10 | `describeAskError` table (one `it.each`, 15 cases), raw fallback, consent action only for consent, local-only "not downloaded", 5xx before connection, `runsOnPlace`, copy |
| vitest | `frontend/src/components/Editor.ask.test.tsx` | 3 | pin + remembered `ui.ask.pinned` (devMock), Esc from the title, dock click-through (class names only) |

**Known gaps:**
- **No cross-check against the Rust strings.** `describeAskError` keys on prose that lives in `llm_commands.rs`, `ollama.rs` and `fly-llm`. The tests (and devMock) use hand copies, so a Rust wording change silently drops Ask to "Ask couldn't answer" (and loses the consent button). CI path gating makes it worse: a Rust-only PR never runs vitest, and a frontend-only PR never runs cargo.
- `CLAUDE_MODELS` vs `ANTHROPIC_MODELS` vs the Rust model ids: no test ties them together (`askModel.test.ts:167-169` asserts the TS list against itself).
- Ask answers containing remote images, `javascript:` links or raw HTML are never rendered in a test (see [ui-editor](ui-editor.md)).
- No test of the Editor wiring of `keptLocal` (the chip's privacy state → the sheet's warning). `AskPanel.test.tsx` passes the prop directly.
- `Editor.ask.test.tsx` "the dock lets clicks through" checks Tailwind class strings. jsdom applies no CSS and does no hit-testing, so it can't see blocked clicks.

## Changing it safely
- Run `npm --prefix frontend test` and lint.
- When a backend error string changes (`llm_commands.rs`, `ollama.rs`, `fly-llm` `LlmError` / `transport_error`), update `describeAskError`, `askModel.test.ts` and devMock's `MOCK_ASK_ERRORS` in the same PR. Never reword `CONSENT_REFUSAL_MARK` without updating the TS match.
- Adding a Claude model: update `CLAUDE_MODELS`, `SettingsModal.tsx` `ANTHROPIC_MODELS`, `fly-llm/src/anthropic.rs` and `fly-core` prompt profiles together.
- Keep every send behind `ask_meeting`. Don't add a client-side LLM call or a second provider path (spec §2 item 1).
- Runs on must keep the re-read-before-write rule and the consent-before-save order. `AskPanel.test.tsx` "a stale sheet never undoes…" pins both.
