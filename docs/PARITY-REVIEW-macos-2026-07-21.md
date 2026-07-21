# macOS ⇄ Windows parity & GPU review — Apple Silicon, 2026-07-21

Scope: full review of the PR #37 branch (`717c041..6b72bd2`) against the Windows
reference implementation, plus a hardware audit of whether Apple Silicon's GPU is
actually used for whisper transcription and the local LLM. Reviewed and measured on
an **Apple M3 Pro, macOS 14.3**, with the app's own tauri-free layers driving the
real pipeline (the same entry points the golden E2E test uses).

## 1. Engine parity — how each capability runs, per OS

| Capability | Windows (reference) | macOS after PR #37 | Verdict |
|---|---|---|---|
| Post-meeting ASR engine | Managed CPU whisper.cpp v1.9.1 zip; benchmark-gated Vulkan build for GPU | Managed **universal** whisper.cpp v1.9.1 (repo-hosted), Metal embedded; Apple Silicon runs Metal, Intel pinned to CPU | ✅ parity (mechanism differs by design, see §2) |
| ASR GPU selection | One-time GPU-vs-CPU benchmark per machine+model, persisted verdict, runtime failover → CPU pin | Runtime `hw.optional.arm64` gate + guarded Metal primary with forced-CPU fallback + same runtime-failure pin; same off→on retest gesture | ✅ same setting/pin semantics |
| Diarization | Managed sherpa-onnx v1.13.3 (CPU) | Managed sherpa-onnx **v1.12.34/ort-1.17.1** (CPU) — v1.13.x dyld-aborts below macOS 14.4 (`MLComputePlan`); verified diarizing on 14.3 | ✅ parity (older pin, verified; see §4 rec 2) |
| System-audio capture | WASAPI loopback (input stream on output device) | Core Audio process tap (14.2+), same two-channel WAV discipline, pad-to-clock, mic-only + banner degradation; **plus** live silent-tap detection Windows doesn't need | ✅ parity 14.2+; <14.2 mic-only+banner |
| Live captions | CPU whisper, small model | Same, `-ng` forced (GPU contention during calls is the one regression the app can't afford) | ✅ parity by design |
| Local LLM (Ollama) | Managed opt-in artifact (v0.31.2) + app-owned `ollama serve` | **Was PATH-only — fixed in this PR**: managed `ollama-darwin.tgz` v0.31.2 pin (same version), same opt-in Install button (`can_install` keys off the registry). Verified: managed download→verify→extract→run, and `ollama serve` reports `library=Metal … Apple M3 Pro (27 GiB)` — pulled models run on the GPU | ✅ parity after this PR |
| Cloud fallback (Groq) | Guarded engine w/ local rescue | Identical, platform-neutral; macOS rescue additionally forces CPU (reliability over speed on the last engine in the chain) | ✅ parity |
| Scheduler / notices / markers | — | Items 3–4 are platform-neutral | ✅ parity |
| Screen capture | ffmpeg (managed) + window/region | ffmpeg **from PATH** (no managed macOS ffmpeg), full-screen avfoundation only | ⚠️ gap (pre-existing; §4 rec 3) |
| Media import / video thumbnails | Managed ffmpeg | PATH ffmpeg only | ⚠️ gap (same root cause) |
| Auto-update | tauri-updater, Windows artifacts | Updater plugin loads but release pipeline publishes no signed macOS artifacts yet | ⚠️ release-ops gap (§4 rec 4) |

## 2. Is Apple Silicon's GPU actually used? (measured)

**Whisper (post-meeting ASR): yes — Metal, with flash attention, by default.**
- The managed universal binary initializes Metal on this machine:
  `ggml_metal_device_init: GPU name: MTL0 (Apple M3 Pro)`, `MTLGPUFamilyApple9`,
  embedded Metal library (no external `.metallib` to lose next to the binary).
- Flash attention is **on by default** in the pinned v1.9.1 CLI (`-fa [true]`) — no
  extra flags needed for the fast path.
- The pipeline labels the engine `whisper.cpp-metal` and the Intel pin does NOT
  catch Apple Silicon (verified across default + small models, GPU off/on cycling).
- Real-workload measurement, 61-minute mono 16 kHz meeting recording, default model
  (large-v3-turbo q5): see the PR conversation for the timing lines from this
  machine's run; speech-time progress 0→100 % with no hallucination-loop collapses
  (`collapse_loops` reported none).

**Why there is deliberately no macOS GPU *benchmark* (Windows has one):** the
Windows Vulkan build is a separately shipped exe racing a separately shipped CPU
exe — the benchmark picks per machine across GPU vendors. On macOS there is one
universal binary and one GPU vendor; on Apple Silicon Metal is uniformly faster
than CPU for whisper, and on Intel the GPU path is *unsafe* (silent corruption,
PR #37 smoke report), not merely slower. An architecture gate is therefore the
correct macOS analogue of the Windows benchmark, not a missing feature.

**Local LLM: yes, once Ollama is present — and this PR makes it one click.**
Ollama's darwin build ships the Metal/MLX runners; on first serve it logs
`inference compute … library=Metal name=MTL0 description="Apple M3 Pro"
total="27.0 GiB"`. Model execution (Enhance / Ask / polish) hits the GPU with no
configuration.

**Live captions: CPU on purpose** — they run *during* the call, exactly when the
GPU serves the video conference; contention there is the one regression the app
can't afford (same reasoning as Windows, which also decodes live on CPU).

**Diarization: CPU on both platforms** (sherpa-onnx). Future headroom: FluidAudio /
ANE per docs/PORTING.md — out of scope.

## 3. Code-review findings on the PR #37 branch

Reviewed every commit (`395594d..6b72bd2`). No correctness blockers found. Notes:

1. **live.rs status snapshot** is a single slot keyed by meeting id — a stale
   status from a previous recording is filtered out by the id check in
   `live_status`; benign.
2. **Tap writer teardown order** is safe: `clock.pause()` precedes `pad_tail_to`,
   and the IO block drops buffers while paused and after the writer is taken, so
   no post-pad appends can race the finalize.
3. **Non-interleaved tap buffers**: handled (per-buffer average); interleaved
   detection via `mBytesPerFrame >= 4×channels` matches CoreAudio's ASBD
   conventions for both layouts.
4. **`ENGINE_MISSING_MARKER` ("is not installed")** is a broad substring; today
   only `ensure_tool` produces it in scheduler-visible errors. Acceptable; worth
   remembering if new error strings are added.
5. **Settings GPU toggle on Intel** persists the stored `use_gpu` value while
   rendering disabled — harmless because the backend gates on the runtime
   architecture check regardless of the stored setting.
6. **`supports_system_loopback()`** dlsyms per call — negligible cost, correct
   under macOS upgrades mid-session (it can only flip false→true after an OS
   update + relaunch anyway).

## 4. Recommendations (ranked)

1. **Ship the managed macOS Ollama artifact** — done in this PR (verified on
   hardware; §1).
2. **Watch sherpa-onnx upstream** for macOS builds with a lower deployment
   target (or built against ORT with weak-linked CoreML): the v1.12.34/ort-1.17.1
   pin is functionally verified but trails the Windows/Linux v1.13.3 pins. When
   upstream fixes their SDK floor, re-align all three.
3. **ffmpeg on macOS**: no official universal2 binary with stable digests exists
   to pin (BtbN builds are Windows/Linux only; evermeet is x86_64-only). The
   honest options are building it in this repo's CI like the whisper sidecar, or
   keeping PATH-only + clear UI guidance. Recommend a follow-up issue; not done
   here (large, out of scope).
4. **macOS release pipeline**: signed + notarized `.app`/`.dmg` with updater
   artifacts is the remaining step for the tap to capture real audio in the wild
   (TCC consent needs a stable signed identity), and for auto-update parity.
   Requirements are documented in docs/PORTING.md.
5. **Optional**: a macOS analogue of the Windows volume warning (`win_volume`)
   is unnecessary — process taps capture pre-mixer process output, so system
   volume/mute doesn't affect the recording (unlike WASAPI loopback).

## 5. What was verified on this machine vs what still needs a human/signed build

Verified headlessly here (Apple M3 Pro, macOS 14.3): fresh-dir managed downloads
(whisper universal 2-arch, sherpa, Ollama, models), Metal engagement + correct
transcripts (default & small models, GPU off/on), 61-minute real-recording pipeline
run, diarization on the repinned sherpa, scheduler fail-once semantics, offline
live-caption resolution ≤8 s, tap structure + all-silence detection timing.

Still requiring a signed build and/or a person at the machine: TCC-consented real
system-audio capture (unsigned builds record silence **by OS design**), the in-app
UI walkthrough (live pane, notices, meetings picker), and the Intel re-verification
pass from the PR #37 merge gate.
