# Transcription pipeline and scheduler (`app-pipeline`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: PR #92 (draft, v2.5): storage schema v3 changes what `save_transcript` writes (per-segment `transcript_segments`/`segments_fts`, `citation_locators` re-anchored on re-transcribe). `run_with` and `re_diarize_with` call it unchanged, but re-diarize's "segment ids never change" contract becomes load-bearing for citations. #86 (merged) added a dormant Parakeet engine to [fly-asr](fly-asr.md). The chain below leads with Parakeet tiers only when the internal `asr.engine` flag (or `FLYONTHEWALL_ASR_ENGINE`) says `parakeet` and the meeting is transcribed as English (`use_parakeet`, `parakeet_tiers`). Otherwise it is unchanged.

## Purpose
Turns a meeting's recorded WAVs into a saved, diarized transcript, then runs the best-effort AI pass (transcript cleanup, then item extraction). Jobs live in a persistent queue (`transcription_jobs`). One worker drains it, one job at a time, and never starts a job while audio or screen capture is active. Also owns the transcript read/edit commands, attendee confirmation, "Re-analyze speakers" with one-level undo, the ASR settings, and cooperative cancel.

## Files it owns
- `src-tauri/src/pipeline.rs`: `run_with` (full pipeline), `re_diarize_with`, the ASR fallback chain (`GuardedAsr`), local tier planning, speaker-count hint, progress detail strings.
- `src-tauri/src/scheduler.rs`: job queue worker (`tick`, `spawn`), retry/permanent/cancel classification, `ai_pass`, consent-skip re-runs, `cancel`, `last_failure`, startup `recover`, the `pipeline:progress` emitter.
- `src-tauri/src/asr_commands.rs`: the transcript, attendee, re-diarize, stage and ASR-settings commands. `download_model` in this file belongs to [app-models-gpu](app-models-gpu.md).
- `src-tauri/src/commands.rs:327-342`: `cancel_transcription`, a thin wrapper over `import_commands::cancel_staged_transcription`.

## Surface area
### Public API
| Item | Where | Called by |
|---|---|---|
| `pipeline::run_with(state, on_stage, on_model, meeting_id) -> Result<Transcript, String>` | `pipeline.rs:152-715` | `scheduler::tick` (`scheduler.rs:117`); e2e tests |
| `pipeline::re_diarize_with(...) -> Result<ReDiarizeOutcome, String>` | `pipeline.rs:731-747` | `re_diarize_meeting` (`asr_commands.rs:121`) |
| `pipeline::{speaker_hint, SpeakerHint}` | `pipeline.rs:33-51` | internal only |
| `pipeline::cloud_asr_allowed(use_groq, tier, keep_local)` | `pipeline.rs:148-150` | internal |
| `pipeline::{PipelineProgress, StageSink, ReDiarizeOutcome, TierKind, MIC_SPEAKER_KEY = "mic", ERR_NO_RECORDING_FILES, sidecar_threads}` | `pipeline.rs:26`, `pipeline.rs:100-111`, `pipeline.rs:139-144`, `pipeline.rs:718-722`, `pipeline.rs:906-910` | scheduler, import, tests |
| `scheduler::enqueue(state, on_stage, meeting_id)` (idempotent) | `scheduler.rs:56-68` | `transcribe_meeting`, `import_transcribe` (`import_commands.rs:745`) |
| `scheduler::tick` / `Tick::{Idle, RecordingActive, Completed, Cancelled, Retrying, GaveUp}` | `scheduler.rs:34-151` | `spawn` loop; tests |
| `scheduler::spawn(app)` | `scheduler.rs:156-201` | `lib.rs:173` (app setup) |
| `scheduler::cancel(state, on_stage, meeting_id) -> Result<bool>` | `scheduler.rs:306-344` | `delete_note` (`commands.rs:296`), `cancel_staged_transcription` (`import_commands.rs:369`) |
| `scheduler::{finish_cancelled, emit_cancelled (crate), recover, stage_emitter, last_failure, retry_consent_skipped, recording_active (crate)}` | `scheduler.rs:237-470` | import, `llm_commands.rs:476` (`retry_consent_skipped`), `privacy.rs:259`, `privacy.rs:405` (`recording_active`) |
| `scheduler::{MAX_ATTEMPTS = 3, WAITING_STAGE = "waiting"}` | `scheduler.rs:24-26` | tests, UI stage handling |

### Tauri commands
All are registered in `src-tauri/src/lib.rs` (`cancel_transcription` at `lib.rs:198`; the rest at `lib.rs:226-238`).

| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `cancel_transcription` | `meeting_id: String` | `Result<(), String>`. Always ends in a terminal `cancelled` event, now or once the run unwinds. | `ui-import` (`App.tsx:682` `cancelImportTranscribe`, the ImportQueue Cancel), via `api.cancelTranscription` |
| `transcribe_meeting` | `meeting_id: String` | `Result<(), String>`. Enqueues; emits `waiting` | `ui-app-shell` (`App.tsx:726` auto-transcribe on stop, `App.tsx:763` Transcribe button, `App.tsx:1113` notice Retry), via `api.transcribeMeeting` |
| `get_transcript` | `meeting_id: String` | `Result<Option<Transcript>, String>` | `ui-app-shell` (`App.tsx:405`, `:555`, `:795`), via `api.getTranscript` |
| `get_cleaned_transcript` | `meeting_id: String` | `Result<Option<Transcript>, String>` (the polished variant) | `ui-app-shell` (`App.tsx:406`, `:556`, `:769`, `:796`), via `api.getCleanedTranscript` |
| `relabel_speaker` | `meeting_id, speaker_key, label: String` | `Result<Transcript, String>` | `ui-editor` (`App.tsx:820`, passed to the transcript speaker menu), via `api.relabelSpeaker` |
| `edit_transcript_segment` | `meeting_id, segment_id, text: String` | `Result<Transcript, String>` | `ui-editor` (`App.tsx:829`), via `api.editTranscriptSegment` |
| `update_meeting_attendees` | `meeting_id: String, attendees: Vec<Attendee>` | `Result<Meeting, String>`. Marks the list confirmed. | `ui-attendees` (`AttendeeEditor.tsx:207` Save, `:264` Save before re-analyze), `ui-editor` (`Editor.tsx:1220` "Someone else…"), via `api.updateMeetingAttendees` |
| `re_diarize_meeting` | `meeting_id: String` | `Result<ReDiarizeOutcome { changed_segments, transcript }, String>` | `ui-attendees` (`AttendeeEditor.tsx:282`), via `api.reDiarizeMeeting` |
| `revert_speaker_assignment` | `meeting_id: String` | `Result<Transcript, String>` | `ui-attendees` (`AttendeeEditor.tsx:220`), via `api.revertSpeakerAssignment` |
| `speaker_undo_state` | `meeting_id: String` | `Result<Option<SpeakerUndoState { taken_at, changed_segments }>, String>` | `ui-attendees` (`AttendeeEditor.tsx:113`), via `api.speakerUndoState` |
| `pipeline_stage` | `meeting_id: String` | `Option<String>` (the in-memory stage, `None` when idle) | `ui-app-shell` (`App.tsx:557`, `:797`, on opening a note), via `api.pipelineStage` |
| `pipeline_failure` | `meeting_id: String` | `Option<String>` (the error of a job parked as failed) | `ui-app-shell` (`App.tsx:561`, `:798`), via `api.pipelineFailure` |
| `get_asr_settings` | none | `Result<AsrSettings, String>` (tier, model, Groq, GPU, bench verdict, hardware, models, engine installed or managed) | `ui-settings` (`SettingsModal.tsx:340`, `:347`), via `api.getAsrSettings` |
| `set_asr_settings` | `update: AsrSettingsUpdate { tier, model_id, use_groq, max_quality, auto_transcribe, use_gpu, groq_key: Option<String> }` | `Result<(), String>` | `ui-settings` (`SettingsModal.tsx:484`, `:501`, `:527-528` GPU re-test), via `api.setAsrSettings` |

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `pipeline:progress` | emits (`scheduler::stage_emitter`, `scheduler.rs:459-466`; sinks in `pipeline.rs:113-132`, `scheduler.rs:369-377`, `:424-437`, `:472-502`, `asr_commands.rs:124-130`) | `PipelineProgress { meeting_id, stage, detail: Option<String>, done: bool, error: Option<String> }` | `App.tsx:373` (stage strip, run chips, failure notices, import queue); devMock mirrors the shape (`devMock.ts:419`, `:484-511`, `:1632`) |
| `model:progress` | emits (worker `on_model`, `scheduler.rs:159-164`; `re_diarize_meeting`, `asr_commands.rs:114-120`) | `models::ModelProgress` (owned by [app-models-gpu](app-models-gpu.md)) | `App.tsx:450` |
| `llm:consent-skips` | emits after every AI pass (`scheduler.rs:222-229`) | `Vec<String>` meeting ids skipped for missing cloud consent | `CloudConsent.tsx:122` (`ui-confirm-consent`) |

`pipeline:progress` stages, in order:

| `stage` | `done` | Emitted by | Meaning / `detail` |
|---|---|---|---|
| `waiting` | false | `mark_waiting` on enqueue, startup recovery, or a retry (`scheduler.rs:472-490`) | detail `retrying (attempt N failed)` on a retry |
| `starting` | false | `pipeline.rs:165` | also takes the per-meeting guard |
| `ensuring-models` | false | `pipeline.rs:240`, `:282-288`, `:299-307`, `:417-427` | downloads; detail carries the Keep-local, missing-Groq-key and fallback-model notices |
| `benchmarking` | false | `pipeline.rs:1238-1241` (Windows, GPU on, no verdict yet) | one-time GPU vs CPU speed test ([app-models-gpu](app-models-gpu.md)) |
| `preparing-audio` | false | `pipeline.rs:461` | resample to 16 kHz into scratch |
| `transcribing` | false | `pipeline.rs:520-526`, `:540-546`, `:624`; progress via `transcribe_detail` (`pipeline.rs:932-956`) | `cloud 42%`, `your microphone (GPU 42%)`, `cloud 40%, waiting for cloud quota (~3m)`, or a failover notice |
| `diarizing` | false | `pipeline.rs:565`, `:643`, `:848` | |
| `aligning` | false | `pipeline.rs:601`, `:646`, `:661`, `:770`, `:859` | |
| `saving` | false | `pipeline.rs:688`, `:864` | |
| `polishing` | false | `scheduler.rs:390` (AI pass) | the UI fetches the fresh raw transcript here (`App.tsx:437-448`) |
| `done` / `error` | true | `emit_final` (`scheduler.rs:494-502`), `re_diarize_meeting` (`asr_commands.rs:124-130`) | `error` carries the message |
| `cancelled` | true | `emit_cancelled` (`scheduler.rs:369-377`) | terminal, neither success nor error. The UI drops back to idle and re-reads the import queue (`App.tsx:384-399`). This string is load-bearing; don't rename it. |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `asr.tier` | R (`pipeline.rs:228-230`), W (`asr_commands.rs:336`) | No | `light`/`balanced`/…/`cloud`. `cloud` turns on Groq. Default comes from the hardware profile |
| `asr.model_id` | R/W | No | whisper model override; `""` means the tier default |
| `asr.use_groq` | R/W | No | Groq first, with the local chain as fallback |
| `asr.max_quality` | R/W | No | large-v3 instead of the tier default |
| `asr.auto_transcribe` | R (`asr_commands.rs:305`)/W | No | the frontend calls `transcribe_meeting` on stop when true |
| `asr.language` | R only (`pipeline.rs:234`) | No | `auto` or a language code; unset means `en`. **No command writes it** (searched `src-tauri/src` and `frontend/src`), so the app always transcribes as English |
| `asr.use_gpu` (`gpu::USE_GPU_KEY`) | R/W | No | GPU switch. Turning it off then on clears `asr.gpu_bench` (`asr_commands.rs:372-377`) |
| `asr.gpu_bench` (`gpu::BENCH_KEY`) | R; W via `gpu::record_runtime_failure` after a GPU tier failure (`pipeline.rs:670-673`) | No | per-model GPU/CPU verdict; `cpu` pins the chain to CPU (`pipeline.rs:1313-1315`) |
| `hw.cache` (`hw::CACHE_KEY`) | R/W (`asr_commands.rs:255-271`) | No | cached hardware profile |
| `asr.engine` (`pipeline::ENGINE_SETTING_KEY`) | R only (`run_with`) | No | Internal engine selector (#86). `parakeet` leads with Parakeet tiers on the fully local path, for English only. No Settings row and no command writes it; the harness and hardware tests set it. The environment variable `FLYONTHEWALL_ASR_ENGINE` wins over it. |
| `asr.xtalk_window_ms` (`pipeline::XTALK_WINDOW_SETTING_KEY`) | R only | No | Internal override of the cross-talk dedup window, for engine re-tuning (#86). No writer in the app. |

The pipeline reads per-meeting/folder Keep local through `storage.get_meeting_privacy(..).effective_keep_local()` (`pipeline.rs:177-193`). That is a column, not a setting.

### Database
| Table | Columns | Read / write |
|---|---|---|
| `transcription_jobs` | `meeting_id` PK, `status` (`queued`/`running`/`done`/`failed`), `attempts`, `last_error`, timestamps | R/W via `fly-storage/src/jobs.rs`: enqueue (idempotent; resets done/failed), next (FIFO by `updated_at`), running/done/failed/requeue, delete on cancel, `reset_running_transcriptions` on startup |
| `meetings` | `recording_json`, `attendees_json`, `attendees_confirmed`, `keep_local`, `audio_deleted_at`, `note_id` | R (pipeline); W `attendees_json` + `attendees_confirmed = 1` (`update_meeting_attendees` → `fly-storage/src/meetings.rs:82`); purged by `finish_cancelled` for orphans |
| `transcripts` | `json` (raw), `cleaned_json` (polished), `speaker_undo_json` | W raw on save; `cleaned_json` cleared after a re-transcribe (`pipeline.rs:694-698`); on re-diarize the cleaned keys are mirrored and the snapshot saved (`pipeline.rs:865-893`); relabel/edit/revert |
| `transcripts_fts`, `embedding_chunks` | | written indirectly by `save_transcript`; `embed_notify` wakes the embedder after `Completed` (`scheduler.rs:171`) |
| `notes` | | R in `finish_cancelled` (`scheduler.rs:350-356`) to detect an orphaned meeting |
| `settings` | see above | R/W |

In-memory state (`src-tauri/src/state.rs`): `pipeline_stage: HashMap<meeting_id, stage>` (the per-meeting guard and the late-joiner stage), `cancel_requests: HashSet`, `consent_skipped`, `jobs_notify`.

A cancel request stays until it's honoured: `run_with` doesn't clear it, so an automatic retry can't lose it, and `tick` ends a run that fails for any reason while a cancel is pending as `Cancelled` (job removed, no retry). Only `scheduler::enqueue` creating a new job drops a stale request.

### Files and directories written
| Path | Written by | Lifetime |
|---|---|---|
| `<scratch_root>/<meeting_id>/{mic,system,track}.16k.wav` | `pipeline.rs:473-484`, `:516-517`, `:621` | deleted with their checkpoints after a successful save (`pipeline.rs:704-710`); kept on failure so the retry resumes |
| `<scratch_root>/<meeting_id>/*.asrpart.json` | fly-asr checkpoints next to each 16k input | same as above |
| `<scratch_root>/<meeting_id>/rediarize.16k.wav` | `pipeline.rs:839-856` | deleted right after diarization |
| `%TEMP%/flyonthewall-{whisper,spill}-<uuid>.*` | fly-asr per batch | see [fly-asr](fly-asr.md) |
| `<data_dir>/recordings/<meeting folder>/transcript.*` mirrors | `storage.save_transcript` ([fly-storage](fly-storage.md)) | permanent |
| `recordings/_unlinked/<name>` renamed back to `recordings/<name>` | `restore_parked_recording` | self-heal for a raced v2 migration; the target must be a plain `recordings/<folder>` (`contained_recordings_dir`), else nothing moves |

| `<scratch_root>/<meeting_id>/mixdown.16k.wav` | `audio_store::regenerate_mixdown_for_asr` (single-track run of a compressed recording whose stored mixdown was dropped) | deleted with the other intermediates |

Inputs may be WAV or lossless FLAC (recording storage v2, [app-audio-store](app-audio-store.md)): `prep`, re-diarize and the GPU sample read through `fly_audio::mix::read_audio_mono`, which yields the same f32 samples either way, so the 16 kHz intermediates (and therefore transcripts and diarization) are byte-identical. A compressed single-track recording gets its mixdown rebuilt, byte-identical to the one capture wrote, before `prep`.

Intermediates never go next to the recordings: a title or date edit renames the meeting folder mid-run. That rename caused the 2026-07-19 import incident (`pipeline.rs:462-475`, `crates/fly-storage/src/scratch.rs:1-18`).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `api.groq.com` (via [fly-asr](fly-asr.md)) | transcribing, when the engine chain starts with Groq | `asr.use_groq` or tier `cloud`, **and** a stored `groq_api_key`, **and** not Keep local for the meeting or its folder (`pipeline.rs:148-150`, `:274-310`). There is no `consent.*` prompt for Groq. |
| Model and sidecar hosts (via `models::ensure` / `ensure_tool`) | `ensuring-models`, when artifacts are missing | first use; see [app-models-gpu](app-models-gpu.md) |
| The configured LLM provider | the AI pass: `llm_commands::run_polish` and `extraction::extract_after_transcribe` (`scheduler.rs:216-221`) | `build_provider` enforces cloud consent and Keep local (spec §2 item 1; `llm_commands.rs:569`, `:821-823`; `extraction.rs:273`). A refusal is recorded as a consent skip (`scheduler.rs:403-406`). |

Engine routing (`pipeline.rs:319-451`, `pipeline.rs:1138-1307`):

| Condition | Chain (falls through on failure) |
|---|---|
| Groq allowed and key stored | Groq (resume, local spillover = best local tier) → local tiers below. If the local model can't be ensured, Groq runs alone. |
| Groq on but no key, or Keep local | local tiers, with a visible notice |
| Windows + GPU on + benchmark verdict `gpu` | `whisper.cpp-vulkan` → CPU build |
| Apple Silicon + GPU on + not pinned | `whisper.cpp-metal` → forced CPU |
| otherwise | CPU (`-ng` when the GPU switch is off) |

A GPU tier failure pins that model to CPU for later meetings. A cloud failure never pins (`pipeline.rs:1075-1077`). A cancel never fails over (`pipeline.rs:1072-1074`).

### External processes
`whisper-cli` (CPU, Vulkan or Metal) and `sherpa-onnx-offline-speaker-diarization`, launched through [fly-asr](fly-asr.md) and [fly-diarize](fly-diarize.md) at below-normal priority with `sidecar_threads()` = half the logical CPUs, clamped 2–8 (`pipeline.rs:139-144`). The GPU benchmark (`gpu::plan`) runs `whisper-cli` twice on a sample cut from the recording.

### Secrets and keychain entries
- `groq_api_key` (`fly_secrets::keys::GROQ_API_KEY`): read at run start (`pipeline.rs:292-296`), set or deleted by `set_asr_settings` (`asr_commands.rs:385-397`; `Some("")` deletes, `None` leaves it), checked for presence in `get_asr_settings` (`asr_commands.rs:299-304`). Never returned to the webview.

## Dependencies
- **Inbound:** `ui-app-shell`, `ui-import`, `ui-attendees`, `ui-editor`, `ui-settings` (commands above); [app-import](app-import.md) (enqueue, cancel, `emit_cancelled`; the scheduler calls back into `import_commands::purge_staged_import` from `finish_cancelled`); [app-notes-library](app-notes-library.md) (`delete_note` calls `scheduler::cancel`); [app-llm](app-llm.md) (`retry_consent_skipped`); [app-privacy](app-privacy.md) (`recording_active`); [app-recording](app-recording.md) and [app-screen-recording](app-screen-recording.md) wake the worker via `jobs_notify` on stop (`recording.rs:273`, `screen_commands.rs:263`).
- **Outbound:** [fly-asr](fly-asr.md), [fly-diarize](fly-diarize.md), [fly-audio](fly-audio.md) (`mix::read_wav_mono`/`resample_linear`/`write_wav_mono_16`), [fly-core](fly-core.md) (`align`, `crosstalk::split_crosstalk`, `repeat::collapse_loops`, `rediarize::apply_turns`), [fly-storage](fly-storage.md) (jobs, transcripts, `scratch::prepare_meeting_scratch`, privacy, `SpeakerSnapshot`), [fly-secrets](fly-secrets.md), [app-models-gpu](app-models-gpu.md) (`models::ensure`/`ensure_tool`, `gpu::plan`/`enabled`/`stored`/`record_runtime_failure`, `hw`), [app-llm](app-llm.md) (`run_polish`, `record_consent_skip`, `extract_after_transcribe`).

## Invariants
- **Only a confirmed attendee list sets `num_speakers`.** Spec §2 item 7. `speaker_hint` returns `Unknown` unless `attendees_confirmed` (`pipeline.rs:43-51`). Confirmed and empty means "Just you": no diarization run (`pipeline.rs:568-569`, `:644-652`, `:768-783`). Confirmed with n attendees means n+1 speakers on a mixed track and n far-end clusters on the system channel (`pipeline.rs:60-72`). Confirmation is set only by `update_meeting_attendees` (`crates/fly-storage/src/meetings.rs:82`). Test: only the ignored `rediarize_e2e`.
- **Keep local skips Groq.** THREAT_MODEL T10. Enforced at `pipeline.rs:148-150`, `:274-290`. Test: `pipeline::keep_local_meetings_never_use_cloud_asr` (predicate only).
- **Diarization is always local, on every tier.** ARCHITECTURE.md "Privacy boundaries"; `pipeline.rs:239-250`.
- **The AI pass goes through `build_provider`.** Spec §2 item 1. Enforced at `scheduler.rs:208-230` → `llm_commands.rs:821-823`, `extraction.rs:273`.
- **ASR, GPU and import settings are not webview-settable.** Spec §2 item 2. They get dedicated commands (`commands.rs:59-66`).
- **Recording always wins.** No job starts while audio or screen capture is active (`scheduler.rs:98-100`, `:468-470`). The check runs at job start only; a recording started mid-run does not pause it (inferred from the code).
- **One pipeline per meeting.** The guard is `pipeline_stage` (`pipeline.rs:158-164`, `:738-743`). The check and the insert take the lock separately (`pipeline.rs:160`, `:120-124`), so the guard is not atomic (inferred minor race).
- **Cancel is not a failure.** It is never retried and never fails over, and it ends in a `cancelled` event (`pipeline.rs:1072-1074`; `scheduler.rs:122-128`, `:367-377`). A last gate stops a deleted note from getting a transcript saved (`pipeline.rs:683-687`).
- **Unfixable failures fail once.** Missing files, deleted audio, 4xx, missing engine and failed download are parked on the first attempt; everything else retries up to 3 times, 10 s apart (`scheduler.rs:24-29`, `:77-82`, `:129-149`).
- **Re-diarize changes only speaker keys and labels.** Segment ids, text and boundaries are unchanged, and the cleaned variant's text is untouched (`pipeline.rs:724-730`, `:865-888`). Test: ignored `rediarize_e2e`.
- **Readers are format-blind.** Never call `read_wav_mono` on a stored recording; use `read_audio_mono` (a source may be FLAC), and keep the single-track input order mixed → mic → system with the mixdown regenerated for storage-v2 recordings.
- **A finished transcript wakes recording compression** (`state.audio_store.nudge()` after `emit_final`), which yields while any `pipeline_stage` entry exists. Re-diarize claims its `pipeline_stage` entry (`starting`) in the same lock as its duplicate check, before it reads the recording.
- **Intermediates live in machine-local scratch keyed by meeting id.** They are kept after a failure so the retry resumes (`pipeline.rs:462-475`, `:701-710`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/pipeline.rs` | 15 | Groq/keep-local predicate, parked-folder restore, speaker numbering, fallback chain (cloud→GPU→CPU, pin rules, cancel doesn't fail over, marker surfacing), tier plans per OS, CPU pin per model, detail strings |
| unit | `src-tauri/src/scheduler.rs` | 6 | permanent / transient / cancelled / 429 classification, consent re-run filtering |
| integration (runs in CI) | `src-tauri/tests/scheduling.rs` | 4 | real `tick` + storage: defer while recording, 3 attempts then parked, missing files fail once, restart recovery |
| unit (scheduler paths) | `src-tauri/src/import_commands.rs` | 6 of 17 | `tick` gives up and keeps the cause across restart; cancel variants emit exactly one `cancelled` (see [app-import](app-import.md)) |
| ignored e2e/harness (never in CI) | `src-tauri/tests/pipeline_e2e.rs` (2), `rediarize_e2e.rs` (1), `scheduling_e2e.rs` (1), `accuracy_harness.rs` (1 + 10 scorer unit tests that do run) | 5 | the real pipeline on the 27 s two-voice fixture: WER < 25%, 2 speakers, GPU→CPU fallback, re-diarize and revert contract, defer then transcribe |
| vitest (devMock backend) | `frontend/src/App.test.tsx`, `pipelineFailures.test.ts`, `pipelineNotice.test.ts`, `importProgress.test.ts`, `components/AttendeeEditor.test.tsx` | — | UI against devMock's copy of these events and commands |

**Known gaps:**
- `speaker_hint` (spec §2 item 7) has no CI test. Only the ignored `rediarize_e2e` exercises confirmed vs unconfirmed counts.
- `run_with` never gets past its early-error paths in CI. Settings→engine routing, keep-local wiring (that `GroqEngine` is not built), scratch placement and cleanup, cross-talk split, the cancel gates and save are covered only by ignored tests.
- `pipeline_e2e` still asserts `track.16k.wav` is absent from the meeting folder (`tests/pipeline_e2e.rs:266-269`). Intermediates moved to scratch, so this can't fail, and scratch cleanup is unverified.
- Nothing times out. `run_with`, the Groq HTTP call and the sidecars have no time limit, so a hang stalls the single worker and Cancel can't interrupt it. The AI pass runs inline before the next job, for up to 2 × `POLISH_TIMEOUT` (20 min, `scheduler.rs:172`, `:382`).
- The `spawn` loop (retry delay, the `Completed` → `ai_pass` → `done` ordering, `embed_notify`) and the cancel unwind (`Tick::Cancelled` → `finish_cancelled` → purge) have no test.
- Deleting a note while its meeting is `polishing`, or mid-re-diarize: `scheduler::cancel` reports running, `delete_note` skips the purge (`commands.rs:305-314`), and nothing purges later. The meeting row, transcript and recordings folder are orphaned (inferred from code).
- `relabel_speaker` and `edit_transcript_segment` don't check for a running re-transcribe, which then overwrites them (inferred).
- `asr.language` has no writer.

## Changing it safely
- Run `cargo test -p fly-app` (unit + `scheduling.rs`). With artifacts installed, also run the ignored `pipeline_e2e`, `rediarize_e2e` and `scheduling_e2e` (`-- --ignored --nocapture`).
- **Stage names and detail strings are a contract.** `App.tsx`, `ImportQueue.tsx`, `importProgress.ts` (`parseTranscribingDetail`) and `devMock.ts` all match on them. Rename nothing (especially `waiting`, `polishing`, `cancelled`) without updating all four.
- **Error classification is by substring.** `REJECTED_MARKER`, `CANCELLED_MARKER`, `ERR_NO_RECORDING_FILES`, `models::{ENGINE_MISSING_MARKER, DOWNLOAD_FAILED_MARKER}` (`scheduler.rs:77-89`). Keep them in the real messages.
- **A new command** needs the `lib.rs` registration, an `api.ts` wrapper and a `devMock.ts` case (spec §2 item 4). Known devMock drift: `pipeline_stage` always returns `null`; `transcribe_meeting` emits no `waiting` (except for the mock import); `re_diarize_meeting` emits no stages or terminal event; Cancel during a mock run emits nothing.
- **Attendees:** any UI that calls `update_meeting_attendees` confirms the whole list. `Editor.tsx:1212-1226` ("Someone else…") appends to a calendar-seeded roster and confirms it, so the roster then drives `num_speakers`.
- **Storage changes** (PR #92) must keep segment ids stable through `save_transcript` on re-diarize.
- Don't start a pipeline outside `scheduler::tick`. On failure, `run_with` leaves the per-meeting guard for the scheduler to clear (`pipeline.rs:712-713`, `scheduler.rs:131`).
