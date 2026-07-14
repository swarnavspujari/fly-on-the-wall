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
| whisper.cpp CLI (`whisper-cli.exe`) | v1.9.1 (CPU build) | ~8 MB zip | ASR; the same zip ships `parakeet-cli.exe` for a future Parakeet engine |
| whisper.cpp CLI (Vulkan GPU, Windows) | v1.9.1 (`GGML_VULKAN=1` build) | ~24 MB zip | optional GPU ASR — one cross-vendor build (NVIDIA/AMD/Intel); see below |
| sherpa-onnx diarization CLI | v1.13.3 | ~19 MB | speaker diarization |
| ffmpeg | n8.1 (BtbN autobuild, dated tag) | ~79 MB zip | screen capture (gdigrab) + media import conversion |

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
hosted as a tools release on this repo). **macOS** whisper.cpp builds
(including brew's, found via PATH) default to Metal already, so the setting
only gates a `-ng` force-CPU flag there. The live transcript loop always stays
on CPU: it runs during capture, exactly when the GPU is busy with the call.

## Model registry

Exact download URLs, SHA-256 checksums (pinned from upstream release digests / HF LFS metadata
and re-verified locally), and sizes live in `src-tauri/src/models.rs`. Everything downloads
into `<data dir>/models` and `<data dir>/bin` with streaming progress; nothing is bundled in
git or the installer.
