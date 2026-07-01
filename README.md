# Looma

**Local-first meeting notes. Your machine, your models, your keys.**

Looma records your meetings (your mic and the other participants' system audio as separate
channels), transcribes and diarizes them **entirely on your machine**, and merges your rough
scratchpad notes with the transcript into clean, structured markdown — with visible provenance
for every AI-written line and click-through to the exact transcript segment it came from.

In the spirit of Granola, but private by default:

- **Capture, transcription, diarization, notes, and search work fully offline.** Nothing leaves
  the machine unless you explicitly call an LLM provider or opt into the Groq cloud-ASR fallback.
- **Bring your own models & keys** — whisper.cpp locally, or NVIDIA NIM / OpenAI / Anthropic /
  local Ollama for note enhancement.
- **Who-said-what** — speaker diarization always runs locally (sherpa-onnx), on every hardware tier.
- **Your calendars** — Google Calendar & Microsoft 365, one-click meeting start. *(from M5)*
- **MCP server** — chat with your notes from Claude Desktop or any MCP client. *(from M6)*

> **Status: pre-release, milestone M0 (scaffold).** The build story below works; features land
> milestone by milestone — see [DECISIONS.md](DECISIONS.md) and the git tags (`m0`, `m1`, …).

## Build & run (Windows)

Prerequisites:

- Rust stable (MSVC toolchain) — `rustup` recommended
- Node.js ≥ 20
- Visual Studio Build Tools with the C++ workload
- WebView2 runtime (preinstalled on Windows 11)

```powershell
git clone https://github.com/swarnavspujari/looma-notetaker.git
cd looma-notetaker
npm install
npm --prefix frontend install
npm run tauri dev      # dev app with hot reload
```

Production build (installer under `src-tauri/target/release/bundle/`):

```powershell
npm run tauri build
```

Run the test suite:

```powershell
cargo test --workspace
```

## Repository layout

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full picture. In short: `crates/looma-core` is the
OS-free domain; every platform capability (audio, ASR, diarization, LLM, calendar, screen,
secrets) is a trait crate; `src-tauri` is the only place impls are picked; `frontend/` is a thin
React layer.

## Docs

- [ARCHITECTURE.md](ARCHITECTURE.md) — module boundaries and the porting story
- [DECISIONS.md](DECISIONS.md) — running log of technical decisions
- [docs/MODELS.md](docs/MODELS.md) — ASR/diarization model tiers, sizes, licenses
- [docs/PORTING.md](docs/PORTING.md) — macOS / iOS / Android guidance
- [docs/TESTING.md](docs/TESTING.md) — test strategy + manual checklist

## License

[MIT](LICENSE)
