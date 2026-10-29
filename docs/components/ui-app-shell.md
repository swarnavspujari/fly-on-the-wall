# App shell, IPC seam and primitives (`ui-app-shell`)

> Layer: frontend feature. Described at main `82b7edf` (v2.4.0), plus backend-only cloud consent (#102) and the v2.5 startup upgrade gate. Line references into `api.ts`, `devMock.ts`, `main.tsx` and `lib.rs` are as of the startup gate; `CloudConsent.tsx` and `commands.rs` as of #102.
> In flight: PR #92 and PR #86 change nothing here.

## Purpose
The React entry point, the `App` orchestrator that owns almost all cross-feature state, the single IPC seam (`api.ts`), the in-browser fake backend (`devMock.ts`), the shared UI primitives (`ui.tsx`), the theme store and the TypeScript mirror of the Rust IPC types. Every other `ui-*` component is mounted from here and talks to the backend through `api.ts`.

## Files it owns
- `frontend/src/main.tsx` — entry: installs devMock (dev only), the link guard, then `LibraryGate` around `ConfirmProvider`, `App` and `CloudConsentOnLaunch`.
- `frontend/src/components/LibraryGate.tsx` — the startup gate: asks `library_status` before anything else mounts, and shows the "Upgrading your library" screen (determinate `ProgressBar`, one line of copy) while the backend upgrades the library in the background.
- `frontend/src/App.tsx` — orchestrator (1,159 lines): state, event listeners, polling, wiring of every screen.
- `frontend/src/api.ts` — typed `invoke` wrappers (105) and the `fotw:llm-settings-changed` window event.
- `frontend/src/devMock.ts` — fake Tauri IPC for port-1420 QA and the jsdom App/RecordingBar/ImportQueue tests.
- `frontend/src/components/ui.tsx` — design-system primitives, Escape-layer stack, dialog focus.
- `frontend/src/theme.ts` — shared theme store (`useTheme`, `setTheme`).
- `frontend/src/types.ts` — hand-written TS mirrors of the Rust IPC types.
- `frontend/vite.config.ts` — Vite on port 1420 (`strictPort`). No `test` block: vitest runs with defaults; jsdom is opted into per file with `// @vitest-environment jsdom`; there are no setup files.
- `frontend/DESIGN.md` — token/primitive map that screens follow (read-only reference).

## Surface area
### Public API
| Export | Where | What |
|---|---|---|
| `App` (default) | `frontend/src/App.tsx:61` | The whole app body. Takes no props. |
| `LibraryGate` (default), `LIBRARY_UPGRADE_EVENT`, `LIBRARY_POLL_MS`, `upgradePercent` | `frontend/src/components/LibraryGate.tsx` | Renders its children only once `library_status` (or a `library:upgrade` event) says `ready`; until then the upgrade screen (or nothing, for the few ms of the first call). A failed call mounts the children. |
| `api` | `frontend/src/api.ts:51-272` | One method per command (table below). Components never call `invoke` directly (`frontend/src/api.ts:1-2`), with one exception: `openExternal` in `frontend/src/safeContent.ts:39` invokes `open_external_url` itself. |
| `LLM_SETTINGS_CHANGED_EVENT` | `frontend/src/api.ts:8` | `"fotw:llm-settings-changed"`, dispatched after `setLlmSettings` resolves (`frontend/src/api.ts:177-178`). |
| `resetMockAppSettings()` | `frontend/src/devMock.ts:884-887` | The only devMock reset helper: restores the get/set_app_setting store. |
| `useTheme`, `setTheme`, `ThemeChoice`, `ResolvedTheme` | `frontend/src/theme.ts:9-76` | Module-level store over `useSyncExternalStore`. |
| Types + `WHISPER_ENGINE_ID` | `frontend/src/types.ts` | `"whisper-bin"` (`frontend/src/types.ts:194`) mirrors `models::WHISPER_ENGINE_ID` (`src-tauri/src/models.rs:281`). |

`ui.tsx` primitives (token CSS injected once as `<style id="fly-ds-css">`, `frontend/src/components/ui.tsx:274-279`):

| Group | Exports |
|---|---|
| Core | `Button` (6 variants × 4 sizes), `SectionLabel`, `Card`, `Input`, `Select`, `Checkbox`, `Badge`, `ProgressBar`, `RecordingIndicator`, `Avatar`, `speakerInitials`, `CitationChip` |
| Layers | `pushEscapeLayer` (`:855`), `escapeLayerDepth` (`:867`), `EscapeLayerScope` (`:875`), `useEscapeLayer` (`:883`) — one module-level stack (`:834`); deepest layer wins, ties go to the newest; the keydown listener skips events already `defaultPrevented`. |
| Focus | `useDialogFocus(ref, active, {trap, autoFocus, restoreFocus})` (`:924-976`) — focus in on open (skips `[data-dialog-close]`), optional Tab trap, focus back on close (`"always"` or `"inside"`). |
| Dialogs | `Modal` (`:991`, Esc via the stack, Tab trapped, overlay click closes), `Sheet` (`:1334`, bottom/right, optional `modal` scrim + trap) |
| Settings | `Switch` (`:1079`, `role="switch"`), `Segmented` (`:1129`, radiogroup with roving tabindex and arrow/Home/End), `SettingsGroup` (`:1212`, "More options" footer), `SettingsRow` (`:1270`) |
| Notices | `Notice` (`:1421`, `role="alert"` for danger else `status`), `StackedNotice`, `orderNotices` (`:1481`, stable by priority), `NoticeStack` (`:1500`, one corner, "+N more") |
| Deprecated shims | `Btn`, `ModalShell`, `speakerColor` (`:1544-1593`), kept for screens not yet restyled |

### Tauri commands
- Calls (via `api.ts`, all 112 registered commands are wrapped or invoked): `library_status`, `ping`, `app_info`, `list_folders`, `create_folder`, `rename_folder`, `move_folder`, `delete_folder`, `create_note`, `get_note`, `list_notes_in_folder`, `list_recent_notes`, `update_note_title`, `update_note_scratchpad`, `move_note`, `delete_note`, `attach_file`, `export_transcript_vtt`, `export_note`, `copy_note_markdown`, `remove_attachment`, `open_attachment`, `ensure_video_thumbnail`, `reveal_attachment`, `reveal_data_dir`, `reveal_logs_dir`, `list_capture_windows`, `mcp_config`, `get_app_setting`, `set_app_setting`, `search`, `search_semantic`, `recording_status`, `start_recording`, `pause_recording`, `resume_recording`, `stop_recording`, `get_meeting_for_note`, `get_meetings_for_note`, `live_status`, `update_meeting_started_at`, `transcribe_meeting`, `get_transcript`, `get_cleaned_transcript`, `polish_transcript`, `relabel_speaker`, `edit_transcript_segment`, `pipeline_stage`, `pipeline_failure`, `update_meeting_attendees`, `re_diarize_meeting`, `revert_speaker_assignment`, `speaker_undo_state`, `get_asr_settings`, `set_asr_settings`, `download_model`, `enhance_note`, `edit_note_block`, `ask_meeting`, `cancel_ask`, `list_ask_people`, `insert_ask_answer`, `add_action_item`, `list_templates`, `save_template`, `delete_template`, `get_llm_settings`, `set_llm_settings`, `test_llm_connection`, `consent_skipped_meetings`, `retry_consent_skipped`, `request_cloud_consent`, `extract_meeting_items`, `backfill_meeting_items`, `pending_backfill_count`, `ollama_status`, `ollama_pull`, `ollama_delete`, `get_calendar_settings`, `set_calendar_settings`, `connect_calendar`, `disconnect_calendar`, `list_calendars`, `set_calendar_enabled`, `upcoming_meetings`, `start_meeting_from_event`, `list_mic_devices`, `preflight_system_audio`, `open_privacy_settings`, `screen_status`, `start_screen_recording`, `stop_screen_recording`, `import_stage`, `import_state`, `import_transcribe`, `cancel_transcription`, `get_meeting_privacy`, `set_note_keep_local`, `set_folder_keep_local`, `list_local_only_folders`, `set_participants_notified`, `copy_recording_notice`, `get_privacy_settings`, `preview_audio_retention`, `set_audio_retention`, `dismiss_wipe_report`, `delete_all_data`, `disk_encryption_status`, `keep_note_block`; plus `open_external_url` from `safeContent.ts`. Registration: `src-tauri/src/lib.rs:94-207`.
- Called before `App` mounts: `library_status`, by `LibraryGate` (`api.libraryStatus`). It's the only command the frontend may call while the backend has no `AppState` (see [app-shell](app-shell.md)).
- Wrappers with no UI caller today: `ping`, `move_folder`, `polish_transcript`, `extract_meeting_items` (inferred from a grep for `api.<name>` outside tests).
- Called by `App.tsx` itself: `app_info`, `list_folders`, `create_folder`, `rename_folder`, `delete_folder`, `list_recent_notes`, `list_notes_in_folder`, `create_note`, `get_note`, `move_note`, `delete_note`, `get_asr_settings`, `get_llm_settings`, `get_calendar_settings`, `list_templates`, `get_app_setting`, `set_app_setting`, `preflight_system_audio`, `open_privacy_settings`, `upcoming_meetings`, `connect_calendar`, `recording_status`, `screen_status`, `start_recording`, `pause_recording`, `resume_recording`, `stop_recording`, `start_meeting_from_event`, `get_meetings_for_note`, `get_meeting_for_note`, `get_transcript`, `get_cleaned_transcript`, `pipeline_stage`, `pipeline_failure`, `import_stage`, `import_state`, `import_transcribe`, `cancel_transcription`, `transcribe_meeting`, `download_model`, `relabel_speaker`, `edit_transcript_segment`, `start_screen_recording`, `stop_screen_recording`, `search`, `search_semantic`.
- **devMock mirrors 103 of 112.** Not mirrored (they fall to `default: return null`, `frontend/src/devMock.ts:1541-1542`): `attach_file`, `remove_attachment`, `open_attachment`, `reveal_attachment`, `open_external_url`, `polish_transcript`, `extract_meeting_items`, `start_screen_recording`, `stop_screen_recording`. Spec §2 item 4 requires every command to be mirrored.
- Drift found (devMock vs Rust, each checked in code):

| # | devMock | Real backend | Effect |
|---|---|---|---|
| 1 | `start_screen_recording` → `null` | `ScreenStatus` | `App.tsx:840` stores `null`; the next render reads `screenStatus.active` (`App.tsx:186`) and throws. No error boundary exists, so port-1420 QA of screen capture blanks the app (inferred). `stop_screen_recording` → `null` throws inside the `try` at `App.tsx:858` and shows as an app error. |
| 2 | `get_app_setting` / `set_app_setting` accept any key, except that `set_app_setting` refuses `consent.cloud_llm.*` like the backend (`devMock.ts:1399-1414`) | Only `consent.` / `capture.` / `ui.` keys, else `"setting '<key>' can't be accessed from the app UI"`; `set_app_setting` also refuses `consent.cloud_llm.*` (`src-tauri/src/commands.rs:59-96`) | A new UI setting under another prefix passes vitest and QA, fails in the app. |
| 3 | `transcribe_meeting` emits nothing (`devMock.ts:1130-1136`) | `scheduler::enqueue` emits `"waiting"` during the call (`src-tauri/src/scheduler.rs:55-67`, `:472-490`) | The "waiting"/"starting" ordering race in `stopRecording` (see Invariants) can't show up in tests. |
| 4 | `pipeline_stage` always `null` (`devMock.ts:1235-1236`) | The live stage map, `"waiting"` while queued (`src-tauri/src/asr_commands.rs:193-200`) | Reopening a queued note never shows its stage in QA. |
| 5 | Some errors are `Error` objects (`devMock.ts:403`, `:1497`) | Tauri rejects with the plain string | `String(e)` reads `"Error: …"` in QA/tests only. |
| 6 | `retry_consent_skipped` ends with `stage: null` (`devMock.ts:1044`) | `stage` is always a string; terminal is `"done"` (`src-tauri/src/scheduler.rs:494-502`) | Payload shape the backend never sends. |
| 7 | `start_recording` / `start_meeting_from_event` always report note `n1` / meeting `m1`; `create_note` always returns `n3`; `get_meetings_for_note` returns at most one meeting; `update_meeting_attendees` always returns `n1`'s meeting (`devMock.ts:1148-1149`, `:1161-1164`, `:1216-1220`, `:1251-1273`) | Per-note | App tests override these per test (`App.test.tsx:437-456`). |
| 8 | `delete_note` just removes the fixture row (`devMock.ts:1081-1085`) | Refuses the recording note, cancels and purges its meetings (emits `"cancelled"`) (`src-tauri/src/commands.rs:281-312`) | — |
| 9 | `app_info` has no `appimage` (`devMock.ts:1101-1111`) | Always present (`src-tauri/src/commands.rs:20-28`) | `App.tsx:187` defaults it. |
| 10 | `plugin:event|unlisten` returns without removing the handler (`devMock.ts:1661`) | Real unlisten removes it | Stale handlers of unmounted components keep receiving `fotwMockEmit` within one module instance. |

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `library:upgrade` | listens, `LibraryGate.tsx` (registered before `library_status` is called; dropped on unmount) | `{phase, done, total}` (`LibraryStatus`, `types.ts`) | `src-tauri/src/startup.rs:64-67` (background open only) |
| `pipeline:progress` | listens, `frontend/src/App.tsx:373-449` | `{meeting_id, stage, detail, done, error}` | `src-tauri/src/scheduler.rs:457-466` (`stage_emitter`), `src-tauri/src/pipeline.rs:103-131`; terminal `"done"`/`"error"` `scheduler.rs:494-502`, `"cancelled"` `scheduler.rs:369-377`, `"waiting"` `scheduler.rs:472-490`, `"polishing"` `scheduler.rs:390` |
| `model:progress` | listens, `frontend/src/App.tsx:450` | `{id, downloaded, total, stage, error}` (`src-tauri/src/models.rs:389-395`) | `asr_commands.rs:118,412`, `scheduler.rs:162`, `ollama.rs:303`, `embeddings.rs:136`, `import_commands.rs:277`, `screen_commands.rs:95,182`, `live.rs:105` |
| `fotw:llm-settings-changed` (window) | emits, `frontend/src/api.ts:9-11` | none | `CloudConsent.tsx:141` listens. **`App.tsx` does not**, so the sidebar privacy pill goes stale (see [ui-home](ui-home.md)). |
| `keydown` Ctrl/Cmd+K (window) | listens, `App.tsx:325-334` | — | opens Settings |
| `resize` (window) | listens, `App.tsx:311-322` | — | narrow mode below 880 px (`App.tsx:50`) |
| `change` on `prefers-color-scheme` | listens, `theme.ts:43-50` | — | re-resolves `system` theme |

Children subscribe to `import:progress` ([ui-import](ui-import.md)), `live:segment` / `live:status` ([ui-recording-bar](ui-recording-bar.md)) and `llm:consent-skips` ([ui-notices](ui-notices.md)).

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `consent.recording_notice_accepted` | R `App.tsx:268,282`; W `App.tsx:1135` | yes (`consent.`) | First-run recording notice accepted; also gates the macOS probe. |
| `localStorage["flyonthewall.notes-cache.v1"]` | R `App.tsx:70`; W `App.tsx:238` | n/a (browser) | Last "All notes" list ([ui-home](ui-home.md)). |
| `localStorage["fotw-theme"]` | R/W `theme.ts:12-61`; R `frontend/index.html:13` | n/a | `system` / `light` / `dark`. |
| devMock query string `?fotwMockUpgrade=1` / `hold` / `fail` (read at load) | R `devMock.ts` | n/a | `library_status` starts `upgrading` and `library:upgrade` events walk the bar to `ready` in about 6 s (`hold`: stays at 40 %; `fail`: ends `failed` at 70 %). Without it, `library_status` is `ready`. |
| devMock toggles (`localStorage`, read per call unless noted) | R `devMock.ts` | n/a | `fotwMockRecording` (also written by the mock on start/stop, `:1271`, `:1539`), `fotwMockFirstRun`, `fotwMockNoTranscript`, `fotwMockEngineMissing`, `fotwMockEngineUnmanaged`, `fotwMockTapDenied`, `fotwMockEncryption`, `fotwMockAudioDeleted`, `fotwMockOllama`, `fotwMockGpuBench` (load time), `fotwMockUpdate`, `fotwMockImportFail`, `fotwMockImportPipelineFail`, `fotwMockRecWarning`, `fotwMockNoAttendees`, `fotwMockConsentReset` (load time), `fotwMockConsentSkips` (load time), `fotwMockAskError`, `fotwMockCalendar` (`none` read at install), `fotwMockNoNotes`, `fotwMockRetentionHeld`, `fotwMockWipeDbBusy`. The last four are missing from the header list (`devMock.ts:11-70`). QA hook: `window.fotwMockEmit(event, payload)` (`devMock.ts:1670-1672`). |

### Database
None (frontend; all storage is behind commands).

### Files and directories written
None directly. Browser `localStorage` only (keys above).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None from the webview | — | All network work runs in Rust behind commands; CSP blocks remote scripts (THREAT_MODEL T2). |

### External processes
None.

### Secrets and keychain entries
None. API keys go one way through `set_llm_settings` / `set_asr_settings` and never come back (`LlmProviderInfo.has_key`, `frontend/src/types.ts:267-274`).

## Dependencies
- **Inbound:** `frontend/index.html` loads `main.tsx`. Every `ui-*` component imports `api`, `types` and `ui.tsx`.
- **Outbound:** `@tauri-apps/api` (`core.invoke`, `event.listen`), `@tauri-apps/plugin-updater` and `@tauri-apps/plugin-process` (via `updater.ts`, see [app-updater](app-updater.md)), React 19, lucide-react. App mounts [ui-home](ui-home.md) (`Sidebar`, `NoteList`, `Home`, `UndoToast`, `FirstRunNotice`), [ui-notices](ui-notices.md) (`AppNotices`, `RestartRequired`), [ui-recording-bar](ui-recording-bar.md) (`RecordingBar`), [ui-editor](ui-editor.md) (`Editor`, which mounts [ui-import](ui-import.md) and `LivePane`), [ui-settings](ui-settings.md) (`SettingsModal`), [ui-confirm-consent](ui-confirm-consent.md) (`ConfirmProvider`, `CloudConsentOnLaunch`). Also `calendarMatch.ts`, `noteDeletion.ts`, `pipelineFailures.ts`, `homeModel.ts`.

### What `App.tsx` holds
| Kind | Items |
|---|---|
| State (37 `useState` + 1 `useReducer`) | `info`, `tapDenied`, `folders`, `selection`, `notes`, `openNote`, `openMeeting`, `noteMeetings`, `transcript`, `cleanedTranscript`, `pipeStage`, `pipeDetail`, `pipelineError`, `pipelineFailures` (reducer), `importStaged`, `modelProgress`, `engine`, `installingEngine`, `searchQuery`, `searchHits`, `recStatus`, `screenStatus`, `screenSource`, `autoTranscribe`, `templates`, `upcoming`, `calendarNeedsReconnect`, `calendarStatus`, `calendarBusy`, `llmSettings`, `asrPrivacy`, `liveRuns`, `showSettings`, `settingsFocus`, `showFirstRun`, `error`, `narrow`, `drawerOpen` (`App.tsx:63-182`) |
| Refs (11) | `failureSeqRef`, `liveRunsRef`, `openMeetingIdRef`, `openNoteIdRef`, `importStagedRef`, `notesRef`, `firstNotesLogged`, `notesSeq`, `refreshNotesRef`, `searchSeq`, `openSeq` |
| Effects | notes fetch first (`:253-255`); startup loads (`:257-271`); macOS consent probe (`:279-291`); Up next every 5 min (`:294-298`); recording + screen status poll every 1 s (`:301-307`); narrow resize (`:311-322`); Ctrl/Cmd+K (`:325-334`); event listeners (`:337-455`); two-pass search, FTS at 200 ms then hybrid at 450 ms (`:462-495`) |
| Children wired | `RecordingBar` (`:983`), narrow menu bar (`:998`), `Sidebar` (`:924`), `NoteList` (`:961`), `Editor` or `Home` (`:1017-1072`), narrow drawer (`:1074`), `AppNotices` (`:1095`), `UndoToast` (`:1122`), `RestartRequired` (`:1130`), `FirstRunNotice` (`:1131`), `SettingsModal` (`:1139`) |

Business logic in `App.tsx` that arguably belongs elsewhere (ARCHITECTURE.md:10 says the frontend is "thin; no business logic"):
- Pipeline event state machine: failure tracking with per-meeting sequence numbers (`:341-363`), queued/running map (`:366-372`), the import-to-normal-note transition on `done` (`:398-430`), early raw-transcript fetch on `polishing` (`:437-447`). A `usePipelineEvents` hook would hold it.
- Race guards for note open and meeting switch (`openSeq`, `failureSeqRef`, `:528-579`, `:784-807`).
- "Transcribe when I stop" is decided client-side: `stopRecording` calls `transcribe_meeting` after `stop_recording` (`:714-736`). If the webview dies between the two calls, nothing is queued (inferred). `RecordingBar` reads the same setting again on its own (`RecordingBar.tsx:107-128`).
- Calendar seeding of a plain Record press (`:688-712`, via `calendarMatch.ts`).
- Delete orchestration and the "recording note can't be deleted" guard (`:597-619`), duplicating `NoteList.tsx:191` and `src-tauri/src/commands.rs:284-287`.
- Engine install flow (`:740-756`) and live-chip derivation (`:894-919`).

## Invariants
- The macOS system-audio probe runs only after `consent.recording_notice_accepted` is `"true"` (spec §2 item 6; `App.tsx:279-291`). Accepting the first-run notice doesn't re-run it this session; the effect only reruns when `info` changes (inferred; safe direction).
- devMock is installed only when `import.meta.env.DEV` and there is no `__TAURI_INTERNALS__`; production builds drop it (`devMock.ts:1686-1688`, `main.tsx:1-4`). It must be the first import in `main.tsx`.
- devMock mirrors every command (spec §2 item 4). **Violated for 9 commands** (see Tauri commands).
- Generic settings only under `consent.` / `capture.` / `ui.` (spec §2 item 2, THREAT_MODEL T3; `src-tauri/src/commands.rs:59-66`); `consent.cloud_llm.*` is read-only to the webview (`commands.rs:68-79`). The shell writes only `consent.recording_notice_accepted`.
- All IPC goes through `api.ts` (`api.ts:1-2`). The only exception is `open_external_url` in `safeContent.ts:39`. No lint rule enforces it.
- Links open in the system browser, never in the window (THREAT_MODEL T1/T5; `main.tsx:16`, `safeContent.ts:46`).
- Nothing that calls a command other than `library_status` mounts before `LibraryGate` sees `ready` (`main.tsx:22-28`): during a startup upgrade the backend has no `AppState`. A failed `library_status` call (older backend, plain dev) mounts the app as before. The gate listens for `library:upgrade` before it asks, keeps `ready` once seen, never moves the bar backwards, and polls every second while upgrading, so a missed event can't strand it.
- Every confirm is the in-app dialog; `ConfirmProvider` wraps `App` (`main.tsx:24-27`); ESLint bans `confirm`/`alert` (`frontend/eslint.config.js:5-38`).
- The newest note open wins: `openSeq` invalidates older fetch chains; `openMeetingIdRef` is set before the reads so an event during them reaches the inline state (`App.tsx:533-569`, PR #100).
- A deleted note never comes back into the editor or lists (`App.tsx:532`, `:638`, `:883-890`).
- Recording is sacred: the update notice is hidden and Settings disables install while anything records (`App.tsx:184-187`, see [app-updater](app-updater.md)).
- Tokens come from `index.css`; primitives from `ui.tsx` (spec §2 item 8; `frontend/DESIGN.md`).
- `types.ts` is hand-written; nothing generates or checks it against Rust (no ts-rs/specta in any `Cargo.toml`). Known soft spots: `NoteSummary` status fields optional in TS (cached rows lack them) but always sent (`crates/fly-storage/src/notes.rs:29-38`); `ImportFile.convert_error` optional in TS, always serialized (`src-tauri/src/import_commands.rs:44-47`); `ModelProgress.stage` is a TS union over a Rust `String` (`src-tauri/src/models.rs:393`); `LiveSegment`/`LiveStatus` live in `LivePane.tsx:7-18`, not `types.ts`; `WHISPER_ENGINE_ID` is copied. `SystemAudioPreflight` is the one shape a Rust serde test pins (`crates/fly-audio/src/lib.rs:99-115`, `:151`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest (jsdom) | `frontend/src/components/LibraryGate.test.tsx` | 7 | Ready → app mounts; upgrading → screen with the bar, then the app after the `ready` event; a finish between listen and the status answer; the poll after a missed event; no backwards bar; command error → app mounts; the failed state |
| vitest (jsdom, devMock backend) | `frontend/src/App.test.tsx` | 20 | Deferred delete in the real App, live chips, the import queue after a finished/failed call, failure carry-over between meetings, events during a note open |
| vitest (jsdom) | `frontend/src/components/ui.test.tsx` | 30 | Switch, Segmented, SettingsGroup/Row, Escape layers, Sheet, dialog focus, NoticeStack |
| vitest (jsdom) | `frontend/src/components/confirm.test.tsx`, `CloudConsent.test.tsx` | — | `ConfirmProvider` and launch consent (owned by [ui-confirm-consent](ui-confirm-consent.md)) |
| vitest (node) | `frontend/src/safeContent.test.ts` | — | Link guard and sanitizing (not audited here) |

**Known gaps:**
- No test checks that devMock mirrors `lib.rs` (9 commands missing, see drift table). A vitest that parses `src-tauri/src/lib.rs` `generate_handler!` and asserts every name has a `case` in `devMock.ts` would catch it in CI.
- No test for the macOS probe gate (spec §2 item 6): nothing asserts `preflight_system_audio` is never invoked before `consent.recording_notice_accepted` is `"true"`.
- `main.tsx` is untested (the gate is tested on its own); App.test renders `App` inside its own `ConfirmProvider` and leaves out `CloudConsentOnLaunch`.
- App.test's backend is devMock, which never emits `"waiting"` on `transcribe_meeting` and always answers `pipeline_stage` with `null`; event-ordering races with the real scheduler are untested.
- `theme.ts`, `notesCache.ts`, `calendarMatch.ts` and `types.ts` drift have no tests.
- `docs/TESTING.md` doesn't describe the vitest suite or devMock at all.

## Changing it safely
- Run `npm --prefix frontend test`, `npm --prefix frontend run lint`, and `tsc -b` (CI's frontend job, Linux only).
- A new command needs four edits together: `lib.rs` registration, an `api.ts` wrapper, a `devMock.ts` `case` with the real return shape (never the `null` default), and `types.ts`. Throw plain strings from devMock, like Tauri does.
- A new setting under the generic commands must use `consent.`, `capture.` or `ui.`; devMock won't catch a wrong prefix.
- Keep `import "./devMock"` the first line of `main.tsx`.
- New devMock toggles: add them to the header list (`devMock.ts:11-70`). Module-level fixture state is mutated by many commands; tests isolate it only with `vi.resetModules()` (App.test) or `resetMockAppSettings()`.
- `App.tsx` handlers registered once read state through refs (`openMeetingIdRef`, `refreshNotesRef`, …). A new value the `pipeline:progress` handler needs must get a ref too, or it reads the first render's value.
- `ui.tsx` Escape and focus behavior is global (one module-level stack). Register Esc handlers through `useEscapeLayer`, not a raw `keydown` listener.
- Mirror Rust type changes in `types.ts` by hand; check the serde attributes (`default`, `skip_serializing_if`).
