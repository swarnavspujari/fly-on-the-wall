# Fly on the Wall

**Private meeting notes that never leave your computer.**

Fly on the Wall listens to your meetings, writes down who said what, and turns your rough
notes into clean, organized notes — all on your own machine. Nothing is uploaded to the
internet unless you choose to switch on a cloud feature.

> 📸 _Screenshot TODO: the main window when you first open the app._

## What Fly on the Wall is

Fly on the Wall is a desktop app for Windows, macOS, and Linux that helps you remember your
meetings.

While you're on a call, it records two things separately: your microphone (your voice) and the
sound coming out of your computer (everyone else on the call). When the meeting ends, it:

- **writes out the whole conversation** as text, and
- **labels who said each part** — "Speaker 1", "Speaker 2", and so on, which you can rename.

Then, if you'd like, it can **tidy your rough notes into a clean summary**.

The important part: **all of this happens on your computer.** The recording, the transcription
(turning speech into text), and the speaker labeling never touch the internet. Your meetings
stay with you.

There are only two exceptions, and both are switched off until you turn them on — and clearly
labeled in the app when they're active:

- sending a meeting to a cloud service for faster transcription (Groq), and
- using a cloud AI assistant to clean up your notes or answer questions about them (for
  example, Anthropic's Claude).

You bring your own account and key for those, so you stay in control of what leaves your
machine.

A couple of tips for the clearest recordings:

- **Keep your speaker volume up.** Fly on the Wall hears the other people through your
  computer's sound. If your output is muted, their side records as silence — the app warns you
  on screen if this happens mid-meeting.
- **Use a headset if you can.** On open speakers, your microphone also picks up the other
  people, which can leave echoes in the transcript. A headset keeps the two sides clean.

> 📸 _Screenshot TODO: a finished note with colored speaker labels._

## Install Fly on the Wall

Download the installer for your system from the
[latest release](https://github.com/swarnavspujari/fly-on-the-wall/releases/latest).

The app isn't signed with a paid certificate yet, so each system shows a one-time warning the
first time you open it. This is expected, and it goes away in a later release once the app is
signed. Here's how to get past it for now.

### Windows

1. Download the file whose name starts with **Fly on the Wall** and ends in
   `-setup-windows-x64.exe`, then double-click it.
2. Windows may show a blue box that says "Windows protected your PC." Click **More info**, then
   **Run anyway**.
3. Open **Fly on the Wall** from the Start menu. The first time you make a transcript, it
   downloads the speech models it needs (you'll see a progress bar). After that, it works
   without any internet connection.

> 📸 _Screenshot TODO: the Windows SmartScreen "More info → Run anyway" prompt._

After this one manual step, the app keeps itself up to date: it checks for a new version when
it starts, downloads it quietly in the background, and restarts once when you say so. Updates
never interrupt a recording — the prompt waits until you're finished.

### macOS

1. Download the `.dmg` for your Mac — choose **macos-arm64** if you have an Apple Silicon Mac
   (M1 or newer), or **macos-x64** if you have an Intel Mac. Open it and drag **Fly on the
   Wall** into your **Applications** folder.
2. The first time you open it, macOS may block it, or say it "can't be checked" or "is
   damaged." Either right-click the app and choose **Open**, then **Open** again — or, if macOS
   still refuses, open the **Terminal** app, run this line once, and then open the app normally:
   ```
   xattr -cr "/Applications/Fly on the Wall.app"
   ```
3. When it asks for permission to use your microphone, choose **Allow**.

> On a Mac, recording the other participants' sound isn't available yet, so Fly on the Wall
> records only your microphone and tells you so. Everything else works.

> 📸 _Screenshot TODO: the macOS "Open anyway" prompt._

### Linux

1. Download the **.AppImage** (works on most Linux systems) or the **.deb** (Debian and
   Ubuntu).
2. For the AppImage: right-click it → **Properties** → allow it to run as a program (or run
   `chmod +x` on it in a terminal), then double-click it. For the `.deb`: open it with your
   software installer, or run `sudo apt install ./` followed by the file's name in a terminal.
3. Your API keys and calendar sign-ins are kept in your system keyring. On a minimal setup,
   make sure `gnome-keyring` (or KWallet) is running so they have somewhere to live.

## Your first meeting

1. **Record.** Open the app and click **Record** (or click **Start** next to a meeting from
   your calendar). A bar shows that you're recording. Jot rough notes in the scratchpad as you
   go.
2. **Stop.** When the meeting ends, stop the recording. Fly on the Wall builds the transcript
   on your computer. You get timestamped text with speaker labels — click any name to rename it
   ("Speaker 1" → "Dana").
3. **Enhance.** Pick a template (1:1, sales, standup, interview, or general) and click
   **✨ Enhance**. It turns your rough notes and the transcript into clean, structured notes.
   Your own words stay in your color; lines the assistant added are tinted and link back to the
   exact moment in the transcript.
4. **Ask.** Click the chat button to ask questions about the meeting — "what did I miss?",
   "draft a follow-up email." Drop any answer into your note with one click.
5. **Organize and find.** Use folders on the left and the search box at the top (it searches
   your notes *and* your transcripts). Every note is also a plain file on your computer that you
   can open in any editor, or export to Markdown or PDF.

By default, **Enhance** and **Ask** use a local assistant that also runs on your computer, so
nothing leaves your machine. If you'd rather use a cloud assistant like Anthropic's Claude, see
[Add your API keys](#add-your-api-keys) below.

## Connect your calendars

Connecting a calendar is optional. It lets you start a note and recording straight from a
meeting, and shows what's coming up next.

### Connect your Google Calendar

If you're one of our invited testers, this is one click:

1. Click **Settings** at the bottom-left, then open the **Calendars** section.
2. Next to **Google Calendar**, click **Connect**.
3. Your web browser opens. Sign in to Google and click **Allow**.
4. You may see a screen saying Google "hasn't verified this app." While we're in testing, that
   screen is expected — click **Advanced**, then the link to continue to the app.
5. When the browser says you're connected, close that tab and come back to Fly on the Wall.

> 📸 _Screenshot TODO: Settings → Calendars, with the Connect buttons._

### Connect your Outlook / Microsoft 365 calendar

Same one click:

1. Open **Settings → Calendars**.
2. Next to **Microsoft 365 / Outlook**, click **Connect**.
3. Sign in with your Microsoft account, review the permissions, and click **Accept**.
4. If you see an "unverified app" notice, that's expected while we're in testing — continue.
5. Come back to the app once the browser says you're connected.

Your calendar sign-in is stored in your system's keychain, never in a plain file.

### Advanced: bring your own OAuth app

The one-click connections above are for invited testers. If you're setting up Fly on the Wall
on your own, you can register your own free calendar app instead — it takes a few minutes once.
Fly on the Wall talks directly to Google and Microsoft, with no server in between: it uses the
standard installed-app OAuth flow (PKCE + a loopback redirect on `http://127.0.0.1`), and your
sign-in token is stored in your system keychain, never in a plain file.

The client ID/secret fields live in **Settings → Calendars** — flip the **View** toggle at the
top of Settings from **Simple** to **Technical** to reveal them.

#### Google Calendar (step by step)

1. **Create a project.** In the [Google Cloud Console](https://console.cloud.google.com/), use
   the project picker at the top → **New Project** (or reuse one).
2. **Enable the API.** Go to **APIs & Services → Library**, search for **Google Calendar API**,
   and click **Enable**.
3. **Configure the consent screen.** Go to **APIs & Services → OAuth consent screen** (newer
   consoles call this the **Google Auth Platform**):
   - User type **External** (choose *Internal* only if everyone is in your Google Workspace org).
   - Fill in the app name, your support email, and a developer contact.
   - Add the scope `https://www.googleapis.com/auth/calendar.readonly`.
   - Add the Google addresses of anyone who will connect as **Test users**.
4. **Create the client.** Go to **APIs & Services → Credentials → Create credentials → OAuth
   client ID**, and choose application type **Desktop app**. (Desktop clients allow the loopback
   redirect automatically — you don't add any redirect URI.) Create it, then copy the **client
   ID** and **client secret**.
5. **Connect.** Paste both into **Settings → Calendars** (Technical view), click **Connect**
   next to Google Calendar, and finish signing in through the browser tab that opens.

> **Note on the "unverified app" screen and testers.** `calendar.readonly` is a *sensitive*
> scope. While your consent screen is in **Testing**, only listed Test users can connect and
> their sign-in refreshes **expire after 7 days** (they'd reconnect weekly). To make it
> permanent, publish the app to **Production** (Google may ask you to verify it for the sensitive
> scope). The one-click tester credentials Fly on the Wall bundles are set up this way already.

#### Microsoft 365 / Outlook (step by step)

> **First, make sure your account has a directory.** If you just created a fresh personal
> Microsoft account and Microsoft Entra ID greets you with *"Selected user account does not exist
> in tenant… Please use a different account,"* your account has no Entra **directory (tenant)**
> yet. The quickest fix: sign up at [azure.microsoft.com/free](https://azure.microsoft.com/free)
> with that account (the free tier isn't charged — the card is for identity only), which
> provisions a default directory. Then return to the steps below. An existing work/school
> account already has a directory and skips this.

1. **Register the app.** In the [Azure Portal](https://portal.azure.com/), go to **Microsoft
   Entra ID → App registrations → New registration**. Give it any name, and for **supported
   account types** choose **"Accounts in any organizational directory and personal Microsoft
   accounts"** — this is required, because Fly on the Wall signs in through the `/common`
   endpoint.
2. **Add the loopback redirect.** Under **Authentication → Add a platform**, choose **Mobile and
   desktop applications**, and add the custom redirect URI **`http://127.0.0.1`** (Azure ignores
   the random port on loopback addresses). On the same page, set **Allow public client flows** to
   **Yes**.
3. **Add permissions.** Under **API permissions → Add a permission → Microsoft Graph → Delegated
   permissions**, add **`Calendars.Read`** and **`offline_access`**. (Most accounts consent at
   sign-in; some organizations require an admin to approve once.)
4. **Connect.** From the app's **Overview**, copy the **Application (client) ID** into **Settings
   → Calendars** (Technical view), then click **Connect**. There is no client secret for
   Microsoft.

## Add your API keys

Two optional keys unlock the cloud features. You paste each one into **Settings**, and it's
stored in your system keychain — never in a plain file.

### Get your Groq API key (for faster transcription on slower computers)

Groq is a cloud service that can produce transcripts quickly when your computer is too slow to
do it well on its own. When Groq is switched on, your meeting audio is sent to Groq for
transcription — the speaker labeling still happens on your machine. Groq has a free tier.

1. Go to [console.groq.com](https://console.groq.com/) and sign in (you can use Google, GitHub,
   or email).
2. Open **API Keys → Create API Key**. Give it a name, then copy the key it shows you (it
   starts with `gsk_`). You won't be able to see it again, so paste it somewhere safe for a
   moment.
3. In Fly on the Wall, open **Settings**. In the transcription area, tick **Use Groq for
   transcription (cloud fallback)** and paste your key into the box just below it.
4. Save your settings. That's it.

> 📸 _Screenshot TODO: Settings, the "Use Groq" checkbox and key field._

### Get your Anthropic API key (for Enhance and Ask)

Anthropic's Claude is the cloud assistant that can clean up your notes (**Enhance**) and answer
questions about your meeting (**Ask**). When you use those features with Claude, your notes and
transcript are sent to Anthropic.

1. Go to [console.anthropic.com](https://console.anthropic.com/) and sign in.
2. Open **API Keys → Create Key**, and copy the key (it starts with `sk-ant-`). Keep it
   somewhere safe — you won't see it again. Using Claude requires some credit in your Anthropic
   account.
3. In Fly on the Wall, open **Settings** and find **AI provider (for Enhance & Ask)**.
4. Click **anthropic**, paste your key into the **API key** box, and click **Save provider**.
   You can click **Test connection** to check it worked.

> 📸 _Screenshot TODO: Settings, AI provider with the Anthropic key field._

---

## For developers

Everything below is for building Fly on the Wall from source or cutting a release. If you just
want to use the app, you're done above.

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
npm run prepare-sidecars   # builds + stages looma-mcp (required once before any cargo build)
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

Releases are built by [`.github/workflows/release.yml`](.github/workflows/release.yml) on every
`v*` tag. The Windows leg also feeds the auto-updater: it signs the NSIS installer (detached
`.sig`) and attaches `latest.json`, which installed apps poll at
`releases/latest/download/latest.json`.

1. Bump the version in `src-tauri/tauri.conf.json` **and** the workspace `Cargo.toml`
   (`[workspace.package] version`) — the workflow refuses a tag that doesn't match the config
   version, because a mismatched `latest.json` would make installed apps re-download the same
   update forever.
2. Make sure the two repo secrets exist (one-time setup): `TAURI_SIGNING_PRIVATE_KEY` and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — the Tauri updater keypair. The public half lives in
   `tauri.conf.json` (`plugins.updater.pubkey`). **If the private key or password is lost,
   shipped apps can never verify another update** — they'd need a manual reinstall with a new
   key.
3. *(Optional — one-click calendar for invited testers.)* Set three more repo secrets so the
   build bundles the app's own calendar OAuth client: `FOTW_GOOGLE_CLIENT_ID`,
   `FOTW_GOOGLE_CLIENT_SECRET` (a non-confidential PKCE desktop-client secret), and
   `FOTW_MS_CLIENT_ID`. They're read at compile time by `src-tauri/src/calendar_defaults.rs` and
   never live in the repo. Leave them unset and the release is simply BYO-only — users add their
   own OAuth app under Settings › Calendars (see [Connect your calendars](#connect-your-calendars)).
4. Land everything on `main` first (tag-push workflows run the workflow file at the tag's
   commit), then `git tag vX.Y.Z && git push origin vX.Y.Z`.

Local production builds don't need the key: `npm run tauri build` skips updater artifacts. To
reproduce the CI build exactly, set both `TAURI_SIGNING_PRIVATE_KEY(_PASSWORD)` env vars and add
`--config src-tauri/tauri.updater.conf.json`.

The installers are **unsigned** for now, which is why users see the SmartScreen/Gatekeeper
warnings documented in [Install Fly on the Wall](#install-fly-on-the-wall). Signing removes them
with no code changes: an OV/EV Authenticode certificate + `signtool` on Windows, and an Apple
Developer ID certificate with notarization (`codesign` + `notarytool`) on macOS — both wire into
`tauri.conf.json`/CI secrets. Linux needs no signing.

### Repository layout

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full picture. In short: `crates/looma-core` is
the OS-free domain; every platform capability (audio, ASR, diarization, LLM, calendar, screen,
secrets) is a trait crate; `src-tauri` is the only place impls are picked; `frontend/` is a thin
React layer.

### Chat with your notes from Claude Desktop (MCP)

Fly on the Wall ships `flyonthewall-mcp.exe`, a local stdio MCP server over your notes, folders,
meetings, and transcripts (read-only; nothing leaves the machine). Add it to Claude Desktop's
`claude_desktop_config.json` — in the app, **Settings → "Chat with your notes (MCP)"** generates
the exact snippet for your install location:

```json
{
  "mcpServers": {
    "flyonthewall": { "command": "C:\\path\\to\\flyonthewall-mcp.exe", "args": [] }
  }
}
```

Tools exposed: `search_notes`, `list_folders`, `get_note`, `get_transcript`, `get_meeting`,
`list_recent`.

### More docs

- [ARCHITECTURE.md](ARCHITECTURE.md) — module boundaries and the porting story
- [DECISIONS.md](DECISIONS.md) — running log of technical decisions
- [docs/MODELS.md](docs/MODELS.md) — ASR/diarization model tiers, sizes, licenses
- [docs/PORTING.md](docs/PORTING.md) — macOS / iOS / Android guidance
- [docs/TESTING.md](docs/TESTING.md) — test strategy + manual checklist

## License

[MIT](LICENSE)
