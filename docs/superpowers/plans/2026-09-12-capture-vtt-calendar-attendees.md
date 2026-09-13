# Capture / VTT / Calendar attendees Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix black window recordings with a Windows Graphics Capture backend, add WebVTT transcript mirrors + export, and seed meeting attendees from the in-progress calendar event on every Record path.

**Architecture:** A second `ScreenRecorder` backend (`wgc.rs`, `windows-capture` crate, in-process H.264) sits behind the existing trait with the ffmpeg recorder as fallback. `Transcript::to_vtt` joins `to_markdown` in fly-core and the storage mirror writes a third file. The calendar crate returns structured attendees; `start_recording` accepts an optional seed the frontend derives from the already-polled Up next list.

**Tech Stack:** Rust (Tauri 2 workspace), `windows-capture` 2.0.1, React/TypeScript frontend, serde JSON fixtures for provider tests.

Spec: `docs/superpowers/specs/2026-09-12-capture-vtt-calendar-attendees-design.md`.

---

### Task 1: `Transcript::to_vtt` (fly-core)
**Files:** Modify `crates/fly-core/src/model.rs` (after `to_markdown`), tests in the same file's `mod tests`.
- [ ] Test: two segments (`mic` labelled "You", `spk_0` labelled "Dana"), text with `<b>&`, one segment with `end_ms == start_ms`. Assert header `WEBVTT`, cue line `00:00:01.500 --> 00:01:02.250`, voice tag `<v Dana>`, escaped `&lt;b&gt;&amp;`, degenerate cue end = start + 1 ms.
- [ ] Run `cargo test -p fly-core to_vtt` → fails (no method).
- [ ] Implement `vtt_ts(ms)` helper + `to_vtt()`; cue identifier = segment id.
- [ ] Run test → pass. Commit `feat(core): WebVTT rendering for transcripts`.

### Task 2: VTT mirror + export command
**Files:** Modify `crates/fly-storage/src/transcripts.rs` (`transcript_mirror_paths` → struct `MirrorPaths { md, json, vtt }`, write vtt in `sync_transcript_derived` and `save_cleaned_transcript`); add `export_transcript_vtt` in `src-tauri/src/commands.rs`, register in `src-tauri/src/lib.rs`; `frontend/src/api.ts` `exportTranscriptVtt(meetingId, cleaned)`; `frontend/src/components/TranscriptPanel.tsx` "Export .vtt" outline button next to "Re-run transcription"; `frontend/src/devMock.ts` stub returning a fake path.
- [ ] Test (storage, existing test module pattern): save a transcript for a meeting with a recording dir → `transcript.vtt` exists and starts with `WEBVTT`.
- [ ] Run → fail. Implement. Run → pass.
- [ ] Command: render via `storage.get_transcript` / `get_cleaned_transcript`, save dialog with filter `WebVTT` `["vtt"]`, write string. Returns `Option<String>` path.
- [ ] Commit `feat(transcript): VTT mirror files + Export .vtt`.

### Task 3: calendar structured attendees
**Files:** `crates/fly-calendar/src/lib.rs` (`CalendarAttendee`, `CalendarEvent.attendees: Vec<CalendarAttendee>`, `merge_upcoming` keeps link-less events), `google.rs` (parse `displayName`/`self`/`responseStatus`), `msgraph.rs` (parse `emailAddress.name`, `status.response`; `fetch_me_addresses` `/me?$select=mail,userPrincipalName` → `is_self`), `tests/connect_flow.rs` if it constructs events, `src-tauri/src/calendar_commands.rs` (`start_meeting_from_event(title, attendees: Vec<CalendarAttendee>)`), `src-tauri/src/recording.rs` (`start_recording(note_id, seed: Option<MeetingSeed>)`, `seed_attendees()` skipping self/declined, `tracing::info!`), `frontend/src/types.ts`, `api.ts`, `Sidebar.tsx` (filter `join_url` for display), `devMock.ts` fixtures.
- [ ] Tests: google fixture with `displayName`, `self: true`, `responseStatus: "declined"`; msgraph fixture with `emailAddress.name` + `status.response = "declined"`; `merge_upcoming` no longer drops link-less; `seed_attendees` unit test in recording.rs (skip self + declined, name falls back to email).
- [ ] Run → fail. Implement. Run → pass. Commit `feat(calendar): structured attendees (name, self, declined)`.

### Task 4: auto-match Record to the in-progress event (frontend)
**Files:** `frontend/src/calendarMatch.ts` (pure `pickInProgressEvent(events, now)`), `frontend/src/App.tsx` `startRecording` builds `seed` (title only when no open note), `api.ts` `startRecording(noteId, seed)`.
- [ ] Vitest? The frontend has no test runner — keep `pickInProgressEvent` pure and verify with the 1420 dev-mock (mock event in progress → note titled after it, attendee pill shows names).
- [ ] Commit `feat(recording): seed title + attendees from the in-progress calendar event`.

### Task 5: WGC recorder
**Files:** Create `crates/fly-capture-screen/src/wgc.rs`; modify `lib.rs` (`pub mod wgc` cfg windows, `FallbackScreenRecorder`), `window_list.rs` (`find_window_hwnd(title) -> Option<isize>`), `ffmpeg.rs` (`-g 30 -movflags +faststart`), `src-tauri/src/screen_commands.rs` (build fallback recorder; ffmpeg still ensured for the fallback + thumbnails), `Cargo.toml` (`windows` features for hwnd lookup already present).
- [ ] Unit tests: `bitrate_for(w,h)` clamps 2–8 Mbps; `even(x)`; `find_window_hwnd` returns Some for a title from `list_windows()` (cfg windows).
- [ ] Ignored e2e `wgc_records_gpu_window`: pick a window whose title contains "Claude" (Electron), record 3 s, assert `moov` present; decode with `%APPDATA%/FlyOnTheWall/bin/ffmpeg` to a PNG and assert mean luma > 10 (not black).
- [ ] Implement handler (`WgcHandler { encoder: Option<VideoEncoder>, frames: Arc<AtomicU64>, crop, closed }`), `WgcSession { control, callback, out_path, started }`, `WgcScreenRecorder::start` (resolve item, size, encoder settings, `start_free_threaded`, 1.5 s health wait).
- [ ] Run tests (including the ignored e2e) → pass. Commit `feat(capture): Windows Graphics Capture backend, ffmpeg fallback`.

### Task 6: QA + ship
- [ ] `npx prettier --check` (frontend), `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `npm --prefix frontend run build`.
- [ ] Dev-mock QA on port 1420: Export .vtt button, Up next → attendee pill with names, Record during a mock in-progress event.
- [ ] Real app: `npm run tauri build` or dev run, record the Claude window for 10 s, play it back inline (scrub), confirm non-black.
- [ ] Bump 2.1.0 → 2.2.0 (root Cargo.toml, package.json, frontend/package.json, src-tauri/tauri.conf.json, devMock app_info), PR → merge → tag `v2.2.0` per release flow.
