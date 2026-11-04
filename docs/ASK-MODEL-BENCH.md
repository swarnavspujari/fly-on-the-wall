# Ask on real local models: the v2.5 row 3.5 bench (2026-11)

Spec: `docs/superpowers/specs/2026-10-27-ux-rollout-and-platform-design.md` §4.2
"Models", roadmap row 3.5. Until now the Ask harness (`crates/fly-ask`,
PR #111) had only been scored against scripted "perfect model" scripts. This is
its first run on real local models, through the app's own provider code, on the
evals' synthetic corpus (29 invented meetings; nothing here comes from a real
meeting).

## 1. Verdict

**Keep `llama3.1` as the default local model for 2.5.0.** No challenger beats it
on quality without costing something else, and the one place a model is clearly
better (the 9B class) needs more RAM and more time per Ask than a default can ask
of an 8 GB machine.

| RAM | Default | Why | Not measured here |
|---|---|---|---|
| 8 GB | `llama3.1` (tier S) | Works with the harness: 0 empty replies, 0 failures in 76 Asks. `qwen3.5:4b` is statistically tied on quality (§4.1) and faster, but answered more unanswerable questions (6 of 16 vs 4) and had one runner failure; `granite4.1:3b` is 3.6× faster but refuses 47% of answerable questions. | An 8 GB machine. Footprints below are from a 32 GB laptop; llama3.1 held 5.8 GB at an 8192 window, so on 8 GB `qwen3.5:4b` (3.6 GB) is the fallback if llama3.1 doesn't fit. |
| 16 GB | **Offer** `qwen3.5:9b` (tier M); never switch silently | Spoken numbers kept (2 sentences flagged for a number in 76 Asks vs 11 for llama3.1), 1 of 16 unanswerable questions answered (llama3.1: 4), grounded 65% vs 53% as scored and 93% vs 65% without the claim check's name rule. Costs ~8 s more per Ask (p50 32 s vs 24 s, both contended). | Whether 16 GB holds the 6.2 GB model plus the OS and an open meeting. |
| 32 GB | `qwen3.5:9b`, or `gemma4:12b-it-qat` | `gemma4:12b-it-qat` is the best on quality as scored (grounded 73%, no number ever corrupted) but 35% slower (p50 44 s) and 8 GB. The 35B / 26B models of the 32 GB tier were **not run**: they need 38.5 GB more disk (over the 30 GB cap) and, on a 4 GB GPU, would be CPU-bound. | `qwen3.6:35b`, `gemma4:26b-a4b`. |

Other findings a release should know:

* **Every retrieval Ask reloaded the model**, 12–22 s each on this machine. The
  provider sized `num_ctx` per request, and Ollama reloads when it changes, even
  by 4 tokens. Fixed in this PR (§3.1). This is the one change that should ship
  in 2.5.0.
* **No model fails a security gate.** 443 real-model replays (five models, quality
  plus red-team and guard scenarios): 0 executed proposals, 0 rendered links, 0
  keep-local off-device requests, 0 out-of-scope reads, 0 run-log free text, 0
  frame mismatches, 0 budget violations, citation validity 1.0 and
  claim-check pass rate 1.0. No model wrote a link or tried a proposal (§4.2).
* **No model meets every tier floor as scored**, and most of the gap is one rule:
  the claim check flags correct names that aren't in the cited line (a project
  name from the question, a customer name). With the name rule switched off in a
  throwaway local patch, `qwen3.5:9b` and `gemma4:12b-it-qat` meet every tier-M
  floor and `llama3.1` still misses two tier-S floors (§4.1). Whether to relax
  that rule is the user's eval decision.
* **The tool tier never used its tools.** In 152 tier-M Asks the models made 0
  tool calls; the nine tool definitions only cost ~4,000 prompt tokens per Ask
  (10.0k vs 5.6–6.6k, §4.5).
* **Ollama issues:** #18944 and #10976 are still real on 0.40.13; #18861 did not
  reproduce (§5). The provider's rules (never `format` + `tools`, always send
  `think`) stay.
* **Ollama pin:** Windows bumped 0.31.2 → 0.40.13, the version every number here
  was measured on. New installs only (§6). macOS stays on 0.31.2.

## 2. Setup

| | |
|---|---|
| Machine | Windows 11 Pro, i7-12700H (20 threads), 31.7 GB RAM, RTX 3050 Laptop GPU with 4 GB (2.8 GiB free to Ollama; the desktop holds the rest). No model fit fully on the GPU (24–75% did). |
| Ollama | 0.40.13, the user-installed server on defaults (flash attention off, `NUM_PARALLEL` 1, keep-alive 5 min, CUDA). It self-updated to 0.40.14 on 2026-11-03 22:21, after every recording below; one later pass (§4.6) ran on 0.40.14. |
| Code | `claude/model-bench-35-903802`, from main `e4a6a4a`, plus this PR |
| Provider | the app's `build_provider_with_reach` on a throwaway data dir, Ollama native `/api/chat`, streaming (the harness always streams), `think:false` |
| Corpus | `evals/fixtures` (synthetic). Retrieval used the CI embedder (`hashed-bow-v1`), so a replay's request fingerprint equals what CI computes: no replay was stale |
| Scenarios | 60 answerable + 16 unanswerable quality scenarios, evenly spaced over the gold set and the same for every model; plus the 4 on-device tier-S red-team and 7 guard scenarios for every model (the M models forced to tier S) and the 4 tier-C red-team scenarios for the two M models |
| Models | `llama3.1` (4.9 GB), `qwen3.5:4b` (3.3 GB), `granite4.1:3b` (2.1 GB), `qwen3.5:9b` (6.6 GB), `gemma4:12b-it-qat` (7.2 GB). The four pulls total **19.2 GB**; `llama3.1` was already installed |
| Tiers | measured by the app's `ask_tier`: the three small models S; `qwen3.5:9b` and `gemma4:12b-it-qat` passed the tool smoke test (6.2 s, 9.5 s) and ran as M |
| Build | the bench is a debug build; harness work (retrieval, validation) was ~0.6 s per Ask, small next to the model |
| **Contention** | Other sessions were compiling on this machine for most of the run. `bench.json` records the non-Ollama CPU and whether builds were running: `granite4.1:3b` 50% other CPU / builds in 100% of samples, `llama3.1` 49% / 96%, `qwen3.5:4b` 32% / 77%, `qwen3.5:9b` 21% / 52%, `gemma4:12b-it-qat` 11% / 0%. **Every latency below is contended**, the small models more than the large ones, so the true S-vs-M gap is wider than shown. Quality does not depend on it. A re-time on a quiet machine was not possible (it wasn't quiet before or after); §9 has the command. |

## 3. What the bench found that changed the code

### 3.1 Each Ask reloaded the model (fixed: `num_ctx` on a 4096 grid)

The provider asks Ollama for a context window sized to the prompt (`chars/3` +
the response budget). The Ask prefetch changes by a few hundred characters from
question to question, so each Ask asked for a different `num_ctx`, and Ollama
reloads the model whenever a request's `num_ctx` differs from the loaded
runner's.

* Three consecutive `llama3.1` Asks (before the fix): `num_ctx` 7598 / 7530 /
  7587, `load_duration` 22.7 s / 21.8 s / 22.6 s, total 58.8 / 51.3 / 72.3 s.
* A plain request, `num_ctx` 4096 twice then 4100 twice then 4096: load 15.5 s
  (cold), **30 ms**, **11.9 s**, 28 ms, **12.3 s**. A 4-token change is a full
  load.
* The estimate now rounds up to a multiple of 4096. After the fix, the same
  Asks: load 14.7 s (cold) then 40 / 36 / 28 ms; total 37.8 / 24.7 / 38.4 /
  23.1 s on the same contended machine.
* The window only grows (at most 4095 tokens), so the front-truncation
  protection is unchanged. The legacy-body golden changes in `num_ctx` values
  only (4622/4623 → 8192, 8719 → 12288, 16910 → 20480).
* Residual: 2 of the 75 warm `llama3.1` Asks (and 2 of `granite4.1:3b`'s) had a
  short prompt that fit the 4096 window; the only warm reloads (2 and 3) sit next
  to them, one into the small window and one back out. The Asks in a session
  mostly share 8192 (tier S) or 12288 (tier M); a floor of 8192 would remove
  the rest, at the cost of a bigger window for Enhance, Polish and Extraction.
  Not done here.

### 3.2 The claim check's name rule decides the scores

Every model's non-sourced sentences are mostly flagged for **names** (20–49
sentences per model vs 0–23 for numbers), and in 8–21 Asks per model the name
was the only flag. The flagged names are correct: a project or customer name
that came from the question or the meeting title but isn't in the cited line.
The name rule requires a name to appear in the cited text or the speaker label. The previous
harness session recorded the same thing for `llama3.1` (26 of 31 non-sourced
sentences). §4.1 shows the scores with and without the rule.

### 3.3 The scorer panicked on any real reply with an em dash

`Db::normalize_ids` sliced a 36-byte window that could end inside a character.
Fixed in the runner (`get` instead of an index) with a test; no metric moves.

### 3.4 One runner failure becomes a failed Ask

One `qwen3.5:4b` Ask got an HTTP 500 (the runner didn't answer its health
check under load). The provider retried without `num_ctx`, and the harness
treats that as a truncation risk and ended the Ask as `model_failed(Truncated)`,
as designed. A transient 500 could be retried once at the same window before
the context is dropped; not done here.

## 4. Results

All five models ran the same 76 quality Asks. "Warm" excludes the first Ask
after an unload; **cold is one Ask per model (n = 1)**, so there is no cold
p50 / p95.

### 4.1 Quality against the tier floors (`evals/thresholds.toml`)

As scored (strict claim check):

| group | grounded | unsourced sentences | correct refusal | false refusal | verdict |
|---|---|---|---|---|---|
| `llama3.1` @S | 53% | 46% | 75% | 5% | misses unsourced, grounded, correct refusal |
| `qwen3.5:4b` @S | 57% | 38% | 63% | 2% | misses unsourced, grounded, correct refusal |
| `granite4.1:3b` @S | 13% | 85% | 100% | 47% | misses unsourced, grounded, false refusal |
| `qwen3.5:9b` @M | 65% | 33% | 94% | 2% | misses unsourced, grounded |
| `gemma4:12b-it-qat` @M | 73% | 27% | 94% | 3% | misses unsourced, grounded |

Floors: S grounded ≥ 70%, unsourced ≤ 30%, correct refusal ≥ 85%, false refusal
≤ 15%; M grounded ≥ 75%, unsourced ≤ 20%, correct refusal ≥ 90%, false refusal ≤
10%. Request shortfall (must-cite lines never sent) is 0 for every model. Every
group is gated: 60 answerable, 16 unanswerable.

What-if, **a throwaway local patch that turns the name rule off in both the
harness and the scorer** (reverted, not committed, only to size the effect):

| group | grounded | unsourced sentences | correct refusal | false refusal | verdict |
|---|---|---|---|---|---|
| `llama3.1` @S | 65% | 27% | 75% | 5% | misses grounded, correct refusal |
| `qwen3.5:4b` @S | 72% | 23% | 56% | 2% | misses correct refusal |
| `granite4.1:3b` @S | 17% | 70% | 100% | 47% | misses three |
| `qwen3.5:9b` @M | 93% | 9% | 94% | 2% | meets every floor |
| `gemma4:12b-it-qat` @M | 92% | 8% | 94% | 3% | meets every floor |

Why the small models lose, from the run logs (sentences over 76 Asks):

| model | sentences | sourced | flagged: number | flagged: name | no valid citation | invalid citations | Asks flagged for names only |
|---|---|---|---|---|---|---|---|
| `llama3.1` | 98 | 54 | 11 | 35 | 9 | 0 | 17 |
| `qwen3.5:4b` | 181 | 110 | 23 | 49 | 15 | 14 | 21 |
| `granite4.1:3b` | 33 | 9 | 13 | 20 | 0 | 0 | 8 |
| `qwen3.5:9b` | 88 | 59 | 2 | 24 | 5 | 0 | 20 |
| `gemma4:12b-it-qat` | 90 | 67 | 0 | 21 | 3 | 3 | 16 |

The July finding holds: small local models turn a spoken "eighty four hundred
dollars" into `$84,000`, and the claim check catches it. `qwen3.5:4b` writes
twice as much as `llama3.1` (126 vs 55 output tokens) and cites lines it wasn't
shown 14 times. Both small models answer unanswerable questions (`llama3.1` 4 of
16, `qwen3.5:4b` 6 of 16); both M models answer 1.

Sample sizes matter: 16 unanswerable questions means 4 vs 6 wrong is within
noise, and 60 answerable ones means ±6 points on a rate. The 9B-class models'
lead over the 8B class is larger than that; the `qwen3.5:4b` vs `llama3.1`
difference is not.

### 4.2 Security and guard gates

Suite-wide over all 443 replays (the five models' 76 quality Asks, 7 guard and
4 red-team scenarios each, and 4 tier-C red-team scenarios for the two M
models): citation validity 1.0, claim-check pass rate 1.0, executed proposals 0,
rendered links 0, keep-local off-device requests 0, out-of-scope reads 0,
run-log free text 0, stream-frame mismatches 0, tier-budget violations 0, stale
replays 0, errored scenarios 0. Per model the same counts are all zero
(`scripts/ask-bench-report.mjs`, "Guards per model"). In the run logs no model
wrote a link or image (the harness removed 0) and none produced a proposal.

Limit: 15 red-team Asks per M model and 11 per S model. They test whether the
injected speech in the corpus steers a real model, not an adaptive attacker.

### 4.3 Latency (per Ask, seconds, contended; see §2)

| model | tier | warm first token p50 / p95 | warm total p50 / p95 | cold first Ask (n = 1) | model load in it |
|---|---|---|---|---|---|
| `granite4.1:3b` | S | 5.9 / 6.8 | 6.7 / 9.3 | 14.6 | 6.6 |
| `qwen3.5:4b` | S | 7.9 / 12.9 | 19.4 / 33.2 | 37.5 | 12.4 |
| `llama3.1` | S | 12.3 / 32.3 | 24.4 / 43.2 | 33.2 | 11.3 |
| `qwen3.5:9b` | M | 20.3 / 27.6 | 32.4 / 65.2 | 50.1 | 14.0 |
| `gemma4:12b-it-qat` | M | 29.3 / 37.1 | 43.7 / 91.3 | 60.3 | 12.9 |

First token is measured by the harness (retrieval plus prefill; Ollama sends
nothing until the prompt is read). Model-only time is within 1–2 s of the total
for every model.

### 4.4 Throughput, context and memory

| model | prompt tokens per Ask (p50) | prefill tok/s | decode tok/s | `num_ctx` sent (min / p50 / max) | model size (GPU / RAM) | on GPU | peak VRAM of 4 GB |
|---|---|---|---|---|---|---|---|
| `granite4.1:3b` | 5,603 | 1,261 | 17.8 | 4096 / 8192 / 8192 | 2.9 GB (2.2 / 0.7) | 75% | 3,865 MB |
| `qwen3.5:4b` | 6,620 | 993 | 10.6 | 4096 / 8192 / 8192 | 3.6 GB (1.8 / 1.9) | 48% | 3,465 MB |
| `llama3.1` | 5,616 | 536 | 4.6 | 4096 / 8192 / 8192 | 5.8 GB (2.1 / 3.7) | 36% | 3,856 MB |
| `qwen3.5:9b` | 10,112 | 513 | 4.8 | 8192 / 12288 / 16384 | 6.2 GB (1.5 / 4.8) | 24% | 3,765 MB |
| `gemma4:12b-it-qat` | 9,945 | 354 | 3.8 | 8192 / 12288 / 16384 | 8.0 GB (2.0 / 6.0) | 25% | 3,280 MB |

Prefill and decode rates are Ollama's own counters (`prompt_eval_*`,
`eval_*`) summed over every call; calls that evaluated fewer than 64 new
tokens (cache hits) are left out of prefill. The VRAM baseline with nothing
loaded was 0.7–1.0 GB (other apps). Context fallbacks: 0 (the one 500 is §3.4).
System RAM was nearly full throughout (27.5–31.7 GB in use at the peak, mostly other
sessions), so the sizes above, not the system figure, are the per-model footprint.

### 4.5 What the provider sent (the wire)

From a logging pass-through between the provider and Ollama, over every call
(75–82 per model; a refused question never reaches the model): `format` sent 0 times, `format` with tools 0, `think`
omitted 0, `think:false` on every call, HTTP errors 1 (§3.4), empty replies 0,
thinking characters 0, done reason `stop` on every call but the failed one.

Tier M offers nine tools. The prompt on the first call is ~10k tokens against
5.6–6.6k at tier S: the tool definitions cost ~4k tokens and, at 350–510
prefill tok/s, 8–11 s per Ask. In 152 tier-M Asks the models made 0
model-chosen tool calls; the extra calls were retries (7 Asks for
`gemma4:12b-it-qat`, 5 for `qwen3.5:9b`).

### 4.6 Is tier M worth it? `qwen3.5:9b` forced to tier S

One more pass ran `qwen3.5:9b` at tier S (same harness, no tools) on 30
answerable + 8 unanswerable Asks. Its even spacing differs from the M run's, so
the two are not paired. It ran on Ollama 0.40.14.

| | @M (60 + 16 Asks) | @S (30 + 8 Asks) |
|---|---|---|
| prompt tokens per Ask (p50) | 10,112 | 6,652 |
| grounded | 65% | 70% |
| unsourced sentences | 33% | 31% |
| correct refusal | 94% (15 of 16) | 88% (7 of 8) |
| false refusal | 2% | 0% |
| model-chosen tool calls | 0 | not offered |

At tier S the group meets every tier-S floor except unsourced sentences (30.9%
against ≤ 30%). With 30 and 8 Asks the differences are inside the noise, so the
honest reading is: **the tool loop bought nothing for this model on this corpus**,
and it costs a third of the prompt (10.1k vs 6.7k tokens). The latency of this
pass is not comparable and isn't reported as a result: another process held
3.7 GB of the 4 GB of VRAM the whole time (3,764 MB in use before the model
loaded), so the model ran almost entirely on the CPU (Ask p50 51 s, other CPU
54%, builds in every sample).

## 5. Known Ollama issues, re-tested on 0.40.13

Raw `/api/chat` calls (no provider), 3 runs each, `temperature` 0, 400 tokens;
the model gets one tool (`lookup_codename`) and a question. The full results
are in `ollama-issues.json` beside the replays.

| issue | what was run | `gemma4:12b-it-qat` | `qwen3.5:9b` | `qwen3.5:4b` | `granite4.1:3b` | `llama3.1` |
|---|---|---|---|---|---|---|
| #18944 `format` + tools | tools + a JSON `format`, `think:false` | **tool call dropped** (3/3; answers `{"codename":"ORION"}`, made up) | **dropped** (3/3) | **dropped** (3/3) | **dropped** (3/3) | tool still called (3/3) |
| #10976 `think` omitted | tools, no `think` field | thinking turned on (3/3), tool still called | thinking on (3/3), tool called | thinking on (3/3), tool called | no thinking | no thinking |
| #18861 empty after a tool result | two-hop dialog, `think:false` on both hops | **answered 3/3** (not reproduced) | answered 3/3 | n/a (never called the tool with `think:false`) | answered 3/3 | answered 3/3 |

What this means for the code:

* **#18944 is still open and real** for the four newer models (the issue's own
  thread dates the regression to 0.34.4). The provider refusing `format` with
  tools (`validate_ollama`) stays.
* **#10976:** an omitted `think` turns thinking on for gemma4 and both qwen3.5
  sizes. With a 400-token budget the tool call still arrived; the failure the
  issue describes is the budget being spent thinking. The provider always sends
  `think`, and the wire log confirms it did on every call.
* **#18861 did not reproduce** on 0.40.13, with `gemma4:12b-it-qat` or the others,
  and the tool smoke test passed for it (and for `qwen3.5:9b`). The release notes
  show a fix for Gemma 4 tool-response continuations in 0.32.1, after the old pin.
  The issue is still open upstream; the tier gate's smoke test is what protects
  an app on a different runtime.
* **`qwen3.5:4b` doesn't call tools with `think:false`** (0/3), which is why it
  stays tier S. `think:true` on a model that doesn't think (`granite4.1:3b`,
  `llama3.1`) is an HTTP 400, which the app never sends to a local model.

## 6. The Ollama pin

| | Windows | macOS |
|---|---|---|
| Pinned | v0.31.2 → **v0.40.13** (this PR) | v0.31.2, unchanged |
| Asset | `ollama-windows-amd64.zip`, 1,601,969,847 bytes, sha256 `7afa3862…cc19` (the GitHub release digest) | `ollama-darwin.tgz` v0.40.13 is 186,689,993 bytes, sha256 `bae29252232b9e1358cf5801a7d65a07442af18fb5467cb68c7fc3711f59c511` |
| Layout | root holds `ollama.exe` + `lib/`, same as 0.31.2 (read from the zip's central directory by range request; nothing downloaded) | not checked |

Why bump: the numbers here were measured on 0.40.13, and a managed install on
0.31.2 would run these models on a runtime nobody measured (the release notes
between them include Gemma 4 tool-continuation fixes in 0.32.1, structured
output changes in 0.34.4, and `/api/show` thinking controls in 0.34.3). The
0.31.2 zip itself was not run: downloading it wasn't covered by the brief.

Two limits to know:

* **Only new installs get it.** The managed layout (`bin/ollama/ollama.exe`) isn't
  versioned, so an existing install passes the probe and keeps its binary. The
  other tools put the version in the path to avoid this. Upgrading existing
  installs needs the model manager v2 flow or a versioned layout (a follow-up).
* **macOS is not bumped.** From 0.40.0, Apple Silicon runs supported models on
  MLX by default, which nobody has smoke-tested on the Mac. The three-line change
  is above, ready when someone has.

## 7. Recommendation

See §1 for the table. In order:

1. **Ship the `num_ctx` grid in 2.5.0** (this PR). Without it every retrieval Ask
   spends 12–22 s reloading the model on this class of machine.
2. **Keep `llama3.1` as the default.** The rule was "keep it unless a challenger
   beats it on quality without breaking latency or empty-answer rate":
   `qwen3.5:4b` is faster (p50 19 s vs 24 s) and ahead on sourcing (38% vs 46%
   unsourced) but behind on refusals and had a runner failure; both are inside
   the noise at n = 60 / 16. `granite4.1:3b` breaks false refusals (47%).
   Neither the 9B nor the 12B model is a default for an 8 GB machine.
3. **Offer `qwen3.5:9b` on machines with 16 GB or more** ("A better local model is
   available · 6.6 GB", spec §4 model upgrades): it is the best quality per
   second, and it keeps spoken numbers. Run at the tier-S budget (no tools) it gave
   the same quality on a third fewer prompt tokens (§4.6), so the tier-M gate isn't
   what makes it work. `gemma4:12b-it-qat` buys 7 points of
   grounded for 35% more latency and 1.8 GB more.
4. **Follow-ups, none blocking 2.5.0:** offer tools at tier M only when the
   prefetch doesn't answer (saves ~4k tokens, ~8–11 s per Ask); retry a 500 once
   at the same window; a floor on `num_ctx` for Ask; a versioned Ollama layout
   plus the macOS bump; re-time on a quiet machine; the 32 GB tier.

## 8. Proposed eval changes (these need the user's OK, CLAUDE.md)

Nothing under `evals/` changed except the runner bug fix (§3.3: no metric moves).
No replay is committed. Proposals:

1. **Committing replays.** The replays are in
   `C:\Users\swarn\Claude Code Projects\fotw-design-2026-10\replays\<model>\`
   (76 quality Asks per model, plus the security ones in
   `replays-security\` and `replays-security-C\`; model output on synthetic data
   only). Committing a model's replays turns its group (`replay:<model>@<tier>`)
   into a gate, because every group of ≥ 30 answerable and ≥ 8 unanswerable
   scenarios is gated. As scored, **no group meets its floors**, so committing any
   of them while `ask_contract` is `required` would fail CI. Committed replays
   also go stale, and fail `stale_replays`, whenever the harness's requests
   change (any prompt or prefetch edit), and re-recording takes hours of GPU time.
2. **`ask_contract` from `pending` to `required`.** It can go `required` today
   with no replays: the `auto` groups and every scripted guard, red-team and
   robustness scenario pass every gate (verified again in this run), and 443
   real-model replays add no failure to any suite-wide gate. If real-model groups
   are wanted in CI later, the floors first have to be decided against the name
   rule (§3.2), because it, not model capability, is what the M floors measure
   today.
3. **The name rule.** Decide whether a name present in the question, the meeting
   title or the attendees counts as supported. With the rule off, the two M models
   meet every floor; with it on, none does.
4. **Report, don't gate, real models:** keep replays out of the repo and
   publish this bench's output per release, with `ask-bench-report.mjs`.

## 9. Limits and how to reproduce

Not done: the 32 GB tier; an 8 GB or 16 GB machine (one 4 GB-VRAM laptop,
31.7 GB); cold p50 / p95 (n = 1); a quiet-machine re-time; the 0.31.2 runtime;
macOS. `granite4.1:3b` was tried on its default settings only.

```bash
# Pull (sizes: 3.3 / 2.1 / 6.6 / 7.2 GB)
ollama pull qwen3.5:4b && ollama pull granite4.1:3b && ollama pull qwen3.5:9b && ollama pull gemma4:12b-it-qat

# Record one model: 60 answerable + 16 unanswerable, evenly spaced, replays outside the repo
FOTW_ASK_LIVE_MODELS=llama3.1 FOTW_ASK_LIVE_SAMPLE=60,16 FOTW_ASK_LIVE_OUT=<dir outside the repo> \
  cargo test -p fly-app --features ask-live --test ask_live -- --ignored --nocapture
# red-team + guard scenarios at tier S, then tier C (tool models only)
FOTW_ASK_LIVE_KINDS=red_team,guard FOTW_ASK_LIVE_TIER=S ...   FOTW_ASK_LIVE_KINDS=red_team FOTW_ASK_LIVE_TIER=C ...
# wait for a quiet machine before each Ask, and make every Ask cold
FOTW_ASK_LIVE_QUIET_MAX_S=600 FOTW_ASK_LIVE_COLD_EVERY=1 ...

# Ollama issues (raw API)
FOTW_OLLAMA_ISSUES_MODELS=gemma4:12b-it-qat,qwen3.5:9b FOTW_OLLAMA_ISSUES_RUNS=3 \
  cargo test -p fly-app --test ollama_issues -- --ignored --nocapture

# Score: copy a model's replays into evals/suites/ask_contract/replays/<model>/ (locally, never commit), then
cargo run -p fly-evals -- --suite ask_contract
node scripts/ask-bench-report.mjs <out dir> --scores target/evals/ask_contract.json
```

The bench test, its wire logger and its resource sampler are
`src-tauri/tests/ask_live.rs` and `src-tauri/tests/ask_live_support/`; none of it
builds without the `ask-live` feature, and CI only compiles it.
