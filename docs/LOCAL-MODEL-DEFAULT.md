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
than that is handled in three places, all in plain words:

* The better model is **held back**: the picker rules it out ("Needs Ollama 0.17.5
  or newer (this computer has 0.16.9)"), Settings says "update Ollama to use it", and
  it is never offered.
* A **pinned default** that the running Ollama can't pull (a new install on an old
  Ollama) shows the same note next to a **disabled** Download button, instead of a
  download that is certain to fail.
* If Ollama refuses a download anyway (its version couldn't be read, so nothing was
  held back), its "requires a newer version" message becomes "qwen3.5:9b needs a newer
  Ollama. Update Ollama (ollama.com), then try again." Any other error is shown as it came.

A version that is unknown (the server isn't running), unreadable, or `0.0.0` (what a
development build of Ollama reports) counts as supported: the pull says so itself if it
can't.

**Memory unreadable.** If the OS won't say (a reading under 1 GB), nothing is guessed:
no pin is written and no offer is made, the default stays `llama3.1`, the picker shows
every model without a "this computer has less memory" claim. That launch still writes
the hardware cache, so the next one counts as an existing install: it pins `llama3.1`
(not the recommendation), and if the reading is good by then the better model arrives as
the one-time offer (§2), never as a switch.

* **Graphics memory is not an input.** The bench ran on a 4 GB GPU with 24–75%
  of each model on it and still met the quality floors; a bigger GPU only
  makes answers faster, and every computer with 8 GB or more of graphics memory
  has 16 GB or more of RAM anyway. A computer with no GPU is slower; the caption
  under the numbers says "your computer will differ".
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
| **New install** (none of the signs below) | At the first launch that knows about this, the recommendation for the computer is **pinned** (`llm.ollama.default_model`). Settings › Ask and Enhance shows it as the model to download, with its size and the numbers; the user confirms the download. |
| **Existing install** (any sign: a note, an accepted first-run notice, any provider setting, the hardware cache, the v2.4 consent migration) | Pinned to **`llama3.1`**, what it was already running. If the computer qualifies for a better model it hasn't got, Settings shows a **one-time offer**; nothing downloads or switches until the user clicks "Download and switch". "Not now" ends the offer for that model for good. |
| **Models already on the disk** (Ollama keeps them when "delete all my data" wipes the library, so a wiped library can look new) | `llama3.1` on the disk → pinned to `llama3.1` (it was the old default, so this is an existing user). Otherwise an offered model that is already downloaded is pinned instead of asking for a second download: the recommended one if it is there, else the best one that fits. The folders checked are the app's own, `OLLAMA_MODELS`, and Ollama's default. |
| **An explicit choice** (`llm.ollama.model`, set in Settings or by clicking a model) | Always wins, and is never offered over. Clearing it falls back to the pinned default, never to a model the user never saw. |
| **A cloud provider** | No offer (the offer is about a local model). It appears if they move to Ollama. |
| **An Ollama on another computer** (`llm.ollama.base_url` not on this machine) | No offer: the memory that matters is the server's, which the app can't read. The picker still lists the models. |

The pin is written once. A later run on a bigger or smaller computer, or a new
recommendation, changes nothing by itself; an existing install can only get an
offer. The 4B is never offered over `llama3.1` (they tie on quality, §4.1 of the
bench); it is only ever the fit-driven default for a new install.

The offer's accept path downloads first, then makes the model the user's
explicit choice, so there is no moment when Ask and Enhance point at a model
that isn't on the disk. A failed download switches nothing. After a switch the
old model is offered for deletion (the existing prompt), never deleted.

**Disk room.** Before a download starts, the app checks the drive the model will land
on and refuses when the free space is less than what is still to download plus 2.0 GB to
spare:

> Not enough free disk space for qwen3.5:9b. It needs about 8.6 GB free (a 6.6 GB
> download plus 2.0 GB to spare) and this drive has 3.1 GB. Free up space (Settings ›
> Your files and privacy shows what the app keeps), or choose a smaller model.

* **A resumed download only counts what is left.** Ollama keeps an interrupted layer as a
  sparse `blobs/<digest>-partial` (its length is the whole layer) and a small
  `<digest>-partial-<n>` JSON per chunk with the bytes written (`Completed`). The app adds
  those up (only records whose data file is beside them) and takes them off the download:
  a 6.6 GB pull that dropped at 5 GB needs 1.6 GB plus the 2.0 GB, and the message says
  "1.6 GB still to download of 6.6 GB". The credit never exceeds the download, because a
  dropped pull of another model leaves files that can't be told apart.
* **Which folder.** For the runtime the app manages, its own models folder, always. For a
  server the user runs on this computer, `OLLAMA_MODELS` when set, else `~/.ollama/models`,
  else (Linux) `/usr/share/ollama/.ollama/models`, where the package's service keeps them;
  a folder is trusted only if it exists and holds `manifests/`. Links, junctions and
  symlinks are followed before the free space is read, so a store moved to another drive is
  measured there.

Nothing is refused when the free space can't be read, when the model is already on the
disk, for a model that isn't in the table, for a server on another computer, or when the
store can't be found: a location this app can't see (the Ollama app's "Model location"
setting, a service's own `OLLAMA_MODELS`) isn't guessed at, because measuring the home
drive could refuse a pull that fits or pass one that doesn't. (If a stale
`~/.ollama/models` is also there, that one is what is measured.) The refusal reaches
Settings as it is written, and nothing is switched.

## 3. What the user is shown

Where the user picks a model (Settings › Ask and Enhance › Local model, and the
offer), each model carries the numbers measured for it, in plain words:

> Answers backed by a source: 83%. Declines what it can’t answer: 94%. About 30
> seconds per answer. Uses about 6 GB of memory while answering. Suits computers
> with 16 GB or more. Notes and cleanup after a meeting take about 1.5–2.5 times as
> long as with llama3.1.
>
> Measured on one test laptop (4 GB graphics memory, 32 GB RAM, Ollama 0.40) while
> other programs were running. Your computer will differ. How this was measured

* "Answers backed by a source" is the bench's grounded rate: of 60 questions the
  meetings answer, the share answered citing a line the model was shown.
  "Declines what it can’t answer" is the correct-refusal rate over 16 questions
  the meetings don't answer. Seconds are the median warm Ask, rounded to 5 s,
  measured while other work was compiling on the laptop.
* **Two kinds of line, kept apart.** The per-model lines are numbers measured for that
  model. The caption is the one honest label for all of them: one laptop, other
  programs running, the Ollama version it ran on, so speeds are indicative and quality
  isn't affected by what else was running. The "1.5–2.5 times as long" line appears
  only on a model that costs more (the 9B, §5), where the user picks, so the extra
  time is part of the choice and not a surprise after a meeting.
* The table is **one static table in Rust** (`CANDIDATES` in `local_models.rs`);
  the UI reads it through `local_model_advice`. Numbers only: no fixture or gold
  text. The same table is `frontend/src/localModels.table.json`, which the dev mock
  (`devMock.ts`) imports, and a Rust test fails when the two differ. When the bench is
  re-run, update `CANDIDATES` and its comments, then regenerate the JSON with
  `FLY_BLESS_TABLE=1 cargo test -p fly-app --lib the_shared_table_matches_the_product_table`
  and commit both.

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
Ollama 0.40.14, on a machine other work was running on. It was run twice: first on
main before #134–#137 landed, then again on main with them (and #138's own changes)
merged, so the result stands on the code that ships.

| Task | Contract | Per meeting, merged main | Per meeting, first pass |
|---|---|---|---|
| Enhance (note blocks) | **3 of 3** pass | 117 s, 129 s, 134 s | 115 s, 98 s, 89 s |
| Polish (cleaned transcript) | **3 of 3** pass, every segment kept (29/29, 27/27, 29/29), 0 flags | 237 s, 204 s, 169 s | 175 s, 150 s, 222 s |
| Extraction (decisions and action items) | **3 of 3** pass, every item read back (13/13, 16/16, 13/13) | 172 s, 213 s, 220 s | 170 s, 206 s, 234 s |

All nine contracts pass on both passes, with the `qwen3.5` profile as it is (thinking off
everywhere, JSON schemas on Polish and Extraction, **none on Enhance**). That matches
what `BENCHMARKS.md` §10–11 recorded for `llama3.1` (27 of 27) and `qwen3.5:4b` (17 of
18). Two limits: the contracts check that the output parses and keeps its content, not
how good the notes are (the judge needs a cloud key, so Enhance quality wasn't
scored here), and three meetings are a light pass, not a re-bench.

The cost is time. These tasks run in the background after a meeting, but the 9B took
roughly 1.5 to 2.5 times what `BENCHMARKS.md` §11 recorded for `llama3.1`
(68 / 108 / 88 s for Enhance / Polish / Extraction, on a quieter day), so an hour-long
meeting's follow-up work takes a few minutes longer. Across the two passes the single
runs ranged from 1.3 to 2.7 times; the middle of both is what Settings says
("about 1.5–2.5 times as long as with llama3.1", `background_slower` in `CANDIDATES`),
and the machine was busier than on the day `llama3.1` was timed, so part of the ratio is
load, not the model. A model that failed a task contract would not have shipped as a
default; none did.

## 6. Changing it

1. Re-run the bench (`ASK-MODEL-BENCH.md` §9) and update `CANDIDATES`
   (`measured`, `memory_gb`, `min_ram_gb`) and its comments together; the tests pin
   the boundaries, so a changed cut-off fails them on purpose.
2. Regenerate `frontend/src/localModels.table.json` (§3); `devMock.ts` and the
   frontend tests read it, so there is no second copy to edit.
3. A new model also needs a `prompt_profile.rs` entry if it has a measured
   failure (the checklist there), and a tool smoke result for the tier.
