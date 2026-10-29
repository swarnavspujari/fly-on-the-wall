# Import queue (`ui-import`)

> Layer: frontend feature. Described at main `82b7edf` (v2.4.0).
> In flight: None.

## Purpose
The panel that replaces a note's content while media imported as that note hasn't been transcribed yet. You reorder the staged files and press Transcribe. It then shows per-file progress from real pipeline events, per-file conversion failures with Retry, a failed run's cause with Retry, and Cancel. `importProgress.ts` turns the pipeline's single global ASR % into per-file status using the concatenation boundaries.

## Files it owns
- `frontend/src/components/ImportQueue.tsx` — the queue: drag-reorder, Transcribe / Retry / Cancel, per-row status.
- `frontend/src/importProgress.ts` — pure mapping: `mapProgress`, `parseTranscribingDetail`, `rollingStatus`, `fmtSize`.

App owns the import flow around it: `importMedia` (`App.tsx:645-654`), `importTranscribe` (`:658-673`), `cancelImportTranscribe` (`:678-686`), the `cancelled` / `done` handling (`:384-397`, `:415-428`) and the reads on note open (`:554-570`). See [ui-app-shell](ui-app-shell.md).

## Surface area
### Public API
| Export | Where | Notes |
|---|---|---|
| `ImportQueue` (default) | `ImportQueue.tsx:50` | Props: `staged: ImportStaged`, `pipeStage`, `pipeDetail`, `pipelineError`, `onTranscribe(order)`, `onRetry`, `onCancel`, `onFilesChange(before, after)` (a file removed or a drag finished). Mounted by the Editor when the note has a staged import (`Editor.tsx:1428-1438`). |
| `mapProgress(files, boundariesMs, pct)` | `importProgress.ts:18-50` | Per-file `waiting` / `transcribing` / `done` and the active file's own % (capped at 99). Falls back to everything `transcribing` with no % when boundaries are missing or don't match. |
| `parseTranscribingDetail(detail)` | `importProgress.ts:64-74` | Reads `%`, engine (`cloud` / `GPU` / `CPU`) and quota wait minutes from the pipeline detail. |
| `rollingStatus(files, boundariesMs, detail)` | `importProgress.ts:80-94` | Header line, e.g. "2 of 3 · Cloud · 20%" or "2 of 3 · Waiting for cloud quota (~3m)". |
| `fmtSize(bytes)` | `importProgress.ts:96-97` | "12.4 MB" / "340 KB". |

How backend import state maps onto the queue:

| Backend | Queue shows |
|---|---|
| `import_stage` → `ImportStaged{started:false}` (note + meeting created, files copied) | Idle: drag to reorder, "Transcribe N files"; unsupported files (`error`) shown with "!" and Remove |
| `import_transcribe` running: `import:progress` `converting` / `converted` per file | Header "Preparing audio…"; the converting row says "Preparing…" (`ImportQueue.tsx:76-89`, `:149`) |
| `import:progress` `failed` with `error` | That row: "Couldn't convert" + its cause; the rest carry on |
| `import_transcribe` resolves `started:false`, files with `convert_error` (the queue pauses before transcribing) | Idle again with the causes; "Retry failed file(s)" converts them again; Transcribe goes on without them (`:100-117`) |
| `import_transcribe` resolves `started:true` with `boundaries_ms`, then `pipeline:progress` | Header from `RUNNING_LABELS` (`:37-48`); during `transcribing`, `rollingStatus` plus per-row % via `mapProgress`; after ASR (`diarizing`…`polishing`) every row "✓ Done" |
| terminal `error` (or a stored failure read by `pipeline_failure` on reopen) while `started` | "Transcription didn't finish. Retry picks up where it stopped." + briefed cause, Retry only (`:96`, `:139`, `:163-164`, `:222-226`) |
| `cancelled` | App re-reads `import_state` (the backend reset `started`): idle queue |
| `done` | App drops the queue and re-reads the meeting and note (`App.tsx:415-428`); `import_state` returns `null` once a transcript exists (`src-tauri/src/import_commands.rs:547-571`) |

**The stuck "Starting…" bug.** Fixed in PR #98 (`3c1bf93`, `4b0c8fa`), with the follow-up in PR #100 (`7f84233`). Before: after a run gave up, reopening the note (or restarting) cleared the error, the queue stayed `started`, and the header fell back to "Starting…" (`ImportQueue.tsx:172-173`) with only Cancel. Cancel with nothing queued or running emitted no event, so nothing re-read the queue. Fix: a new `pipeline_failure` command (`src-tauri/src/asr_commands.rs:204-207` → `scheduler::last_failure`) read when a note opens or the meeting switches (`App.tsx:561`, `:798`); `cancel_transcription` always ends in a terminal `"cancelled"`, emitting it itself when nothing was in flight (`src-tauri/src/import_commands.rs:352-387`); the queue shows a failed run's cause with Retry and no "Waiting" rows; an idle queue ignores a stored failure. PR #100 made `openNoteById`/`selectMeeting` set `openMeetingIdRef` before their reads and apply the stage/failure only if no event for that meeting arrived meanwhile (`App.tsx:543-570`). The "Starting…" fallback still exists for a started queue with no stage and no failure (an enqueue that never happened); Cancel is the way out.

### Tauri commands
- Calls (through App callbacks): `import_stage`, `import_state`, `import_transcribe`, `cancel_transcription`, `transcribe_meeting` (Retry), `pipeline_stage`, `pipeline_failure`, `get_meeting_for_note`, `get_note`, `get_transcript`, `get_cleaned_transcript`. The queue itself calls none.
- devMock mirrors all of them, with a scripted event stream (`devMock.ts:302-521`). Drift:

| devMock | Real backend |
|---|---|
| Unsupported file error "Unsupported file type — audio or video only" (`devMock.ts:371`) | `unsupported_file_message()`: "Not an audio or video file this app imports (WAV, MP3, …)." (`import_commands.rs:79-92`) |
| Fixture files have no `convert_error` key until a failure (`devMock.ts:348-373`) | Always serialized, `null` when none (`import_commands.rs:44-47`) |
| Cancel while running is ignored and leaves `started` as is (`devMock.ts:504-506`) | Resets `started` first, then the worker emits `"cancelled"` when it unwinds (`import_commands.rs:363-372`, `scheduler.rs:306-318`) |
| Run gives up at once (`fotwMockImportPipelineFail`, `devMock.ts:453-463`) | Up to 3 attempts, each retry emitting `"waiting"` with detail `"retrying (attempt N failed)"` (`scheduler.rs:175-191`) |
| `import_state` non-null until `"done"` (`devMock.ts:1405-1408`) | `null` as soon as the transcript is saved, before `"polishing"` (`import_commands.rs:554-569`) |
| `"no staged import"` thrown as an `Error` (`devMock.ts:396`) | Plain string |

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `import:progress` | listens, `ImportQueue.tsx:76-89` (filtered to `staged.meeting_id`) | `{meeting_id, file_id, stage: "converting" \| "converted" \| "failed", error?}` | `src-tauri/src/import_commands.rs:64-74`, emitted `:646-656` |
| `pipeline:progress` | indirect: App passes `pipeStage`, `pipeDetail`, `pipelineError` | see [ui-app-shell](ui-app-shell.md) | `"transcribing"` details from `src-tauri/src/pipeline.rs:932-956` (`transcribe_detail`) |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `import.staged.<meeting_id>` | backend only (`import_commands.rs:29-31`) | no | The staged manifest; the UI reaches it only through `import_state` |
| devMock toggles | — | n/a | `fotwMockImportFail` (one file fails to convert once), `fotwMockImportPipelineFail` (the run fails at speaker detection) |

### Database
None directly (the manifest is a settings row owned by [app-import](app-import.md)).

### Files and directories written
None by the frontend. The backend copies the originals into the meeting's recordings folder and converts in `<scratch_root>` (see [app-import](app-import.md)).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None from the frontend | — | Groq transcription of the joined file follows the usual ASR settings and Keep local (backend) |

### External processes
None from the frontend (ffmpeg runs as a backend sidecar).

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `Editor.tsx:1428-1438` (with `onRetry` = App's `transcribeNow`, `onTranscribe` = `importTranscribe`, `onCancel` = `cancelImportTranscribe`).
- **Outbound:** `importProgress.ts`, `pipelineNotice.ts` (`briefError`), `ui.tsx` (`Button`), `@tauri-apps/api/event`, `types.ts` (`ImportFile`, `ImportStaged`, `ImportProgress`).

## Invariants
- Files are transcribed top to bottom in the order shown; only an idle queue can be reordered (`ImportQueue.tsx:91-131`, `:249`).
- A conversion failure belongs to its file: during a run only this run's failures count; when idle the queue shows the causes the last run left (`ImportQueue.tsx:97-103`). Transcribe leaves failed files out; Retry runs the whole queue again, reusing finished conversions (`:105-117`).
- A failed run counts only while the queue is started: it shows the cause with Retry, no per-file progress and no Cancel; an idle queue never offers Retry over a stored failure (`ImportQueue.tsx:93-96`, `:139`, `:222-236`; PR #98).
- Cancel is offered only before ASR finishes (`ImportQueue.tsx:227`); the backend always answers with a terminal `"cancelled"` (`import_commands.rs:352-387`), and `"cancelled"` is neither success nor failure (`App.tsx:381-397`, `pipelineFailures.ts:30-32`).
- Per-file % is proportional to each file's share of the joined timeline (1 s gap between files, `import_commands.rs:28`), never a timer (`importProgress.ts:14-17`).
- A finished `import_transcribe` updates only the queue of the import note that's still open (`App.tsx:664-671`, PR #95).
- The queue's local file list re-syncs whenever the backend hands back a new `staged` object (`ImportQueue.tsx:68-74`).
- The note is titled after the first file (`importTitle`, the backend's `file_stem`). Until the user renames it, the Editor's title follows the queue's first file when a file is removed or the order changes (`followedImportTitle` in `importProgress.ts`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest (jsdom, devMock event bus) | `frontend/src/components/ImportQueue.test.tsx` | 10 | Per-file failure attribution, Retry/Transcribe/Remove, count wording, live `failed` event, unsupported files, failed run, idle queue |
| vitest (node) | `frontend/src/importProgress.test.ts` | 11 | `mapProgress`, detail parsing, rolling status, sizes |
| vitest (jsdom, devMock backend) | `frontend/src/App.test.tsx` (import describes) | 4 | A finished call only touches the open import note; the failed run's cause and Retry after reopen; Cancel unsticks a started queue with no job |

**Known gaps:**
- The real retry path (three attempts, each a `"waiting"` event with `"retrying (attempt N failed)"`) is never simulated. The queue then says "Waiting to transcribe (recording comes first)…" (`ImportQueue.tsx:38`), which is wrong for a retry (inferred).
- `import:progress` is subscribed asynchronously on mount; events emitted before the subscription resolves (a note reopened mid-conversion) are lost. No test.
- devMock's import shapes and strings aren't checked against Rust (see drift table).
- The channel-plus-quota detail form (`"other participants (cloud 40%, waiting for cloud quota ~2m)"`, `pipeline.rs:953`) has no TS test, though the regex handles it.

## Changing it safely
- Run `npm --prefix frontend test` (ImportQueue, importProgress, App).
- A new pipeline stage: add a header label to `RUNNING_LABELS` and decide whether it counts as "after ASR" (`ImportQueue.tsx:134-135`).
- A change to the `ImportStaged` / `ImportFile` / `import:progress` shapes in `import_commands.rs`: update `types.ts`, devMock's import mock (`devMock.ts:302-521`, `:1512-1558`) and the tests together.
- Keep `cancel_transcription` ending in a terminal `"cancelled"` for every case; the queue has no other way back to idle.
- Before tagging, check a real failed import in the app (PR #100's lesson: unit tests passed while the real app stuck).
