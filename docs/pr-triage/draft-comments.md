# Draft GitHub comments (ready to paste — nothing has been posted)

All texts below are drafts for the maintainer to review, edit, and post by
hand. Nothing in this triage touched GitHub.

---

## PR #34 — merge note

> Merged — thanks @iansumner! You're right that the tier/model interaction was
> invisible; the hint is exactly the fix. Two small touches on top: the helper
> text now matches its siblings' styling (they rely on the section gap rather
> than a negative margin), and it also names the "Maximum quality" checkbox,
> since an explicit model choice overrides that too (`default_model_for_tier`
> is only consulted when no override is set).

## PR #27 — merge note

> Merged — thank you for this one especially, @iansumner. The guarded
> Metal→CPU fallback with the runtime-failure pin mirrors the Windows Vulkan
> guard exactly the way it should, and it fixes a crash class we couldn't have
> reproduced without your Intel-era hardware. While integrating it we extended
> the same idea to two paths the PR didn't reach: the Groq path's local
> fallback engine now honors the stored CPU pin (and always runs `-ng` on
> macOS — it's the last engine in the chain, so it must never gamble on
> Metal), and the live-caption loop now actually stays on CPU on macOS like
> the docs always claimed. Plus a unit test over the pin logic.

## PR #29 — merge note

> Merged — the speech-milliseconds accounting is the honest way to report
> progress and the plumbing through `GuardedAsr` was clean. Thanks
> @iansumner! Integration touches: `cargo fmt` over a few lines (our CI gate
> is strict about it), the fallback engine's initial 0% no longer clobbers
> the "GPU transcription failed — continuing on CPU" notice, the detail is
> formatted "your microphone (42%)" so it doesn't collide with the em-dash
> the UI already inserts, and the batch accounting is extracted into a
> testable helper with monotonicity/exact-100%/silent-recording tests.

## PR #28 — merge note

> Merged — thanks @iansumner! SHA-per-attempt was the right call and made the
> mirror fallback safe to accept. Integration changes worth knowing about:
> a checksum mismatch now skips that source immediately instead of
> re-downloading from it (a deterministically-corrupt source cost up to four
> full downloads before); the hf-mirror.com fallback is now disclosed in
> docs/MODELS.md and has an env opt-out (`FLYONTHEWALL_NO_HF_MIRROR=1`) —
> default stays on; and the live-caption path uses a single-attempt mode so
> an offline machine reports "live unavailable" promptly instead of after
> ~12 s of retries. The installed-model fallback you added already saved one
> of my own test runs.

## PR #30 — merge note

> Merged — this is a big usability win, thanks @iansumner! (It also carries
> everything from #25, which I'm closing as superseded — see there.)
> Integration hardening, mostly in the spirit of your own design: the
> engine-missing notice now keys off the resolver's actual error text rather
> than "any error while the engine is absent", so Groq failures and download
> errors get their own treatment (the selection logic is a pure function with
> a small vitest suite — our first frontend tests, nice side effect); a
> failed in-app install surfaces inside the notice instead of vanishing; the
> Groq CTA now deep-links to a Settings state where Groq can actually be
> enabled; `whisper-bin-vulkan` is visible again in the Windows models list;
> your `/opt/homebrew/bin` observation was spot-on and is included; and the
> notice links are real buttons now (keyboard-reachable). Every notice state
> was verified in the dev-mock.

## PR #25 — close as superseded

> Closing as superseded by #30, which contains this PR's whole
> "engine not installed" flow (readiness flags, actionable notice, install
> path) and builds the download-failure and Groq escape-hatch handling on
> top of it. Nothing here is lost — #30 is merged and credits this design.
> Thanks @iansumner!

## PR #26 — respectful rejection + supersede plan

> I'm going to close this one, but not because of the code — the design is
> right, and I want to land it. The blocker is hosting/provenance policy:
> everything the app auto-downloads and executes has to be built and hosted
> by this repo, the same way the Windows Vulkan build is (maintainer-built
> from the pinned tag, attached to a `tools-whisper-*` release here,
> SHA-pinned in `models.rs`). A binary built on a contributor's machine and
> served from a fork — however much I trust it, and I do — isn't something I
> can ask every future user to execute, and it would also go stale the
> moment your fork moves.
>
> So: `feat/managed-mac-whisper-rehost` supersedes this PR and keeps your
> commits in its history. It takes your build script and registry entry, adds
> a commit pin + universal-slice assert + deployment-target pin to the
> script, adds a `workflow_dispatch` GitHub Action that builds on a macOS
> runner and attaches the tarball to this repo's `tools-whisper-v1.9.1`
> release, and ships the `models.rs` entry with a placeholder SHA that I
> replace with the workflow's emitted pin before merging. Once the artifact
> is up I'd genuinely appreciate a smoke run on your MacBook — your machine
> is the only Intel Mac in this project's orbit, and the checklist in
> `docs/pr-triage/pr-26-rehost-checklist.md` has a section written for it.
> Thank you for pushing this — the macOS dead-end you hit is exactly what
> this closes.
