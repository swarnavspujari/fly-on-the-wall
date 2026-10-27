# UX rollout and platform plan: design

Status: **draft for approval** · 2026-10-27
Inputs: the Claude Design handoff `design_handoff_ask_harness_ux` (README, ASK_HARNESS, PROMPT, four `.dc.html` prototypes), a code gap map of the current app, and six research reports (transcription, Ask harness incl. Hamza Farooq's multi-agent course, local models, MCP client, storage, CI/CD). Research notes live in the session scratchpad; the decisions that matter are restated here so this document stands alone.

---

## 0. Summary

1. **Adopt the redesign's structure, not its scope.** The direction is right (setup before the first call, one-row recording bar, visible trust signals, Ask as a docked composer, Settings with one row pattern). But the new Settings drops ~15 shipped controls, four "not built" claims are wrong, and several controls would be inert or unsafe as specified. Section 3 lists every pushback; Appendix A maps every current Settings control to a home.
2. **Ship in five releases**, split by what can be verified in CI and the port-1420 dev mock versus what we must test together on real hardware, real meetings and real cloud folders:
   - **v2.4 (ship now):** lean CI with a path-scoped Claude security review, storage safety fixes, and UX wave 1 (frontend-only).
   - **v2.5 (test together):** Parakeet transcription engine, Silero VAD, Ask harness v1 with validated citations, setup wizard.
   - **v2.6 (test together):** the user-chosen Library folder (Drive / OneDrive / NAS safe).
   - **v2.7 (beta flags):** Connectors (MCP client), ~2 s live captions, screen-clip captions v1.
   - **Later:** Live Assist, the full video player, "search my files".
3. **Transcription:** the engine, not decode settings, is the bottleneck. Parakeet TDT 0.6B v3 scores 11.3% WER on AMI meetings vs ~16% for Whisper large-v3-turbo and runs 15–30× realtime on a laptop CPU; it already ships in the pinned whisper.cpp v1.9.5. Wispr Flow measures AMI 10.6% in independent tests, so Parakeet-class is "Wispr-level" on meeting audio. Names come next (vocabulary spotting + glossary-constrained cleanup).
4. **Ask:** prefetch-then-answer; a model-driven tool loop only where a model has earned it; one read-only tool layer shared by Ask, Enhance and the MCP server; citations are segment ids that the app validates, never the model.
   - It adds a claim check (numbers, dates and names must appear in the cited lines), "not found" as a tested answer, and citations that survive re-transcription. These come from Hamza Farooq's course (4.2a).
   - `llama3.1` stays the default local model unless a challenger beats it on the new harness.
8. **File sizes:**
   - Recordings shrink about 87% with no loss for anything the app computes: 16 kHz FLAC sources, M4A playback, and no stored mix.
   - The installer is already 14–19 MB; first-run downloads shrink by fetching only your tier's models.
   - Model upgrades become a verified, reversible swap (4.6).
5. **Storage:** the database never goes in a user-chosen or synced folder. User content moves to a **Library** of plain files (Markdown, transcripts, compressed audio) that is safe on Drive/OneDrive/NAS; each machine's index is rebuilt from it.
6. **Connectors:** feasible with the official `rmcp` crate, but only after Ask has tool calling, as an off-by-default beta: read-only, confirm every call, MCP only (no CLI wrapping — there's no safe sandbox on Windows).
7. **CI/CD:** path-gated jobs behind one required `ci-ok` check (~45–50% fewer job-minutes), fix the cache thrash, tighten where release secrets are scoped (details in the private CI audit notes), and port the security review from `swarnavspujari/coffee`.

---

## 1. Intent, goals, non-goals, anti-goals

**Intent.** A first-time user should get through their first meeting without poking around; a technical user should keep every model, GPU, key and MCP control. Privacy stays the brand: nothing leaves the machine without an explicit, labeled opt-in.

**Goals**
- Roll out the Claude Design UX with no lost functionality.
- Make transcription faster *and* more accurate, measured on the user's own meetings.
- Make Ask useful with small local models and swappable for Anthropic/OpenAI, with citations users can trust.
- Let users choose where their notes live, including network and cloud folders, without risking the database.
- Make CI cheaper and faster, and add a security review that runs only when a PR needs one.

**Non-goals (this plan)**
- Shared team libraries, hosted sharing, mobile.
- Live Assist "Suggest" mode, write-capable connectors, CLI connectors, searching arbitrary user files.
- Bundling our own LLM runtime (stay on managed Ollama; a llama-server sidecar is a later experiment for install size).

**Anti-goals (a failure even if it technically works)**
- Any Settings control silently disappears.
- A connector or cloud call happens without the user seeing exactly what leaves the machine.
- The SQLite database ends up in a synced or network folder.
- A model change ships on leaderboard numbers without passing FOTW's own WER / Ask harnesses.
- An AI line looks sourced when it isn't.
- Live features degrade the user's actual Zoom/Teams call.

---

## 2. Invariants every PR must keep

From `security-hardening-2026-10`, `ARCHITECTURE.md` and the gap map:
- `build_provider` is the only LLM constructor; cloud consent and per-note/folder **Keep local** are enforced there (`llm_commands.rs:165-248`).
- Webview-settable keys stay limited to `consent.` / `capture.` / `ui.` (`commands.rs:59-66`); anything else gets a dedicated command.
- CSP and nav guard unchanged; asset protocol scope stays `recordings/` + `attachments/` until the Library phase moves it deliberately.
- `devMock.ts` mirrors every new command so port-1420 QA keeps working.
- The MCP binary never holds keys or calls a cloud LLM.
- On macOS, the system-audio probe that triggers the TCC prompt runs only after consent is accepted (`App.tsx:208-226`).
- Calendar-seeded attendees stay **unconfirmed** until the user confirms them; only confirmed rosters set `num_speakers` (`pipeline.rs:28-72`, `recording.rs:105-108`).
- Sentence case, "you", violet primary, yellow only as a highlight with ink text; tokens from `index.css`, primitives from `ui.tsx`.

---

## 3. Screen-by-screen review

Format per screen: **Agree**, **Push back**, then build notes. Sizes: S < ½ day, M 1–2 days, L 3+ days.

### 3.1 Setup (`FirstRunNotice.tsx`)
**Agree.** This is the single biggest win: the model download, system-audio check and consent move out of the first live call.

**Push back**
1. **Order breaks a macOS invariant.** Sound check (step 2) runs before Consent (step 5), but the TCC-prompting probe is gated on consent. New order: **Welcome → Consent → Sound check → Models → AI notes → Calendar**.
2. **"Skip" must not skip consent.** Skip jumps to Consent, or the current consent modal appears on first Record.
3. **Missing "AI notes" step.** The first moment of value is Enhance, which needs a local model (managed Ollama install, ~1.5 GB runtime + model) or a cloud key. Add a step: "On this computer (download) / Use a cloud provider (key, labeled) / Later". The managed-Ollama block already exists in Settings; reuse it.
4. **"Best quality · Needs 32 GB RAM" is wrong** and contradicts the Settings spec ("6 GB RAM"). Today Balanced and Best use the same model unless Maximum quality is on (`hw.rs:130-139`). Tier copy must come from the backend (a `models_for_tier` command), because Phase 2 changes the engine.
5. **Sound check is L, not a copy change.** No level-metering API exists. v1: mic level meter plus "play a test tone and detect it on the loopback channel". Copy differs per OS (Windows failures are muted output / Bluetooth hands-free; macOS is TCC or a stale grant).
6. **"Download paused (offline)" needs resumable downloads** (today: retry from scratch, `models.rs:530-553`). v1 copy: "will retry when you're online".

### 3.2 Home (`NoteList.tsx`, `Sidebar.tsx`, `App.tsx`)
**Agree.** Date grouping, status chips per meeting, empty state with all three entry points, undo toast instead of `confirm()`, Up next card, no-calendar prompt, expired-calendar state.

**Push back**
1. **The privacy pill can't be global-only.** Per-note and per-folder Keep local exist (`Editor.tsx:737-763`, `Sidebar.tsx:325-348`). Copy: "On this computer" / "Cloud on · Anthropic" with a hover list of what uses it (Ask, Enhance, transcript cleanup, action items) and "except notes you keep local".
2. **Ask needs a home outside a note.** The highest-value question is cross-meeting ("what did Acme say about pricing?"). Phase 3 adds a global Ask entry. Ctrl+K opens Settings today; leave it alone in Phase 1.
3. **"Bring notes from another computer" has no backend** (heal exists, export doesn't). Hide it until the Library phase, where it becomes "Open a Library".
4. Up next shows only events with a join URL (`Sidebar.tsx:163-166`); keep that rule and say so in the empty copy.
5. Undo toast = **deferred delete** (the delete command runs after the toast expires; undo cancels). No soft-delete schema change.

### 3.3 Recording (`RecordingBar.tsx`)
**Agree.** One fixed-height row, warnings become a chip, "Copy recording notice" in the bar, **"Stop and transcribe"**.

**Push back**
1. **Live health meters need peak metering in three capture backends (L).** v1 drives You/Others chips from the existing `status.warnings` (muted output, loopback never started, macOS silence). Real meters come in Phase 4 with the sound-check API.
2. **The speaker card must not auto-confirm calendar attendees.** Wrong counts merge real voices; that's why rosters stay unconfirmed. The card offers "Looks right" (confirms) and edits. "Just a count" has no data field (`Meeting.attendees` only); defer it.
3. **"Diarization runs async and doesn't block the transcript" isn't a UX change.** The transcript is saved only after alignment; decoupling needs a two-phase save (L). Deferred.
4. Keep the "participants notified" state. "Remind me" defaults on.

### 3.4 Note: Notes / Transcript / Enhanced
**Agree.** One legend instead of per-block "AI" labels; **unsourced AI lines visible without hover** (the most important trust signal); hover preview of the cited quote; the template's sections as ghost headings; a placeholder that teaches the model.

**Push back**
1. **Keep / Remove on an unsourced line:** Keep converts the block to yours (the existing rule: editing an AI block flips it to `user`). No new data type.
2. **"Unsourced" isn't truthful yet.** Enhance silently drops source indexes that don't resolve (`enhance.rs:208-232`), so "model gave no source" and "model gave a bad source" look the same. Phase 3's validator fixes this; Phase 1 ships the visual.
3. **Images inline:** the safe renderer allows them, but there's no paste/drop-to-attach path (M-L). It ships after Phase 1 using the existing attachments API; the Library phase moves files later.
4. Screen-clip tiles render only in the Transcript view (`Editor.tsx:1459-1461`); show them in every view.
5. "New name updates everywhere" misses `meeting_items.owner` snapshots (`items.rs:40-52`). Fix in Phase 3 (relabel updates item owners by speaker key).

### 3.5 Ask (`AskPanel.tsx`)
**Agree.** Composer docked by the view switcher; answers in a sheet over the note; citations; collapsed step trace; nothing writes without a click; the chat itself stays ephemeral.

**Push back**
1. **There are no citations today.** `ask_meeting` returns a bare string built from the whole transcript with no segment ids (`llm_commands.rs:546-607`). Citations are Phase 3 backend work (L), not a UI change.
2. **Runs on: "This computer / Anthropic" regresses OpenAI, NVIDIA NIM, custom OpenAI-compatible endpoints and the Claude model picker.** Use a select, keep every provider.
3. **"Smaller (single-step) / Larger (multi-step)" exposes harness internals.** Replace with capability detection plus "Recommended for this computer" and a Custom model field.
4. **"Show the steps Ask took" switch:** drop it. The trace is always present and collapsed.
5. **Recipes are the same thing as Enhance templates** (saved prompts). Template CRUD already exists and the redesign gives it no home. One concept: **templates**, used by Enhance and by `/` in Ask.
6. **Insert into note should create an AI block with sources**, not append plain text to the scratchpad (today provenance is lost, `Editor.tsx:1159-1164`).
7. **The answer sheet covers the note you insert into.** Allow pinning it to the side on wide windows (≥1440 px).
8. **"Also search my files" is deferred.** It needs document parsers and widens the privacy surface; separate track.

### 3.6 Import (`ImportQueue.tsx`)
**Already built** (per-file stages, cancel, retry). Two fixes:
- Conversion failures fail the whole batch (`import_commands.rs:393` uses `?`); attribute them per file.
- The copy contradicts itself (MOV is listed as supported, then "format not supported").

### 3.7 Notices
**Agree**, and the overlap is a real bug today: four components anchor to bottom-right/left with different z-indexes. One `NoticeStack` with priority (system audio > pipeline error > update) and a "+N more" count; each notice reads cause → numbered fix → collapsible details. Copy is per OS. `UpdateBanner` finishes its migration off legacy aliases.

### 3.8 Settings (use the Rethink version)
**Agree.** Full-window overlay that returns you where you were; 220 px nav with search and scroll-spy; one row pattern; per-group "More options" instead of the global Simple/Technical toggle; "Where your data goes" tiles at the top; cloud badges on the row where you turn cloud on.

**Push back**
1. **About 15 shipped controls have no home**, including audio retention, two-step delete-all, the disk-encryption notice, note templates, auto-transcribe, the models-on-disk list, managed Ollama, OpenAI/NIM, per-calendar Up next toggles, index backfill and Open logs folder. **Appendix A maps every one.** Net structure: seven sections, with "Your files" becoming **"Your files and privacy"**.
2. **The location shown, `~/Documents/Fly on the Wall`, is wrong.** Data lives in `%APPDATA%\FlyOnTheWall`. Documents is the wrong default anyway: with OneDrive Known Folder Move (true on this machine) it is silently cloud-synced. The Library phase sets the default to `~/Fly on the Wall`.
3. **"Words to listen for" would be inert.** The app passes no prompt, and Whisper prompt biasing is a measured no-op under the mandatory `-mc 0` hallucination guard. It ships in Phase 2 with a real mechanism.
4. **"Language: Detect automatically" destabilizes decoding** (code comment, `pipeline.rs:254-262`), and `asr.language` isn't webview-settable. Hold until Phase 2 routes non-English audio to Whisper deliberately.
5. **`large-v3-turbo · ~2.9% WER` misleads.** That's a clean-read-speech number; meetings run 10–30%. Show model name, size and a speed estimate, no WER.
6. **"Save recordings as … Opus"**: recordings are WAV only. Compressed playback audio is a Library-phase format change, measured against ASR first.
7. **"When a calendar meeting starts: Record"** conflicts with the consent model and needs notification/autostart plugins. Ship **Nothing / Ask me** only.
8. **"Set up" for Claude Desktop must not edit its config.** Claude Desktop on Windows is MSIX with virtualized AppData, so an outside write can fork or be lost. Show the snippet with the correct per-OS path, "Copy" and "Open config folder", plus instructions for Claude Code, Cursor and VS Code.
9. **"Done" implies live-apply, but ASR settings save all-or-nothing** (`set_asr_settings`). Phase 1 keeps an explicit save bar when the Transcripts group is dirty; Esc with unsaved changes asks to discard.
10. **The "Ask and Enhance" tile undercounts.** Transcript cleanup and action-item extraction also run on the chosen provider, automatically after transcription. The tile and badge copy must say so.

### 3.9 Screen recordings with captions
**Agree with the core**, which is high value per effort because cue id = segment id and the VTT is already regenerated on every save (`model.rs:312-338`, `transcripts.rs:285-288`).

**Push back and build notes**
1. Store `{screen_offset_ms (signed), meeting_id}` on the attachment. Attachments are a JSON blob, so `#[serde(default)]` needs no SQL migration. A note can hold several meetings, so the offset alone isn't enough.
2. **Sample the offset at first frame, not after `start()` returns.** WGC can take up to 1.5 s to start; first-frame needs a hook on the `ScreenSession` trait. Pausing audio mid-clip freezes the audio clock while video continues, so store pause intervals or mark the clip "out of sync after 12:30".
3. **Build cues from transcript JSON with `VTTCue`, not `fetch()`.** CSP `connect-src` blocks asset URLs.
4. **Imported videos have their own audio**: mute the meeting audio for them. Use a fixed offset of 0 plus the nudge, and skip cross-correlation.
5. **"macOS isn't supported" is stale.** Full-screen capture works on macOS; window/region don't.
6. **The YouTube-grade player is L for a niche surface.**
   - v1: CC toggle, synced `recording.playback.wav`, ±0.5 s nudge, side transcript with click-to-seek, caption styling, basic keys (Space/K, J/L, C, F).
   - v2: chapters (no data source exists; would need LLM extraction), hover preview frames, speaker strip, PiP mini window, double-click zones, speed/size menu.

### 3.10 Live transcript and Live Assist
**Agree** that live text at ~2 s with in-place firming is worth building (Phase 6). **Live Assist is deferred**:
1. Running streaming ASR, question detection, retrieval and an LLM next to Zoom/Teams on an 8 GB laptop risks the call itself. A hard CPU budget (≤25% during a call) gates every live feature.
2. **The mock shows named speakers live.** Reality is You / Them (or the one calendar attendee) until diarization; streaming diarization (Sortformer) is a later experiment.
3. **"Nothing from the call is shared" is inaccurate.** The query is derived from the call. Copy: "Sends only this query to Salesforce."
4. It integrates three tracks (live ASR, the Ask index, connectors), so it comes last.

---

## 4. Exploration verdicts

### 4.1 Transcription: faster *and* more accurate
- **Engine.** Add a Parakeet TDT 0.6B v3 `TranscriptionEngine` for English, beside `WhisperCppEngine` (`crates/fly-asr/src/lib.rs:233`).
  - Path A: the `parakeet-cli` sidecar already in the pinned whisper.cpp zip. It needs a parser for token `word_start` flags; there's no `-oj`.
  - Path B later: in-process `parakeet-rs` (ort is already a dependency). This also removes today's per-batch reload of a 574 MB model.
  - Whisper stays for non-English and as the `GuardedAsr` fallback. Groq stays Whisper.
- **VAD.** Replace the energy VAD (`fly-audio/src/vad.rs:31-43`) with Silero v6.2, keeping an energy floor for digital-silence loopback.
- **Live.** Replace the 5 s tick / ≥8 s chunk loop (`live.rs:20-25`, ~13–15 s to first text) with the Nemotron speech-streaming EN 0.6B transducer via sherpa-onnx ≥ 1.13.4 (we ship 1.13.3 for diarization; older versions decode badly). It does 6.9% avg WER at 1.12 s chunks. Two passes stay: live rough text, then the final diarized transcript.
- **Names (the Wispr part).**
  - CTC keyword spotting with Parakeet-CTC-110M, fed from calendar attendees, note text and a user glossary.
  - The existing `polish_transcript` pass gets the glossary under an **edit-only, proper-noun-substitution contract** (edit distance ≤ k to a glossary term, every substitution logged).
  - This is what "Words to listen for" becomes.
- **Later top tier.** Cohere Transcribe 2B, Granite Speech 4.1 2B or Qwen3-ASR-1.7B for GPUs. They're another ~3 points better on AMI but 6–15× slower, and need forced alignment for timestamps.
- **GPU vs CPU.** The GPU stays preferred wherever it's faster. Parakeet uses the same ggml backends as Whisper (Vulkan on Windows, Metal/CoreML on Apple Silicon). The per-machine speed test (`gpu.rs`) keeps choosing GPU or CPU. "15–30× realtime" is the CPU floor, which matters because many users have no usable GPU. Parakeet-on-Vulkan was only tested upstream on CUDA and CPU, so the RTX 3050 run is a ship gate.
- **Google's 2026 voice models (checked 2026-10-27):**
  - **Gemma 4 E2B/E4B/12B** (April 2026, Apache-2.0) accept audio and can transcribe on-device. They are LLM decoders, though: no word timestamps for diarization, slower, and prone to the hallucination class Parakeet avoids. Bench as a curiosity only.
  - **Gemini 3.5 Live Translate** (June 2026, [Google](https://blog.google/innovation-and-ai/models-and-research/gemini-models/gemini-live-3-5-translate/)) and **Gemini 3.8 Live** (Sept 2026, secondary sources) are cloud speech-to-speech models for voice agents and translation. They're not note-taking ASR; at most a future opt-in cloud live-caption engine, labeled like Groq.
  - **"Embedding Gemma Transcribe"** (Oct 13, 2026) is an independent project, not Google's. It reuses EmbeddingGemma 2's audio encoder and claims an hour of audio in ~13 s on an M3 Air at ~5% WER (dataset unstated, English only). Unverified; add it to the harness watch list.
  - None of these displaces Parakeet for FOTW today.
- **Measure first.**
  - Extend `src-tauri/tests/accuracy_harness.rs` to run alternate engines, with a Whisper-style normalizer.
  - **Corpus** (local-only in `fotw-design-2026-10\corpus\`, never committed). The original fixtures were deleted in October 2026, so it's rebuilt in three layers:
    1. **AMI** (public, human transcripts + RTTM, CC-BY-4.0): 3–4 meetings, distant-mic and headset mixes. This gives *true* WER and DER on meeting audio.
    2. **2–3 new Teams meetings** (recording + Teams `.vtt`, supplied by the user). This gives *relative* WER against Teams' own ASR and speaker DER in your real conditions.
    3. **A 15-minute gold excerpt:** one new meeting recorded on the built-in mic with laptop speakers (the hardest echo case), with a draft transcript the user hand-corrects (~1 hour). It replaces the lost human-verified gold.
    - Your 27 existing meetings are used read-only for RTF, hallucination loops, duplicate rate and engine agreement.
  - **Ship gates:**
    - no WER regression on any corpus item (AMI, the Teams meetings and the gold excerpt each count);
    - speaker-attributed WER ≤ baseline;
    - ≥ 5× realtime on the Balanced CPU;
    - live first partial ≤ 2 s;
    - CPU ≤ 25% during a call.
- **Second-order effects.**
  - Parakeet's frame-exact timestamps change cross-talk dedup timing: re-tune `window_ms`.
  - The sherpa upgrade must not regress DER baselines.
  - Store engine + model in transcript metadata.
  - Old transcripts stay as they are; re-transcribe is opt-in.
  - The NVIDIA Open Model License on Nemotron needs your OK (Moonshine v2, MIT, is the fallback).

### 4.2 Ask: agentic RAG that works on small models
- **Shape** (Section 5 of the harness report; aligns with the course's harness principles: stream the first token, one durable record per unit of work, declare budgets, a tool is a contract, ground every claim, fail loudly at seams):
  1. **Scope:** this meeting / @meeting / @person / @folder / all. With a cloud provider and a wide scope, Keep-local meetings are excluded inside the SQL and the trace says "N kept-local meetings not searched".
  2. **Deterministic prefetch:** hybrid FTS + vector → top-8 **passages** (speaker-turn windows of ~160 words, carrying segment ids), plus extracted items and the scratchpad. That's ~2.5k tokens instead of today's whole transcript, which also fixes the silent front-truncation of long imports at `num_ctx` 32768.
  3. **Tier gate by measured capability, not a Settings dropdown:**
     - S (≤8B local): 0 model-chosen tool calls.
     - M (local with tools that passed a smoke eval): ≤ 3 calls / 2 rounds.
     - C (cloud): ≤ 8 calls / 5 rounds / 40k input tokens.
  4. **Synthesis** in prose with `[seg:ID]` markers, streamed, with no grammar constraint (format constraints tax reasoning; qwen collapsed to `[]` under the Enhance grammar).
  5. **Validate:** a marker counts only if it's in the retrieved set. Sentences without a valid cite are styled unsourced; numbers, dates and attributed quotes without a cite are flagged. Zero valid cites → one retry → otherwise shown fully flagged.
  6. **Proposals** (`insert_into_note`, `add_action_item`) are validated and rendered as buttons, never executed by the harness.
  7. **A durable run log** per answer.
- **Shared layer.** A new crate `crates/fly-context` (fly-core + fly-storage only) with `search_passages`, `read_segments(around_segment_id, before, after)`, `list_meetings`, `get_event`, `get_note`, `get_items`. The MCP server, Ask and Enhance all wrap it. The MCP keeps its existing tools stable and adds a passage-level search.
- **Retrieval fixes.**
  - Store `segment_ids` and `end_ms` on `embedding_chunks`.
  - Add a per-segment FTS table.
  - Index the polished transcript.
  - Embed text **without** speaker labels, so a rename costs zero re-embeds.
  - Fuse at the passage level.
  - Add extracted items as a third ranked list.
- **Providers.** Extend `fly-llm` rather than replace it:
  - content blocks, `tools`, `tool_choice` (auto/none/required), `ChatResponse` with `stop_reason`/usage, and `chat_stream()`;
  - capability probes;
  - keep `chat() -> String` as a shim.
- **Known provider quirks.**
  - Ollama has no `tool_choice`.
  - Ollama: never send `format` and `tools` together (#18944).
  - Ollama: always send `think` explicitly (#10976).
  - Anthropic Opus/Sonnet 5.5 and Fable 5.1 reject forced tool choice.
  - Anthropic search-result blocks give free citations on cloud.
- **Models: `llama3.1` stays the default** (all challengers must beat it on FOTW's own harness).
  - **July bench recap** (`docs/BENCHMARKS.md` §10–11):
    - llama3.1 with the engineered harness: 27/27 contracts, 11 s Ask, 0 empty answers.
    - qwen3.5:4b: better note quality (Enhance judge 3.33 vs 2.56), but 81–90 s per Ask and 4/30 empty answers.
    - Every local model corrupted spoken numbers ("eighty four hundred" → $84k).
    - That bench tested the *old* Ask (whole transcript, no tools).
  - **What the new design changes:**
    - Tier S makes zero model tool calls, so llama3.1's weak tool calling (independent BFCL v4 25.8) doesn't matter there.
    - Prefetching ~2.5k tokens instead of the whole transcript is the bench's own "lever (b)" and should cut latency and number errors. That is to be measured, not assumed.
  - **Challengers:**
    - Tier S (8 GB): `qwen3.5:4b` (think off) and `granite4.1:3b` against llama3.1, on quality, latency and empty-answer rate.
    - Tier M (16 GB, tool loop): `qwen3.5:9b` vs `gemma4:12b-it-qat`.
    - 32 GB+/GPU: `qwen3.6:35b` (A3B) vs `gemma4:26b-a4b`.
  - Embeddings: keep `embeddinggemma:300m`.
- **On the designer's model claims.** The Foresight and EmbeddingGemma 2 claims check out (Foresight is a direct competitor). The Gemma 4 τ2 numbers are retail-only scores for the thinking variants: cross-domain averages are 76.9 / 68.2 / 69.0 (12B) / 42.2 / 24.5. Gemma 4 on Ollama with `think:false` also returns empty replies after a tool result (#18861), which is exactly FOTW's setup.
- **Evals.**
  1. Retrieval gold set (≥ 30 questions with expected segment ids, recall@8 ≥ 0.85) runs in CI without a model.
  2. Harness contract tests with `MockProvider` run in CI.
  3. Local model comparison extends `llm_bench.rs`.

### 4.3 Connectors: FOTW using MCP servers and CLIs
- **Yes for MCP, later, as a beta.** Use the official `rmcp` 3.8 (pinned) for stdio + Streamable HTTP + OAuth.
  - Remote OAuth servers are the critical path: HubSpot and Notion are hosted, and Salesforce's local DX server reaches end of life on 2026-11-02.
  - New crate `fly-mcp-client`. The server crate is untouched.
- **Prerequisite:** Ask tool calling (4.2).
- **Why not sooner:** FOTW combines private data, attacker-controllable input (speech) and, with connectors, an outbound channel (the "lethal trifecta").
- **Day-one rules:**
  - approval bound to a hash of the server config;
  - each tool definition pinned by hash (on change: disable and show the diff);
  - annotations can only make a tool stricter, and tools are treated as writes until you mark them read-only;
  - exact, editable arguments on every call;
  - a connector result can't trigger another proposal in the same turn;
  - results fenced, schema-validated and size-capped;
  - config editable only from Settings;
  - https only, with private IPs blocked during OAuth discovery;
  - no sampling or roots.
- **Keychain and privacy details:**
  - Keep-local notes block all connectors.
  - "Delete all my data" must also clear connector keychain entries.
  - Windows keyring caps entries at ~1,280 characters, so access and refresh tokens are stored separately.
- **CLIs: no.** Allowlists aren't a security boundary, and there's no practical sandbox on Windows yet.
- **Effort:** about 5–7 engineer-weeks after the tool loop.

### 4.4 Where notes are stored
- **Split storage.**
  - **App-local** (moves from Roaming `%APPDATA%` to `%LOCALAPPDATA%\FlyOnTheWall`): DB + WAL, indexes, models, sidecars, logs, capture staging, pipeline scratch.
  - **User-chosen Library:** `Notes.md` with frontmatter, `Transcript.md`/`.vtt`, compressed `Audio.m4a`/Opus, attachments, and a hidden `.fotw/` folder of JSON per meeting.
  - The app publishes only finished meetings (temp → verify → rename, with `.fotw/meeting.json` written last as the commit marker).
  - Each machine rebuilds its index from `.fotw/*.json` without opening audio.
  - Conflicts never overwrite: the losing edit becomes a conflict copy and the meeting is badged.
  - v1 supports one user on several devices and is one-way (see call 17 in Section 8).
- **Layout:** `Meetings/2026/2026-10/2026-10-27 1000 Weekly sync — Acme/`, with the physical layout by date only. App folders and series are metadata, so filing a note never moves gigabytes. Names follow OneDrive's rules, are NFC-normalized and stay within a 240-character path budget.
- **Why it matters now.**
  - 41 GB of WAVs for 27 meetings on this machine (~1.1 GB/hour).
  - Meeting material is scattered across three trees.
  - There's no whole-library export, and portable notes drop attachments and screen recordings.
- **Ship these safety fixes first (v2.4), before any Library work:**
  - a fresh database must not run the v2 orphan sweep (`migrations.rs:195`), which would mass-rename every folder;
  - atomic mirror writes;
  - the startup backfill must stop overwriting files from the DB (`recovery.rs:197-256`);
  - persistent WAL;
  - scratch and `live-tmp` go to the local cache.
- **Past corruption is not OneDrive's doing.** Roaming AppData isn't redirected. The confirmed cause was a mispaired DB and WAL under Claude Desktop's MSIX overlay. FOTW itself ships NSIS, not MSIX.

### 4.5 CI/CD and the security review
- The repo is public, so runner minutes are free. The waste is feedback time, macOS queueing and cache churn.
- **Pipeline changes:**
  - path-gated jobs (`changes` → `fmt`, `frontend` incl. **vitest, which runs nowhere today**, `rust` matrix with clippy on all three OSes, `audit`, `workflows` with actionlint + zizmor) behind one required `ci-ok` gate;
  - rust-cache saves on main only;
  - stub the MCP sidecar in CI instead of release-building it;
  - never cancel runs on main;
  - timeouts on every job;
  - Dependabot cooldown, with actions moved to monthly.
- **Security review**, ported from `coffee`:
  - `anthropics/claude-code-action` on Opus 5.5 via Workload Identity Federation, read-only tools, report-only;
  - runs when changed files match `paths.txt`, incrementally since the last review;
  - skips drafts and Dependabot; forks get a neutral notice; the label `security-review` forces a full review;
  - the scope script runs **from the base commit** so a PR can't skip itself;
  - estimated $5–30 a quarter.
- **Risk fixes that need you in GitHub settings:**
  - release-secret scoping (see the private CI audit notes);
  - make `ci-ok` the required check;
  - turn on Dependabot alerts;
  - add a notary preflight call (v2.3.0 and v1.7.0 failed at notarization after a full build).

### 4.2a Production RAG: what Hamza Farooq's course adds
From Module 1 (Alex Perplexity clone, harness principles, Lumina quality rules), Module 3 (Agentic RAG, Evaluation and Guardrails, Semantic Cache, Knowledge Graphs, Moment RAG), FDE Assignments 1–4, Module 5 voice benchmarking, and Module 4 Rag_react/MCP. The course is strongest on *proving* a system works. FOTW is already ahead of its demo code on fencing, retrieval-time filtering and read-only MCP. Changes adopted into the Ask plan:

1. **"Not found" is a first-class, tested answer.**
   - A retrieval score floor: below it, Ask says "not found in your meetings" and cites nothing.
   - A **claim check**: every number, date and proper name in a cited sentence must appear in the segments it cites. This is cheap, deterministic, and aimed squarely at the spoken-number corruption the July bench found.
   - ≥ 8 unanswerable questions in the gold set.
2. **One benchmark command with thresholds committed *before* the first run.**
   - It exits non-zero on any miss and compares against a committed baseline.
   - Judges report but never block.
   - Cost and latency are reported next to quality.
3. **Contention benchmark** (the single-machine version of the course's ARGUS gate):
   - Ask p95 (retrieval, first token) while recording + live captions + an embedding backfill run;
   - caption lag p95 while an Ask runs.
   - This sets an explicit CPU priority: captions > Ask > backfill.
4. **Deterministic self-query router before the prefetch.**
   - Extract dates, speakers/attendees and series/folder from the question.
   - Validate each against known values, dropping anything invented.
   - Soft-fallback when a filter leaves fewer than 3 hits.
   - Route list and count questions ("open items for Dana") to typed `meeting_items` queries, not passage RAG. This is the knowledge-graph lesson without model-written SQL.
5. **One proposal gate for every write**:
   - a per-tool tier table (auto / review with expiry / blocked);
   - an idempotency key and an audit row;
   - a visible structured refusal, never a silent no-op;
   - red-team cases where injected speech tries to create proposals.
6. **Citations that survive re-transcription.**
   - Re-diarize keeps segment ids (`pipeline.rs:720`), but re-transcribing builds new ids (`align.rs:52` `new_id()`).
   - Every citation (Enhanced blocks, items, Ask inserts) stores `(meeting_id, seg_id, start_ms, end_ms, text_hash)`. After a re-transcribe, a missing id re-anchors by time overlap plus text similarity, or shows "source changed — check".
   - This is required before re-transcribe-with-Parakeet is offered.
7. **Run-log hardening:**
   - an allowlisted typed schema and never raw provider error strings (they can contain content);
   - per-stage timings and per-call tokens/cost;
   - a test asserting the log holds no free text.
8. **Deeper candidate pool, then diversify:**
   - fuse ~100 candidates and cap passages per meeting before taking the top 8;
   - test a tiny cross-encoder rerank at top-20 and ship it only if recall@8/MRR improves on CPU budget.
9. **Query decomposition (2–4 sub-queries, original kept) on tiers M and C only.**
10. **Stream order: trace → citations → answer.** Sources render the instant prefetch finishes; passages are clipped (~750 chars) for a faster first token.
11. **No semantic answer cache.** Meeting corpora change constantly, keep-local and cloud need separate partitions, and "last week" is the norm. If one is ever added, key it on scope + tier + corpus generation and copy the course's leak test.
12. **Tool-loop hygiene** (tiers M/C):
    - strip text the model writes after a tool-call marker;
    - truncate observations;
    - unknown tool → corrective message;
    - cap/error is its own UI state;
    - one deterministic re-query before accepting "not discussed".
13. **Security in code, not prompts** (fits FOTW's existing posture):
    - test that *every* retrieval path, including tool calls, goes through the Keep-local SQL filter;
    - validate every model-proposed argument (meeting ids, segment ids, speakers) against the scope or retrieved set;
    - deterministic output guards (claim check, link/image block, proposal gate) instead of a CPU-heavy guard model on the small tier;
    - MCP read-only enforced at the DB connection (`query_only`), never sampling;
    - any future redaction before cloud calls is synchronous and fail-closed;
    - an Ask cancel button, a per-answer token budget, and an optional daily cloud-spend cap.
14. **Eval additions:**
    - MRR and must-cite sets alongside recall@8;
    - citation-validity 100%, an unsourced-sentence threshold, claim-check pass rate and correct refusals;
    - red-team transcript fixtures (asserting no proposal, no rendered link, and zero keep-local text in a cloud request);
    - real-model outputs recorded and replayed in CI through MockProvider;
    - cold vs warm p50/p95;
    - a kill-mid-answer/backfill resilience test.
15. **From the meeting starter projects:**
    - a `tentative` flag on `meeting_items` ("flag it rather than fabricate specificity");
    - explicit null owners: an owner is a cited speaker or null, never inferred.

### 4.6 File sizes, download sizes, and model upgrades

**Recordings.** Today every meeting writes four 16-bit mono WAVs (`cpal_backend.rs:437-493`, `mix.rs:9`), ≈ 1.15 GB per hour:
- `mic.wav` and `system.wav` at the device rate (~48 kHz): ~345 MB/h each;
- `mixed.wav` at 16 kHz (the transcription input): ~115 MB/h;
- `playback.wav`: ~345 MB/h.

Two of the four are derived. Every model in the pipeline (Whisper, Parakeet, sherpa diarization) works at 16 kHz.

**Proposed recording storage v2 (no quality loss for anything the app computes):**
1. Keep the two source channels as **16 kHz FLAC**, made with the same resampler the pipeline already uses, so the transcription input is bit-identical to today's (~60 MB/h per channel; FLAC ratios vary with background noise, to be measured).
2. Keep playback as **M4A/AAC** (~64 kbps stereo, ~29 MB/h), encoded from the full-rate originals so listening quality stays high. AAC plays natively in WebView2 and macOS WebKit; Opus is smaller but Safari support is uneven (test both).
3. Stop storing `mixed.wav`; regenerate it from the FLACs on demand (re-transcribe, re-diarize).
4. Delete the full-rate WAVs once the transcript is saved, unless "Keep original recordings" is on.
5. Result: ~150 MB/h instead of ~1,150 MB/h, about 87% smaller. On this machine, 41 GB becomes about 5–6 GB.
6. A one-time "Compress existing recordings" job in Settings shows the estimated saving, runs in the background, verifies each file before deleting the WAV, and is resumable.
7. Encoding and decoding use the bundled ffmpeg sidecar (no new native dependency). FLAC decoding in Rust uses `claxon` for the pipeline.
8. An "Even smaller" option (Opus ~24 kbps per channel, ~20 MB/h total) is offered only if the WER harness shows no regression on your corpus.

This changes the stored format: manifest file names, heal/backfill, re-transcribe, the asset scope for playback, and export. It ships with a compatibility reader so old WAV meetings keep working.

**Install and download size.** The installer is already small: 14 MB on Windows, 19 MB on macOS for v2.3.0. The weight is in first-run downloads:
- speech engine + model: ~0.3–1 GB per tier;
- managed Ollama runtime: ~1.4–1.5 GB;
- the chat model: 2–5 GB;
- the embedding model: ~0.6 GB.

Levers, in order:
1. Download only the current tier's models; today's per-machine `models/` can hold several Whisper variants (2.3 GB on this machine).
2. Garbage-collect superseded models after a successful upgrade, with a "Free up X GB" row in Settings.
3. Parakeet int8 is lossless versus fp16 and about the same size as the Whisper turbo it replaces.
4. Smaller default chat models only if they win the bench.
5. Experiment (after v2.6): a pinned `llama-server` sidecar (~35 MB Windows Vulkan, ~16 MB macOS) instead of the ~1.4 GB Ollama runtime. It would also give a rerank endpoint. Cost: we manage model files ourselves and lose Ollama's MLX path on Apple Silicon.

**Model upgrades: a safe swap, never a silent switch.** "Model manager v2" ships in v2.5, because the Parakeet rollout needs it:
1. **Manifest:** each app release carries a versioned model manifest (`id, version, url, sha256, size, tier, engine, min_app_version`).
2. **Download:** in the background, resumable (HTTP Range), checksum-verified into a staging folder, and never during a recording or a pipeline run.
3. **Smoke test before activation:** a bundled 10-second clip must transcribe to the expected text within tolerance (for LLMs: one known prompt; for embeddings: known nearest neighbours). Fail → keep the old model and show "Update didn't pass its check; still using X".
4. **Atomic switch:** the previous version is kept until three real jobs succeed, then garbage-collected. Rollback is one click while it exists.
5. **Old meetings are never silently rewritten:**
   - each transcript records the engine and model that made it;
   - re-transcribing is opt-in per meeting, folder or everything, with a time estimate;
   - an explicit Whisper "Model override" pins Whisper.
6. **Embedding model change:** build the new index side-by-side in the background (throttled), serve the old one until the new one is complete, then swap and drop the old.
7. **LLM default change:** offer it ("A better local model is available · 3.3 GB"); never delete a model the user picked; Ollama pin upgrades re-run the smoke test.

---

## 5. Second- and third-order impacts

| Change | Ripples to |
|---|---|
| Parakeet engine | Aligner input (token timestamps), cross-talk dedup window, DER baselines, VTT timing, Groq parity (two engines produce different text for the same meeting), model download sizes and setup copy, sidecar CI build, `-mc 0` guards (keep for the Whisper fallback), re-transcribe UX for old meetings |
| Nemotron live | sherpa-onnx 1.13.3 → ≥ 1.13.4 for diarization too; CPU budget during calls; license review |
| Retrieval changes | MCP `search_notes` output (keep it stable, add a passage tool), a one-time re-embed backfill (throttle it; it runs on users' CPUs after upgrade), index size |
| Insert creates AI blocks | Export, the Markdown mirror and MCP `get_note` now show provenance on Ask-inserted text (intended) |
| `fly-llm` content blocks | Every provider and every call site (Enhance, polish, extraction, Ask, bench); the `chat()` shim limits the blast radius |
| Default model change | Users already on `llama3.1` see a new download; tier detection needs a RAM/VRAM probe that's honest about whisper + LLM + embedder running together |
| Library | Asset scope and `safe_path` roots, MCP data-dir discovery (bootstrap config), retention vs cloud recycle bins, Keep local vs a cloud-synced Library (Keep-local meetings stay app-local), export/import collapses into "open a Library", the screen-recording location, test fixtures, manifest v1/v2 compatibility |
| Roaming → Local move | The Claude Desktop–spawned MCP must find the DB; persistent WAL against MSIX forking; enterprise roaming profiles stop carrying 40 GB |
| Settings rewrite | Allowlist keys, consent flows, deep links (`engine`, `groq`), `devMock`, Ctrl+K |
| Setup wizard | Consent ordering on macOS, engine-agnostic model tiers, managed Ollama, TCC prompt timing |
| Path-gated CI | Frontend-only PRs skip Rust tests (the release still builds everything); the many PRs from this plan will trigger security reviews (incremental, ~1 min each) |
| Connectors | A new egress path appears in the privacy pill and tiles, delete-all, the threat model (new T14–T17), and security-review paths |
| Shared `fly-context` layer | Closes THREAT_MODEL T10's open gap: the MCP server can return Keep-local meetings to Claude Desktop, whose model is in the cloud. The shared layer applies the same Keep-local filter to MCP by default, with a Settings switch "Let other AI apps read notes you keep local" (off) |
| Parallel sub-sessions | Hotspot files (`App.tsx`, `Editor.tsx` at 52 KB, `SettingsModal.tsx` at 72 KB) need single owners per wave; UI primitives land first |

---

## 6. Release plan

Every PR keeps existing behavior unless this document says otherwise. Every PR runs CI; UI PRs also get dev-mock QA screenshots on port 1420.

### v2.4: ship now (CI-verifiable, no data-format changes)

| PR | Content | Main files | Verify |
|---|---|---|---|
| 0.1 | Lean CI (path gating, `ci-ok`, cache, sidecar stub, fmt/frontend+vitest/clippy×3, timeouts, workflow lint, Dependabot cooldown) | `.github/workflows/ci.yml`, `.github/dependabot.yml` | Runs on its own PR; compare job-minutes |
| 0.2 | Claude security review (port from `coffee`, base-commit gatekeeper, fork notice, FOTW `paths.txt` + rule files from THREAT_MODEL) | `.github/workflows/security-review.yml`, `.github/security-review/*` | Dry run on a test PR touching `src-tauri/capabilities` |
| 0.3 | Release preflight (notary API call), cargo-cyclonedx prebuilt | `.github/workflows/release.yml` | Manual tag on a fork or a prerelease |
| 0.4 | Storage safety (no sweep on a fresh DB, atomic writes, backfill doesn't overwrite, persistent WAL, scratch to cache) | `crates/fly-storage/**`, `pipeline.rs`, `live.rs` | New fly-storage tests; heal/backfill regression tests |
| 0.5 | Triage the 16 open Dependabot PRs (frontend majors fail `npm ci`) | — | CI |
| 1.1 | UI primitives: Switch, Segmented, SettingsGroup/Row, Sheet, NoticeStack, layered Esc | `ui.tsx`, `index.css` | vitest + dev-mock |
| 1.2 | Settings shell + every control mapped (Appendix A) | `SettingsModal.tsx`, `PrivacySection.tsx`, `devMock.ts` | dev-mock walk of every row; Appendix A checklist |
| 1.3 | Notices stack + per-OS copy + UpdateBanner migration | `SystemAudioNotice.tsx`, `UpdateBanner.tsx`, `pipelineNotice.ts`, `App.tsx` | Existing `pipelineNotice` tests + new |
| 1.4 | Home: grouping, status chips, empty state, undo toast, Up next, no-calendar / expired states, privacy pill | `NoteList.tsx`, `Sidebar.tsx`, `App.tsx` | dev-mock states |
| 1.5 | Recording bar: one row, warning chip, copy notice, "Stop and transcribe", speaker card v1 (confirm, never auto-confirm) | `RecordingBar.tsx`, `AttendeeEditor.tsx` | dev-mock + a real recording smoke test |
| 1.6 | Enhanced trust: legend, visible unsourced + Keep/Remove, quote preview, ghost headings, clip tiles in all views | `EnhancedDoc.tsx`, `Editor.tsx`, `NotesEditor.tsx` | vitest + dev-mock |
| 1.7 | Ask UI over the current backend: docked composer, sheet, human errors, quick prompts kept | `AskPanel.tsx`, `Editor.tsx` | dev-mock |
| 1.8 | Import: per-file failure attribution + copy | `import_commands.rs`, `ImportQueue.tsx` | A bad file in a batch |

**Before tagging v2.4:** one real-app smoke run by you on Windows (and macOS if available): record, transcribe, enhance, Ask, Settings round-trip.
**Your actions:**
- release-secret scoping (private notes);
- required check `ci-ok`;
- Dependabot alerts on;
- Anthropic Console federation rule + 4 repo variables;
- label `security-review`;
- accept the Apple developer agreement.

### v2.5: test together (real meetings, real hardware)

| PR | Content | Verify |
|---|---|---|
| 2.0 | Model manager v2: manifest, resumable verified downloads, smoke test, atomic switch + rollback, GC / "Free up X GB", engine metadata on transcripts (4.6) | Kill mid-download; corrupt file; failed smoke test; offline |
| 2.1 | WER harness: multi-engine, normalizer, corpus manifest (local only) | Baseline numbers on your corpus |
| 2.2 | Parakeet engine (sidecar path), parser, routing (English → Parakeet), engine metadata | Ship gates (4.1) on RTX 3050 Vulkan + CPU + a Mac |
| 2.3 | Silero VAD | Hallucination-loop count, WER |
| 2.4 | Tier mapping, `models_for_tier`, Settings and setup copy | dev-mock + real download |
| 3.1 | `fly-llm`: content blocks, tools, streaming, probes (`chat()` shim) | MockProvider tests; all providers smoke |
| 3.2 | `fly-context` crate + retrieval upgrades (segment ids on chunks, segment FTS, polished indexing, label-free embeddings, throttled re-embed backfill) — **schema migration** | Retrieval gold set in CI |
| 3.3 | Harness: self-query router, scope, prefetch (deep pool + per-meeting cap), tiers, citation + claim validator, score floor / "not found", proposal gate, allowlisted run log; Insert → AI block; add_action_item; relabel updates item owners; citation locators with re-anchoring after re-transcribe (4.2a) | Harness contract + red-team tests; benchmark command with committed thresholds |
| 3.3b | Contention benchmark (Ask p95 under recording + backfill; later + live captions) and resilience test | Runs on your machine |
| 3.4 | Ask UI: streaming sheet, chips with unsourced/flagged, trace pill, scope chip + @, global Ask entry | dev-mock + real models |
| 3.5 | Model bench and default decision; Ollama pin 0.31.2 → 0.40.x with bug re-tests | `llm_bench` on your machine (8 GB/16 GB profiles) |
| 3.6 | MCP passage search on `fly-context` (existing tools unchanged) | stdio lifecycle test + Claude Desktop |
| 3.7 | Enhance: id validator + passage preselection for long meetings | Enhance contracts bench |
| 2.5 | Glossary spotting + glossary-constrained polish; "Words to listen for" goes live | Name accuracy on the corpus |
| 2.6 | sherpa-onnx 1.13.3 → ≥ 1.13.4 (needed for live captions later), so diarization is re-validated once | DER baselines in `docs/data/diarization/` |
| 4.x | Setup wizard (consent-first order, sound-check API, tier models, AI-notes step, calendar) | Fresh-profile run on Windows and macOS |
| 4.y | Image paste/drop into notes (existing attachments API); calendar-start "Ask me" prompt (adds a notification plugin) | dev-mock + real app |
| 4.z | Recording storage v2 (16 kHz FLAC sources, M4A playback, no stored mix, delete full-rate WAVs after transcript) + "Compress existing recordings" job + compatibility reader — **stored-format change** (4.6) | Transcript bit-identity vs WAV on the corpus; playback on Windows + macOS; kill mid-compress; old meetings still re-transcribe |

### v2.6: test together (real cloud folders)
Library:
- Roaming → Local move;
- bootstrap config and MCP discovery;
- path-root split;
- staging → publish outbox;
- compressed audio (measured against ASR in 2.1 first);
- layout v3 with frontmatter and naming rules;
- attachments inside meeting folders;
- index rebuild;
- change tracking and conflict copies;
- offline mode and placeholder detection;
- Library chooser, move job and cloud detection;
- "Open a Library" replaces "Bring notes in".

**Schema and format migration.** Test matrix from the storage report: OneDrive personal and business, Google Drive Stream and Mirror, Dropbox, iCloud, SMB NAS, USB unplug, a kill mid-move, a 1,000-meeting streamed rebuild with no audio downloads.

### v2.7: beta flags
- Connectors beta: `fly-mcp-client`, Settings › Connectors, the confirm card, pinning, OAuth, the call log. Test with HubSpot, Notion, GitHub and a malicious fixture server.
- Live captions on Nemotron, gated by the CPU budget.
- Screen-clip captions v1 (3.9).

### Later
- Live Assist ("When I ask" first).
- Player v2.
- "Search my files".
- Write-capable connectors and CLI templates.
- Two-phase transcript (async diarization) and streaming diarization.
- Top-tier ASR models.
- A llama-server sidecar experiment.

---

## 7. How the work runs

- **Sessions.** One sub-session per PR-sized track. Each works in its own worktree and branch and opens a PR. I review every diff, including an adversarial "did this drop a feature or handler?" pass as in M10, coordinate merge order and keep this document current.
- **Wave A** (parallel, disjoint files):
  - CI + security review (`.github/**`).
  - Storage safety (`crates/fly-storage/**`).
  - UI primitives → Settings shell (`ui.tsx`, `SettingsModal.tsx`, `PrivacySection.tsx`).
  - Transcription harness + Parakeet spike (`crates/fly-asr/**`, `src-tauri/tests/accuracy_harness.rs`). Needs your corpus.
  - Ask foundations, `fly-llm` + `fly-context` scaffolding (`crates/fly-llm/**`, new crate). Its storage-schema PR waits for storage safety to merge.
- **Wave B** (after the primitives merge):
  - Notices + Home (`App.tsx` owner).
  - Note/Enhanced trust + Ask sheet (`Editor.tsx`, `EnhancedDoc.tsx`, `AskPanel.tsx`).
  - Recording bar + import attribution.
- **Models.** Every session runs on **Opus 5.5**. The transcription and Ask-harness sessions run at **high** effort; the rest use the default.
  - Fable 5.1 earned its keep in the research phase (open-ended, ambiguous, many sources). Building against this spec is well-defined work where Opus 5.5 is the right tool at 2.5× lower cost.
  - Escalate a track to Fable only on the "didn't *know* enough" signal: confidently wrong on alignment/timestamp maths, a citation-validator edge case it keeps missing, or eval failures it can't explain. Use it as a targeted subagent or reviewer, not for a whole session.
- **Testing together.** Anything in v2.5+ marked "test together" pauses at a local checkpoint: I prepare the build and a script, and you run it on your hardware and meetings (or we drive it together).

---

## 8. Calls I made (check these)
1. Seven Settings sections; "Your files" becomes "Your files and privacy"; templates live under "Ask and Enhance"; diagnostics under "General" (Appendix A).
2. Setup order Welcome → Consent → Sound check → Models → AI notes → Calendar.
3. Calendar auto-start offers Nothing / Ask me (no auto-Record).
4. Recipes merge into templates.
5. "Show steps" is not a setting.
6. Undo toast = deferred delete.
7. Insert into note creates an AI block with sources.
8. Ask defaults to "this meeting" inside a note and "all meetings" from Home.
9. Ask tiers are detected, not chosen; tier S makes zero model-driven tool calls.
10. Phase 1 keeps an explicit save for the Transcripts group.
11. The Claude Desktop "Set up" button shows the snippet and opens the folder; it never edits the file.
12. Player v1 only; chapters dropped until a data source exists.
13. Library default `~/Fly on the Wall`; Keep-local meetings never publish to a cloud-synced Library.
14. Connectors are MCP only, beta, read-only, confirm every call.
15. The MCP server may use loopback Ollama for query embeddings, falling back to FTS when it's absent.
16. The polished transcript is canonical for retrieval and citations; segment ids are unchanged by polish.
17. **The Library is one-way in v1.**
    - `.fotw/*.json` is the source of truth; `Notes.md`, `Transcript.md` and the other readable files are renderings.
    - An outside edit to `Notes.md` is detected and preserved as a conflict copy with a badge, never imported and never overwritten. Two-way import is deferred.
    - What travels in `.fotw/` so a second machine never re-runs AI or loses state: extracted items, speaker labels and keys, confirmed rosters, folder membership, the Keep-local flag, series, and templates (`.fotw/library.json`).
    - The `.fotw` JSON shape is fixed in v2.5 PR 3.2's design, so the schema migrates once.
18. **The Ask run log stores no question or answer text.**
    - It records ids, timings, token counts, tool names and arguments (ids only), cited and unsourced counts, and the termination reason.
    - It keeps the last 30 days, is cleared by "Delete all my data", and is never written to the Library.
19. The sherpa-onnx upgrade happens in v2.5 (PR 2.6), so diarization is re-validated once.
20. Image paste and the calendar-start "Ask me" prompt land in v2.5 (PR 4.y).

## 9. Open questions for you

Needed before Wave A starts:
1. **Approve the plan** as the basis for Wave A (or mark what to change).
2. **Merge authority.**
   - Recommended: I merge PRs to main once CI is green, the security review is clean and I've reviewed the diff; you alone approve release tags.
   - v2.5 work (fly-llm, fly-context, the Parakeet spike) merges to main dormant behind internal flags, instead of long-lived branches.
3. **What goes in the public repo.**
   - Recommended: commit this spec; keep the design prototypes, competitor notes and research notes in a stable local folder outside the repo (sub-sessions read them by absolute path).
   - Docs under `docs/` are also served by the public Pages site.

Needed before v2.5:
4. **Parakeet on upgrade.**
   - Recommended: it becomes the default for English *new* recordings only after it passes the ship gates on your meetings, with a background download.
   - Anyone with an explicit Whisper model override stays on Whisper.
   - Old transcripts are untouched.
5. **Mac access.** Is a Mac available for the v2.5 checkpoints? If not, macOS stays on Whisper in v2.5 and is validated by CI only.
6. **Your testing time.**
   - About 2–3 hours to label ~30 Ask questions with expected transcript lines.
   - Whether to hand-correct the 1:1 meeting as a reference. Without it, the 1:1 gates only speaker attribution and hallucination loops.
7. **Recording storage v2** (4.6): OK to delete full-rate WAVs after the transcript is saved by default, with "Keep original recordings" as an opt-in?

Needed before v2.7:
8. **The NVIDIA Open Model License** for Nemotron live captions, or Moonshine v2 (MIT) instead.
9. **Which connectors you can test** with real accounts: HubSpot, Notion, GitHub, Salesforce (needs Enterprise Edition).

---

## Appendix A: every current Settings control → its home

| Current control | New home |
|---|---|
| Simple/Technical toggle | Removed; per-group More options |
| Machine info (cores, RAM, GPU, VRAM) | Transcripts › More › This computer |
| Appearance | General |
| Microphone | Recording › Microphone |
| Hardware tier Light/Balanced/Best/Cloud | Transcripts › Quality (Fast/Balanced/Best) + Made on (Cloud) |
| Groq key | Transcripts › Made on: Groq › key row |
| "Use Groq, local fills in" (hybrid) | Transcripts › Made on: This computer / Groq / **Groq, then this computer** |
| Use GPU + speed-test verdict + test again + Intel-Mac unavailable | Transcripts › More › Use the graphics card (switch, verdict line, "Test again", disabled state with reason) |
| Transcribe automatically when a recording stops | Recording › Transcribe when I stop |
| Transcription model override | Transcripts › More › Model |
| Maximum quality | Transcripts › More › Maximum quality |
| Models on disk (engine row + all artifacts) | Transcripts › More › Models on this computer (deep-link target `engine`) |
| AI provider (Ollama / OpenAI / Anthropic / NIM) | Ask and Enhance › Runs on (select, all four + custom) |
| Claude model select | Ask and Enhance › Model (when Anthropic) |
| Base URL + model overrides | Ask and Enhance › More › Endpoint and model |
| Test connection / Save provider | Ask and Enhance › Test (row button) |
| Managed Ollama (install, status, pull/swap/delete, models) | Ask and Enhance › More › Local models |
| Per-provider cloud consent | Kept as the confirm flow when picking a cloud provider; badge after |
| Google / Outlook connect | Calendars |
| Per-calendar Up next toggles | Calendars › under each connected account |
| OAuth client IDs, Google secret, Save | Calendars › More › Your own OAuth app |
| Note templates CRUD | Ask and Enhance › Note templates · Manage |
| MCP snippet + Copy | Other AI apps |
| Backfill N meetings not indexed | Your files and privacy › Search index |
| Disk-encryption banner | Your files and privacy (top row) |
| Last-wipe report | Your files and privacy › Delete all my data (subline) |
| Audio retention | Your files and privacy › Delete audio after |
| Delete all my data (two-step) | Your files and privacy (last row, destructive) |
| Open logs folder | General › Diagnostics |
| App updates (check, download, progress, restart, recording block, deb note) | General › Updates (full flow kept) |
| Ctrl+K opens Settings | Unchanged in v2.4 |
| Deep links `engine`, `groq` | Re-pointed to the new rows |
