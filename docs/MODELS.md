# Models: ASR & diarization

Fly on the Wall downloads models on first use into the data dir (`models/`), with progress and checksum
verification. **Weights are never committed to git and never bundled in the installer.**

## Hardware-adaptive ASR tiers (auto-picked on first run, user-overridable)

| Tier | Trigger | Default model | Footprint / notes |
|---|---|---|---|
| **Light** | ≤8 GB RAM, integrated GPU, or older CPU | Whisper `small` (Q5) | ~2 GB RAM, ~3.4% WER, ~3× realtime on CPU |
| **Balanced** | ~16 GB RAM, weak/no discrete GPU | Whisper `medium` or `large-v3-turbo` if acceptable | medium ~5 GB / ~2.9% WER |
| **Best** | NVIDIA ≥8 GB VRAM, Apple Silicon, or strong CPU + ≥16 GB RAM | **`large-v3-turbo`** | near-large accuracy, ~6× faster than large-v3; full `large-v3` as "maximum quality" toggle |
| **Cloud** | device can't transcribe acceptably | **Groq** (Whisper large-v3/turbo) | needs network + Groq key; UI shows a privacy notice — audio leaves the device |

Rationale: medium→large is only ~0.4 pp WER on clean audio, but large is more robust on messy
meeting audio; **large-v3-turbo is the sweet spot for capable machines**. Prefer Q5_0/Q8_0
quantization — negligible accuracy loss, big RAM/disk savings, especially on Light.

## Engines

| Engine | Role | Runs on | License |
|---|---|---|---|
| **whisper.cpp** | primary ASR | CPU, CUDA, Metal, Vulkan; desktop + mobile | MIT (weights: OpenAI Whisper, MIT) — 99 languages |
| **NVIDIA Parakeet** | optional ASR | NVIDIA GPUs; Apple ANE via FluidAudio (macOS port) | weights CC-BY-4.0 — En + 25 EU languages; near-zero silence hallucination |
| **Groq** | cloud ASR **fallback only** | network | free tier ~2k req/day, ~7,200 audio-s/hour; word+segment timestamps. Preprocessing matches the local path: VAD strips non-speech before upload (anti-hallucination, smaller payloads), peak normalization, greedy decode (temperature 0); word timestamps are mapped back to the original timeline so local diarization stays aligned |
| **sherpa-onnx** | diarization, **always local** | CPU everywhere incl. phones | Apache-2.0 |

## Diarization models (always downloaded, all tiers)

- `pyannote-segmentation-3.0` (ONNX) — ~6 MB — speaker segmentation (license: MIT, gated
  upstream on HF; Fly on the Wall fetches the ONNX conversion published for sherpa-onnx)
- Speaker embedding: 3D-Speaker CAM++ (or WeSpeaker) ONNX — ~26 MB — Apache-2.0

Even on the Cloud tier, diarization runs locally and Groq's word timestamps are merged with the
local speaker turns (spec §6.3): "who said what" never depends on the network.

## Sidecar binaries (also downloaded on first use, checksum-pinned)

| Artifact | Version | Size | Purpose |
|---|---|---|---|
| whisper.cpp CLI (`whisper-cli.exe`, Windows) | v1.9.1 (CPU build) | ~8 MB zip | ASR; the same zip ships `parakeet-cli.exe` for a future Parakeet engine |
| whisper.cpp CLI (Vulkan GPU, Windows) | v1.9.1 (`GGML_VULKAN=1` build) | ~24 MB zip | optional GPU ASR — one cross-vendor build (NVIDIA/AMD/Intel); see below |
| whisper.cpp CLI (macOS universal) | v1.9.1 (static, Metal embedded) | ~2.4 MB tar.bz2 | ASR — one archive for Intel + Apple Silicon; see "Building the macOS engine" |
| sherpa-onnx diarization CLI | v1.13.3 | ~19 MB | speaker diarization |
| ffmpeg | n8.1 (BtbN autobuild, dated tag) | ~79 MB zip | screen capture (gdigrab) + media import conversion |

### Building the macOS engine

Upstream whisper.cpp ships **no** macOS or Linux binary, so those platforms
historically depended on a `whisper-cli` on `PATH` (e.g. `brew install
whisper-cpp`) — which most users don't have, producing a dead-end transcribe
error. To close that on macOS, the maintainer builds the same pinned commit
(v1.9.1, `f049fff`) and hosts it as a `tools-whisper-v1.9.1` release on THIS
repo — the same pattern as the Windows Vulkan build — so `ensure_tool`
auto-downloads it on first transcribe exactly like Windows.

Maintainer flow (all hosted from this repo, never a fork):

1. Run the **Build whisper sidecar (macOS)** workflow
   (`.github/workflows/build-whisper-sidecar.yml`, `workflow_dispatch`) with
   `create_release` on. It builds via
   [`scripts/build-whisper-sidecar.sh`](../scripts/build-whisper-sidecar.sh) —
   a **static** universal (x86_64 + arm64, hard-asserted via `lipo -archs`)
   binary with Metal embedded, minimum macOS pinned to the app's
   `minimumSystemVersion` — attaches the tarball to the tools release, and
   prints the `Artifact` pin (url / sha256 / bytes) in the job summary.
2. Paste the pin over the placeholder `whisper-bin` entry in the macOS `TOOLS`
   array in `src-tauri/src/models.rs` (id `whisper-bin`, `probe_rel`
   `bin/whisper/whisper-cli`).

The script also runs locally (`scripts/build-whisper-sidecar.sh macos`, needs
`git` + `cmake`) to reproduce and inspect the build: it verifies HEAD equals
the pinned commit, checks the binary links only system libraries, packages it
flat (archive root holds just `whisper-cli`), and prints the same pin.
(Rebuilds are not bit-identical — timestamps land in the tar header —
integrity rests on the SHA-256 pin of the one hosted artifact.)

Status: **hosted and pinned** (2026-07-16) — built by workflow run
29546886480, attached to `tools-whisper-v1.9.1`, and pinned in `models.rs`
after independently re-hashing the hosted asset and verifying both fat-binary
slices (x86_64 + arm64). Awaiting the Apple Silicon and Intel-Mac smoke tests
in `docs/pr-triage/pr-26-rehost-checklist.md` before this branch merges — the
arm64 slice has never been executed on real hardware. **Linux** stays
PATH-resolved (`whisper-cli` on PATH); the script's `linux` target exists for
whoever adds that platform later. Where no engine is resolvable, the app
shows an actionable "engine not installed" prompt (Install / Settings)
rather than a raw error.

## GPU transcription (post-meeting only)

The CPU whisper build stays the shipped, validated default everywhere. With
`asr.use_gpu` on (the default), the first transcription on a machine runs a
one-time GPU-vs-CPU speed test on ~60 s of real speech cut from that
recording; the verdict persists per (machine, model) and the GPU is used only
when it measured faster (`src-tauri/src/gpu.rs`). Any GPU failure — benchmark,
launch, or mid-run — falls back to CPU visibly and re-pins the machine to CPU
(toggling the Settings switch off→on re-tests).

Per-OS strategy: **Windows** downloads the pinned Vulkan build above (upstream
whisper.cpp publishes no Vulkan Windows binary — only CPU/BLAS/CUDA-only — so
this one is built from the upstream v1.9.1 tag with `-DGGML_VULKAN=1` and
hosted as a tools release on this repo). **macOS** whisper.cpp builds default
to Metal already — the managed universal archive above is built with Metal
embedded, and a brew/PATH build works the same. Metal runs as a guarded
primary with a forced-CPU (`-ng`) fallback, because on GPUs that Metal can't
serve (e.g. Intel-era Macs) ggml's Metal init aborts — that failure falls
back to CPU mid-run and pins the machine to CPU like Windows does. The live
transcript loop always stays on CPU (`-ng` on macOS, the CPU build on
Windows): it runs during capture, exactly when the GPU is busy with the call.

## Model registry

Exact download URLs, SHA-256 checksums (pinned from upstream release digests / HF LFS metadata
and re-verified locally), and sizes live in `src-tauri/src/models.rs`. Everything downloads
into `<data dir>/models` and `<data dir>/bin` with streaming progress; nothing is bundled in
git or the installer.

## Download sources and mirror fallback

Each Hugging Face-hosted artifact is fetched from huggingface.co first (two
attempts — HF's Xet CDN bridge intermittently rejects downloads). Only if
both fail is **hf-mirror.com** tried: a community-run Hugging Face proxy
operated from China that serves the same repository paths. It is not
affiliated with this project or with Hugging Face. Integrity does not depend
on the source: every artifact is accepted only if its SHA-256 matches the
pin in `models.rs`, so the worst a wrong or compromised mirror can do is
fail the checksum (a checksum mismatch also skips that source immediately
instead of re-downloading from it). To never contact the mirror, set the
environment variable `FLYONTHEWALL_NO_HF_MIRROR=1`. GitHub-hosted tool
binaries are single-source.
