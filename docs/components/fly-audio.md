# Audio capture and DSP (`fly-audio`)

> Layer: crate. Described at main `82b7edf` (v2.4.0).
> In flight: recording resilience (v2.5 row 3.3b, branch `claude/recording-resilience-09f112`): the ring + writer capture path, gap accounting and `testtone` described below. Line references in `cpal_backend.rs`, `coreaudio_tap.rs`, `pulse_loopback.rs` and `sink.rs` are to that branch; the rest are to `82b7edf`.

## Purpose
This crate defines the `AudioCapture` / `CaptureSession` traits and the one real implementation. It records the microphone and the system output ("them") as separate mono WAV channels, then renders a 16 kHz mixdown for ASR and a native-rate playback mix. It also ships pure audio helpers (WAV I/O, resampling, peak normalizing, an adaptive energy VAD with stitching) that fly-asr, the pipeline and import reuse. Finally, it has the macOS system-audio consent probe and the user-facing text for macOS TCC (privacy permission) problems.

## Files it owns
- `crates/fly-audio/Cargo.toml` — `cpal`, `hound`, `rtrb` (lock-free SPSC ring); per-OS deps: `windows` 0.62 (Windows; `Win32_System_Threading` for MMCSS); `objc2*`, `block2`, `dispatch2`, `core-foundation` (macOS); `libpulse-binding`, `libpulse-simple-binding` (Linux). Lists `fly-core` but never uses it.
- `crates/fly-audio/src/lib.rs` — traits, `CaptureConfig`/`CaptureOutput`/`CaptureState`/`AudioDevice`/`AudioError`, `SystemAudioPreflight`, the `preflight_system_audio` dispatcher, and the cfg gates for the per-OS modules (`lib.rs:12-22`).
- `crates/fly-audio/src/cpal_backend.rs` — `CpalAudioCapture`: an audio thread that owns the streams, a pause-aware `Clock` (with a resume `epoch`), the capture callbacks (downmix, gap detection, pad-to-clock), and stop → writer finish, then mixdown and playback mix. It picks the loopback backend per OS, reports Windows mute/volume warnings, lost-audio warnings and macOS tap health.
- `crates/fly-audio/src/sink.rs` — the real-time-safe channel path: `ChannelProducer` (callback side, wait-free), `ChannelSink` + `WriterThread` (all file I/O), `ChannelStats` / `ChannelReport` (per-channel loss accounting) and `boost_capture_thread` (MMCSS Pro Audio on Windows).
- `crates/fly-audio/src/testtone.rs` — the contention bench's marker tone: `ToneSpec`, `TonePlayer` (default output, MMCSS, 1 s buffer) and `analyze` (padded / removed / inserted audio in a captured tone).
- `crates/fly-audio/src/coreaudio_tap.rs` — **macOS only.** A Core Audio process tap feeds a private aggregate device and an IOProc. The 14.2+ symbols are resolved with `dlsym`. Also `TapHealth` silence detection and `preflight()`.
- `crates/fly-audio/src/pulse_loopback.rs` — **Linux only.** `PulseRecorder` reads `@DEFAULT_MONITOR@` at 48 kHz on its own thread (PulseAudio or PipeWire).
- `crates/fly-audio/src/win_volume.rs` — **Windows only.** Default render endpoint volume scalar and mute flag over COM.
- `crates/fly-audio/src/tap_messages.rs` — platform-neutral text for the macOS TCC problems (`BUNDLE_ID`, `PRIVACY_PANE`, the `tccutil` command, the silence banner), so CI can test it on every OS.
- `crates/fly-audio/src/null.rs` — `NullAudioCapture`: a no-op backend. No code uses it, including tests.
- `crates/fly-audio/src/mix.rs` — WAV read/write, linear resample, `normalize_peak`, `mix_tracks`, `write_mixdown` (16 kHz), `write_playback_mix`.
- `crates/fly-audio/src/vad.rs` — adaptive energy VAD (`detect_speech_spans`), `stitch_spans`, `map_to_original`.
- `crates/fly-audio/examples/capture_smoke.rs` — manual hardware smoke test: records ~4 s with a pause, then prints per-channel RMS. CI compiles it (clippy `--all-targets`) but never runs it.

## Surface area
### Public API
| Group | Items | Used by |
|---|---|---|
| Traits | `AudioCapture` (`list_mic_devices`, `supports_system_loopback`, `start`, `capture_warnings`), `CaptureSession` (`pause`, `resume`, `stop`, `state`, `elapsed_ms`, `warnings`, `health`) | `src-tauri/src/state.rs:20-23`, `recording.rs`; `src-tauri/tests/scheduling.rs` and `scheduling_e2e.rs` fake `CaptureSession` |
| Types | `AudioError`, `Result`, `AudioDevice`, `CaptureConfig`, `CaptureState`, `CaptureOutput` (now with `health: CaptureHealthReport`, `#[serde(default)]`) | app-recording |
| Loss accounting | `CaptureHealthReport { mic, system: Option<ChannelReport> }`, `ChannelReport` (`lost_ms`, `overflow_ms`, `padded_ms`, events, `xruns`, `last_loss_at_ms`, `write_errors`, `realtime_priority`, `max_backlog_ms`, `upstream_lost_events`), `LOST_AUDIO_WARN_MS` (50) | `recording.rs` (`recording.capture.json`, `recording:audio_lost`) |
| Bench tone | `testtone::{ToneSpec, TonePlayer, analyze, ToneReport, ToneGap}` | `src-tauri/tests/contention_bench.rs` |
| Consent probe | `SystemAudioPreflight` (serde `{"verdict", "detail"}`), `preflight_system_audio(duration_ms)` (`lib.rs:95-129`) | `recording.rs:341-351` |
| Backends | `cpal_backend::CpalAudioCapture` (the only impl wired, at `state.rs:85`); `null::NullAudioCapture` (unused) | app composition root |
| Mix | `MIX_SAMPLE_RATE`, `read_wav_mono`, `resample_linear`, `normalize_peak`, `mix_tracks`, `write_wav_mono_16`, `write_mixdown`, `write_playback_mix` (`mix.rs`) | fly-asr `whisper_cpp.rs`, `groq.rs`; `pipeline.rs`, `import_commands.rs`, `gpu.rs`, `live.rs` |
| VAD | `VadConfig`, `SpeechSpan`, `detect_speech_spans`, `StitchMap`, `stitch_spans`, `map_to_original` (`vad.rs`) | fly-asr `whisper_cpp.rs`, `groq.rs`, `checkpoint.rs`; `gpu.rs` |
| TCC text | `tap_messages::{BUNDLE_ID, PRIVACY_PANE, tccutil_reset_command, silence_warning_text}` | `coreaudio_tap.rs:167`; the frontend copies the text (`recordingBar.ts`, `devMock.ts`) |
| crate-internal | `Clock`, `idle_pad_frames`, `lost_audio_warning`, `sink::{channel, ChannelProducer, ChannelSink, WriterThread, ChannelStats}` (shared by all three loopback backends and the mic), `TapRecorder`, `TapHealth`, `PulseRecorder` | — |

**Which code compiles and runs on which OS**
| Code | Compiled on | Runtime behavior | Tests |
|---|---|---|---|
| `cpal_backend.rs` mic path | all | default or named input device, native rate; first packet aligned to the clock | warning text, health threshold |
| WASAPI loopback (`build_loopback_channel`) | all (branch is a runtime `cfg!(windows)`) | input stream on the default output device | none |
| WASAPI 400 ms buffer + timestamp gap detection + MMCSS (`build_recorder`, `TIMESTAMP_GAPS`, `sink::boost_capture_thread`) | Windows only (`cfg`) | other OSes keep the default buffer and count only ring overflows | contention bench (manual) |
| `sink.rs` ring + writer | all | every channel on every OS | 4 unit tests (order, overflow, tail top-up, readable mid-recording) |
| `win_volume.rs` + mute/0% warnings (`cpal_backend.rs:75-90`) | Windows | COM call on every `recording_status` poll | none |
| `pulse_loopback.rs` | Linux | needs a Pulse or PipeWire server; DECISIONS.md M13 says "not yet run on a Linux device" | none |
| `coreaudio_tap.rs`, `CaptureHealth.tap` | macOS | needs 14.2+ (else `LoopbackUnsupported` → mic-only); a signed build plus the TCC grant, else zeros | none |
| `preflight_system_audio` | all | real probe on macOS, the constant `Unsupported` elsewhere | serde shape only |
| `mix`, `vad`, `tap_messages`, `testtone`, `lib.rs` serde | all | pure (the tone player needs an output device) | 35 tests in the crate, run on all 3 CI legs |

No test in this crate is cfg-gated. The per-OS backends get only compile and clippy coverage, each on its own CI leg.

### Tauri commands
None (crates expose no commands).

### Events
None.

### Settings keys
None. The caller passes the mic device in `CaptureConfig.mic_device_id`.

### Database
None.

### Files and directories written
- `CaptureConfig.out_dir` (created with `create_dir_all`, `cpal_backend.rs:95`), all files named after `base_name`:
  - `<base>.mic.wav` — mono 16-bit at the input device's native rate. Written by the writer thread, header refreshed every second (a crash leaves a playable file).
  - `<base>.system.wav` — mono 16-bit at the WASAPI output's native rate, 48 kHz on Linux, the tap's rate on macOS. Only when loopback started.
  - `<base>.mixed.wav` — 16 kHz mono mix of both channels, for ASR (`cpal_backend.rs:438-439`).
  - `<base>.playback.wav` — native-rate mix, only when both channels exist. For a mic-only capture, `playback_path` points at the mic WAV (`cpal_backend.rs:443-455`).
- macOS probe: `<OS temp>/fotw-tap-preflight-<pid>.wav`, deleted after the probe (`coreaudio_tap.rs:180,197`).

### Network egress
| Host | When | Gate |
|---|---|---|
| None | — | — |

### External processes
None. On macOS the tap creates a private Core Audio aggregate device (`com.flyonthewall.tap-aggregate-<pid>`) and destroys it at shutdown (`coreaudio_tap.rs:315,519-528`).

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `src-tauri` (`state.rs` chooses `CpalAudioCapture`; `recording.rs`, `live.rs`, `pipeline.rs`, `import_commands.rs`, `gpu.rs`), and fly-asr (`whisper_cpp.rs`, `groq.rs`, `checkpoint.rs` for `mix` and `vad`).
- **Outbound:** `cpal`, `hound`, `serde`, `thiserror`, `tracing`, plus the per-OS deps above. `fly-core` is declared (`Cargo.toml:8`) but unused.

## Invariants
- **The UI and domain see only the trait.** The impl is chosen once, in `src-tauri` (ARCHITECTURE.md "The golden rule"; `lib.rs:1-6`; `state.rs:20-21,85`).
- **Channels stay aligned with the wall clock.** Loopback is padded with zeros against a shared pause-aware clock when it falls > 200 ms behind (`idle_pad_frames`, shared by the WASAPI, Pulse and tap paths), and the tail is topped up at stop by the writer (`ChannelSink::finalize`, target set in `audio_thread`) (DECISIONS.md M2 "loopback silence-padding"). The mic's first packet is aligned the same way, so both channels start on the clock.
- **The capture callback never waits.** No file I/O, locks or allocation in a callback (the recording `Clock` is atomics only): samples go into a bounded `rtrb` ring (30 s per channel) and one `fly-audio-writer` thread writes the WAVs (`sink.rs`). The tap's IOProc takes an uncontended `try_lock` (only it locks).
- **Lost audio is never silent.** Every WASAPI capture-timestamp jump is replaced by the same number of zero frames (timeline intact), and so is every ring overflow. It counts as `lost` on the mic always. On loopback it counts as idle padding only when the callback waited as long as the jump, the packet before it was silent (≤ −60 dBFS) and our buffer wasn't nearly full in the second before: meeting apps render continuous silence during a call, so a gap right after audible output is lost audio, and a gap after silence lost nothing audible. `max_backlog_ms` is the most audio that ever waited in the OS capture buffer (from WASAPI's device-position QPC time minus the packet's capture time); our buffer can only overflow once that nears its size. `upstream_lost_events` are losses with no near-full buffer (≥ 75%) in the second before them: the OS audio engine or driver dropped the audio, which only reporting can address. Totals ≥ `LOST_AUDIO_WARN_MS` become a live `warnings()` sentence; `stop` returns the `CaptureHealthReport`. Timestamp gaps are trusted on Windows only (measured exact there); a single gap can never push a channel more than 0.5 s ahead of the clock.
- **Capture threads are real-time on Windows** (MMCSS Pro Audio on the first callback; `realtime_priority` in the report) and WASAPI gets a 400 ms buffer, falling back to the driver default if refused.
- **Paused audio never reaches a file.** cpal streams are paused (`cpal_backend.rs:394-399`); Pulse and the tap drop frames while the clock is stopped (`pulse_loopback.rs:92-94`; `coreaudio_tap.rs:394-396`).
- **Graceful degrade.** If loopback can't start, recording continues mic-only with a visible warning (DECISIONS.md M2; `cpal_backend.rs:350-368`).
- **`mixed.wav` is 16 kHz mono** because ASR depends on it (`mix.rs:9,117-135`; `cpal_backend.rs:437`). The playback mix uses the higher native rate (`mix.rs:140-160`).
- **`SystemAudioPreflight` tags are a UI contract** (`lib.rs:99`, test `lib.rs:146-171`; mirrored in `frontend/src/types.ts:114-119`).
- **Warning wording is a UI contract.** `frontend/src/recordingBar.ts` classifies warnings by the substrings "recording only silence" / `PRIVACY_PANE`, "was lost" (+ "microphone" for the You side), "couldn't start", "volume is at 0%" and "muted". The macOS text and the lost-audio text (`lost_audio_warning_matches_the_recording_bar_classifier`) have Rust tests.
- **The macOS tap is never hard-linked.** The deployment target is 12.0, and the tap symbols are resolved at runtime (`coreaudio_tap.rs:22-24,72-97`).
- **A silent tap is reported while recording, not afterwards.** After ≥ 5 s of all-zero samples while the output device is rendering, the session raises a warning (`coreaudio_tap.rs:148-169`; `cpal_backend.rs:152-160,202-206`).
- **The VAD threshold adapts per recording**: noise floor (10th percentile) + 8 dB, clamped to −55…−25 dBFS (`vad.rs:1-8,69-72`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-audio/src/lib.rs` | 1 | `SystemAudioPreflight` JSON tags |
| unit | `crates/fly-audio/src/mix.rs` | 8 | resample, normalize, mix, mixdown, playback mix |
| unit | `crates/fly-audio/src/vad.rs` | 8 | adaptive threshold, padding/merge, click rejection, stitch/map |
| unit | `crates/fly-audio/src/tap_messages.rs` | 2 | `tccutil` command, stale-grant banner text |
| unit | `crates/fly-audio/src/sink.rs` | 4 | audio + gaps reach the file in order, overflow counted with the timeline intact, tail top-up, file readable (header current) mid-recording |
| unit | `crates/fly-audio/src/cpal_backend.rs` | 3 | lost-audio sentence vs the classifier, idle-pad slack, warning threshold + report |
| unit | `crates/fly-audio/src/testtone.rs` | 10 | clean tone, padded / removed / inserted gaps, gaps across markers, capture gain and loudness-equalization ramps |
| manual (local) | `src-tauri/tests/contention_bench.rs` | 1 (+2 helpers) | real capture under load: tone gaps, mic drift, Ask p95, caption lag, induced gap (see app-recording) |
| integration (CI) | `src-tauri/tests/scheduling.rs` | 1 relevant | a fake `CaptureSession` in `AppState.recording` defers the queue (trait shape only) |
| manual | `crates/fly-audio/examples/capture_smoke.rs`; `docs/TESTING.md` (M2, M11) | — | real capture and mute warning, read by a person |

**Known gaps:**
- **No automated test of real capture in CI.** The contention bench drives the real WASAPI path, but only locally on Windows. The macOS tap and Pulse paths moved to the ring + writer with compile and clippy coverage only; neither has run on hardware with it. Their callbacks still don't detect device-level gaps (only ring overflows).
- `NullAudioCapture` exists "for tests" (`null.rs:1-2`) but nothing uses it, and `AppState::init_with` hard-codes `CpalAudioCapture` (`state.rs:85`). So no app test can drive start/stop with a fake backend.
- The Linux Pulse path has never been reported working on hardware (DECISIONS.md M13). The macOS tap depends on signing and TCC, which CI can't exercise.
- VAD tests use pure tones and digital silence; there are no noisy-speech, music or near-threshold cases. The "long silence → Whisper loop" link is checked only by the ignored accuracy harness.
- `wav_roundtrip_and_mixdown` would still pass if the system track were dropped from the ASR mixdown. The mic alone sets both the duration and the energy it checks.
- The mute / 0% / loopback-failed sentences aren't pinned by any Rust test, while the UI classifies them by substring and `devMock.ts` copies them word for word.

## Changing it safely
- `cargo test -p fly-audio` runs on every OS but proves nothing about capture. For a backend change, the matching CI leg's `cargo clippy --workspace --all-targets` is the only automated check. Then run `cargo run -p fly-audio --example capture_smoke -- <out_dir>` on real hardware, the `docs/TESTING.md` M2 and M11 items, and on Windows the contention bench (`cargo test -p fly-app --release --test contention_bench -- --ignored --nocapture`, target: 0 gaps > 10 ms, mic drift < 0.1%).
- **Callbacks:** keep them wait-free. Anything that can block (file I/O, `Mutex::lock`, allocation, logging) goes to the writer thread or is counted in `ChannelStats` and logged at stop.
- **Lost-audio sentence:** `lost_audio_warning` is pinned by a Rust test against the classifier keys; change it with `recordingBar.ts`, its tests and devMock's `MOCK_REC_WARNINGS.lost`.
- **Warning text:** change `cpal_backend.rs` / `tap_messages.rs`, `frontend/src/recordingBar.ts` (classifier) and `devMock.ts` (`MOCK_REC_WARNINGS`) together.
- **File names:** the `.mic.wav` / `.system.wav` suffixes plus `base_name: "recording"` are hard-coded in `live.rs:190,196`. fly-storage itself stores whatever paths `RecordingRef` carries, but its tests and fixtures assume `recording.*.wav`. Keep them in step.
- **`SystemAudioPreflight`:** update the serde test, `frontend/src/types.ts`, the verdict check in `App.tsx`, and the `preflight_system_audio` case in `devMock.ts`.
- **`mixed.wav` format** feeds every ASR engine. Don't change its rate or channel count without re-running the ignored `pipeline_e2e` / accuracy harness.
- **macOS:** a tap from an unsigned or re-signed build records zeros. Test with a signed build and remember stale TCC grants (`tap_messages.rs:1-8`).
