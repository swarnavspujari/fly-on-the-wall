# App notices (`ui-notices`)

> Layer: frontend feature. Described at main `82b7edf` (v2.4.0).
> In flight: None.

## Purpose
Every app-wide notice in one bottom-right corner: a failed user action, the macOS system-audio block, a recording that just lost audio, transcription failures for notes that aren't open, skipped AI cleanup, recovered meetings set to Keep local, and the update prompt. The top notice shows and the rest sit behind "+N more", so notices never overlap. The pure modules decide priority, copy and which failures are held. Also owns the blocking restart screen after "Delete all my data" and the "Remind me to tell people" settings row.

## Files it owns
- `frontend/src/components/AppNotices.tsx` — builds the `StackedNotice` list and renders one `NoticeStack`.
- `frontend/src/notices.ts` — `NOTICE_PRIORITY`, per-OS system-audio copy, app-error copy, lost-recording-audio copy (`AudioLost`, `audioLostCopy`), consent-skip copy, update visibility and copy.
- `frontend/src/pipelineNotice.ts` — `briefError`, `selectPipelineNotice` (engine-missing / download-failed / generic), `pipelineFailureCopy`.
- `frontend/src/pipelineFailures.ts` — reducer for failures held in the stack, plus `isBackgroundFailure`, `failureFrom`, `shouldAddFailure`.
- `frontend/src/components/SystemAudioNotice.tsx` — the system-audio notice (macOS: Open System Settings).
- `frontend/src/components/RemindNoticeRow.tsx` — Settings › Recording "Remind me to tell people" switch.
- `frontend/src/components/RestartRequired.tsx` — full-window alertdialog after a wipe; no close button.
- `frontend/src/components/UpdateBanner.tsx` — update notice body (see [app-updater](app-updater.md) for `useUpdater` and the plugins).

## Surface area
### Public API
| Export | Where | Notes |
|---|---|---|
| `AppNotices` | `AppNotices.tsx:58` | Props: `os`, `recordingActive`, `systemAudioBlocked`, `appError`, `pipelineFailures`, `engine`, `updater`, callbacks. |
| `NOTICE_PRIORITY` | `notices.ts:12-22` | appError 4 > systemAudio 3 > audioLost 2.5 > pipeline 2 > consentSkips 1.5 > recoveredKeptLocal 1.2 > update 1 > compressed 0.5. |
| `audioLostCopy(lost)` | `notices.ts` | "Part of the last recording is missing": seconds lost per side, one fix step. |
| `useCompressNotice` | `CompressNotice.tsx` | The one-time "Recordings compressed" notice (`storage_report` → `compress.notice_pending`, `storage:compress`); Dismiss calls `dismiss_compress_notice`, so it never returns. Waits out a recording. Copy: `storageModel.compressNoticeCopy`. |
| `systemAudioCopy`, `appErrorCopy`, `consentSkipsCopy`, `updateNoticeVisible`, `updateCopy`, `MACOS_BUNDLE_ID`, `NoticeCopy` | `notices.ts:54-196` | Copy shape: cause → numbered fix → collapsible details. |
| `briefError(error, max=260)` | `pipelineNotice.ts:26-32` | Strips `http(s)://…`, collapses whitespace, caps with "…". |
| `selectPipelineNotice`, `pipelineFailureCopy`, `PipelineNoticeKind` | `pipelineNotice.ts:47-152` | Also used inline by the transcript panel ([ui-editor](ui-editor.md)). |
| `failuresReducer`, `FailureAction`, `PipelineFailure`, `isBackgroundFailure`, `failureFrom`, `shouldAddFailure` | `pipelineFailures.ts` | Actions: `event`, `add`, `retry`, `dismiss`, `noteDeleted`. Unchanged state returns the same array. |
| `SystemAudioNotice` | `SystemAudioNotice.tsx:19` | `os`, `onOpenSettings?`, `onDismiss`. |
| `RemindNoticeRow` | `RemindNoticeRow.tsx:9` | Rendered by `SettingsModal`. |
| `RestartRequired` | `RestartRequired.tsx:19` | Armed by the `fotw:restart-required` window event. |
| `UpdateBanner` | `UpdateBanner.tsx:15` | `updater: Updater`. |

### Tauri commands
- Calls through `useConsentSkips` (mounted by `AppNotices.tsx:59`): `consent_skipped_meetings`, `get_llm_settings`, `get_app_setting`, `retry_consent_skipped`, and `request_cloud_consent` (the backend asks in a native dialog and records a yes).
- `RemindNoticeRow`: `get_app_setting`, `set_app_setting` for `ui.recording.remind_notice` (`RemindNoticeRow.tsx:13-32`).
- `RestartRequired`: `delete_all_data` on "Try again" (`RestartRequired.tsx:75`); `relaunch()` from `@tauri-apps/plugin-process`.
- `UpdateBanner`: none directly; its buttons call `useUpdater` (`plugin:updater|check`, download/install, relaunch).
- Through App callbacks: `transcribe_meeting` (Try again, `App.tsx:1110-1114`), `open_privacy_settings` (`App.tsx:1099`), `get_note` and friends (Open note), and `list_recent_notes` to name a failure whose note isn't in the current list (`App.tsx:357-361`).
- devMock mirrors all of these, plus `plugin:updater|check` and `plugin:updater|download` (`devMock.ts:1486-1495`). Drift: the consent re-run's final event has `stage: null` (`devMock.ts:1037`) where Rust always sends `"done"` (`src-tauri/src/scheduler.rs:494-502`); `plugin:process|restart` falls to the `null` default.

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `llm:consent-skips` | listens, `CloudConsent.tsx:122` (via `AppNotices`) | `string[]` meeting ids | `src-tauri/src/scheduler.rs:208-229` (`ai_pass`) |
| `recording:audio_lost` | listens, `AppNotices.tsx` `useAudioLost` | `{meeting_id, note_id, mic_lost_ms, system_lost_ms}` | `src-tauri/src/recording.rs` `record_capture_health` (during `stop_recording`); devMock with `fotwMockAudioLost=1` |
| `privacy:recovered-kept-local` | listens, `useRecoveredKeptLocal.ts` (via `AppNotices`) | `RecoveredMeeting[]` | `src-tauri/src/lib.rs` startup self-heal ([app-privacy](app-privacy.md)) |
| `fotw:llm-settings-changed` (window) | listens, `CloudConsent.tsx:138-143` | none | `api.ts:9-11`, `CloudConsent.tsx:51` |
| `fotw:restart-required` (window) | listens, `RestartRequired.tsx:31-35` | `CustomEvent<{report: WipeReport, includeModels}>` | `PrivacySection.tsx:120-124` |
| `keydown` (window, capture phase) | listens while the restart screen shows, `RestartRequired.tsx:45-55` | — | Blocks app shortcuts; Tab and in-dialog Enter/Space pass |
| `pipeline:progress` | indirect: App feeds every event to `failuresReducer` (`App.tsx:341-363`) | `PipelineProgress` | see [ui-app-shell](ui-app-shell.md) |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_writable_setting_key`) | Meaning |
|---|---|---|---|
| `ui.recording.remind_notice` | R/W `RemindNoticeRow.tsx` | yes (`ui.`) | Start-of-recording reminder; on unless `"false"` (`recordingBar.ts:9-14`) |
| `consent.cloud_llm.<provider>` | R `CloudConsent.tsx:152` (who to name) | no: read-only to the webview; only the backend's `request_cloud_consent` records it | Cloud consent for the saved provider |
| devMock toggles | — | n/a | `fotwMockTapDenied`, `fotwMockUpdate` (`available` / `fail`), `fotwMockConsentSkips`, `fotwMockConsentReset`, `fotwMockWipeDbBusy`, `fotwMockEngineMissing`, `fotwMockEngineUnmanaged` |

### Database
None directly. Held failures come from events and, on note open, from `pipeline_failure` (the failed job row; `src-tauri/src/scheduler.rs:286-298`).

### Files and directories written
None.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| Update host (via the updater plugin) | Startup check after 5 s; download on "Update now" | See [app-updater](app-updater.md); never while recording |

### External processes
None (relaunch goes through `plugin-process`).

### Secrets and keychain entries
None. `RestartRequired` lists keychain entries a wipe couldn't remove (`WipeReport.secrets_failed`, `RestartRequired.tsx:119-120`).

## Dependencies
- **Inbound:** `App.tsx` mounts `AppNotices` (`:1095-1121`) and `RestartRequired` (`:1130`), owns the `failuresReducer` (`:87`) and calls `failureFrom`, `isBackgroundFailure`, `shouldAddFailure`. `SettingsModal` mounts `RemindNoticeRow`. The transcript panel uses `selectPipelineNotice` and `briefError`; `ImportQueue` uses `briefError`.
- **Outbound:** `ui.tsx` (`Notice`, `NoticeStack`, `useDialogFocus`), `CloudConsent.tsx` (`useConsentSkips`), `updater.ts`, `privacy.ts` (`wipeOutcome`, `RESTART_REQUIRED_EVENT`), `recordingBar.ts` (`REMIND_NOTICE_KEY`), `@tauri-apps/plugin-process`.

## Invariants
- One corner, priority order: the user's own failed action > system audio > lost recording audio > pipeline failures > skipped cleanup > recovered meetings set to Keep local > update; ties keep insertion order (`notices.ts:9-18`, `ui.tsx:1481-1486`). A failed click's alert never hides behind "+N more" (`AppNotices.tsx:75-96`).
- Recording is sacred: the launch system-audio notice, skipped cleanup and the update wait until recording stops; errors and the lost-audio notice (it's about the recording that just ended) still show (`AppNotices.tsx:61`, `:166`, `:198`).
- The open note shows its own failure inline; only a terminal error for a meeting that isn't open is held (`pipelineFailures.ts:30-32`). Any later event for that meeting, a retry, a dismiss or the note's deletion clears it (`pipelineFailures.ts:43-64`). After the async note lookup, it's added only if nothing newer happened to that meeting and its note wasn't opened (`pipelineFailures.ts:79-86`, `App.tsx:341-363`).
- "Try again" only for a first transcription; with a transcript the failed run may have been a speaker re-analysis (`AppNotices.tsx:113-115`).
- Raw errors are briefed: no signed URLs, capped length; the full text stays under Details when cut (`pipelineNotice.ts:26-32`, `notices.ts:73-82`).
- The engine notice is chosen by matching backend text: `/whisper-cli is not installed/i` (`pipelineNotice.ts:36`) ← `"{name} is not installed — …"` (`src-tauri/src/models.rs:292`, `:377-380`); `/download (failed|interrupted)/i` (`pipelineNotice.ts:38`) ← `src-tauri/src/models.rs:293`, `:530-553`, `:711`; `/^engine install failed/i` (`pipelineNotice.ts:42`) ← App's own tag (`App.tsx:749`). No test checks these against the Rust strings.
- The tccutil command names the bundle id from a TS copy (`notices.ts:32`) of `src-tauri/tauri.conf.json:5` and `crates/fly-audio/src/tap_messages.rs:11`.
- Skipped cleanup runs only once the provider is allowed: "Run" asks through `request_cloud_consent` (the backend's native dialog) when there's no consent yet, and a decline records nothing and runs nothing (`CloudConsent.tsx:163-176`). The backend enforces consent again in `build_provider` (spec §2 item 1).
- The skipped-cleanup notice reads "Review and allow" (consent missing), "Run them now", or, while the saved cloud provider has no API key, "Add a key", which calls the `onOpenLlmKeySettings` prop (App opens Settings at the key field).
- After a wipe, the restart screen blocks the app until it restarts; a clean wipe relaunches by itself after 1.5 s; other outcomes wait for the user (`RestartRequired.tsx:14-66`; `privacy.ts:127-131`).
- A failed update check stays in Settings; only a failed download/install shows in the stack (`notices.ts:108-124`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest (jsdom) | `frontend/src/components/AppNotices.test.tsx` | 25 | Stack order and "+N more", recording rules, each notice's actions, update phases, skipped-cleanup flow, with `cloudConsentTesting.ts` standing in for the native consent dialog (`listen` and `api` mocked) |
| vitest (node) | `frontend/src/notices.test.ts` | 16 | Priorities and copy |
| vitest (node) | `frontend/src/pipelineFailures.test.ts` | 15 | Reducer transitions, lookup guard |
| vitest (node) | `frontend/src/pipelineNotice.test.ts` | 20 | Notice selection, briefing, failure copy |
| vitest (jsdom) | `frontend/src/components/ui.test.tsx` (NoticeStack) | 5 | Primitive behavior (counted under [ui-app-shell](ui-app-shell.md)) |
| vitest (jsdom) | `frontend/src/components/SettingsModal.test.tsx` | — | `RestartRequired` (line 440) and the Remind row (line 469); audited with [ui-settings](ui-settings.md) |

**Known gaps:**
- The notice classifiers key on Rust error text that no test reads from Rust; a rewording in `models.rs` fails nothing (fixtures in `pipelineNotice.test.ts:4-13` are copies).
- No App-level test of the background-failure path: the async `list_recent_notes` lookup for a note outside the current folder, and the sequence guard against a retry that starts meanwhile, are only covered as pure functions.
- `MACOS_BUNDLE_ID` isn't checked against `tauri.conf.json`.
- `SystemAudioNotice` and `UpdateBanner` are only tested through `AppNotices`.
- The skipped-cleanup tests mock both `listen` and `api`, and stand in for the native consent dialog, so they never see the real dialog or the backend's consent write.

## Changing it safely
- Run `npm --prefix frontend test` (AppNotices, notices, pipelineFailures, pipelineNotice).
- Recovered meetings set to Keep local (`useRecoveredKeptLocal.ts`, `recovered_kept_local` / `dismiss_recovered_kept_local`): "N recovered meetings were set to Keep local because their privacy setting couldn't be confirmed." Review takes the first off the list and opens its note (`onOpenNote`); Dismiss clears the list. Keep local itself is never changed by the notice. Waits out a recording.
- A new notice: add a priority to `NOTICE_PRIORITY` (with its relation tests), build its copy in a pure module, and decide whether it waits out a recording.
- Changing a backend error string in `models.rs` or the pipeline: grep `pipelineNotice.ts` and `recordingBar.ts` for the phrase and update both sides in one PR.
- Any new terminal pipeline stage must be handled in `isBackgroundFailure` and `runStateOf` (`homeModel.ts:116-119`); `"cancelled"` is load-bearing.
- Keep `RestartRequired` mounted at App level so it outlives the Settings modal.
