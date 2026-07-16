# Community PR triage — Ian Sumner's #25–#30, #34

Integration branch: `integration/ian-prs` (off `main` @ 16f27a8 / v1.2.3).
Each accepted PR is a real merge of the contributor's commits followed by
`fix:`/`test:` remediation commits; every step passed the full gate
(`cargo fmt --check`, `clippy --workspace --all-targets` zero warnings,
`cargo test -p fly-app -p fly-asr`, `tsc --noEmit`, eslint, and — once added —
`npm test` vitest) and a fresh-context re-review.

Merge order: #34 → #27 → #29 → #28 → #30, then the diagnostics work item.
Separate branch: `feat/managed-mac-whisper-rehost` (supersedes #26).

| PR | Verdict | One-liner |
|---|---|---|
| #34 | **merge** | model-dropdown-overrides-tier hint; fixed styling + wording |
| #27 | **merge** | macOS Metal→CPU guarded fallback; extended to the Groq-fallback and live paths |
| #29 | **merge** | batch transcription progress; fmt, notice-clobber and em-dash fixes, tests |
| #28 | **merge** | resilient downloads; checksum-skip, mirror disclosure/opt-out, live single-attempt |
| #30 | **merge** | actionable engine/download notices; 9 fixes + vitest + dev-mock QA |
| #25 | **close as superseded** | #30 contains and extends it (same feature, more complete) |
| #26 | **reject, supersede** | sound design, but the binary must be maintainer-built and repo-hosted → `feat/managed-mac-whisper-rehost` |

---

## PR #34 — model dropdown overrides hardware tier (hint)

- **Fixed:** stray `marginTop: -4` (peer helper texts have none); hint now
  also names the "Maximum quality" checkbox (verified:
  `default_model_for_tier(tier, max_quality)` is only consulted when no
  override is set — pipeline.rs); re-review caught that "settings above"
  misdirected (the checkbox is *below* the hint) — word dropped.
- **Known simplification (documented, not fixed):** on the Cloud tier the
  chosen model only configures the local *fallback* engine (Groq still
  transcribes); the hint doesn't spell that out. Acceptable for helper text.
- **Tests:** none needed (copy + style only). Verified in dev-mock QA.

## PR #27 — Metal→CPU fallback on macOS

- **Merged mechanism (sound):** Metal runs as a guarded primary with a
  forced-CPU (`-ng`) sibling, mirroring the Windows Vulkan guard; a runtime
  failure pins the machine+model to CPU (`gpu::record_runtime_failure`),
  toggling the GPU switch off→on clears the pin.
- **Fixed (same bug class):**
  - Groq path's local fallback ignored the stored CPU pin (`force_cpu:
    !use_gpu`); it now honors the pin via extracted
    `cpu_pinned_for_model`/`local_fallback_force_cpu`, and on macOS always
    forces CPU — it is the last engine in the chain, and an unpinned machine
    may be exactly the one whose Metal init aborts (re-review finding, fixed).
  - live.rs built its engine with `force_cpu: false` — live captions actually
    ran on Metal on macOS, contradicting MODELS.md; now
    `cfg!(target_os = "macos")` forces `-ng` (Windows invocation unchanged).
  - The mangled "because on GPUs Metal can't serve…" sentence fixed in both
    MODELS.md and the code comment.
- **Tests:** `pinned_cpu_verdict_forces_cpu_on_groq_fallback_path` (real
  Storage round-trip through `gpu::record_runtime_failure`).
- **Mac-unverified:** the `#[cfg(target_os = "macos")]` block cannot compile
  on this machine — desk-checked (and pattern-replicated under `rustc --cfg`
  by the re-reviewer); behavior is on the smoke checklist.

## PR #29 — transcription progress ("your microphone (42%)")

- **Merged logic (correct):** speech-milliseconds accounting over VAD batch
  plans; `TranscribeProgress` + default-method plumbing through
  `TranscriptionEngine` and `GuardedAsr`.
- **Fixed:** the PR's own lines failed `cargo fmt --check`; the fallback
  engine's immediate 0% event clobbered the "GPU transcription failed —
  continuing on CPU" notice ~1 s after it appeared (suppressed — progress
  resumes with the first completed batch, and only in the failover arm: a
  later channel still shows 0%); "Transcribing — your microphone — 42%"
  double em-dash (detail is now "your microphone (42%)"); silent recordings
  no longer emit a 0/0 event.
- **Tests:** `progress_is_monotonic_and_ends_at_exactly_total`,
  `silent_recording_yields_no_progress_points` (via extracted
  `cumulative_progress`, testable without a whisper-cli binary).

## PR #28 — resilient model downloads

- **Merged (verified):** SHA-256 enforced per attempt regardless of source;
  hf-mirror.com tried only after huggingface.co fails twice;
  `best_installed_asr_model` fallback so an offline machine with weights on
  disk still transcribes.
- **Fixed:** checksum mismatch now skips the source instead of re-downloading
  it (was: up to 4 full downloads from a deterministically-corrupt source;
  now: at most 1 wasted download per source); the producer error string and
  the predicate share `CHECKSUM_MISMATCH_PREFIX` so a reword can't decouple
  them; hf-mirror.com disclosed in MODELS.md (community-run HF proxy operated
  from China; integrity bounded by the pins) with a `FLYONTHEWALL_NO_HF_MIRROR`
  opt-out checked in `candidate_urls` — **default remains mirror-on; the
  maintainer may want to reverse that default** (flagging per instruction);
  live captions use `DownloadEffort::SingleAttempt` so offline users get
  `live:status = unavailable` promptly instead of after ~12 s of retries;
  the installed-model-fallback notice uses display names and no em-dash.
- **Tests:** `candidate_urls_opt_out_disables_mirror`,
  `checksum_mismatch_advances_to_next_source` (+ the PR's own
  `candidate_urls_mirror_only_for_hf`, `best_installed_prefers_largest`).
- **Known gaps (documented, out of remediation scope):**
  - The "X unavailable, transcribing with installed Y" stage detail is
    overwritten by the next stage emit almost immediately — effectively
    logs-only. A persistent-notice mechanism would be its real fix.
  - Pre-existing (inherited from main, kept by the PR's rewrite): concurrent
    `ensure()` calls for the same artifact shared one temp file while hashing
    the network stream, not the disk file — two simultaneous downloads could
    install corrupt bytes. **Fixed on this branch** in a follow-up commit:
    every attempt gets its own `{id}.download.{pid}.{n}` temp path (+ test
    `download_tmp_paths_are_unique_per_attempt`).

## PR #30 — actionable engine/model-download errors + Groq escape hatch

Contains #25 (see below). All nine findings fixed:

1. Notice selection extracted to pure `selectPipelineNotice`/`briefError`
   (frontend/src/pipelineNotice.ts), keyed on the error CONTENT
   ("whisper-cli is not installed" from `ensure_tool`) — Groq failures and
   download errors are no longer mislabeled as engine-missing.
2. Failed in-app engine installs surface inside EngineMissingNotice
   (`installError`); additionally App.tsx tags install errors
   ("engine install failed: …") so non-download failures (e.g. extraction)
   keep the actionable notice (re-review finding, fixed).
3. Groq CTA deep-links to a "groq" Settings focus: Technical forced, Groq
   card rendered even before the toggle is on, scrolled into view.
4. Re-transcription banner applies the same URL-strip/brief treatment.
5. Models-list filter matches `whisper-bin` exactly — `whisper-bin-vulkan`
   is visible again on Windows.
6. `find_on_path` probes `/opt/homebrew/bin` and `/usr/local/bin` on macOS
   (Finder-launched apps don't inherit brew's PATH).
7. devMock gained `engine_installed`/`engine_managed` (+ QA toggles,
   `fotwMockEmit` event hook, a vulkan mock entry).
8. GroqHint is a real `<button>` (keyboard-reachable) styled as a text link.
9. `installEngine` clears only its own `model:progress` entry.

- **Tests:** minimal vitest setup (`npm test`), 12 pure-logic tests over
  notice selection and error briefing.
- **Dev-mock QA:** all nine notice/deep-link/progress states verified in the
  port-1420 dev-mock — see `pr-30-devmock-qa.md` (DOM evidence; the sandboxed
  browser's screenshot capture was broken this session, noted there).
- **Known minor (documented):** on a fresh offline machine the first failure
  is the sherpa download (ensured before whisper), so the engine notice's
  surfaced error text names sherpa — the engine genuinely is missing and the
  CTA is right, but the mixed messaging is worth knowing. Settings' engine
  row doesn't know about an install started from the transcript notice —
  redundant concurrent downloads are possible (now merely wasteful, no
  longer corrupting: see the temp-path fix above).

## PR #25 — close as superseded

#30 is a strict superset of #25's "engine not installed" flow (readiness
flags, notice, install path) with the download-failure and Groq-CTA work on
top. Nothing in #25 is lost by closing it; credit goes in the close note.

## PR #26 — reject and supersede (`feat/managed-mac-whisper-rehost`)

- **Why rejected:** the artifact URL points at the contributor's fork and a
  binary only he built. Everything the app auto-downloads and executes must
  be maintainer-built and repo-hosted (same policy as the Windows Vulkan
  build) — this is a provenance/hosting decision, not a code-quality one.
  The design itself (managed engine artifact + PATH-first resolution) is
  right and is credited.
- **Supersede branch (compiles; deliberately fails closed):** pr/26 merged,
  then: build script pins the whisper.cpp COMMIT and asserts it, fixes the
  shallow-tag reuse path, pins CMAKE_OSX_DEPLOYMENT_TARGET=12.0 (the app's
  minimumSystemVersion), adds GGML_NATIVE=OFF for macOS, hard-asserts
  `lipo -archs` == "x86_64 arm64"; a `workflow_dispatch` GitHub workflow
  (macOS runner) builds, emits the pin, and — gated on a `create_release`
  input — attaches the tarball to a `tools-whisper-v1.9.1` release of this
  repo; models.rs keeps the entry structure with the canonical-repo URL and
  a deliberately invalid placeholder SHA the maintainer replaces with the
  workflow's emitted pin. MODELS.md corrected (real ~2.4 MB size, Linux row
  removed, honest "pending" status). Steps: `pr-26-rehost-checklist.md`
  (on that branch).
- **Re-review catch (fixed on the branch):** the PR's tar packaging line used
  GNU-only `--owner/--group` flags — macOS's bsdtar 3.5.x rejects them, so
  under `set -e` the script died right after a successful compile, on the CI
  runner and local Macs alike (the author presumably had GNU tar installed).
  Now branched per target (`--uid/--gid` on macOS).
- **The arm64 slice has never been executed by anyone** — the checklist
  makes both smoke tests explicit.

## Diagnostics work item (integration branch)

Rolling file logs under `<data dir>/logs` (daily, 5-file cap,
tracing-appender, blocking writes so panic lines reach disk), a startup line
with app version + OS/arch, a panic hook (message + location, then the
default hook), and Settings → Technical → "Open logs folder"
(`reveal_logs_dir`). Ordering hazard handled: the legacy-Looma data-dir
migration runs before the logs dir is created (`state::prepared_data_dir`).
No telemetry; nothing leaves the machine. Tests:
`rolling_appender_writes_into_logs_dir`, `payload_str_extracts_both_panic_shapes`.

---

## What remains mac-unverified (see mac-smoke-checklist.md)

Everything inside `#[cfg(target_os = "macos")]` and every macOS runtime
behavior: the Metal guarded fallback + CPU pinning, the Groq-fallback forced
CPU, live captions on `-ng`, brew-path probing, the managed-engine download
(rehost branch, blocked on the artifact), and the log folder on macOS. The
Windows-visible surface (notices, Settings, progress text, downloads,
diagnostics) was verified here via tests + dev-mock QA.
