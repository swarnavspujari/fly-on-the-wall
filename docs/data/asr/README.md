# ASR engine comparison data

Committed per-run metric files from `src-tauri/tests/accuracy_harness.rs` corpus mode
(v2.5 PR 2.1). They hold numbers only — no audio, no transcript text. The corpus itself is
real meetings and **never** lives in git.

## Running it

1. Copy `corpus.example.json` somewhere outside the repo and point each item at a recording
   folder (`recording.mic.wav` + `recording.system.wav`, or `recording.mixed.wav`) and its
   reference (a Fathom/human `.txt` or a Teams `.vtt`). An item with no `reference` is
   structure-only: it gates speaker attribution and hallucination loops, not WER.
2. Run every item for each engine, then print the table and the ship gates:

   ```
   FLYONTHEWALL_HARNESS_CORPUS=path\to\corpus.json \
   FLYONTHEWALL_HARNESS_ENGINES=whisper,parakeet \
     cargo test -p fly-app --release --test accuracy_harness -- --ignored --nocapture
   ```

   Add `FLYONTHEWALL_HARNESS_GPU=1` for the Vulkan build (plus
   `FLYONTHEWALL_HARNESS_PARAKEET_VULKAN=1` to try Parakeet on it), or
   `FLYONTHEWALL_HARNESS_DEDUP_WINDOW_MS=<ms>` to re-tune cross-talk dedup. The variant name
   in each file (`parakeet-cpu`, `whisper-vulkan-xtalk700`, …) records which one ran.
3. `FLYONTHEWALL_HARNESS_COMPARE=<results dir>` reprints the table and gates without
   re-running anything.
4. Optional manifest fields: `teams_dir` (a folder of Teams recordings and their `.vtt`
   files, paired by title prefix and picked up automatically; Teams' text is a *relative*
   reference, its speaker names drive DER), `cache_dir` (derived 16 kHz audio), and per item
   `start_secs` + `max_secs` to score one window.
5. With two engines in one run, each Parakeet result also gets `agreement`: its transcript
   scored against Whisper's (WER and DER of one against the other). This is how meetings
   with no human reference still say something.

## Gold excerpt

`FLYONTHEWALL_HARNESS_GOLD_EXPORT=<recording folder>` with `_GOLD_START` (mm:ss),
`_GOLD_SECS` (default 900) and `_GOLD_OUT=<corpus\gold\meeting>` writes `draft.txt`
(`[mm:ss] Speaker: text`, times relative to the clip), `clip.m4a` (the same window) and a
`manifest-item.json`. It never overwrites an existing draft. The user corrects the draft
into `gold.txt`; adding the manifest item scores every engine against it. Case and
punctuation are ignored, the normalizer applies, and a line containing `[inaudible]` is
excluded, together with any hypothesis words between its stamp and the next line's.
Alignment is word-level, so lines may be split or merged.

## Metrics

| Field | Meaning |
|---|---|
| `reference.wer_merged` | WER of the merged transcript vs the reference, after the English normalizer (`config.normalizer`) |
| `reference.sa_wer_merged` | Speaker-attributed WER: word errors plus correctly-recognized words given to the wrong speaker |
| `reference.attribution_error` | Share of matched words carrying the wrong speaker |
| `reference.xtalk_dup_windowed` | Reference words transcribed on both channels within 500 ms (cross-talk duplicates) |
| `reference.diarization.der_collar_250` | DER with a ±250 ms collar (timed `.vtt` references: Teams, AMI) |
| `structure.dup_rate_reffree` | The same duplicate signal without a reference: mic tokens echoed on the far-end channel in runs of 2 or more |
| `structure.far_end_minority_share` | With a known far-end speaker count, far-end time not on the top-N clusters (phantom speakers + Unknown) |
| `speed_x_realtime`, `rtf` | Audio seconds per ASR second, and the inverse. ASR covers both channels of a two-track recording |
| `resources.peak_rss_mb` | Peak resident memory of each decode sidecar (children of the test process only) |
| `resources.machine_cpu_avg_pct` | Whole-machine CPU during the run. A high value means other work competed and the timing is pessimistic |
| `agreement.*` | The same metrics with the Whisper transcript as the reference (engine agreement) |
| `loops.count` | Hallucination loops the pipeline's guard collapsed. These are produced by the engine, so the final transcript no longer shows them |

Ship gates (spec §4.1) checked by compare mode, per fixture, against the Whisper run on the
same device class:
- no WER regression;
- SA-WER ≤ baseline;
- loops ≤ baseline;
- far-end attribution ≤ baseline (structure-only items);
- ≥ 5× realtime on CPU.

"Live first partial ≤ 2 s" and "CPU ≤ 25 % during a call" belong to the live-captions PR.

## Committed results (provisional)

`ami/` holds the 2026-10-28 run on four public AMI meetings: ES2004a, IS1009a, TS3003a and EN2002a, each as `sdm` (Array1-01 single distant mic) and `ihm` (headset mix). AMI Meeting Corpus, CC BY 4.0. References were built from the AMI manual annotations v1.6.2, words + segments. There's one file per run, plus `summary.json` with the gate table. Variants:
- `whisper-vulkan`: large-v3-turbo q5_0, as shipped on the RTX 3050 test laptop;
- `parakeet-vulkan`: the harness Vulkan experiment;
- `parakeet-cpu`.

**Caveat on the Parakeet files:** they predate the seam-dedupe fix in #86 (`499ed70`). Expect about one extra insertion per recovered hole. Their `config.asr_model` also shows the Whisper model (the fallback tier) rather than the Parakeet weights; `transcript_engine` names the model that actually decoded. They'll be re-run at v2.5 integration, when the machine is idle.

These numbers are **provisional**. AMI isn't the user's audio, and the spec §4.1 ship gates need the user's own recordings (Teams, gold excerpt) before any default changes. Teams results stay local (they're the user's meetings).
