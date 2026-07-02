# Porting guide (macOS, iOS, Android)

Everything OS-specific sits behind traits (`AudioCapture`, `TranscriptionEngine`,
`DiarizationEngine`, `ScreenRecorder`, `SecretStore`, `CalendarProvider`), and impl selection
happens only in `src-tauri`. A port = one impl crate per capability + platform config. Core,
storage, and UI are untouched.

## macOS

> **Status (v0.2):** Looma builds and runs on macOS (CI-verified; not yet exercised on a
> physical Mac). Capture is **mic-only**: process taps need a signed binary (see the third
> trap below) and v0.2 ships unsigned, so a tap impl would record silence for every user.
> Diarization downloads the universal2 sherpa-onnx build; whisper-cli and ffmpeg are picked
> up from PATH (`brew install whisper-cpp ffmpeg`). Screen capture is full-screen via
> ffmpeg avfoundation. Once releases are signed, implement the taps below and move screen
> capture to ScreenCaptureKit.

- **AudioCapture → Core Audio Process Taps** (macOS 14.2+, `CATapDescription`) — the clean
  audio-only path; preferred over ScreenCaptureKit for system audio. Known traps to encode in
  the impl:
  - `AVAudioEngine` **cannot** be retargeted to a tap-backed aggregate device — use
    `AudioDeviceCreateIOProcIDWithBlock` directly.
  - The aggregate device needs a **real output device as its main sub-device**, the tap as a
    sub-tap, and `kAudioAggregateDeviceTapAutoStartKey: true`.
  - Requires `NSAudioCaptureUsageDescription` **and a signed binary**, or the IO callbacks
    silently return all-zero samples.
- **ScreenRecorder → ScreenCaptureKit.**
- **ASR/diarization:** whisper.cpp runs on Metal; sherpa-onnx on CPU. On Apple Silicon consider
  **FluidAudio** (Parakeet on the Neural Engine; also diarizes) behind the existing traits.
- **SecretStore:** keyring's `apple-native` feature already covers Keychain.

## iOS

- System-audio capture is **sandbox-forbidden** (only a Broadcast Upload Extension with a 50 MB
  cap). iOS Looma is therefore a **mic-only / in-person** notetaker — the honest alternative;
  the UI must not pretend otherwise.
- whisper.cpp and sherpa-onnx both run on iOS; core + storage are reused as-is via Tauri mobile.

## Android

- Mic capture behind `AudioCapture`; whisper.cpp and sherpa-onnx both run on Android.
- Tauri 2 supports Android targets; `src-tauri` is already `staticlib`/`cdylib`-ready.

## Linux

> **Status (v0.2):** builds in CI (ubuntu-22.04); not yet run on a Linux device.

- **AudioCapture:** mic via cpal/ALSA; system audio via the default sink's **monitor
  source** over the PulseAudio simple API (`@DEFAULT_MONITOR@`) — served natively by
  PulseAudio and by PipeWire's pipewire-pulse. Implemented in
  `looma-audio/src/pulse_loopback.rs` with the same pad-to-clock discipline as WASAPI.
- **ScreenRecorder:** ffmpeg x11grab (full screen + region). Wayland sessions need an
  xdg-desktop-portal/PipeWire recorder — not implemented; x11grab's failure is surfaced.
- **Tools:** sherpa-onnx and ffmpeg download managed per-OS builds; whisper.cpp publishes
  no Linux CLI binaries, so `whisper-cli` is resolved from PATH (package manager or source
  build) with the Groq fallback as the no-install alternative.
- **Secrets:** keyring's `sync-secret-service` feature (GNOME Keyring / KWallet over DBus).

## Rules that keep ports cheap

1. No `#[cfg(target_os)]` outside platform impl modules inside the capability crates.
2. New capability? Define the trait first, stub the other platforms with honest errors
   (`LoopbackUnsupported`, `Unavailable`), then implement Windows.
3. Sidecar binaries are per-platform artifacts resolved at runtime — never hardcode paths.
