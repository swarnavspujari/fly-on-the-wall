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
- **Integration test** (`src-tauri/tests/enhance_flow.rs`, runs in CI): the enhance flow
  offline with the deterministic `MockLLMProvider` — note + transcript → prompt (numbered
  segments) → canned block JSON → provenance-tagged storage, zoom-in id mapping, FTS
  searchability, markdown mirror, and reclaim-on-edit semantics.
- **MCP test** *(lands with M6)*: spawn the stdio server, assert `search_notes`/`get_note`
  return expected resources.

## CI

Every push: ESLint + Prettier check, `tsc` typecheck, frontend build, rustfmt check, clippy
(`-D warnings`), `cargo test`, `cargo build` — all on `windows-latest`. `main` stays green.

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
- [ ] External MCP client (Claude Desktop) can search and read notes (M6)
- [ ] Screen recording (full/window/region) attaches to a note (M7)
- [ ] Importing an audio/video file yields a diarized, summarized note (M8)
- [ ] Clean-machine install from the built installer runs the full flow (M9)
