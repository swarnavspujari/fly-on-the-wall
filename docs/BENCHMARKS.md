# LLM Benchmarks — default local model decision

*Last run: 2026-07-13 → 07-14 (follow-up §6–§9), on a Windows 11 laptop — RTX 3050 Laptop (4 GB VRAM), 32 GB RAM,
12 cores. All local models served by Ollama 0.x on this machine; latency numbers are
end-to-end (prompt → full response) on this hardware and will differ on other machines.*

This document grounds the "which model should `llm.ollama.model` default to?" decision.
It reports a full run of the repeatable harness (`src-tauri/tests/llm_bench.rs`) over the
app's four LLM tasks. **No model switch ships with this report** — it is evidence, not a
decision.

## 1. Verifying the external model claims

The external research suggested "Qwen3.5-4B" and "Gemma 4 E4B" as stronger small models.
Checked against the Ollama registry (ollama.com/library) on 2026-07-13, then confirmed by
actually pulling each model:

| Claim | Registry reality | Verdict |
|---|---|---|
| "Qwen3.5-4B" | `qwen3.5:4b` exists — 3.4 GB, 256K context, multimodal family, 15.5M pulls | **Real** |
| "Gemma 4 E4B" | `gemma4:e4b` exists — 9.6 GB, 128K context (gemma4 also ships 12b/26b/31b; the search page hides the e-tags) | **Real** |
| — (third candidate, own judgment) | `gemma3:4b` — 3.3 GB, 128K context, 38.5M pulls; the most-used current ~4B general model | Added |

Baseline: `llama3.1` (8B, 4.9 GB — today's default). Reference: `claude-sonnet-5` via the
existing AnthropicProvider (the app's default cloud model).

## 2. Methodology

- **Harness**: `src-tauri/tests/llm_bench.rs` (`#[ignore]`d test; run instructions in the
  file header and §6). It drives the *shipped* code paths — `build_enhance_prompt` /
  `parse_enhanced_blocks`, the polish batch loop + retention guard (`apply_cleanup`),
  `extraction.rs` prompt/parse/validate, and `llm_commands::build_ask_system` — over three
  committed fixture transcripts (`src-tauri/fixtures/bench/`): a 3-person standup, a 2-person
  budget review, and a 4-person product review (~30 segments each, seeded with disfluencies,
  figures, decisions, action items, and deliberately-unanswered questions).
- **Contract compliance** (hard pass/fail): Enhance = output parses as a JSON block array
  (the shipped fallback to untraced paragraphs counts as FAIL — it silently loses
  provenance); Polish = every batch parses AND zero retention-guard flags AND provenance
  preserved; Extraction = parseable AND ≥1 valid item AND ≤50 % of raw items dropped by
  validation.
- **Quality (LLM judge)**: 15 curated Ask questions (action items, decisions, "what did X
  say about Y", numbers recall) plus the Enhance notes are scored 1–5 by `claude-sonnet-5`
  against the sonnet-5 reference output, one-line justification required per score.
  **Circularity caveat**: Sonnet is judging closeness-to-Sonnet. This biases scores toward
  sonnet-ish phrasing and structure; treat judge scores as "how close to our current cloud
  quality bar", not as absolute quality. Contract compliance and factual-recall failures
  are the harder signals.
- **Latency**: wall-clock per call (`Instant`), full response, no streaming.
- **Peak memory**: peak RSS summed over all `ollama*` processes, sampled at 250 ms during
  each call (local models only).

## 3. Results

### 3.0 qwen3.5:4b is unusable through the app's Ollama path — measured, then excluded

Qwen3.5 **reasons unconditionally**. Through Ollama's OpenAI-compat endpoint — the only
endpoint the app's `OpenAiCompatProvider` speaks — the reasoning goes to a separate
`reasoning` field the provider doesn't read, and with the app's `max_tokens` budgets the
model never finishes reasoning, so **`content` comes back empty**:

- Full harness call (Enhance, standup fixture, the shipped prompt): **1360 s, 0 blocks,
  contract FAIL** — the entire 4096-token budget went to reasoning.
- Minimal probe (2-decision JSON ask, 400 tokens): `finish=length`, `reasoning` = 1543
  chars, `content` = empty. The Qwen3 `/no_think` soft switch is gone in 3.5 — prepending
  it changed nothing (still 1700 chars of reasoning). **A prompt-profile preamble cannot
  fix this.**
- Ollama's **native** `/api/chat` with `think: false`: perfect JSON in **41 s**
  (vs 183 s → empty with thinking). The model itself is fine; the app's plumbing can't
  reach that switch today.

The remaining qwen3.5 harness tasks were **skipped explicitly** (each call is ~20 min of
guaranteed contract failure; the failure mode is fully characterized above).
**Superseded 2026-07-14:** fly-llm now has native `think` control and the full qwen3.5:4b
rows were run — see §6.

### 3.1 Score tables

_(from `target/llm-bench/*/results.json` + `judged.json`; default profile — i.e. the
prompts exactly as the app ships them)_

**Contract compliance** (hard pass/fail, 3 fixtures):

| Model | Enhance | Polish | Extraction |
|---|---|---|---|
| claude-sonnet-5 (reference) | 3/3 | 3/3 | 3/3 |
| llama3.1 (8B, baseline) | 2/3 | 2/3 | 3/3 |
| gemma3:4b | 3/3 | 2/3 | 2/3 |
| gemma4:e4b | 3/3 | 2/3 | 2/3 |

Every Polish failure was caught by the shipped lossless fallback (unparseable batch →
segments kept raw, zero retention-guard flags fired) — the no-loss design held for every
model.

**Judge scores** (claude-sonnet-5 scoring closeness to its own reference output, 1–5,
one-line justification each; see §2 for the circularity caveat):

| Model | Enhance (3 fixtures) | Ask (15 questions) |
|---|---|---|
| llama3.1 (8B, baseline) | 2.00 | 4.27 |
| gemma3:4b | 2.67 | 4.13 |
| gemma4:e4b | 2.67 | 4.33 |

**Latency (mean per call, this machine) + peak memory** (RSS = `ollama` + `llama-server`
processes, sampled at 15 s during the runs; VRAM from `/api/ps`):

| Model | Enhance | Polish (per fixture) | Extraction | Ask | Peak RSS | Peak VRAM |
|---|---|---|---|---|---|---|
| claude-sonnet-5 (reference) | 13 s | 10 s | 10 s | 4 s | n/a | n/a |
| llama3.1 (8B, baseline) | 261 s | 626 s | 312 s | 36 s | 8.7 GB | 2.2 GB |
| gemma3:4b | 357 s | 655 s | 487 s | 26 s | 7.3 GB | 2.3 GB |
| gemma4:e4b | 603 s | 891 s | 1160 s | 140 s | 5.9 GB | 2.4 GB |

Reading the table: local models on this hardware are **20–100× slower** than the cloud
reference on every task; gemma4:e4b's 140 s mean per Ask answer is not interactive.

### 3.2 The systematic small-model failure: spoken-number corruption

The budget fixture states the lease as "eighty four hundred" ($8,400/month) and SaaS
savings as "thirty two hundred" ($3,200/month) — the way people actually say numbers.
**Every local model corrupted both by 10×** (llama3.1: "$84k/month", "$32,200/month";
gemma3:4b: "$84,000", "$32,000"); claude-sonnet-5 got both right. For a meeting-notes
product whose Extraction feature exists to capture figures, this is the strongest
argument in the whole benchmark against a small local default. Judge lowlights (verbatim
one-liners):

- llama3.1 / ask q09 — 2/5: "Candidate reports $32,200/month, contradicting reference's
  $3,200/month figure, a likely fabricated/altered number despite correct owner and
  deadline."
- gemma3:4b / enhance budget — 2/5: "Multiple factual errors: lease is $8,400/month not
  $84,000, SaaS savings is $3,200/month not $32,000, action item owner for recruiter
  email is Alex not Dana…"
- gemma4:e4b / ask q10 — 2/5: "Candidate states July 20th while reference states July
  21st, a factual date discrepancy."

Ask quality is otherwise genuinely decent for all three (means 4.1–4.3/5): direct factual
recall is a small-model strength; structured multi-fact synthesis (Enhance) is where they
fall to 2.0–2.7/5 — coarse structure (gemma4 often returned ONE giant block), missing
metrics, misfiled action items.

## 4. Contract-failure examples (verbatim)

**llama3.1, Enhance (budget)** — emits a `"sources"` key *outside* its object, malformed
JSON, so the shipped parser falls back to untraced paragraphs (provenance lost):

```
  {"type": "ai", "markdown": "* Office lease renewed at $84k per month for 12 months"}, "sources": [13, 14, 16],
```

(Note the 10× number corruption inside the same line.)

**llama3.1, Polish (product)** — prose preamble is tolerated by the parser, but the JSON
itself has a delimiter error at char ~1789; batch dropped, all 29 segments kept raw:

```
Here is the cleaned transcript:

{"segments":[{"id":"seg001","speaker":"mic","start":0,"text":"Alright, this is the …
```

**gemma3:4b, Extraction (standup)** — `segment_ids` emitted as a *string* instead of an
array; serde rejects the batch, 0 items extracted:

```
    "speaker_key": "(mic)",
    "segment_ids": "[seg023]"
```

**gemma4:e4b, Polish (budget)** — returned a bare `[{"id","text"}…]` array instead of the
required `{"segments":[…]}` wrapper; batch dropped losslessly.

## 5. Recommendation as of 2026-07-13: don't switch (superseded by §9 after the follow-up run)

**Recommendation: keep `llama3.1` as the default local model for now.**

1. **No candidate wins decisively through the app's current plumbing.** gemma3:4b trades
   llama3.1's Enhance weakness (2/3 contract, judge 2.00 vs 2.67) for a new Extraction
   weakness (2/3 vs 3/3) at similar speed; gemma4:e4b matches gemma3's quality at 2–5×
   the latency (140 s per Ask answer). Nothing approaches the cloud reference.
2. **The most promising candidate is blocked on plumbing, not quality.** qwen3.5:4b
   produced perfect JSON in 41 s via Ollama-native `think: false` — by far the best
   small-model probe result — but is literally unusable (empty responses) through the
   OpenAI-compat path the app speaks (§3.0).
3. **All small models corrupt spoken numbers** (§3.2). Whatever the default, the honest
   product posture is that local = private-but-approximate and Anthropic remains the
   quality path.

**What would change the decision:** add Ollama-native `/api/chat` support (or `think`
control) to `fly-llm`, then re-run this benchmark with qwen3.5:4b included. Its probe
profile (fast, contract-clean, 256K context) suggests it could beat everything above.

### Prompt-profile adjustments (measured on the fly-core profile seam)

Variant runs (`LLM_BENCH_VARIANT=…`, single run per cell — treat small deltas as noise):

| Adjustment | gemma3:4b Extraction | llama3.1 Enhance |
|---|---|---|
| default | 2/3 (schema drift: `segment_ids` as string) | 2/3 (malformed JSON), judge 2.00 |
| **simplified contract** | **3/3**, item counts intact (12/9/8) | **3/3**, ~30 % faster (189 s vs 261 s) — but judge **1.67**: the contract's inline example ("Ship Friday", "my note") **leaks into the notes as invented content** |
| few-shot preamble | 3/3 but recall suppressed (5–6 items vs 11–13) | not run |
| "no preamble" instruction | 3/3, best item counts (12/12/9) | not run |

Takeaways: **simplified contract phrasing is the most effective single adjustment for
extraction-style tasks** (fixes schema drift without hurting recall); inline examples
are dangerous on llama3.1, which copies them into output — a future llama3.1 profile
should prefer an example-free simplified contract. The gemma3 standup extraction failure
was also fixed by the *no-preamble* variant, so that failure is likely stochastic —
another reason not to over-fit profiles to single runs. **The registry ships empty**: no
adjustment produced an unambiguous quality win for the current default model, and
per the registry's own rule, profiles are added only for measured, reproducible failures.

## 6. Follow-up run (2026-07-14): qwen3.5 unblocked via native `think` control

`fly-llm` now drives Ollama through its **native `POST /api/chat`**
(`OpenAiCompatProvider::chat_ollama_native`): `ThinkingMode::Disabled` maps to
`"think": false`, `Default` omits the field, `temperature`/`max_tokens` map to
`options.{temperature,num_predict}`. Non-thinking models ignore `think: false` on current
Ollama (probed on 0.32.0 against llama3.1 and gemma3:4b); a 400 mentioning thinking gets
one retry with the field stripped (that fallback is unit-tested but **not live-verified —
no old server on this machine**). OpenAI/NIM keep the compat path. 13/13 fly-llm unit
tests pass, covering both request-body shapes and both response parsers.

With that plumbing, the previously-skipped qwen3.5:4b rows (same fixtures, same judge):

| qwen3.5:4b run | Enhance | Polish | Extraction | Ask | Judge enh / ask | Mean latency (enh/pol/ext/ask) |
|---|---|---|---|---|---|---|
| as-shipped (thinking on for Enhance/Ask) | **0/3** (2 fully EMPTY) | ≡ nothink | ≡ nothink | 3/15 answers empty | 1.33 / 3.87 | 279 s / – / – / 88 s |
| `think:false` everywhere (`LLM_BENCH_THINKING=disabled`) | 2/3 | 2/3 | 2/3 | 1/15 empty | **3.00 / 4.47** | **62 s / 118 s / 75 s / 78 s** |

Peak RSS 3.2–5.2 GB (sampler now correctly includes Ollama's `llama-server` runner).

Read against §3.1: qwen3.5:4b with `think:false` posts **the best local judge scores in
the whole benchmark** (Enhance 3.00 vs 2.67 gemmas / 2.00 llama3.1; Ask 4.47 vs 4.2–4.3)
at **3–8× lower latency** (polish 118 s vs 626–891 s), with contract rates in the same
2/3 band as every other small model. It also handled the spoken-number questions best —
the only local model to answer "$3,200/month" correctly on q09 (all others corrupted it
10×) — though Enhance synthesis still fabricated figures ("~$6M pipeline" for $600k,
"$201,000" transposition), so §3.2's caveat stands in weakened form. Its failures:
one empty Ask answer and one truncated Enhance even with thinking off, and the polish
`product` batch failed to parse once (a replay of the identical prompt parsed fine —
stochastic, like the gemma polish failures; every polish failure remained lossless).

**Caveat:** thinking stays ON for Enhance/Ask as shipped (they use
`ThinkingMode::Default`, correct for Anthropic where adaptive thinking helps). The 3.00 /
4.47 numbers require flipping those two call sites to `Disabled` *for thinking local
models* — a one-line-per-site change once a `thinking` knob is added to the prompt-profile
registry (or the call sites branch on `provider.is_local()`). Not done here: it changes
app behavior beyond the plumbing this run needed.

## 7. Harness techniques for small models (researched + probed 2026-07-14)

What the community does to get reliable output from 4–8B local models, tested against
this app's actual failure cases where possible:

1. **Grammar-constrained decoding (Ollama structured outputs)** — pass a JSON schema in
   the native API's `format` field; Ollama compiles it to a llama.cpp GBNF grammar that
   masks invalid tokens, making malformed JSON *mechanically impossible*
   ([how it works](https://blog.danielclayton.co.uk/posts/ollama-structured-outputs/),
   [guide](https://llmconfigurator.com/en/guides/llm-json-structured-output),
   [llama.cpp grammars](https://deepwiki.com/ggml-org/llama.cpp/8.1-grammar-and-structured-output)).
   **Probed on llama3.1's exact failing Enhance case (budget fixture): 3/3 parses across
   repeats vs FAIL default, no example leakage, and 28–56 s vs 261 s average — ~5×
   faster** because constrained sampling stops the rambling. **BUT it does not fix
   content**: the constrained output still read "$84,400" and "$32,200/month" — §3.2's
   number corruption is comprehension, not formatting. Caveats: constrained decoding can
   dent reasoning-heavy tasks ([Let Me Speak Freely?, EMNLP 2024](https://arxiv.org/abs/2408.02442));
   Ollama Cloud models don't support it; and quality under constraint should be judged,
   not assumed (probe n=3, unjudged).
2. **Validate-and-retry loops** — parse/validate every response (the app already parses);
   on failure, re-ask once *including the validator error*. Community data puts 3 retries
   at ~70 % → 95 %+ success on 7–8B models ([failure patterns](https://explore.n1n.ai/blog/local-llm-json-output-failure-patterns-fix-2026-04-24),
   [instructor pattern](https://mljourney.com/how-to-get-reliable-structured-output-from-llms/)).
   The app currently falls back silently (Enhance → untraced paragraphs; Polish → raw
   segments); a single corrective retry before falling back is the cheapest upgrade and
   fits the existing `LlmError` plumbing. **Not measured here** — recommend measuring via
   a `LLM_BENCH_RETRY=1` harness knob before shipping.
3. **Flat schemas / two-step extraction** — small models handle nesting poorly; prefer
   flat arrays (the app's contracts are already flat — good) and split "find the facts"
   from "format the facts" when quality lags ([techniques](https://mychen76.medium.com/practical-techniques-to-constraint-llm-output-in-json-format-e3e72396c670)).
4. **temperature 0 for extraction-shaped tasks** — measurably improves field accuracy on
   7–8B models ([guide](https://llmconfigurator.com/en/guides/llm-json-structured-output)).
   Polish/Extraction already omit temperature; Ollama's default is 0.8 — pinning
   `options.temperature: 0` for those two tasks is a plausible free win (**unmeasured**).
5. **Explicit context sizing** — see §8: without `num_ctx`, everything else on this list
   is moot for long meetings.

Priority order for this app, grounded in the probes: fix `num_ctx` (§8) → structured
outputs for Extraction (pure extraction, negligible reasoning cost, probe-proven parse
fix) → corrective retry for Polish/Enhance → temperature 0 for Polish/Extraction.
Structured outputs for Enhance need a judged quality pass first (the EMNLP caveat applies
— Enhance is the most synthesis-heavy task).

## 8. Ollama component code review (2026-07-14, against current Ollama docs)

Surgical review of `src-tauri/src/ollama.rs`, `src-tauri/src/models.rs` (ollama artifact)
and `crates/fly-llm/src/openai_compat.rs` against docs.ollama.com:

1. **PROVEN BUG — silent context truncation.** Neither the old compat path nor the new
   native path sets `options.num_ctx`; the effective default on this machine is **~2048
   tokens** (marker test: 6k-token prompt → `prompt_eval_count=2050`, the system-prompt
   front was silently dropped and llama3.1 *hallucinated* the missing fact; with
   `num_ctx: 8000` the same prompt answered correctly). Ollama trims from the FRONT —
   exactly where the app puts the system prompt and (for Ask) the transcript. Impact
   today: **Ask** with any transcript beyond ~10–15 min of speech, and **Extraction**
   batches (3000 words ≈ 4k tokens) silently degrade; Enhance/Polish batches mostly fit
   (~1.8k tokens — verified, so the §3/§6 benchmark numbers are NOT contaminated).
   Recommended fix: size `options.num_ctx` from the prompt length (chars/3 + response
   budget, clamped to the model's `context_length` from `/api/show`) in the native path.
   ([community reports](https://github.com/ollama/ollama/issues/14259),
   [analysis](https://jangwook.net/en/blog/en/ollama-num-ctx-silent-truncation-experiment/))
   **Caution:** on this 4 GB-VRAM machine, `gemma4:e4b` with `num_ctx` 8192/16384 crashed
   the `llama-server` runner outright (HTTP 500, `0xc0000409` stack-buffer overrun) —
   raising num_ctx must degrade gracefully (retry at a smaller size on 500).
2. **Thinking control** — fixed this run (§6). Detection alternative: `/api/show`
   `capabilities` includes `"thinking"` (verified: qwen3.5 lists it, llama3.1 doesn't) if
   the retry-on-400 heuristic ever proves insufficient.
3. **Cross-platform posture is coherent.** Managed install (`ollama-bin` artifact) is
   Windows-only by design — macOS/Linux fall back to `find_on_path` + "install from
   ollama.com" UI, and `CREATE_NO_WINDOW` is correctly `#[cfg(windows)]`-guarded;
   `OLLAMA_MODELS` env and the spawn/kill lifecycle are OS-agnostic. No changes needed.
4. **Pinned runtime `v0.31.2` (Windows artifact) is current enough**: native `think`
   (≥0.9) and `format` structured outputs (≥0.5) both predate it. The user-run server
   probed here is 0.32.0. No action.
5. **Endpoints match the docs**: `/api/tags` (+`size` now read), `/api/pull` streaming
   shape, `/api/delete` (DELETE + `{"model"}` — verified live, 200/404 handling correct),
   `/api/version` liveness. `normalize_root`'s `/v1`-stripping matches what the native
   path needs and `ollama_root()` mirrors it.
6. **Minor, optional**: (a) chat requests have no client timeout — a hung runner blocks
   forever; consider a generous (10–15 min) ceiling now that latencies are measured;
   (b) `keep_alive` defaults to 5 min — the post-transcription polish→extract chain stays
   warm, but a user returning to Ask after idle pays a cold reload (llama3.1: ~16–20 s
   observed); (c) the docs' cloud-model deprecation table (e.g. `gemma3:4b` cloud retiring
   2026-07-15) does not affect local tags — no action.

## 9. Updated recommendation (superseded by §10 after the fixes shipped)

**Short term: still don't switch.** The as-shipped default (llama3.1) works everywhere
today, and qwen3.5:4b's headline numbers require flipping Enhance/Ask to
`ThinkingMode::Disabled` for local thinking models — an app change that should ride with
the switch, not precede it silently.

**The switch now has a concrete, evidenced path** (in order):
1. Ship the `num_ctx` fix (§8.1) — it improves every local model including the current
   default, and real meetings are hurt by it today.
2. Add a thinking knob to the per-model profile seam (or branch the two `Default` call
   sites on local thinking models), making qwen3.5:4b run at its measured 3.00/4.47.
3. Re-run this benchmark on ≥3 samples per cell (polish failures proved stochastic) and,
   if the qwen edge holds, switch the default to `qwen3.5:4b` with a profile entry.
4. Independently: structured outputs for Extraction (§7.1), corrective retry (§7.2).

## 10. Follow-up 2 (2026-07-14): fixes shipped, 3× repeat runs, harness engineering

Shipped since §9 (all committed on this branch): the `num_ctx` fix (§8.1 — auto-sized per
request, marker-tested live through the provider), `ChatRequest.format` (JSON-schema
constrained decoding on the native path), and the profile **thinking knob**
(`PromptProfile.disable_thinking`, first registry entry: `qwen3.5`). Every run below goes
through the app's now-shipped native path; each cell is **3 runs × 3 fixtures** (llama
default: 1 native-path run — its contract pattern matched the compat-path run and re-runs
were spent on the engineered cells instead). "Engineered" = community harness: `format`
JSON schemas on the three JSON tasks + temperature 0 on polish/extract; prompts identical.

| Config | Contracts E / P / X | Judge Enh / Ask | Latency E/P/X/Ask | Empty Asks |
|---|---|---|---|---|
| llama3.1 default | 2/3 · 2/3 · 3/3 | 1.67 / 4.33 | 64 / 167 / 124 / **11 s** | 0/15 |
| **llama3.1 engineered** | **9/9 · 9/9 · 9/9** | 2.22 / 4.27 | 65 / 111 / 91 / **11 s** | **0/45** |
| qwen3.5:4b shipped (profile) | 8/9 · 4/9 · 6/9 | **3.11** / 4.24 | 77 / 133 / 109 / 90 s | 4/45 |
| qwen3.5:4b engineered | 0/9† · 9/9 · 9/9 | 1.00† / 4.24 | 24† / 92 / 56 / 87 s | 5/45 |

† Degenerate: under the enhance schema qwen emits a grammar-valid EMPTY array `[]` in
~23 s — the "Let Me Speak Freely" failure mode in its purest form. Schema constraint on
the synthesis-heavy task destroys qwen and mildly *helps* llama (2.22 vs 1.67, no example
leakage since prompts are unchanged). **Harness engineering must be per-task.**

Other findings from this round:

- **The native API path alone is ~4× faster than OpenAI-compat** on identical prompts
  (llama default enhance 261 s → 64 s, polish 626 s → 167 s, ask 36 s → 11 s). All §3.1
  local latency numbers are superseded; local Ask at 11 s is genuinely usable.
- **qwen's empty-Ask problem persists** even with thinking off (4/45 shipped, 5/45
  engineered, `q03` failing in every shipped run) and did NOT reproduce in isolated
  replays of the identical request — an unresolved in-batch intermittent. The harness
  now needs `done_reason` logging on Ask records to diagnose further (not yet added).
- qwen's judged quality is stable across runs (Enhance 3.00/3.00/3.33) — the §6 result
  replicates.

### Revised recommendation

**Keep `llama3.1` as the default local model, and ship the engineered harness for it.**
With per-task `format` schemas + temperature 0 (plumbing already in fly-llm via
`ChatRequest.format`; the app's call sites just need to pass the schemas), llama3.1 went
**27/27 on contracts** with 11-second Ask answers and zero empty responses — it is now the
most reliable AND fastest local option measured, which matches this product's speed
requirement. qwen3.5:4b keeps the Enhance quality edge (3.11 vs 2.22) but pays for it
with 90-second Ask latency, 4/9 polish contracts, and unexplained empty answers — wrong
trade for a "fast local" mode; its profile entry stays (it's strictly better than
qwen-with-thinking for anyone who selects it manually).

**On closing the quality gap to sonnet-5** (Enhance 2.22 vs reference): harness
engineering closes the *reliability and speed* gaps but not the *comprehension* gap —
spoken-number corruption and shallow synthesis survive every technique probed (§7.1).
Remaining levers, in order of cost: (a) two-pass Enhance (extract facts → compose notes
from the facts — unmeasured, worth one bench variant); (b) retrieval-style Ask for very
long meetings (top-k relevant segments instead of full-transcript stuffing); (c) the
fine-tune/distill route — precedents exist (DeepSeek-R1-distill-Llama-8B; community
meeting-summary LoRAs like UDZH/deepseek-meeting-summary), so distilling sonnet-5
meeting-notes outputs into a llama3.1 LoRA published on HF is viable, but it's a separate
effort with dataset, eval, and licensing work — only justified if (a)+(b) leave the gap
unacceptable.

## 11. Shipped (2026-07-14): the §10 recommendation, proven as the release config

The harness adoption shipped behind the profile registry — `constrained_json`
(Polish/Extraction schemas + temperature 0; llama3.1 AND qwen3.5) and
`constrained_enhance` (Enhance schema; llama3.1 only — §10's qwen empty-array failure
makes it per-model by necessity). Unmeasured models (gemmas, cloud, anything user-typed)
resolve the DEFAULT profile and keep byte-identical requests, asserted in unit tests.

Proof runs of the exact shipped configuration (bench "default" variant = the app's
resolved profile), fresh runs after the change:

| Shipped config | Contracts E/P/X | Judge Enh/Ask | Latency E/P/X/Ask | Empty Asks |
|---|---|---|---|---|
| llama3.1 (default model), n=3 | **9/9 · 9/9 · 9/9** | **2.56** / 4.29 | 68/108/88/**11 s** | **0/45** |
| qwen3.5:4b, n=2 | 5/6 · **6/6** · **6/6** | **3.33** / 4.17 | 60/88/53/81 s | 4/30 |

Against the pre-change shipped behavior (§3.1/§10 baselines): llama3.1 contract passes
7/9 → 27/27, Enhance judge 1.67 → 2.56, polish latency 626 s (compat) → 108 s, Ask
36 s → 11 s, zero empty or fallback responses across 45 questions. qwen3.5's known
empty-Ask intermittent persists (4/30) and remains its blocking defect as a default.

Release verification: full workspace suite 233/233 green (the Defender-blocked unit-test
binary was recovered with a distinct `-C metadata` fingerprint), clippy `-D warnings`
clean, rustfmt clean, `tsc` clean, live num_ctx marker test green through the shipped
provider. No UI, settings, or provider-selection changes in this release.

## 12. Reproducing

```
# 1) reference outputs (needs the Anthropic key in env or the app keychain)
LLM_BENCH_MODELS="anthropic:claude-sonnet-5" \
  cargo test -p fly-app --test llm_bench -- --ignored --nocapture

# 2) candidates (pull models first: ollama pull qwen3.5:4b …)
LLM_BENCH_MODELS="ollama:llama3.1,ollama:qwen3.5:4b,ollama:gemma3:4b,ollama:gemma4:e4b" \
  cargo test -p fly-app --test llm_bench -- --ignored --nocapture

# 3) judge phase (sonnet-5 scores everything in target/llm-bench vs the reference)
LLM_BENCH_PHASE=judge \
  cargo test -p fly-app --test llm_bench -- --ignored --nocapture

# 4) prompt-profile experiments (rerun a candidate with a profile variant)
LLM_BENCH_MODELS="ollama:<best>" LLM_BENCH_VARIANT=simple LLM_BENCH_TASKS=enhance,extract \
  cargo test -p fly-app --test llm_bench -- --ignored --nocapture

# 5) thinking-model ceiling (forces ThinkingMode::Disabled on all tasks, "-nothink" slug)
LLM_BENCH_MODELS="ollama:qwen3.5:4b" LLM_BENCH_THINKING=disabled \
  cargo test -p fly-app --test llm_bench -- --ignored --nocapture
```

Adding a per-model prompt profile after a decision: follow the checklist in
`crates/fly-core/src/prompt_profile.rs`.
