# Screen capture (`fly-capture-screen`)

> Layer: crate. Described at main `82b7edf` (v2.4.0).
> In flight: None.

## Purpose
Records the screen, one window, or a region to an MP4. On Windows the primary backend is Windows Graphics Capture (WGC, in-process H.264 via Media Foundation), which records GPU-composited windows (Zoom, Teams, browsers) that GDI capture turns black; an ffmpeg child process (`gdigrab`) is the fallback. Linux uses ffmpeg `x11grab` (full screen or region); macOS uses ffmpeg `avfoundation` (full screen only). It also enumerates capturable windows and resolves a typed or stale window title to the exact current one.

## Files it owns
- `crates/fly-capture-screen/Cargo.toml` — deps; Windows-only `windows` 0.62 and `windows-capture` 2.0.1.
- `crates/fly-capture-screen/src/lib.rs` — `ScreenRecorder` / `ScreenSession` traits, `CaptureTarget`, `ScreenError`, `FallbackScreenRecorder`, `NullScreenRecorder`.
- `crates/fly-capture-screen/src/ffmpeg.rs` — `FfmpegScreenRecorder`, `grab_args` (pure), start-time liveness probe, graceful `q` stop, `brief_stderr`.
- `crates/fly-capture-screen/src/wgc.rs` — `WgcScreenRecorder` (Windows only, `#[cfg(windows)]` at `lib.rs:9-10`), bitrate rule, proof-of-life probe, finalize on stop/window close.
- `crates/fly-capture-screen/src/window_list.rs` — `list_windows`, `find_window_hwnd`, `resolve_window_title`, `pick_title` (pure).
- `crates/fly-capture-screen/examples/screen_smoke.rs` — manual 4 s ffmpeg full-screen smoke (`cargo run --example screen_smoke -- <ffmpeg> <out.mp4>`).

## Surface area
### Public API
- **Traits** `ScreenRecorder { is_available(), start(target, out_path) -> Box<dyn ScreenSession> }`, `ScreenSession { stop(self: Box) -> PathBuf, elapsed_ms() }` (`lib.rs:44-54`).
- **Types** `CaptureTarget` (serde `kind` tag: `full_screen` | `window { title }` | `region { x, y, width, height }`, `lib.rs:29-42`) — crosses IPC as-is; `ScreenError { Unavailable, Capture, Io }`.
- **Recorders** `FallbackScreenRecorder { primary, secondary }` (`lib.rs:56-80`), `NullScreenRecorder`, `ffmpeg::FfmpegScreenRecorder::new(exe)` (10 fps), `wgc::WgcScreenRecorder` (30 fps ceiling).
- **Pure helpers** `ffmpeg::{grab_args, host_backend, GrabBackend}`, `wgc::{bitrate_for, even, FRAME_RATE}`, `window_list::pick_title`.
- **Window enumeration** `window_list::list_windows()` (Windows: visible, titled, non-tool, non-cloaked top-level windows, de-duplicated by title; empty elsewhere), `find_window_hwnd`, `resolve_window_title` (Windows only).

### Tauri commands
None (crates expose no commands). See [app-screen-recording](app-screen-recording.md).

### Events
None.

### Settings keys
None.

### Database
None.

### Files and directories written
- The caller's `out_path` (the app passes `attachments/<note_id>/screen-<timestamp>.mp4`); parent dirs are created (`ffmpeg.rs:150-152`, `wgc.rs:74-76`).
- A failed WGC start or a zero-frame WGC stop deletes the partial file (`wgc.rs:136`, `wgc.rs:321-326`).

### Network egress
None. (The app downloads ffmpeg; see [app-screen-recording](app-screen-recording.md).)

### External processes
- `ffmpeg` (path supplied by the caller): `-hide_banner -nostats -loglevel error`, grab args, `libx264 -preset ultrafast -crf 28 -pix_fmt yuv420p`, width capped at 1920, `-g 30`, `+faststart` (`ffmpeg.rs:155-195`). `CREATE_NO_WINDOW` on Windows. stdin piped (for `q`), stdout null, stderr piped and only read on failure.
- Stop: write `q`, wait up to 10 s, then kill (`ffmpeg.rs:272-307`).
- WGC runs on a capture thread inside the app process (no child process).

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `src-tauri/src/screen_commands.rs` only (`build_recorder`, `list_windows`, `resolve_window_title`).
- **Outbound:** fly-core (declared in `Cargo.toml`, never imported), `windows` / `windows-capture` / `parking_lot` (Windows), tracing.

## Invariants
- A doomed capture fails at **start**, never silently at stop: ffmpeg gets a 600 ms liveness window (`ffmpeg.rs:205-229`); WGC waits up to 1.5 s for a first frame or a thread error (`wgc.rs:117-147`). Source: DECISIONS.md "M7/M8 — Screen recording"; the field failure noted in `ffmpeg.rs:326-337`.
- The MP4 index must be written: ffmpeg is stopped with `q`, not killed (kill only after 10 s); WGC calls `encoder.finish()` on stop or on window close (`wgc.rs:288-295`, `:313-319`). Source: DECISIONS.md "M7/M8" (graceful `q` shutdown).
- Even output dimensions for H.264 4:2:0 (`ffmpeg.rs:79-81`, `wgc.rs:62-64`).
- An ambiguous window title matches nothing rather than a random window (`window_list.rs:115-134`).
- Fallback is logged so a black ffmpeg recording can be traced to why WGC was skipped (`lib.rs:66-79`).
- stderr from ffmpeg is never logged, only its byte count (`ffmpeg.rs:219`, `:291`) — log hygiene, `src-tauri/src/logging.rs:6-13`. **But** `FallbackScreenRecorder` logs the primary's error with `{e}` (`lib.rs:73`), and WGC errors embed the window title (`wgc.rs:101-109`), which can carry a meeting or document name.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src/ffmpeg.rs` | 5 | `brief_stderr` (window-not-found + tail); `grab_args` for gdigrab, x11grab, avfoundation |
| unit (Windows CI only) | `src/wgc.rs` | 2 | `bitrate_for` clamps, `even` |
| ignored e2e | `src/wgc.rs` | 2 | WGC records a GPU window (needs an open "Claude" window and the managed ffmpeg in the real data dir) and full screen + region |
| unit | `src/window_list.rs` | 5 | `pick_title` rules; `list_windows` returns non-empty titles (vacuous when empty) |
| unit (Windows CI only) | `src/window_list.rs` | 1 | Listed titles resolve to HWNDs (early-returns on an empty desktop) |

**Known gaps:**
- WGC capture itself never runs in CI (both e2e tests are `#[ignore]`); the backend the app uses first on Windows is verified only by hand.
- `FallbackScreenRecorder` has no test (primary unavailable, primary start error, both failing).
- The full ffmpeg command line (encoder flags, scale filter, keyframe cadence, `+faststart`) and the `q`-then-kill stop are untested; only the grab-input args are.
- No test for WGC's `on_closed` path (window closed mid-recording), the zero-frame stop error, or region cropping off-screen (`wgc.rs:271-277`).
- The test fixtures in `ffmpeg.rs` and `window_list.rs` reuse a real field-failure window title; the repo is public (see the audit summary).

## Changing it safely
- Run `cargo test -p fly-capture-screen`; on a Windows desktop also run `cargo test -p fly-capture-screen -- --ignored` (needs ffmpeg under `<data_dir>/bin/ffmpeg` and a window titled "*Claude*").
- `CaptureTarget` is serialized to/from the frontend (`types.ts` `CaptureTarget`); renaming a variant breaks `start_screen_recording`.
- Keep start fail-fast; the app records the session as active only after `start` returns.
- Never log window titles or ffmpeg stderr content; log sizes and statuses.
- macOS needs a ScreenCaptureKit backend for window/region capture (`lib.rs:1-6`, docs/PORTING.md).
