# Recording storage v2 (`app-audio-store`)

> Layer: backend feature (src-tauri). Added on top of main after #105 (manifest v3).
> In flight: this is the change itself (recording storage v2, spec §4.6 / v2.5 row 4.z, made lossless). PR #112 adds `AppState::background_should_yield()`; `busy_reason` is its interim copy and becomes a one-line call once #112 merges.

## Purpose
Finished recordings are kept as **lossless FLAC** instead of WAV, and the two derived files capture writes (the 16 kHz ASR mixdown and the native-rate playback mix) are no longer stored when they can be rebuilt bit-identically. A low-priority background job ("Compress recordings") does this on every install, for old meetings and for each new one once its transcript is final. Every reader accepts WAV or FLAC, so nothing about playback, re-transcription, re-diarization or portability changes. This module also owns picking (or rebuilding) the file the editor's player streams, and the Settings › Storage report.

The user's requirement is "compress without losing anything". The spec's earlier plan (16 kHz FLAC sources + lossy M4A playback) was replaced: sources keep their original sample rate and bit depth, and every FLAC is decoded again and compared sample for sample with its WAV before the WAV is deleted.

## Files it owns
- `src-tauri/src/audio_store.rs`: readers' helpers (`is_storage_v2`, `regenerate_mixdown_for_asr`), the per-meeting commit protocol (`compress_meeting`, `needs_work`), the job (`spawn`, `run_pass`, pause rules), playback (`prepare_playback`, the scratch cache), the storage report, 3 commands.
- `crates/fly-audio/src/flac.rs`: strict WAV layout check, streaming FLAC encoder (flacenc), independent verifier (claxon), streaming FLAC reader. See [fly-audio](fly-audio.md).
- `crates/fly-audio/src/derive.rs`: streaming, bit-identical regeneration of the mixdown and the playback mix; `matches_stored`. See [fly-audio](fly-audio.md).
- `crates/fly-storage/src/recording_store.rs`: folder leases, `meetings_with_stored_audio`, `recording_file_in_folder` (containment), `replace_meeting_recording` (the manifest on disk with the new recording, then the row). See [fly-storage](fly-storage.md).
- `crates/fly-audio/examples/flac_bench.rs`: size/speed/bit-exactness on copies of real meetings.

## The file map
Every recording file, who writes it and who reads it, before and after storage v2.

| File (in `recordings/<date> <title>/` unless noted) | Written by | Read by | Storage v2 |
|---|---|---|---|
| `recording.mic.wav` | capture (`fly-audio` `cpal_backend`, the shared writer the loopback backends use), for the whole recording | live captions (`live.rs` `take_chunk`, growing file, only while recording); stop-time mixdown and playback mix (`mix::write_mixdown`, `write_playback_mix`); pipeline `prep` (per-channel path, and the single-track fallback); GPU speed test sample (`gpu.rs`); re-diarize (single-track fallback); the player (last fallbacks); open/reveal in the OS (`open_attachment`, `reveal_attachment`) | → `recording.mic.flac`, same rate and bit depth, verified sample for sample, same modified time |
| `recording.system.wav` | capture (loopback) | same as mic; it's the diarized channel in the per-channel path | → `recording.system.flac` |
| `recording.mixed.wav` | capture at stop (16 kHz mix of both channels); media import (`import_commands.rs`: the joined 16 kHz track of all imported files) | pipeline single-track path (mic-only captures, imports); re-diarize single-track path; GPU sample (third choice); player (second fallback); Editor "Show in folder" / "Open in your player" (see below) | capture: **dropped** when it regenerates bit-identically from the FLAC sources (`mixed_path: null`), else → `.flac`. Import: no sources, so it **is** a source → `recording.mixed.flac` |
| `recording.playback.wav` | capture at stop, only with both channels (mic-only: `playback_path` points at the mic file) | the player only (and "open in your player") | **dropped** when it regenerates bit-identically (`playback_path: null`), else → `.flac`. Mic-only: `playback_path` follows the mic to `recording.mic.flac` |
| imported originals (`<user's name>.mp4/.m4a/.wav…`) | `import_stage` copies | the import's conversion step (ffmpeg), the user | untouched (they're the user's files; videos are reported in Settings) |
| `recording.manifest.json` | stop (stash), edits (refresh), startup backfill | self-heal / portability | rewritten at commit, **before** `recording_json`, so it never names deleted files. The manifest on disk is kept and only its `recording` and `version` change; fields an older manifest lacks are filled from the rows (manifest v3: Keep local and the folder flags), Keep local is merged so it only ever stays on, a manifest naming another meeting is refused, a missing or unreadable one is written from the rows. Compression adds no field and never lowers the version (a folder whose manifest is newer is left alone) |
| `<scratch>/<meeting id>/{mic,system,track,rediarize}.16k.wav`, `*.asrpart.json` | pipeline / re-diarize | ASR engines, diarizer | unchanged (always WAV, machine-local, deleted after a successful run) |
| `<scratch>/<meeting id>/mixdown.16k.wav` | **new**: `regenerate_mixdown_for_asr` (single-track run of a compressed recording) | pipeline `prep`, re-diarize | deleted with the other intermediates |
| `<scratch>/<meeting id>/playback.wav` | **new**: `prepare_playback(build: true)`: the rebuilt mix of a compressed two-channel recording, or any FLAC the player would otherwise stream (mic-only, import, a kept mix) decoded to WAV with `flac::decode_to_wav` | the player (the asset scope allows this one file) | a cache: newest-first, up to 1.5 GB in total; goes with the meeting's scratch (purge, retention, delete-all, startup sweep) |
| `attachments/<note>/screen-*.mp4` (+ `.jpg` poster) | screen recording | the video lightbox | untouched (reported in Settings) |

"Open in your player" and "Show in folder" (`Editor.tsx` `audioPath`: playback, then mixed, then mic) point at **`recording.mic.flac`** for a compressed two-channel meeting, since its mix is no longer stored: the OS player opens the microphone channel only. Proposal (not built): an "Export audio" action that writes the rebuilt mix to a place the user picks.

Not readers of audio: the MCP server (database and transcript mirrors only), `export_note` / `export_transcript_vtt` (text), search and embeddings.

## Surface area
### Public API
| Item | Called by |
|---|---|
| `is_storage_v2(&RecordingRef)`: a source is `.flac`, so absent derived files regenerate | pipeline, `prepare_playback`, tests |
| `regenerate_mixdown_for_asr(rec, mixed, mic, system, work_dir) -> Result<Option<PathBuf>>` | `pipeline::run_with` (single-track path), `pipeline::re_diarize_with` |
| `compress_meeting(&mut MeetingCtx, id) -> Result<MeetingOutcome, MeetingError>`, `needs_work(data_dir, rec)`, `needs_work_skipping(data_dir, rec, skip_leftover)`. `MeetingCtx` carries the checkpoint, the commit-time check, `skip_leftover` and `free_space`; `MeetingError` is `Busy` (later), `Stopped`, `Gone` (deleted meanwhile; its new FLACs removed) or `Failed` | the job; `src-tauri/tests/compress_recordings.rs`, `compress_e2e.rs` |
| `free_space(path) -> Option<u64>` (sysinfo; `None` never blocks) | the job, `compress_e2e.rs` |
| `spawn(app)` | `lib.rs` setup |
| `StoreState` (`AppState.audio_store`): `nudge()`, `status()`, `current()` | scheduler (nudge after a transcript is final), privacy (retention busy set) |
| `stop_meeting(state, id, timeout)`, `halt(state, timeout)`, `resume(state)` | `delete_note` (before the purge), `delete_all_data` (halts first; resumes when the wipe is refused or fails) |

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `prepare_playback` | `meeting_id: String, build: bool` (stored WAV → its path; FLAC or a dropped mix → `needs_build` until rebuilt into the cache) | `Result<Option<PlaybackSource { path: Option<String>, needs_build: bool }>, String>` (absolute path; `None` = no audio) | `ui-editor` (`Editor.tsx`, on note open with `build: false`; on Play with `build: true`), via `api.preparePlayback` |
| `storage_report` | none | `Result<StorageReport, String>` (audio, WAV part, imported videos, screen recordings, other attachments, `compress: CompressStatus`) | `ui-settings` (`StorageRows.tsx`), `ui-notices` (`CompressNotice.tsx`), via `api.storageReport` |
| `dismiss_compress_notice` | none | `Result<(), String>` | `ui-notices` (`CompressNotice.tsx`), via `api.dismissCompressNotice` |

devMock mirrors all three (`devMock.ts`, flags `fotwMockCompress` = `working`/`paused`/`done`, `fotwMockCompressNotice`, `fotwMockCompressed`, `fotwMockCompressFailed`; sizes are made up).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `storage:compress` | emits whenever the job's status changes | `CompressStatus { state, paused_for, meetings_total, meetings_done, meetings_skipped, meetings_failed, bytes_total, bytes_done, saved_bytes, kept_files, notice_pending, last_error }` (the counts and `last_error` are this pass's) | `StorageRows.tsx`, `CompressNotice.tsx` |

### Settings keys
| Key | Read / write | Webview-settable? | Meaning |
|---|---|---|---|
| `storage.compress.saved_bytes` | R/W (job) | no (`storage.` isn't an allowed prefix) | bytes freed so far, across runs |
| `storage.compress.notice` | R/W (job, `dismiss_compress_notice`) | no | `pending` once a pass freed space; `shown` after dismissal (the notice never returns) |

### Database
`meetings.recording_json` (rewritten at commit through `replace_meeting_recording`); reads `transcription_jobs` (skip queued/running), `meetings.audio_deleted_at` (skip retention-deleted audio), `settings`.

### Files and directories written
See the file map. Temp files are `.<name>.compressing.flac` in the meeting folder (media extension on purpose: retention and delete-all treat a stray one as audio); a later pass deletes strays first.

### Network egress
None. Encoding and decoding are in-process, pure Rust; no ffmpeg download.

### External processes
None. The job runs on its own thread (`fotw-compress`) at background priority: Windows background mode (`THREAD_MODE_BACKGROUND_BEGIN`: low CPU and I/O priority), macOS's background QoS class (`pthread_set_qos_class_self_np`, which also throttles I/O), and on Linux nice 19 for that thread (`setpriority`; CPU only).

### Secrets and keychain entries
None.

## The commit protocol (per meeting)
1. Check it isn't being recorded (before the storage lock, see Lock order), then under the lock take the folder lease (renames and retention skip the folder) after checking: no transcription queued/running for it, no pipeline stage for it, audio not retention-deleted, playback not requested in the last 30 minutes. Every recording path must be a file directly inside the meeting's own folder (`recording_file_in_folder`), else the meeting is left as it is; a folder whose manifest is from a newer build is left alone; the disk must have room for the sources' FLACs plus 256 MB (else `Failed`, shown in Settings).
2. For each source (and each derived file that can't be dropped): check free space again, encode to `.<name>.compressing.flac`, `fsync`, verify with claxon (format, sample count, every sample, STREAMINFO MD5; the WAV's header is read by both this module and hound, which must agree), copy the WAV's modified time (retention ages audio by it), rename into place. A `.flac` already there from an interrupted run is reused only if it verifies.
3. For `mixed` and `playback`: `matches_stored` rebuilds them from the FLAC sources (what will be kept) and compares every sample, the rate and the length. Equal → drop; else convert.
4. Under the storage lock: if the meeting, its recording or its audio was deleted meanwhile, stop with `Gone` and delete the FLACs this run made; re-run the step-1 checks and confirm `recording_json` is unchanged; write the manifest, then `recording_json`; delete the replaced WAVs (still under the lock, so no reader can pick up an old path in between).
5. A WAV left by a crash between 4's commit and its delete (any layout, imports included) is deleted by a later pass only after it is re-proven (its FLAC twin verifies, or the derived file regenerates). One that doesn't match stays, is reported (`kept`), and isn't decoded again while its size and modified time are unchanged (in memory; a restart tries it once more).

Every step before 4 can be interrupted (kill, cancel, a busy machine) without changing anything a reader sees.

## Old builds (v2.4 and earlier) on a compressed meeting
Recorded run (2026-10-29): the `v2.4.0` tag built in debug with a temporary scratch data-dir override, on five meetings compressed by this build (no real data; synthetic TTS audio). Nothing was lost or corrupted:
- **Startup backfill** only added the missing `note.json` files; every manifest, FLAC and transcript mirror was byte-identical afterwards.
- **Self-heal** resurrected a compressed folder the database didn't know, with the `.flac` paths and `null` mixdown/playback, transcript restored.
- **Re-transcribe** failed cleanly ("Ill-formed WAVE file: no RIFF tag found" from `read_wav_mono`), was retried 3 times, parked as failed; the existing transcript was untouched.
- **Audio retention** (30 days, a 40-day-old meeting): deleted the two FLACs, kept manifest and transcripts, stamped `audio_deleted_at`.
- **Delete note** removed the meeting and its folder.
- The player: v2.4 falls back to `mic_path`, so it plays the microphone channel only (a FLAC; WebView2 and WKWebView decode it, webkit2gtk only with GStreamer's flac plugin).

That run used schema-v2 manifests. With manifest v3 (#105) an older build's backfill leaves the file alone entirely (`version` > its own), and its heal still parses it (unknown fields are ignored).

## Dependencies
- **Inbound:** `lib.rs` (spawn, commands), `scheduler.rs` (nudge), `pipeline.rs` (mixdown regeneration), `commands.rs` (`delete_note`), `privacy.rs` (retention busy set, delete-all halt), `state.rs`.
- **Outbound:** [fly-audio](fly-audio.md) (`flac`, `derive`, `mix::read_audio_mono`), [fly-storage](fly-storage.md) (leases, `replace_meeting_recording`, `recording_file_in_folder`, `meetings_with_stored_audio`, jobs, privacy, settings, scratch paths), [fly-core](fly-core.md) (`RecordingRef`), `windows` / `libc` (thread priority), `sysinfo` (free disk space).

## Invariants
- **A WAV is deleted only after a proof made in the same run:** its FLAC decodes (independent decoder) to exactly its samples, or (derived files) the file regenerates exactly from the sources as stored.
- **Nothing is ever converted from a WAV that isn't in the layout capture writes** (`inspect_pcm16_wav`): an unfinalized header, extra chunks, trailing bytes, other bit depths stay WAV.
- **The manifest is written before `recording_json`, and both before any delete.**
- **Readers stay bit-identical:** `read_audio_mono(flac)` returns the same f32 bits as `read_wav_mono(wav)`, and the regenerated mixdown/playback mix are byte-identical to what capture wrote (unit tests and the corpus e2e).
- **Recording and transcription win:** while anything records (live captions included), transcribes, re-diarizes, polishes or imports (`busy_reason`; a merely queued job doesn't count), the job abandons the step in flight (temp deleted, WAVs untouched), waits, and retries the meeting, reusing FLACs it already verified. A meeting whose own transcription is queued or running is never committed (its transcript isn't final).
- **Audio deleted by retention never comes back:** retention skips a meeting whose folder the job leases (checked under the storage lock the lease is taken under); the commit re-checks `audio_deleted_at` and, if the audio (or the meeting) went meanwhile, removes the FLACs it made instead of committing.
- **Only the meeting's own folder:** no file outside `recordings/<folder>/` is converted, replaced or deleted, whatever a (copied-in) manifest says; `replace_meeting_recording` refuses such paths too.
- **Lock order:** `storage`, then `pipeline_stage` or this module's own locks. Never `recording` or `screen` while holding `storage` (starting a recording holds `recording`, then takes `storage`); the commit-time check (`may_commit`) runs under `storage` and so never looks at them.
- **Capture is untouched:** recording still writes WAV (crash safety), owned by app-recording / fly-audio.
- **The webview never decodes FLAC.** `prepare_playback` streams stored WAVs as they are and routes every FLAC through the scratch cache (exact decode or rebuilt mix), so playback doesn't depend on WebView2 / WKWebView / webkit2gtk (+ GStreamer's flac plugin) codec support.
- **No manifest writer races the commit.** Every manifest rewrite (title/date edits, Keep-local toggles, backfill, the commit) builds from the row while holding the storage lock, and the commit writes manifest, row and deletes in one hold of it: a rewrite before the commit names the WAVs (still there), one after names the FLACs.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-audio/src/flac.rs` | 8 | round trips (mono/stereo/4 ch, 8–48 kHz, empty), verify catches a flipped bit / truncation / a 1-LSB WAV change / an extra sample, interrupted encode, the strict WAV layout check |
| unit | `crates/fly-audio/src/derive.rs` | 5 | streaming resampler = `resample_linear` bit for bit (10 rate pairs), `MonoFile` = `read_wav_mono` for WAV and FLAC, regenerated mixdown/playback byte-identical to capture's for 8 channel layouts, from FLAC sources too, mismatch detection |
| unit | `crates/fly-storage/src/recording_store.rs` | 11 | manifest-then-row commit (v3 payload, Keep local never turned off, a newer manifest refused), the manifest on disk kept with only the recording replaced, an old manifest filled from the rows, another meeting's manifest refused, an unreadable one rewritten, any path outside the folder refused, a leased folder isn't renamed, the stored-audio list |
| unit | `src-tauri/src/audio_store.rs`, `privacy.rs` | 4 + 1 | the player never gets a FLAC (every layout's playback plan), twin/temp names, halt then resume, free space (unknown never blocks); retention skips a leased folder |
| integration (CI) | `src-tauri/tests/compress_recordings.rs` | 19 | every layout (two-channel, mic-only, import, foreign mixdown, unfinalized WAV), readers and ASR input bytes identical, mtime kept, stop mid-encode, refused commit, damaged FLAC rebuilt, leftovers re-proven (an import's too; an unmatched one reported and not re-decoded), stray temp, hard failure leaves no extra copy, audio deleted mid-run leaves no FLAC, a full disk fails before encoding, a path outside the folder leaves the meeting alone, unreadable stored mix kept, Keep local toggled mid-compress, newer-build manifest left alone, **a child process aborted at every checkpoint and killed at wall-clock points** |
| ignored e2e | `src-tauri/tests/compress_e2e.rs` | 1 | the real whisper + sherpa pipeline and re-diarize on WAV vs compressed: identical transcripts (text, timings, words, speakers) |
| vitest | `frontend/src/storageModel.test.ts`, `components/StorageRows.test.tsx`, `components/Editor.playback.test.tsx` | 17 | Settings copy per job state (failed and skipped meetings named, with the reason), video report, notice due/dismiss, rebuild on Play, keep-alive while playing, re-prepare when the file went away |

## Changing it safely
- Never delete a recording file without a same-run proof (see Invariants); add a test to `compress_recordings.rs` for any new layout.
- Changing `mix::write_mixdown` / `write_playback_mix` / `resample_linear` / `read_wav_mono` changes what "regenerates bit-identically" means: update `derive.rs` in the same PR (its tests compare the two), and remember already-compressed meetings regenerate with the new code.
- Keep `busy_reason` in step with what else needs the machine (recording, transcription, import).
- Keep the lock order (see Invariants): anything new in `may_commit` or `blocked` runs under the storage lock.
- Run `cargo test -p fly-audio -p fly-storage`, `cargo test -p fly-app --test compress_recordings`, and with artifacts installed `cargo test -p fly-app --test compress_e2e -- --ignored --nocapture`.
