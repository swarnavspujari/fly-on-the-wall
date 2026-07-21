# PR #26 rehost checklist — managed macOS whisper engine

Branch: `feat/managed-mac-whisper-rehost` (supersedes PR #26; design credit:
Ian Sumner). The branch **compiles** (`cargo check`) but is deliberately
**non-functional on a clean Mac** until the maintainer publishes the artifact:
the `whisper-bin` entry in the macOS `TOOLS` array carries an invalid
placeholder SHA, so every download fails closed. This is by design — do not
merge before completing steps 1–2, and do not ship without the smoke tests
(the **arm64 slice has never been executed by anyone**; only the PR author's
Intel build of his own artifact has ever run).

## 1. Build + host the artifact — DONE 2026-07-16

- [x] Workflow run 29546886480 (dispatched from main with
      `create_release = true`) — green, including the hard `lipo -archs`
      assert and the pinned-commit check
      (`f049fff95a089aa9969deb009cdd4892b3e74916`).
- [x] Release `tools-whisper-v1.9.1` carries
      `whisper-bin-macos-universal2-v1.9.1.tar.bz2` (2,388,157 bytes).
- [x] Pin taken by independently downloading + sha256-hashing the hosted
      asset (`f9a4bcae555dd3d14f0a8795aad63b8a7a006d59f705bca94e83ef2215805070`)
      and verifying the fat header carries both x86_64 and arm64 slices.

## 2. Pin it — DONE 2026-07-16

- [x] `models.rs` placeholder replaced with the verified sha256/bytes;
      maintainer comment block dropped.
- [x] `docs/MODELS.md` status updated to "hosted and pinned".
- [x] `cargo check` green; committed on this branch.

## 3. Smoke test — Apple Silicon (maintainer's partner's machine, arm64 slice)

This slice has **never been executed by anyone**. On a Mac WITHOUT
`brew install whisper-cpp` (or after `brew uninstall whisper-cpp`):

- [ ] Install the app build from this branch; delete any previous
      `bin/whisper/` under the app data dir so the download path is exercised.
- [ ] Record a ~1-minute meeting with speech from both mic and system audio.
- [ ] Transcribe. Expect: engine auto-downloads (~2.4 MB, progress visible),
      transcription completes, plausible text on both channels.
- [ ] Verify the binary really is the managed one and universal:
      `file "<app data dir>/bin/whisper/whisper-cli"` → "Mach-O universal
      binary with 2 architectures".
- [ ] Verify minimum OS: `otool -l .../whisper-cli | grep -A2 LC_BUILD_VERSION`
      → `minos 12.0` on both slices.
- [ ] Check logs for the decode device (`whisper device:` lines): expect
      Metal init on Apple Silicon.
- [ ] Re-transcribe with Settings → "Use GPU…" toggled off: expect `-ng`
      (CPU) and same transcript quality.

## 4. Smoke test — Intel Mac (PR author's 2019 MBP 16", x86_64 slice + Metal fallback)

- [ ] Same install + download steps as above (remove brew's whisper-cpp first
      so the managed engine is what resolves).
- [ ] Transcribe a short meeting. On this machine (AMD Radeon Pro 5500M,
      macOS Tahoe) the Metal primary is expected to abort — verify the
      "GPU transcription failed — continuing on CPU" notice appears, the
      meeting still gets its transcript, and the next transcription goes
      straight to CPU (pin recorded).
- [ ] `file .../bin/whisper/whisper-cli` → universal, 2 architectures.
- [ ] With brew's whisper-cpp REinstalled, confirm PATH/brew resolution still
      wins over a fresh download when `bin/whisper/` is absent (managed copy
      absent + brew present → no download).
- [ ] Live captions: start a meeting, confirm live text appears and logs show
      no Metal init in the live loop (forced `-ng`).

## 5. Merge

- [ ] Only after 1–4 pass. If any smoke test fails, the failure mode is
      contained: the SHA pin means a bad artifact can't be silently swapped,
      and reverting the models.rs entry returns macOS to PATH-only
      resolution (the PR-30 engine notice covers that state).
