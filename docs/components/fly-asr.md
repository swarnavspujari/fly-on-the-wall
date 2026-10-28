# Speech recognition engines (`fly-asr`)

> Layer: crate. Described at main after #86 (v2.4.0 plus the v2.5 ASR harness).
> In flight: None. #86 (merged) added `batch.rs` and the dormant `parakeet.rs`. The app uses Parakeet only behind the internal `asr.engine` flag or the `FLYONTHEWALL_ASR_ENGINE` environment variable (see [app-pipeline](app-pipeline.md)).

## Purpose
Defines the `TranscriptionEngine` trait and its backends:
- **local whisper.cpp:** the `whisper-cli` sidecar;
- **local Parakeet TDT:** the `parakeet-cli` sidecar from the same pinned release zip. Dormant: it is off unless an internal flag asks for it, and English only;
- **Groq cloud ASR.**

Every backend applies the same preprocessing. VAD drops silence, each batch is peak-normalized, and word timestamps are mapped back to the original timeline. The local engines share one batched decode loop (`batch::transcribe_batched`). Every engine checkpoints finished batches, so a crash, restart or failover resumes instead of re-decoding.

The crate returns only words with timestamps. Speaker turns come from [fly-diarize](fly-diarize.md), which always runs locally.

## Files it owns
- `crates/fly-asr/Cargo.toml`: deps (fly-audio, fly-core, reqwest, hound, tokio, uuid).
- `crates/fly-asr/src/lib.rs`: the trait, `AsrError` and its string markers, options/progress/cancel types, and the non-speech token filters.
- `crates/fly-asr/src/batch.rs`: `transcribe_batched`, the VAD-batched decode loop shared by the local sidecar engines (speech spans, ≤ 120 s batches, normalization, map-back, checkpoints, progress, cancel), plus `split_long_spans`, which cuts over-long spans at their quietest point.
- `crates/fly-asr/src/whisper_cpp.rs`: `WhisperCppEngine` (`run_whisper_cli`, JSON parsing), and the batch planning and progress helpers that `batch.rs` and `groq.rs` reuse.
- `crates/fly-asr/src/parakeet.rs`: `ParakeetEngine`. It parses the `-ps` token dump on stderr into timed words, re-decodes "holes" the decoder skipped, and drops seam copies. Spans are capped at 120 s, because a single long call silently drops audio.
- `crates/fly-asr/src/groq.rs`: `GroqEngine` (upload chunking, overlap stitching, quota pacing, local spillover).
- `crates/fly-asr/src/retry.rs`: the pure cloud retry and pacing policy (429/4xx/5xx classification, backoff, `QuotaPacer`).
- `crates/fly-asr/src/checkpoint.rs`: per-batch `*.asrpart.json` checkpoints keyed by the decode plan.

## Surface area
### Public API
| Group | Items | Where |
|---|---|---|
| Trait | `TranscriptionEngine { id(), is_local(), transcribe(), transcribe_with_progress(on_progress, cancel) }` | `lib.rs` |
| Errors | `AsrError::{ModelMissing, BadAudio, Engine, Network, Rejected, RateLimited{message, retry_after}, Cancelled, Io}`, `Result<T>` | `lib.rs` |
| String markers (the app scheduler matches on them) | `REJECTED_MARKER = "cloud ASR rejected the request"`, `CANCELLED_MARKER = "transcription cancelled"` | `lib.rs` |
| Options and data | `TranscribeOptions { language, prompt, max_context }`, `RawTranscript { language, words, segments }`, `RawSegment` | `lib.rs` |
| Progress and cancel | `TranscribeProgress { done_ms, total_ms, quota_wait_ms }`, `TranscribeProgressFn`, `CancelFn` | `lib.rs` |
| Token hygiene | `is_non_speech_token`, `clean_word_text`, `drop_non_speech_spans` | `lib.rs` |
| whisper.cpp | `WhisperCppEngine { exe, model, threads, force_cpu, resume }` (`id()` is `"whisper.cpp"`), `parse_whisper_json` | `whisper_cpp.rs` |
| Parakeet (dormant) | `ParakeetEngine { exe, model, threads, force_cpu, resume }` (`id()` is `"parakeet"`), `parse_parakeet_tokens`, `model_tag_for` | `parakeet.rs` |
| Groq | `GroqEngine { api_key, model, base_url, resume, local_spillover }`, `GroqEngine::new`, `GROQ_DEFAULT_MODEL = "whisper-large-v3-turbo"`, `parse_groq_verbose_json` | `groq.rs` |
| Retry policy | `classify_http_failure`, `parse_retry_hint`, `retry_delay`, `spill_to_local`, `QuotaPacer`, and constants (`FREE_TIER_AUDIO_MS_PER_HOUR`, `QUOTA_SAFETY_MARGIN_MS`, `MAX_RATE_LIMIT_WAIT` = 20 min, `SPILL_TO_LOCAL_THRESHOLD_MS` = 30 s) | `retry.rs` |
| Checkpoints | `BatchResult { language, words, engine }`, `CheckpointFile::{open, resumed, push}`, `checkpoint_path_for`, `plan_key` | `checkpoint.rs` |

Crate-private but load-bearing:
- `batch::transcribe_batched` and `batch::MAX_BATCH_SPEECH_MS`.
- `whisper_cpp::plan_batches` (≤ 120 s of speech per batch) and `cumulative_progress`.
- `NORMALIZE_TARGET_PEAK` / `NORMALIZE_MAX_GAIN`, which Groq and `batch.rs` share.

### Tauri commands
None (crates expose no commands).

### Events
None. Progress goes out through the `on_progress` callback. The app turns it into `pipeline:progress` stage details (see [app-pipeline](app-pipeline.md)).

### Settings keys
None. The crate reads no settings. The app picks the engine (including the internal `asr.engine` flag), the language, model and GPU flag, and whether to use Groq, then builds the engine.

### Database
None.

### Files and directories written
| Path | When | Cleanup |
|---|---|---|
| `<input wav>` with extension `asrpart.json` (and `.asrpart.json.tmp` while writing) | After each finished batch when `resume: true` (`CheckpointFile::push`) | By the caller after a successful save (`pipeline::run_with`). The app always passes inputs under `<scratch_root>/<meeting_id>/`, so checkpoints never land in the meeting folder. |
| `%TEMP%/flyonthewall-whisper-<uuid>.wav` and `.json` | One per whisper batch (`run_whisper_cli`) | The wav is removed after the process exits; the json only on the success path. A crash mid-batch leaves both behind (inferred). Nothing sweeps them. |
| `%TEMP%/flyonthewall-parakeet-<uuid>.wav` | One per Parakeet batch | Removed after the process exits. A crash leaves it behind (inferred). |
| `%TEMP%/flyonthewall-spill-<uuid>.wav` | Each Groq quota spillover chunk | Removed after the local decode. |

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `https://api.groq.com/openai/v1/audio/transcriptions` (`GroqEngine::new`, the upload loop) | Each upload chunk of a `GroqEngine` run: a multipart WAV with `response_format=verbose_json`, word timestamps and `temperature=0`. | The crate has no gate. The app constructs `GroqEngine` only when Groq is on (`asr.use_groq` or tier `cloud`), a key is stored, and the meeting is not Keep local (`pipeline::cloud_asr_allowed`, used inside `run_with`). There is no `consent.*` prompt for Groq: the Settings toggle plus the key is the user action. |

The HTTP client is `reqwest::Client::new()` with **no timeout**. The local engines make no network calls; their binaries and models come through [app-models-gpu](app-models-gpu.md).

### External processes
| Process | Args | Notes |
|---|---|---|
| `whisper-cli` (CPU build, or the Vulkan/Metal build the app chooses) | `-m <model> -f <chunk.wav> -oj -of <base> -ml 1 -sow -t <threads> -l <lang or auto>`, plus optional `-mc <n>`, `--prompt <p>`, `-ng` (`run_whisper_cli`) | On Windows: `CREATE_NO_WINDOW` and `BELOW_NORMAL_PRIORITY_CLASS`. Stderr device lines are logged at debug. A non-zero exit becomes `AsrError::Engine` with 500 chars of stderr. No timeout. |
| `parakeet-cli` (beside `whisper-cli` in the CPU or Vulkan zip) | `-m <model> -f <chunk.wav> -t <threads> -ps`, plus `-ng` when `force_cpu` | Same Windows flags as `whisper-cli`. Words come from the `-ps` stderr token dump (10 ms ticks). If the CLI reports tokens but none parse, that is an error ("token dump format changed?"). No timeout. |

GPU vs CPU: the crate only knows `force_cpu` (`-ng`). The app chooses the executable and the flag per OS: a benchmark-gated Vulkan build on Windows, guarded Metal on Apple Silicon, otherwise CPU (`pipeline::local_tier_specs` / `local_tiers`). Parakeet tiers, when enabled, go ahead of the whisper chain and never pin whisper's GPU verdict (`pipeline::parakeet_tiers`).

### Secrets and keychain entries
None stored. `GroqEngine.api_key` comes from the caller: the app reads keychain entry `groq_api_key` inside `pipeline::run_with`. It is sent only as a Bearer header and is never logged.

## Dependencies
- **Inbound:**
  - [app-pipeline](app-pipeline.md): `pipeline.rs` builds the engine chain, including `parakeet_tiers`; `scheduler.rs` uses the markers.
  - [app-models-gpu](app-models-gpu.md): `gpu.rs` runs `WhisperCppEngine` for the GPU benchmark.
  - [app-recording](app-recording.md): `live.rs` runs live captions with `resume: false`.
  - The harnesses in `src-tauri/tests/accuracy_harness*`.
- **Outbound:**
  - [fly-audio](fly-audio.md): `vad::{detect_speech_spans, stitch_spans, map_to_original}` and `mix::{read_wav_mono, write_wav_mono_16, normalize_peak}`.
  - [fly-core](fly-core.md): `Word`.
  - reqwest, hound, tokio (process, sleep), uuid, serde_json.

## Invariants
- **Diarization is never delegated to the cloud.** Groq returns no speaker labels, and its word timestamps are mapped back so the local diarizer and aligner see the original timeline. Sources: ARCHITECTURE.md "Privacy boundaries"; the `groq.rs` module docs.
- **Local and cloud preprocessing match.** Same VAD, same normalize constants, cuts at speech gaps. Both local engines go through `batch::transcribe_batched`.
- **429 is transient and 4xx is permanent.**
  - 429 maps to `RateLimited`, never `Rejected`. Other 4xx map to `Rejected`, and 5xx maps to `Engine` (`retry::classify_http_failure`).
  - The app scheduler parks `REJECTED_MARKER` errors as permanent.
  - Tests: `retry::status_429_is_rate_limited_not_rejected`, `scheduler::rate_limited_cloud_errors_are_retryable`.
- **Cancel is cooperative and typed.**
  - Engines poll `cancel` between batches and inside cloud sleeps (about once a second), then return `AsrError::Cancelled`, whose Display equals `CANCELLED_MARKER`.
  - A request already in flight (an HTTP upload, or a `whisper-cli` or `parakeet-cli` run) is not interrupted.
- **Checkpoints are reused only for the identical plan.**
  - The key covers engine, model, language and the span layout. A mismatched or corrupt file starts empty.
  - A resumed run never mixes batches from a different engine or model.
  - Writes go to a tmp file that is then renamed. This is the response to the 586-minute import that re-decoded three times.
- **No prompt biasing.** The pipeline passes `prompt: None` and `max_context: Some(0)`. Do not synthesize a prompt in Groq.
- **Decoding never outranks capture.** Both sidecars run below normal priority on Windows.
- **Parakeet is opt-in and English only.** `parakeet-cli` has no language flag, so other languages and auto-detect stay on whisper (`pipeline::wants_parakeet`). A missing CLI or model falls back to whisper.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-asr/src/lib.rs` | 4 | Non-speech tokens, `>>` stripping, split `[ Silence ]` spans, an unmatched `[` keeps speech |
| unit | `crates/fly-asr/src/whisper_cpp.rs` | 10 | Batch planning, monotonic progress ending at 100 %, resume from a full checkpoint without launching the exe, cancel before the first batch, JSON parsing |
| unit | `crates/fly-asr/src/batch.rs` | 3 | Long-span cutting at the quiet point; short spans untouched; no sliver tail |
| unit | `crates/fly-asr/src/parakeet.rs` | 10 | Token glue into timed words, log lines ignored, hole detection (skipped speech vs silence), seam de-duplication, edge words, declared-token sums, model tag, cancel before decoding |
| unit | `crates/fly-asr/src/checkpoint.rs` | 6 | Plan-key sensitivity, reopen/resume, old-format compatibility, stale-key discard, corrupt file |
| unit | `crates/fly-asr/src/retry.rs` | 10 | 429/4xx/5xx classification, retry-after parsing, backoff, rate-limit budget, spill threshold, pacer math |
| unit | `crates/fly-asr/src/groq.rs` | 12 | verbose_json parsing, chunk overlap and stitching, offsets, resume progress with no network, cancel before upload, spillover decode, speech-gap cuts |
| ignored e2e/harness (never in CI) | `src-tauri/tests/pipeline_e2e.rs`, `rediarize_e2e.rs`, `scheduling_e2e.rs`, `accuracy_harness.rs` | 5 | The real `whisper-cli` (and, in the harness, Parakeet) via the app pipeline. They skip when the developer's data-dir artifacts are missing. |

**Known gaps:**
- **The CLI argument vectors aren't pinned.** No test pins either one (`-ml 1 -sow -oj`, `-mc 0`, `-ng`, `-l en`; Parakeet's `-ps`). Both are built inline, so dropping `-mc 0` (the anti-hallucination setting) passes CI.
- **The Groq upload loop has no fake-server test.** The loop runs pacer, then spill, then retry, then sleep. A 429 with `retry-after`, network backoff, the spill decision and quota-wait progress events are each tested only as pure functions.
- **Nothing can time out:** not the Groq request, not `whisper-cli`, not `parakeet-cli`. A hang stalls the app's single transcription worker, and Cancel can't reach it. This is untested and unmitigated.
- **Cancel tests stop too early.** Every cancel test cancels before the first batch, so cancelling after a decoded batch (checkpoint kept, run aborted) is untested.
- **Format changes go unnoticed in CI.** Changes to the real `whisper-cli` JSON, the Parakeet token dump or Groq's JSON shape are caught only by ignored tests.

## Changing it safely
- Run `cargo test -p fly-asr`. On a machine with artifacts, also run `cargo test -p fly-app --test pipeline_e2e -- --ignored --nocapture`.
- **Keep error text stable.** Don't reword the `Display` text of `AsrError::Rejected`, `RateLimited` or `Cancelled`: the app classifies errors by substring (`scheduler.rs`), so rewording silently changes retry behavior.
- **Some changes discard checkpoints.** Changing VAD, `MAX_BATCH_SPEECH_MS`, `MAX_CHUNK_BYTES` or the model tag changes `plan_key`, which throws away every in-progress checkpoint. That costs a re-decode, not data.
- **Progress semantics span three places.** `done_ms`/`total_ms` are speech milliseconds, plus `quota_wait_ms`. They feed `pipeline::transcribe_detail` and the frontend parser `frontend/src/importProgress.ts`, so change all three together.
- **A new local engine** should go through `batch::transcribe_batched`, so it inherits checkpoints, progress and cancel. It also needs an `id()` that differs from the existing engines, because the id keys the checkpoint.
- **Temp files:** if you add one, clean it up on the error path too. `%TEMP%` is never swept.
