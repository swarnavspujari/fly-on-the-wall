# Testing

## Automated

- **Unit tests (`cargo test --workspace`)**
  - `fly-core`: provenance transitions (AI block reclaimed on edit), the word↔speaker aligner
    (overlap assignment, straddling words, pause splitting, orphan fallback), markdown export.
  - `fly-storage`: schema creation, **FTS5 availability** (guards the bundled-SQLite
    assumption), and — from M1 — folder ops, note CRUD, search indexing.
  - `fly-secrets`: in-memory store roundtrip (keychain impl is exercised manually — CI runners
    have no unlocked keychain).
- **Golden transcription/diarization sample** (`src-tauri/tests/pipeline_e2e.rs`): a committed
  two-voice Windows-TTS clip (license-clear by construction) runs through the REAL pipeline —
  whisper.cpp sidecar → sherpa-onnx diarization → aligner → storage. Asserts WER < 25 %
  (measured: 5.4 %), exactly 2 speakers, correct line attribution, and searchable persistence.
  Heavy + needs downloaded artifacts, so it's `#[ignore]`d in CI; run locally with:
  `cargo test -p fly-app --test pipeline_e2e -- --ignored --nocapture`
  (artifacts are hardlinked from `%APPDATA%/Looma`; the test skips if absent).
  `FLYONTHEWALL_E2E_GPU=1` runs it through the pinned Vulkan GPU engine instead (combine with
  `GGML_VK_VISIBLE_DEVICES=<n>` to pick the GPU); the same file also carries
  `gpu_failure_falls_back_to_cpu`, which plants a broken GPU exe and asserts the pipeline
  completes on CPU with the verdict re-pinned.
- **Offline accuracy harness** (`src-tauri/tests/accuracy_harness.rs`, `#[ignore]`d): runs the
  real per-channel pipeline over any recording folder (`recording.mic.wav` +
  `recording.system.wav`) and reports the trust metrics — consecutive n-gram repetition runs
  (hallucination loops), distinct speaker count, per-channel word counts. Point
  `FLYONTHEWALL_HARNESS_DIR` at the folder (optionally `FLYONTHEWALL_HARNESS_MODEL`, and
  `FLYONTHEWALL_HARNESS_MAX_SECS` to trim for fast iteration), or score an already-exported transcript
  JSON without running the pipeline via `FLYONTHEWALL_HARNESS_SCORE_JSON`. `FLYONTHEWALL_HARNESS_GPU=1`
  forces the Vulkan GPU engine (seeded verdict, no benchmark); `FLYONTHEWALL_HARNESS_GPU=bench`
  enables GPU with no verdict so the real in-pipeline benchmark + gate run. Built to validate the
  anti-hallucination work against a real 1-hour meeting (a 1881×-repetition loop and 68 phantom
  speakers in the baseline).
- **LLM benchmark harness** (`src-tauri/tests/llm_bench.rs`, `#[ignore]`d): runs the four LLM
  tasks (Enhance / Polish / Extraction / Ask) over the committed fixture transcripts
  (`src-tauri/fixtures/bench/`) against a real provider+model and scores contract compliance,
  latency, and peak Ollama RSS; a judge phase scores candidate outputs against a reference
  model with claude-sonnet-5. Drives the shipped prompt/parse/guard code paths. Results and
  the default-local-model decision live in `docs/BENCHMARKS.md`, which documents how to run
  it. (Its `bench_fixtures_parse` test runs in CI to keep the fixtures honest.)
- **Integration test** (`src-tauri/tests/enhance_flow.rs`, runs in CI): the enhance flow
  offline with the deterministic `MockLLMProvider` — note + transcript → prompt (numbered
  segments) → canned block JSON → provenance-tagged storage, zoom-in id mapping, FTS
  searchability, markdown mirror, and reclaim-on-edit semantics.
- **MCP tests** (`crates/fly-mcp`, run in CI): in-process protocol tests (initialize
  handshake, tools/list, tool calls, error paths) plus `tests/stdio.rs`, which spawns the real
  `fly-mcp` binary against a seeded data dir and asserts `search_notes`/`get_note` return the
  expected resources over actual stdio.

## CI

Every push: ESLint + Prettier check, `tsc` typecheck, frontend build, rustfmt check, clippy
(`-D warnings`), `cargo test`, `cargo build` on `windows-latest`, plus a build-and-test
matrix on `macos-latest` and `ubuntu-22.04`. Tag pushes (`v*`) build the three-OS
installers and attach them to a GitHub Release; the Windows leg also signs the installer
with the updater key and attaches `latest.json` for the in-app auto-updater (requires the
`TAURI_SIGNING_PRIVATE_KEY(_PASSWORD)` secrets — see README "Cutting a release").
`main` stays green.

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
- [ ] Settings → App updates shows the current version and "Check for updates" answers
      against the latest published release (needs a release with `latest.json`) (auto-update)
- [ ] With an older build installed and a newer release published: the update banner appears
      shortly after launch; "Update now" downloads with visible progress and the app
      restarts into the new version (auto-update)
- [ ] While a recording (mic or screen) is active, no update banner ever appears and
      Settings' install/restart buttons are disabled; once the recording stops, the deferred
      prompt shows up (auto-update)
