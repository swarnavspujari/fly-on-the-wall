# PR #30 dev-mock QA log (port-1420 flow)

Date: 2026-07-16 · Branch: `integration/ian-prs` (post-remediation) ·
Driver: `npm run dev` in `frontend/` + the dev-mock IPC stub in a plain browser.

> **Screenshots:** the sandboxed browser pane's screenshot capture timed out on
> every attempt in this session (page interaction, DOM reads, and console all
> worked normally — only the capture pipeline hung). Each state below was
> therefore verified against the rendered DOM/accessibility tree instead, with
> the checks recorded verbatim. To eyeball the states yourself: run
> `npm run dev` in `frontend/`, open http://localhost:1420, and use the
> localStorage toggles + `fotwMockEmit` console hook documented at the top of
> `frontend/src/devMock.ts`.

## How states were driven

- `fotwMockNoTranscript=1` → pre-transcription "Transcribe recording" box.
- `fotwMockEngineMissing=1` / `fotwMockEngineUnmanaged=1` → engine readiness.
- `fotwMockEmit("pipeline:progress", {...})` → pipeline events (errors, progress)
  fired at the real App listeners, exactly as the backend would.

## Verified states

| # | State | How driven | Checks (all true) |
|---|---|---|---|
| 1 | Engine-missing notice, managed OS | engineMissing=1, emit `whisper-cli is not installed — …` error | "Transcription engine not installed" title; explainer copy; **Install engine** + **Set up in Settings** + **Groq cloud transcription** all real `<button>` elements (keyboard-reachable) |
| 2 | Engine-missing, unmanaged OS (macOS/Linux pre-hosting) | + engineUnmanaged=1 | notice shown; **no** Install button; `brew install whisper-cpp` manual guidance; Groq link present |
| 3 | Groq CTA deep-link | click "Groq cloud transcription" in notice | Settings opened; Technical view forced (the technical-only "Use Groq for transcription" checkbox rendered); Groq card rendered with key input **although Groq was off** (`useGroq=false`, tier=balanced); card scrolled into view (`top=318`, inView=true) |
| 4 | Download-failed notice (engine fine) | engine toggles cleared, emit `download failed for … X-Amz-Signature=…` | "Model download failed"; signed URL stripped from shown text; **Try again** + Settings + Groq buttons; engine notice correctly absent |
| 5 | Groq/generic failure NOT mislabeled | emit `groq returned 413 Payload Too Large: {}` | raw error box shown; engine-missing and download-failed notices both absent (the PR's original `engineMissing` predicate would have mislabeled this) |
| 6 | Transcription progress text | emit stage=transcribing, detail=`your microphone (42%)` | banner reads `Transcribing — your microphone (42%)` — exactly **one** em-dash (PR #29 cosmetic fix) |
| 7 | Re-transcription banner | transcript present, emit download error with URL wall | "Re-transcription failed — showing the previous transcript. download failed: status 403" — URL-stripped/briefed; previous transcript still rendered |
| 8 | Settings Models list | Technical view | dedicated "Transcription engine (whisper.cpp)" row with status hint; `whisper.cpp CLI (Vulkan GPU, v1.9.1)` **visible** in the Models list (the PR's prefix filter had hidden it) |
| 9 | PR #34 hint | Technical view | "A specific choice here overrides the Hardware tier and "Maximum quality" settings." rendered, aligned with sibling helper text |

## Unit-test coverage of the same logic

`frontend/src/pipelineNotice.test.ts` (vitest, `npm test`): 11 tests over
`selectPipelineNotice` (engine/download/Groq/stale/unknown/installing) and
`briefError` (URL stripping, 260-char cap, short text untouched). All pass.
