# Settings and cloud consent (`app-settings-consent`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0), plus backend-only cloud consent (#102). Line references into `commands.rs`, `llm_commands.rs`, `state.rs`, `lib.rs`, `api.ts`, `CloudConsent.tsx` and `devMock.ts` are as of #102.

## Purpose
The cross-cutting store of non-secret settings: the generic `get_app_setting` / `set_app_setting` pair and the allowlists that limit what the webview may read and write, the LLM provider settings (`get_llm_settings` / `set_llm_settings`, including keychain writes for API keys), and cloud consent: `request_cloud_consent`, which asks in a native dialog and is the only code that records consent, plus the in-memory list of meetings whose background AI pass was refused for missing consent, and the command that re-runs them. This doc also holds the **settings-key registry for the whole app**.

## Files it owns
- `src-tauri/src/commands.rs` (part) — `is_webview_setting_key`, `is_webview_writable_setting_key`, `check_webview_setting_key`, `check_webview_writable_setting_key`, `get_app_setting`, `set_app_setting` (lines 52-121) and their tests (704-772).
- `src-tauri/src/llm_commands.rs` (part) — `CLOUD_CONSENT_PREFIX`, `cloud_consent_key` (61-70); the consent dialog and its only writer (131-431): `ConsentPrompt`, `cloud_consent_prompt`, `ConsentAnswer`, `has_cloud_consent`, `cloud_consent_question`, `ConsentPacing`, `record_cloud_consent`, `request_cloud_consent`, `consent_answer`, `consent_buttons`, `ask_consent_natively`; `consent_skipped_meetings`, `retry_consent_skipped` (457-477); `get_llm_settings`, `set_llm_settings` (a thin wrapper over `apply_llm_settings`), `key_endpoint`, `endpoint_needs_key`, `LlmSettings*` types. `build_provider`, which *reads* consent, belongs to [app-llm](app-llm.md).
- `src-tauri/src/state.rs:46-54` — `AppState::consent_skipped`, `AppState::consent_prompt`.
- `src-tauri/Cargo.toml` (part) — the `rfd` dependency behind the consent dialog (desktop targets only).
- Settings storage itself: `crates/fly-storage/src/settings.rs` (owned by [fly-storage](fly-storage.md)).

## Surface area
### Public API
| Item | Line | Used by |
|---|---|---|
| `is_webview_setting_key(&str) -> bool` (the read allowlist): prefix `consent.` / `capture.` / `ui.`, ≤ 128 chars, `[A-Za-z0-9._-]` only | `commands.rs:59-66` | `get_app_setting`, `is_webview_writable_setting_key`, `llm_commands.rs:1313` (test) |
| `is_webview_writable_setting_key(&str) -> bool` (the write allowlist): the read allowlist minus `consent.cloud_llm.*`, compared case-insensitively | `commands.rs:68-79` | `set_app_setting` (through `check_webview_writable_setting_key`, `commands.rs:89-96`) |
| `CLOUD_CONSENT_PREFIX = "consent.cloud_llm."` | `llm_commands.rs:61-66` | `is_webview_writable_setting_key`, `cloud_consent_key`, `reset_unseen_cloud_consent` |
| `cloud_consent_prompt(provider, at_launch) -> ConsentPrompt{title, message, allow, decline}` (and `cloud_consent_prompt_for(provider, at_launch, custom_host)`, `consent_recipient`): the disclosure, built from the provider id. When the provider's saved base URL isn't its default server, `cloud_consent_question` names it: "sent to the server at <host> (your custom <Vendor> endpoint)". The launch variant starts with "Your AI provider is set to …" and declines with "Not now" instead of "Cancel" | `llm_commands.rs:135-176` | `cloud_consent_question`, tests |
| `cloud_consent_question(&Storage, provider, at_launch) -> Result<Option<ConsentPrompt>, String>`: `None` for a local provider or one already allowed, an error for an unknown id. Uses the launch wording only when `provider` is the saved `llm.provider` | `llm_commands.rs:197-216` | `request_cloud_consent`, tests |
| `ConsentPacing` (`is_quiet`, `record`): two declines for a provider within 60 s mean no dialog for that provider for 60 s; an allow clears its record | `llm_commands.rs:218-262` | `request_cloud_consent` (through `AppState::consent_prompt`), tests |
| `record_cloud_consent(&Storage, provider, ConsentAnswer) -> Result<bool, String>`: writes `consent.cloud_llm.<provider>` = `"true"` on `Allow`, nothing on `Decline`. The only writer of the key | `llm_commands.rs:264-279` | `request_cloud_consent`, tests |
| `AppState::consent_skipped: Mutex<BTreeSet<String>>` (in memory, lost on restart) | `state.rs:50` | written by `record_consent_skip` (`llm_commands.rs:445-455`), cleared per meeting by `scheduler::ai_pass` (`scheduler.rs:215`), emptied by `take_consent_rerun` (`scheduler.rs:263-269`) |
| `AppState::consent_prompt: tokio::sync::Mutex<ConsentPacing>`: held while the dialog is open, so one dialog shows at a time; also holds the pacing | `state.rs:51-54` | `request_cloud_consent` |
| `validate_base_url(provider, url)`: keyed providers need https (http only for loopback), no userinfo; Ollama any http(s) | `llm_commands.rs:1134-1163` | `set_llm_settings`, `build_provider` |
| `Storage::get_setting / set_setting / delete_setting / delete_settings_with_prefix` (literal prefix, empty prefix deletes nothing) | `crates/fly-storage/src/settings.rs:8-45` | every key below |

#### Consent flow
1. The UI's `useCloudConsent` returns `askCloudConsent` (`CloudConsent.tsx:33-64`). It reads `consent.cloud_llm.<id>` with `get_app_setting` (`:45`). If it isn't `"true"`, it calls `request_cloud_consent(id, atLaunch)` (`:50`). The UI never writes the key. Asked at launch for a cloud provider (`CloudConsentOnLaunch`, 72-88, with `atLaunch: true`), before Settings › Test (`SettingsModal.tsx:779`), from Ask's "Review and allow", and from the consent-skips notice.
2. `request_cloud_consent` (`llm_commands.rs:281-311`) takes `AppState::consent_prompt`, so requests take turns and only one dialog is open; one that waited sees the earlier answer. `cloud_consent_question` decides: an unknown provider is an error, and a local or already-allowed provider returns true with no dialog. While the provider is in its quiet period (`ConsentPacing`), it returns false with no dialog.
3. Otherwise it shows a native dialog from Rust with the `rfd` crate (`ask_consent_natively`, `llm_commands.rs:353-431`). The webview gets no new capability, and a script there can't draw, fill in or click it. Decline is the first, default button, so Enter declines. Only a click on the allow button counts as yes (`consent_answer`, `consent_buttons`, 313-351).
   - **Windows and macOS:** rfd's async dialog, built on the main thread and parented to the app window. Esc and closing decline on Windows; on macOS Esc does nothing, so one of the two buttons must be clicked.
   - **GTK (Linux, the BSDs):** rfd's blocking dialog via `spawn_blocking`, because rfd's async GTK path can't report which custom button was clicked. It isn't modal to the app window: rfd's GTK backend ignores the parent.
   - **Other targets (mobile):** no dialog; the answer is always decline.
4. The answer goes into the pacing record, then to `record_cloud_consent`, which writes `"true"` only on an allow. The command returns whether the provider may now receive content. On true, the UI fires `fotw:llm-settings-changed` (`CloudConsent.tsx:51`).
5. `build_provider` reads the same row (`llm_commands.rs:602`) and refuses without it ([app-llm](app-llm.md#build_provider-what-runs-in-order-llm_commandsrs569-653)).
6. Background AI-pass refusals for missing consent or a missing API key (`is_consent_refusal` / `is_missing_key`) land in `consent_skipped` (`record_consent_skip`, called at `scheduler.rs:404` and `extraction.rs:317`). After each AI pass, `scheduler::ai_pass` emits the whole list as `llm:consent-skips` (`scheduler.rs:229`).
7. The notice (`useConsentSkips`, `CloudConsent.tsx`) catches up with `consent_skipped_meetings` at mount and then follows the event. While the saved cloud provider has no key it offers "Add a key", which opens Settings at the key field (`initialFocus: "llm-key"`); otherwise "Run" asks for consent if needed, then calls `retry_consent_skipped`. That empties the list, drops meetings that are transcribing or whose last transcription failed (`scheduler.rs:263-280`), and re-runs `ai_pass` for the rest in the background. Every re-run goes through `build_provider` again, and a re-run that still has no key puts the meeting back on the list.
8. At startup, once per data dir, `reset_unseen_cloud_consent` deletes every `consent.cloud_llm.*` row recorded before v2.4. Those rows were written while the old async `window.confirm` made every prompt count as yes. The flag `migration.cloud_consent_reset_v2_4` records that the reset ran (`llm_commands.rs:72-119`, `startup.rs:193`). Consents given since v2.4 are kept.

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `get_app_setting` | `key: String` (read allowlist) | `Result<Option<String>, String>`; non-allowlisted → `"setting '<key>' can't be accessed from the app UI"` | `ui-app-shell` (`App.tsx:268,282`), `ui-confirm-consent` (`CloudConsent.tsx:45,152`), `ui-editor` (`Editor.tsx:997`), `ui-settings` (`SettingsModal.tsx:379`, `PrivacySection.tsx:59`), `ui-recording-bar` (`RecordingBar.tsx:62`, `RemindNoticeRow.tsx:16`), via `api.getAppSetting` |
| `set_app_setting` | `key: String` (write allowlist), `value: String` (≤ 4096 bytes) | `Result<(), String>`; non-allowlisted → `"setting '<key>' can't be accessed from the app UI"`; `consent.cloud_llm.*` in any letter case → `"setting '<key>' can't be changed from the app UI"` | `ui-app-shell` (`App.tsx:1135`), `ui-editor` (`Editor.tsx:1005`), `ui-settings` (`SettingsModal.tsx:1385`, `PrivacySection.tsx:167`), `ui-recording-bar` (`RemindNoticeRow.tsx:29`), via `api.setAppSetting` |
| `request_cloud_consent` | `provider: String`, `at_launch: bool` (`atLaunch` from JS) | `Result<bool, String>`: true when the provider may receive content (local, allowed before, or allowed now); false on a decline or during the quiet period; unknown provider → `"unknown provider '<id>'"` | `ui-confirm-consent` (`CloudConsent.tsx:50`), via `api.requestCloudConsent` (`api.ts:186-187`) |
| `get_llm_settings` | — | `Result<LlmSettings{provider, providers: [{id, default_model, is_local, has_key, model, base_url}]}, String>` | `ui-app-shell` (`App.tsx:131`), `ui-settings` (`SettingsModal.tsx:350`), `ui-ask` (`AskPanel.tsx:136`), `ui-confirm-consent` (`CloudConsent.tsx:80,149,167`), via `api.getLlmSettings` |
| `set_llm_settings` | `update: LlmSettingsUpdate{provider, model?, base_url?, api_key?}`; `api_key: Some("")` deletes the key, `None` leaves it | `Result<(), String>`; unknown provider or bad base URL refused before any write. For OpenAI and NIM the saved key is tied to its endpoint's origin (scheme, host, port; blank = the provider default): a change of origin with `api_key: None` while a key is stored is refused with `endpoint_needs_key` ("To use a different server for <vendor>, enter the API key again. Nothing was changed.") and nothing is written. With a new key or `""`, the old key is deleted before the new URL is saved. Same origin (another path, the default spelled out) needs no key. Any accepted change of origin also deletes `consent.cloud_llm.<provider>`, so the next use asks again in the native dialog, naming the new server. Anthropic's host is fixed, so its URL never carries the key | `ui-settings` (`SettingsModal.tsx:684,713,757,784`), `ui-ask` "Runs on" (`AskPanel.tsx:357`), via `api.setLlmSettings` (which then fires the DOM event `fotw:llm-settings-changed`, `api.ts:8-11,175`) |
| `consent_skipped_meetings` | — | `Vec<String>` (meeting ids, sorted) | `ui-confirm-consent` (`CloudConsent.tsx:117`, feeding `ui-notices` `AppNotices.tsx:169`), via `api.consentSkippedMeetings` |
| `retry_consent_skipped` | — (`AppHandle`) | `usize` (meetings taken on) | `ui-confirm-consent` (`CloudConsent.tsx:169`), via `api.retryConsentSkipped` |

devMock mirrors all seven (`devMock.ts:1252-1269,1335-1340,1365-1380`). Drift:
- Mock `get_app_setting` / `set_app_setting` accept **any** other key and any length. A UI change that reads or writes a non-allowlisted key works in port-1420 QA and in jsdom tests, then fails in the real app. Like the backend, mock `set_app_setting` refuses `consent.cloud_llm.*` in any letter case, with the same error (`devMock.ts:1373-1378`).
- Mock `request_cloud_consent` (`mockRequestCloudConsent`, `devMock.ts:1007-1022`) asks with the browser's `window.confirm`, its stand-in for the native dialog. It only runs in a plain browser, never under Tauri. Its words come from `mockConsentPrompt` (`devMock.ts:987-1005`), which the Rust test `dev_mock_shows_the_same_disclosure` keeps equal to `cloud_consent_prompt`. It has no pacing, and it uses the launch wording whenever asked to, without checking the saved provider.
- Mock `set_llm_settings` skips the unknown-provider and base-URL validation. It applies the same endpoint rule as the backend (`serverChangeNeedsKey` from `settingsModel.ts`), with the same refusal text.
- Mock `consent.recording_notice_accepted` is driven by the `fotwMockFirstRun` localStorage toggle (`devMock.ts:1366-1371`).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `llm:consent-skips` | emitted by `scheduler::ai_pass` (`scheduler.rs:229`) after every AI pass (including re-runs) | `string[]`: the full current `consent_skipped` set | listened by `CloudConsent.tsx:122` (`useConsentSkips`) → `AppNotices.tsx:169`; devMock emits it at `devMock.ts:1038` |
| `fotw:llm-settings-changed` (DOM event, not Tauri) | dispatched by `api.setLlmSettings` and after `request_cloud_consent` returns true (`CloudConsent.tsx:51`) | none | `CloudConsent.tsx:139-143` re-reads who still needs consent |

Not emitted: re-extraction after re-diarize/revert (`asr_commands.rs:158-163`) can add to `consent_skipped` without an event.

### Settings keys
The app-wide registry. All rows live in the SQLite `settings` table (`crates/fly-storage/src/lib.rs:620-623`). "Delete all my data" erases every row (`crates/fly-storage/src/privacy.rs:375`). Webview-settable means `is_webview_writable_setting_key` accepts it, so any webview script can read and write it. Webview-readable keys (`is_webview_setting_key`) are the same set plus `consent.cloud_llm.*`.

| Key (or prefix) | Owner | Reader(s) | Writer(s) | Webview-settable? |
|---|---|---|---|---|
| **`consent.cloud_llm.<provider>`** | app-settings-consent | `build_provider` (`llm_commands.rs:602`), `has_cloud_consent` (`llm_commands.rs:187-195`, for `request_cloud_consent`); UI `CloudConsent.tsx:45,152` | `record_cloud_consent` (`llm_commands.rs:264-279`), only after a click on allow in `request_cloud_consent`'s native dialog; bulk delete `reset_unseen_cloud_consent` (`llm_commands.rs:102`) | **no: read-only.** `get_app_setting` reads it; `set_app_setting` refuses it in any letter case (`commands.rs:68-79`; test `webview_cannot_write_cloud_consent`, `commands.rs:709-750`). |
| `consent.recording_notice_accepted` | ui-app-shell | `App.tsx:268,282` | `App.tsx:1135` | yes |
| `capture.mic_device_id` | app-recording | `recording.rs:168`; `SettingsModal.tsx:379` | `SettingsModal.tsx:1385` | yes |
| `ui.ask.pinned` | ui-ask | `Editor.tsx:997` | `Editor.tsx:1005` | yes |
| `ui.privacy.disk_encryption_banner_dismissed` (`privacy.ts:8`) | ui-settings | `PrivacySection.tsx:59` | `PrivacySection.tsx:167` | yes |
| `ui.recording.remind_notice` (`recordingBar.ts:9`) | ui-recording-bar | `RecordingBar.tsx:62`, `RemindNoticeRow.tsx:16` | `RemindNoticeRow.tsx:29` | yes |
| `llm.provider` | app-settings-consent | `build_provider` (`llm_commands.rs:581`), `ensure_provider_ready` (487), `cloud_consent_question` (213, launch wording), `get_llm_settings` (1117), `ollama_delete` (`ollama.rs:261`) | `set_llm_settings` (`llm_commands.rs:1174`) | no |
| `llm.<provider>.model` | app-settings-consent | `build_provider` (587), `get_llm_settings` (1112), `ollama_delete` (`llm.ollama.model`, `ollama.rs:262`) | `set_llm_settings` (1177-1181; `""` = default); `migrate_anthropic_model` (`llm.anthropic.model`, 49) | no |
| `llm.<provider>.base_url` | app-settings-consent | `build_provider` (588, re-validated), `get_llm_settings` (1113), `ollama::root_url` (`llm.ollama.base_url`, `ollama.rs:37`) → Ollama probe/spawn, embedding worker and query embeds ([app-search](app-search.md)) | `set_llm_settings` (validated; a change of origin for a keyed provider needs the key again) | no |
| `migration.cloud_consent_reset_v2_4` | app-settings-consent | `reset_unseen_cloud_consent` (`llm_commands.rs:95`) | same (104) | no (`llm_commands.rs:1314`) |
| `asr.tier` | app-pipeline | `get_asr_settings` (`asr_commands.rs:295`), `pipeline.rs:228` | `set_asr_settings` (`asr_commands.rs:336`) | no |
| `asr.model_id` | app-pipeline | `asr_commands.rs:296`, `pipeline.rs:233` | `asr_commands.rs:340,343` | no |
| `asr.use_groq` | app-pipeline | `asr_commands.rs:297`, `pipeline.rs:231` | `asr_commands.rs:349` | no |
| `asr.max_quality` | app-pipeline | `asr_commands.rs:298`, `pipeline.rs:232` | `asr_commands.rs:355` | no |
| `asr.auto_transcribe` | app-pipeline | `asr_commands.rs:305` (UI acts on it) | `asr_commands.rs:361` | no |
| `asr.language` | app-pipeline | `pipeline::run_with` | none in code (inferred: set by hand) | no |
| `asr.engine` (`pipeline::ENGINE_SETTING_KEY`) | app-pipeline | `pipeline::run_with`; `FLYONTHEWALL_ASR_ENGINE` wins over it | none in the app; the accuracy harness sets it | no (pinned by a test in `pipeline.rs`) |
| `asr.xtalk_window_ms` (`pipeline::XTALK_WINDOW_SETTING_KEY`) | app-pipeline | `pipeline::run_with` | none in the app; the accuracy harness sets it | no (pinned by a test in `pipeline.rs`) |
| `asr.use_gpu` (`gpu::USE_GPU_KEY`) | app-models-gpu | `gpu.rs:40` | `asr_commands.rs:378-381` | no |
| `asr.gpu_bench` (`gpu::BENCH_KEY`) | app-models-gpu | `gpu.rs:49` | `gpu.rs:58`; cleared `asr_commands.rs:375` | no |
| `hw.cache` (`hw::CACHE_KEY`) | app-models-gpu | `hw.rs:26` | `hw.rs:36`, `asr_commands.rs:267` | no |
| `calendar.google.client_id`, `calendar.ms.client_id` | app-calendar | `calendar_commands.rs:38,56,135-137` | `calendar_commands.rs:184,187` | no |
| `calendar.google.disabled_ids`, `calendar.ms.disabled_ids` | app-calendar | `calendar_commands.rs:66` | `calendar_commands.rs:415` | no |
| `import.staged.<meeting_id>` | app-import | `import_commands.rs:320` | `import_commands.rs:336`; deleted 408, 566 | no |
| `live.enabled` | app-recording | `live.rs:92` (default on) | none in code (inferred: set by hand) | no |
| `privacy.audio_retention_days` | app-privacy | `crates/fly-storage/src/privacy.rs:451` | `crates/fly-storage/src/privacy.rs:460-462` (choices only) | no |
| `privacy.retention_last_sweep` | app-privacy | `privacy.rs:187` | `privacy.rs:225` | no |
| `privacy.retention_held` | app-privacy | `privacy.rs:316` | `privacy.rs:215`; deleted 183, 223 | no |
| `storage.compress.saved_bytes` | app-audio-store | `audio_store.rs` (`spawn`, `run_pass`) | `audio_store.rs` (`run_pass`) | no |
| `storage.compress.notice` | app-audio-store | `audio_store.rs` (`run_pass`) | `audio_store.rs` (`run_pass` → `pending`, `dismiss_compress_notice` → `shown`) | no |
| `connectors.registry` | app-connectors | `connectors.rs` (`SettingsStore::load`) | `connectors.rs` (`SettingsStore::save`), only from connector commands; trust grants only after a native-dialog yes | **no: not readable either.** Outside the read allowlist and listed in `BACKEND_ONLY` (`commands.rs`; test `webview_cannot_touch_connector_state`) |
| `connectors.call_log` | app-connectors | `connectors.rs` (`connector_call_log`) | `connectors.rs` (calls, `clear_connector_call_log`) | no |
| `ui.beta.connectors` | ui-connectors | `connectors.rs` (`beta_enabled`: shows the feature, lets `turn_for_ask` offer tools; grants nothing); `ConnectorsSection.tsx` | `ConnectorsSection.tsx` | yes |
| `privacy.last_wipe_report` | app-privacy | `privacy.rs:311` | `privacy.rs:775`; deleted 375 | no |

Any other key under `consent.`, `capture.` or `ui.` (except `consent.cloud_llm.*`) can also be created by the webview. Backend code reads one of them, `ui.beta.connectors`, as a show/hide switch only: what connectors may do is decided by native-dialog approvals stored in `connectors.*`.

### Database
| Table | Columns | Read / write |
|---|---|---|
| `settings` | `key TEXT PRIMARY KEY, value TEXT NOT NULL` | read/write (all keys above) |

### Files and directories written
None directly (rows only).

### Network egress
None. `get_llm_settings` reads the keychain locally; `retry_consent_skipped` triggers [app-llm](app-llm.md) calls through `build_provider`.

### External processes
None. The consent dialog is an OS dialog shown in-process (Windows TaskDialog, macOS NSAlert, GTK).

### Secrets and keychain entries
| Entry | Use |
|---|---|
| `openai_api_key`, `anthropic_api_key`, `nim_api_key` | `get_llm_settings` reads them only to report `has_key` (`llm_commands.rs:1109-1111`). `set_llm_settings` sets or deletes them (`llm_commands.rs:1189-1201`). Keys never return to the webview. |

## Dependencies
- **Inbound:** frontend (`api.ts:95-96,173-187`); `scheduler.rs` (`ai_pass`, `retry_consent_skipped`, `take_consent_rerun`); `extraction.rs:317` and `scheduler.rs:404` (`record_consent_skip`); `startup.rs:187-195`, `lib.rs:122-123,160-165`.
- **Outbound:** [fly-storage](fly-storage.md) `settings.rs`, [fly-secrets](fly-secrets.md), [app-pipeline](app-pipeline.md) (`scheduler::retry_consent_skipped`, `ai_pass`), [app-llm](app-llm.md) (`PROVIDERS`, `provider_vendor`, `secret_key_for`, `validate_base_url`); the `rfd` crate for the native dialog (the same version and features tauri-plugin-dialog already builds, desktop targets only), and Tauri's `AppHandle` (main thread, main window as the dialog's parent).

## Invariants
- Webview-readable keys stay limited to `consent.` / `capture.` / `ui.`; anything else gets a dedicated, validating command. Spec §2 item 2; THREAT_MODEL T3. Enforced at `commands.rs:59-96`, checked on read (`commands.rs:100`) and, with the narrower write allowlist, on write (`commands.rs:111`).
- Secrets never go through the generic settings commands (`commands.rs:52-53`). API keys are written only by `set_llm_settings` to the keychain.
- Custom base URLs for keyed providers must be https (http only for loopback), checked on save here and on use in `build_provider`. T4.
- A saved key and a recorded consent only cover the endpoint they were given for: `apply_llm_settings` refuses a change of origin without the key, drops the old key before saving a new origin, and deletes the provider's consent so `request_cloud_consent` asks again (Settings asks right after such a save). `set_llm_settings` is the only writer of `llm.<provider>.base_url` (`llm.*` isn't webview-writable through `set_app_setting`). T4.
- Cloud consent is recorded only by the backend, and only after a click on the allow button of the native dialog. T10. `record_cloud_consent` (`llm_commands.rs:264-279`) is the only writer, and `set_app_setting` refuses `consent.cloud_llm.*` in any letter case (`commands.rs:68-79`). Decline is the default button: Enter, Esc, closing and any other result record nothing (`llm_commands.rs:313-351`). The words are built in Rust from the provider id, never taken from the webview (`cloud_consent_prompt`, 148-176).
- One consent dialog at a time (`AppState::consent_prompt`). Two declines for a provider within 60 s mean no dialog for it for 60 s, so a request loop can't keep a modal up; an allow clears that (`ConsentPacing`, `llm_commands.rs:218-262`).
- The one-time consent reset runs once, and only marks itself done after the delete succeeds; its flag is not webview-settable (`llm_commands.rs:93-119`, test `llm_commands.rs:1310-1317`).
- Consent re-runs never run over a held transcription failure or a running job (`scheduler.rs:263-280`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/commands.rs:704-772` | 2 | `webview_cannot_write_cloud_consent`: `consent.cloud_llm.*` can't be written in any letter case but stays readable, the exact error texts, other UI-level keys (and look-alikes without the dot) stay writable, nothing outside the read allowlist becomes writable. `webview_can_only_touch_ui_level_settings`: the read allowlist accepts two consent keys and one capture key, rejects several internal keys |
| unit | `src-tauri/src/llm_commands.rs:1268-1317` | 2 | consent reset clears only `consent.cloud_llm.*` and runs once; reset flag not webview-settable |
| unit | `src-tauri/src/llm_commands.rs:1319-1598` | 8 | the consent dialog: disclosure pinned word for word (and the launch variant), asked only when missing (local, allowed, unknown id), launch wording only for the saved provider, only an allow records, a per-platform table mapping each dialog result to allow or decline, repeated declines quiet the dialog, devMock's disclosure matches Rust, `build_provider` refuses without consent on a real `AppState` |
| unit | `src-tauri/src/llm_commands.rs` `security_tests` | 4 | `validate_base_url`; `a_new_server_needs_consent_again_and_the_dialog_names_it` (OpenAI and NIM: consent cleared on a new server, kept for another path, the dialog names the host, `build_provider` refuses until allowed again); `a_saved_key_cant_be_pointed_at_another_server` drives `apply_llm_settings` with the frontend's JSON payloads on a real `AppState`: another host, a look-alike host, another port or scheme without the key → refused and nothing changed; the default spelled out, a new path, a new key or clearing it → allowed; Anthropic unaffected |
| vitest | `frontend/src/settingsModel.test.ts` | 1 | `serverChangeNeedsKey` (the Settings hint and the devMock rule) |
| unit | `src-tauri/src/scheduler.rs:522-557` | 1 | `take_consent_rerun` skips failed/running meetings and empties the set |
| unit | `crates/fly-storage/src/settings.rs` | 2 | get/set/delete; literal prefix delete |
| vitest | `frontend/src/components/CloudConsent.test.tsx` | 7 | launch check asks the backend once with `atLaunch`, a decline records nothing, a yes is left to the backend and fires the change event, StrictMode single request, local/allowed/unreadable settings stay quiet, unknown provider refused without asking, a failed request counts as no (`api` spied; `cloudConsentTesting.ts` stands in for the native dialog) |
| vitest | `frontend/src/components/AppNotices.test.tsx`, `SettingsModal.test.tsx`, `AskPanel.test.tsx` | — | consent-skips notice and Settings/Ask flows, against spies with `cloudConsentTesting.ts`, or against devMock answering its `window.confirm`. `SettingsModal.test.tsx` also checks that devMock refuses consent writes and records only a yes |

**Known gaps:**
- No test calls `get_app_setting` / `set_app_setting` themselves. The 4096-byte value cap and the read-side check are untested; the error texts are pinned only through `check_webview_writable_setting_key`. The allowlist tests never assert the 128-char limit or a key with `/`. The read test's `"consent."` case is skipped with `continue`, so it is dead.
- `request_cloud_consent` itself has no test: not the lock, not the quiet-period early return, not the dialog wiring. Its parts are tested one by one, and the native dialog can't run in CI.
- `get_llm_settings` is untested (`has_key` never leaking the key), and so is `set_llm_settings`'s unknown-provider refusal. The endpoint rule is tested through `apply_llm_settings`, not through Tauri's IPC layer.
- `llm:consent-skips` emission and `retry_consent_skipped`'s background loop are untested. Only the pure helper is. The re-diarize path that records skips without emitting is untested.
- `migrate_anthropic_model` has no test in this crate (only `successor_model` in fly-llm).
- devMock's settings commands skip the allowlist (apart from refusing `consent.cloud_llm.*` writes), so jsdom tests can't catch a UI that writes a disallowed key.

## Changing it safely
- New setting the UI must write: prefer a dedicated validating command. Use the `ui.` prefix only for harmless view state, never for anything a backend path trusts. A record only the backend may write goes in `BACKEND_ONLY` in `is_webview_writable_setting_key` (today: `consent.cloud_llm.` and `connectors.`).
- Changing the consent words: edit `cloud_consent_prompt` and devMock's `mockConsentPrompt` together. `consent_dialog_says_what_the_in_app_prompt_said` and `dev_mock_shows_the_same_disclosure` pin both.
- Changing the dialog (an `rfd` upgrade, a new platform path, other buttons): check again what each backend reports for each button, Esc and closing, and update `each_platforms_dialog_results_map_to_the_right_answer`. Keep decline as the first (default) button, and count only a click on allow as yes.
- Add the key to the registry table above, and to devMock if the UI reads it.
- Run `cargo test -p fly-app` (allowlists, consent dialog, consent reset, scheduler) and `npm --prefix frontend test` (CloudConsent, AppNotices, SettingsModal, AskPanel).
- Trap: `consent_skipped` is in memory only. After a restart the notice is empty even though those meetings still have no cleaned transcript or items. Backfill in Settings covers items, but nothing covers polish.
