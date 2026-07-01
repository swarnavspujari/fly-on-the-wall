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
