# Porting guide (macOS, iOS, Android)

Everything OS-specific sits behind traits (`AudioCapture`, `TranscriptionEngine`,
`DiarizationEngine`, `ScreenRecorder`, `SecretStore`, `CalendarProvider`), and impl selection
happens only in `src-tauri`. A port = one impl crate per capability + platform config. Core,
storage, and UI are untouched.

## macOS

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

## Rules that keep ports cheap

1. No `#[cfg(target_os)]` outside platform impl modules inside the capability crates.
2. New capability? Define the trait first, stub the other platforms with honest errors
   (`LoopbackUnsupported`, `Unavailable`), then implement Windows.
3. Sidecar binaries are per-platform artifacts resolved at runtime — never hardcode paths.
