# Recording bar and live pane (`ui-recording-bar`)

> Layer: frontend feature. Described at main `82b7edf` (v2.4.0).
> In flight: recording resilience (branch `claude/recording-resilience-09f112`): the "Audio lost" chip below.

## Purpose
The one-row bar pinned above every view while audio is recording: indicator and timer, the note's title, the You / Others health chips built from the backend's capture warnings, the recording notice (copy, "Participants notified", start-of-recording reminder), Pause/Resume and Stop. Also the live, rough transcript pane shown in the recording note's Transcript view.

## Files it owns
- `frontend/src/components/RecordingBar.tsx` — the bar, health-chip popovers, notice reminder, `fmtElapsed`.
- `frontend/src/recordingBar.ts` — pure rules: warning classifier, health chips, the "Remind me" key and default.
- `frontend/src/components/LivePane.tsx` — live caption bubbles (channel-level "You" / "Them") and the Listening banner.

## Surface area
### Public API
| Export | Where | Notes |
|---|---|---|
| `RecordingBar` (default) | `RecordingBar.tsx:329` | Props: `status: RecordingStatus`, `noteTitle`, `screenActive?`, `screenSource?`, `onPause`, `onResume`, `onStop`, `onOpenNote`, `onOpenPrivacySettings?`. Renders nothing unless `status.active` (`:344`). |
| `fmtElapsed(ms)` | `RecordingBar.tsx:32-40` | `m:ss` or `h:mm:ss`; also used by `LivePane`. |
| `classifyWarning(text)` | `recordingBar.ts` | → `{side, label, text, privacyPane}`. |
| `chipHeading(chip)` | `recordingBar.ts` | The warning popover's heading: "Part of your microphone audio was lost" / "Part of the other participants’ audio was lost" when every warning on the side is "Audio lost", else the "may be missing from this recording" heading. |
| `healthChips(warnings, paused)` | `recordingBar.ts:64-76` | Always two chips, You first. No warning: You = `unmonitored`, Others = `ok` (both `paused` while paused). With warnings: `"Others · <first label> +N"`. |
| `REMIND_NOTICE_KEY`, `remindNoticeOn` | `recordingBar.ts:9-14` | `ui.recording.remind_notice`; on unless the stored value is `"false"`. |
| `MACOS_PRIVACY_PANE` | `recordingBar.ts:18` | `"Screen & System Audio Recording"`, a copy of `PRIVACY_PANE` (`crates/fly-audio/src/tap_messages.rs:15`). |
| `LivePane` (default) | `LivePane.tsx:23` | Prop: `meetingId`. Mounted by the Editor while this note records (`Editor.tsx:1442-1446`). |

**Warning classifier.** Most warnings concern the Others (system audio) side; lost audio names its side. Labels are tested in this order (`recordingBar.ts` `classifyWarning`):

| Chip label | Matched by | Backend sentence | Rust source | When |
|---|---|---|---|---|
| Blocked by macOS (+ "Open Settings") | contains `Screen & System Audio Recording`, or `recording only silence` | "System audio is playing but its capture is recording only silence — macOS is blocking system-audio capture for this build, … Open System Settings → Privacy & Security → Screen & System Audio Recording …" | `crates/fly-audio/src/tap_messages.rs:15`, `:25-35`; raised by `coreaudio_tap.rs:154-167` through `CaptureHealth` (`cpal_backend.rs:153-158`) | macOS, re-checked every poll |
| Device changed (You side if it says `microphone`, else Others) | `changed or disconnected` | "Your microphone changed or disconnected — your side may be missing from the rest of this recording. …" / "The sound output changed or disconnected — …" | `cpal_backend.rs` `device_lost_warning` (a `DeviceNotAvailable` / `DeviceChanged` / `StreamInvalidated` stream error) | Any OS, from then on |
| Not saving (You side if it says `microphone`, else Others) | `couldn't be saved` | "Part of your microphone audio couldn't be saved — the disk may be full or unavailable. …" | `cpal_backend.rs` `write_failed_warning` (the WAV writer failed a block) | Any OS, from then on |
| Audio lost (You side if it says `microphone`, else Others) | `was lost` | "Your microphone has gaps: 1.2 s was lost because the computer was too busy (most recently at 12:34). The rest is still recording — closing heavy apps can help." / "The other participants' audio has gaps: …" | `crates/fly-audio/src/cpal_backend.rs` `lost_audio_warning`, raised by `CaptureHealth::warnings` once a channel lost ≥ 50 ms (ring overflow or WASAPI capture-timestamp gap); stays for the rest of the recording | Any OS (device-level gaps: Windows only), every poll |
| Not recorded | `couldn't start` (ASCII apostrophe) | "System audio capture couldn't start (<cause>) — only your microphone is being recorded, so the other participants won't be in this recording." | `crates/fly-audio/src/cpal_backend.rs:359-362` | Any OS, set once at start |
| Volume at 0% | `volume is at 0%` | "System volume is at 0% — the other participants' audio will record as silence. Raise your system volume." | `cpal_backend.rs:84-88` | Windows only (`cfg`), every poll |
| Output muted | `muted` | "System output is muted — the other participants' audio will record as silence. Unmute your speakers." | `cpal_backend.rs:77-82` | Windows only, every poll |
| Check your mic (You side) | `microphone` but not `only your microphone` | none today: lost-audio sentences match "Audio lost" first | — | — |
| Check audio | anything else | — | — | — |

The backend lists startup ("session") warnings before the per-poll ones (`src-tauri/src/recording.rs:47-58`), so the chip names the loopback failure first.

### Tauri commands
- Calls directly: `get_app_setting` (`ui.recording.remind_notice`, `RecordingBar.tsx:62`), `get_meeting_privacy` (`:63-66`), `copy_recording_notice` (`:78-79`; the clipboard write happens natively), `set_participants_notified` (`:90`), `get_asr_settings` (`:117-118`), `live_status` (`LivePane.tsx:45-52`).
- Through App: `recording_status` (polled every 1 s, `App.tsx:301-307`), `pause_recording`, `resume_recording`, `stop_recording` then `transcribe_meeting` (`App.tsx:714-736`), `open_privacy_settings` (macOS only, `App.tsx:988-992`).
- devMock mirrors all of them. Drift: with only `fotwMockRecording=1` the warning is a sentence the backend never sends ("System output looks muted — …", `devMock.ts:822-824`), and the `fotwMockTapDenied` warning is an abridged macOS text (`devMock.ts:820`). The `fotwMockRecWarning` set is an exact copy of today's Rust text (`devMock.ts:1566-1584`), but nothing keeps it in sync. `live_status` always returns `null` and devMock never emits `live:*` events by itself (`devMock.ts:1131-1132`). `copy_recording_notice` uses `navigator.clipboard` in the browser (`devMock.ts:1439-1441`).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `live:segment` | listens, `LivePane.tsx:31-36` | `{meeting_id, channel: "you" \| "them", text, start_ms}` | `src-tauri/src/live.rs:28-35`, emitted `:307-315` |
| `live:status` | listens, `LivePane.tsx:37-40` | `{meeting_id, state: "ready" \| "unavailable", detail}` | `src-tauri/src/live.rs:37-51` (also stored for `live_status` catch-up) |
| `keydown` Escape | listens on the reminder only, `RecordingBar.tsx:268-272` | — | not the global Esc stack, on purpose |

The bar has no Tauri events of its own: `status` comes from App's 1 s poll, so a new warning shows within about a second.

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `ui.recording.remind_notice` | R `RecordingBar.tsx:62` (written by `RemindNoticeRow`, [ui-notices](ui-notices.md)) | yes (`ui.`) | Show the "Let participants know" reminder at start |
| devMock toggles | — | n/a | `fotwMockRecording`, `fotwMockRecWarning` (`none` / `muted` / `volume` / `loopback` / `macos` / `worst` / `lost` / `device` / `disk`), `fotwMockTapDenied`, `fotwMockNoAttendees`, `fotwMockAudioLost` (Stop emits `recording:audio_lost`, shown by [ui-notices](ui-notices.md)) |

### Database
None directly. "Participants notified" is stored per meeting behind `get_meeting_privacy` / `set_participants_notified` (see [app-privacy](app-privacy.md)).

### Files and directories written
None.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None | — | — |

### External processes
None (live captions run whisper in the backend, see [app-recording](app-recording.md)).

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `App.tsx:983-997` mounts `RecordingBar` above every view; `Editor.tsx:1442-1446` mounts `LivePane`.
- **Outbound:** `api.ts`, `privacy.ts` (`recordingNoticeText`), `ui.tsx` (`RecordingIndicator`, `Checkbox`, `Button`, `useEscapeLayer`, `useDialogFocus`, `EscapeLayerScope`), `@tauri-apps/api/event`.

## Invariants
- Nothing in the bar gates the capture; the notice controls are advice (THREAT_MODEL T12; `RecordingBar.tsx:42-45`).
- The reminder is on unless explicitly turned off; an unreadable setting or privacy row means "remind" (`recordingBar.ts:11-14`, `RecordingBar.tsx:59-71`). Ticking "Participants notified" closes it.
- The reminder never takes focus and only closes on Esc from inside it; closing moves focus to the notice button (`RecordingBar.tsx:251-273`, `:345-350`).
- The You chip never claims the mic is fine: its level isn't measured yet (`recordingBar.ts:5-6`). It does turn to "Audio lost" when the capture dropped mic audio.
- A warning's popover is an Esc layer with dialog focus; if the warning clears while it's open, focus returns to the chip (`RecordingBar.tsx:146-155`).
- "Open Settings" appears only for a warning naming the macOS privacy pane and only when App passes the handler (macOS) (`RecordingBar.tsx:231`, `App.tsx:988-992`).
- The classifier keys on backend sentences. Only the lost-audio sentence is pinned on both sides (`lost_audio_warning_matches_the_recording_bar_classifier` in Rust, `MIC_LOST`/`SYSTEM_LOST` in `recordingBar.test.ts`); the mute / volume / loopback strings have no Rust test, and the `tap_messages.rs` test checks `PRIVACY_PANE` against its own constant.
- The Stop label reads "Transcribe when I stop" from `get_asr_settings`, re-read on hover and focus (`RecordingBar.tsx:103-128`, `:462-472`); App decides whether to transcribe from its own copy, refreshed when Settings closes (`App.tsx:217-226`, `:725`).
- The live pane catches up on a status emitted before it mounted, keeps at most 200 segments sorted by start, and ignores other meetings (`LivePane.tsx:28-57`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest (jsdom, devMock backend) | `frontend/src/components/RecordingBar.test.tsx` | 17 | One-row bar, Stop label, chips and popovers, focus return, notice copy, notified state, reminder rules, Pause/Resume |
| vitest (node) | `frontend/src/recordingBar.test.ts` | 17 | Classifier labels (incl. lost audio on each side), chip tones, popover heading, remind default |
| vitest (jsdom, devMock) | `frontend/src/App.test.tsx` | 1 (mounts `LivePane` while recording) | Only with a `scrollIntoView` stub; nothing asserted about the pane |

**Known gaps:**
- No cross-language check of the warning sentences. `recordingBar.test.ts:4-13` says its fixtures are "the exact sentences the backend sends today"; the macOS one is already abridged, and a Rust rewording (for example `couldn't` → `couldn’t`, the curly apostrophe the TS copy uses elsewhere) passes both suites.
- `LivePane` has no test: catch-up via `live_status`, per-meeting filtering, the 200 cap, the "unavailable" banner.
- jsdom applies no CSS: responsive classes (`lg:hidden`, `xl:inline`) and the "one row" layout aren't checked.
- Pause/Resume and Stop aren't tested end to end through App against the poll.

## Changing it safely
- Run `npm --prefix frontend test` (RecordingBar, recordingBar, App).
- Changing a warning sentence in `crates/fly-audio` (`cpal_backend.rs`, `tap_messages.rs`): update `classifyWarning`, both test fixtures and devMock's `MOCK_REC_WARNINGS` in the same PR. Keep `couldn't` with an ASCII apostrophe, or change the matcher.
- A new warning kind: add a label in `classifyWarning` and a case in `recordingBar.test.ts`; decide which side it belongs to.
- Keep the reminder out of the global Esc stack, so an Esc meant for the editor doesn't close it.
- The bar must stay one fixed-height row so the editor below never moves.
