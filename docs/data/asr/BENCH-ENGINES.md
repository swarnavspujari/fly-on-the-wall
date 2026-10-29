# Bench-only ASR engines and diarizers (v2.5 engine choice)

The accuracy harness can run candidate engines that **do not ship in the app**, through the
real pipeline: `pipeline::run_with_asr_engine` swaps only the transcription step, so prep,
the loop guard, diarization and word alignment are the shipped code. Code:
`src-tauri/tests/accuracy_harness/bench_{engines,servers,align,diarize}.rs` and
`scripts/asr-bench/`.

Everything they need (runtimes, weights, a Python venv) lives in one folder **outside the
repo**, named by `FLYONTHEWALL_HARNESS_BENCH_DIR`. Nothing is downloaded by the harness; a
missing file is a SKIP.

## Engines

| Engine name | What runs | Timestamps |
|---|---|---|
| `nemocpp-parakeet` | Parakeet TDT 0.6B v3 q8_0 on NVIDIA NeMo-Speech.cpp (`nemo-speech serve`, one server per channel, shipped VAD batches) | native word times |
| `nemocpp-parakeet-win` | the same, contiguous ≤ 120 s windows of the raw channel (no silence removal, no stitching) | native word times |
| `nemocpp-parakeet-whole` | the same, the whole channel in one request (NeMo-Speech.cpp's own 400 s chunking; needs a 5.1 GB graph, so CPU only on a 4 GB GPU) | native word times |
| `nemocpp-nemotron` | nemotron-speech-streaming-en-0.6b q8_0 on NeMo-Speech.cpp, 1.12 s right context | native word times |
| `nemotron-live` | nemotron-speech-streaming-en-0.6b int8 (sherpa-onnx), the whole channel fed in 160 ms capture buffers like a live call; 2 threads (`FLYONTHEWALL_HARNESS_LIVE_THREADS`) | token times |
| `nemotron-offline` | the same model over the shipped VAD batches | token times |
| `nemotron35-live` | nemotron-3.5-asr-streaming-0.6b int8, live like `nemotron-live` | token times |
| `parakeet-sherpa` | Parakeet TDT 0.6B v3 int8 on sherpa-onnx | token times + TDT durations |
| `cohere` | Cohere Transcribe int8 (sherpa-onnx), ≤ 30 s pieces | none: transferred from `parakeet-sherpa` |
| `granite` | IBM Granite Speech 5.0 2B GGUF on `llama-server`, ≤ 60 s pieces | none: transferred from `parakeet-sherpa` |

"Transferred" timing: the LLM decoder's words are aligned (minimum edit) to a timestamped
engine's words on the same piece; matched and substituted words take that engine's times,
inserted words are spread between neighbours. It is not acoustic forced alignment, and the
result file says which aligner timed the words (`bench.timing`, `bench.transfer_*`).

Diarizer arms (`FLYONTHEWALL_HARNESS_DIARIZE_CORPUS`, words held fixed from one kept
transcript): `sherpa` (shipped: sherpa-onnx segmentation + CAM++ + VBx + dust filter),
`nemotron3` (Nemotron-3-Diarization q8_0 via `nemo-speech diarize`, default 1.12 s geometry),
`nemotron3-30s` (the model card's 30.4 s config), `sortformer` (streaming Sortformer 4spk v2).

## Bench folder layout

All files are pinned. Check them with `sha256sum` after download.

```
<bench>/venv/                      python -m venv / uv venv; pip install "sherpa-onnx>=1.13.5" numpy
                                   (2.0.1 measured; 1.13.4 drops words at chunk boundaries, issue #3782)
<bench>/bin/nemo-speech-{cpu,vulkan}/bin/nemo-speech.exe
    github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.2.0/
      nemo-speech-0.2.0-windows-x86_64-cpu.zip     82c80451086e86194aba9af19f800da77cd66d1c28d993d0efceffd6cdb49bb8
      nemo-speech-0.2.0-windows-x86_64-vulkan.zip  edf15a04ba98740aa0ab75ae4681ac7ad0a488b22b35ce2cbebd10590bb1c1e1
    (unzip, then move the inner folder's contents up one level)
<bench>/bin/llama-{cpu,vulkan}/llama-server.exe
    github.com/ggml-org/llama.cpp/releases/download/b11976/
      llama-b11976-bin-win-cpu-x64.zip             c7d5d65986917da50bfd4aad0d127648a70b61ec457a2b83ec42570e49fbe25b
      llama-b11976-bin-win-vulkan-x64.zip          fd94feb540bb6dc3c95e2dac24ef5ced3bfeb7e09175db4d6291f898158b8c10
<bench>/models/nemo-speech/        huggingface.co/<repo>/resolve/<rev>/<file>
      nvidia/parakeet-tdt-0.6b-v3 @541d1f9  parakeet-tdt-0.6b-v3.q8_0.gguf                e3880d0aaaaf2c308ea2c35016b2b895c423eb3fda924c1b463d1c19b7f4d32e
      nvidia/nemotron-speech-streaming-en-0.6b @ebe59e5  nemotron-speech-streaming-en-0.6b.q8_0.gguf  d9a01898d2a611c8764e23a1c2f45e70bbd5a425dc4de93692ac951dd603812d
      nvidia/Nemotron-3-Diarization @f667ed7  Nemotron-3-Diarization.q8_0.gguf            08456d9e22cd9a323c0364d98375f3746d6e68507ebb705cd46438c534c7a3a1
      nvidia/diar_streaming_sortformer_4spk-v2 @5240a64  diar_streaming_sortformer_4spk-v2.q8_0.gguf  0679cfeb1ce356d0dea9470b31274f4bfc7eb927497d82005483770666da998a
      (fetch these directly: `nemo-speech pull` v0.2.0 breaks on a cache path with spaces)
<bench>/models/<name>/             github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/<file>.tar.bz2
      nemotron-streaming-en-1120ms  sherpa-onnx-nemotron-speech-streaming-en-0.6b-1120ms-int8-2026-04-25  840c48deed02d4a5975716e7b12dc0a8b1ba620776c6366f7e5677d8907edd73
      nemotron35-streaming-1120ms   sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-1120ms-int8-2026-06-11    adbdd5e9fef87300c37cebfcfc4f1ebe56845c860c8a760af0a1dd65ce9beed3
      parakeet-tdt-0.6b-v3-int8     sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8                           5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf
      cohere-transcribe-int8        sherpa-onnx-cohere-transcribe-14-lang-int8-2026-04-01                bd582588d50685a795dcd2807ab77e11361b8312d96c53884682def45ab4206d
<bench>/models/granite-speech-5.0-2b/   huggingface.co/ibm-granite/granite-speech-5.0-2b-GGUF @945b458
      granite-speech-5.0-2b-Q8_0.gguf   4972786fa455b71ee1f020e8ae1b147ca6cd2b1e5b0818048011d7f1da242790
      mmproj-model-f16.gguf             c0d108e1a3cf345b22c411245fc2762319f1c6b6e56bb15bdffe748c9342ca7d
```

Sizes: NeMo-Speech.cpp zips 5.5 / 19.3 MB; llama.cpp zips 24.4 / 37.3 MB; Parakeet q8_0 GGUF
714 MB; Nemotron streaming q8_0 700 MB; Nemotron-3-Diarization 107 MB; Sortformer v2 147 MB;
sherpa Nemotron 1120 ms int8 464 MB (633 MB unpacked); Nemotron 3.5 int8 475 MB; Parakeet int8
487 MB; Cohere int8 1.70 GB (2.9 GB unpacked); Granite Q8_0 1.96 GB + mmproj 1.16 GB.

## Running

```
FLYONTHEWALL_HARNESS_BENCH_DIR=<bench> \
FLYONTHEWALL_HARNESS_CORPUS=<corpus.json> \
FLYONTHEWALL_HARNESS_ENGINES=nemocpp-parakeet \
FLYONTHEWALL_HARNESS_GPU=1 \                 # NeMo-Speech.cpp / llama.cpp on Vulkan
FLYONTHEWALL_HARNESS_BENCH_THREADS=4 \       # CPU threads for batch engines
  cargo test -p fly-app --release --test accuracy_harness -- --ignored --nocapture
```

sherpa-onnx has no Vulkan provider, so its engines are CPU only (a `-vulkan` variant SKIPs).
Bench runtimes start at the sidecars' below-normal priority. On a machine other work keeps
at 100 %, they're starved (Nemotron live measured 0.1× realtime that way), so take timings on
a quiet machine and read `resources.machine_cpu_avg_pct`.

`scripts/asr-bench/cut_excerpt.py <wav> <vtt> <out_dir> <start_s> <len_s>` cuts a scoring
excerpt: audio and `.vtt` reference cropped to the same window, with both edges snapped to
silent gaps between reference cues.

## Extra metrics in the result files

| Field | Meaning |
|---|---|
| `speech_holes` | Stretches of ≥ 5 s with no transcribed word whose audio is mostly speech-level energy (the rule the shipped Parakeet engine uses to catch parakeet-cli's silent decoder), measured on the final transcript of any engine. Reference-free; compare engines on the same audio |
| `call_cpu_pct` | ASR CPU-seconds ÷ (audio seconds × logical CPUs): what the engine would cost spread over a call of that length. The "CPU ≤ 25 % during a call" gate |
| `bench.live_latency` | Live variants: time from each speech onset (after ≥ 1.5 s of silence) to the first text on screen, assuming audio arrives in real time (`first_partial_p50_s/p90_s/max_s`), and every token's own delay |
| `speed_x_decode`, `decode_secs` | Speed without server start, model load or warmup |
| `bench.*_load_secs` | Server or model start-up (includes warmup) |
| `bench.timing`, `bench.transfer_*` | How LLM-decoder words were timed (see above) |

Compare mode adds two gates for live variants: first partial ≤ 2 s (p90) and CPU ≤ 25 % during
a call.

## Runtime notes (measured 2026-10-28/29, RTX 3050 4 GB laptop, i7-12700H)

- **NeMo-Speech.cpp v0.2.0**: prebuilt Windows CPU and Vulkan zips; JSON with per-word
  start/end; splits audio over its 400 s encoder limit internally; stable C ABI
  (`include/nemo_speech/asr.h`: `nemo_speech_asr_recognize_f32`, a push/next streaming API,
  word start/end/speaker accessors). The ASR-only DLL set is about 6 MB on CPU, plus 44 MB
  for `ggml-vulkan.dll`. Apache-2.0, with NOTICE and third-party notices. Its warmup
  trips `GGML_ASSERT(ne3 == ne13)` with the Nemotron model on Vulkan (upstream #48);
  `--no-warmup` avoids it. Process start costs ~12 s of warmup on CPU, so keep one
  recognizer/server per recording, never one per batch.
- **sherpa-onnx Nemotron streaming**: no offline export (the encoder is cache-aware), so
  "offline" means the streaming model over batches. The sherpa CLI's default 0.5 s leading
  silence made a 60 s AMI clip worse (lost punctuation and a word); feed size made no
  difference. The worker uses no padding.
- **Cohere Transcribe** (int8, CPU): ~2.2× realtime on 4 contended threads, 30 s model load,
  no timestamps.
- **Granite Speech 5.0 2B** (Q8_0): 0.4× realtime on CPU, so Vulkan only; the first request
  pays ~2 min of Vulkan shader warmup, then ~2.2× realtime on a loaded machine.

## Licenses, if any of these ships (read 2026-10-28; an engineering summary, not legal advice)

| Model / runtime | License | What shipping needs |
|---|---|---|
| NeMo-Speech.cpp | Apache-2.0 (+ third-party notices: ggml, llama.cpp, SentencePiece, miniaudio, …) | Ship `LICENSE`, `NOTICE` and `THIRD_PARTY_NOTICES.md` with the DLLs |
| Parakeet TDT 0.6B v3 | CC-BY-4.0 | Credit NVIDIA, link the model and the license, say if converted (int8/GGUF). No patent grant |
| nemotron-speech-streaming-en-0.6b | NVIDIA Open Model License (revocable; NVIDIA may change it) | Runtime download: show the license name + link before download. If we mirror it: ship the full license and a NOTICE reading "Licensed by NVIDIA Corporation under the NVIDIA Open Model License". It terminates on bypassing "technical limitations" or on IP litigation; Trustworthy-AI terms (lawful use, which ties to recording consent); licensee indemnifies NVIDIA. Needs the user's OK (spec §4.1) |
| nemotron-3.5-asr-streaming-0.6b | OpenMDW-1.1 | Keep the license and origin notices; no field-of-use limits. Avoids the Open Model License's red flags |
| Nemotron-3-Diarization | OpenMDW-1.1 | Same as above (litigation-termination clause, no cure period) |
| Cohere Transcribe | Apache-2.0 (HF repo is gated; the sherpa int8 package isn't) | License text, keep notices, mark modified files |
| Granite Speech 5.0 2B | Apache-2.0 | Same. Avoid `granite-speech-5.0-470m-turboctc-nc` (CC-BY-NC-SA) |

Downloading from the official host at runtime makes the user, not us, the recipient. Mirroring
(for example on our GitHub releases) makes us the distributor, and every condition above
applies. The sherpa-onnx packages ship no license or NOTICE file, so we would add them.

## Results (2026-10-29, provisional)

Two 10-minute AMI excerpts (public, CC BY 4.0) cut with `cut_excerpt.py`:
`ami-ES2004a-ihm-x10` (headset mix, 05:06–14:56) and `ami-TS3003a-sdm-x10` (single distant mic,
05:32–14:59). Whisper and `parakeet-cli` rows are the 2026-10-27/28 full-meeting runs cropped to
the same window ("cached"); everything else ran on the excerpt. The machine was shared with
other builds throughout (machine CPU 50–100 %), so speeds are **contended** lower bounds. WER and
DER don't depend on load. The user's Teams excerpt was scored too; it stays local, like all
Teams results.

### English engine and runtime

| Engine (runtime, input) | ES2004a-ihm WER / SA-WER | TS3003a-sdm WER / SA-WER | speech holes (ihm/sdm) | ×realtime (Vulkan) |
|---|---|---|---|---|
| Whisper large-v3-turbo (shipped default, cached) | 23.0 / 29.8 | 49.6 / 52.5 | 0 / 5 | — |
| Parakeet v3, `parakeet-cli`, VAD batches + hole guard (cached) | 20.9 / 27.3 | 53.4 / 55.2 | 2 / 18 | 8.4 (fresh) |
| Parakeet v3, NeMo-Speech.cpp, VAD batches | 24.6 / 30.7 | 50.9 / 53.4 | 3 / 10 | 30–47 |
| **Parakeet v3, NeMo-Speech.cpp, contiguous ≤ 120 s windows** | **20.5 / 27.2** | **39.7 / 43.4** | 2 / 16 | 24–41 |
| Parakeet v3, NeMo-Speech.cpp, whole file in one call (CPU) | 22.2 / – | – | 2 / – | — |
| Nemotron streaming 0.6B, NeMo-Speech.cpp q8, 1.12 s | 23.0 / 28.2 | 51.3 / 53.6 | 3 / 15 | 1.8–39 |
| Nemotron streaming 0.6B, sherpa-onnx int8, live, 2 threads | 23.3 / 28.2 | – | 2 / – | CPU 0.5 (93 % busy) |

- The shipped energy VAD's silence removal costs Parakeet a lot on distant-mic audio: the same
  runtime and model go from 50.9 % (VAD batches) to 39.7 % (raw windows) on TS3003a-sdm.
  Silero VAD (v2.5 2.3) and the batching design should be measured with this in mind.
- `parakeet-cli` given the whole 590 s excerpt in one call ran 31 min, reached 8.9 GB of memory
  and produced nothing; it was stopped. NeMo-Speech.cpp transcribed the same file in one call
  with words right up to 589.9 s (WER 22.2 %).
- CPU cost is the same for both runtimes: 1.44 (NeMo-Speech.cpp) vs 1.55 (`parakeet-cli`)
  CPU-seconds per audio second at 4 and 8 threads. NeMo-Speech.cpp's Vulkan lead comes from
  one loaded model per recording (no per-batch reload, no hole re-decodes).
- Both Nemotron runtimes emit the same 21× one-word loop inside 0.5 s at 305 s of ES2004a
  (a transducer repeating a token); the pipeline's loop guard collapses it.

### Live captions (Nemotron streaming, sherpa-onnx int8, 1.12 s chunks, 2 threads)

| | ES2004a-ihm (93 % busy) |
|---|---|
| first text after a speech onset, p50 / p90 | 2.65 / 5.54 s (starved: one chunk took 52 s) |
| algorithmic only (chunk + lookahead), first token after a pause, p50 / p90 | 1.12 / 1.28 s |
| CPU during a call (share of the machine) | 9.7 % |

On a less-loaded run of the local Teams excerpt (73 % busy): first text p50/p90 2.10/2.44 s
from the VAD onset (which includes the VAD's pre-roll), 1.66/1.77 s from the token's own time,
CPU 7.9 %. The 560 ms export would cut about half a second at a small WER cost (NVIDIA:
7.07 % vs 6.93 % average).

### Diarizer (words fixed from the cached `parakeet-vulkan` transcript)

| Diarizer | ES2004a-ihm turn DER250 / DER0 | clusters | words with wrong speaker | TS3003a-sdm turn DER250 | clusters | wall s (ihm / sdm) |
|---|---|---|---|---|---|---|
| shipped: sherpa-onnx + CAM++ + VBx | 25.5 / 34.1 | 4 / 4 | 7.7 % | 29.0 (conf 6.1) | 1 / 4 | 936 / 425 |
| Nemotron-3-Diarization, default 1.12 s geometry | 18.6 / 24.6 | 4 / 4 | 4.7 % | 32.5 | 3 / 4 | 905 / 1373 |
| **Nemotron-3-Diarization, card's 30.4 s config** | **18.8 / 25.1** | 4 / 4 | **4.2 %** | 30.5 (conf 0.0) | 3 / 4 | **65 / 56** |

TS3003a's window is dominated by one speaker, so it's a weak diarization test: Nemotron finds
3 of 4 speakers where the shipped pipeline collapses to one, but misses more quiet far-field
speech (miss 30 % vs 22 %). The 30.4 s config is as accurate as the default and ~13× cheaper
(195 vs 2,486 CPU-seconds on ES2004a).

### Not run

Cohere Transcribe and Granite Speech 5.0 2B: smoke clips only (28–60 s), on a busy machine.
Granite gave the most complete transcript of the clip but ran at 0.4× realtime on CPU and
~2.2× on Vulkan; Cohere ran at ~2.2× on 4 CPU threads with no timestamps. Neither is a default
candidate on this hardware.
