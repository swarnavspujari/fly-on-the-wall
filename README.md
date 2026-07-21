<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/brand/logo-dark.png">
  <img src="docs/assets/brand/logo.png" alt="Fly on the Wall" width="360">
</picture>

### The meeting notes app that never phones home.

No bot joins your call. No audio leaves your computer. No subscription.

[**Download**](https://github.com/swarnavspujari/fly-on-the-wall/releases/latest) · [**Website**](https://swarnavspujari.github.io/fly-on-the-wall/) · [How it works](#how-it-works) · [Compare](#how-it-compares) · [Setup guides](#connect-your-calendars)

[![CI](https://github.com/swarnavspujari/fly-on-the-wall/actions/workflows/ci.yml/badge.svg)](https://github.com/swarnavspujari/fly-on-the-wall/actions/workflows/ci.yml)
[![Downloads](https://img.shields.io/endpoint?url=https%3A%2F%2Fswarnavspujari.github.io%2Ffly-on-the-wall%2Fdata%2Fdownloads-badge.json)](https://github.com/swarnavspujari/fly-on-the-wall/releases)
[![Latest release](https://img.shields.io/github/v/release/swarnavspujari/fly-on-the-wall?label=release&color=6A4AE0)](https://github.com/swarnavspujari/fly-on-the-wall/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-2B2B3C.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platforms-Windows%20·%20macOS%20·%20Linux-2B2B3C)

</div>

---

## Why this exists

Every popular meeting note-taker works the same way: a bot joins your call, your
conversation streams to someone else's servers, a transcript lives in someone else's
database, and you pay monthly for the privilege.

That's a strange deal for the most sensitive data you produce all day — candidate
interviews, sales negotiations, board conversations, customer calls under NDA.

**Fly on the Wall flips it.** It's a desktop app that listens from your side of the call —
your microphone and the sound coming out of your computer — so it works with Zoom, Meet,
Teams, or a phone on speaker, with nothing joining the meeting and nothing for other
participants to see. Recording, transcription, and speaker labeling all run on your own
machine. Your meetings stay on your disk, as plain files you can open in any editor.

It's free, open source (MIT), and it works offline.

<img src="docs/assets/screenshots/app-transcript-light.webp" alt="The Fly on the Wall main window: a meeting transcript with three color-coded speakers, a playable waveform, screen recordings, folders, and upcoming calendar meetings" width="100%">

## How it works

**1 — Record.** Click **Record**, or click **Start** next to a meeting from your connected
calendar. Fly on the Wall captures two channels separately — your mic (you) and your
system audio (everyone else) — and you jot rough notes in the scratchpad as you go.

**2 — Transcribe.** Stop the recording and the transcript is built on your computer:
timestamped text with speakers labeled and separated ("Speaker 1", "Speaker 2" — click any
name to rename it to "Dana"). Because your voice and everyone else's arrive on different
channels, "who said what" starts from ground truth instead of guesswork.

**3 — Enhance.** Pick a template — 1:1, sales, standup, interview, or general — and click
**✨ Enhance**. Your rough notes and the transcript become clean, structured notes. Your
own words stay in your color; every line the assistant added is tinted and carries a
citation chip that jumps to the exact moment in the transcript it came from. You always
know what you wrote, what the AI wrote, and what the evidence was.

**4 — Ask.** Open the chat panel and ask the meeting anything — "what did I commit to?",
"draft the follow-up email" — and drop any answer into your note with one click.

By default, Enhance and Ask use a local assistant that also runs on your computer, so
even the AI step never touches the internet. Prefer a frontier model? Bring your own
Anthropic key and use Claude — see [Add your API keys](#add-your-api-keys).

## What you get

- **No bot in the room.** Nothing joins your call, nothing announces itself, nothing
  depends on which meeting app the other side chose. If it makes sound on your computer,
  it can be captured.
- **On-device transcription and speaker labeling.** Speech-to-text and diarization run
  locally. The first transcript downloads the speech models once; after that it works
  with no internet connection at all.
- **Private by default, cloud by choice.** Exactly two features can touch the internet —
  Groq for faster transcription and a cloud AI for Enhance/Ask — both off until you turn
  them on, both clearly labeled in the app when active, both using *your* keys.
- **Notes with receipts.** Enhanced notes separate your words from AI words visually, and
  AI additions cite their transcript segments. No mystery summaries.
- **Search that actually finds things.** The search box covers your notes *and* your
  transcripts, with hybrid keyword + semantic matching — "the call where we discussed
  pricing" works even if nobody said the word "pricing".
- **Your files, not a silo.** Every note is a plain file on your disk. Organize with
  folders and drag-and-drop, export to Markdown or PDF, back up however you already back
  up files.
- **Import old recordings.** Drop in existing audio or video files — even several at once
  — and get the same transcripts, speakers, and enhanced notes for meetings that happened
  before you installed the app.
- **Calendar aware.** Connect Google Calendar and Outlook / Microsoft 365 (read-only) to
  see what's next and start a note straight from the meeting. Sign-ins are stored in your
  system keychain, never in a plain file.
- **Talks to your AI tools.** A bundled [MCP server](#chat-with-your-notes-from-claude-desktop-mcp)
  lets Claude Desktop and other AI clients read your meetings locally — ask "what's
  changed with the Acme account?" and get an answer with citations, without your notes
  leaving the machine.
- **Quiet auto-updates.** The app updates itself in the background and never interrupts a
  recording.

A couple of tips for the clearest recordings:

- **Keep your speaker volume up.** Fly on the Wall hears the other participants through
  your computer's sound. If your output is muted, their side records as silence — the app
  warns you on screen if this happens mid-meeting.
- **Use a headset if you can.** On open speakers, your microphone also picks up the other
  people, which can leave echoes in the transcript. A headset keeps the two sides clean.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/screenshots/app-notes-dark.webp">
  <img src="docs/assets/screenshots/app-notes-light.webp" alt="A meeting note in the editor with rough notes, a highlighted line, checked-off action items, an attached PDF, and the Enhance button — shown in your theme" width="100%">
</picture>

## How it compares

The honest version: cloud note-takers are polished products with real strengths —
mobile apps, team workspaces, CRM integrations. If those matter more to you than privacy,
cost, or control, they're reasonable choices. Here's the objective difference in how they
work, as of July 2026 (details change — check each vendor):

|  | **Fly on the Wall** | Otter.ai | Fireflies.ai | Fathom | Granola |
|---|---|---|---|---|---|
| Where audio is processed | **Your computer** | Their cloud | Their cloud | Their cloud | Cloud AI for notes |
| A bot joins your call | **Never** | Optional — bot or desktop app | Yes, by default | Optional — bot-free in beta | No |
| Works with any meeting app | **Yes — captures system audio** | Yes, via desktop app | Supported platforms | Supported platforms | Yes |
| Works fully offline | **Yes** | No | No | No | No |
| Where transcripts live | **Your disk, plain files** | Their servers | Their servers | Their servers | Their cloud sync |
| Open source | **Yes, MIT** | No | No | No | No |
| Price | **Free** | Free tier + paid plans | Free tier + paid plans | Free tier + paid plans | Free tier + paid plans |
| AI provider | **Local by default; bring your own key** | Theirs | Theirs | Theirs | Theirs |

If your meetings involve anything you wouldn't paste into a public form — legal, medical,
HR, unreleased product, other people's confidential information — the architecture is the
feature. There is no server to breach, no retention policy to read, and no vendor
processing agreement to negotiate, because there is no vendor in the loop.

## Install

Download the installer for your system from the
[latest release](https://github.com/swarnavspujari/fly-on-the-wall/releases/latest).

The app isn't signed with a paid certificate yet, so each system shows a one-time warning
the first time you open it. This is expected, and it goes away in a later release once the
app is signed. Here's how to get past it for now.

### Windows

1. Download the file whose name starts with **Fly on the Wall** and ends in
   `-setup-windows-x64.exe`, then double-click it.
2. Windows may show a blue box that says "Windows protected your PC." Click **More info**,
   then **Run anyway**.
3. Open **Fly on the Wall** from the Start menu. The first time you make a transcript, it
   downloads the speech models it needs (you'll see a progress bar). After that, it works
   without any internet connection.

After this one manual step, the app keeps itself up to date: it checks for a new version
when it starts, downloads it quietly in the background, and restarts once when you say so.
Updates never interrupt a recording — the prompt waits until you're finished.

### macOS

1. Download the `.dmg` for your Mac — choose **macos-arm64** if you have an Apple Silicon
   Mac (M1 or newer), or **macos-x64** if you have an Intel Mac. Open it and drag **Fly on
   the Wall** into your **Applications** folder.
2. The first time you open it, macOS may block it, or say it "can't be checked" or "is
   damaged." Either right-click the app and choose **Open**, then **Open** again — or, if
   macOS still refuses, open the **Terminal** app, run this line once, and then open the
   app normally:
   ```
   xattr -cr "/Applications/Fly on the Wall.app"
   ```
3. When it asks for permission to use your microphone, choose **Allow**.
4. On macOS 14.2 or newer, the first recording also asks for permission to record
   **system audio** (the other participants' sound). Choose **Allow** — you can change it
   later under **System Settings → Privacy & Security → Screen & System Audio Recording**.

> Everything downloads itself on first use — the transcription engine and speech models
> arrive automatically; nothing needs Homebrew. Apple Silicon Macs (M1 and newer)
> transcribe on the GPU; Intel Macs use the CPU. On macOS older than 14.2, or when the
> system-audio permission can't be granted, Fly on the Wall records only your microphone
> and says so in the recording bar — everything else works.

### Linux

1. Download the **.AppImage** (works on most Linux systems) or the **.deb** (Debian and
   Ubuntu).
2. For the AppImage: right-click it → **Properties** → allow it to run as a program (or
   run `chmod +x` on it in a terminal), then double-click it. For the `.deb`: open it with
   your software installer, or run `sudo apt install ./` followed by the file's name in a
   terminal.
3. Your API keys and calendar sign-ins are kept in your system keyring. On a minimal
   setup, make sure `gnome-keyring` (or KWallet) is running so they have somewhere to
   live.

## Your first meeting

1. **Record.** Open the app and click **Record** (or click **Start** next to a meeting
   from your calendar). A bar shows that you're recording. Jot rough notes in the
   scratchpad as you go.
2. **Stop.** When the meeting ends, stop the recording. Fly on the Wall builds the
   transcript on your computer. You get timestamped text with speaker labels — click any
   name to rename it ("Speaker 1" → "Dana").
3. **Enhance.** Pick a template (1:1, sales, standup, interview, or general) and click
   **✨ Enhance**. It turns your rough notes and the transcript into clean, structured
   notes. Your own words stay in your color; lines the assistant added are tinted and link
   back to the exact moment in the transcript.
4. **Ask.** Click the chat button to ask questions about the meeting — "what did I miss?",
   "draft a follow-up email." Drop any answer into your note with one click.
5. **Organize and find.** Use folders on the left and the search box at the top (it
   searches your notes *and* your transcripts). Every note is also a plain file on your
   computer that you can open in any editor, or export to Markdown or PDF.

By default, **Enhance** and **Ask** use a local assistant that also runs on your computer,
so nothing leaves your machine. If you'd rather use a cloud assistant like Anthropic's
Claude, see [Add your API keys](#add-your-api-keys) below.

## Connect your calendars

Connecting a calendar is optional. It lets you start a note and recording straight from a
meeting, and shows what's coming up next.

<img src="docs/assets/screenshots/upnext.webp" alt="The Up next sidebar widget: a live meeting from Google Calendar and an upcoming one from Outlook, each with a Start button" width="360">

### Connect your Google Calendar

If you're one of our invited testers, this is one click:

1. Click **Settings** at the bottom-left, then open the **Calendars** section.
2. Next to **Google Calendar**, click **Connect**.
3. Your web browser opens. Sign in to Google and click **Allow**.
4. You may see a screen saying Google "hasn't verified this app." While we're in testing,
   that screen is expected — click **Advanced**, then the link to continue to the app.
5. When the browser says you're connected, close that tab and come back to Fly on the
   Wall.

### Connect your Outlook / Microsoft 365 calendar

Same one click:

1. Open **Settings → Calendars**.
2. Next to **Microsoft 365 / Outlook**, click **Connect**.
3. Sign in with your Microsoft account, review the permissions, and click **Accept**.
4. If you see an "unverified app" notice, that's expected while we're in testing —
   continue.
5. Come back to the app once the browser says you're connected.

Your calendar sign-in is stored in your system's keychain, never in a plain file.

### Advanced: bring your own OAuth app

The one-click connections above are for invited testers. If you're setting up Fly on the
Wall on your own, you can register your own free calendar app instead — it takes a few
minutes once. Fly on the Wall talks directly to Google and Microsoft, with no server in
between: it uses the standard installed-app OAuth flow (PKCE + a loopback redirect on
`http://127.0.0.1`), and your sign-in token is stored in your system keychain, never in a
plain file.

The client ID/secret fields live in **Settings → Calendars** — flip the **View** toggle at
the top of Settings from **Simple** to **Technical** to reveal them.

#### Google Calendar (step by step)

1. **Create a project.** In the [Google Cloud Console](https://console.cloud.google.com/),
   use the project picker at the top → **New Project** (or reuse one).
2. **Enable the API.** Go to **APIs & Services → Library**, search for **Google Calendar
   API**, and click **Enable**.
3. **Configure the consent screen.** Go to **APIs & Services → OAuth consent screen**
   (newer consoles call this the **Google Auth Platform**):
   - User type **External** (choose *Internal* only if everyone is in your Google
     Workspace org).
   - Fill in the app name, your support email, and a developer contact.
   - Add the scope `https://www.googleapis.com/auth/calendar.readonly`.
   - Add the Google addresses of anyone who will connect as **Test users**.
4. **Create the client.** Go to **APIs & Services → Credentials → Create credentials →
   OAuth client ID**, and choose application type **Desktop app**. (Desktop clients allow
   the loopback redirect automatically — you don't add any redirect URI.) Create it, then
   copy the **client ID** and **client secret**.
5. **Connect.** Paste both into **Settings → Calendars** (Technical view), click
   **Connect** next to Google Calendar, and finish signing in through the browser tab that
   opens.

> **Note on the "unverified app" screen and testers.** `calendar.readonly` is a
> *sensitive* scope. While your consent screen is in **Testing**, only listed Test users
> can connect and their sign-in refreshes **expire after 7 days** (they'd reconnect
> weekly). To make it permanent, publish the app to **Production** (Google may ask you to
> verify it for the sensitive scope). The one-click tester credentials Fly on the Wall
> bundles are set up this way already.

#### Microsoft 365 / Outlook (step by step)

> **First, make sure your account has a directory.** If you just created a fresh personal
> Microsoft account and Microsoft Entra ID greets you with *"Selected user account does
> not exist in tenant… Please use a different account,"* your account has no Entra
> **directory (tenant)** yet. The quickest fix: sign up at
> [azure.microsoft.com/free](https://azure.microsoft.com/free) with that account (the free
> tier isn't charged — the card is for identity only), which provisions a default
> directory. Then return to the steps below. An existing work/school account already has a
> directory and skips this.

1. **Register the app.** In the [Azure Portal](https://portal.azure.com/), go to
   **Microsoft Entra ID → App registrations → New registration**. Give it any name, and
   for **supported account types** choose **"Accounts in any organizational directory and
   personal Microsoft accounts"** — this is required, because Fly on the Wall signs in
   through the `/common` endpoint.
2. **Add the loopback redirect.** Under **Authentication → Add a platform**, choose
   **Mobile and desktop applications**, and add the custom redirect URI
   **`http://127.0.0.1`** (Azure ignores the random port on loopback addresses). On the
   same page, set **Allow public client flows** to **Yes**.
3. **Add permissions.** Under **API permissions → Add a permission → Microsoft Graph →
   Delegated permissions**, add **`Calendars.Read`** and **`offline_access`**. (Most
   accounts consent at sign-in; some organizations require an admin to approve once.)
4. **Connect.** From the app's **Overview**, copy the **Application (client) ID** into
   **Settings → Calendars** (Technical view), then click **Connect**. There is no client
   secret for Microsoft.

## Add your API keys

Two optional keys unlock the cloud features. You paste each one into **Settings**, and
it's stored in your system keychain — never in a plain file.

### Get your Groq API key (for faster transcription on slower computers)

Groq is a cloud service that can produce transcripts quickly when your computer is too
slow to do it well on its own. When Groq is switched on, your meeting audio is sent to
Groq for transcription — the speaker labeling still happens on your machine. Groq has a
free tier.

1. Go to [console.groq.com](https://console.groq.com/) and sign in (you can use Google,
   GitHub, or email).
2. Open **API Keys → Create API Key**. Give it a name, then copy the key it shows you (it
   starts with `gsk_`). You won't be able to see it again, so paste it somewhere safe for
   a moment.
3. In Fly on the Wall, open **Settings**. In the transcription area, tick **Use Groq for
   transcription (cloud fallback)** and paste your key into the box just below it.
4. Save your settings. That's it.

### Get your Anthropic API key (for Enhance and Ask)

Anthropic's Claude is the cloud assistant that can clean up your notes (**Enhance**) and
answer questions about your meeting (**Ask**). When you use those features with Claude,
your notes and transcript are sent to Anthropic.

1. Go to [console.anthropic.com](https://console.anthropic.com/) and sign in.
2. Open **API Keys → Create Key**, and copy the key (it starts with `sk-ant-`). Keep it
   somewhere safe — you won't see it again. Using Claude requires some credit in your
   Anthropic account.
3. In Fly on the Wall, open **Settings** and find **AI provider (for Enhance & Ask)**.
4. Click **anthropic**, paste your key into the **API key** box, and click **Save
   provider**. You can click **Test connection** to check it worked.

---

## For developers

Everything below is for building Fly on the Wall from source or cutting a release. If you
just want to use the app, you're done above.

### Build & run (Windows)

Prerequisites:

- Rust stable (MSVC toolchain) — `rustup` recommended
- Node.js ≥ 20
- Visual Studio Build Tools with the C++ workload
- WebView2 runtime (preinstalled on Windows 11)

```powershell
git clone https://github.com/swarnavspujari/fly-on-the-wall.git
cd fly-on-the-wall
npm install
npm --prefix frontend install
npm run prepare-sidecars   # builds + stages fly-mcp (required once before any cargo build)
npm run tauri dev          # dev app with hot reload
```

Production build (installers under the workspace `target/release/bundle/`):

```powershell
npm run tauri build
```

Run the test suite:

```powershell
cargo test --workspace
```

### Cutting a release

Releases are built by [`.github/workflows/release.yml`](.github/workflows/release.yml) on
every `v*` tag. The Windows leg also feeds the auto-updater: it signs the NSIS installer
(detached `.sig`) and attaches `latest.json`, which installed apps poll at
`releases/latest/download/latest.json`.

1. Bump the version in `src-tauri/tauri.conf.json` **and** the workspace `Cargo.toml`
   (`[workspace.package] version`) — the workflow refuses a tag that doesn't match the
   config version, because a mismatched `latest.json` would make installed apps
   re-download the same update forever.
2. Make sure the two repo secrets exist (one-time setup): `TAURI_SIGNING_PRIVATE_KEY` and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — the Tauri updater keypair. The public half lives
   in `tauri.conf.json` (`plugins.updater.pubkey`). **If the private key or password is
   lost, shipped apps can never verify another update** — they'd need a manual reinstall
   with a new key.
3. *(Optional — one-click calendar for invited testers.)* Set three more repo secrets so
   the build bundles the app's own calendar OAuth client: `FOTW_GOOGLE_CLIENT_ID`,
   `FOTW_GOOGLE_CLIENT_SECRET` (a non-confidential PKCE desktop-client secret), and
   `FOTW_MS_CLIENT_ID`. They're read at compile time by
   `src-tauri/src/calendar_defaults.rs` and never live in the repo. Leave them unset and
   the release is simply BYO-only — users add their own OAuth app under Settings ›
   Calendars (see [Connect your calendars](#connect-your-calendars)).
4. Land everything on `main` first (tag-push workflows run the workflow file at the tag's
   commit), then `git tag vX.Y.Z && git push origin vX.Y.Z`.

Local production builds don't need the key: `npm run tauri build` skips updater artifacts.
To reproduce the CI build exactly, set both `TAURI_SIGNING_PRIVATE_KEY(_PASSWORD)` env
vars and add `--config src-tauri/tauri.updater.conf.json`.

The installers are **unsigned** for now, which is why users see the
SmartScreen/Gatekeeper warnings documented in [Install](#install). Signing removes them
with no code changes: an OV/EV Authenticode certificate + `signtool` on Windows, and an
Apple Developer ID certificate with notarization (`codesign` + `notarytool`) on macOS —
both wire into `tauri.conf.json`/CI secrets. Linux needs no signing.

### Repository layout

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full picture. In short: `crates/fly-core`
is the OS-free domain; every platform capability (audio, ASR, diarization, LLM, calendar,
screen, secrets) is a trait crate; `src-tauri` is the only place impls are picked;
`frontend/` is a thin React layer.

### Chat with your notes from Claude Desktop (MCP)

Fly on the Wall ships `flyonthewall-mcp.exe`, a local stdio MCP server over your notes,
meetings, transcripts, and the structured items extracted from them (nothing leaves the
machine; the only write it allows is renaming a speaker label). Add it to Claude Desktop's
`claude_desktop_config.json` — in the app, **Settings → "Chat with your notes (MCP)"**
generates the exact snippet for your install location:

```json
{
  "mcpServers": {
    "flyonthewall": { "command": "C:\\path\\to\\flyonthewall-mcp.exe", "args": [] }
  }
}
```

Context layer (start here): `get_context` — a deterministic briefing for any project /
customer / recurring meeting, with every claim citing its meeting and transcript segments
— plus `whats_changed`, `open_items`, `query_items`, `get_meeting_items`. Underlying
material: `search_notes`, `list_folders`, `get_note`, `get_transcript`,
`get_transcripts`, `get_meeting`, `list_recent`. The write: `set_speaker_label`. Items are
extracted in the app after each transcription (Settings → the MCP card has a one-click
backfill for older meetings) using your selected AI provider.

**Skill for AI clients:**
[`skills/flyonthewall-meetings/SKILL.md`](skills/flyonthewall-meetings/SKILL.md) teaches
the intended workflow (context first, transcripts to verify, provenance rules). Install it
in Claude Code / Claude Desktop by copying the folder into `~/.claude/skills/` (or your
project's `.claude/skills/`); for ChatGPT or Gemini, paste the file's body into custom
instructions / a Gem alongside the MCP connection.

### More docs

- [ARCHITECTURE.md](ARCHITECTURE.md) — module boundaries and the porting story
- [DECISIONS.md](DECISIONS.md) — running log of technical decisions
- [docs/MODELS.md](docs/MODELS.md) — ASR/diarization model tiers, sizes, licenses
- [docs/BENCHMARKS.md](docs/BENCHMARKS.md) — local-LLM benchmark behind the default model choice
- [docs/PORTING.md](docs/PORTING.md) — macOS / iOS / Android guidance
- [docs/TESTING.md](docs/TESTING.md) — test strategy + manual checklist

## License

[MIT](LICENSE)
