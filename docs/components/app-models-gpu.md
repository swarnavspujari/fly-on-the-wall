# Artifacts, GPU selection and hardware detection (`app-models-gpu`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: None. #86 (merged) added `ENGINE_MODELS` (Parakeet TDT 0.6B v3 weights from huggingface.co, SHA-256 pinned) outside `registry()`, plus `engine_artifact`, `ensure_engine_model`, `PARAKEET_CLI_NAME`, `parakeet_cli_beside`, and split `ensure_with` into `ensure_artifact`. By design, `download_model` and `artifact()` never resolve the new ids. The Parakeet weights (`ggml-org/parakeet-GGUF` on huggingface.co, pinned by commit and SHA-256) are downloaded only when the pipeline enables Parakeet.

## Purpose
`models.rs` is the artifact manager: every sidecar binary (whisper.cpp, the Vulkan whisper build, sherpa-onnx, ffmpeg, Ollama) and every model weight (whisper ggml files, pyannote, CAM++) is downloaded on first use into `<data_dir>`, streamed with progress, accepted only if its SHA-256 matches a pin in source, and extracted in-process with path, link and size checks. `gpu.rs` decides, per machine and model, whether post-meeting transcription runs on the pinned Vulkan build (Windows), based on a one-time measured benchmark; any GPU failure pins the machine back to CPU. `hw.rs` detects RAM, cores and GPU to recommend an ASR tier.

## Files it owns
- `src-tauri/src/models.rs` — artifact registry and pins (`TOOLS` per OS, `MODELS`), `ensure*`, `tool_installed`, `find_on_path`, `best_installed_asr_model`, download / verify / mirror fallback, staged install, pruning, `sweep_stale_downloads`, in-process zip / tar extraction; 34 tests.
- `src-tauri/src/gpu.rs` — `asr.use_gpu` / `asr.gpu_bench` settings, `GpuBench`, `record_runtime_failure`, `is_apple_silicon` (macOS), `plan` + benchmark (Windows). No tests.
- `src-tauri/src/hw.rs` — `HwInfo`, `detect` (sysinfo + `nvidia-smi` + PowerShell/WMI), `hw.cache`, `default_model_for_tier`. No tests.
- `src-tauri/src/asr_commands.rs:401-417` — the `download_model` command only (the rest of that file belongs to [app-pipeline](app-pipeline.md)).

## Surface area
### Public API
**models.rs** (callers: `pipeline.rs`, `live.rs`, `scheduler.rs`, `import_commands.rs`, `screen_commands.rs`, `asr_commands.rs`, `ollama.rs`, `lib.rs`)
- Types: `Artifact { id, display, url, sha256, bytes, kind, dest_rel, probe_rel }`, `ArtifactKind::{File, Archive}`, `ModelProgress { id, downloaded, total, stage, error }`, `ProgressSink`, `DownloadEffort::{Full, SingleAttempt}` (`models.rs:12-34`, `388-395`, `596-604`).
- Registry: `registry()`, `artifact(id)` (`models.rs:270-276`); `WHISPER_ENGINE_ID = "whisper-bin"`, `WHISPER_CLI_NAMES` (`models.rs:281-282`).
- Resolution: `ensure(progress, data_dir, id)`, `ensure_with(…, effort)` (`models.rs:608-767`); `ensure_tool` / `ensure_tool_with` — installed managed copy → PATH → managed download → "`<name>` is not installed — `<guidance>`" (`models.rs:338-381`); `installed_path` (probe file exists) (`models.rs:383-386`); `tool_installed` (`models.rs:300-307`); `find_on_path` (also `/opt/homebrew/bin`, `/usr/local/bin` on macOS) (`models.rs:314-333`).
- `best_installed_asr_model(data_dir, wanted)` — largest installed whisper model at or below the wanted one, else the smallest larger one (`models.rs:405-422`; used by `pipeline.rs:403`).
- `sweep_stale_downloads(data_dir, max_age)` — called once per launch with 1 h (`lib.rs:132-143`) (`models.rs:432-466`).
- Markers shared with `scheduler.rs:80-81` and `frontend/src/pipelineNotice.ts:36`: `ENGINE_MISSING_MARKER = "is not installed"`, `DOWNLOAD_FAILED_MARKER = "download failed for"` (`models.rs:292-293`).
- `NO_HF_MIRROR_ENV = "FLYONTHEWALL_NO_HF_MIRROR"` (`models.rs:472`).

**gpu.rs** (callers: `pipeline.rs:235`, `pipeline.rs:672`, `pipeline.rs:1242-1272`, `pipeline.rs:1314`, `asr_commands.rs:306-311`, `asr_commands.rs:372-381`)
- `USE_GPU_KEY`, `BENCH_KEY`, `VULKAN_TOOL_ID = "whisper-bin-vulkan"`, `GpuBench { verdict, reason, gpu_secs, cpu_secs, model_id }` (`gpu.rs:17-36`).
- `enabled`, `stored`, `store`, `record_runtime_failure` (`gpu.rs:38-82`).
- `is_apple_silicon()` — `sysctlbyname("hw.optional.arm64")`, any failure = Intel = CPU (`gpu.rs:92-120`, macOS only).
- `plan(state, on_model, notify, PlanRequest)` → `Option<PathBuf>` of the Vulkan exe (`gpu.rs:157-283`, Windows only).

**hw.rs** (callers: `lib.rs:127`, `asr_commands.rs:257-268`, `pipeline.rs:229-230`, `pipeline.rs:327`, `pipeline.rs:387`)
- `HwInfo { ram_gb, cpu_cores, gpu_name, vram_mb, recommended_tier }`, `CACHE_KEY = "hw.cache"`, `cached`, `detect_and_cache`, `detect`, `default_model_for_tier(tier, max_quality)` (`hw.rs:12-140`).
- Tier rule: VRAM ≥ 8,192 MB or (RAM ≥ 16 GB and ≥ 12 cores) → `best`; RAM ≤ 8.5 GB → `light`; else `balanced` (`hw.rs:60-66`). `light` → `ggml-small-q5_1`, others → `ggml-large-v3-turbo-q5_0`, max quality → `ggml-large-v3-q5_0` (`hw.rs:130-140`).

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `download_model` | `id: String` (any `registry()` id) | `Result<String, String>` — installed probe path | `ui-app-shell` (`App.tsx:744`, engine install from the notice), `ui-settings` (`SettingsModal.tsx:545` model downloads, `SettingsModal.tsx:597` Ollama runtime install), via `api.downloadModel` (`api.ts:161`) |

`download_model` has no gate beyond the id being in the registry (`asr_commands.rs:403-417`): the webview can start any pinned download, including the 1.5 GB Ollama runtime. devMock mirrors it (`devMock.ts:1337-1343`) but returns `""` instead of a path and emits no `model:progress`.

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `model:progress` | Emitted by `download_model` (`asr_commands.rs:408-413`); the same `ModelProgress` sink is passed by pipeline, scheduler, live, import, screen, embeddings and Ollama code | `{ id, downloaded, total, stage: "downloading" \| "verifying" \| "extracting" \| "done" \| "error", error }` (throttled to one per 200 ms while downloading, `models.rs:560-569`) | `App.tsx:450` (`setModelProgress`), shown in Settings |
| `pipeline:progress` (details) | `gpu::plan`'s `notify` narrates the benchmark ("one-time GPU speed test…", "GPU 12 s vs CPU 30 s — …") (`gpu.rs:205-208`, `273-280`) | string detail | Owned by [app-pipeline](app-pipeline.md) |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `hw.cache` | R/W (`hw.rs:24-41`; also `asr_commands.rs:267`) | No | JSON `HwInfo` from the last detection; refreshed in the background each launch |
| `asr.use_gpu` | R here (`gpu.rs:38-45`); written by `set_asr_settings` | No | GPU transcription opt-out; default on (anything but `"false"`) |
| `asr.gpu_bench` | R/W (`gpu.rs:47-82`); cleared by `set_asr_settings` on off→on (`asr_commands.rs:372-377`) | No | JSON `GpuBench` verdict per (machine, model) |
| env `FLYONTHEWALL_NO_HF_MIRROR` | R (`models::hf_mirror_disabled`) | n/a | Any value but empty / `"0"` disables the hf-mirror.com fallback |

### Database
| Table | Columns | Read / write |
|---|---|---|
| `settings` | key, value (`hw.cache`, `asr.use_gpu`, `asr.gpu_bench`) | R/W |

### Files and directories written
- `<data_dir>/<id>.download.<pid>.<n>` — per-attempt temp download (`models.rs:625-630`); removed on failure, renamed (file) or deleted after extraction (archive); stale ones (untouched ≥ 1 h) swept at launch (`models.rs:440-465`).
- `<data_dir>/bin/<tool>/…` — tool installs: `bin/whisper/v1.9.5`, `bin/whisper-vulkan/v1.9.5`, `bin/sherpa`, `bin/ffmpeg`, `bin/ollama` (`models.rs:47-202`). Staged in `<dest>.partial`, then promoted entry by entry, probe-holding entry last (`models.rs:784-874`).
- Superseded installs under `bin/<whisper|whisper-vulkan|ffmpeg|sherpa>/` are **deleted** after a successful install and at each launch sweep (never following links) (`models.rs:880-935`).
- `<data_dir>/models/asr/ggml-*.bin`, `<data_dir>/models/diarize/{sherpa-onnx-pyannote-segmentation-3-0/, campplus.onnx}` (`models.rs:206-267`).
- `%TEMP%/flyonthewall-gpu-bench-<pid>.wav` (Windows) — up to 60 s of speech cut from the meeting being transcribed, deleted after the benchmark (`gpu.rs:298-323`, `gpu.rs:244`, `255`, `259`). It is outside `<data_dir>`: a crash mid-benchmark leaves meeting audio in the system temp folder, where "Delete all my data" and audio retention don't look (inferred).

### Network egress
All downloads are HTTPS GET, 8 s connect / 30 s read timeouts, no overall timeout (`models.rs:668-672`). Acceptance is the SHA-256 pin, never the host. GitHub and Hugging Face redirect to their CDNs (inferred).

| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `github.com/ggml-org/whisper.cpp` (release `b5454`) | Windows CPU whisper engine, first transcription or engine install | None beyond starting a transcription / clicking Install |
| `github.com/swarnavspujari/fly-on-the-wall` (`tools-whisper-v1.9.5`, `tools-whisper-vulkan-v1.9.5`) | macOS whisper engine; Windows Vulkan build on the first post-meeting transcription with `asr.use_gpu` on (default) | `asr.use_gpu` (Windows Vulkan); none for the macOS engine |
| `github.com/k2-fsa/sherpa-onnx` | sherpa CLI (all OS) and the pyannote / CAM++ models, first diarization | Transcription |
| `github.com/BtbN/FFmpeg-Builds` | ffmpeg (Windows, Linux), first screen recording or media import | User action |
| `github.com/ollama/ollama` | Ollama runtime (Windows 1.5 GB, macOS 129 MB) | User clicks Install in Settings (`download_model("ollama-bin")`) |
| `huggingface.co/ggerganov/whisper.cpp` | Whisper weights (small, medium, large-v3-turbo, large-v3) | Transcription, or Settings download |
| `hf-mirror.com` (community proxy, not affiliated) | Only after both huggingface.co attempts fail (`models.rs:482-494`; docs/MODELS.md "Download sources") | Opt-out via `FLYONTHEWALL_NO_HF_MIRROR` |

Keep-local doesn't apply: nothing about the user's meetings is sent. Downloads go out under the user's IP whenever the artifact is missing.

### External processes
| Process | When | Notes |
|---|---|---|
| `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits` | `hw::detect`: each launch in the background (`lib.rs:121-130`), or on the first `get_asr_settings` before the cache exists | Resolved from PATH; spawned **without** `CREATE_NO_WINDOW` (`hw.rs:78-83`), unlike the PowerShell call, so it may flash a console window on Windows (inferred) |
| `powershell -NoProfile -Command "(Get-CimInstance Win32_VideoController …)"` | Windows, when nvidia-smi finds nothing | `CREATE_NO_WINDOW` (`hw.rs:106-115`) |
| `whisper-cli` (Vulkan build, then CPU build) | `gpu::plan` benchmark on the 60 s sample, once per (machine, model) | Spawned through `fly_asr::whisper_cpp::WhisperCppEngine` with `resume: false` (`gpu.rs:210-228`) |
| none for extraction | Archives are extracted in-process (zip, tar.bz2, tgz, tar.xz) | Deliberately no bsdtar (`models.rs:955-990`) |

The installed binaries are spawned by their callers ([app-pipeline](app-pipeline.md), [app-screen-recording](app-screen-recording.md), [app-import](app-import.md), [app-ollama](app-ollama.md)).

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** [app-pipeline](app-pipeline.md) (engine/model resolution, tier → model, GPU plan and runtime-failure pin), `live.rs` (single-attempt engine/model for live captions), [app-import](app-import.md) and [app-screen-recording](app-screen-recording.md) (ffmpeg), `scheduler.rs` (permanent-failure markers), [app-ollama](app-ollama.md) (`ollama-bin` artifact, PATH lookup, progress type), `lib.rs` (startup hardware warm-up and download sweep), [ui-settings](ui-settings.md) and [ui-app-shell](ui-app-shell.md) via `download_model`.
- **Outbound:** reqwest, sha2, hex, zip, tar, bzip2, flate2, xz2, futures-util, tokio; `fly_storage::safe_path::is_plain_name` (entry-name rules); [fly-asr](fly-asr.md) and [fly-audio](fly-audio.md) (benchmark run, VAD, resample, WAV I/O); sysinfo; [fly-storage](fly-storage.md) settings.

## Invariants
- **Nothing is bundled; everything downloaded is SHA-256 pinned.** docs/MODELS.md ("Model registry"), DECISIONS.md 2026-07-01 ("Checksums pinned from upstream metadata"), docs/SECURITY_AUDIT_2026-10.md "Verified good practices". Enforced at `models.rs:583-590`; the digest is computed over the network stream, and each attempt has its own temp file so a verified stream can't be swapped for interleaved bytes (`models.rs:616-630`).
- **A wrong or compromised mirror can only fail the checksum.** docs/MODELS.md "Download sources"; a mismatch skips straight to the next source (`models.rs:499-509`, `models.rs:693-697`).
- **Archives can't write outside their destination.** Zip: raw names must be plain relative components, symlinks rejected, only the exec bit honored, bytes written capped (`models.rs:1003-1082`). Tar: hard links rejected, symlinks must be descending plain names, `unpack_in` must accept every entry (`models.rs:1084-1151`). Extraction is capped at max(10 × archive size, 1 GiB) (`models.rs:937-949`). docs/SECURITY_AUDIT_2026-10.md Round 3 ("zip 9.0.5 … hardened").
- **An install is all-or-nothing.** The probe file never exists before everything it needs (`models.rs:777-874`).
- **Pruning stays inside `bin/<tool>` and never follows links** (`models.rs:895-935`). Ollama is never pruned (its archive root is `bin/ollama`, `models.rs:876-880`).
- **CPU is canon; the GPU is used only when measured faster on this machine, per model.** DECISIONS.md 2026-07-08; docs/MODELS.md "GPU transcription". `gpu.rs:153-283`. A failed GPU benchmark pins CPU; a failed CPU benchmark stores nothing; an unavailable Vulkan download stores nothing (`gpu.rs:184-192`, `230-258`). A GPU failure mid-pipeline pins CPU (`gpu.rs:66-82`, called at `pipeline.rs:672`).
- **Intel Macs never use Metal** (silent corruption seen on the Intel smoke test) (`gpu.rs:84-120`, `pipeline.rs:1272`).
- **Detection never sits on a command path.** `hw::detect` runs in the background at launch and is cached (`hw.rs:1-7`, `lib.rs:121-130`); `get_asr_settings` runs it off the IPC thread only on the very first launch (`asr_commands.rs:249-270`).
- **Tool releases on this repo stay prerelease** so `/releases/latest` keeps pointing at the app (`.github/workflows/build-whisper-sidecar.yml:16-17`); see [app-updater](app-updater.md).
- **Markers are shared, not copied.** `ENGINE_MISSING_MARKER` / `DOWNLOAD_FAILED_MARKER` are matched by `scheduler.rs:80-81`; the frontend matches the same phrase (`pipelineNotice.ts:36`) (`models.rs:284-293`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/models.rs` | 33 (30 on Windows; 3 are `cfg(unix)`) | In-process extraction for each format; zip/tar path, symlink, hard-link, absolute-path rejection; size caps; exec-bit only; staged install, broken-install repair, failed-extract cleanup; prune plans, prune-without-current, link safety; temp-name uniqueness; mirror list and opt-out; checksum-mismatch retry predicate; best-installed fallback; stale-temp sweep; `tool_installed` |
| ignored e2e/harness | `src-tauri/src/models.rs` `pinned_tool_archives_download_verify_and_extract` | 1 | Every pinned tool archive for this OS (except Ollama) downloads, matches its pin and extracts |
| unit (in [app-pipeline](app-pipeline.md)) | `src-tauri/src/pipeline.rs` `pinned_cpu_verdict_tracks_the_model_it_was_recorded_for` | 1 | `gpu::record_runtime_failure` + `gpu::stored` pin is per model |
| ignored e2e/harness | `src-tauri/tests/pipeline_e2e.rs` `gpu_failure_falls_back_to_cpu` | 1 | Windows: planted broken Vulkan exe → CPU fallback and CPU pin. Also needs real artifacts in `<data_dir>` (early-returns "SKIP" without them) |
| ignored e2e/harness | `src-tauri/tests/clean_machine_extract_e2e.rs` | 1 | Lazy download + in-process `.tar.bz2` extraction on a clean data dir |
| unit | `src-tauri/src/gpu.rs`, `src-tauri/src/hw.rs` | 0 | — |

**Known gaps:**
- **The checksum gate itself never runs in CI.** No test feeds `download_and_verify` bytes that don't match (for example from a local HTTP listener) and asserts rejection plus temp cleanup. `checksum_mismatch_advances_to_next_source` tests only the retry predicate on a hand-written string. Inverting `digest != a.sha256` would pass CI. Risk: supply-chain integrity rests on one untested comparison.
- **Pins are cross-checked against nothing.** No test asserts each `sha256` is 64 lowercase hex chars, that `bytes` is non-zero, or that `probe_rel` starts with `dest_rel`. A pin changed to an attacker's hash (with a matching hosted file) is caught only by code review; the ignored network E2E would pass.
- **`gpu.rs` and `hw.rs` have zero tests.** The benchmark decision (`gpu_secs < cpu_secs`), "no verdict on CPU failure / offline", verdict-model matching in `plan`, the bench-sample cleanup, `detect_nvidia` CSV parsing and the tier thresholds are untested. The only GPU fallback test is `#[ignore]` and needs real artifacts.
- **`download_model` has no test**, and devMock never emits `model:progress`, so the progress UI is only exercised by hand.
- An installed file is never re-verified: `installed_path` checks only that the probe exists (`models.rs:383-386`), so a truncated or corrupted weight file stays "installed".
- `candidate_urls_mirror_only_for_hf` reads `FLYONTHEWALL_NO_HF_MIRROR` from the real environment, so it fails on a machine that sets it.

## Changing it safely
- **Bumping a pin:** re-hash the hosted asset independently (DECISIONS.md 2026-07-01), update `url`, `sha256`, `bytes`, and version the `dest_rel` / `probe_rel` dir for engines (an unversioned path leaves existing installs on the old build, `models.rs:42-46`). Then run `cargo test -p fly-app --lib pinned_tool_archives -- --ignored` on each OS you changed. Update docs/MODELS.md tables.
- **Tools hosted on this repo** (`tools-whisper*`) must be published as **prereleases** or the updater breaks ([app-updater](app-updater.md)).
- **New archive formats** go through `extract_archive` (`models.rs:961-990`) with the same caps and path rules; never shell out to tar/bsdtar.
- **New tool with a versioned layout:** add it to `PRUNABLE_TOOLS` only if its probe is `bin/<tool>/<version>/…`; `prune_plans_only_versioned_tool_layouts` checks every real pin.
- **Error wording:** the two markers are matched by `scheduler.rs` and `pipelineNotice.ts`; change all three together.
- **GPU verdict shape** is persisted JSON in `asr.gpu_bench`; renaming fields silently resets every machine to "no verdict" (`stored` drops unparsable JSON). devMock's `fotwMockGpuBench` mirrors the shape (`devMock.ts:602-610`).
- **Spawning new console tools on Windows:** set `CREATE_NO_WINDOW` (as `hw.rs:113` does and `hw.rs:78` doesn't).
- `ENGINE_MODELS` (from #86) is a second, separate registry. Keep `download_model` and `best_installed_asr_model` blind to it. Engine weights are fetched only through `ensure_engine_model`, when the pipeline enables Parakeet.
