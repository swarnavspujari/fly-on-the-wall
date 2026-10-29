# Recording storage v2 (`app-audio-store`)

> Layer: backend feature (src-tauri). Added on top of main `ccca11b` (v2.4.0+).
> In flight: this is the change itself (recording storage v2, spec §4.6 / v2.5 row 4.z, made lossless). The search-indexing session may add a shared "background work should yield" signal; `busy_reason` here should switch to it.

## Purpose
Finished recordings are kept as **lossless FLAC** instead of WAV, and the two derived files capture writes (the 16 kHz ASR mixdown and the native-rate playback mix) are no longer stored when they can be rebuilt bit-identically. A low-priority background job ("Compress recordings") does this on every install, for old meetings and for each new one once its transcript is final. Every reader accepts WAV or FLAC, so nothing about playback, re-transcription, re-diarization or portability changes. This module also owns picking (or rebuilding) the file the editor's player streams, and the Settings › Storage report.

The user's requirement is "compress without losing anything". The spec's earlier plan (16 kHz FLAC sources + lossy M4A playback) was replaced: sources keep their original sample rate and bit depth, and every FLAC is decoded again and compared sample for sample with its WAV before the WAV is deleted.

## Files it owns
- `src-tauri/src/audio_store.rs`: readers' helpers (`is_storage_v2`, `regenerate_mixdown_for_asr`), the per-meeting commit protocol (`compress_meeting`, `needs_work`), the job (`spawn`, `run_pass`, pause rules), playback (`prepare_playback`, the scratch cache), the storage report, 3 commands.
- `crates/fly-audio/src/flac.rs`: strict WAV layout check, streaming FLAC encoder (flacenc), independent verifier (claxon), streaming FLAC reader. See [fly-audio](fly-audio.md).
- `crates/fly-audio/src/derive.rs`: streaming, bit-identical regeneration of the mixdown and the playback mix; `matches_stored`. See [fly-audio](fly-audio.md).
- `crates/fly-storage/src/recording_store.rs`: folder leases, `meetings_with_stored_audio`, `replace_meeting_recording` (manifest, then row). See [fly-storage](fly-storage.md).
- `crates/fly-audio/examples/flac_bench.rs`: size/speed/bit-exactness on copies of real meetings.

## The file map
Every recording file, who writes it and who reads it, before and after storage v2.

| File (in `recordings/<date> <title>/` unless noted) | Written by | Read by | Storage v2 |
|---|---|---|---|
| `recording.mic.wav` | capture (`fly-audio` `cpal_backend`, the shared writer the loopback backends use), for the whole recording | live captions (`live.rs` `take_chunk`, growing file, only while recording); stop-time mixdown and playback mix (`mix::write_mixdown`, `write_playback_mix`); pipeline `prep` (per-channel path, and the single-track fallback); GPU speed test sample (`gpu.rs`); re-diarize (single-track fallback); the player (last fallbacks); open/reveal in the OS (`open_attachment`, `reveal_attachment`) | → `recording.mic.flac`, same rate and bit depth, verified sample for sample, same modified time |
| `recording.system.wav` | capture (loopback) | same as mic; it's the diarized channel in the per-channel path | → `recording.system.flac` |
| `recording.mixed.wav` | capture at stop (16 kHz mix of both channels); media import (`import_commands.rs`: the joined 16 kHz track of all imported files) | pipeline single-track path (mic-only captures, imports); re-diarize single-track path; GPU sample (third choice); player (second fallback); Editor "Show in folder" | capture: **dropped** when it regenerates bit-identically from the FLAC sources (`mixed_path: null`), else → `.flac`. Import: no sources, so it **is** a source → `recording.mixed.flac` |
| `recording.playback.wav` | capture at stop, only with both channels (mic-only: `playback_path` points at the mic file) | the player only (and "open in your player") | **dropped** when it regenerates bit-identically (`playback_path: null`), else → `.flac`. Mic-only: `playback_path` follows the mic to `recording.mic.flac` |
| imported originals (`<user's name>.mp4/.m4a/.wav…`) | `import_stage` copies | the import's conversion step (ffmpeg), the user | untouched (they're the user's files; videos are reported in Settings) |
| `recording.manifest.json` | stop (stash), edits (refresh), startup backfill | self-heal / portability | rewritten at commit, **before** `recording_json`, so it never names deleted files. Same v2 schema; only paths change |
| `<scratch>/<meeting id>/{mic,system,track,rediarize}.16k.wav`, `*.asrpart.json` | pipeline / re-diarize | ASR engines, diarizer | unchanged (always WAV, machine-local, deleted after a successful run) |
| `<scratch>/<meeting id>/mixdown.16k.wav` | **new**: `regenerate_mixdown_for_asr` (single-track run of a compressed recording) | pipeline `prep`, re-diarize | deleted with the other intermediates |
| `<scratch>/<meeting id>/playback.wav` | **new**: `prepare_playback(build: true)` | the player (the asset scope allows this one file) | a cache: newest-first, up to 1.5 GB in total; goes with the meeting's scratch (purge, retention, delete-all, startup sweep) |
| `attachments/<note>/screen-*.mp4` (+ `.jpg` poster) | screen recording | the video lightbox | untouched (reported in Settings) |

Not readers of audio: the MCP server (database and transcript mirrors only), `export_note` / `export_transcript_vtt` (text), search and embeddings.

## Surface area
### Public API
| Item | Called by |
|---|---|
| `is_storage_v2(&RecordingRef)`: a source is `.flac`, so absent derived files regenerate | pipeline, `prepare_playback`, tests |
| `regenerate_mixdown_for_asr(rec, mixed, mic, system, work_dir) -> Result<Option<PathBuf>>` | `pipeline::run_with` (single-track path), `pipeline::re_diarize_with` |
| `compress_meeting(&mut MeetingCtx, id) -> Result<MeetingOutcome, MeetingError>`, `needs_work(data_dir, rec)` | the job; `src-tauri/tests/compress_recordings.rs`, `compress_e2e.rs` |
| `spawn(app)` | `lib.rs` setup |
| `StoreState` (`AppState.audio_store`): `nudge()`, `status()`, `current()` | scheduler (nudge after a transcript is final), privacy (retention busy set) |
| `stop_meeting(state, id, timeout)`, `halt(state, timeout)` | `delete_note` (before the purge), `delete_all_data` |

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `prepare_playback` | `meeting_id: String, build: bool` | `Result<Option<PlaybackSource { path: Option<String>, needs_build: bool }>, String>` (absolute path; `None` = no audio) | `ui-editor` (`Editor.tsx`, on note open with `build: false`; on Play with `build: true`), via `api.preparePlayback` |
| `storage_report` | none | `Result<StorageReport, String>` (audio, WAV part, imported videos, screen recordings, other attachments, `compress: CompressStatus`) | `ui-settings` (`StorageRows.tsx`), `ui-notices` (`CompressNotice.tsx`), via `api.storageReport` |
| `dismiss_compress_notice` | none | `Result<(), String>` | `ui-notices` (`CompressNotice.tsx`), via `api.dismissCompressNotice` |

devMock mirrors all three (`devMock.ts`, flags `fotwMockCompress` = `working`/`paused`/`done`, `fotwMockCompressNotice`, `fotwMockCompressed`).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `storage:compress` | emits whenever the job's status changes | `CompressStatus { state, paused_for, meetings_total, meetings_done, bytes_total, bytes_done, saved_bytes, kept_files, notice_pending, last_error }` | `StorageRows.tsx`, `CompressNotice.tsx` |

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
None. The job runs on its own thread (`fotw-compress`), in Windows background mode (`THREAD_MODE_BACKGROUND_BEGIN`: low CPU and I/O priority).

### Secrets and keychain entries
None.

## The commit protocol (per meeting)
1. Take the folder lease (renames skip the folder) after checking: no transcription queued/running for it, no pipeline stage, not being recorded, audio not deleted, playback not opened in the last 30 minutes.
2. For each source (and each derived file that can't be dropped): encode to `.<name>.compressing.flac`, `fsync`, verify with claxon (format, sample count, every sample, STREAMINFO MD5), copy the WAV's modified time (retention ages audio by it), rename into place. A `.flac` already there from an interrupted run is reused only if it verifies.
3. For `mixed` and `playback`: `matches_stored` rebuilds them from the FLAC sources (what will be kept) and compares every sample, the rate and the length. Equal → drop; else convert.
4. Under the storage lock: re-run the step-1 checks and confirm `recording_json` is unchanged; write the manifest, then `recording_json`; delete the replaced WAVs (still under the lock, so no reader can pick up an old path in between).
5. A WAV left by a crash between 4's commit and its delete is deleted by a later pass only after it is re-proven (FLAC twin verifies, or the derived file regenerates).

Every step before 4 can be interrupted (kill, cancel, a busy machine) without changing anything a reader sees.

## Old builds (v2.4 and earlier) on a compressed meeting
Checked against `main`'s code (see Tests). Nothing is deleted or corrupted:
- The manifest stays schema v2 (no version bump), so old heal/backfill parse it; rebasing is by file name, so `.flac` paths survive heal; backfill never replaces existing values.
- Retention and "Delete all my data" already treat `.flac` as audio.
- Re-transcribe / re-analyze fail with an error from `read_wav_mono` (not a WAV), are retried three times and parked as failed; the existing transcript is kept. A mic-only or import meeting fails the same way.
- The player gets `playback_path: null` and `mixed_path: null`, falls back to `mic_path` (FLAC plays in WebView2 and WKWebView), so it plays the microphone channel only.

## Dependencies
- **Inbound:** `lib.rs` (spawn, commands), `scheduler.rs` (nudge), `pipeline.rs` (mixdown regeneration), `commands.rs` (`delete_note`), `privacy.rs` (retention busy set, delete-all halt), `state.rs`.
- **Outbound:** [fly-audio](fly-audio.md) (`flac`, `derive`, `mix::read_audio_mono`), [fly-storage](fly-storage.md) (leases, `replace_meeting_recording`, `meetings_with_stored_audio`, jobs, privacy, settings, scratch paths), [fly-core](fly-core.md) (`RecordingRef`), `windows` (thread priority).

## Invariants
- **A WAV is deleted only after a proof made in the same run:** its FLAC decodes (independent decoder) to exactly its samples, or (derived files) the file regenerates exactly from the sources as stored.
- **Nothing is ever converted from a WAV that isn't in the layout capture writes** (`inspect_pcm16_wav`): an unfinalized header, extra chunks, trailing bytes, other bit depths stay WAV.
- **The manifest is written before `recording_json`, and both before any delete.**
- **Readers stay bit-identical:** `read_audio_mono(flac)` returns the same f32 bits as `read_wav_mono(wav)`, and the regenerated mixdown/playback mix are byte-identical to what capture wrote (unit tests and the corpus e2e).
- **Recording and transcription win:** the job pauses in place while anything records, transcribes or imports, and never commits under a running pipeline for that meeting.
- **Audio deleted by retention never comes back:** the commit re-checks `audio_deleted_at`; retention skips the meeting the job holds.
- **Capture is untouched:** recording still writes WAV (crash safety), owned by app-recording / fly-audio.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-audio/src/flac.rs` | 7 | round trips (mono/stereo/4 ch, 8–48 kHz, empty), verify catches a flipped bit / truncation / a 1-LSB WAV change / an extra sample, interrupted encode, the strict WAV layout check |
| unit | `crates/fly-audio/src/derive.rs` | 5 | streaming resampler = `resample_linear` bit for bit (10 rate pairs), `MonoFile` = `read_wav_mono` for WAV and FLAC, regenerated mixdown/playback byte-identical to capture's for 8 channel layouts, from FLAC sources too, mismatch detection |
| unit | `crates/fly-storage/src/recording_store.rs` | 4 | manifest-then-row commit, refusing another folder, a leased folder isn't renamed, the stored-audio list |
| integration (CI) | `src-tauri/tests/compress_recordings.rs` | 11 | every layout (two-channel, mic-only, import, foreign mixdown, unfinalized WAV), readers and ASR input bytes identical, mtime kept, stop mid-encode, refused commit, damaged FLAC rebuilt, leftovers re-proven, stray temp, **a child process aborted at every checkpoint and killed at wall-clock points** |
| ignored e2e | `src-tauri/tests/compress_e2e.rs` | 1 | the real whisper + sherpa pipeline and re-diarize on WAV vs compressed: identical transcripts (text, timings, words, speakers) |
| vitest | `frontend/src/storageModel.test.ts`, `components/StorageRows.test.tsx` | 11 | Settings copy per job state, video report, notice due/dismiss |

## Changing it safely
- Never delete a recording file without a same-run proof (see Invariants); add a test to `compress_recordings.rs` for any new layout.
- Changing `mix::write_mixdown` / `write_playback_mix` / `resample_linear` / `read_wav_mono` changes what "regenerates bit-identically" means: update `derive.rs` in the same PR (its tests compare the two), and remember already-compressed meetings regenerate with the new code.
- Keep `busy_reason` in step with what else needs the machine (recording, transcription, import).
- Run `cargo test -p fly-audio -p fly-storage`, `cargo test -p fly-app --test compress_recordings`, and with artifacts installed `cargo test -p fly-app --test compress_e2e -- --ignored --nocapture`.
