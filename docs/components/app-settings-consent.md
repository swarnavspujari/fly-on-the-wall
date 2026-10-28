# Settings and cloud consent (`app-settings-consent`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: **Consent hardening** (#102, open) targets `commands.rs` `is_webview_setting_key`, `llm_commands.rs` (`build_provider`'s consent read, `reset_unseen_cloud_consent`) and `frontend/src/components/CloudConsent.tsx`. Today `consent.cloud_llm.<id>` sits under the webview-settable `consent.` prefix, so any script in the webview can record cloud consent through `set_app_setting` without the in-app prompt. Expect a dedicated consent command and a narrowed allowlist.

## Purpose
The cross-cutting store of non-secret settings: the generic `get_app_setting` / `set_app_setting` pair and the allowlist that limits what the webview may touch, the LLM provider settings (`get_llm_settings` / `set_llm_settings`, including keychain writes for API keys), and the cloud-consent bookkeeping: the in-memory list of meetings whose background AI pass was refused for missing consent, and the command that re-runs them. This doc also holds the **settings-key registry for the whole app**.

## Files it owns
- `src-tauri/src/commands.rs` (part) — `is_webview_setting_key`, `check_webview_setting_key`, `get_app_setting`, `set_app_setting` (lines 52-99) and their test (682-706).
- `src-tauri/src/llm_commands.rs` (part) — `get_llm_settings`, `set_llm_settings`, `LlmSettings*` types (765-891), `consent_skipped_meetings`, `retry_consent_skipped` (144-164). `build_provider`, which *reads* consent, belongs to [app-llm](app-llm.md).
- `src-tauri/src/state.rs:46-50` — `AppState::consent_skipped`.
- Settings storage itself: `crates/fly-storage/src/settings.rs` (owned by [fly-storage](fly-storage.md)).

## Surface area
### Public API
| Item | Line | Used by |
|---|---|---|
| `is_webview_setting_key(&str) -> bool`: prefix `consent.` / `capture.` / `ui.`, ≤ 128 chars, `[A-Za-z0-9._-]` only | `commands.rs:59-66` | `get_app_setting`, `set_app_setting`, `llm_commands.rs:1001` (test) |
| `AppState::consent_skipped: Mutex<BTreeSet<String>>` (in memory, lost on restart) | `state.rs:50` | written by `record_consent_skip` (`llm_commands.rs:132-142`), cleared per meeting by `scheduler::ai_pass` (`scheduler.rs:215`), emptied by `take_consent_rerun` (`scheduler.rs:263-269`) |
| `validate_base_url(provider, url)`: keyed providers need https (http only for loopback), no userinfo; Ollama any http(s) | `llm_commands.rs:822-851` | `set_llm_settings`, `build_provider` |
| `Storage::get_setting / set_setting / delete_setting / delete_settings_with_prefix` (literal prefix, empty prefix deletes nothing) | `crates/fly-storage/src/settings.rs:8-45` | every key below |

#### Consent flow (as built today)
1. The UI's `useCloudConsent` (`CloudConsent.tsx:32-73`) reads `consent.cloud_llm.<id>` with `get_app_setting`. If it isn't `"true"`, it shows the in-app `useConfirm` dialog. Only on an explicit yes does it write `"true"` with `set_app_setting` (`CloudConsent.tsx:61`). Asked at launch for a cloud provider (`CloudConsentOnLaunch`, 81-102), before Settings › Test (`SettingsModal.tsx:779`), from Ask's "Review and allow", and from the consent-skips notice.
2. `build_provider` reads the same row (`llm_commands.rs:290`) and refuses without it ([app-llm](app-llm.md#build_provider-what-runs-in-order-llm_commandsrs256-341)).
3. Background AI-pass refusals for missing consent land in `consent_skipped` (`record_consent_skip`, called at `scheduler.rs:404` and `extraction.rs:317`). After each AI pass, `scheduler::ai_pass` emits the whole list as `llm:consent-skips` (`scheduler.rs:229`).
4. The notice (`useConsentSkips`, `CloudConsent.tsx:118-197`) catches up with `consent_skipped_meetings` at mount and then follows the event. "Run" asks for consent if needed, then calls `retry_consent_skipped`. That empties the list, drops meetings that are transcribing or whose last transcription failed (`scheduler.rs:263-280`), and re-runs `ai_pass` for the rest in the background. Every re-run goes through `build_provider` again.
5. At startup, once per data dir, `reset_unseen_cloud_consent` deletes every `consent.cloud_llm.*` row recorded before v2.4. Those rows were written while the old async `window.confirm` made every prompt count as yes. The flag `migration.cloud_consent_reset_v2_4` records that the reset ran (`llm_commands.rs:61-108`, `lib.rs:115`).

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `get_app_setting` | `key: String` (allowlisted) | `Result<Option<String>, String>`; non-allowlisted → `"setting '<key>' can't be accessed from the app UI"` | `ui-app-shell` (`App.tsx:268,282`), `ui-confirm-consent` (`CloudConsent.tsx:48,164`), `ui-editor` (`Editor.tsx:997`), `ui-settings` (`SettingsModal.tsx:379`, `PrivacySection.tsx:59`), `ui-recording-bar` (`RecordingBar.tsx:62`, `RemindNoticeRow.tsx:16`), via `api.getAppSetting` |
| `set_app_setting` | `key: String` (allowlisted), `value: String` (≤ 4096 bytes) | `Result<(), String>` | `ui-app-shell` (`App.tsx:1135`), `ui-confirm-consent` (`CloudConsent.tsx:61`), `ui-editor` (`Editor.tsx:1005`), `ui-settings` (`SettingsModal.tsx:1385`, `PrivacySection.tsx:167`), `ui-recording-bar` (`RemindNoticeRow.tsx:29`), via `api.setAppSetting` |
| `get_llm_settings` | — | `Result<LlmSettings{provider, providers: [{id, default_model, is_local, has_key, model, base_url}]}, String>` | `ui-app-shell` (`App.tsx:131`), `ui-settings` (`SettingsModal.tsx:350`), `ui-ask` (`AskPanel.tsx:136`), `ui-confirm-consent` (`CloudConsent.tsx:89,161,179`), via `api.getLlmSettings` |
| `set_llm_settings` | `update: LlmSettingsUpdate{provider, model?, base_url?, api_key?}`; `api_key: Some("")` deletes the key, `None` leaves it | `Result<(), String>`; unknown provider or bad base URL refused before any write | `ui-settings` (`SettingsModal.tsx:684,713,757,784`), `ui-ask` "Runs on" (`AskPanel.tsx:357`), via `api.setLlmSettings` (which then fires the DOM event `fotw:llm-settings-changed`, `api.ts:8-11,175`) |
| `consent_skipped_meetings` | — | `Vec<String>` (meeting ids, sorted) | `ui-confirm-consent` (`CloudConsent.tsx:129`, feeding `ui-notices` `AppNotices.tsx:169`), via `api.consentSkippedMeetings` |
| `retry_consent_skipped` | — (`AppHandle`) | `usize` (meetings taken on) | `ui-confirm-consent` (`CloudConsent.tsx:181`), via `api.retryConsentSkipped` |

devMock mirrors all six (`devMock.ts:1215-1232,1298-1301,1326-1336`). Drift:
- Mock `get_app_setting` / `set_app_setting` accept **any** key and any length. A UI change that reads or writes a non-allowlisted key works in port-1420 QA and in jsdom tests, then fails in the real app.
- Mock `set_llm_settings` skips the unknown-provider and base-URL validation.
- Mock `consent.recording_notice_accepted` is driven by the `fotwMockFirstRun` localStorage toggle (`devMock.ts:1327-1332`).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `llm:consent-skips` | emitted by `scheduler::ai_pass` (`scheduler.rs:229`) after every AI pass (including re-runs) | `string[]`: the full current `consent_skipped` set | listened by `CloudConsent.tsx:134` (`useConsentSkips`) → `AppNotices.tsx:169`; devMock emits it at `devMock.ts:1001` |
| `fotw:llm-settings-changed` (DOM event, not Tauri) | dispatched by `api.setLlmSettings` and after a consent yes (`CloudConsent.tsx:62`) | none | `CloudConsent.tsx:153` re-reads who still needs consent |

Not emitted: re-extraction after re-diarize/revert (`asr_commands.rs:158-163`) can add to `consent_skipped` without an event.

### Settings keys
The app-wide registry. All rows live in the SQLite `settings` table (`crates/fly-storage/src/lib.rs:484-487`). "Delete all my data" erases every row (`crates/fly-storage/src/privacy.rs:375`). Webview-settable means `is_webview_setting_key` accepts it, so any webview script can read and write it.

| Key (or prefix) | Owner | Reader(s) | Writer(s) | Webview-settable? |
|---|---|---|---|---|
| **`consent.cloud_llm.<provider>`** | app-settings-consent | `build_provider` (`llm_commands.rs:290`); UI `CloudConsent.tsx:48,164` | UI `CloudConsent.tsx:61` (the only writer in code); bulk delete `reset_unseen_cloud_consent` (`llm_commands.rs:91`) | **yes: the open gap.** A script can grant consent without the prompt; `llm_commands.rs:1006` and `commands.rs:690` assert it stays settable. Being changed by the consent hardening (#102). |
| `consent.recording_notice_accepted` | ui-app-shell | `App.tsx:268,282` | `App.tsx:1135` | yes |
| `capture.mic_device_id` | app-recording | `recording.rs:168`; `SettingsModal.tsx:379` | `SettingsModal.tsx:1385` | yes |
| `ui.ask.pinned` | ui-ask | `Editor.tsx:997` | `Editor.tsx:1005` | yes |
| `ui.privacy.disk_encryption_banner_dismissed` (`privacy.ts:8`) | ui-settings | `PrivacySection.tsx:59` | `PrivacySection.tsx:167` | yes |
| `ui.recording.remind_notice` (`recordingBar.ts:9`) | ui-recording-bar | `RecordingBar.tsx:62`, `RemindNoticeRow.tsx:16` | `RemindNoticeRow.tsx:29` | yes |
| `llm.provider` | app-settings-consent | `build_provider` (`llm_commands.rs:268`), `ensure_provider_ready` (174), `get_llm_settings` (805), `ollama_delete` (`ollama.rs:261`) | `set_llm_settings` (`llm_commands.rs:862`) | no |
| `llm.<provider>.model` | app-settings-consent | `build_provider` (274), `get_llm_settings` (800), `ollama_delete` (`llm.ollama.model`, `ollama.rs:262`) | `set_llm_settings` (865-869; `""` = default); `migrate_anthropic_model` (`llm.anthropic.model`, 49) | no |
| `llm.<provider>.base_url` | app-settings-consent | `build_provider` (275, re-validated), `get_llm_settings` (801), `ollama::root_url` (`llm.ollama.base_url`, `ollama.rs:37`) → Ollama probe/spawn, embedding worker and query embeds ([app-search](app-search.md)) | `set_llm_settings` (871-875, validated) | no |
| `migration.cloud_consent_reset_v2_4` | app-settings-consent | `reset_unseen_cloud_consent` (`llm_commands.rs:84`) | same (93) | no (`llm_commands.rs:1002`) |
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
| `privacy.last_wipe_report` | app-privacy | `privacy.rs:311` | `privacy.rs:775`; deleted 375 | no |

Any other key under `consent.`, `capture.` or `ui.` can also be created by the webview. Today no backend code reads one.

### Database
| Table | Columns | Read / write |
|---|---|---|
| `settings` | `key TEXT PRIMARY KEY, value TEXT NOT NULL` | read/write (all keys above) |

### Files and directories written
None directly (rows only).

### Network egress
None. `get_llm_settings` reads the keychain locally; `retry_consent_skipped` triggers [app-llm](app-llm.md) calls through `build_provider`.

### External processes
None.

### Secrets and keychain entries
| Entry | Use |
|---|---|
| `openai_api_key`, `anthropic_api_key`, `nim_api_key` | `get_llm_settings` reads them only to report `has_key` (`llm_commands.rs:797-799`). `set_llm_settings` sets or deletes them (`llm_commands.rs:877-889`). Keys never return to the webview. |

## Dependencies
- **Inbound:** frontend (`api.ts:95-96,173-181`); `scheduler.rs` (`ai_pass`, `retry_consent_skipped`, `take_consent_rerun`); `extraction.rs:317` and `scheduler.rs:404` (`record_consent_skip`); `lib.rs:111-117,210-211,248-252`.
- **Outbound:** [fly-storage](fly-storage.md) `settings.rs`, [fly-secrets](fly-secrets.md), [app-pipeline](app-pipeline.md) (`scheduler::retry_consent_skipped`, `ai_pass`), [app-llm](app-llm.md) (`PROVIDERS`, `secret_key_for`, `validate_base_url`).

## Invariants
- Webview-settable keys stay limited to `consent.` / `capture.` / `ui.`; anything else gets a dedicated, validating command. Spec §2 item 2; THREAT_MODEL T3. Enforced at `commands.rs:59-74`, checked on both read and write (`commands.rs:78,89`).
- Secrets never go through the generic settings commands (`commands.rs:52-53`). API keys are written only by `set_llm_settings` to the keychain.
- Custom base URLs for keyed providers must be https (http only for loopback), checked on save here and on use in `build_provider`. T4. `llm_commands.rs:858,279-281`.
- Cloud consent is recorded only after an explicit in-app yes. T10; the PR #99 fix. UI-enforced only (`CloudConsent.tsx:55-61`). **Not enforced by the backend:** the key is webview-writable.
- The one-time consent reset runs once, and only marks itself done after the delete succeeds; its flag is not webview-settable (`llm_commands.rs:82-108`, test `llm_commands.rs:999-1007`).
- Consent re-runs never run over a held transcription failure or a running job (`scheduler.rs:263-280`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/commands.rs:682-706` | 1 | allowlist accepts two consent keys and one capture key, rejects several internal keys |
| unit | `src-tauri/src/llm_commands.rs:956-1007` | 2 | consent reset clears only `consent.cloud_llm.*` and runs once; reset flag not webview-settable (and asserts `consent.cloud_llm.anthropic` **is**) |
| unit | `src-tauri/src/llm_commands.rs:1033-1054` | 2 | `validate_base_url` |
| unit | `src-tauri/src/scheduler.rs:522-557` | 1 | `take_consent_rerun` skips failed/running meetings and empties the set |
| unit | `crates/fly-storage/src/settings.rs` | 2 | get/set/delete; literal prefix delete |
| vitest | `frontend/src/components/CloudConsent.test.tsx` | 6 | launch ask, consent saved only on yes, StrictMode single prompt, unknown provider refused (`api` spied) |
| vitest | `frontend/src/components/AppNotices.test.tsx`, `SettingsModal.test.tsx`, `AskPanel.test.tsx` | — | consent-skips notice and Settings/Ask flows against spies or devMock |

**Known gaps:**
- No test calls `get_app_setting` / `set_app_setting` themselves. The 4096-byte value cap, the error text and the read-side check are untested. The allowlist test never asserts a `ui.` key, the 128-char limit or a key with `/`. Its `"consent."` case is skipped with `continue`, so it is dead.
- No test proves the webview **cannot** grant consent. The current tests assert the opposite (`commands.rs:690`, `llm_commands.rs:1006`), so the hardening PR must flip them on purpose.
- `set_llm_settings` / `get_llm_settings` are untested: unknown provider, base URL refused before any write, `api_key: Some("")` deletion, `has_key` never leaking the key.
- `llm:consent-skips` emission and `retry_consent_skipped`'s background loop are untested. Only the pure helper is. The re-diarize path that records skips without emitting is untested.
- `migrate_anthropic_model` has no test in this crate (only `successor_model` in fly-llm).
- devMock's settings commands skip the allowlist, so jsdom tests can't catch a UI that writes a disallowed key.

## Changing it safely
- New setting the UI must write: prefer a dedicated validating command. Use the `ui.` prefix only for harmless view state, never for anything a backend path trusts.
- After the consent hardening: update `build_provider`'s consent read, `reset_unseen_cloud_consent`, `CloudConsent.tsx` (read and write), devMock (`mockConsentRefusal` reads `mockUiSettings`, `devMock.ts:977-985`), and flip `commands.rs:690` / `llm_commands.rs:1006`.
- Add the key to the registry table above, and to devMock if the UI reads it.
- Run `cargo test -p fly-app` (allowlist, consent reset, scheduler) and `npm --prefix frontend test` (CloudConsent, AppNotices, SettingsModal).
- Trap: `consent_skipped` is in memory only. After a restart the notice is empty even though those meetings still have no cleaned transcript or items. Backfill in Settings covers items, but nothing covers polish.
