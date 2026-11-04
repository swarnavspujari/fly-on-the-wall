# The default local model, picked from this computer's memory

Shipped in v2.5. The code is `src-tauri/src/local_models.rs` (the rule, the one
table of numbers, the migration, the offer) and `frontend/src/localModels.ts` +
`frontend/src/components/LocalModelChoices.tsx` (what Settings says). The
evidence is the Ask bench on real local models ([ASK-MODEL-BENCH.md](ASK-MODEL-BENCH.md)),
and §5 here for the other local tasks.

## 1. The rule

| Installed memory (as the OS reports it) | Default | Download | Held while answering |
|---|---|---|---|
| 15 GB or more (the 16 GB class and up, **including 32 GB**) | `qwen3.5:9b` | 6.6 GB | 6.2 GB |
| 11 GB up to 15 GB (the 12 GB class) | `llama3.1` | 4.9 GB | 5.8 GB |
| under 11 GB (8 GB class and below) | `qwen3.5:4b` | 3.3 GB | 3.6 GB |

The cut-offs come from each model's measured footprint plus what else runs
during a meeting (the table's comments in `local_models.rs` have the
arithmetic):

* `qwen3.5:9b`: 6.2 GB + ~9 GB for the OS, this app, the speech engine (1–3 GB
  while transcribing), the search model and a call or browser app → **15 GB**
  (a "16 GB" computer reads 15.2–16.0).
* `llama3.1`: 5.8 GB + ~5 GB → **11 GB**.
* `qwen3.5:4b`: 3.6 GB + ~3.5 GB → an 8 GB computer with little else open.
  Below ~7 GB nothing measured fits well; it is still the lightest.

**Inputs.** Memory only, plus the running Ollama's version, which can only take
a model away (`qwen3.5` needs Ollama 0.17.5; both managed runtimes, 0.31.2 on
macOS and 0.40.13 on Windows from #137, are far past it). A user-run Ollama older
than that is shown the held-back model and "update Ollama to use it", and is
never offered it.

* **Graphics memory is not an input.** The bench ran on a 4 GB GPU with 24–75%
  of each model on it and still met the quality floors; a bigger GPU only
  makes answers faster, and every computer with 8 GB or more of graphics memory
  has 16 GB or more of RAM anyway. A computer with no GPU is slower; the speed
  line says speeds vary.
* **Apple Silicon** has one memory pool, so installed memory is the figure, with
  the same cut-offs. The measured speeds understate a Mac, whose GPU runs the
  whole model.
* **Linux and Intel Macs** use installed memory the same way.
* **Not promoted:** `gemma4:12b-it-qat` meets the same floors but is 35%
  slower and 1.8 GB bigger; the 35B and 26B models were not run. 32 GB computers
  stay on `qwen3.5:9b`.

## 2. It never switches anyone

| Who | What they get |
|---|---|
| **New install** (no notes, no accepted first-run notice, no provider setting) | At the first launch that knows about this, the recommendation for the computer is **pinned** (`llm.ollama.default_model`). Settings › Ask and Enhance shows it as the model to download, with its size and the numbers; the user confirms the download. |
| **Existing install** | Pinned to **`llama3.1`**, what it was already running. If the computer qualifies for a better model it hasn't got, Settings shows a **one-time offer**; nothing downloads or switches until the user clicks "Download and switch". "Not now" ends the offer for that model for good. |
| **An explicit choice** (`llm.ollama.model`, set in Settings or by clicking a model) | Always wins, and is never offered over. Clearing it falls back to the pinned default, never to a model the user never saw. |
| **A cloud provider** | No offer (the offer is about a local model). It appears if they move to Ollama. |

One edge: a computer that has Ollama and `llama3.1` installed but no notes, no accepted first-run notice and no provider setting reads as a new install, so it is pinned to the recommendation and shows that model as "to download" while `llama3.1` sits unused on disk. It has never used the old default, nothing it made is affected, and the picker lists `llama3.1` as one click.

The pin is written once. A later run on a bigger or smaller computer, or a new
recommendation, changes nothing by itself; an existing install can only get an
offer. The 4B is never offered over `llama3.1` (they tie on quality, §4.1 of the
bench); it is only ever the fit-driven default for a new install.

The offer's accept path downloads first, then makes the model the user's
explicit choice, so there is no moment when Ask and Enhance point at a model
that isn't on the disk. A failed download switches nothing. After a switch the
old model is offered for deletion (the existing prompt), never deleted.

## 3. What the user is shown

Where the user picks a model (Settings › Ask and Enhance › Local model, and the
offer), each model carries the numbers measured for it, in plain words:

> Answers backed by a source: 83%. Declines what it can’t answer: 94%. About 30
> seconds per answer. Uses about 6 GB of memory while answering. Suits computers
> with 16 GB or more.
>
> Measured on one test laptop (4 GB graphics memory, 32 GB RAM) while other
> programs were running. Your computer will differ. How this was measured

* "Answers backed by a source" is the bench's grounded rate: of 60 questions the
  meetings answer, the share answered citing a line the model was shown.
  "Declines what it can’t answer" is the correct-refusal rate over 16 questions
  the meetings don't answer. Seconds are the median warm Ask, rounded to 5 s,
  measured while other work was compiling on the laptop.
* The table is **one static table in Rust** (`CANDIDATES` in `local_models.rs`);
  the UI reads it through `local_model_advice`. Numbers only: no fixture or gold
  text. When the bench is re-run, update that table and its comments, and
  `devMock.ts` (`MOCK_MODEL_TABLE`).

## 4. Tier, profile and runtime

* **Ask tier.** `qwen3.5:9b` passes the tool smoke test and runs as tier M (tools,
  ≤ 3 calls, 2 rounds); the bench also showed it meets every tier-S floor with no
  tools. `llama3.1` and `qwen3.5:4b` run as tier S.
* **Profile.** The `qwen3.5` entry in `fly-core`'s `prompt_profile.rs` matches
  `qwen3.5:9b` and `:4b` by base name: thinking off on every task, JSON schemas on
  Polish and Extraction, **no schema on Enhance** (it collapses qwen to an empty
  array).
* **Runtime.** `qwen3.5` came to Ollama in 0.17.4, its 4B and 9B sizes in 0.17.5;
  the pull checks too.

## 5. The other local tasks

The setting drives every local task, not only Ask: Enhance, Polish, action-item
Extraction. (The title comes with Enhance; there is no separate title task, and the
connection test is one short call.) Each of them builds its provider from the same
resolved default (`build_provider`; a test pins it), so the recommended model runs all
of them.

A light pass on `qwen3.5:9b` with the existing LLM bench (`src-tauri/tests/llm_bench.rs`,
its "default" variant, which is the profile the app resolves for the model): three
synthetic meetings (standup, budget, product) through Enhance, Polish and Extraction on
Ollama 0.40.14, on a machine other work was running on.

| Task | Contract | Per meeting |
|---|---|---|
| Enhance (note blocks) | **3 of 3** pass | 115 s, 98 s, 89 s |
| Polish (cleaned transcript) | **3 of 3** pass, every segment kept (29/29, 27/27, 29/29), 0 flags | 175 s, 150 s, 222 s |
| Extraction (decisions and action items) | **3 of 3** pass, every item read back (13/13, 16/16, 13/13) | 170 s, 206 s, 234 s |

All nine contracts pass, with the `qwen3.5` profile as it is (thinking off everywhere,
JSON schemas on Polish and Extraction, **none on Enhance**). That matches what
`BENCHMARKS.md` §10–11 recorded for `llama3.1` (27 of 27) and `qwen3.5:4b` (17 of 18).
Two limits: the contracts check that the output parses and keeps its content, not
how good the notes are (the judge needs a cloud key, so Enhance quality wasn't
scored here), and three meetings are a light pass, not a re-bench.

The cost is time. These tasks run in the background after a meeting, but the 9B took
roughly 1.5 to 2.5 times what `BENCHMARKS.md` §11 recorded for `llama3.1`
(68 / 108 / 88 s for Enhance / Polish / Extraction, on a quieter day), so an hour-long
meeting's follow-up work takes a few minutes longer. A model that failed a task contract
would not have shipped as a default; none did.

## 6. Changing it

1. Re-run the bench (`ASK-MODEL-BENCH.md` §9) and update `CANDIDATES`
   (`measured`, `memory_gb`, `min_ram_gb`) and its comments together; the tests pin
   the boundaries, so a changed cut-off fails them on purpose.
2. Mirror the numbers in `devMock.ts`.
3. A new model also needs a `prompt_profile.rs` entry if it has a measured
   failure (the checklist there), and a tool smoke result for the tier.
