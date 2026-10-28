# In-app confirm and cloud consent (`ui-confirm-consent`)

> Layer: frontend feature. Described at main `82b7edf` (v2.4.0).
> In flight: the consent hardening (#102, open) targets `CloudConsent.tsx`, the settings allowlist in `commands.rs` and `llm_commands.rs`. Today any script in the webview can write `consent.cloud_llm.<id>` through `set_app_setting`; that write path is what changes. PR #92 and PR #86 don't touch this component.

## Purpose
`confirm.tsx` is the app's only yes/no dialog. It replaced `window.confirm`, which tauri-plugin-dialog makes async (a Promise is always truthy, so `if (confirm(...))` never waited). `CloudConsent.tsx` is the one place the UI asks before a cloud LLM provider first gets meeting content: from Settings, Ask's Runs on, the launch check and the skipped-cleanup notice. The backend (`build_provider`) enforces the recorded consent. The UI only asks for it and records it.

## Files it owns
- `frontend/src/components/confirm.tsx` — `ConfirmProvider`, `useConfirm`, the dialog (`ConfirmDialog`), the 400 ms click guard.
- `frontend/src/components/confirmTesting.ts` — DOM helpers that find the open dialog and its buttons, for tests.
- `frontend/src/components/CloudConsent.tsx` — `useCloudConsent`, `CloudConsentOnLaunch`, `useConsentSkips`, `cloudConsentKey`.
- Shared with `ui-ask`: the disclosure copy `cloudConsentCopy` / `cloudConsentLaunchIntro` live in `frontend/src/askModel.ts:34-56`.
- Shared with the app shell: `LLM_SETTINGS_CHANGED_EVENT` is declared in `frontend/src/api.ts:8`. The ESLint ban on `confirm`/`alert` is in `frontend/eslint.config.js:18-52`. The provider is mounted in `frontend/src/main.tsx:20-23`.

## Surface area
### Public API
| Export | File | What it does |
|---|---|---|
| `ConfirmProvider` | `confirm.tsx:67-120` | Renders one dialog at a time for everything inside it. A second question waits in a queue. On unmount it answers every open question "no" (`:74-80`). Mounted once in `main.tsx:20`. |
| `useConfirm(): ConfirmFn` | `confirm.tsx:31-35` | Returns `confirm(options) => Promise<boolean>`. It throws without a provider. |
| `ConfirmOptions` | `confirm.tsx:11-22` | `title`, `message` (string or node; a blank line in a string starts a new paragraph), `confirmLabel` (never "OK"), `cancelLabel`, `tone: "default" \| "danger"`. |
| `CONFIRM_CLICK_GUARD_MS = 400` | `confirm.tsx:59` | A mouse click on yes (`detail >= 1`) within 400 ms of opening is ignored, and so is the second click of a double click (`detail > 1`). Keyboard activation (`detail 0`) always counts (`:140-144`). |
| `useCloudConsent()` | `CloudConsent.tsx:32-73` | Returns `ask(providers, id, {intro, cancelLabel}) => Promise<boolean>`. A local provider gives true with no question. An id not in `providers` gives false with no question. A provider with consent already recorded gives true. Otherwise it shows the disclosure. Only an explicit yes writes `consent.cloud_llm.<id>` = `"true"` and fires `fotw:llm-settings-changed`. A failed write returns false. |
| `CloudConsentOnLaunch` | `CloudConsent.tsx:81-100` | Asks once per launch (a ref guard, StrictMode-safe) when the saved provider is cloud with no consent. Cancel reads "Not now" and saves nothing. |
| `useConsentSkips()` | `CloudConsent.tsx:118-197` | Backs the skipped-cleanup notice in `AppNotices` (`ui-notices`). Reads `consent_skipped_meetings`, follows `llm:consent-skips`, re-reads the vendor on `fotw:llm-settings-changed`. `run()` asks for consent and then calls `retry_consent_skipped`. |
| `cloudConsentKey(id)` | `CloudConsent.tsx:15` | `consent.cloud_llm.${id}`. |
| `confirmDialog`, `confirmTitle`, `confirmChoice(answer)` | `confirmTesting.ts:5-23` | Test helpers. Cancel is the `[data-autofocus]` button in `.fly-modal-foot`; yes is the other one. |

Behaviour details:
- Cancel starts with focus (`data-autofocus`, `confirm.tsx:159-163`), so Enter or Space never says yes by accident. The role is `alertdialog` (`:187`).
- Tone `danger`: the yes button is a red outline with `mr-auto`, on the far side from Cancel (`:164-203`). Tone `default`: Cancel, then a primary yes.
- De-duplication: a second identical question (same title, message, labels and tone; plain-string message only) that arrives while the first is open gets the same Promise (`:46-55`, `:85-88`). `useCloudConsent` adds a module-level `inFlight` map so there is one question per provider at a time (`CloudConsent.tsx:23-26`, `:43-44`).
- Esc layer depth 1000 (`confirm.tsx:63`): Esc answers the question before it closes Settings, a sheet or a modal.

### Tauri commands
- Calls: `get_app_setting`, `set_app_setting` (consent read/write), `get_llm_settings` (launch check, skips vendor), `consent_skipped_meetings`, `retry_consent_skipped`.
- Backend owners: `app-settings-consent` for all five.
- devMock mirrors all five (`frontend/src/devMock.ts:1298-1300`, `:1326-1336`). Drift: devMock's `set_app_setting`/`get_app_setting` accept **any** key (`devMock.ts:1326-1336`). The backend rejects keys outside `consent.` / `capture.` / `ui.` (`src-tauri/src/commands.rs:59-74`). A UI write to a non-allowlisted key passes in jsdom and port-1420 QA but fails in the app.

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `fotw:llm-settings-changed` (window `Event`) | Emits: `useCloudConsent` after a consent write (`CloudConsent.tsx:62`); `api.setLlmSettings` after any successful `set_llm_settings` (`api.ts:9-11`, `:174-175`). Listens: `useConsentSkips` (`CloudConsent.tsx:151-155`). | none | Settings and Ask's Runs on (writers); the skipped-cleanup notice (reader) |
| `llm:consent-skips` (Tauri) | Listens: `useConsentSkips` (`CloudConsent.tsx:134`) | `string[]` meeting ids | Emitted by `src-tauri/src/scheduler.rs:229` |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `consent.cloud_llm.<provider>` | read + write (`CloudConsent.tsx:48`, `:61`, `:164`) | **Yes** (`consent.` prefix; `commands.rs:59-66`; asserted by `llm_commands.rs:1006`) | `"true"` = you allowed this cloud provider. `build_provider` reads it (`llm_commands.rs:290`). Any webview script can set it today, bypassing the prompt (the consent hardening (#102) fixes this). |
| `migration.cloud_consent_reset_v2_4` | not touched by UI | No | Backend flag for the one-time v2.4 reset (`llm_commands.rs:61-64`, `:82-104`, run from `lib.rs:115`). |

localStorage keys: none. devMock toggles: `fotwMockConsentReset="1"` (no consent recorded, so the launch check asks), `fotwMockConsentSkips="1"` (the skipped-cleanup notice shows). Test-only export `resetMockAppSettings()` (`devMock.ts:877`).

### Database
None directly. Consent is a row in the backend settings table, written via `set_app_setting` (owned by `fly-storage` / `app-settings-consent`).

### Files and directories written
None.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None from this component | It only records consent. Calls that send content go through `build_provider`. | — |

### External processes
None.

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `main.tsx` (provider + launch check). `useConfirm` is used by `TranscriptPanel` (re-run), `SettingsModal` (delete an Ollama model), `AskPanel` RunsOn (switch back to Anthropic), `useCloudConsent`, and the notes library / sidebar deletes (outside this doc). `useCloudConsent` is used by `SettingsModal` (`confirmCloudProvider`, `SettingsModal.tsx:664-665`), `AskPanel` (RunsOn `:333`, `AskSheet` "Review and allow" `:564`, `:589-594`), `CloudConsentOnLaunch`, and `useConsentSkips` (via `AppNotices`).
- **Outbound:** `ui.tsx` (`Modal`, `Button`, `EscapeLayerScope`, the Esc layer stack, `useDialogFocus`), `api.ts`, `askModel.ts` (copy, `providerName`), `@tauri-apps/api/event` (`listen`).

## Invariants
- Cloud content needs recorded consent per provider, and the backend enforces it (spec §2 item 1; `check_cloud_policy` at `src-tauri/src/llm_commands.rs:228-254`, consent read at `:290`). The UI is not the gate.
- Consent is written only after an explicit yes. Esc, the backdrop, the close button and Cancel all mean no (`confirm.tsx:24-26`, `:100-104`; `CloudConsent.tsx:59`). Not enforceable against a hostile webview while `consent.` stays webview-settable (`docs/SECURITY_AUDIT_2026-10.md` line 106; spec §2 item 2).
- No native dialogs: `confirm`/`alert` are banned by lint (`frontend/eslint.config.js:37-51`). `window.prompt` is **not** banned and is still used twice (see `ui-editor`).
- One question at a time, and a duplicate question shares one answer (`confirm.tsx:65-66`, `:82-97`).
- The launch check asks at most once per launch and never on a local provider (`CloudConsent.tsx:81-99`). Since the v2.4 reset (`llm_commands.rs:66-104`), every earlier consent is gone, so cloud users are asked again.
- The disclosure copy is single-sourced in `askModel.ts:34-50` (`cloudConsentCopy`). The old duplicate (`cloudConsentMessage`) was removed in PR #99. `SettingsModal`'s `confirmCloudProvider` is only a wrapper over `useCloudConsent`.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest | `frontend/src/components/confirm.test.tsx` | 12 | yes waits; every "no" path; Cancel focused; danger layout; paragraph split; 400 ms / double-click guard (the only test with `detail >= 1`); de-dup; queue order; Esc layering; unmount → no; needs provider |
| vitest | `frontend/src/components/CloudConsent.test.tsx` | 6 | launch check asks once under StrictMode, "Not now" writes nothing, yes writes once, local/allowed/unreadable stay quiet, unknown id refused, one question per provider |
| vitest (other docs) | `AskPanel.test.tsx`, `SettingsModal.test.tsx`, `AppNotices.test.tsx` | — | Consent from Runs on, "Review and allow", Settings Test/Save/model/backfill, the skipped-cleanup notice |

**Known gaps:**
- Every integration test answers with `confirmChoice(x).click()`. In jsdom that is a `PointerEvent` with `detail 0`, so it takes the keyboard path and skips the 400 ms guard. Only `confirm.test.tsx` "ignores a mouse click…" exercises mouse clicks.
- Consent tests mock `api.setAppSetting` or use devMock. Nothing shows that the backend refuses a forged `consent.cloud_llm.*` (it doesn't today), and nothing checks the real `is_webview_setting_key` from the UI side.
- `useConsentSkips` is tested only through `AppNotices.test.tsx`. The `llm:consent-skips` listener and unlisten-on-unmount race (`CloudConsent.tsx:134-143`) have no direct test.
- No test that a failed consent write (`setAppSetting` rejects) returns false and fires no event (`CloudConsent.tsx:60-66`).
- Module state (`inFlight` map, `CloudConsent.tsx:26`) is shared by every test in a file. A question left open by a failing test would be handed to the next ask for the same provider (an order risk; today `ConfirmProvider` unmount answers it "no").

## Changing it safely
- Run `npm --prefix frontend test` (vitest + jsdom) and `npm --prefix frontend run lint`. The lint rule is the only guard against reintroducing `confirm`/`alert`.
- Never call `window.confirm` / `alert` (and avoid `window.prompt`). Ask with `await confirm({...})`. The yes label says what it does.
- When the consent-hardening work lands: move the consent write to a dedicated backend command, drop `consent.cloud_llm.` from the webview allowlist (`commands.rs:59-66`), flip the assertion at `llm_commands.rs:1006`, update `useCloudConsent` (`CloudConsent.tsx:46-66`), mirror the new command in `devMock.ts` (spec §2 item 4), and rewrite the consent tests that spy on `api.setAppSetting`.
- Keep the copy in `askModel.ts` only. Settings, Ask and the launch check must show the same words.
- If you add a key that the UI writes through `set_app_setting`, keep it under `consent.` / `capture.` / `ui.`. devMock will not catch a wrong prefix.
- Mount nothing that asks outside `<ConfirmProvider>` (`useConfirm` throws).
