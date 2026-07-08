# Decisions log

Running log of technical decisions, newest last. Format: date — decision — why.

## M0 — Scaffold

- **2026-07-01 — Tauri 2 + React/TS/Vite/Tailwind, npm as package manager.** Matches the spec's
  stack; npm (not pnpm/bun) because it's preinstalled and CI-friendly on the target machine.
- **2026-07-01 — Rust edition 2021, workspace-wide dependency pinning.** Edition 2021 for maximum
  ecosystem compatibility; all shared deps live in `[workspace.dependencies]` so versions are
  agreed in one place. Verified current versions before pinning (tauri 2.11, rusqlite 0.40,
  cpal 0.18, reqwest 0.13, keyring 3).
- **2026-07-01 — keyring 3.x, not 4.x.** keyring 4 splits platform stores into separate
  `keyring-core` + store crates and pushes apps toward manual store selection; 3.x ships
  battle-tested native stores behind feature flags (`windows-native`, `apple-native`). Revisit
  once the 4.x ecosystem settles.
- **2026-07-01 — async traits via `async-trait` crate.** Providers/engines are selected at
  runtime (`Box<dyn Trait>`), which needs object safety; native `async fn` in traits isn't
  object-safe yet.
- **2026-07-01 — Blocks-with-origin as the provenance model.** A note = ordered blocks, each
  `user` or `ai{source_segment_ids}`. Editing an AI block reclaims it as user text (tested).
  Simpler and more robust than span-level diffing, and maps 1:1 to the required UI (colors,
  zoom-in, reclaim-on-edit).
- **2026-07-01 — SQLite is the index, files are the truth.** FTS5 for search; portable
  markdown/JSON on disk for user ownership (spec §10). FTS5 availability in the bundled SQLite is
  guarded by a unit test rather than assumed.
- **2026-07-01 — Speaker keys vs labels split.** Transcript segments store stable machine keys
  (`mic`, `spk_0`); display labels live in a relabelable `speakers` table on the transcript.
  Relabeling never rewrites segments.
- **2026-07-01 — mixed + per-channel audio all persisted.** Mic and system loopback are captured
  as separate WAVs plus a mixdown (spec §6.4): the mic channel is a known "you" speaker, the
  system channel gets diarized — better attribution than diarizing one mixed track.
- **2026-07-01 — TypeScript ~5.9 (not 6.x) in the frontend.** TS 6 is weeks-old major; the
  eslint/vite toolchain is validated against 5.9. Revisit later.
- **2026-07-01 — MIT license** (repo came initialized with it).

## M1 — Notes core

- **2026-07-01 — Scratchpad vs enhanced-blocks split on Note.** `scratchpad: String` holds the
  user's raw in-meeting notes; `blocks: Vec<NoteBlock>` is the enhanced document (empty until
  M4's Enhance). Matches the Granola mental model and keeps provenance unambiguous: the
  scratchpad is always user text; enhanced blocks carry user/ai origin per block.
- **2026-07-01 — FTS5 MATCH input is sanitized, never raw.** User queries are tokenized, quotes
  stripped, each token double-quoted, last token prefix-starred. Kills FTS syntax-error crashes
  and operator injection; hostile-input test included.
- **2026-07-01 — Native dialogs/openers via Rust plugin APIs, not JS.** File picker
  (tauri-plugin-dialog) and open/reveal (tauri-plugin-opener) are called inside commands, so no
  webview capability scoping is needed and paths never round-trip through the DOM.
- **2026-07-01 — Attachments copied, never referenced in place.** Files are copied under
  `attachments/<note_id>/` with collision-deduped names and stored as relative paths — the data
  dir stays self-contained and portable (spec §10).
- **2026-07-01 — react-hooks `set-state-in-effect` rule disabled.** Our effects fetch from the
  Tauri backend then set state — the standard desktop-app data flow the new rule over-flags.

## M2 — Meeting recorder

- **2026-07-01 — cpal loopback confirmed as primary path (no wasapi-crate fallback needed).**
  cpal 0.18 transparently enables WASAPI loopback when an input stream is built on an output
  device. Verified on real hardware: a 440 Hz tone played through the default output landed in
  the system channel at RMS 0.43. The escape hatch (raw `wasapi` crate) was not needed.
- **2026-07-01 — dedicated audio thread owns the streams.** cpal streams are !Send; the capture
  session talks to an owning thread via channels (pause/resume/stop), which keeps
  `Box<dyn CaptureSession>` Send for Tauri state.
- **2026-07-01 — loopback silence-padding to a pause-aware clock.** WASAPI loopback delivers no
  packets while the system is idle, which would silently compress the "them" channel's timeline
  and desync it from the mic. Callbacks pad with zeros against a shared pause-aware clock
  (>200 ms deficit) and the tail is padded at stop, so per-channel timelines stay wall-clock
  aligned — a prerequisite for the M3 per-channel transcript merge (§6.4).
- **2026-07-01 — channels recorded mono/native-rate; mixdown at 16 kHz.** Each channel is
  downmixed to mono 16-bit at the device's native rate (archival + per-channel ASR); stop()
  renders a 16 kHz mono mixed.wav (whisper's native input) with a dependency-free linear
  resampler. Graceful degrade: if loopback can't build, recording continues mic-only with a
  warning rather than failing the meeting.

## M3 — Transcription + diarization

- **2026-07-01 — Sidecar binaries first (spec escape hatch), taken deliberately.**
  whisper-cli.exe (whisper.cpp v1.9.1 release zip) and
  sherpa-onnx-offline-speaker-diarization.exe (v1.13.3) are downloaded per-machine with SHA-256
  verification and spawned with CREATE_NO_WINDOW. In-process bindings (whisper-rs, sherpa-onnx
  Rust API) remain a later refinement; the CLI contract is proven by the golden E2E test.
  Bonus: the same whisper.cpp zip ships parakeet-cli.exe, giving a cheap future Parakeet engine.
- **2026-07-01 — Word timestamps via `-ml 1 -sow -oj`.** whisper-cli emits one word per JSON
  entry with millisecond offsets — exactly what the aligner needs; no token-level DTW parsing.
- **2026-07-01 — Checksums pinned from upstream metadata.** GitHub release asset `digest` fields
  and HF LFS `oid` values, cross-verified by hashing the actual downloads locally. The two
  sherpa model assets predate GitHub digests and were pinned from local hashes.
- **2026-07-01 — Balanced tier defaults to large-v3-turbo-q5_0, not medium.** Turbo beats medium
  on accuracy at comparable CPU cost and is the spec's named sweet spot; medium stays in the
  dropdown. Max-quality toggle selects large-v3-q5_0 (~1 GB) rather than the 3 GB f16.
- **2026-07-01 — Pipeline core is tauri-free.** `pipeline::run_with` + `models::ensure` take
  progress-sink callbacks; the Tauri layer bridges them to events. Chosen after
  `tauri::test::mock_app` test exes died with STATUS_ENTRYPOINT_NOT_FOUND on Windows — and it is
  better layering anyway: the golden E2E test drives the real pipeline with zero webview.
- **2026-07-01 — Golden sample is generated Windows-TTS audio.** Two SAPI voices (David/Zira)
  reading a scripted 27.5 s meeting — license-clear by construction, committed as a fixture.
  Measured: WER 5.4 % (whisper small-q5), 2 speakers cleanly separated, correct attribution;
  the E2E test enforces WER < 25 % and exact speaker-count/attribution.

## M4 — Enhance, provenance, templates, Ask

- **2026-07-01 — One OpenAI-compatible impl covers OpenAI + NIM + Ollama.** They all speak
  `/chat/completions`; only Anthropic needs its own client (`/v1/messages`, system out-of-band).
  Four providers, two implementations.
- **2026-07-01 — Enhance output contract: JSON block array with per-block provenance.** The LLM
  returns `{type: user|ai, markdown, sources: [segment numbers]}`; transcript segments are sent
  with short numeric indices and mapped back to real segment ids after parsing (out-of-range
  citations dropped). `user` blocks restate scratchpad lines and render as the user's text; `ai`
  blocks carry their sources — that mapping IS the zoom-in feature. Malformed output falls back
  to untraced AI paragraphs rather than losing the response.
- **2026-07-01 — Ask chat is ephemeral by design** (spec §9): grounded in transcript + notes,
  never persisted; each answer has an explicit "insert into note" affordance instead.
- **2026-07-01 — Ollama is the default provider.** Fully local = the privacy-preserving default;
  cloud providers (OpenAI/Anthropic/NIM) are opt-in with keys in the keychain and a clear
  local/cloud badge in Settings.

## M5 — Calendars

- **2026-07-02 — Hand-rolled OAuth (PKCE + loopback) instead of the oauth2 crate.** The flow is
  ~200 lines: bind 127.0.0.1:0, open the system browser, catch one redirect, exchange the code.
  Owning it keeps the dependency tree small and makes the redirect UX (success page, state
  check, 5-min timeout) explicit. Pure parsers (redirect query, token JSON) are unit-tested.
- **2026-07-02 — BYO OAuth app registrations.** Looma ships no client credentials: users create
  a free Google "Desktop app" client (ID + secret) and/or an Azure public-client registration
  (ID only, PKCE). README documents both, step by step. Tokens (and Google's client secret) live
  in the keychain; client IDs are non-secret settings.
- **2026-07-02 — MS Graph as public client, Google as installed-app client.** Graph supports
  secret-less PKCE for desktop apps; Google's installed-app flow still wants its
  (non-confidential) client secret. Interactive connect cannot run in CI/agent context — pure
  parsers are tested and the connect flow is on the manual checklist.

## M6 — MCP server

- **2026-07-02 — Hand-rolled MCP over newline-delimited JSON-RPC.** The server speaks the MCP
  basics (initialize/ping/tools) in ~300 lines with zero protocol dependencies; it echoes the
  client's protocolVersion. Read-only by design — external clients can search and read, never
  mutate notes. Verified two ways: in-process unit tests and an integration test that spawns the
  real binary and talks over its stdio; plus a live client session against the app data dir.
- **2026-07-02 — Settings generates the exact Claude Desktop snippet** (absolute path to
  looma-mcp.exe next to the app exe) with copy-to-clipboard, so setup is paste-one-block.

## M7/M8 — Screen recording & file import

- **2026-07-02 — ffmpeg sidecar with gdigrab; graceful 'q' shutdown.** Full screen / window
  title / region map to gdigrab args (pure fn, unit-tested); stopping writes `q` to stdin so
  the MP4 moov atom is finalized, with a 10 s kill fallback. Output is written directly into
  `attachments/<note_id>/` and registered in-place (no copy of large videos).
- **2026-07-02 — Imports reuse the recording pipeline unchanged.** An imported file becomes a
  note + meeting with a 16 kHz mono `recording.mixed.wav` (pure-Rust for PCM WAVs; ffmpeg for
  everything else), then the exact same transcribe→diarize→align flow runs — one pipeline,
  two entry points.
- **2026-07-02 — ffmpeg pinned to a DATED BtbN autobuild, ultrafast preset, 1080p cap.** The
  rolling "latest" tag re-tags assets (hash drift breaks checksum pinning). Measured on this
  laptop: x264 veryfast at native high-DPI resolution encoded only 9 frames in 4 s (timeline
  compressed to 0.6 s); ultrafast + a 1920-wide cap records true wall-clock duration.

## M9 — Hardening & packaging

- **2026-07-02 — looma-mcp ships as a Tauri externalBin sidecar.** `prepare-sidecars.mjs`
  copies the release binary with the host target-triple suffix; the installer places
  `looma-mcp.exe` next to the app exe — exactly where the Settings MCP snippet points.
- **2026-07-02 — Data dir stays fixed (%APPDATA%/Looma) for v0.1.** A configurable storage
  path implies migration of recordings/models/DB; deferred rather than half-done. The dir is
  user-visible with a reveal-in-Explorer affordance.
- **2026-07-02 — Mic device selection is a stored setting** (`capture.mic_device_id`), applied
  at recording start; default remains the system default microphone.

## M10 — Design-system UI overhaul

- **2026-07-01 — Adopted the Claude design export as the visual source of truth.** The export
  (`design/Looma.dc.html`) defines a warm cream/coral language (Bricolage Grotesque display +
  Spline Sans body). Tokens live in `frontend/src/index.css` under Tailwind v4 `@theme`; shared
  primitives (`Btn`, `SectionLabel`, `ModalShell`, speaker color/initials helpers) in
  `frontend/src/components/ui.tsx`. Colors are used only via tokens — no raw palette classes.
- **2026-07-01 — Fonts are bundled, not fetched.** `@fontsource/bricolage-grotesque` +
  `@fontsource/spline-sans` ship in the app bundle; a font CDN would break the offline-first
  guarantee (and the app must render correctly in airplane mode).
- **2026-07-01 — Light theme replaces the dark zinc theme.** The design system is a light,
  warm language; the old hardcoded dark palette was replaced everywhere rather than themed
  twice. A dark mode can come later as a token swap.
- **2026-07-01 — Provenance rendering per the design's citation idiom.** User blocks are plain
  ink ("your words stay yours"); AI blocks are peach-tinted with a coral left rule and a
  peach/clay citation chip that zooms to the source segments. Mic/self is always coral in
  transcripts; other speakers rotate through the design's speaker palette.
- **2026-07-01 — Editor header restructured: actions row above, display title below.** With
  Bricolage at 26px the old single-row header truncated titles at any reasonable window width
  (verified in the running app); the design export also separates actions from the title.
- **2026-07-01 — Reskin executed as one-agent-per-surface with adversarial diff review.** Seven
  surfaces reskinned in parallel with disjoint file ownership; each diff was reviewed by an
  independent agent for dropped features/handlers before integration, then the whole app was
  driven and screenshotted over CDP (WebView2 remote debugging + playwright-core) for visual
  verification.

## M11 — Real-machine validation (fixes)

- **2026-07-01 — Versioned migrations + column repair in looma-storage.** Found live: this
  machine's `looma.db` predated the `scratchpad` column and every write failed —
  `CREATE TABLE IF NOT EXISTS` alone never upgrades old tables. `Storage::open` now runs the
  baseline DDL, then diffs each table against the expected column set and `ALTER TABLE ADD
  COLUMN`s what's missing (idempotent), then stamps `PRAGMA user_version = 1` for future
  numbered migrations. Regression test creates an old-schema DB and asserts note CRUD works.
- **2026-07-02 — Loopback-mute warning surfaces live in the recording bar.** This machine often
  sits at 0 % output volume, which makes WASAPI loopback record pure silence. New
  `AudioCapture::capture_warnings` (default empty, Windows impl reads the render endpoint via
  `IAudioEndpointVolume`) rides along `RecordingStatus`, which the UI already polls at 1 Hz —
  mute mid-meeting and a dark warning strip appears under the recording bar within a second
  (verified live: muted → warning shown, unmuted → gone).
- **2026-07-02 — ASR non-speech tokens are filtered at the parser.** A real recording with
  quiet stretches filled the transcript with whisper's `[BLANK_AUDIO]` / `[ Silence]`
  annotations, `>>` speaker-change markers, and `spk_unknown` filler segments. Word entries
  that are bracketed annotations are dropped (including annotations split across `-ml 1` word
  tokens — a span pass), `>>` prefixes are stripped, and the same filter guards the Groq path.
  Unit-tested; verified by re-transcribing the same recording (clean before/after diff).
- **2026-07-02 — Real-meeting validation run (this machine, Windows).** Full flow driven
  through the running app over CDP: record (TTS "meeting" through speakers → loopback), live
  scratchpad note, stop → transcribe+diarize (~50 s for ~3 min audio, small-q5), two TTS
  voices cleanly separated and attributed, speaker rename, Enhance via **local Ollama
  llama3.2:3b** (12 blocks, correct user/AI interleave, 1–2 citations per AI block), zoom-in
  from citation chip highlights the exact source segments, Ask returns a grounded answer with
  insert-into-note, FTS search hits both note and transcript, markdown/JSON mirrors on disk.
  Known physical limitation (not a bug): recording over laptop speakers bleeds the meeting
  audio into the mic channel as short echo fragments — a headset avoids it; noted in README.
- **2026-07-02 — Ollama is installed on demand, not bundled.** Validation found the default
  provider (Ollama) produced a raw reqwest error when absent. The error path now says what to
  do ("start Ollama / install from ollama.com / switch provider in Settings"). Bundling a
  model runtime stays out of scope — BYO models is the product's stance.

## M12 — Distribution

- **2026-07-02 — Unsigned installers, documented honestly.** NSIS (Windows), .app/.dmg
  (macOS arm64 + x64 runners), AppImage + .deb (Linux), built by a tag-push release workflow
  and attached to a GitHub Release. Code signing costs real money and keys we don't have;
  the README documents the SmartScreen/Gatekeeper bypasses and the exact signing path for
  later instead of pretending.
- **2026-07-02 — npm install (not ci) on non-Windows CI.** The lockfiles were regenerated on
  Windows, and npm's optional-dependency bug (npm/cli#4828) leaves other platforms' native
  bindings (rolldown, tauri-cli) out of the lockfile; `npm ci` then fails on macOS/Linux.
- **2026-07-02 — App icon drawn from the design system** (coral rounded square + white ring,
  the sidebar mark) and regenerated across every platform format with `tauri icon`.

## M13 — Cross-platform capture

- **2026-07-02 — Linux system audio via the Pulse/PipeWire monitor source.** A
  `PulseRecorder` (libpulse-simple, device `@DEFAULT_MONITOR@`) records the default sink's
  monitor on its own thread with the same pad-to-clock and pause-by-discard discipline as the
  WASAPI loopback; `LoopbackChannel` in cpal_backend picks per OS. Works on PulseAudio and
  PipeWire (pipewire-pulse). Builds in CI; not yet run on a Linux device.
- **2026-07-02 — macOS Core Audio process taps DEFERRED, with cause.** docs/PORTING.md's own
  research: tap IO callbacks return all-zero samples unless the binary is signed with the
  audio-capture entitlement usage string honored — and this release line ships unsigned
  (see M12). Implementing taps now would produce silent recordings for every real user, so
  macOS records mic-only (existing graceful degrade, warning logged) until signing exists.
  Screen capture on macOS is full-screen-only via ffmpeg avfoundation for the same
  pragmatism; ScreenCaptureKit is the right long-term impl once signing lands.
- **2026-07-02 — Per-OS tool registry + PATH fallback.** sherpa-onnx publishes binaries for
  all three OSes (pinned from upstream digests); ffmpeg has BtbN builds for Windows/Linux;
  whisper.cpp publishes no macOS/Linux CLI binaries at all. `ensure_tool` resolves: managed
  install → same tool on PATH → managed download → actionable error ("brew install
  whisper-cpp", "enable Groq fallback"). Linux screen capture is x11grab full-screen/region
  (Wayland needs a portal recorder — surfaced as an ffmpeg error, documented).

## M14 — Feature gaps

- **2026-07-02 — Live partial transcript (beta), chunked not streaming.** While recording, a
  loop transcribes the NEW audio appended to each channel WAV every few ticks (≥8 s chunks,
  ≤30 s, whisper small regardless of tier — live must outrun real time on laptop CPUs) and
  streams `live:segment` events; the UI shows a design-language live pane with channel-level
  attribution ("You" = mic, "Them" = loopback). No live diarization — the full pipeline
  replaces everything at Stop. Verified live on this machine: TTS meeting text appeared in
  the pane mid-recording, then the diarized transcript took over after Stop. Reading the
  in-progress WAVs ignores their stale headers (data grows past the 44-byte header;
  unit-tested chunk reader).
- **2026-07-02 — Export = save-as of the existing markdown mirror; PDF = print stylesheet.**
  The provenance-flattened `.md` mirror already exists per note, so "Export .md" is a save
  dialog + copy (dialog opens — the copy path reuses the attach_file idiom; the dialog step
  itself can't be driven headlessly). "Print / PDF" is `window.print()` plus `print:hidden`
  on all chrome so only the note content prints — no PDF renderer dependency.
- **2026-07-02 — Auto-updater and in-process whisper-rs/sherpa bindings deferred.** The
  updater needs its own signing key management and a hosted latest.json flow — meaningful
  work that shouldn't be rushed at the end of a session; friends can re-download installers.
  In-process bindings remain the right endgame (they'd give macOS/Linux whisper without
  Homebrew), noted for the next session.

## Storage naming & startup latency

- **2026-07-07 — Disk artifacts are named `<YYYY-MM-DD> <title>`, one folder per meeting.**
  Meeting folders (`recordings/2026-07-02 Tina 1-1/`) hold the WAVs, the imported source
  file, and the transcript mirrors (`transcript.md`/`.json`); note mirrors are
  `notes/<date> <title>.md`. Names are Windows-safe (illegal chars stripped, trailing
  dots/spaces trimmed, reserved device names suffixed, title capped at 60 chars) and deduped
  with ` (2)` suffixes. The date prefix makes Explorer's name sort chronological. The
  relative paths in `recording_json` remain the single source of truth for recording
  locations; meeting folders are found through them, never derived from titles. Notes get a
  `disk_path` column because dedup suffixes make mirror names non-derivable.
- **2026-07-07 — Title-edit policy: renames follow the note, best-effort for folders.**
  Renaming a note always renames its markdown mirror, and renames its meeting folders
  (rewriting `recording_json`) unless the folder is busy — e.g. mid-recording or
  mid-transcription, where Windows refuses the rename and the old (still valid) name is
  kept. Meeting rows keep their creation-time title snapshot; folders follow the note title
  because that's the one the user curates.
- **2026-07-07 — Versioned migrations via SQLite `user_version` (now 2).** `Storage::migrate`
  gates one-shot upgrade steps on the stored version. v2 renames existing UUID artifacts,
  moves transcript mirrors into meeting folders, sweeps leftover `*.16k.wav` intermediates
  (the pipeline now deletes its own on success), and parks artifacts with no DB row under
  `recordings/_unlinked/` and `notes/_unlinked/` — preserved, never deleted, and never
  resurrected into the DB (mirrors flatten provenance; re-indexing would be guesswork).
- **2026-07-07 — Startup: async commands + cached hardware detection + notes-first fetch.**
  Tauri runs sync commands serially on the main thread; the launch burst convoyed behind
  `get_asr_settings`, whose synchronous `nvidia-smi` ran before the notes query got a turn.
  Startup/polling commands are async now, `hw::detect()` is persisted in settings
  (`hw.cache`, background-refreshed each launch), the frontend fires the notes query first,
  and the last-known notes list (localStorage) paints immediately while the fresh fetch
  reconciles. Transient state (recording/queue/pipeline progress) is never persisted —
  always rendered from live polls and events.
- **2026-07-08 — GPU transcription: one Vulkan build per OS, gated by a measured-on-this-machine
  benchmark; CPU stays canon.** Post-meeting ASR may run on the GPU (any vendor) but only when a
  one-time speed test on ~60 s of real speech from the recording measured it faster than the CPU
  on that machine; the verdict persists per (machine, model) in `asr.gpu_bench`. Upstream
  whisper.cpp ships no Vulkan Windows binary (CPU/BLAS/CUDA-only), and a CUDA-only entry was
  rejected as vendor-locked, so `whisper-bin-vulkan` is whisper.cpp v1.9.1 built from the
  upstream tag with `-DGGML_VULKAN=1`, SHA-256-pinned and hosted as a tools release on this
  repo. Every GPU failure (benchmark, launch, nonzero exit, mid-run) falls back to the CPU
  engine visibly and re-pins the machine to CPU — a meeting transcript is never lost to a GPU.
  macOS keeps its Metal-by-default PATH build (setting gates a `-ng` flag only). The live loop
  never uses the GPU: it runs during capture, exactly when the GPU is busy with the video call.
