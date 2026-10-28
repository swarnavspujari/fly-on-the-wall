# Notes library commands (`app-notes-library`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: None in this file. PR #92 (draft, v2.5) changes fly-storage notes/search internals underneath these commands (per-segment FTS, embedding chunk columns) without changing their signatures (inferred from the PR summary).

## Purpose
The Tauri surface for the user's library: folders, notes, attachments, exports, and opening or revealing files. Each command is a thin wrapper over [fly-storage](fly-storage.md), plus the parts that need the app: native file pickers and save dialogs, the clipboard, the OS opener, and deleting a note safely while transcriptions may be running.

## Files it owns
- `src-tauri/src/commands.rs` (part) — the 20 commands below, `delete_allowed_while_recording` (`:320-325`), `is_executable_attachment` (`:533-570`), and the `delete_guard_tests` module (`:774-789`).

The rest of `commands.rs` belongs to [app-shell](app-shell.md) (`ping`, `app_info`, `reveal_*_dir`, `open_external_url`), [app-settings-consent](app-settings-consent.md) (`get/set_app_setting`, `is_webview_setting_key`), [app-search](app-search.md) (`search`, `search_semantic`), [app-pipeline](app-pipeline.md) (`cancel_transcription`) and [app-mcp-sidecar](app-mcp-sidecar.md) (`mcp_config`).

## Surface area
### Public API
- `pub fn is_executable_attachment(&Path) -> bool` (`commands.rs:533-570`) — extension deny-list used by `open_attachment`.
- No other module calls these commands' internals.

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `list_folders` | — | `Result<Vec<Folder>, String>` | `ui-app-shell` (`App.tsx:210`), via `api.listFolders` |
| `create_folder` | `name: String, parent_id: Option<String>` | `Result<Folder, String>` | `ui-app-shell` (`App.tsx:936`, sidebar folder menu in `ui-home`), via `api.createFolder` |
| `rename_folder` | `id: String, name: String` | `Result<(), String>` | `ui-app-shell` (`App.tsx:938`), via `api.renameFolder` |
| `move_folder` | `id: String, parent_id: Option<String>` | `Result<(), String>` (cycles rejected in storage) | None (wrapped as `api.moveFolder`, no UI caller) |
| `delete_folder` | `id: String` | `Result<(), String>` | `ui-app-shell` (`App.tsx:940`), via `api.deleteFolder` |
| `create_note` | `title: String, folder_id: Option<String>` | `Result<Note, String>` | `ui-app-shell` (`App.tsx:583`), via `api.createNote` |
| `get_note` | `id: String` | `Result<Note, String>` | `ui-app-shell` (`App.tsx:421`, `:535`), via `api.getNote` |
| `list_notes_in_folder` | `folder_id: Option<String>` (None = unfiled) | `Result<Vec<NoteSummary>, String>` | `ui-app-shell` (`App.tsx:235`), via `api.listNotesInFolder` |
| `list_recent_notes` | `limit: usize` | `Result<Vec<NoteSummary>, String>` | `ui-app-shell` (`App.tsx:234`, limit 200), via `api.listRecentNotes` |
| `update_note_title` | `id: String, title: String` | `Result<Note, String>` | `ui-editor` (`Editor.tsx:1114`), via `api.updateNoteTitle` |
| `update_note_scratchpad` | `id: String, scratchpad: String` | `Result<Note, String>` | `ui-editor` (`Editor.tsx:1117`), via `api.updateNoteScratchpad` |
| `move_note` | `id: String, folder_id: Option<String>` | `Result<(), String>` | `ui-app-shell` (`App.tsx:623`, `:630`), via `api.moveNote` |
| `delete_note` | `id: String` | `Result<(), String>` | `ui-home` (`noteDeletion.ts`, undoable deferred delete wired at `App.tsx:518`), via `api.deleteNote` |
| `attach_file` | `note_id: String` | `Result<Option<Note>, String>` (None = picker cancelled) | `ui-editor` (`Editor.tsx:1129`), via `api.attachFile` |
| `export_note` | `note_id: String` | `Result<Option<String>, String>` (saved path) | `ui-editor` (`Editor.tsx:1179`), via `api.exportNote` |
| `export_transcript_vtt` | `meeting_id: String, cleaned: bool` | `Result<Option<String>, String>` | `ui-editor` (`TranscriptPanel.tsx:388`), via `api.exportTranscriptVtt` |
| `copy_note_markdown` | `note_id: String` | `Result<String, String>` (the copied markdown) | `ui-editor` (`Editor.tsx:1164`), via `api.copyNoteMarkdown` |
| `remove_attachment` | `note_id: String, attachment_id: String` | `Result<Note, String>` | `ui-editor` (`Editor.tsx:1134`), via `api.removeAttachment` |
| `open_attachment` | `rel_path: String` | `Result<(), String>` | `ui-editor` (`Editor.tsx:389`, `:1248`, `:1250`, `:1256`), via `api.openAttachment` |
| `reveal_attachment` | `rel_path: String` | `Result<(), String>` | `ui-editor` (`Editor.tsx:1263`), via `api.revealAttachment` |

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `pipeline:progress` (stage `cancelled`) | Emitted indirectly by `delete_note` through `scheduler::cancel` for each of the note's meetings (`commands.rs:294-297`) | Owned by [app-pipeline](app-pipeline.md) | Pipeline / import UI |

`update_note_scratchpad` also nudges the embedding worker (`AppState.embed_notify`, `commands.rs:256`); not an IPC event.

### Settings keys
None.

### Database
All through [fly-storage](fly-storage.md):
| Table | Columns | Read / write |
|---|---|---|
| `folders` | `id, name, parent_id (ON DELETE CASCADE), created_at, keep_local` | R/W; `delete_folder` cascades to sub-folders |
| `notes` | `id, title, folder_id (ON DELETE SET NULL), meeting_id, scratchpad, blocks_json, attachments_json, created_at, updated_at, disk_path` | R/W; deleting a folder unfiles its notes, it never deletes them |
| `notes_fts` | `note_id, title, body` | W (sync on every note write; rows removed in the delete transaction) |
| embedding chunk tables | per note | W (re-chunked on write; deleted with the note) |
| `meetings`, `transcripts`, jobs | via `meetings_for_note`, `purge_meeting`, `get_transcript` | R; W (purge) on `delete_note` |

### Files and directories written
- **Mirrors (every note write):** `notes/<date> <title>.md` (atomic), plus `note.json` inside each of the note's `recordings/<date> <title>/` folders (`crates/fly-storage/src/notes.rs:320-345`). Title edits also rename the note mirror and the meeting folders and refresh their manifests (`notes.rs:186-201`). Moving a note or deleting a folder refreshes the meetings' `manifest.json` (filed-under path).
- **Attachments:** `attach_file` copies the picked file to `attachments/<note_id>/<name>` (de-duplicated `name (1).ext`) (`crates/fly-storage/src/attachments.rs:11-50`). `remove_attachment` deletes the copied file best-effort (`attachments.rs:53-64`).
- **Recursive deletes:** `delete_note` removes the note mirror and `attachments/<note_id>/` with `remove_dir_all` (`notes.rs:263-286`), then purges each idle meeting (its `recordings/` folder, through fly-storage's contained-delete guard) and any staged import (`commands.rs:304-315`). A meeting with a running pipeline gets a cooperative cancel; the scheduler purges it when the run unwinds.
- **Exports (user-chosen path, outside the data dir):** `export_note` copies the markdown mirror (provenance flattened) to a `.md` (`commands.rs:373-415`); `export_transcript_vtt` writes WebVTT rendered fresh from the stored transcript (polished variant when `cleaned` and present) (`:420-469`). Suggested file names keep only alphanumerics and ` -_.`.
- **Clipboard:** `copy_note_markdown` writes `note.to_markdown(false)` natively (`:453-469`).

### Network egress
None.

### External processes
- OS default handler via `tauri-plugin-opener` (`open_attachment` → `open_path`; `reveal_attachment` → `reveal_item_in_dir`).
- Native file picker / save dialog via `tauri-plugin-dialog` (`attach_file`, `export_*`), run inside async commands so the blocking dialog never sits on the main thread.

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `lib.rs:185-206` (registration); the frontend callers above.
- **Outbound:** [fly-storage](fly-storage.md) (notes, folders, attachments, meetings, `attachment_abs_path`, `note_mirror_abs`), `scheduler::cancel` / `stage_emitter` and `import_commands::purge_staged_import` (on delete), `AppState.recording` / `pipeline_stage` / `embed_notify`, tauri-plugin-dialog, -opener, -clipboard-manager.

## Invariants
- **Attachment paths stay inside `attachments/` or `recordings/`.** `open_attachment`, `reveal_attachment` (and `ensure_video_thumbnail`) resolve `rel_path` through `attachment_abs_path` → `safe_path::contained(.., ["attachments", "recordings"], 3)`, which rejects absolute/UNC paths, `..`, drive prefixes, trailing dots/spaces, `a:stream` and device names (`crates/fly-storage/src/attachments.rs:70-79`, `safe_path.rs:41-58`). Source: THREAT_MODEL T3; SECURITY_AUDIT_2026-10 "Path containment hardened".
- **Opening never executes code.** `open_attachment` refuses program/script extensions (`exe`, `bat`, `ps1`, `lnk`, `url`, `hta`, `jar`, `sh`, `app`, …) and points the user to "Show in folder" (`commands.rs:519-524`, `:533-570`). Source: SECURITY_AUDIT_2026-10.
- **The recording note can't be deleted.** `delete_note` refuses when `id` is the active audio recording's note (`commands.rs:282-287`, `:320-325`); the UI also hides its trash (`App.test.tsx:228`).
- **No orphaned meetings.** Queued jobs are cancelled, running pipelines get a cooperative cancel, idle meetings are purged with their recordings (`commands.rs:274-317`). The DB delete is one transaction including FTS and chunks (`notes.rs:263-275`; THREAT_MODEL T11).
- Folder deletes never delete notes or files: sub-folders cascade, notes become unfiled (`crates/fly-storage/src/folders.rs:168-179`, schema `lib.rs:422`, `:432`).
- Pickers and openers run in Rust; paths never round-trip through the DOM for picking (DECISIONS.md "M1 — Native dialogs/openers via Rust plugin APIs"). Attachments are copied, never referenced in place (DECISIONS.md "M1 — Attachments copied").

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/commands.rs` (`delete_guard_tests`) | 2 | `delete_allowed_while_recording` only |
| unit | `crates/fly-storage/src/attachments.rs`, `folders.rs`, `safe_path.rs`, `notes.rs` (owned by fly-storage) | several | Copy/dedupe/remove, folder CRUD + cycle rejection, path containment, note delete |
| vitest | `frontend/src/App.test.tsx` | 7 delete-related (`:108-228`) | Deferred delete, undo, failure restore, recording note has no trash — all against devMock |
| vitest | `frontend/src/deferredDelete.test.tsx` | 7 | The undo-delay hook (pure) |

**Known gaps:**
- **19 of the 20 commands have no direct test**, and the 20th (`delete_note`) is tested only through its 3-line guard. The cancel-then-purge ordering, the "running pipeline → don't purge" branch, `open_attachment`'s executable refusal (`is_executable_attachment` has no test) and the export file-name sanitizing are untested.
- **Screen capture vs delete (inferred, untested):** `delete_note` checks only `AppState.recording`, not `AppState.screen`. Deleting a note during its screen capture removes `attachments/<note_id>/` under the live encoder, and `stop_screen_recording` then fails.
- **No test pins that ids used as path components are plain ids.** Note ids become path components in `delete_note` and in attachment storage (`notes.rs:276-286`). Ids can also arrive from outside the app, through copied-in recording folders (THREAT_MODEL T7). A containment test belongs here and in [fly-storage](fly-storage.md).
- `remove_attachment` should resolve its path through the same containment check as `open_attachment` (`attachments.rs:62`).
- devMock drift (spec §2 item 4): `attach_file`, `remove_attachment`, `open_attachment` and `reveal_attachment` fall through to `return null` (`devMock.ts:1507`); `remove_attachment` should return a `Note` (`Editor.tsx:1134-1135` passes `null` on). devMock `delete_note` never models the backend's "Stop the recording before deleting this note." error (`devMock.ts:1049-1053`).
- Every vitest that covers deletes runs against devMock in jsdom; none exercises the real cancel/purge path.

## Changing it safely
- Run `cargo test -p fly-storage` (where the real logic and its tests live) and `cargo test -p fly-app delete_guard`; then `npm --prefix frontend test` for the delete UX.
- Any new command taking a path from the webview must go through `attachment_abs_path` / `safe_path::contained`, never `data_dir.join(raw)`.
- Any new recursive delete must name its root explicitly and be contained (THREAT_MODEL T3/T7). Validate ids that become path components.
- Keep exports going through the native save dialog; never accept a destination path over IPC.
- Update `api.ts`, `devMock.ts` (return the same shapes as the backend), and `types.ts` with any signature change.
