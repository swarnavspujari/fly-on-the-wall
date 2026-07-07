# Testing

## Automated

- **Unit tests (`cargo test --workspace`)**
  - `looma-core`: provenance transitions (AI block reclaimed on edit), the word↔speaker aligner
    (overlap assignment, straddling words, pause splitting, orphan fallback), markdown export.
  - `looma-storage`: schema creation, **FTS5 availability** (guards the bundled-SQLite
    assumption), and — from M1 — folder ops, note CRUD, search indexing.
  - `looma-secrets`: in-memory store roundtrip (keychain impl is exercised manually — CI runners
    have no unlocked keychain).
- **Golden transcription/diarization sample** (`src-tauri/tests/pipeline_e2e.rs`): a committed
  two-voice Windows-TTS clip (license-clear by construction) runs through the REAL pipeline —
  whisper.cpp sidecar → sherpa-onnx diarization → aligner → storage. Asserts WER < 25 %
  (measured: 5.4 %), exactly 2 speakers, correct line attribution, and searchable persistence.
  Heavy + needs downloaded artifacts, so it's `#[ignore]`d in CI; run locally with:
  `cargo test -p looma-app --test pipeline_e2e -- --ignored --nocapture`
  (artifacts are hardlinked from `%APPDATA%/Looma`; the test skips if absent).
- **Offline accuracy harness** (`src-tauri/tests/accuracy_harness.rs`, `#[ignore]`d): runs the
  real per-channel pipeline over any recording folder (`recording.mic.wav` +
  `recording.system.wav`) and reports the trust metrics — consecutive n-gram repetition runs
  (hallucination loops), distinct speaker count, per-channel word counts. Point
  `LOOMA_HARNESS_DIR` at the folder (optionally `LOOMA_HARNESS_MODEL`, and
  `LOOMA_HARNESS_MAX_SECS` to trim for fast iteration), or score an already-exported transcript
  JSON without running the pipeline via `LOOMA_HARNESS_SCORE_JSON`. Built to validate the
  anti-hallucination work against a real 1-hour meeting (a 1881×-repetition loop and 68 phantom
  speakers in the baseline).
- **Integration test** (`src-tauri/tests/enhance_flow.rs`, runs in CI): the enhance flow
  offline with the deterministic `MockLLMProvider` — note + transcript → prompt (numbered
  segments) → canned block JSON → provenance-tagged storage, zoom-in id mapping, FTS
  searchability, markdown mirror, and reclaim-on-edit semantics.
- **MCP tests** (`crates/looma-mcp`, run in CI): in-process protocol tests (initialize
  handshake, tools/list, tool calls, error paths) plus `tests/stdio.rs`, which spawns the real
  `looma-mcp` binary against a seeded data dir and asserts `search_notes`/`get_note` return the
  expected resources over actual stdio.

## CI

Every push: ESLint + Prettier check, `tsc` typecheck, frontend build, rustfmt check, clippy
(`-D warnings`), `cargo test`, `cargo build` on `windows-latest`, plus a build-and-test
matrix on `macos-latest` and `ubuntu-22.04`. Tag pushes (`v*`) build the three-OS
installers and attach them to a GitHub Release. `main` stays green.

## Manual checklist (run before tagging a milestone)

- [ ] `npm run tauri dev` boots to a window; backend status shows "connected" (M0)
- [ ] Create/rename/nest folders; create/edit/move/delete notes; search hits note bodies (M1)
- [ ] Attach a file; paste a URL; both survive app restart; note markdown on disk is readable (M1)
- [ ] Record a real call: mic + system audio present as separate WAVs; pause/resume works;
      indicator visible while recording (M2)
- [ ] Recording produces a diarized transcript offline (airplane mode); speakers relabelable (M3)
- [ ] Model tier switch (Light/Balanced/Best/Cloud) changes the engine/model actually used (M3)
- [ ] Enhance merges scratch notes + transcript; provenance colors correct; editing an AI line
      recolors it as user text; zoom-in shows the right segment (M4)
- [ ] Ask panel answers from the transcript; provider switch (incl. Ollama local) works (M4)
- [ ] Google and Microsoft calendars connect; upcoming meeting one-click starts a note (M5)
- [ ] External MCP client (Claude Desktop) can search and read notes (M6) — paste the snippet
      from Settings → "Chat with your notes (MCP)" into `claude_desktop_config.json`
- [ ] Screen recording (full/window/region) records, finalizes on Stop, and appears as an
      attachment on the note; ffmpeg downloads on first use with progress (M7)
- [ ] Importing an audio file (wav) AND a video file (mp4) each yield a diarized transcript on
      a new note; non-wav goes through ffmpeg conversion (M8)
- [ ] First-run consent notice appears once; recording indicator stays visible while recording
      (M9)
- [ ] Clean-machine install from the built installer runs the full flow (M9)
- [ ] Mute the system output while recording → a warning strip appears under the recording bar
      within ~1 s; unmute → it disappears (M11)
- [ ] With Ollama not running, Enhance/Ask show an actionable message (start Ollama / switch
      provider), not a raw network error (M11)
- [ ] A recording with long quiet stretches produces no `[BLANK_AUDIO]`/`[ Silence]`/`>>`
      artifacts or unknown-speaker filler segments in the transcript (M11)
- [ ] While recording, live partial passages appear in the Live transcript pane within
      ~20 s of speech and the full diarized transcript replaces them after Stop (M14)
- [ ] Export .md saves the note's markdown; Print / PDF prints only the note content, no
      app chrome (M14)
