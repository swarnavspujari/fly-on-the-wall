# Design: window capture fix, VTT transcripts, calendar attendee seeding

Date: 2026-09-12. Status: approved by the user in conversation ("everything else is
good to implement, ship once QA'd"). Windows is the only capture target in scope.

## 1. Problems

1. **Window recording is black.** `FfmpegScreenRecorder` records a window with
   ffmpeg `gdigrab -i title=…` (`crates/fly-capture-screen/src/ffmpeg.rs`). GDI
   cannot read GPU-composited windows (Zoom, Teams, Chrome, Electron), so the
   file is black for the whole meeting. Full-screen works because gdigrab reads
   the composited desktop there.
2. **Playback is clunky.** Recordings are 10 fps, x264 default GOP (one keyframe
   per ~25 s) with the MP4 index at the end, so scrubbing a 2 h file stalls.
3. **No VTT.** Transcripts mirror to `transcript.md` / `.json` only; the segment
   model already carries `start_ms`/`end_ms`/speaker.
4. **Calendar attendees never reach the meeting.** Evidence from the user's DB:
   every recent meeting has the default `Meeting YYYY-MM-DD HH:MM` title and
   `attendees_json = []`. That title is only produced by the plain Record path
   (`start_recording`), which passes no attendees. The Up next path
   (`start_meeting_from_event`) does seed attendees but only as bare emails, and
   the user did not end up on it. Nothing is logged, so this was invisible.

## 2. Capture: Windows Graphics Capture backend

New module `crates/fly-capture-screen/src/wgc.rs` (`#[cfg(windows)]`) using the
`windows-capture` crate (2.0.1, MIT). It implements the existing
`ScreenRecorder` / `ScreenSession` traits, so `screen_commands.rs` and the
frontend contract (`CaptureTarget`) do not change.

- **Target resolution.** `Window { title }` → existing `resolve_window_title` →
  new `window_list::find_window_hwnd(title)` → `Window::from_raw_hwnd`.
  `FullScreen` → `Monitor::primary()`. `Region` → primary monitor, cropped per
  frame with `Frame::buffer_crop` and sent via `send_frame_buffer`.
- **Encoding.** In-process `VideoEncoder`: H.264 in MP4 (Media Foundation,
  hardware when available), 30 fps, bitrate scaled to area (≈0.1 bit/pixel/frame,
  clamped 2–8 Mbps), even dimensions. No ffmpeg in the recording path.
- **Lifecycle.** `start_free_threaded` owns the capture thread. The handler holds
  `Option<VideoEncoder>` and a frame counter. Session `stop()` keeps the
  `callback()` Arc, calls `CaptureControl::stop()` (joins the thread), then
  takes the encoder out of the handler and calls `finish()` so the MP4 is
  finalized. `on_closed` (window closed mid-meeting) finishes the encoder and
  records the reason; `stop()` still returns the path.
- **Start health check.** After start, wait up to 1.5 s for the capture thread
  to either deliver a frame or die. A dead thread fails the start with its
  error text (same behavior the ffmpeg path has today).
- **Fallback.** `screen_commands` builds a `FallbackScreenRecorder { primary:
  Wgc, secondary: Ffmpeg }` (new in `lib.rs`). If WGC fails to start, the ffmpeg
  path is used and the fallback is logged at warn. The ffmpeg path also gets
  `-g 30 -movflags +faststart` so its output seeks properly.
- Thumbnails (`ensure_video_thumbnail`) stay on ffmpeg. Unchanged.
- Windows only: WGC needs Windows 10 1903+. macOS/Linux keep the ffmpeg path.

## 3. VTT transcripts

- `fly_core::Transcript::to_vtt()` next to `to_markdown()`: `WEBVTT` header,
  one cue per segment with the segment id, `HH:MM:SS.mmm --> HH:MM:SS.mmm`,
  `<v Label>text`. Text escapes `&`, `<`, `>`. A cue whose end ≤ start gets
  end = start + 1 ms so the file stays valid.
- Storage mirrors `transcript.vtt` and `transcript.cleaned.vtt` next to the
  existing `.md`/`.json` files (`transcript_mirror_paths` returns a third path).
  The `.md` stays: notes, search, LLM prompts and the MCP server consume it.
- New command `export_transcript_vtt(meeting_id, cleaned)` renders fresh from
  the DB and opens a save dialog (same shape as `export_note`). The transcript
  panel gains an "Export .vtt" outline button exporting the variant on screen.
- Captions on the video player are out of scope: screen clips start at a
  different moment than the audio recording, so the timelines do not line up.

## 4. Calendar attendee seeding

- `fly_calendar::CalendarAttendee { email, name: Option<String>, is_self,
  declined }`. Google fills it from `attendees[].{email,displayName,self,
  responseStatus}`. Microsoft Graph from `attendees[].{emailAddress.{address,
  name}, status.response}`; `is_self` is decided by comparing against `/me`
  (`mail` / `userPrincipalName`, fetched once per `upcoming` call).
- `CalendarEvent.attendees` becomes `Vec<CalendarAttendee>`; the frontend type
  follows. `merge_upcoming` stops dropping link-less events; the Sidebar filters
  `join_url` for display so Up next looks the same. Events without links are
  needed for matching below.
- Seeding converts to `fly_core::Attendee { name: name or email, email }`,
  skipping `is_self` and `declined`. The list stays unconfirmed (existing
  diarization gate). A `tracing::info!` line records the event title and count.
- `start_recording(note_id, seed: Option<MeetingSeed { title, attendees }>)`.
  `start_meeting_from_event` takes the struct attendees. Both go through
  `start_recording_impl`.
- Frontend `startRecording`: pick the calendar event in progress (start − 5 min
  ≤ now ≤ end, closest start wins) from the already-polled `upcoming` list. New
  meeting: seed title and attendees. Recording into an open note: seed
  attendees only. The attendee pill in the editor is the visible confirmation.

## 5. Testing and QA

- Unit: `to_vtt` formatting/escaping/degenerate cues; calendar attendee parsing
  for both providers (existing JSON fixtures extended); seed conversion skips
  self/declined; in-progress event picker (pure function, frontend test-free
  but simple); `find_window_hwnd` on a known window.
- Ignored e2e (`wgc_e2e`): record a GPU-rendered window (this machine's Claude
  desktop app, an Electron window) for 3 s, assert the MP4 has `ftyp`/`moov`,
  decode a frame with ffmpeg and assert it is not black (mean luma > 10).
- CI gates run locally before push: prettier, `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test`,
  `npm --prefix frontend run build`.
- Ship: PR to main, merge, bump to 2.2.0 and tag per the release flow.
