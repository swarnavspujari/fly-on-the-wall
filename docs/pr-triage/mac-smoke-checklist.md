# macOS smoke-test checklist — integration/ian-prs (+ rehost branch)

Everything below is code this Windows machine cannot execute: macOS
`#[cfg]` blocks were desk-checked and unit-tested where possible, but never
run. Two target machines:

- **[AS]** Apple Silicon Mac (maintainer's partner) — also covers the
  never-executed arm64 slice of the rehost artifact.
- **[Intel]** Ian's 2019 MBP 16" (AMD Radeon Pro 5500M, macOS Tahoe) — the
  machine whose Metal aborts; the only hardware that can prove the fallback.

Build to test: a branch build of `integration/ian-prs` (the rehost branch
adds its own checklist, `docs/pr-triage/pr-26-rehost-checklist.md`, once the
artifact exists).

Before starting: Settings → Technical → **Open logs folder** must open a
folder that gains a `flyonthewall.*.log` file — the log lines below are the
evidence for every other item.

## 1. Startup + diagnostics (both machines)

- [ ] Launch the app. The newest log file's first line logs
      `version=… os=macos arch=…` ("Fly on the Wall starting").
- [ ] Settings → Technical → Diagnostics row exists; "Open logs folder"
      opens `<data dir>/logs` in Finder.
- [ ] After a few days of use (or by touching fake `flyonthewall.YYYY-MM-DD.log`
      files), no more than 5 log files remain.

## 2. Engine resolution via brew paths (PR #30 — both machines)

- [ ] With `brew install whisper-cpp` done and the app launched from
      **Finder** (not a terminal — Finder launches don't inherit brew's
      PATH): Settings → Technical → "Transcription engine (whisper.cpp)"
      row shows **ready**.
      Intel note: brew's prefix is `/usr/local/bin`; Apple Silicon's is
      `/opt/homebrew/bin` — this checks both probe paths.
- [ ] `brew uninstall whisper-cpp`, relaunch: the engine row shows the
      manual-install guidance (macOS has no managed artifact on this
      branch), and transcribing a recording produces the
      "Transcription engine not installed" notice — with **no** Install
      button, `brew install whisper-cpp` guidance, and a working
      "Groq cloud transcription" link that opens Settings scrolled to the
      Groq card with the enable checkbox visible.

## 3. Metal→CPU fallback + pin (PR #27 — [Intel] primarily)

- [ ] With whisper-cpp installed and "Use GPU…" ON: transcribe a ~1-min
      recording. Expected on this Intel machine: the Metal primary aborts,
      the banner shows "GPU transcription failed — continuing on CPU", and
      the meeting still gets a full transcript.
- [ ] The notice is NOT immediately replaced by a "0%" progress line
      (PR #29 interaction) — progress resumes only when the first CPU batch
      completes.
- [ ] Transcribe a second meeting: no Metal attempt at all (pin recorded).
      Log evidence: `whisper device:` lines show no Metal init the second
      time.
- [ ] Settings → GPU toggle off→on, transcribe again: Metal is retried once
      (pin cleared), fails, re-pins.
- [ ] [AS] control run: same steps — Metal should simply work, verdict
      stays/goes "gpu", no fallback banner.

## 4. Groq-fallback path forced to CPU (PR #27 remediation — [Intel])

- [ ] Enable Groq with a valid key, disconnect from the network mid-meeting
      or use an invalid key, transcribe: the cloud attempt fails, the local
      fallback runs — and the log's `whisper device:` lines must show **no
      Metal init** (the rescue engine always passes `-ng` on macOS).

## 5. Live captions stay on CPU (PR #27 remediation — both machines)

- [ ] Start a meeting with live captions; text appears while recording.
- [ ] Log evidence during capture: no `ggml_metal` init lines from the live
      loop ( `-ng` forced). [Intel]: previously this could abort — live
      captions surviving a full meeting on this machine is itself the test.
- [ ] Offline machine (Wi-Fi off) with the live model NOT yet downloaded:
      live status reports unavailable within a couple of seconds, not after
      ~12 s of retries (PR #28 single-attempt mode).

## 6. Downloads + notices (PR #28/#30 — either machine)

- [ ] Fresh data dir, transcribe: models download with progress; the stage
      line reads "Transcribing — your microphone (NN%)" — one em-dash, and
      NN reaches exactly 100.
- [ ] Wi-Fi off, wanted model absent but a smaller model installed:
      transcription proceeds with the installed model (log line "model
      unavailable — using installed model instead").
- [ ] Wi-Fi off, no models: "Model download failed" notice with Try again /
      Settings / Groq actions; the shown error has no signed-URL wall.

## 7. Rehost branch (`feat/managed-mac-whisper-rehost`) — AFTER the artifact exists

Follow `docs/pr-triage/pr-26-rehost-checklist.md` §3 ([AS]) and §4
([Intel]) — engine auto-download, `file`/`otool` universal + minos checks,
Metal fallback with the managed binary, brew-precedence.

## Reporting

For each item: pass/fail plus evidence — the relevant transcript, the exact
notice text, and the matching lines from Settings → Technical → Open logs
folder (newest file). On failure additionally: macOS version,
`file <path>/whisper-cli`, `otool -l <path>/whisper-cli | grep -A2 LC_BUILD_VERSION`,
and the last 50 log lines.
