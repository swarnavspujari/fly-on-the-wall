# Speech recognition engines (`fly-asr`)

> Layer: crate. Described at main `82b7edf` (v2.4.0).
> In flight: PR #86 (open, v2.5) adds `batch.rs` (multi-engine ASR harness) and `parakeet.rs` (a dormant Parakeet engine), and edits `whisper_cpp.rs`; nothing in the app constructs Parakeet yet. Not described below.

## Purpose
Defines the `TranscriptionEngine` trait and its two shipped backends: local whisper.cpp (the `whisper-cli` sidecar) and Groq cloud ASR. Both apply the same preprocessing: VAD drops silence, each batch is peak-normalized, and word timestamps are mapped back to the original timeline. Both checkpoint finished batches so a crash, restart or failover resumes instead of re-decoding. The crate only returns words with timestamps. Speaker turns come from [fly-diarize](fly-diarize.md), which always runs locally.

## Files it owns
- `crates/fly-asr/Cargo.toml`: deps (fly-audio, fly-core, reqwest, hound, tokio, uuid).
- `crates/fly-asr/src/lib.rs`: the trait, `AsrError` and its string markers, options/progress/cancel types, and the non-speech token filters.
- `crates/fly-asr/src/whisper_cpp.rs`: `WhisperCppEngine` (VAD batching, the `whisper-cli` invocation, JSON parsing).
- `crates/fly-asr/src/groq.rs`: `GroqEngine` (upload chunking, overlap stitching, quota pacing, local spillover).
- `crates/fly-asr/src/retry.rs`: the pure cloud retry and pacing policy (429/4xx/5xx classification, backoff, `QuotaPacer`).
- `crates/fly-asr/src/checkpoint.rs`: per-batch `*.asrpart.json` checkpoints keyed by the decode plan.

## Surface area
### Public API
| Group | Items | Where |
|---|---|---|
| Trait | `TranscriptionEngine { id(), is_local(), transcribe(), transcribe_with_progress(on_progress, cancel) }` | `crates/fly-asr/src/lib.rs:233-252` |
| Errors | `AsrError::{ModelMissing, BadAudio, Engine, Network, Rejected, RateLimited{message, retry_after}, Cancelled, Io}`, `Result<T>` | `lib.rs:20-54` |
| String markers (matched by the app scheduler) | `REJECTED_MARKER = "cloud ASR rejected the request"`, `CANCELLED_MARKER = "transcription cancelled"` | `lib.rs:59`, `lib.rs:63` |
| Options and data | `TranscribeOptions { language, prompt, max_context }`, `RawTranscript { language, words, segments }`, `RawSegment` | `lib.rs:66-75`, `lib.rs:191-205` |
| Progress and cancel | `TranscribeProgress { done_ms, total_ms, quota_wait_ms }`, `TranscribeProgressFn`, `CancelFn` | `lib.rs:210-228` |
| Token hygiene | `is_non_speech_token`, `clean_word_text`, `drop_non_speech_spans` | `lib.rs:81-94`, `lib.rs:165-188` |
| whisper.cpp | `WhisperCppEngine { exe, model, threads, force_cpu, resume }`, `parse_whisper_json` | `whisper_cpp.rs:26-41`, `whisper_cpp.rs:265` |
| Groq | `GroqEngine { api_key, model, base_url, resume, local_spillover }`, `GroqEngine::new`, `GROQ_DEFAULT_MODEL = "whisper-large-v3-turbo"`, `parse_groq_verbose_json` | `groq.rs:20-56`, `groq.rs:545` |
| Retry policy | `classify_http_failure`, `parse_retry_hint`, `retry_delay`, `spill_to_local`, `QuotaPacer`, constants (`FREE_TIER_AUDIO_MS_PER_HOUR`, `QUOTA_SAFETY_MARGIN_MS`, `MAX_RATE_LIMIT_WAIT` = 20 min, `SPILL_TO_LOCAL_THRESHOLD_MS` = 30 s) | `retry.rs:13-201` |
| Checkpoints | `BatchResult { language, words, engine }`, `CheckpointFile::{open, resumed, push}`, `checkpoint_path_for`, `plan_key` | `checkpoint.rs:21-123` |

Crate-private but load-bearing: `plan_batches` (≤120 s of speech per whisper batch, `whisper_cpp.rs:307-324`), `cumulative_progress` (`whisper_cpp.rs:331-349`), and `NORMALIZE_TARGET_PEAK` / `NORMALIZE_MAX_GAIN`, which Groq shares (`whisper_cpp.rs:22-24`, `groq.rs:17`).

### Tauri commands
None (crates expose no commands).

### Events
None. Progress goes out through the `on_progress` callback. The app turns it into `pipeline:progress` stage details (see [app-pipeline](app-pipeline.md)).

### Settings keys
None. The crate reads no settings. The app picks the language, model, GPU flag and Groq use, then builds the engine.

### Database
None.

### Files and directories written
| Path | When | Cleanup |
|---|---|---|
| `<input wav>` with extension `asrpart.json` (and `.asrpart.json.tmp` while writing) | after each finished batch when `resume: true` (`checkpoint.rs:49-51`, `checkpoint.rs:104-122`) | by the caller after a successful save (`src-tauri/src/pipeline.rs:704-709`). The app always passes inputs under `<scratch_root>/<meeting_id>/`, so checkpoints never land in the meeting folder. |
| `%TEMP%/flyonthewall-whisper-<uuid>.wav` and `.json` | one per whisper batch (`whisper_cpp.rs:180-183`) | the wav is removed after the process exits (`whisper_cpp.rs:226`); the json only on the success path (`whisper_cpp.rs:238-240`). A crash mid-batch leaves both behind (inferred). Nothing sweeps them. |
| `%TEMP%/flyonthewall-spill-<uuid>.wav` | each quota spillover chunk (`groq.rs:398-402`) | removed after the local decode |

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `https://api.groq.com/openai/v1/audio/transcriptions` (`groq.rs:51`, `groq.rs:438-445`) | each upload chunk of a `GroqEngine` run. Multipart WAV with `response_format=verbose_json`, word timestamps and `temperature=0`. | The crate has no gate. The app constructs `GroqEngine` only when Groq is on (`asr.use_groq` or tier `cloud`), a key is stored, and the meeting is not Keep local (`src-tauri/src/pipeline.rs:148-150`, `pipeline.rs:274-310`). There is no `consent.*` prompt for Groq; the Settings toggle plus the key is the user action. |

The HTTP client is `reqwest::Client::new()` with **no timeout** (`groq.rs:438`).

### External processes
| Process | Args | Notes |
|---|---|---|
| `whisper-cli` (CPU build, or the Vulkan/Metal build chosen by the app) | `-m <model> -f <chunk.wav> -oj -of <base> -ml 1 -sow -t <threads> -l <lang or auto>` plus optional `-mc <n>`, `--prompt <p>`, `-ng` (`whisper_cpp.rs:186-212`) | Windows: `CREATE_NO_WINDOW` and `BELOW_NORMAL_PRIORITY_CLASS` (`whisper_cpp.rs:213-220`). Stderr device lines (`ggml_vulkan`/`ggml_metal`/`ggml_cuda`) are logged at debug (`whisper_cpp.rs:248-261`). Non-zero exit becomes `AsrError::Engine` with 500 chars of stderr. No timeout. |

GPU vs CPU: the crate only knows `force_cpu` (`-ng`). The app chooses the exe and the flag per OS: Windows uses a benchmark-gated Vulkan build; Apple Silicon uses guarded Metal; otherwise CPU (`src-tauri/src/pipeline.rs:1138-1208`).

### Secrets and keychain entries
None stored. `GroqEngine.api_key` comes from the caller (the app reads keychain entry `groq_api_key`, `src-tauri/src/pipeline.rs:292-296`). It is sent only as a Bearer header (`groq.rs:441`) and is never logged.

## Dependencies
- **Inbound:** [app-pipeline](app-pipeline.md) (`src-tauri/src/pipeline.rs`, the engine chain and markers; `scheduler.rs` uses the markers), [app-models-gpu](app-models-gpu.md) (`src-tauri/src/gpu.rs:129-211`, the GPU benchmark runs `WhisperCppEngine`), [app-recording](app-recording.md) (`src-tauri/src/live.rs:171-246`, live captions, `resume: false`), and `src-tauri/tests/accuracy_harness.rs`.
- **Outbound:** [fly-audio](fly-audio.md) (`vad::{detect_speech_spans, stitch_spans, map_to_original}`, `mix::{read_wav_mono, write_wav_mono_16, normalize_peak}`), [fly-core](fly-core.md) (`Word`), reqwest, hound, tokio (process, sleep), uuid, serde_json.

## Invariants
- **Diarization is never delegated to the cloud.** Groq returns no speaker labels, and its word timestamps are mapped back so the local diarizer and aligner see the original timeline. Sources: ARCHITECTURE.md "Privacy boundaries"; `groq.rs:1-9`, `groq.rs:221-229`.
- **Local and cloud preprocessing match.** Same VAD, same normalize constants, cuts at speech gaps (`groq.rs:80-87`, `groq.rs:105`, `groq.rs:199-205`; constants at `whisper_cpp.rs:19-24`).
- **429 is transient and 4xx is permanent.** 429 maps to `RateLimited`, never `Rejected`; other 4xx map to `Rejected`; 5xx maps to `Engine` (`retry.rs:51-69`). The app scheduler parks `REJECTED_MARKER` errors as permanent (`src-tauri/src/scheduler.rs:77-82`). Tests: `retry::status_429_is_rate_limited_not_rejected`, `scheduler::rate_limited_cloud_errors_are_retryable`.
- **Cancel is cooperative and typed.** Engines poll `cancel` between batches and inside cloud sleeps (about once a second, `groq.rs:265-279`), then return `AsrError::Cancelled`, whose Display equals `CANCELLED_MARKER` (`lib.rs:48-49`, `lib.rs:63`). A request already in flight (an HTTP upload or a `whisper-cli` run) is not interrupted.
- **Checkpoints are only reused for the identical plan.** The key covers engine, model, language and the span layout; a mismatched or corrupt file starts empty (`checkpoint.rs:55-95`). Writes go to a tmp file and are then renamed (`checkpoint.rs:112-118`). This is the response to the 586-minute import that re-decoded three times (`checkpoint.rs:1-11`).
- **No prompt biasing.** The pipeline passes `prompt: None` and `max_context: Some(0)`. Do not synthesize a prompt in Groq (`groq.rs:430-433`; `src-tauri/src/pipeline.rs:263-269`).
- **Decoding never outranks capture.** The sidecar runs below normal priority (`whisper_cpp.rs:213-220`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-asr/src/lib.rs` | 4 | non-speech tokens, `>>` stripping, split `[ Silence ]` spans, unmatched `[` keeps speech |
| unit | `crates/fly-asr/src/whisper_cpp.rs` | 10 | batch planning, monotonic progress ending at 100%, resume from a full checkpoint without launching the exe, cancel before the first batch, JSON parsing |
| unit | `crates/fly-asr/src/checkpoint.rs` | 6 | plan-key sensitivity, reopen/resume, old-format compatibility, stale-key discard, corrupt file |
| unit | `crates/fly-asr/src/retry.rs` | 10 | 429/4xx/5xx classification, retry-after parsing, backoff, rate-limit budget, spill threshold, pacer math |
| unit | `crates/fly-asr/src/groq.rs` | 12 | verbose_json parsing, chunk overlap and stitching, offsets, resume progress with no network, cancel before upload, spillover decode, speech-gap cuts |
| ignored e2e/harness (never in CI) | `src-tauri/tests/pipeline_e2e.rs`, `rediarize_e2e.rs`, `scheduling_e2e.rs`, `accuracy_harness.rs` | 5 | the real `whisper-cli` via the app pipeline; skips when `%APPDATA%` artifacts are missing |

**Known gaps:**
- No test pins the `whisper-cli` argument vector (`-ml 1 -sow -oj`, `-mc 0`, `-ng`, `-l en`). It is built inline in `run_whisper_cli` (`whisper_cpp.rs:186-212`), so a dropped `-mc 0` (the anti-hallucination setting) passes CI.
- The Groq upload loop (pacer, then spill, then retry, then sleep; `groq.rs:304-381`) has no test against a fake HTTP server. 429 with `retry-after`, network backoff, the spill decision, and quota-wait progress events are each tested only as pure functions.
- No timeout on the Groq request (`groq.rs:438`) or the `whisper-cli` process (`whisper_cpp.rs:222-225`). A hang stalls the app's single transcription worker, and Cancel cannot reach it. This is both untested and unmitigated.
- Both cancel tests cancel before the first batch. Cancelling after a decoded batch (checkpoint kept, run aborted) is untested.
- Changes to the real `whisper-cli` or Groq JSON shape are caught only by ignored tests.

## Changing it safely
- Run `cargo test -p fly-asr`. On a machine with artifacts, also run `cargo test -p fly-app --test pipeline_e2e -- --ignored --nocapture`.
- Keep the `Display` text of `AsrError::Rejected`, `RateLimited` and `Cancelled` stable. The app classifies errors by substring (`src-tauri/src/scheduler.rs:77-89`), so rewording silently changes retry behavior.
- Changing VAD, `MAX_BATCH_SPEECH_MS`, `MAX_CHUNK_BYTES` or the model tag changes `plan_key`, which discards every in-progress checkpoint (costs a re-decode, not data).
- Progress semantics (`done_ms`/`total_ms` are speech milliseconds; `quota_wait_ms`) feed `transcribe_detail` in `src-tauri/src/pipeline.rs:932-956` and the frontend parser `frontend/src/importProgress.ts`. Change all three together.
- If you add a temp file, add its cleanup on the error path too. `%TEMP%` is never swept.
- PR #86 touches `whisper_cpp.rs` and adds two engines. Rebase carefully and keep `resume`/checkpoint semantics for any new batch engine.
