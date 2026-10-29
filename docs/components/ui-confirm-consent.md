# In-app confirm and cloud consent (`ui-confirm-consent`)

> Layer: frontend feature. Described at main `82b7edf` (v2.4.0), plus backend-only cloud consent (#102). Line references into `CloudConsent.tsx`, `api.ts`, `devMock.ts`, `commands.rs`, `llm_commands.rs` and `lib.rs` are as of #102.
> In flight: none here. PR #92 and PR #86 don't touch this component.

## Purpose
`confirm.tsx` is the app's in-app yes/no dialog, for destructive and other questions. It replaced `window.confirm`, which tauri-plugin-dialog makes async (a Promise is always truthy, so `if (confirm(...))` never waited). `CloudConsent.tsx` is the one place the UI asks before a cloud LLM provider first gets meeting content: from Settings, Ask's Runs on, the launch check and the skipped-cleanup notice. It doesn't draw that question itself. It calls the backend's `request_cloud_consent`, which shows a native dialog with the disclosure in the backend's own words and records a yes. The backend (`build_provider`) enforces the recorded consent. The UI only reads it and asks for it.

## Files it owns
- `frontend/src/components/confirm.tsx` — `ConfirmProvider`, `useConfirm`, the dialog (`ConfirmDialog`), the 400 ms click guard.
- `frontend/src/components/confirmTesting.ts` — DOM helpers that find the open dialog and its buttons, for tests.
- `frontend/src/components/CloudConsent.tsx` — `useCloudConsent` (returns the module function `askCloudConsent`), `CloudConsentOnLaunch`, `useConsentSkips`, `cloudConsentKey`.
- `frontend/src/components/cloudConsentTesting.ts` — test stand-in for the native consent dialog: `holdConsentRequests`, `consentRequest`, `answerConsent`.
- Not here: the disclosure copy lives in the backend (`cloud_consent_prompt`, `src-tauri/src/llm_commands.rs:148-176`), with a copy in devMock (`mockConsentPrompt`, `frontend/src/devMock.ts:989-1005`) that a Rust test keeps equal. `providerName` (the skipped-cleanup notice's vendor) is in `frontend/src/askModel.ts:27-29`.
- Shared with the app shell: `LLM_SETTINGS_CHANGED_EVENT` is declared in `frontend/src/api.ts:8`, and `api.requestCloudConsent` in `api.ts:186-187`. The ESLint ban on `confirm`/`alert` is in `frontend/eslint.config.js:24-38`. The provider is mounted in `frontend/src/main.tsx:20-23`.

## Surface area
### Public API
| Export | File | What it does |
|---|---|---|
| `ConfirmProvider` | `confirm.tsx:67-120` | Renders one dialog at a time for everything inside it. A second question waits in a queue. On unmount it answers every open question "no" (`:74-80`). Mounted once in `main.tsx:20`. |
| `useConfirm(): ConfirmFn` | `confirm.tsx:31-35` | Returns `confirm(options) => Promise<boolean>`. It throws without a provider. |
| `ConfirmOptions` | `confirm.tsx:11-22` | `title`, `message` (string or node; a blank line in a string starts a new paragraph), `confirmLabel` (never "OK"), `cancelLabel`, `tone: "default" \| "danger"`. |
| `CONFIRM_CLICK_GUARD_MS = 400` | `confirm.tsx:59` | A mouse click on yes (`detail >= 1`) within 400 ms of opening is ignored, and so is the second click of a double click (`detail > 1`). Keyboard activation (`detail 0`) always counts (`:140-144`). |
| `useCloudConsent()` | `CloudConsent.tsx:33-64` | Returns `askCloudConsent(providers, id, {atLaunch}) => Promise<boolean>` (`:33-59`). A local provider gives true with no question. An id not in `providers` gives false with no question. A provider with consent already recorded (read with `get_app_setting`) gives true. Otherwise it calls `request_cloud_consent(id, atLaunch)`, and the backend asks in its native dialog. On true it fires `fotw:llm-settings-changed`. A failed request returns false. It never writes the consent itself. |
| `CloudConsentOnLaunch` | `CloudConsent.tsx:72-88` | Asks once per launch (a ref guard, StrictMode-safe) when the saved provider is cloud with no consent, with `{ atLaunch: true }`. The backend's dialog then starts with "Your AI provider is set to …" and declines with "Not now". Declining saves nothing. |
| `useConsentSkips()` | `CloudConsent.tsx:106-185` | Backs the skipped-cleanup notice in `AppNotices` (`ui-notices`). Reads `consent_skipped_meetings`, follows `llm:consent-skips`, re-reads the vendor (and whether it has a key: `keyVendor`) on `fotw:llm-settings-changed`. `run()` asks for consent and then calls `retry_consent_skipped`; with `keyVendor` set the notice offers "Add a key" instead. |
| `cloudConsentKey(id)` | `CloudConsent.tsx:16` | `consent.cloud_llm.${id}`. |
| `confirmDialog`, `confirmTitle`, `confirmChoice(answer)` | `confirmTesting.ts:5-23` | Test helpers. Cancel is the `[data-autofocus]` button in `.fly-modal-foot`; yes is the other one. |
| `holdConsentRequests(record)`, `consentRequest()`, `answerConsent(yes)` | `cloudConsentTesting.ts:17-45` | Test helpers. Spy on `api.requestCloudConsent`; each request waits until a test answers it, like the dialog. A yes calls `record(provider)`, as the backend records consent. |

Behaviour details:
- Cancel starts with focus (`data-autofocus`, `confirm.tsx:159-163`), so Enter or Space never says yes by accident. The role is `alertdialog` (`:187`).
- Tone `danger`: the yes button is a red outline with `mr-auto`, on the far side from Cancel (`:164-203`). Tone `default`: Cancel, then a primary yes.
- De-duplication: a second identical question (same title, message, labels and tone; plain-string message only) that arrives while the first is open gets the same Promise (`:46-55`, `:85-88`). `askCloudConsent` adds a module-level `inFlight` map so there is one request per provider at a time (`CloudConsent.tsx:24-28`, `:41-42`). The backend also shows one consent dialog at a time.
- Esc layer depth 1000 (`confirm.tsx:63`): Esc answers the question before it closes Settings, a sheet or a modal.
- The consent dialog is not `confirm.tsx`. The backend draws it (with `rfd`), so the Esc layers and the click guard don't apply. Its decline button is the first, default one, so Enter declines. Only a click on its allow button records consent. On Windows and macOS it's modal to the app window; on GTK (Linux, the BSDs) it isn't. After two declines for one provider within 60 s, the backend answers false for that provider with no dialog for 60 s ([app-settings-consent](app-settings-consent.md#consent-flow)).

### Tauri commands
- Calls: `get_app_setting` (consent read), `request_cloud_consent` (the backend asks and records), `get_llm_settings` (launch check, skips vendor), `consent_skipped_meetings`, `retry_consent_skipped`. Never `set_app_setting`.
- Backend owners: `app-settings-consent` for all five.
- devMock mirrors all five (`frontend/src/devMock.ts:1252-1253`, `:1335-1340`, `:1365-1372`). Its `request_cloud_consent` (`mockRequestCloudConsent`, `devMock.ts:1007-1022`) asks with the browser's own `window.confirm` as the stand-in for the native dialog, using `mockConsentPrompt`'s copy of the backend's words. It only runs in a plain browser, never under Tauri. It doesn't pace repeated declines, and it uses the launch wording whenever asked to, without checking the saved provider. Mock `set_app_setting` refuses `consent.cloud_llm.*` like the backend (`devMock.ts:1373-1378`). Drift: devMock's `set_app_setting`/`get_app_setting` otherwise accept **any** key (`devMock.ts:1365-1380`). The backend rejects keys outside `consent.` / `capture.` / `ui.` (`src-tauri/src/commands.rs:59-96`). A UI write to a non-allowlisted key passes in jsdom and port-1420 QA but fails in the app.

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `fotw:llm-settings-changed` (window `Event`) | Emits: `askCloudConsent` after `request_cloud_consent` returns true (`CloudConsent.tsx:51`); `api.setLlmSettings` after any successful `set_llm_settings` (`api.ts:9-11`, `:174-175`). Listens: `useConsentSkips` (`CloudConsent.tsx:139-143`). | none | Settings and Ask's Runs on (writers); the skipped-cleanup notice (reader) |
| `llm:consent-skips` (Tauri) | Listens: `useConsentSkips` (`CloudConsent.tsx:122`) | `string[]` meeting ids | Emitted by `src-tauri/src/scheduler.rs:229` |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_writable_setting_key`) | Meaning |
|---|---|---|---|
| `consent.cloud_llm.<provider>` | read only (`CloudConsent.tsx:45`, `:152`) | **No.** Readable (`is_webview_setting_key`, `commands.rs:59-66`), but `set_app_setting` refuses it in any letter case (`commands.rs:68-79`) | `"true"` = you allowed this cloud provider. Only the backend writes it: `record_cloud_consent`, after a click on allow in the native dialog (`llm_commands.rs:264-279`). `build_provider` reads it (`llm_commands.rs:602`). |
| `migration.cloud_consent_reset_v2_4` | not touched by UI | No | Backend flag for the one-time v2.4 reset (`llm_commands.rs:72-75`, `:93-115`, run from `lib.rs:115`). |

localStorage keys: none. devMock toggles: `fotwMockConsentReset="1"` (no consent recorded, so the launch check asks), `fotwMockConsentSkips="1"` (the skipped-cleanup notice shows). Test-only export `resetMockAppSettings()` (`devMock.ts:877`).

### Database
None directly. Consent is a row in the backend settings table, written by the backend's `request_cloud_consent` (owned by `fly-storage` / `app-settings-consent`).

### Files and directories written
None.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None from this component | It only asks for consent. Calls that send content go through `build_provider`. | — |

### External processes
None.

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `main.tsx` (provider + launch check). `useConfirm` is used by `TranscriptPanel` (re-run), `SettingsModal` (delete an Ollama model), `AskPanel` RunsOn (switch back to Anthropic), and the notes library / sidebar deletes (outside this doc). `useCloudConsent` is used by `SettingsModal` (`confirmCloudProvider`, `SettingsModal.tsx:664-665`), `AskPanel` (RunsOn `:333`, `AskSheet` "Review and allow" `:564`, `:589-594`), `CloudConsentOnLaunch`, and `useConsentSkips` (via `AppNotices`).
- **Outbound:** `ui.tsx` (`Modal`, `Button`, `EscapeLayerScope`, the Esc layer stack, `useDialogFocus`), `api.ts` (including `requestCloudConsent`), `askModel.ts` (`providerName`), `@tauri-apps/api/event` (`listen`); the backend's native consent dialog ([app-settings-consent](app-settings-consent.md)).

## Invariants
- Cloud content needs recorded consent per provider, and the backend enforces it (spec §2 item 1; `check_cloud_policy` at `src-tauri/src/llm_commands.rs:541-566`, consent read at `:602`). The UI is not the gate.
- Consent is written only by the backend, and only after a click on the native dialog's allow button (`record_cloud_consent`, `llm_commands.rs:264-279`). The webview can't write it: `set_app_setting` refuses `consent.cloud_llm.*` (`commands.rs:68-79`), and the UI never tries. Anything but a click on allow means no: the decline button, Enter, Esc, closing the dialog (`llm_commands.rs:313-351`). The words are built in Rust from the provider id, so no script can change what the user agrees to (THREAT_MODEL T10).
- No browser dialogs in the webview: `confirm`/`alert` are banned by lint (`frontend/eslint.config.js:24-38`). The one exception is devMock's browser-only consent stand-in (`devMock.ts:1018-1019`, behind an `eslint-disable`). `window.prompt` is **not** banned and is still used twice (see `ui-editor`). The consent dialog is native too, but the backend shows it, not the webview.
- One question at a time, and a duplicate question shares one answer (`confirm.tsx:65-66`, `:82-97`). For consent: one request per provider in the UI (`inFlight`), and one native dialog at a time in the backend (`AppState::consent_prompt`).
- The launch check asks at most once per launch and never on a local provider (`CloudConsent.tsx:72-87`). Since the v2.4 reset (`llm_commands.rs:77-119`), every consent recorded before v2.4 is gone, so cloud users are asked again.
- The disclosure copy is single-sourced in the backend (`cloud_consent_prompt`, `llm_commands.rs:148-176`), so Settings, Ask, the launch check and the notice show the same words. devMock's `mockConsentPrompt` is a copy for its browser stand-in, kept equal by the Rust test `dev_mock_shows_the_same_disclosure`. `SettingsModal`'s `confirmCloudProvider` is only a wrapper over `useCloudConsent`.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest | `frontend/src/components/confirm.test.tsx` | 12 | yes waits; every "no" path; Cancel focused; danger layout; paragraph split; 400 ms / double-click guard (the only test with `detail >= 1`); de-dup; queue order; Esc layering; unmount → no; needs provider |
| vitest | `frontend/src/components/CloudConsent.test.tsx` | 7 | launch check asks the backend once under StrictMode with `atLaunch: true`, a decline records nothing, a yes is left to the backend and fires the change event, local/allowed/unreadable stay quiet, unknown id refused without asking, one request per provider, a failed request counts as no; `setAppSetting` is never called |
| vitest (other docs) | `AskPanel.test.tsx`, `SettingsModal.test.tsx`, `AppNotices.test.tsx` | — | Consent from Runs on, "Review and allow", Settings Test/Save/model/backfill, the skipped-cleanup notice. AskPanel and AppNotices answer through `cloudConsentTesting.ts`. SettingsModal runs on devMock and answers its `window.confirm`, and checks that devMock refuses consent writes and records only a yes |
| unit (Rust, other docs) | `src-tauri/src/llm_commands.rs`, `src-tauri/src/commands.rs` | — | The dialog's words, when it asks, the per-platform result mapping, pacing, only an allow records, devMock disclosure parity; `set_app_setting`'s key check refuses `consent.cloud_llm.*` (see [app-settings-consent](app-settings-consent.md#tests)) |

**Known gaps:**
- Integration tests that answer an in-app question use `confirmChoice(x).click()`. In jsdom that is a `PointerEvent` with `detail 0`, so it takes the keyboard path and skips the 400 ms guard. Only `confirm.test.tsx` "ignores a mouse click…" exercises mouse clicks.
- No webview test can see the native consent dialog. Frontend tests stand in for it with `cloudConsentTesting.ts` or devMock's `window.confirm`, so its buttons, default button and Esc handling are covered only by the Rust result-mapping test. Nothing checks the real `is_webview_writable_setting_key` from the UI side (devMock copies its prefix check).
- `useConsentSkips` is tested only through `AppNotices.test.tsx`. The `llm:consent-skips` listener and unlisten-on-unmount race (`CloudConsent.tsx:122-131`) have no direct test.
- A failed `request_cloud_consent` is tested to return false, but not that it fires no event (`CloudConsent.tsx:49-55`).
- Module state (`inFlight` map, `CloudConsent.tsx:28`) is shared by every test in a file. A request left unanswered by a failing test stays in it, and the next ask for the same provider would wait on that request, which never settles (`holdConsentRequests` drops unanswered requests).

## Changing it safely
- Run `npm --prefix frontend test` (vitest + jsdom) and `npm --prefix frontend run lint`. The lint rule is the only guard against reintroducing `confirm`/`alert`.
- Never call `window.confirm` / `alert` (and avoid `window.prompt`). Ask with `await confirm({...})`. The yes label says what it does. devMock's consent stand-in is the one exception.
- Ask for cloud consent only through `useCloudConsent` → `request_cloud_consent`. Don't write `consent.cloud_llm.*` with `setAppSetting` (the backend and devMock refuse it), and don't ask for it with `useConfirm`.
- The disclosure lives in `cloud_consent_prompt` (Rust). Change it there and in devMock's `mockConsentPrompt` together; the Rust tests `consent_dialog_says_what_the_in_app_prompt_said` and `dev_mock_shows_the_same_disclosure` fail otherwise.
- If you add a key that the UI writes through `set_app_setting`, keep it under `consent.` / `capture.` / `ui.`, and not under `consent.cloud_llm.`. devMock will not catch a wrong prefix.
- Mount nothing that asks outside `<ConfirmProvider>` (`useConfirm` throws).
