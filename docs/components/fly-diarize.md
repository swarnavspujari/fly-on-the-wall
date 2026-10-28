# Speaker diarization (`fly-diarize`)

> Layer: crate. Described at main `82b7edf` (v2.4.0).
> In flight: None.

## Purpose
Turns a 16 kHz mono WAV into speaker turns (`spk_0`, `spk_1`, …) entirely on the device. The sherpa-onnx sidecar does pyannote segmentation, speaker embeddings and an initial agglomerative clustering. An in-process VBx pass (Bayesian HMM over embeddings) then merges the over-split clusters that plain thresholds leave behind. Diarization always runs locally, including when ASR is offloaded to Groq (ARCHITECTURE.md "Privacy boundaries").

## Files it owns
- `crates/fly-diarize/Cargo.toml`: deps; `ort` pinned to `2.0.0-rc.13`, with per-target linking modes.
- `crates/fly-diarize/src/lib.rs`: `DiarizationEngine` trait, `DiarizeOptions`, dust-cluster filter.
- `crates/fly-diarize/src/sherpa.rs`: `SherpaDiarizeEngine` (sidecar call, output parsing, VBx hand-off, Linux onnxruntime loading).
- `crates/fly-diarize/src/embed.rs`: in-process ONNX speaker-embedding extractor (same model file as the sidecar).
- `crates/fly-diarize/src/fbank.rs`: Kaldi-style 80-bin log-mel features and mean normalization.
- `crates/fly-diarize/src/vbx.rs`: Rust port of BUT's VBx (Apache-2.0, credited in the header).
- `crates/fly-diarize/src/refine.rs`: subsegmentation, two-covariance whitening, the VBx run, turn reassembly, WAV reader.

## Surface area
### Public API
| Group | Items | Where |
|---|---|---|
| Trait | `DiarizationEngine { id(), diarize(wav, opts) -> Vec<SpeakerTurn> }` | `crates/fly-diarize/src/lib.rs:65-68` |
| Errors | `DiarizeError::{ModelMissing, BadAudio, Engine, Io}`, `Result<T>` | `lib.rs:19-31` |
| Options | `DiarizeOptions { num_speakers, cluster_threshold (default 0.95), speaker_key_prefix ("spk") }` | `lib.rs:33-58` |
| Dust filter | `drop_dust_clusters`, `drop_dust_clusters_with`, `DUST_FLOOR_MS` (15 s), `DUST_FRACTION` (5%) | `lib.rs:71-101` |
| Sidecar engine | `SherpaDiarizeEngine { exe, segmentation_model, embedding_model, threads, refine: Option<VbxParams> }`, `parse_diarization_output` | `sherpa.rs:19-33`, `sherpa.rs:201-229` |
| VBx | `VbxParams` (default loopP 0.99, Fa 0.1, Fb 17, 40 iters), `VbxOutcome`, `vbx()` | `vbx.rs:17-56`, `vbx.rs:113` |
| Refinement | `refine_with_vbx`, `VbxLab::{prepare, run}` (harness sweeps), `read_wav_mono_16k` | `refine.rs:104-318` |
| Features / embeddings | `FbankComputer`, `apply_cmn`, `NUM_BINS`, `EmbeddingExtractor::{new, embed}` | `fbank.rs:17-163`, `embed.rs:12-73` |

### Tauri commands
None (crates expose no commands).

### Events
None.

### Settings keys
None. The caller fills `DiarizeOptions`. The app sets `num_speakers` only from a user-confirmed attendee list (see Invariants).

### Database
None.

### Files and directories written
None. The engine reads the WAV it is given (the app passes `<scratch_root>/<meeting_id>/*.16k.wav`) and the model files. The sidecar writes nothing; turns come back on stdout.

### Network egress
None. The crate has no HTTP dependency (`crates/fly-diarize/Cargo.toml:7-17`). Model and sidecar downloads belong to [app-models-gpu](app-models-gpu.md).

### External processes
| Process | Args | Notes |
|---|---|---|
| `sherpa-onnx-offline-speaker-diarization` | `--segmentation.pyannote-model=<seg.onnx> --embedding.model=<emb.onnx> --segmentation.num-threads=N --embedding.num-threads=N` plus **either** `--clustering.num-clusters=<n>` (a speaker count was given) **or** `--clustering.cluster-threshold=<t>`, then the wav path (`sherpa.rs:136-154`, `sherpa.rs:65-67`) | Windows: `CREATE_NO_WINDOW` and `BELOW_NORMAL_PRIORITY_CLASS` (`sherpa.rs:68-73`). Stdout lines `0.318 -- 6.865 speaker_00` are parsed and everything else is ignored (`sherpa.rs:201-229`). No timeout. |

When VBx is on and no count is forced, the threshold sent is `VBX_INIT_THRESHOLD = 0.8`, a deliberate over-split, not the 0.95 default (`sherpa.rs:15-17`, `sherpa.rs:61-64`). VBx itself runs in-process on `spawn_blocking` (`sherpa.rs:108-127`).

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** [app-pipeline](app-pipeline.md) only (`src-tauri/src/pipeline.rs:452-458` for the full pipeline, `pipeline.rs:799-805` for re-diarize), plus `src-tauri/tests/accuracy_harness.rs` (sweeps and the VBx lab).
- **Outbound:** [fly-core](fly-core.md) (`SpeakerTurn`; the word-to-speaker aligner lives there, not here), `ort` (ONNX Runtime), `rustfft`, `hound`, tokio.

`ort` pin note: `ort = "2.0.0-rc.13"` is a pre-release requirement; `Cargo.lock` pins `2.0.0-rc.13` and CI builds `--locked`. Windows and Apple-Silicon macOS link the prebuilt onnxruntime statically. Linux and Intel macOS use `features = ["load-dynamic"]` (`Cargo.toml:19-32`). Linux needs this because the prebuilts require glibc ≥ 2.38 and releases build on ubuntu-22.04. It then dlopens the `libonnxruntime.so` shipped inside the sherpa bundle (`sherpa.rs:157-192`). Intel macOS never builds the refiner (`src-tauri/src/pipeline.rs:87-96`). Bump `ort` deliberately: a caret requirement on an rc can resolve to a newer rc on `cargo update`, and rc APIs have changed between releases (inferred).

## Invariants
- **Always local.** No network code, on every hardware tier, including the Groq tier. Sources: ARCHITECTURE.md "Privacy boundaries"; `lib.rs:3-6`; the app always ensures the diarization models (`src-tauri/src/pipeline.rs:239-250`).
- **A forced speaker count wins and bypasses VBx.** `num_speakers` becomes `--clustering.num-clusters` and no threshold is passed (`sherpa.rs:146-153`). VBx is skipped (`sherpa.rs:93-95`) because it can only merge. Upstream, spec §2 item 7 limits `num_speakers` to confirmed attendee lists (`src-tauri/src/pipeline.rs:43-51`). Test: `sherpa::known_speaker_count_wins_over_threshold`.
- **VBx merges, never splits.** So the initial clustering is run deliberately over-split (docs/BENCHMARKS.md Phase 3; `sherpa.rs:57-64`).
- **One pinned embedding model.** The sidecar and the in-process embedder load the same ONNX file (`embed.rs:3-5`, `sherpa.rs:24-26`).
- **Intel macOS ships plain agglomerative clustering at 0.95.** `ort` has no x86_64-apple-darwin prebuilt (docs/BENCHMARKS.md Phase 2a; `src-tauri/src/pipeline.rs:82-96`).
- **Dust clusters are dropped before alignment** so phantom speakers don't appear. The floor scales down for short recordings (`lib.rs:76-101`, applied at `src-tauri/src/pipeline.rs:571`, `:654`, `:857`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-diarize/src/lib.rs` | 4 | dust dropping for long and short audio, single cluster, empty input |
| unit | `crates/fly-diarize/src/sherpa.rs` | 5 | stdout parsing, CLI args (threshold vs forced count), engine default |
| unit | `crates/fly-diarize/src/fbank.rs` | 3 | frame count, tone lands in the right mel bin, mean normalization cancels input scale |
| unit | `crates/fly-diarize/src/vbx.rs` | 3 | merges shattered clusters, ELBO non-decreasing, one speaker collapses to one |
| unit | `crates/fly-diarize/src/refine.rs` | 4 | subsegment cuts, tiny turns skipped, overlap midpoint, dense relabel |
| ignored e2e/harness (never in CI) | `src-tauri/tests/pipeline_e2e.rs`, `rediarize_e2e.rs`, `accuracy_harness.rs` | 4 | the real sidecar plus VBx on the committed two-voice fixture; DER only in the harness |

**Known gaps:**
- `embed.rs` (ONNX session, L2 normalization) and `VbxLab::prepare` (two-covariance whitening) have no CI test. They need the model file, so the only coverage is the ignored e2e tests.
- The `VBX_INIT_THRESHOLD` override (`sherpa.rs:61-64`) is untested. `default_options_pass_the_cluster_threshold` builds the engine with `refine: None`, but production uses `Some(VbxParams)`, so the threshold the shipped app actually sends (0.8) is never asserted.
- `ensure_ort_dylib` (the Linux release path) and `read_wav_mono_16k` (resampling, multi-channel averaging) are untested. Linux CI never runs VBx.
- Fbank parity with kaldi-native-fbank is checked only by self-consistency (tone placement uses the crate's own `mel()`), not against reference values.
- No DER threshold is asserted anywhere. `accuracy_harness` writes DER to JSON (committed baselines under `docs/data/diarization/` change as a git diff), and `pipeline_e2e` asserts "exactly 2 speakers" on a 27 s TTS fixture, which is ignored in CI.

## Changing it safely
- Run `cargo test -p fly-diarize`. Re-measure with `src-tauri/tests/accuracy_harness.rs` (diarize-only mode, `FLYONTHEWALL_HARNESS_DIARIZE_WAV` + `FLYONTHEWALL_HARNESS_BASE_JSON` + `FLYONTHEWALL_HARNESS_RESULTS_JSON`) and diff the committed baselines in `docs/data/diarization/`.
- Changing `VbxParams` defaults, `VBX_INIT_THRESHOLD`, the dust constants or the 0.95 default changes shipped accuracy. Update docs/BENCHMARKS.md together with the code.
- Keep the forced-count path free of VBx, and keep the CLI from passing both `num-clusters` and a threshold.
- Changing the embedding model means changing both the sidecar input and the in-process embedder; the model registry lives in `src-tauri/src/models.rs` ([app-models-gpu](app-models-gpu.md)).
- A sherpa-onnx version bump can change the stdout format. Run the ignored `pipeline_e2e` and `rediarize_e2e` before shipping it.
- Touching `ort` or the target tables: check the Linux release build (glibc floor) and Intel macOS (load-dynamic, no refiner) as well as Windows.
