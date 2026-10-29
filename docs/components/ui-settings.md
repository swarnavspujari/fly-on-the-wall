# Settings (`ui-settings`)

> Layer: frontend feature. Described at main `82b7edf` (v2.4.0).
> In flight: #86 (merged) added a dormant Parakeet ASR engine behind the internal `asr.engine` flag, with no Settings row. A user-facing engine choice would land in the Transcripts section. PR #92: none.

## Purpose
The full-window Settings overlay: eight sections (Recording, Transcripts, Ask and Enhance, Calendars, Your files and privacy, Other AI apps, Connectors (a Beta pill in its heading), General). The Connectors section body is its own component, documented in [ui-connectors](ui-connectors.md). It has search, scroll-spy and a "Where your data goes" summary built from the **saved** settings. Transcript settings are a draft that saves all-or-nothing (`set_asr_settings`) from a save bar. Every other control applies at once. Any control that sends meeting content to a cloud provider asks for consent first.

## Files it owns
- `frontend/src/components/SettingsModal.tsx` — the overlay (2,328 lines: every section, the LLM / Ollama / calendar / template / MCP / update state).
- `frontend/src/settingsModel.ts` — pure helpers: sections and search, scroll-spy, the ASR draft ⇄ payload, data tiles, provider labels, MCP snippet parsing.
- `frontend/src/components/PrivacySection.tsx` — disk-encryption notice, audio retention, "Delete all my data".
- `frontend/src/privacy.ts` — pure privacy helpers and constants (shared with the editor's Keep-local chip and the restart screen).
- Used here, owned elsewhere: `ConnectorsSection.tsx` (the Connectors section body, [ui-connectors](ui-connectors.md)), `RemindNoticeRow.tsx` (`ui-recording-bar` key), `RestartRequired.tsx` (`ui-app-shell`), `CloudConsent.tsx` / `confirm.tsx` ([ui-confirm-consent](ui-confirm-consent.md)).

## Surface area
### Public API
| Export | File | Notes |
|---|---|---|
| `SettingsModal` (default) | `SettingsModal.tsx:213` | Props `modelProgress`, `updater`, `recordingActive`, `appVersion`, `initialFocus: SettingsFocus \| null` (`"engine" \| "groq" \| "llm-key"`, `settingsModel.ts`; `"llm-key"` scrolls to Ask and Enhance › Runs on, flashes it and focuses the API key field), `onClose` (`:61-71`). |
| `PrivacySection` (default) | `PrivacySection.tsx:25` | `recordingActive`, `children` (caller rows go between the notice and retention). |
| `SETTINGS_SECTIONS`, `matchSections`, `activeSectionAt` | `settingsModel.ts:18-89` | |
| `asrDraftFrom`, `asrUpdateFrom`, `isAsrDirty`, `defaultModelForTier`, `effectiveModel`, `qualityHelper`, `madeOnHelper` | `settingsModel.ts:109-197` | `asrUpdateFrom` sends `groq_key` only when typed, because an empty string would delete the saved key (`:125-145`). `defaultModelForTier` mirrors `src-tauri/src/hw.rs:130-140`. |
| `providerLabel`, `providerVendor` | `settingsModel.ts:201-216` | `providerVendor` mirrors Rust `provider_vendor` (`src-tauri/src/llm_commands.rs:122-129`). |
| `dataTiles` | `settingsModel.ts:230-259` | From saved settings, never a draft. |
| `mcpCommand`, `claudeCodeCommand` | `settingsModel.ts:265-283` | |
| `WIPE_CONFIRMATION`, `canConfirmWipe`, `retentionOptions`, `parseRetention`, `retentionWarning`, `showEncryptionBanner`, `encryptionBannerText`, `formatBytes`, `wipeOutcome`, `isKeptLocal`, `keepLocalTitle`, `recordingNoticeText`, `ENCRYPTION_BANNER_DISMISSED_KEY`, `PRIVACY_CHANGED_EVENT`, `RESTART_REQUIRED_EVENT`, `RESTART_FAILED_TEXT` | `privacy.ts:8-134` | |

**Every control → what it writes**
| Section | Control | Writes (command / key) | Gate | Code |
|---|---|---|---|---|
| Recording | Microphone | `set_app_setting("capture.mic_device_id")` | none (rejection unhandled) | `SettingsModal.tsx:1379-1395` |
| Recording | Transcribe when I stop | `set_asr_settings` (saved snapshot + `auto_transcribe`; the draft stays unsaved) | disabled while any ASR write runs | `:477-492`, `:1403-1408` |
| Recording | Remind me to tell people | `set_app_setting("ui.recording.remind_notice")` | none | `RemindNoticeRow.tsx:26-33` |
| Transcripts | Quality, Made on, Groq API key, Model, Maximum quality, Use the graphics card | draft only, then the save bar (or the close prompt's Save) → one `set_asr_settings` (Groq key to the keychain only when typed) | Discard / Keep editing / Save prompt on close | `:1517-1554`, `:1451-1504`, `:907-930`, `:496-517`, `:2259-2320` |
| Transcripts | Test again (GPU) | `set_asr_settings` twice (`use_gpu` false, then true) | none | `:521-535`, `:1492` |
| Transcripts | Install engine / Download model | `download_model(id)` | none | `:537-553`, `:976`, `:1030` |
| Ask and Enhance | Runs on | consent → `set_llm_settings{provider, model, base_url, api_key: null}` | `useCloudConsent` | `:667-699`, `:1676-1681` |
| Ask and Enhance | Test | consent → `set_llm_settings` (with a typed key) → `test_llm_connection` | `useCloudConsent` | `:779-800`, `:1694` |
| Ask and Enhance | API key Save / Update | consent → `set_llm_settings` with `api_key` (keychain) | `useCloudConsent` | `:701-740`, `:1707-1727` |
| Ask and Enhance | Claude model | consent → `set_llm_settings{provider: "anthropic", model}` | `useCloudConsent` | `:746-777`, `:1754-1761` |
| Ask and Enhance › More | Model name, Base URL, Save | consent → `set_llm_settings`. While the Base URL points a saved key at another server (`serverChangeNeedsKey`) and no key is typed, a hint asks for the key again; the backend refuses the save without it. A save or Test that moves a cloud provider to another server (`serverChanged`) asks for consent again right away (the backend cleared it; the dialog names the server); a decline leaves it saved but unused | `useCloudConsent` (local providers pass without asking) | `:1572-1620` |
| Ask and Enhance | Local model Install / Download / swap offer | `download_model("ollama-bin")`, `ollama_pull` | none | `:593-640`, `:1794`, `:1806`, `:1841` |
| Ask and Enhance | Delete a local model (list or swap offer) | `ollama_delete` | in-app confirm, danger tone | `:642-660`, `:1639`, `:1865` |
| Ask and Enhance | Note templates Save / Delete | `save_template` / `delete_template` | **no confirm on Delete**; no error handling | `:802-812`, `:1954`, `:1992` |
| Calendars | Connect / Disconnect | `set_calendar_settings` then `connect_calendar` / `disconnect_calendar` | user action (browser sign-in) | `:848-881`, `:1112-1129` |
| Calendars | Show `<calendar>` in Up next | `set_calendar_enabled` (optimistic, reverts on failure) | none | `:816-831`, `:1153-1156` |
| Calendars › More | Your own OAuth app Save | `set_calendar_settings` (client secret only when typed) | none | `:833-846`, `:2030-2068` |
| Files and privacy | Disk-encryption Dismiss | `set_app_setting("ui.privacy.disk_encryption_banner_dismissed")` | none | `PrivacySection.tsx:160-172` |
| Files and privacy | Show folder | `reveal_data_dir` | none | `SettingsModal.tsx:2093` |
| Files and privacy | Search index (Extract from N meetings) | consent → `backfill_meeting_items`; all failed → "Couldn't extract from …" with the backend's first error under it | `useCloudConsent` | `:555-589`, `:2107` |
| Files and privacy | Delete audio after | `preview_audio_retention`, then `set_audio_retention(days \| null)`; a change that deletes now shows a confirm card first | in-card confirm | `PrivacySection.tsx:66-105`, `:182-237` |
| Files and privacy | Delete all… | `delete_all_data(includeModels, "delete everything")`, then `localStorage.clear()` and a `fotw:restart-required` event | checkbox + typed phrase; disabled while recording | `PrivacySection.tsx:107-130`, `:249-341` |
| Files and privacy | Dismiss the last wipe report | `dismiss_wipe_report` | none | `PrivacySection.tsx:291-300` |
| Other AI apps | Set up / Copy snippet | nothing (reads `mcp_config`; Copy uses `navigator.clipboard.writeText`, no error path) | — | `:164-211`, `:2136-2214` |
| Connectors | Let Ask use connectors (beta), and everything below it | `set_app_setting("ui.beta.connectors")`; the rest goes through the connector commands | trust grants in the backend's native dialogs | `ConnectorsSection.tsx`; see [ui-connectors](ui-connectors.md#tauri-commands) |
| General | Open logs folder | `reveal_logs_dir` | none | `:2228` |
| General | Appearance | localStorage `fotw-theme` (`useTheme().setTheme`) | none | `:2239-2242`; `theme.ts:12`, `:55` |
| General | Check now / Download and install / Restart now | tauri-plugin-updater / plugin-process through the `updater` prop (no app command) | install and restart disabled while recording | `:1165-1245` |

Read on open: `get_asr_settings`, `get_llm_settings`, `ollama_status`, `list_templates`, `get_calendar_settings`, `list_calendars`, `mcp_config`, `list_mic_devices`, `app_info`, `get_app_setting("capture.mic_device_id")`, `pending_backfill_count` (`SettingsModal.tsx:368-387`). PrivacySection adds `get_privacy_settings`, `disk_encryption_status` and the banner key (`PrivacySection.tsx:49-62`).

### Tauri commands
- Calls: `get_asr_settings`, `set_asr_settings`, `download_model`, `get_llm_settings`, `set_llm_settings`, `test_llm_connection`, `backfill_meeting_items`, `pending_backfill_count`, `ollama_status`, `ollama_pull`, `ollama_delete`, `list_templates`, `save_template`, `delete_template`, `get_calendar_settings`, `set_calendar_settings`, `connect_calendar`, `disconnect_calendar`, `list_calendars`, `set_calendar_enabled`, `mcp_config`, `list_mic_devices`, `app_info`, `reveal_data_dir`, `reveal_logs_dir`, `get_app_setting`, `set_app_setting`, `request_cloud_consent` (through `useCloudConsent`), `get_privacy_settings`, `disk_encryption_status`, `preview_audio_retention`, `set_audio_retention`, `delete_all_data`, `dismiss_wipe_report`.
- Backend owners: `app-pipeline` (ASR settings), `app-models-gpu`, `app-llm`, `app-ollama`, `app-calendar`, `app-mcp-sidecar`, `app-shell`, `app-settings-consent`, `app-privacy`.
- devMock mirrors every one (stateful for the session). Drift: `set_app_setting` / `get_app_setting` accept any key, except that `set_app_setting` refuses `consent.cloud_llm.*` like the backend (`devMock.ts:1365-1380`; the backend allowlists are `commands.rs:59-96`). `request_cloud_consent` asks with the browser's `window.confirm`, its stand-in for the native dialog (`devMock.ts:1007-1022`). `test_llm_connection` only emulates the consent refusal (`devMock.ts:1330-1334`). `set_llm_settings` does no base-URL validation (the backend's T4 https check, `devMock.ts:1255-1269`). `mcp_config` is a fixed Windows snippet (`devMock.ts:855-862`).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `fotw:restart-required` (window `CustomEvent`) | Emits after `delete_all_data` (`PrivacySection.tsx:120-124`). Settings listens and closes (`SettingsModal.tsx:411-415`). | `{report: WipeReport, includeModels}` | `RestartRequired.tsx` (app shell) |
| `fotw:llm-settings-changed` (window) | Emitted by `api.setLlmSettings`, and by `useCloudConsent` after the backend records consent | none | skipped-cleanup notice |
| `model:progress` (Tauri) | read through the `modelProgress` prop (App listens) | `ModelProgress` | `download_model`, `ollama_pull` |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_writable_setting_key`) | Meaning |
|---|---|---|---|
| `capture.mic_device_id` | read + write | Yes (`capture.`) | Chosen microphone ("" = system default). |
| `ui.recording.remind_notice` | read + write (`RemindNoticeRow.tsx`) | Yes (`ui.`) | Record-start reminder on/off (`recordingBar.ts:9`). |
| `ui.privacy.disk_encryption_banner_dismissed` | read + write | Yes (`ui.`) | Banner dismissed (`privacy.ts:8`). |
| `consent.cloud_llm.<provider>` | read via `useCloudConsent`; recorded only by the backend's `request_cloud_consent` | No: read-only to the webview (`is_webview_writable_setting_key`) | See [ui-confirm-consent](ui-confirm-consent.md). |
| ASR, LLM, calendar, retention settings | via dedicated commands | No (dedicated, validating commands) | spec §2 item 2 |

localStorage keys: `fotw-theme` (written by Appearance); the whole store is cleared after "Delete all my data" (`PrivacySection.tsx:113-117`). devMock toggles that change this screen: `fotwMockConsentReset`, `fotwMockOllama`, `fotwMockGpuBench`, `fotwMockEncryption`, `fotwMockUpdate`, `fotwMockEngineMissing`, `fotwMockEngineUnmanaged`, `fotwMockCalendar`.

### Database
None directly. Every write is a command above (settings, templates, retention, wipe).

### Files and directories written
None from the webview. Through commands: model and engine downloads (`download_model`), Ollama models (`ollama_pull` / `ollama_delete`), audio deletion (`set_audio_retention`), the full wipe of `<data_dir>` (`delete_all_data`).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| Cloud LLM vendor (or a custom base URL) | Test, Search index | consent (UI + `build_provider`); Keep local applies to backfill per meeting (backend) |
| Model / engine CDN, Ollama registry | Install / Download / pull | user action |
| Google / Microsoft OAuth | Connect | user action in the system browser |
| Update server | Check now / Download | user action (plus the app's startup check, outside this doc) |

### External processes
None from the webview. The backend may run Ollama, open the browser for OAuth, or open Explorer / Finder (`reveal_*`).

### Secrets and keychain entries
Entered here and passed to commands that store them in the keychain: the Groq API key (`set_asr_settings.groq_key`), LLM API keys (`set_llm_settings.api_key`), the Google client secret (`set_calendar_settings.google_client_secret`). Inputs are `type="password"` and cleared after a successful save (`SettingsModal.tsx:502`, `:719`, `:790`, `:840`).

## Dependencies
- **Inbound:** `App.tsx` (opens Settings, the deep links from `TranscriptPanel` via `onOpenSettings` and from the skipped-cleanup notice's "Add a key", the `updater` hook).
- **Outbound:** [ui-confirm-consent](ui-confirm-consent.md), `ui.tsx` (`SettingsGroup`, `SettingsRow`, `Switch`, `Segmented`, `Select`, `Input`, `Modal`, `useEscapeLayer`, `useDialogFocus`), `theme.ts`, `pipelineNotice.ts` (`briefError`), `updater.ts`, `RemindNoticeRow.tsx`, `RestartRequired.tsx` (via the event).

## Invariants
- Consent is asked before any Settings action that sends meeting content or saves a cloud provider: Runs on, Save, Update, Claude model, Test, Search index (`SettingsModal.tsx:555-560`, `:662-705`, `:750`, `:780`). The backend enforces it anyway (spec §2 item 1). The disclosure copy has one source, the backend's `cloud_consent_prompt` (`src-tauri/src/llm_commands.rs:148-176`), shown in its native dialog.
- Webview-written keys stay under `consent.` / `capture.` / `ui.`, and never `consent.cloud_llm.*`, which only the backend writes. Everything else goes through a dedicated command (spec §2 item 2; `commands.rs:59-79`).
- "Where your data goes" reflects saved settings, never the unsaved draft (`settingsModel.ts:229-230`).
- Transcript settings save all-or-nothing. Instant-apply ASR writes send the saved snapshot and never race the save bar (`SettingsModal.tsx:317-320`, `:483-484`).
- An empty Groq key is never sent (it would delete the saved one) (`settingsModel.ts:125-126`, `:143`).
- "Delete all my data" needs the checkbox **and** the typed phrase, and is disabled while recording (`privacy.ts:79-81`; `PrivacySection.tsx:260`, `:336`). The frontend always sends the constant phrase (`PrivacySection.tsx:111`), so the backend's phrase check stops only accidental calls, not a script (inferred).
- Esc closes the topmost layer only: search clears first, then the discard prompt, then Settings (`SettingsModal.tsx:392-396`, `:1293-1299`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest (devMock backend) | `frontend/src/components/SettingsModal.test.tsx` | 29 | sections / tiles / search / Esc; save bar, discard and close prompt; Transcribe when I stop; consent before a cloud pick (nothing saved while devMock's stand-in dialog is open, no `set_llm_settings` on a decline, consent recorded by the consent command, never a settings write); Ollama delete confirm; More options; deep links; Groq key dropped; key-less cloud; hidden forms kept; write locks; MCP snippet; restart hand-over; RestartRequired focus; Remind key; consent before Test / key save / Claude model / backfill (no IPC on a decline); backfill error copy; devMock's consent mirror (refuses consent writes, records only a yes) |
| vitest | `frontend/src/settingsModel.test.ts` | 18 | search, scroll-spy, ASR draft round trip, payloads, empty key, dirty check, effective model, helper copy, data tiles, MCP parsing / quoting |
| vitest | `frontend/src/privacy.test.ts` | 15 | notice text, retention options / parse / warning, encryption banner, wipe gating, keep-local helpers, bytes, wipe outcome |

**Known gaps:**
- `PrivacySection` is untested as a component: retention preview → confirm card → `set_audio_retention`, held-retention confirm, the Delete-all flow (gating, `deleteAllData` args, `localStorage.clear`, the event) and the encryption Dismiss.
- Calendar connect / disconnect / toggle, template save / delete, Ollama install / pull / swap offer, model downloads, and the mic select have no tests.
- Tests run against devMock, which accepts any setting key (apart from refusing consent writes) and doesn't validate base URLs. A wrong key prefix or an http custom endpoint would pass here and fail in the app.
- `SnippetCard` copy uses `navigator.clipboard` (absent in jsdom, permission-dependent in WebView2) with no failure path. Untested.
- "deep link 'engine'" asserts `scrollIntoView` was called on a `vi.fn` installed in `beforeAll`. `vi.restoreAllMocks()` doesn't clear its calls, so under shuffle the "groq" deep-link test can satisfy it (an order risk).
- Several tests restore devMock state by hand at the end (provider back to Anthropic, ASR tier, a deleted Ollama model is never restored). A failing assertion skips the restore and can cascade to later tests in the file.
- Consent is per provider id. With a custom base URL, content goes to that host while the disclosure names the vendor (`providerVendor`). No test or copy covers that case (inferred).

## Changing it safely
- Run `npm --prefix frontend test`, lint and `tsc`. Then check port 1420 with the devMock toggles above, and smoke-test in the real app anything that touches the keychain, OAuth, the updater or the wipe.
- A new control that sends content to the LLM provider must call `confirmCloudProvider` / `useCloudConsent` first, and must not write `consent.*` itself.
- A new webview-written key must use `consent.` / `capture.` / `ui.`. Otherwise add a dedicated command in `src-tauri` + `api.ts` + `lib.rs` + devMock.
- Keep `ANTHROPIC_MODELS` in step with `askModel.ts` `CLAUDE_MODELS` and the Rust ids. Keep `defaultModelForTier` in step with `hw.rs`, and `PROVIDER_VENDORS` with `llm_commands.rs` `provider_vendor`.
- `SettingsModal.tsx` is a god file. Prefer moving a section into its own component (as `PrivacySection` / `RemindNoticeRow` did) over adding more state to it.
- Tests that mutate devMock must restore it in `afterEach`, not at the end of the test body.
