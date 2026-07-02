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

> **Status: v0.1.0.** All nine milestones (`m0`…`m8` + hardening) are in — recording,
> local transcription + diarization, enhance with provenance, templates, Ask, calendars,
> MCP, screen recording, and file import. See [DECISIONS.md](DECISIONS.md) for the build story.

## A two-minute tour

1. **Record** — open Looma, hit **● Record** (or click **Start** on a calendar event). A red
   bar shows while recording; your mic and the meeting's system audio are captured as separate
   channels. Jot rough notes in the scratchpad as you go.
2. **Stop** — the transcript starts automatically: whisper.cpp + sherpa-onnx run *on your
   machine* (first run downloads the models with progress + checksums). You get timestamped,
   speaker-labeled text; click any name to rename ("Speaker 1" → "Dana").
3. **Enhance** — pick a template (1:1, Sales discovery, Standup, Interview, General) and hit
   **✨ Enhance**. Your own lines stay your color; AI-added lines are tinted and cite their
   transcript segments — click 🔍 to jump to the exact source. Edit an AI line and it becomes
   yours.
4. **Ask** — 💬 opens a chat grounded in this meeting ("What did I miss?", "Draft a follow-up
   email"). Insert any answer into the note with one click.
5. **Organize & find** — folders on the left, full-text search across notes *and* transcripts
   at the top, attachments and pasted links on any note. Everything is markdown on disk you can
   open without Looma.
6. **Chat from Claude Desktop** — Settings → copy the MCP snippet, and your notes are queryable
   from any MCP client, fully locally.

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

## Connecting calendars (bring your own OAuth app)

Looma talks directly to Google/Microsoft — no middleman server — so you register your own
(free) OAuth app once:

**Google Calendar**
1. In [Google Cloud Console](https://console.cloud.google.com/) create a project → *APIs &
   Services* → enable the **Google Calendar API**.
2. *Credentials* → *Create credentials* → **OAuth client ID** → type **Desktop app**.
3. Copy the client ID and client secret into Looma → Settings → Calendars, hit **Connect**,
   finish sign-in in the browser tab that opens.

**Microsoft 365 / Outlook**
1. In [Azure Portal](https://portal.azure.com/) → *App registrations* → *New registration*
   (any name; supported account types: personal + work accounts).
2. Under *Authentication* → *Add a platform* → **Mobile and desktop applications** → check the
   loopback option (`http://localhost`) and enable **Allow public client flows**.
3. Copy the *Application (client) ID* into Looma → Settings → Calendars → **Connect**.
   No client secret is needed (PKCE public client).

Tokens are stored in the Windows Credential Manager, never on disk.

## Chat with your notes from Claude Desktop (MCP)

Looma ships `looma-mcp.exe`, a local stdio MCP server over your notes, folders, meetings, and
transcripts (read-only; nothing leaves the machine). Add it to Claude Desktop's
`claude_desktop_config.json` — Looma → Settings → "Chat with your notes (MCP)" generates the
exact snippet for your install location:

```json
{
  "mcpServers": {
    "looma": { "command": "C:\\path\\to\\looma-mcp.exe", "args": [] }
  }
}
```

Tools exposed: `search_notes`, `list_folders`, `get_note`, `get_transcript`, `get_meeting`,
`list_recent`.

## Docs

- [ARCHITECTURE.md](ARCHITECTURE.md) — module boundaries and the porting story
- [DECISIONS.md](DECISIONS.md) — running log of technical decisions
- [docs/MODELS.md](docs/MODELS.md) — ASR/diarization model tiers, sizes, licenses
- [docs/PORTING.md](docs/PORTING.md) — macOS / iOS / Android guidance
- [docs/TESTING.md](docs/TESTING.md) — test strategy + manual checklist

## License

[MIT](LICENSE)
