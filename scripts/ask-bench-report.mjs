#!/usr/bin/env node
// Turns an Ask bench run into Markdown tables.
//
//   node scripts/ask-bench-report.mjs <out dir> [--scores <ask_contract.json>]
//
// <out dir> is what src-tauri/tests/ask_live.rs wrote (FOTW_ASK_LIVE_OUT):
// `_bench/<model>/bench.json` + `rows.jsonl` for timing and resources.
// --scores is the evals runner's result for the same replays
// (`target/evals/ask_contract.json`, after copying them into
// evals/suites/ask_contract/replays/ for a local run): quality per group
// and the guard counts per model. Both parts are optional.
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";

const args = process.argv.slice(2);
const out = args.find((a) => !a.startsWith("--") && args[args.indexOf(a) - 1] !== "--scores");
const scoresPath = args.includes("--scores") ? args[args.indexOf("--scores") + 1] : null;
if (!out) {
  console.error("usage: ask-bench-report.mjs <out dir> [--scores <ask_contract.json>]");
  process.exit(2);
}

const read = (p) => JSON.parse(readFileSync(p, "utf8"));
const num = (v, d = 1) => (v === null || v === undefined || Number.isNaN(v) ? "n/a" : Number(v).toFixed(d));
const sec = (ms) => (ms === null || ms === undefined ? "n/a" : (ms / 1000).toFixed(1));
const pct = (v) => (v === null || v === undefined ? "n/a" : `${(v * 100).toFixed(0)}%`);

// Tier gates from evals/thresholds.toml: `name = { max = x }` per tier.
function tierGates() {
  const text = readFileSync(resolve(import.meta.dirname, "..", "evals", "thresholds.toml"), "utf8");
  const gates = {};
  let tier = null;
  for (const line of text.split(/\r?\n/)) {
    const h = line.match(/^\[ask_contract\.tiers\.([SMC])\.gates\]/);
    if (h) { tier = h[1]; gates[tier] = {}; continue; }
    if (line.startsWith("[")) { tier = null; continue; }
    const g = line.match(/^(\w+)\s*=\s*\{\s*(min|max)\s*=\s*([\d.]+)\s*\}/);
    if (tier && g) gates[tier][g[1]] = { [g[2]]: Number(g[3]) };
  }
  return gates;
}

const benchDir = join(out, "_bench");
// A model still recording has rows but no bench.json yet.
const models = existsSync(benchDir)
  ? readdirSync(benchDir).filter((m) => existsSync(join(benchDir, m, "bench.json"))).sort()
  : [];
const bench = Object.fromEntries(models.map((m) => [m, read(join(benchDir, m, "bench.json"))]));

const readRows = (m) =>
  readFileSync(join(benchDir, m, "rows.jsonl"), "utf8").trim().split(/\r?\n/).filter(Boolean).map((l) => JSON.parse(l));
const lines = [];
const h = (t) => lines.push("", t, "");

// ---- latency and resources ------------------------------------------------
if (models.length) {
  h("### Latency (per Ask, seconds)");
  lines.push(
    "| model | tier | n | warm first token p50 / p95 | warm total p50 / p95 | warm model-only p50 | cold first Ask (total) | model load (cold) |",
    "|---|---|---|---|---|---|---|---|",
  );
  for (const m of models) {
    const b = bench[m], r = b.results;
    const w = r.warm, c = r.cold;
    lines.push(
      `| ${b.model} | ${b.tier} | ${w.total_ms.n}+${c.total_ms.n} | ${sec(w.first_token_ms.p50)} / ${sec(w.first_token_ms.p95)} | ${sec(w.total_ms.p50)} / ${sec(w.total_ms.p95)} | ${sec(w.model_ms.p50)} | ${sec(c.total_ms.p50)} | ${sec(c.model_load_ms?.[0])} |`,
    );
  }
  h("### Throughput, context and memory");
  lines.push(
    "| model | prompt tokens per Ask (p50, first call) | model-chosen tool calls | prefill tok/s | decode tok/s | `num_ctx` sent (min / p50 / max) | ctx fallbacks | 5xx | model size (on GPU / in RAM) | fully on GPU | Ollama working sets, peak (shared pages count per process) | peak VRAM (baseline) | machine CPU avg (other / builds seen) |",
    "|---|---|---|---|---|---|---|---|---|---|---|---|---|",
  );
  for (const m of models) {
    const b = bench[m], r = b.results, res = b.resources, wi = r.wire;
    const rws = readRows(m);
    const first = rws.map((x) => x.wires?.[0]?.done?.prompt_eval_count).filter((x) => x).sort((x, y) => x - y);
    const ptok = first.length ? first[Math.floor(first.length / 2)] : null;
    const tcalls = rws.reduce((a, x) => a + (x.wires ?? []).reduce((c, w) => c + (w.tool_calls ?? 0), 0), 0);
    lines.push(
      `| ${b.model} | ${ptok ?? "n/a"} | ${tcalls} | ${num(r.rates.prefill_tokens_per_s, 0)} | ${num(r.rates.decode_tokens_per_s)} | ${wi.num_ctx.min} / ${wi.num_ctx.p50} / ${wi.num_ctx.max} | ${r.context_fallback_asks} | ${wi.http_5xx} | ${num(res.model_size_mb / 1024, 1)} GB (${num(res.model_size_vram_mb / 1024, 1)} / ${num((res.model_size_mb - res.model_size_vram_mb) / 1024, 1)} GB) | ${res.model_fully_on_gpu === null ? "n/a" : res.model_fully_on_gpu ? "yes" : `no (${pct(res.model_gpu_fraction_min)})`} | ${num(res.peak_ollama_rss_mb / 1024, 1)} GB | ${res.vram_peak_used_mb ?? "n/a"} / ${res.vram_total_mb ?? "n/a"} MB (${res.vram_baseline_used_mb ?? "n/a"}) | ${num(res.machine_cpu_avg_pct)}% (${num(res.other_cpu_avg_pct)}% / ${num(res.build_processes_seen_pct, 0)}%) |`,
    );
  }
  h("### What the provider sent");
  lines.push(
    "| model | chat calls | calls with tools | `format` sent | `format` + tools | `think` omitted | `think:false` | HTTP errors | empty replies | thinking chars | done reasons |",
    "|---|---|---|---|---|---|---|---|---|---|---|",
  );
  for (const m of models) {
    const b = bench[m], wi = b.results.wire;
    lines.push(
      `| ${b.model} | ${wi.chat_calls} | ${wi.calls_with_tools} | ${wi.format_sent} | ${wi.format_with_tools} | ${wi.think_omitted} | ${wi.think_false} | ${wi.http_errors} | ${wi.empty_replies} | ${wi.thinking_chars_total} | ${Object.entries(wi.done_reasons).map(([k, v]) => `${k}:${v}`).join(", ")} |`,
    );
  }
  h("### Why sentences were not shown as sourced (answerable + unanswerable Asks, from the run logs)");
  lines.push(
    "| model | Asks | sentences | sourced | flagged: number | flagged: date | flagged: name | flagged: quote | no valid citation | invalid citations | retries | Asks flagged for names only |",
    "|---|---|---|---|---|---|---|---|---|---|---|---|",
  );
  for (const m of models) {
    const rows = readFileSync(join(benchDir, m, "rows.jsonl"), "utf8").trim().split(/\r?\n/).filter(Boolean).map((l) => JSON.parse(l));
    const t = { asks: 0, sentences: 0, sourced: 0, n: 0, d: 0, nm: 0, q: 0, unsourced: 0, invalid: 0, retries: 0, nameOnly: 0 };
    for (const r of rows) {
      const v = r.run_log?.validation;
      if (!v) continue;
      t.asks++; t.sentences += v.sentences; t.sourced += v.sourced; t.n += v.claim_numbers; t.d += v.claim_dates;
      t.nm += v.claim_names; t.q += v.claim_quotes; t.unsourced += v.unsourced; t.invalid += v.invalid_citations;
      t.retries += r.run_log.retries?.length ?? 0;
      if (v.claim_names > 0 && !v.claim_numbers && !v.claim_dates && !v.claim_quotes) t.nameOnly++;
    }
    lines.push(`| ${bench[m].model} | ${t.asks} | ${t.sentences} | ${t.sourced} | ${t.n} | ${t.d} | ${t.nm} | ${t.q} | ${t.unsourced} | ${t.invalid} | ${t.retries} | ${t.nameOnly} |`);
  }
  lines.push("", "A sentence can be flagged for more than one claim kind, so the flagged columns can sum past the sentence count.");
  h("### Outcomes (as the harness reported them)");
  lines.push("| model | tier | tool smoke | outcomes | timed out |", "|---|---|---|---|---|");
  for (const m of models) {
    const b = bench[m];
    lines.push(
      `| ${b.model} | ${b.tier} (measured ${b.measured_tier}) | ${b.tool_smoke ? `${b.tool_smoke.result} (${sec(b.tool_smoke.ms)} s)` : "not run (≤ 8.5B or no tools)"} | ${Object.entries(b.results.by_status).map(([k, v]) => `${k}:${v}`).join(", ")} | ${b.results.timed_out} |`,
    );
  }
}

// ---- quality and guards from the scorer --------------------------------------
if (scoresPath) {
  const s = read(scoresPath);
  const gates = tierGates();
  const metrics = s.metrics ?? {};
  const groups = s.info?.groups ?? {};
  h("### Quality per group (tier floors from `evals/thresholds.toml`)");
  lines.push(
    "| group | answerable | unanswerable | gated | grounded | unsourced sentences | correct refusal | false refusal | model failed | request shortfall | verdict |",
    "|---|---|---|---|---|---|---|---|---|---|---|",
  );
  for (const [g, info] of Object.entries(groups).sort()) {
    if (!g.startsWith("replay:")) continue;
    const mm = (k) => metrics[`${g}.${k}`];
    const tg = gates[info.tier] ?? {};
    const fails = Object.entries(tg).filter(([k, gate]) => {
      const v = mm(k);
      return v === undefined || ("min" in gate ? v < gate.min : v > gate.max);
    }).map(([k]) => k);
    lines.push(
      `| ${g} | ${info.answerable} | ${info.unanswerable} | ${info.gated ? "yes" : "no (below floor)"} | ${pct(mm("grounded_answer_rate"))} | ${pct(mm("unsourced_sentence_rate"))} | ${pct(mm("correct_refusal_rate"))} | ${pct(mm("false_refusal_rate"))} | ${pct(mm("model_failed_rate"))} | ${num(mm("must_cite_request_shortfall"), 3)} | ${fails.length ? `**misses ${fails.join(", ")}**` : "meets every floor"} |`,
    );
  }
  lines.push("", "Floors by tier: " + Object.entries(gates).map(([t, g]) => `${t}: ` + Object.entries(g).map(([k, v]) => `${k} ${"min" in v ? "≥" : "≤"} ${v.min ?? v.max}`).join(", ")).join("; ") + ".");

  // Guards per model, from the scenario rows (every replay, any kind).
  const perModel = {};
  for (const row of s.info?.scenarios ?? []) {
    if (!row.id.startsWith("replay:")) continue;
    const rest = row.id.slice("replay:".length);
    const model = rest.slice(0, rest.lastIndexOf(":"));
    const t = (perModel[model] ??= { scenarios: 0, sourced: 0, valid: 0, claims: 0, claim_fail: 0, executed: 0, links: 0, leaks: 0, frames: 0, oos: 0, free_text: 0, budget: 0, stale: 0, errored: 0 });
    t.scenarios++;
    t.sourced += row.sourced_citations; t.valid += row.valid_sourced_citations;
    t.claims += row.claims_checked; t.claim_fail += row.claim_failures;
    t.executed += row.executed; t.links += row.links; t.leaks += row.off_device_leaks;
    t.frames += row.frame_mismatch ? 1 : 0; t.oos += row.out_of_scope_segments > 0 ? 1 : 0;
    t.free_text += row.log_free_text; t.budget += row.budget_violations.length ? 1 : 0;
    t.stale += row.stale ? 1 : 0; t.errored += row.errored ? 1 : 0;
  }
  h("### Guards per model (security gates are the zero columns)");
  lines.push(
    "| model | scenarios | citation validity | claim check | executed proposals | rendered links | keep-local off-device | frame mismatches | out-of-scope reads | run-log free text | budget violations | stale | errored |",
    "|---|---|---|---|---|---|---|---|---|---|---|---|---|",
  );
  for (const [m, t] of Object.entries(perModel).sort()) {
    lines.push(
      `| ${m} | ${t.scenarios} | ${t.valid}/${t.sourced} | ${t.claims - t.claim_fail}/${t.claims} | ${t.executed} | ${t.links} | ${t.leaks} | ${t.frames} | ${t.oos} | ${t.free_text} | ${t.budget} | ${t.stale} | ${t.errored} |`,
    );
  }
}

console.log(lines.join("\n").trim());
