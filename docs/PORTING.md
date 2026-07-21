# Porting guide (macOS, iOS, Android)

Everything OS-specific sits behind traits (`AudioCapture`, `TranscriptionEngine`,
`DiarizationEngine`, `ScreenRecorder`, `SecretStore`, `CalendarProvider`), and impl selection
happens only in `src-tauri`. A port = one impl crate per capability + platform config. Core,
storage, and UI are untouched.

## macOS

> **Status (PR #37):** system-audio capture via Core Audio process taps is IMPLEMENTED
> (`fly-audio/src/coreaudio_tap.rs`, macOS 14.2+ gated at runtime; < 14.2 keeps the mic-only
> + banner behavior). The whisper engine and sherpa-onnx diarizer are managed downloads —
> nothing needs brew. Screen capture is full-screen via ffmpeg avfoundation (ffmpeg still
> from PATH). Remaining trap, verified live on a 14.3 Apple Silicon machine: an
> **unsigned/un-entitled build's tap runs perfectly and delivers only zeros** — the IOProc
> fires, the file is written and padded, no error anywhere. The app now detects this during
> the recording (tap timeline ≥5 s, zero non-silent samples, output device
> `IsRunningSomewhere`) and warns in the recording bar. For real capture the release build
> needs: (1) `NSAudioCaptureUsageDescription` in Info.plist (present), (2) a **code-signed
> app bundle** (Developer ID + notarization for distribution) so TCC can attribute consent,
> and (3) the user's one-time consent — the system prompt on first tap, or System Settings →
> Privacy & Security → Screen & System Audio Recording. No special entitlement is involved;
> it is signature + TCC consent. Release-build (signed) verification is still pending.

- **AudioCapture → Core Audio Process Taps** (macOS 14.2+, `CATapDescription`) — the clean
  audio-only path; preferred over ScreenCaptureKit for system audio. Traps encoded in the
  impl (coreaudio_tap.rs):
  - `AVAudioEngine` **cannot** be retargeted to a tap-backed aggregate device — use
    `AudioDeviceCreateIOProcIDWithBlock` directly.
  - The aggregate device needs a **real output device as its main sub-device**, the tap as a
    sub-tap, and `kAudioAggregateDeviceTapAutoStartKey: true`.
  - Requires `NSAudioCaptureUsageDescription` **and a signed binary**, or the IO callbacks
    silently return all-zero samples (detected at runtime, see above).
  - `AudioHardwareCreateProcessTap`/`DestroyProcessTap` are 14.2+ symbols: resolve them with
    `dlsym`, or the binary won't launch on the macOS 12.0 deployment floor.
  - sherpa-onnx ≥ v1.13 macOS builds bundle an onnxruntime whose CoreML EP hard-references
    `MLComputePlan` (14.4+): dyld kills the diarizer on macOS 12–14.3. The macOS pin is the
    v1.12.34 `onnxruntime-1.17.1` variant (minos 11.0) for that reason.
- **ScreenRecorder → ScreenCaptureKit.**
- **ASR/diarization:** whisper.cpp runs on Metal; sherpa-onnx on CPU. On Apple Silicon consider
  **FluidAudio** (Parakeet on the Neural Engine; also diarizes) behind the existing traits.
- **SecretStore:** keyring's `apple-native` feature already covers Keychain.

## iOS

- System-audio capture is **sandbox-forbidden** (only a Broadcast Upload Extension with a 50 MB
  cap). iOS Fly on the Wall is therefore a **mic-only / in-person** notetaker — the honest alternative;
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
  `fly-audio/src/pulse_loopback.rs` with the same pad-to-clock discipline as WASAPI.
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
