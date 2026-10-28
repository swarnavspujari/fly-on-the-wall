# Media import (`app-import`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: None directly. #86 (merged) added a dormant engine to [fly-asr](fly-asr.md), and PR #92 changes transcript storage; imports reuse [app-pipeline](app-pipeline.md) unchanged.

## Purpose
Turns existing audio or video files into a normal note in three steps. **Stage:** pick files, create the note and meeting, copy each original into the meeting folder. **Review:** the note opens on an import queue where the user reorders or removes files. **Transcribe:** each file is converted to a 16 kHz mono intermediate (pure Rust for PCM WAV, ffmpeg otherwise) and the results are joined, with 1 s of silence between files, into one `recording.mixed.wav`, which is queued on the same pipeline a live recording uses. If any file fails to convert, the queue pauses before transcribing, with the cause on that file, because a file skipped now can never be added to the single transcript later.

## Files it owns
- `src-tauri/src/import_commands.rs`: the three commands, the staged manifest, conversion (`normalize_files`, `convert_one`, `SidecarFfmpeg`), `concat_16k`, the import-side cancel (`cancel_staged_transcription`), and purge of staged residue.

## Surface area
### Public API
| Item | Where | Called by |
|---|---|---|
| `ImportFile { id, file_name, size, kind, rel_path, error, convert_error }`, `ImportStaged { note_id, meeting_id, files, boundaries_ms, started }` | `import_commands.rs:34-61` | serialized to the UI (`frontend/src/types.ts:381-404` mirrors them) |
| `cancel_staged_transcription(state, on_stage, meeting_id)` (crate) | `import_commands.rs:363-387` | `cancel_transcription` (`commands.rs:310-320`, owned by [app-pipeline](app-pipeline.md)) |
| `reset_staged_started(state, meeting_id)` (crate) | `import_commands.rs:343-350` | the function above |
| `purge_staged_import(state, meeting_id)` (crate) | `import_commands.rs:392-409` | `delete_note` (`commands.rs:288`), `scheduler::finish_cancelled` (`scheduler.rs:358`) |

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `import_stage` | none (opens the native multi-file picker) | `Result<Option<ImportStaged>, String>`; `None` when the picker is cancelled | `ui-home` (`Sidebar.tsx:598` "Import media as a note"), `ui-home` (`Home.tsx:201` "Import recordings"), both → `App.tsx:645-654` `importMedia`, via `api.importStage` |
| `import_state` | `meeting_id: String` | `Result<Option<ImportStaged>, String>`. Deletes the manifest and returns `None` once the meeting has a transcript. | `ui-app-shell` (`App.tsx:558` on note open; `App.tsx:388-398` after `cancelled`; `App.tsx:670` after a failed transcribe), via `api.importState` |
| `import_transcribe` | `meeting_id: String, order: Vec<String>` (file ids) | `Result<ImportStaged, String>`. `started: true` once enqueued; `started: false` with `convert_error`s when paused. | `ui-import` (`ImportQueue` Transcribe/Retry → `App.tsx:658-673`), via `api.importTranscribe` |

Cancel for a running import is `cancel_transcription`, documented in [app-pipeline](app-pipeline.md).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `import:progress` | emits during conversion (`import_commands.rs:646-656`) | `ImportProgress { meeting_id, file_id, stage: "converting" \| "converted" \| "failed", error? }` (`import_commands.rs:66-74`) | `ImportQueue.tsx:77`; devMock mirrors (`devMock.ts:424-436`, `:1490-1505`) |
| `pipeline:progress` | emits `waiting` via `scheduler::enqueue`; `cancelled` via `cancel_staged_transcription` | `PipelineProgress` (see [app-pipeline](app-pipeline.md)) | `App.tsx:373` |
| `model:progress` | emits while the ffmpeg sidecar downloads on first use (`import_commands.rs:274-279`) | `models::ModelProgress` | `App.tsx:450` |

Per-file progress during ASR: `import_transcribe` stores `boundaries_ms` (the cumulative end of each file on the joined timeline, `import_commands.rs:576-591`). The frontend maps the pipeline's global `transcribing` percent onto files (`frontend/src/importProgress.ts:18-50`). That percent is a fraction of speech time while the boundaries are timeline milliseconds, so the per-file split is proportional, not exact (documented at `importProgress.ts:14-17`).

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `import.staged.<meeting_id>` | R/W/delete (`import_commands.rs:30-32`, `:315-338`, `:404-408`, `:562-567`) | No. Pinned by `commands::security_tests::webview_can_only_touch_ui_level_settings` (`commands.rs:694`). | JSON `ImportStaged` manifest. It holds file paths, so only these commands may write it. |

### Database
| Table | Columns | Read / write |
|---|---|---|
| `notes` | new row titled after the first file's stem | W (`create_note`, `import_commands.rs:473-475`) |
| `meetings` | new row, no attendees (unconfirmed); `recording_json.mixed_path`, `ended_at` | W (`create_meeting`, `allocate_meeting_dir`; `end_meeting` at transcribe, `import_commands.rs:711-723`) |
| attachments (via `add_attachment_in_place`) | imported videos registered in place on the note | W (`import_commands.rs:724-731`) |
| `settings` | `import.staged.*` | R/W |
| `transcription_jobs` | | W via `scheduler::enqueue`; R/delete in cancel (`import_commands.rs:372-384`) |
| `transcripts` | existence check only | R (`import_state`, `import_commands.rs:554-560`) |

### Files and directories written
| Path | When | Notes |
|---|---|---|
| `<data_dir>/recordings/<date> <title>/<sanitized name>` | `import_stage`: a copy of each supported file (`import_commands.rs:508-519`) | `copy_name` keeps letters, digits and ` -_.`, and de-duplicates with ` (2)` (`import_commands.rs:413-435`). Originals are never modified. |
| `<scratch_root>/<meeting_id>/import-<file_id>.part.wav` → `import-<file_id>.16k.wav` | `import_transcribe`, per file (`import_commands.rs:222-243`) | renamed from `.part` only when complete (with retry for scanner/indexer locks). Finished conversions are reused by Retry. |
| `<data_dir>/recordings/<date> <title>/recording.mixed.wav` | after all files convert (`import_commands.rs:699-703`) | the joined 16 kHz track the pipeline transcribes; the per-file intermediates are deleted afterwards |
| deletes | `drop_staged_files` (`import_commands.rs:751-758`) removes copies and conversions of files dropped from the order, only after the new manifest is saved; `purge_staged_import` removes the staged-copies folder through `contained_recordings_dir` (`import_commands.rs:392-403`) | |

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| ffmpeg sidecar download host (via `models::ensure_tool("ffmpeg")`, [app-models-gpu](app-models-gpu.md)) | first non-WAV (or exotic WAV) conversion when no ffmpeg is installed or on PATH (`import_commands.rs:271-292`) | user action (Transcribe) |
| Groq and the LLM provider | indirectly, through the queued pipeline | see [app-pipeline](app-pipeline.md) |

### External processes
| Process | Args | Notes |
|---|---|---|
| `ffmpeg` (managed sidecar or PATH) | `-i <src> -hide_banner -ac 1 -ar 16000 -vn -y <dst.part.wav>` (`import_commands.rs:293-297`) | Windows `CREATE_NO_WINDOW` only. It runs at normal priority, and conversion is not deferred while a recording is active; only the pipeline waits (inferred from `import_commands.rs:600-672`). Stderr becomes one sentence per file (`describe_ffmpeg_failure`, `import_commands.rs:135-164`). Launch failure or missing ffmpeg fails the whole batch; a rejected file fails only itself. No timeout. |

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `ui-home` / `ui-app-shell` / `ui-import` (commands above), [app-pipeline](app-pipeline.md) (`cancel_transcription` → `cancel_staged_transcription`; `finish_cancelled` → `purge_staged_import`), [app-notes-library](app-notes-library.md) (`delete_note` → `purge_staged_import`), [app-privacy](app-privacy.md) (`ImportGuard` stops "Delete all my data" while an import runs, `import_commands.rs:446`, `:606`).
- **Outbound:** [app-pipeline](app-pipeline.md) (`scheduler::{enqueue, cancel, emit_cancelled, last_failure, stage_emitter}`), [fly-storage](fly-storage.md) (notes, meetings, settings, `scratch::prepare_meeting_scratch`, `rename_with_retry`, `contained_recordings_dir`, `add_attachment_in_place`), [fly-audio](fly-audio.md) (`mix::read_wav_mono`, `resample_linear`, `write_wav_mono_16`), [app-models-gpu](app-models-gpu.md) (`ensure_tool`), `tauri-plugin-dialog`.

## Invariants
- **Imports reuse the recording pipeline unchanged.** DECISIONS.md M7/M8, "Imports reuse the recording pipeline unchanged". The single mixed track is diarized whole (`src-tauri/src/pipeline.rs:618-666`).
- **A conversion failure pauses before transcribing.** Every other file still converts, each failed file carries its own `convert_error`, the queue returns with `started: false`, and nothing is enqueued (`import_commands.rs:681-696`; PR #93/#100).
- **Backend and UI agree when nothing is running.** Cancel always produces exactly one terminal `cancelled`, including when nothing is queued or running, and drops `started` to false, so the queue can't stick on "Starting" (`import_commands.rs:352-387`; PR #100). Tests: `cancel_with_nothing_running_drops_the_queue_to_idle_and_says_so`, `cancel_of_a_queued_job_emits_one_cancelled`.
- **A failure after Transcribe belongs to the run, not the queue.** `started` stays true, Retry re-enqueues and resumes from ASR checkpoints, and the cause survives a restart via `pipeline_failure` (`scheduler.rs:286-298`). Test: `a_terminal_failure_leaves_the_staged_queue_retryable_with_its_cause`.
- **Conversions live in machine-local scratch, never the meeting folder.** A rename mid-run can't break them (`import_commands.rs:640-644`; `crates/fly-storage/src/scratch.rs:1-18`).
- **Only the native picker supplies file paths.** The webview sends file ids and an order, never paths (`import_commands.rs:448-459`, `:617-621`). The manifest is not webview-writable (spec §2 item 2; `commands.rs:59-66`).
- **Nothing is deleted before the new manifest is saved** (`import_commands.rs:622-623`, `:693-694`, `:740-741`).
- **A deleted import is never written back.** A note deleted while its files converted is detected after conversion (`import_commands.rs:674-679`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/import_commands.rs` | 11 | `concat_16k` boundaries and gaps; `normalize_files` with a fake ffmpeg (bad file attributed, garbage WAV falls back to ffmpeg, no ffmpeg fails the batch, Retry reuses finished conversions, rename failure is per-file, missing copy); ffmpeg stderr → cause; unsupported-format message; `copy_name` |
| unit (real scheduler + storage) | `src-tauri/src/import_commands.rs` | 6 | failure after Transcribe keeps the queue started and the cause across restart; auto-retry isn't "failed"; three cancel shapes (nothing running, after a failure, queued) emit exactly one `cancelled`; running cancel sets a request and waits |
| vitest (devMock backend) | `frontend/src/components/ImportQueue.test.tsx`, `frontend/src/App.test.tsx` ("an import whose run failed after Transcribe"), `frontend/src/importProgress.test.ts` | — | UI against devMock's copy of these commands and events |

**Known gaps:**
- `import_transcribe` and `import_stage` have no test (they need an `AppHandle` and the dialog). Their orchestration is untested: pause-on-failure (`import_commands.rs:686-696`), applying the order and dropping rows, the `end_meeting` + enqueue + `started` ordering, attaching videos, and the deleted-while-converting check. Only the pieces they call are tested.
- The running-cancel path is never driven to completion (unwind → `Tick::Cancelled` → `cancelled`). `cancel_of_a_running_job_waits_for_the_worker` fakes "running" with a stage string and doesn't check `started`.
- Re-Transcribe during an unwinding cancel (inferred race). `reset_staged_started` drops `started` immediately (`import_commands.rs:368`). If the UI re-reads the queue (by reopening the note) before `cancelled` arrives, Transcribe is offered. `scheduler::enqueue` is a no-op while the job row is `running` (`crates/fly-storage/src/jobs.rs:30-41`), and the unwinding run then deletes the job (`scheduler.rs:126`). The result is `started: true` with no job, the stuck-"Starting" class again; Cancel recovers it.
- `drop_staged_files` deletes `data_dir.join(rel_path)` from the manifest without the containment check `purge_staged_import` uses (`import_commands.rs:751-758` vs `:398`). The manifest isn't webview-writable, so this is a defense-in-depth gap, not an exposure.
- `concat_16k` holds the whole joined track in memory as `f32` (`import_commands.rs:576-591`). A multi-hour import allocates gigabytes (inferred, untested).
- The ffmpeg stderr phrases in `describe_ffmpeg_failure` tests are hand-written, not captured from a real ffmpeg; an ffmpeg upgrade can change the wording.
- devMock drift: a Cancel during a mock run returns without resetting `started` or emitting `cancelled` (`devMock.ts:504-520`), whereas the backend resets `started` at once and emits `cancelled` after the unwind.

## Changing it safely
- Run `cargo test -p fly-app import_commands` and the vitest suites `ImportQueue`, `App` and `importProgress`. Then smoke a real import (a WAV plus a short MP4 plus a deliberately broken file) in the app. Unit tests can't reach `import_transcribe`.
- Keep `ImportStaged`/`ImportFile` field names in sync with `frontend/src/types.ts:381-404` and `devMock.ts`. Old manifests must still deserialize: new fields need `#[serde(default)]`, as `convert_error` has.
- Changing the order rules, `GAP_MS`, or the conversion settings changes the joined track and therefore the ASR plan key. Old checkpoints are then discarded (a correctness-safe re-decode).
- Any new delete path must go through `contained_recordings_dir` or the scratch helpers, and must run only after the manifest that no longer references the file is saved.
- Keep the "exactly one `cancelled`" behavior. `App.tsx:384-399` relies on it to drop the queue to idle.
