# Domain core (`fly-core`)

> Layer: crate. Described at main `82b7edf` (v2.4.0).
> In flight: None. #86 (merged) changed only the doc comment on `Transcript.engine` in `model.rs`. `engine` can now also hold `whisper.cpp-vulkan` and `parakeet[-vulkan]:<model file stem>`, with no type changes. Anything that matches on `engine` must expect those strings. PR #92 doesn't touch this crate.

## Purpose
This crate holds the types every other crate shares: notes, blocks with provenance, meetings, attendees, transcripts, speaker turns, meeting items and templates. It also holds the pure algorithms that turn raw ASR and diarization output into a trustworthy transcript (word↔speaker alignment, cross-talk de-dup, hallucination-loop collapse, re-diarize). It owns the Enhance and Polish prompt contracts and their parsers, plus a per-model prompt-profile table. Everything is deterministic and in-memory, with no I/O.

## Files it owns
- `crates/fly-core/Cargo.toml` — deps: `serde`, `serde_json`, `thiserror` (unused), `uuid`, `chrono`.
- `crates/fly-core/src/lib.rs` — module list, re-exports `model::*` and the aligner, `new_id()` (UUID v4).
- `crates/fly-core/src/model.rs` — domain types; provenance (`BlockOrigin`, `NoteBlock::apply_edit`/`reclaim`); `Attendee` legacy-compatible serde; `Meeting.attendees_confirmed`; `Transcript::to_markdown`/`to_vtt`; `ItemKind`/`MeetingItem`.
- `crates/fly-core/src/align.rs` — word↔speaker aligner (word-level and sentence-vote variants), single-speaker segmenting, channel merge.
- `crates/fly-core/src/crosstalk.rs` — mic/system echo de-dup for two-channel recordings.
- `crates/fly-core/src/enhance.rs` — Enhance prompt and block parser (with truncation salvage), `fence_safe`, Polish prompt, batching, the retention guard and the provenance check.
- `crates/fly-core/src/prompt_profile.rs` — per-model `PromptProfile` registry (`profile_for`).
- `crates/fly-core/src/rediarize.rs` — reassigns speaker keys onto an existing transcript and rebuilds the speaker list, keeping labels that stay stable.
- `crates/fly-core/src/repeat.rs` — n-gram repetition (ASR loop) detection and collapse.
- `crates/fly-core/src/seams.rs` — empty `SharingProvider` / `Integration` traits (deliberately unimplemented).

## Surface area
### Public API
| Group | Items | Used by |
|---|---|---|
| Ids | `new_id()` (`lib.rs:20-22`) | every crate that creates rows |
| Notes + provenance | `Folder`, `Note` (`to_markdown(include_sources)`), `NoteBlock` (`user`, `ai`, `apply_edit`, `reclaim`), `BlockOrigin::{User, Ai{source_segment_ids}}`, `Attachment` (`model.rs:10-133`) | fly-storage (`notes.rs:227` `apply_edit`, `notes.rs:244` `reclaim`, behind app-llm's `edit_note_block` / `keep_note_block`), fly-mcp, fly-context |
| Meetings | `Meeting` (`attendees_confirmed`, `attendee_names()`), `Attendee` (`from_legacy`, `display_name`, `match_key`), `RecordingRef` (`model.rs:139-251`) | fly-storage, app-recording, app-pipeline, fly-context, fly-mcp |
| Transcript | `Word`, `TranscriptSegment`, `Speaker`, `Transcript` (`label_for`, `to_markdown`, `to_vtt`), `SpeakerTurn` (`model.rs:257-373`) | fly-asr, fly-diarize, fly-storage, app-pipeline, fly-mcp |
| Items + templates | `ItemKind` (`ALL`, `as_str`, `parse`), `MeetingItem`, `Template` (`model.rs:380-451`) | fly-storage, extraction (`src-tauri/src/extraction.rs`), fly-mcp |
| Aligner | `AlignOptions` (2 s gap, `spk_unknown` fallback), `align_words_to_speakers`, `align_words_to_speakers_by_sentence`, `segments_from_single_speaker`, `merge_channel_segments` (`align.rs:10-196`) | `pipeline.rs:16,602-610,647,662,679`; the sentence variant is called only by the ignored `accuracy_harness.rs:1693` |
| Cross-talk | `CrosstalkOptions` (defaults: 1500 ms window, `min_run` 2, keep the system copy, `min_echo_ratio` 0.15), `ChannelSplit`, `split_crosstalk` (`crosstalk.rs:35-195`) | `pipeline.rs:587-591` |
| Loops | `MAX_PERIOD`, `LoopRun`, `loop_token`, `max_consecutive_run`, `collapse_loops` (`repeat.rs:9-115`) | `pipeline.rs:18,1354` (`guard_loops`); `crosstalk.rs:32` |
| Re-diarize | `UNKNOWN_KEY`, `Reassignment`, `reassign_segment_speakers`, `is_generic_label`, `rebuild_speakers`, `apply_turns` (`rediarize.rs:16-216`) | `pipeline.rs:861` |
| Enhance | `EnhancePrompt`, `build_enhance_prompt`, `enhance_blocks_schema`, `parse_enhanced_blocks`, `fence_safe` (`enhance.rs:17-206`) | `llm_commands.rs` (Enhance, and `fence_safe` for Ask at ~`620-640`), `extraction.rs:93-107` |
| Polish | `CLEANUP_SYSTEM_PROMPT`, `CleanupPrompt`, `build_cleanup_prompt`, `plan_cleanup_batches`, `cleanup_response_schema`, `parse_cleanup_response`, `apply_cleanup`, `PolishFlag`, `PolishOutcome`, `preserves_provenance` (`enhance.rs:306-612`) | `llm_commands.rs` (`polish_transcript`), fly-storage `transcripts.rs` (doc reference) |
| Profiles | `PromptProfile`, `TaskMaxTokens`, `TaskEffort`, `DEFAULT_PROFILE`, `profile_for` (`prompt_profile.rs:46-249`) | `llm_commands.rs:7`, `extraction.rs:10` |
| Seams | `seams::{SharingProvider, Integration}` (`seams.rs:8-16`) | none (checked with grep) |

### Tauri commands
None (crates expose no commands).

### Events
None.

### Settings keys
None.

### Database
None. fly-storage serializes these types; `Attendee` must accept the legacy plain-string form, or a stored attendee list fails to parse (`model.rs:157-189`).

### Files and directories written
None.

### Network egress
None.

### External processes
None.

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** every other workspace crate except fly-secrets lists it in `Cargo.toml` (fly-audio declares it but never uses it). The app crate uses `align`, `crosstalk`, `repeat` and `rediarize` in `pipeline.rs`; `enhance` and `prompt_profile` in `llm_commands.rs` and `extraction.rs`.
- **Outbound:** `serde`, `serde_json`, `uuid` (v4 → `getrandom`, the OS RNG), `chrono` (workspace default features, so `clock` → `iana-time-zone`), `thiserror` (declared, unused).

## Invariants
- **No OS, UI or network code** (ARCHITECTURE.md "The golden rule"; `lib.rs:1-5`). Holds at code level: `Cargo.toml:7-12` lists only serde/serde_json/thiserror/uuid/chrono. The only platform-touching dependencies are transitive: `uuid` v4 uses the OS RNG through `getrandom`, and chrono's default `clock` feature pulls in `iana-time-zone` (inferred from `Cargo.lock`). Nothing enforces the rule: `deny.toml` has no ban for this crate.
- **Provenance: user and AI text never share a block, and editing an AI block reclaims it as the user's** (ARCHITECTURE.md "Provenance"; `model.rs:19-31,59-72`). An identical edit doesn't reclaim (`model.rs:61`).
- **Enhance sources map to real segment ids; out-of-range indices are dropped** (DECISIONS.md M4 "Enhance output contract"; `enhance.rs:222-226`).
- **JSON-shaped LLM output is never pasted raw into a note.** A truncated array salvages its complete blocks; anything unsalvageable returns empty so the caller fails visibly (`enhance.rs:181-206,240-286`; field failure 2026-07-16).
- **Untrusted meeting data is fenced.** `fence_safe` neutralizes the crate's own XML fence tags, and the contract tells the model never to follow embedded instructions (THREAT_MODEL T1; `enhance.rs:33-35,49-96`).
- **Polish changes text only.** Segment count, ids, speakers, timestamps and words stay the same. A segment that keeps less than 50% of its non-whitespace characters falls back to raw and is flagged (`enhance.rs:344,353,535-612`).
- **Re-diarize only changes `speaker_key`.** Segments with the protected key (`mic`) are never reassigned. A custom label carries over only when ≥ 80% of the duration maps both ways; otherwise it resets to "Speaker N" (`rediarize.rs:4-9,29-46,96,162-175`).
- **Aligner policy:** most-overlap wins; otherwise snap to the nearest turn within 1 s, else `spk_unknown`; split on pauses > 2 s (ARCHITECTURE.md "Transcript model & aligner"; `align.rs:20-27,198-231`).
- **Cross-talk de-dup is a no-op on clean or headset recordings** (< 15% of system words echoed) and needs a run of ≥ 2 matched words (`crosstalk.rs:49-55,124-162`).
- **Loop collapse:** a 2–12-word phrase repeated ≥ 3× keeps its first occurrence; a single word repeated ≥ 4× keeps two (`repeat.rs:68-115`).
- **Calendar-seeded attendees stay unconfirmed** (spec §2 item 7). The flag lives here and defaults to `false` for legacy rows (`model.rs:230-236`). It's set by fly-storage (`meetings.rs:53,82`) and read by `pipeline.rs:43-51`.
- **Prompt profiles are code, not settings.** Unknown models get the no-op `DEFAULT_PROFILE`. `qwen3.5` must never get `constrained_enhance` (`prompt_profile.rs:8-42,118-171,242-249`).
- **Seams stay empty** (ARCHITECTURE.md "Deliberately empty seams"; `seams.rs:1-4`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-core/src/model.rs` | 9 | reclaim on edit, legacy attendee parse, default-unconfirmed parse, VTT cues and escaping, markdown export |
| unit | `crates/fly-core/src/align.rs` | 12 | overlap assignment, 1 s snap, fallback, pause split, sentence vote (dormant variant), channel merge |
| unit | `crates/fly-core/src/crosstalk.rs` | 13 | echo runs, both keep directions, window, `min_run`, echo-ratio gate, token normalization |
| unit | `crates/fly-core/src/enhance.rs` | 20 | prompt shape, provenance mapping, truncation salvage, Polish guard and provenance, batching, `fence_safe` |
| unit | `crates/fly-core/src/prompt_profile.rs` | 8 | profile table, tag stripping, preamble |
| unit | `crates/fly-core/src/rediarize.rs` | 7 | text/id invariance, mic protection, label carry and reset |
| unit | `crates/fly-core/src/repeat.rs` | 11 | loop collapse thresholds, minimal period, 1881× loop |
| integration (CI) | `src-tauri/tests/enhance_flow.rs`, `src-tauri/tests/polish_flow.rs` | 1 + 2 | Enhance and Polish through the app with a mock provider |
| ignored e2e/harness | `src-tauri/tests/accuracy_harness.rs`, `pipeline_e2e.rs`, `rediarize_e2e.rs`, `polish_harness.rs`, `llm_bench.rs` | 1, 2, 1, 1, 1 | aligner, cross-talk and loops on real recordings; prompt contracts against real models |

**Known gaps:**
- All aligner, cross-talk and loop tests use tiny synthetic word lists. Nothing in CI checks overlapping speaker turns, equal-overlap ties, unsorted words or hour-long silences. The only real-recording check is the ignored accuracy harness.
- The sentence-vote aligner has 4 tests, but production calls the word-level aligner (`pipeline.rs:610,662`). So the "boundary jitter is absorbed" confidence doesn't apply to shipped transcripts.
- Cross-talk's documented false-echo case is untested: a near speaker who repeats 2+ of the far end's words within 1.5 s (`crosstalk.rs:29-30`). So is the greedy matcher splitting a run. `talk_over_survives` only checks that the unmatched word survives.
- Loop collapse also eats genuine repetition ("thank you thank you thank you" → period 2 × 3 collapses to one). No test pins this trade-off. Near-identical loops with one word varying escape the detector.
- Enhance: a small model that returns `"sources": ["2"]` (strings) fails the whole parse, so the user sees an empty or failed Enhance (`enhance.rs:172-179,197-199`, inferred). Untested.
- Nothing automated guards the golden rule (no dependency ban or CI check).

## Changing it safely
- Run `cargo test -p fly-core` (fast, all OSes). For aligner, cross-talk, repeat or VAD-adjacent changes, also run the ignored accuracy harness on a real two-channel recording (`docs/TESTING.md`), because the unit tests are synthetic.
- **Serde shape is a cross-crate contract.** fly-storage rows and manifests (`recovery.rs`), fly-mcp output and `frontend/src/types.ts` all mirror these structs. New fields need `#[serde(default)]`, as `attendees_confirmed` and `playback_path` have.
- **Prompt text:** follow the checklist in `prompt_profile.rs:11-42`. Benchmark with `llm_bench`, record the change in `docs/BENCHMARKS.md`, and update the `PROVIDERS` table in `llm_commands.rs` if a default model changes.
- **`FENCE_TAGS` (`enhance.rs:49-58`) must list every fence tag any prompt uses,** including the app-side Ask and extraction prompts (`llm_commands.rs` ~`620-640`, `extraction.rs:93-107`).
- Polish: never loosen `MIN_RETAINED_FRACTION` and never let the cleaned variant change structure. `preserves_provenance` is checked when the result is saved.
- Keep test fixtures synthetic (the repo is public).
- Don't add I/O, OS, UI or network crates here. Put them in an impl crate and select the impl in `src-tauri`.
