# Models: ASR & diarization

Looma downloads models on first use into the data dir (`models/`), with progress and checksum
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
| **Groq** | cloud ASR **fallback only** | network | free tier ~2k req/day, ~7,200 audio-s/hour; word+segment timestamps |
| **sherpa-onnx** | diarization, **always local** | CPU everywhere incl. phones | Apache-2.0 |

## Diarization models (always downloaded, all tiers)

- `pyannote-segmentation-3.0` (ONNX) — ~6 MB — speaker segmentation (license: MIT, gated
  upstream on HF; Looma fetches the ONNX conversion published for sherpa-onnx)
- Speaker embedding: 3D-Speaker CAM++ (or WeSpeaker) ONNX — ~26 MB — Apache-2.0

Even on the Cloud tier, diarization runs locally and Groq's word timestamps are merged with the
local speaker turns (spec §6.3): "who said what" never depends on the network.

## Model registry

Exact download URLs, SHA-256 checksums, and sizes live in code
(`crates/looma-asr` / `crates/looma-diarize`, landing M3) and are documented here as they ship.
