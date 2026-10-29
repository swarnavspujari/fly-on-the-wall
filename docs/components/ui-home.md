# Home, note list and sidebar (`ui-home`)

> Layer: frontend feature. Described at main `82b7edf` (v2.4.0).
> In flight: None. (PR #92 changes storage, not the list summary shape.)

## Purpose
What you see when no note is open, plus the two left columns that are always there: the sidebar (folders, Up next, privacy pill, Import, Settings) and the note list (date groups, status chips, search results, delete with Undo). Home's rules live in the pure `homeModel.ts`; deletion is deferred behind an Undo toast.

## Files it owns
- `frontend/src/components/Home.tsx` — no-note-open screen: greeting, Up next card, expired-calendar banners, empty state.
- `frontend/src/components/NoteList.tsx` — search box, New note, date-grouped rows with status chips, search hits, hover trash.
- `frontend/src/components/Sidebar.tsx` — logo, All notes, Up next list, folder tree (create/rename/delete, keep-local lock, color/emoji), Unfiled, privacy pill, Import, Settings; drag-drop filing.
- `frontend/src/homeModel.ts` — pure rules: date groups, row dates, status chips, Up next, greeting, privacy summary, `isLoopbackUrl`.
- `frontend/src/notesCache.ts` — last "All notes" list in `localStorage` for an instant first paint.
- `frontend/src/deferredDelete.ts` — `useDeferredDelete`: commit only after the Undo window.
- `frontend/src/noteDeletion.ts` — `useNoteDeletion`: App's delete flow (hide now, delete later, restore on failure).
- `frontend/src/components/UndoToast.tsx` — "Deleted … · Undo" toast (bottom-left).
- `frontend/src/components/PrivacyPill.tsx` — sidebar footer pill with hover/focus tooltip.
- `frontend/src/components/FirstRunNotice.tsx` — first-run recording-consent modal.

## Surface area
### Public API
| Export | Where | Notes |
|---|---|---|
| `Home` | `Home.tsx:42` | Props: `recording`, `upcoming`, `calendar`, `needsReconnect`, `calendarBusy`, `hasNotes`, callbacks. Clock refreshes every 30 s (`:58-63`). |
| `NoteList` | `NoteList.tsx:54` | Props: `notes`, `searchQuery`, `searchHits`, `selectedNoteId`, `live: LiveNoteState`, callbacks. Clock refreshes every 60 s (`:67-71`). |
| `Sidebar`, `Selection` | `Sidebar.tsx:155`, `:23` | `Selection = {view:"all"} \| {view:"unfiled"} \| {view:"folder", id}`. Uses `useConfirm` (needs `ConfirmProvider`). |
| `UndoToast` | `UndoToast.tsx:17` | `titles`, `onUndo`, `onDismiss` (dismiss = delete now). |
| `PrivacyPill` | `PrivacyPill.tsx:15` | `summary: PrivacySummary`, `onOpen`. Esc closes the tooltip through the layer stack. |
| `FirstRunNotice` | `FirstRunNotice.tsx:12` | `onAccept`. No close button, no Esc, no overlay close. |
| `daysAgo`, `dateGroupLabel`, `groupNotesByDate`, `rowDateLabel` | `homeModel.ts:18-79` | Rolling windows: Today, Yesterday, Previous 7/30 days, then month; future = "Upcoming". |
| `noteStatus`, `runStateOf`, `LiveNoteState`, `ChipTone` | `homeModel.ts:83-142` | Chip precedence below. |
| `videoEvents`, `nextVideoEvent`, `meetingApp`, `upNextWhen`, `eventGuests`, `upNextState`, `CALENDAR_NAMES`, `greeting` | `homeModel.ts:146-229` | Up next only lists events with a join link. |
| `isLoopbackUrl`, `privacySummary`, `AI_USES`, `SEARCH_USE`, `PrivacySummary` | `homeModel.ts:241-336` | Mirrors `llm_commands.rs::is_loopback_url` (`src-tauri/src/llm_commands.rs:522-533`). |
| `readNotesCache`, `writeNotesCache` | `notesCache.ts:17-31` | Drops `job_status` both ways. |
| `useDeferredDelete`, `UNDO_DELETE_MS` (8 000) | `deferredDelete.ts:9,30` | `pending`, `schedule`, `undo`, `flush`. |
| `useNoteDeletion`, `DeleteItem` | `noteDeletion.ts:43` | `pending`, `hiddenIds`, `isHidden`, `request`, `undo`, `flush`. |

Status chip precedence (`homeModel.ts:128-142`): Recording > live Transcribing > live Queued > this session's Failed > stored `job_status` running/queued/failed > Enhanced > Ready to enhance > Notes only (no meeting) > Not transcribed (`has_transcript === false`) > no chip (summary cached before the fields existed).

### Tauri commands
- Calls directly: `list_local_only_folders` (`Sidebar.tsx:195-198`), `set_folder_keep_local` (`Sidebar.tsx:221-224`).
- Calls through App callbacks: `create_folder`, `rename_folder`, `delete_folder`, `move_note` (drag-drop filing), `create_note`, `delete_note` and the list refresh `list_recent_notes` / `list_notes_in_folder` (via `useNoteDeletion`, `App.tsx:517-525`), `start_recording`, `start_meeting_from_event`, `connect_calendar`, `import_stage`. The pill's data comes from `get_asr_settings` and `get_llm_settings` read by App.
- devMock mirrors all of them, with drift: `create_folder` returns a folder but `list_folders` keeps returning the three fixtures, and `rename_folder`, `delete_folder` and `move_note` are no-ops (`frontend/src/devMock.ts:1078-1091`), so folder edits and drag-drop filing show no change in port-1420 QA. `delete_note` has no recording guard and emits no `"cancelled"` (`devMock.ts:1049-1053` vs `src-tauri/src/commands.rs:281-312`).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `fotw:privacy-changed` (window) | emits, `Sidebar.tsx:223` (after a folder keep-local change) | none | `Editor.tsx:1038` re-reads the note's keep-local chip |
| `mouseenter` / `focus` | listens, `PrivacyPill.tsx:24-31` | — | shows the tooltip |
| `dragstart` / `drop` (`text/plain` = note id) | `NoteList.tsx:43-52` / `Sidebar.tsx:252-275` | note id | `App.tsx:629-633` `moveNoteToFolder` |

No Tauri events. The list's live chips come from App's `pipeline:progress` handling (`App.tsx:366-379`, `:894-919`).

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `consent.recording_notice_accepted` | W by App when `FirstRunNotice` is accepted (`App.tsx:1133-1136`) | yes (`consent.`) | First-run notice done |
| `localStorage["flyonthewall.notes-cache.v1"]` | R/W `notesCache.ts:8,17-31` | n/a | Last "All notes" list, `job_status` stripped |
| `localStorage["fotw-folder-meta"]` | R/W `Sidebar.tsx:146,231-247` | n/a | Per-folder color and emoji (frontend only) |
| devMock toggles | — | n/a | `fotwMockCalendar` (`none` / `expired` / `empty`), `fotwMockNoNotes`, `fotwMockFirstRun` (`devMock.ts:734-738`, `:1104`, `:1366-1371`) |

### Database
None directly. List rows come from `list_recent_notes` / `list_notes_in_folder` (`NoteSummary`, `crates/fly-storage/src/notes.rs:16-38`).

### Files and directories written
None.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None from these files | — | Calendar sign-in runs in Rust after a click (`connect_calendar`). |

### External processes
None.

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `App.tsx` mounts `Sidebar` (`:924`), `NoteList` (`:961`), `Home` (`:1056`), `UndoToast` (`:1122`), `FirstRunNotice` (`:1131`), calls `useNoteDeletion` (`:517`), `privacySummary` (`:921`), `runStateOf` (`:376`), `readNotesCache`/`writeNotesCache` (`:70`, `:238`).
- **Outbound:** `api.ts`, `ui.tsx` primitives, `confirm.tsx` (`useConfirm`), `privacy.ts` (`PRIVACY_CHANGED_EVENT`), `settingsModel.ts` (`providerVendor`), brand SVGs.

## Invariants
- Up next shows only events with a join link; link-less events still reach App so a plain Record press can match them (`homeModel.ts:146-150`, `Sidebar.tsx:179`, `App.tsx:692`).
- Live state beats the stored row, and a `"waiting"` event means Queued, not Transcribing (`homeModel.ts:116-142`; scheduler's `WAITING_STAGE`, `src-tauri/src/scheduler.rs:26`).
- The privacy pill reads the **saved** settings, is `null` until both ASR and LLM settings load, counts every non-local provider, and treats a non-loopback Ollama as off-device (THREAT_MODEL T10; `homeModel.ts:276-336`). **Gap:** App re-reads the LLM settings only at launch and when Settings closes (`App.tsx:130-132`, `:264`, `:1153`). The Ask sheet's "Runs on" select saves a new provider (`AskPanel.tsx:357`) and fires `fotw:llm-settings-changed`, which App doesn't listen to, so the pill can say "On this computer" while Ask now sends to a cloud provider.
- `isLoopbackUrl` matches Rust's `is_loopback_url` (`homeModel.ts:241-253` vs `src-tauri/src/llm_commands.rs:522-533`). The two test tables are separate, so parity isn't checked.
- Nothing is deleted before the Undo window ends (8 s) or the toast is closed; a second delete joins the toast and restarts the timer; unmounting deletes nothing (`deferredDelete.ts:25-72`).
- A deleted note is hidden from the moment it's requested until the delete and the list refresh land; a failed delete restores it with "Couldn’t delete “…”: <cause>" (`noteDeletion.ts:67-95`). If the delete succeeds but the refresh then fails, the same `catch` un-hides the note and reports a failed delete (`noteDeletion.ts:70-81`, inferred).
- The note being recorded has no trash (`NoteList.tsx:188-204`); App also refuses (`App.tsx:600-603`) and so does the backend (`src-tauri/src/commands.rs:284-287`).
- Keeping a folder local asks first (in-app confirm, async); turning it off doesn't ask; the toggle is optimistic and rolls back on error (`Sidebar.tsx:200-225`). Deleting a folder asks first (`Sidebar.tsx:383-396`).
- The notes cache never stores the transient `job_status` (`notesCache.ts:10-15`).
- The first-run notice can only be closed by ticking the checkbox and pressing "Got it" (THREAT_MODEL T12; `FirstRunNotice.tsx:8-16`, `:34-43`). Nothing in the backend blocks recording before it is accepted (inferred: `startRecording` doesn't check it, `App.tsx:688-712`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest (jsdom) | `frontend/src/components/Home.test.tsx` | 21 | Home (10), NoteList (7), PrivacyPill (2), UndoToast (2) |
| vitest (jsdom) | `frontend/src/components/Sidebar.test.tsx` | 5 | Folder delete and keep-local confirm (real in-app dialog, async) |
| vitest (node) | `frontend/src/homeModel.test.ts` | 34 | Every pure rule above |
| vitest (jsdom) | `frontend/src/deferredDelete.test.tsx` | 7 | Undo window, batching, flush, unmount |
| vitest (jsdom, devMock) | `frontend/src/App.test.tsx` (deleting a note) | 7 | `useNoteDeletion` inside the real App |

**Known gaps:**
- No test that the sidebar pill follows a provider change made outside Settings (the staleness gap above).
- No shared vector table between `isLoopbackUrl` and Rust's `is_loopback_url`.
- `noteDeletion.ts` has no unit test; the refresh-fails-after-delete path is untested.
- `notesCache.ts` and `FirstRunNotice.tsx` have no tests.
- Sidebar's Up next list, Reconnect rows, folder create/rename, drag-drop filing and color/emoji picker are untested; devMock can't show folder edits either.
- `dateGroupLabel` and `rowDateLabel` use `toLocale*String(undefined, …)`, so their tests assume an English ICU locale.

## Changing it safely
- Run `npm --prefix frontend test` (Home, Sidebar, homeModel, deferredDelete, App tests).
- A new chip state goes in `noteStatus` and `homeModel.test.ts`; keep the precedence order documented there.
- A new provider kind must be classified in `privacySummary`; if it changes what counts as on-device, change `isLoopbackUrl` and Rust's `is_loopback_url` together.
- Anything that changes the saved LLM or ASR provider should make App re-read them (today only Settings close does).
- `Sidebar` calls `api` directly for folder keep-local; keep its devMock cases (`set_folder_keep_local`, `list_local_only_folders`) in step.
- Never call `window.confirm`; use `useConfirm` (ESLint enforces it).
