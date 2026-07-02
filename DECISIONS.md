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
