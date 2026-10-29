# Screen recording commands (`app-screen-recording`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: None.

## Purpose
Lets the user record the screen, a window or a region alongside a note. The MP4 is written straight into `attachments/<note_id>/` and registered on the note in place when the capture stops (no copy). Picks the recorder (Windows: WGC first, ffmpeg fallback; elsewhere ffmpeg), makes sure the ffmpeg sidecar is installed, lists capturable windows, and lazily makes poster thumbnails for video attachments.

## Files it owns
- `src-tauri/src/screen_commands.rs` — the 5 commands, `ActiveScreenRecording`, `ScreenStatus`, `build_recorder`, `generate_thumbnail`.

## Surface area
### Public API
- `pub struct ActiveScreenRecording { session, note_id, rel_path }` (`screen_commands.rs:14-18`) — the type of `AppState.screen` (`state.rs:33`).
- `pub struct ScreenStatus { active, note_id, elapsed_ms }` (`screen_commands.rs:20-25`).
- Read by other modules: `scheduler.rs:469` checks `state.screen` so queued transcriptions wait while a screen capture runs.

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `screen_status` | — | `Result<ScreenStatus, String>` | `ui-app-shell` (`App.tsx:304`, polled every 1 s), via `api.screenStatus` |
| `list_capture_windows` | — | `Result<Vec<String>, String>` (exact current titles, front to back; empty off Windows) | `ui-editor` (`Editor.tsx:476`, window picker), via `api.listCaptureWindows` |
| `start_screen_recording` | `note_id: String, target: CaptureTarget` | `Result<ScreenStatus, String>` | `ui-app-shell` (`App.tsx:840`, from the Editor's capture menu), via `api.startScreenRecording` |
| `stop_screen_recording` | — | `Result<Note, String>` (the note with the new attachment) | `ui-app-shell` (`App.tsx:855`), via `api.stopScreenRecording` |
| `ensure_video_thumbnail` | `rel_path: String` | `Result<String, String>` (the poster's rel path, `<rel_path>.jpg`) | `ui-editor` (`Editor.tsx:1083`, first time a note's videos show), via `api.ensureVideoThumbnail` |

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `model:progress` | Emits while ffmpeg downloads (`screen_commands.rs:91-97`, `:178-184`) | `models::ModelProgress { id: "ffmpeg", downloaded, total, stage, error }` | `App.tsx:450` (shared progress stream) |

### Settings keys
None.

### Database
| Table | Columns | Read / write |
|---|---|---|
| `notes` | `attachments_json` (+ FTS/mirror sync via `set_note_attachments`) | R (`get_note` existence check, `screen_commands.rs:84-89`); W on stop (`add_attachment_in_place`, `:256-261`) |

### Files and directories written
- `attachments/<note_id>/screen-<YYYYMMDD-HHMMSS>.mp4` (`screen_commands.rs:107-112`).
- `<rel_path>.jpg` poster next to a video under `attachments/` or `recordings/`; a failed extraction removes any partial jpg (`screen_commands.rs:173-176`, `:226-228`).
- First use: the ffmpeg sidecar under `<data_dir>/bin/ffmpeg/` via `models::ensure_tool` (owned by [app-models-gpu](app-models-gpu.md)).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `github.com` (BtbN FFmpeg-Builds release asset, SHA-256 pinned, ~90 MB; `models.rs:90-100`, `:133-135`) | First screen recording or first thumbnail, only when no managed or PATH ffmpeg exists | User action (start recording / open a note with a video). Windows and Linux only; macOS needs ffmpeg on PATH. |

### External processes
- Windows: WGC capture thread in-process; the ffmpeg fallback child is launched by [fly-capture-screen](fly-capture-screen.md).
- `ffmpeg -y -i <video> -vf thumbnail=n=100,scale=min(640\,iw):-2 -frames:v 1 -q:v 4 <thumb>` with `CREATE_NO_WINDOW` (`screen_commands.rs:200-236`).

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `lib.rs:180-184` (registration), `state.rs:13` / `:33` (`AppState.screen`), `scheduler.rs:469`.
- **Outbound:** [fly-capture-screen](fly-capture-screen.md) (`FallbackScreenRecorder`, `WgcScreenRecorder`, `FfmpegScreenRecorder`, `window_list`), `models.rs` (`ensure_tool`), [fly-storage](fly-storage.md) (`get_note`, `attachment_abs_path`, `add_attachment_in_place`), `AppState.jobs_notify`.

## Invariants
- At most one screen capture at a time (`screen_commands.rs:61-63`; `state.rs:32-33`).
- A window title is re-resolved against the windows open **now**; a vanished or ambiguous window fails at start with a readable message (`screen_commands.rs:64-82`).
- The note must exist before any capture starts, and the output goes into `note_attachments_dir(note_id)`, which is `None` (capture refused) unless the id is one plain path component inside `attachments/`.
- Recorder start and stop run on blocking threads, never the async runtime (`screen_commands.rs:117-120`, `:251-254`).
- Thumbnails only for paths inside `attachments/` or `recordings/` (`attachment_abs_path`, `crates/fly-storage/src/attachments.rs:70-79`); THREAT_MODEL T3.
- Stopping a capture nudges the transcription queue (`screen_commands.rs:262-263`); the queue waits while a capture is active (`scheduler.rs:469`).
- Same ffmpeg sidecar for capture, thumbnails and import, never a second bundle (`screen_commands.rs:154-157`). Source: DECISIONS.md "M7/M8".

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| env-gated | `src-tauri/src/screen_commands.rs` | 1 | `generate_thumbnail` on a synthesized clip; missing input leaves no partial jpg (skips without ffmpeg) |
| env-gated (Windows) | `src-tauri/src/screen_commands.rs` | 1 | ffmpeg recorder fails at start with a readable error for a missing window (skips without ffmpeg) |
| ignored e2e (Windows) | `src-tauri/src/screen_commands.rs` | 1 | Spawns a titled console window, resolves a partial title, records 2.5 s through `build_recorder`, checks the MP4 size |

**Known gaps:**
- None of the 5 commands is invoked by a test; `start_screen_recording` / `stop_screen_recording` (single-capture guard, note check, attachment registration, queue nudge) are untested.
- The two env-gated tests look for ffmpeg in the real `<data_dir>` and then on PATH (`screen_commands.rs:273-280`). CI installs no ffmpeg, so whether they run there depends on the runner image (unverified).
- The missing-window test drives `FfmpegScreenRecorder` directly; on Windows the app path is title resolution then WGC (`build_recorder`), so the test proves only the fallback leg.
- **Closing the app mid-capture (inferred, untested):** `RunEvent::Exit` only stops Ollama (`lib.rs:202-211`). The capture is not stopped, the MP4 is not finalized (WGC needs `finish()`), and the file is never registered on the note.
- **Deleting the note mid-capture (inferred, untested):** `delete_note` refuses only the audio-recording note (`commands.rs:284-287`), not `state.screen.note_id`. It removes `attachments/<note_id>/` while the encoder writes there; `stop_screen_recording` then fails with a missing note.
- On Windows, `ensure_tool("ffmpeg")` runs before every start even though WGC does the capture, so a first screen recording on a fresh install waits for a ~90 MB download (`screen_commands.rs:98-105`).
- Region capture in the UI asks with `window.prompt` (`Editor.tsx:481`). tauri-plugin-dialog overrides only `alert`/`confirm`, the ESLint ban covers only those two, and no test runs in a real webview, so region capture's behavior in WebView2/WKWebView is unverified.
- devMock drift (spec §2 item 4): `start_screen_recording` and `stop_screen_recording` fall through to `default: return null` (`devMock.ts:1507`), so port-1420 QA gets `null` instead of a `ScreenStatus` / `Note` (`App.tsx:855-858` then reads `updated.id`).

## Changing it safely
- Run `cargo test -p fly-app screen_commands` and, on a Windows desktop with ffmpeg, `cargo test -p fly-app -- --ignored window_capture_records_a_real_window_e2e`.
- Update together: `lib.rs` registration, `frontend/src/api.ts`, `devMock.ts` (add real `start_screen_recording` / `stop_screen_recording` replies), `types.ts` (`ScreenStatus`, `CaptureTarget`).
- Keep the output under `attachments/<note_id>/` (inside the asset scope, so the player and thumbnails can load it; THREAT_MODEL T6).
- Never log window titles; they often carry meeting or document names.
