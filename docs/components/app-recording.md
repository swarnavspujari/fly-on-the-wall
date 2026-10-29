# Recording, capture and live captions (`app-recording`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: recording resilience (v2.5 row 3.3b, branch `claude/recording-resilience-09f112`): the capture report + `recording:audio_lost`, the shared `LiveCaptioner`, and the contention bench described below. `recording.rs` / `live.rs` line references are to `82b7edf` unless they name a function.

## Purpose
This feature turns a Record press into a meeting row plus a meeting folder of per-channel WAVs, using the [fly-audio](fly-audio.md) backend. Only one capture runs at a time. Stop writes a filesystem-only manifest first and then attaches the recording in SQLite, so a broken database can't lose the audio. While a recording runs, a live-caption loop transcribes newly appended audio with a small whisper model and streams "you" / "them" partials to the UI. It also owns meeting lookups for a note, the meeting date edit, mic listing, and the macOS system-audio consent probe.

## Files it owns
- `src-tauri/src/recording.rs` — `ActiveRecording`, `RecordingStatus`, `MeetingSeed`, `seed_attendees`, `start_recording_impl`, and 11 commands.
- `src-tauri/src/live.rs` — the live-caption loop (`spawn`/`run`), its engine-agnostic core `LiveCaptioner` (cursors, chunking, transcription → `ChunkOutcome`), `live_engine`, `take_chunk`, `LiveStatus`, and the `live_status` command.
- `src-tauri/tests/contention_bench.rs` — the local contention benchmark (spec v2.5 row 3.3b, §4.2a #3); see Tests.

## Surface area
### Public API
- `recording::start_recording_impl(app, state, note_id, title, attendees)` (`recording.rs:127-195`) — the shared start path. `calendar_commands::start_meeting_from_event` also calls it (`calendar_commands.rs:470-471`).
- `recording::seed_attendees(&[CalendarAttendee]) -> Vec<Attendee>` (`recording.rs:109-123`) — drops self and declined invitees; the name falls back to the email. It returns no confirmed flag; storage sets that.
- `recording::{ActiveRecording, RecordingStatus, MeetingSeed}` — `ActiveRecording` lives in `AppState.recording` (`state.rs:22-23`). Tests build one with a fake session (`src-tauri/tests/scheduling.rs:55`).
- `live::spawn(app, meeting_id, out_dir) -> Arc<AtomicBool>` — returns the stop flag held in `ActiveRecording.live_stop`. `live::LiveStatus` lives in `AppState.live_status` (`state.rs:45`). The module is `pub` so the bench can use `live::{LiveCaptioner, ChunkOutcome, live_engine}`.
- `recording::{CAPTURE_REPORT_FILE, AudioLost}` — the per-recording capture report file name and the `recording:audio_lost` payload.

**Lifecycle**
| Step | What happens | Where |
|---|---|---|
| Start | Refuses if a capture is active. Uses the given note or creates "Meeting <local date time>" (the seed title wins for a new note; an existing note keeps its title). Creates the meeting row with the seeded attendees (unconfirmed) and allocates `recordings/<date> <title>/`. Reads `capture.mic_device_id`, starts the capture with `capture_system: true` and `base_name "recording"`, then spawns the live loop. | `recording.rs:76-92,134-195` |
| Pause / resume | Forwarded to the session (cpal pauses its streams; the clock stops). Returns fresh status with warnings. | `recording.rs:197-217` |
| Status | Async; the UI polls it every 1 s (`App.tsx:300-307`). Session warnings (loopback failed, audio lost on a channel, macOS silent tap) come before backend warnings (Windows mute / 0% volume). | `recording.rs:47-71` |
| Stop | Takes the session out of state, raises the live stop flag, and runs `session.stop()` (writer drains + finalizes the WAVs, mixdown, playback mix) on a blocking thread. `record_capture_health` writes `recording.capture.json` next to the WAVs and, when ≥ 50 ms of audio was lost, emits `recording:audio_lost`. Converts paths to data-dir-relative. **Writes `recording.manifest.json` + `note.json` before** `end_meeting` (which sets `ended_at` and `recording_json`), then nudges the transcription queue. | `stop_recording`, `record_capture_health` |
| Live loop | Off when `live.enabled == "false"`. Ensures whisper-cli and `ggml-small-q5_1` (single download attempt) and emits `live:status` ready or unavailable. Then every 5 s: skips while paused, and stops once the recording ends or belongs to another meeting. `LiveCaptioner::tick`: for each channel it reads only the 8–30 s of new samples (seek past what it consumed), resamples to 16 kHz, writes a scratch chunk and transcribes on CPU; the loop emits `live:segment`. After 3 straight failures it emits "unavailable"; a later success flips back to "ready". | `live.rs` `run`, `LiveCaptioner` |

**While a capture is active** (`AppState.recording` is `Some`), other work is held back:
- The transcription queue doesn't start a job (`scheduler.rs:91-100`, `recording_active` at `scheduler.rs:468-470`).
- The retention sweep is skipped, and "Delete all my data" refuses (`privacy.rs:258-261,404-406`).
- The note being recorded can't be deleted (`commands.rs:283-287,320-325`).
- Updates are deferred, but only in the frontend: `updater.ts:10-13,148-163` reads the polled `recStatus.active || screenStatus.active` (`App.tsx:186-187`).
- `preflight_system_audio` returns `Inconclusive` instead of probing (`recording.rs:345-347`).

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `recording_status` | none | `Result<RecordingStatus, String>` | `ui-app-shell` (`App.tsx:303`, 1 s poll), via `api.recordingStatus` |
| `start_recording` | `note_id: Option<String>, seed: Option<MeetingSeed>` | `Result<RecordingStatus, String>` | `ui-app-shell` (`App.tsx:688-711`; Record in `Home` and `Editor`, seed from `calendarMatch.ts`), via `api.startRecording` |
| `pause_recording` | none | `Result<RecordingStatus, String>` | `ui-recording-bar` (`RecordingBar` `onPause`, `App.tsx:993`), via `api.pauseRecording` |
| `resume_recording` | none | `Result<RecordingStatus, String>` | `ui-recording-bar` (`RecordingBar` `onResume`, `App.tsx:994`), via `api.resumeRecording` |
| `stop_recording` | none (the `AppHandle` is injected) | `Result<Meeting, String>`; may emit `recording:audio_lost` first | `ui-recording-bar` (`RecordingBar` `onStop` → `App.tsx:714-735`), via `api.stopRecording` |
| `get_meeting_for_note` | `note_id: String` | `Result<Option<Meeting>, String>` | `ui-app-shell` (`App.tsx:420,666`), via `api.getMeetingForNote` |
| `get_meetings_for_note` | `note_id: String` | `Result<Vec<Meeting>, String>` (newest first) | `ui-app-shell` (`App.tsx:538,700,722`), via `api.getMeetingsForNote` |
| `update_meeting_started_at` | `meeting_id: String, started_at: String` (RFC 3339) | `Result<Meeting, String>` | `ui-editor` (`Editor.tsx:663`, date editor), via `api.updateMeetingStartedAt` |
| `list_mic_devices` | none | `Result<Vec<AudioDevice>, String>` | `ui-settings` (`SettingsModal.tsx:376`), via `api.listMicDevices` |
| `preflight_system_audio` | none | `Result<SystemAudioPreflight, String>` | `ui-app-shell` (`App.tsx:273-292`, macOS only, after consent), via `api.preflightSystemAudio` |
| `open_privacy_settings` | none | `Result<(), String>` (`Err` off macOS) | `ui-recording-bar` (`App.tsx:987-991`) and `ui-notices` (`AppNotices`, `App.tsx:1099`), via `api.openPrivacySettings` |
| `live_status` | `meeting_id: String` | `Option<LiveStatus>` (not a `Result`) | `ui-recording-bar` (`LivePane.tsx:45-52`, catch-up on mount), via `api.liveStatus` |

devMock mirrors all 12 (`devMock.ts:1125-1131,1177-1181,1209-1240,1321-1325,1496-1506`). Where it drifts:
- start, pause, resume and stop never fail: there is no "a recording is already in progress" or "no active recording" error.
- `live_status` always returns `null`, and no `live:segment` events are ever sent.
- `get_meetings_for_note` returns at most one meeting.
- `stop_recording` emits `recording:audio_lost` (1.2 s mic, 0.4 s system) only with `fotwMockAudioLost=1`; `fotwMockRecWarning=lost` shows both live lost-audio warnings.

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `live:status` | emits (`live.rs:48-51`) | `{meeting_id, state: "ready" \| "unavailable", detail}` | `LivePane.tsx:37`; also stored for the `live_status` catch-up |
| `live:segment` | emits (`live.rs:307-315`) | `{meeting_id, channel: "you" \| "them", text, start_ms}` (`start_ms` is on that channel's own timeline) | `LivePane.tsx:31` (sorted by `start_ms`, keeps the last 200) |
| `model:progress` | emits (`live.rs:102-107`) while the live engine or model downloads | `models::ModelProgress` | `App.tsx:450` |
| `recording:audio_lost` | emits (`record_capture_health` → `audio_lost_payload`, during `stop_recording`) when a channel lost ≥ `fly_audio::LOST_AUDIO_WARN_MS` (50 ms) — the same per-side rule as the live chip, so a side under it reads 0 | `{meeting_id, note_id, mic_lost_ms, system_lost_ms}` | `AppNotices.tsx` `useAudioLost` → "Part of the last recording is missing" notice ([ui-notices](ui-notices.md)) |

Stop also calls `jobs_notify.notify_one()` in-process.

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `capture.mic_device_id` | read (`recording.rs:167-171`); written by Settings via `set_app_setting` (`SettingsModal.tsx:1385`) | yes (`capture.`) | mic device name; empty = system default |
| `live.enabled` | read (`live.rs:88-97`) | **no** (not under `consent.`/`capture.`/`ui.`) | `"false"` turns live captions off. Nothing in the repo writes it, so the opt-out can't be reached (inferred). |
| `consent.recording_notice_accepted` | not read by the backend; the frontend gates the macOS probe on it (`App.tsx:282`) | yes | first-run recording notice accepted |

### Database
| Table | Columns | Read / write |
|---|---|---|
| `notes` | new row when recording without a note (`create_note`); `meeting_id` repointed to the new meeting (`meetings.rs:64-67`) | write |
| `meetings` | insert `id, title, note_id, attendees_json, started_at` (`meetings.rs:57-63`; `attendees_confirmed` keeps its column default 0); `keep_local` inherited from earlier meetings of the note (`privacy.rs:266-273`); `ended_at, recording_json` at stop (`meetings.rs:139-151`); `started_at, ended_at` on a date edit (`meetings.rs:161-171`) | read / write |
| `settings` | `capture.mic_device_id`, `live.enabled` | read |

### Files and directories written
- `<data_dir>/recordings/<date> <title>/`, allocated at start and deduped for same-day duplicate titles (`meetings.rs:272-277`). Inside it:
  - `recording.mic.wav` and `recording.system.wav` (per channel, mono 16-bit, native rate),
  - `recording.mixed.wav` (16 kHz),
  - `recording.playback.wav` (only when both channels exist),
  - at stop: `recording.manifest.json` and `note.json` (`recovery.rs:35,40,680-719`),
  - at stop: `recording.capture.json` — `{schema: 1, duration_ms, mic, system}` with each channel's `fly_audio::ChannelReport` (lost / overflow / padded ms, events, xruns, last loss time, write errors, real-time priority, the most audio ever waiting in the OS buffer, losses dropped upstream by the OS audio engine). Written for every recording, so a clean capture is on record as clean. Not media: retention and "delete audio" leave it, like the manifest.
- After the transcript is final, recording storage v2 ([app-audio-store](app-audio-store.md)) replaces the mic/system WAVs with lossless `recording.{mic,system}.flac` and drops `mixed`/`playback` when they regenerate bit-identically. Capture itself is unchanged and still writes WAV.
- Stored paths are data-dir-relative with `/` separators (`recording.rs:241-245`). A path outside `<data_dir>` silently becomes `None` (inferred edge).
- A date edit renames the folder (only once a recording is attached) and rewrites the manifest (`meetings.rs:173-182`).
- Live captions write `<scratch_root>/<meeting id>/live-tmp/{you|them}-<start_sample>.wav` (16 kHz chunks). Each chunk is deleted after transcription, and the folder is removed when the loop ends (`live.rs:202-206,242,295,325`). After a crash, the startup scratch sweep removes leftovers (`scratch.rs:1-16`).
- macOS probe: a temp WAV in the OS temp dir (see [fly-audio](fly-audio.md)).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `github.com` (whisper.cpp CLI release asset for this OS, `models.rs`) | the live loop starts and whisper-cli isn't installed (`live.rs:112-134`) | none; automatic, a single attempt (`models.rs:657-660`). Downloads a tool, never meeting data. |
| `huggingface.co` (`ggml-small-q5_1.bin`, `models.rs:228-235`) | the live loop starts and the small model is missing (`live.rs:135-155`) | none; automatic, a single attempt, no `hf-mirror.com` fallback |

No meeting audio or text leaves the machine from this component.

### External processes
- `whisper-cli`, one run per live chunk, on CPU (`-ng` on macOS via `force_cpu`), with `clamp(cores/4, 2, 4)` threads and no checkpointing (`live.rs:156-178,251`). The 24-word tail of the previous chunk is passed as the prompt (`live.rs:246-250,298-306`).
- macOS: `open x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture` (`recording.rs:357-365`).
- macOS: a ~1.5 s throwaway Core Audio tap for `preflight_system_audio` (`recording.rs:348`).

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `src-tauri/src/lib.rs:128-139` (command registration); `calendar_commands.rs:470-471` (`start_meeting_from_event`). These read `AppState.recording`: `scheduler.rs:468-470`, `privacy.rs:259,405`, `commands.rs:285`, and `live.rs:223-232` itself. `src-tauri/tests/scheduling.rs` injects a fake `ActiveRecording`.
- **Outbound:**
  - [fly-audio](fly-audio.md): the capture trait, `preflight_system_audio`, `mix::{resample_linear, write_wav_mono_16}`.
  - [fly-storage](fly-storage.md): `create_note`, `create_meeting`, `allocate_meeting_dir`, `stash_recording_manifest`, `end_meeting`, `get_meeting_for_note`, `meetings_for_note`, `set_meeting_started_at`, `get_setting`, `scratch::meeting_scratch_dir`.
  - [fly-asr](fly-asr.md): `WhisperCppEngine`.
  - [fly-calendar](fly-calendar.md): `CalendarAttendee`.
  - [fly-core](fly-core.md): `Meeting`, `RecordingRef`, `Attendee`.
  - `models.rs` (`ensure_tool_with`, `ensure_with`, `DownloadEffort::SingleAttempt`).

## Invariants
- **At most one audio capture at a time** (`state.rs:22-23`; `recording.rs:134-137`).
- **Calendar-seeded attendees stay unconfirmed, and only a confirmed roster sets `num_speakers`** (spec §2 item 7). `seed_attendees` carries no flag (`recording.rs:105-123`). `create_meeting` hard-codes `attendees_confirmed: false` (`fly-storage meetings.rs:42-53`). Only `update_attendees` (the editor's Save) sets it (`meetings.rs:79-84`). `pipeline::speaker_hint` returns `Unknown` unless confirmed (`pipeline.rs:43-51`).
- **The audio never depends on SQLite.** The manifest is stashed before `end_meeting`, and startup self-heal re-attaches from it (`recording.rs:254-271`; `recovery.rs:1-23`; DB incident 2026-07-13).
- **Recording into an existing note keeps its title;** the seed fills only attendees (`recording.rs:83-87`).
- **No transcription, retention sweep or wipe runs during a capture** (`scheduler.rs:91-100`; `privacy.rs:258-261,404-406`).
- **An update never interrupts a recording.** Enforced only in the frontend (`updater.ts:10-13,148-163`; `AppNotices.tsx:298`; `SettingsModal.tsx:1216,1234`).
- **The macOS TCC probe runs only after the recording notice is accepted** (spec §2 item 6). Enforced only in the frontend (`App.tsx:273-292`). The backend command has no consent check, and `start_recording` on macOS also builds a tap without one.
- **Live captions stay on CPU, never the GPU,** with a quarter-core budget, so they can't starve the capture or the call (`live_engine`).
- **Lost audio is never silent.** The recording bar shows it live (fly-audio's lost-audio warning, "Audio lost" chip), Stop stores it in `recording.capture.json` and raises the `recording:audio_lost` notice. The gap itself is silence of the same length, so transcripts stay aligned.
- **Live chunks go to per-meeting scratch, not the meeting folder,** which a rename can move (`live.rs:202-205`; `scratch.rs:1-10`).
- **Live partials are throwaway.** The post-Stop pipeline replaces them (`live.rs:1-8`).
- **The participant-consent duty (THREAT_MODEL T12)** is a first-run notice in the frontend (`App.tsx:1131-1137`). The backend doesn't check it.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/recording.rs` | 1 | `seed_attendees`: skips self and declined, email fallback |
| unit | `src-tauri/src/live.rs` | 1 | `take_chunk`: 8 s minimum, consumed offset |
| integration (CI) | `src-tauri/tests/scheduling.rs`, `scheduling_e2e.rs` | — | fake sessions return `CaptureOutput` with a default `health` |
| manual (local, ignored) | `src-tauri/tests/contention_bench.rs` (`contention_bench`, helpers `analyze_kept_recording`, `tone_player_child`) | 1 | A real 10-min recording of a marker tone played through system audio while a re-embed (`embed_raw`, batches of 16; it steps aside while the recording runs, like the app's embedding worker, or runs throughout with `FOTW_BENCH_LOAD=embed_unpaced,…`), a pipeline run already in flight (`pipeline::run_with` on a ~4 min meeting, restarted back to back, CPU), an Ask every 30 s (`build_ask_system` + Ollama `llama3.1`), ~50 MB/s of disk writes and live captions (`LiveCaptioner`) run. Reports gaps > 10 ms in the system channel (`fly_audio::testtone::analyze`), mic sample-count drift, Ask p50/p95, caption lag, CPU, and the capture's own report, as JSON outside the repo (`~/fotw-bench/results/`). `FOTW_BENCH_INDUCE_GAP_AT_SECS` freezes the process to prove a loss is reported. Targets: 0 gaps, \|drift\| < 0.1%. Never in CI: plays sound, needs Ollama, downloads models into its own scratch data dir. **Results (PR #116, i7-12700H):** drift is met in every run (−9.7% before → ±0.007%). 0 gaps is met without the bench's own load (other sessions' builds still at ~98% CPU) but **not under it**: with the CPU saturated, the Windows audio engine/driver drops audio upstream (the app's buffer peaked under 0.5 s and never overflowed; ~99% of loss events had no near-full buffer). That audio is filled with silence and reported; preventing it needs the background work to back off during a recording. With the re-embed stepping aside while recording (#112), the same load gave 29 gaps and 2.0 s mic / 1.5 s system lost, down from 304 gaps and 25.4 s / 8.8 s, on a machine still at 97% CPU from other work. |
| integration (CI) | `src-tauri/tests/scheduling.rs` (`queue_defers_while_recording_then_attempts_after`) | 1 | the queue waits while a (fake) capture is active |
| unit (other owner) | `src-tauri/src/commands.rs` (`delete_guard_tests`) | 2 | the note being recorded can't be deleted |
| integration (other owner) | `crates/fly-storage/tests/storage_safety.rs`, `crates/fly-storage/src/meetings.rs` | — | manifest stash and self-heal; a new meeting starts unconfirmed (empty attendee list only) |
| vitest (other owner) | `frontend/src/recordingBar.test.ts`, `components/RecordingBar.test.tsx`, `App.test.tsx` | — | warning chips, pause and resume UI, recording flows against devMock |
| ignored e2e | `src-tauri/tests/rediarize_e2e.rs` | 1 | a confirmed roster → `num_speakers` (never in CI) |
| manual | `docs/TESTING.md` (M2, M9, M11, M14, auto-update) | — | real capture; mute warning within ~1 s; live partials within ~20 s; no update banner while recording |

**Known gaps:**
- **No test drives start, pause, resume or stop through this module.** The commands need an `AppHandle`, and `AppState::init_with` hard-codes `CpalAudioCapture` (`state.rs:144`). So the manifest-before-database ordering, path relativizing, the stop error path and status warnings ordering are untested here.
- **Spec §2 item 7 has no CI test at any seam.** The seed test doesn't touch the flag. The storage test checks "unconfirmed" only with an empty list (`meetings.rs:476-478`). `speaker_hint` has no test, and the only confirmed→`num_speakers` check is `#[ignore]`d.
- **Stop failure orphans the audio (inferred).** If `session.stop()` fails (mixdown or finalize error, dead audio thread), the command returns before the manifest is written (`recording.rs:236-239`). A crash mid-recording is the same. Self-heal only scans folders that have a manifest (`recovery.rs:172-176`), so the WAVs stay in the folder with nothing pointing at them.
- **Live captions under load.** The contention bench measures caption lag (`take_chunk` now reads only the new samples). There is still no catch-up when transcription runs slower than real time, and partials skip `guard_loops` while feeding the previous text back as the prompt, a known Whisper loop amplifier.
- **A pipeline already running keeps running during a recording.** The scheduler only refuses to *start* a job while recording (`scheduler.rs` `tick`); an in-flight transcription competes with the capture (the embedding worker steps aside while recording, see [app-search](app-search.md)). The capture now survives that (contention bench), but the load still slows captions and Ask.
- **Spec §2 item 6 ordering** (no probe before consent) and `calendarMatch.ts` (`pickInProgressEvent`, `seedFromEvent`) have no vitest.

## Changing it safely
- **New or renamed command:** register it in `src-tauri/src/lib.rs:128-139`, wrap it in `api.ts`, add a `types.ts` type, and mirror it in `devMock.ts` (spec §2 item 4). Then document it here.
- **Payload shapes** (`RecordingStatus`, `LiveStatus`, `LiveSegment`) are copied in `types.ts` and in local interfaces in `LivePane.tsx:7-18`. Change them together.
- **Warning strings** come from fly-audio; `recordingBar.ts` and devMock match them by substring.
- **Keep the stash-before-database order** in `stop_recording`. Never put a fallible step between `session.stop()` and the manifest without also writing a manifest on failure.
- **Background work that touches `recordings/` or runs heavy compute** must check `scheduler::recording_active`.
- **macOS:** anything that builds a tap can trigger the TCC prompt, so keep it behind the consent gate (spec §2 item 6).
- **Run:** `cargo test -p fly-app --lib -- recording:: live::`, `cargo test -p fly-app --test scheduling`, and vitest for the recording bar and notices. Then the `docs/TESTING.md` recording items on real hardware (Windows mute warning, macOS signed build). For capture or background-load changes on Windows, the contention bench: `cargo test -p fly-app --release --test contention_bench -- --ignored --exact contention_bench --nocapture` (≈13 min, plays a quiet tone).
- **`recording:audio_lost` payload** is copied in `frontend/src/notices.ts` (`AudioLost`) and devMock's `stop_recording`. Change them together.
