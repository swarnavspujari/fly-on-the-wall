# The Ask harness (`fly-ask`)

> Layer: crate. Added by spec v2.5 PR 3.3 (up to date with main `b191291`). The streaming Ask UI that renders its sentences, sources, frames and proposals is PR 3.4.

## Purpose
Turns a question into an answer the user can trust because code, not the model, checks it (spec §4.2, §4.2a). One `ask()` call:

1. **Scope**: this meeting, a note, a folder (with subfolders), a person, or everything. Visibility comes from the caller's `on_device` (`build_provider_with_reach`), never from `provider.is_local()`.
2. **Route**: a deterministic self-query router pulls dates ("last week", "Friday's meeting"), people, a folder (`@Clients`) and list intents ("open action items") from the question, validated against known values. Dates are soft filters.
3. **Prefetch** through `fly_context::Context::search_passages_scored` only: the plain search's top 8 transcript passages always go in whole; ranks 9-20 and the router, follow-up, subfolder, wide-scope 4-per-meeting and (one long meeting) single-term searches fill the rest, clipped around their best line. Plus a memory briefing when the question names a known person or recurring meeting (fly-context `memory_prefetch`, facts with their verbatim source lines), else extracted items with their source lines, and the user's notes (demoted: they can't be cited). A short meeting is read whole.
4. **Truncation**: a server that read less of the prompt than was sent (round 1: under 70% of it; later rounds: under 70% of what's new since the previous request, which Ollama can't have cached), or Ollama's default-context fallback, ends the Ask as `model_failed(Truncated)`.
4a. **Floor**: a wide-scope question whose topic words aren't in what was found is "not found" with no model call. An unscoped question that fly-context also measured semantically (an embedder with a measured floor, 4+ content words: `PassageScores::similarity_floor`) must clear that cosine floor as well (`top_similarity`).
5. **Synthesize**, streamed as sanitized frames (no markers, links or reasoning tags while streaming; the final frame, `turn` 0, is the validated answer). Tier S writes from the prefetch alone; tiers M and C may call fly-context's nine tools (six read tools and the three memory tools, all scope-clamped) and the two proposal tools, through fly-llm's tool loop.
6. **Validate**: a citation counts only if the model was shown that line — its marker and its whole text in the same request (`Evidence::add_shown`). `[seg:a, b]` lists, a trailing "Sources: [seg:…]" line and a bare `[id]` of a shown line all cite; sentences are sourced, unsourced or flagged; numbers (spoken forms normalized), dates, names and quotes are checked against the cited lines; links and images are removed. No valid citation → one retry → otherwise shown fully flagged. An empty or cut-off reply is a model failure, not "not found".
7. **Proposals** (`insert_into_note`, `add_action_item`) are validated and returned, never executed. One whose every cited line is speech aimed at an assistant (`proposals::addressed_to_assistant`: instruction overrides, requests to write into the notes, an assistant addressed with a command) is refused (`injected_instruction`). The app holds each by id until it expires; approving applies exactly that.
7a. **Connectors** (`AskEnv::external`, fly-llm `ExternalTools`): offered only on tool tiers and when the explicit scope holds no Keep-local note (otherwise `trace.connectors_off_keep_local`, with a trace line saying so); `mark_keep_local` is called if a Keep-local meeting's line reaches the context anyway (`mark_if_keep_local`, after the prefetch and each tool read); the whole turn then reads Keep-local content excluded and drops on-device history; once `sealed()`, no proposal or further connector call is made (`sealed`).
8. **Run log**: one typed, allowlisted record per answer, returned for the app to persist.
9. **Cancel** (`AskEnv::cancel`, the app's `cancel_ask`): checked before each call and raced against every wait (the prompt read before the first token included, and connector calls), every 100 ms; dropping the request stops Ollama. A stopped Ask returns `cancelled` with no sentences and no proposals.

## Files it owns
- `crates/fly-ask/src/lib.rs` — `ask()`, `AskEnv`/`AskInput`/`AskAnswer`, `Turn`, `Trace`, the retry and failure logic, the M/C re-query.
- `scope.rs` (scope resolution, Keep-local exclusion and its user-only count), `router.rs`, `prefetch.rs`, `evidence.rs` (the retrieved set, prompt rendering, sanitized `SourceView`s), `prompt.rs`, `tier.rs` (tiers, limits, budgets, `measure_tier`, `tool_smoke`), `provider.rs` (`Metered`: budgets, streaming frames, truncation detection), `agent.rs` (tool execution for M/C), `validate.rs`, `claims.rs`, `text.rs` (markers, sentences, links, refusals), `proposals.rs`, `runlog.rs`, `events.rs`, `store.rs`.
- `crates/fly-ask/tests/harness.rs` — end-to-end against a synthetic fixture database with scripted models.

## Surface area
### Public API
| Item | Use |
|---|---|
| `ask<S: StoreAccess, E: QueryEmbedder>(AskEnv, AskInput) -> AskAnswer` | the app's `ask_meeting`; the evals adapter |
| `AskEnv { store, provider, on_device, tier, limits, embedder, events, cancel }` | `provider` comes from `build_provider`; `limits: None` = the tier's own |
| `AskInput { question, history: &[Turn], scope: AskScope, now }` | `Turn { question, answer, citations, on_device }` |
| `AskScope` (serde `{kind, id}`: `all`, `meeting`, `note`, `folder`, `person`) | the command's `scope` argument |
| `AskAnswer { status, text, sentences, sources (each with its `note_id`), proposals, refusals, trace (incl. `connectors_off_keep_local`), failure, timing, on_device, run_log }` | `AnswerStatus`: `answered`, `not_found`, `unverified`, `refused`, `model_failed`, `cancelled` |
| `StoreAccess` (impl for `Storage` and `Mutex<Storage>`) | the lock is taken for one synchronous read at a time, never across a model call |
| `QueryEmbedder` (RPITIT; `NoEmbedder`) | the app's embeds through local Ollama `embeddinggemma`; the future is `Send` when the impl's is |
| `Tier`, `Limits`, `Tier::limits`, `Tier::budget_with`, `measure_tier(TierInputs)`, `tier::smoke_worth_running`, `tool_smoke(&dyn LLMProvider) -> SmokeResult` | `ask_tier` in the app |
| `AskEvent` (`stage`, `sources`, `turn_start`, `frame`), `EventSink` | streamed to the UI (`ask:event`) |
| `text::strip_links`, `text::take_markers`, `text::defuse_markers`, `claims::unsupported` | `insert_ask_answer` / `add_action_item` validation |

### Tiers and budgets (`tier.rs`)
| Tier | Who | Model calls | Tool rounds / calls | Tokens (chars/4) | Prefetch |
|---|---|---|---|---|---|
| S | local ≤ ~8B (llama3.1) | 2 (answer + one retry) | none offered | ≤ 6,000 per request | 14k chars; extras clipped to 600 |
| M | local > 8.5B whose tool calling passed `tool_smoke` (probed `tools` capability) | 3 | 2 / 3 | ≤ 12,000 per request | 18k chars |
| C | cloud | 8 | 5 / 8 | ≤ 40,000 per Ask and per request | 18k chars |

`Metered` forces the last affordable call to answer (`ToolChoice::None`), never sends a request over a cap (`BudgetExhausted`), retries one transient provider error, and flags a first-call prompt whose evaluated token count is under 70% of what was sent (Ollama trimmed it after its retry without `num_ctx`).

### Run log
`RunLog` serializes ids, counts, timings, token counts and enum codes only. Every enum value is written as `<kind>_<value>` (`scope_meeting`, `status_answered`, `retry_no_valid_citations`), so no string in a record is ever a bare word of the question. Never the question, a passage, the answer, a user-typed model name or a provider error. A unit test serializes a full record and checks every leaf.

### Tauri commands, events, settings, files
None of its own (a crate). The app's `ask_meeting`, `insert_ask_answer`, `add_action_item`, the `ask:event` stream, the `ask.tool_smoke.*` setting and `logs/ask-runs.jsonl` are documented in [app-llm](app-llm.md).

### Network egress
Only through the provider and embedder it is handed. It constructs no client.

## Dependencies
- **Inbound:** `src-tauri` (`llm_commands.rs` Ask section); the system evals' Ask contract adapter (`evals/runner/src/harnesses.rs`, a locked change).
- **Outbound:** [fly-context](fly-context.md) (every read: `search_passages`, the read tools, Keep-local filtering), [fly-llm](fly-llm.md) (`LLMProvider`, `run_tool_loop_async`, streaming accumulator), [fly-storage](fly-storage.md) (scope resolution: meetings, folders, keep-local sets, transcripts for item citations), [fly-core](fly-core.md) (`fence_safe`, prompt profiles, models). Path dependencies stay these four so the evals can link it.

## Invariants
- Never constructs a provider, reads a secret or writes to storage (the evals diff the database).
- Keep-local visibility = `on_device`. Off device: Keep-local meetings are excluded in every query (fly-context) and by a fail-closed check here; a narrow Keep-local scope is refused; the excluded count goes to the user's trace only, never into a prompt or tool result.
- A citation is valid only if its id's marker was in a message the harness sent; meeting content has its marker syntax defused (any case, any spacing) and its fences escaped before it reaches a prompt.
- Model-supplied ids and filters are clamped to the scope (`agent.rs`); `get_note` arguments are normalized exactly as fly-context does before the scope check.
- An earlier turn that ran on this device is never sent to an off-device model.
- Nothing displayed (sentences, frames, sources, trace, proposal previews) contains a link or image.
- Proposals are never executed: `executed` is always false, and the app's approval commands validate again.
- The run log holds no free text.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-ask/src/*.rs` | 47 | markers and sentence splitting, link stripping, refusals, defusing; spoken numbers, dates, names, quotes; clip/coverage; the cosine signal; router dates/people/intents; tiers, sizes, budgets, smoke test; `Metered` caps, forcing, cancel, frames, truncation heuristic; proposal gate (injected and unsourced proposals refused); run-log free text |
| integration (runs in CI) | `crates/fly-ask/tests/harness.rs` | 24 | Stop (after a proposal, and before the first token); tier S one-call answer; corrupted numbers/names flagged; out-of-set citations retried then flagged; refusal vs empty reply; the coverage floor and the cosine floor (under it: not found; over it: the model is asked); the kept-local count never reaching the next turn's model; a cut context (Ollama's default-context fallback) as `model_failed(Truncated)`; a giant line cut short instead of failing the budget; connectors (offered only on non-Keep-local scopes, the turn read without Keep-local content, nothing proposed or called once sealed); Keep local for cloud and remote Ollama (and the count stays out of prompts); a Keep-local scope refused; tier C tools clamped to scope, proposals never executed; tool results join the retrieved set; history privacy; links never render (frames and sources too) |
| system evals | `evals/` `ask_contract` (registered in `evals/runner/src/harnesses.rs`) | 439 scenarios | every gate scored from what the harness sent; see [evals/README.md](../../evals/README.md) |
| ignored live | `src-tauri/tests/ask_live.rs` (`cargo test -p fly-app --features ask-live --test ask_live -- --ignored --nocapture`) | — | real local models through the app's provider; records replays outside the repo. The `ask-live` feature (off by default) is the only thing that links fly-evals into fly-app |

## Changing it safely
- Run `cargo test -p fly-ask`. For prompt, budget or validation changes also run the Ask contract (`cargo run -p fly-evals -- --suite ask_contract`) and the live test against the local models.
- The claim rules in `claims.rs` follow the eval scorer's on purpose (number tokenizing in particular): a looser reading of the source side would accept numbers the scorer rejects. **Known limit:** it's a near-copy of `evals/runner/src/claims.rs` (same 107 common words), so the two can drift. With a live model, most unsourced sentences fail only the name rule (names from the meeting title or attendee list, or a capitalized word after a list bullet); relaxing it is an eval change.
- Tests and comments here use invented content only: never text, names, ids or numbers from the eval fixtures or gold questions (the prompt's citation example is `[seg:example-1]`).
- Keep the plain top-8 passages whole in the prefetch: the evals' `must_cite_request_shortfall` gate compares against plain retrieval.
- New run-log fields: typed values only; enums through the `code::` serializers.
- Trap: fly-context's default is 3 passages per meeting, and slots it would leave empty go to further passages of the meetings already found, so a narrow scope already gets more than 3. An explicit `per_meeting` is a strict cap: the harness sets it (4) only on the wide scope's second search, never on a meeting scope.
- The cosine floor only adds "not found" answers to the coverage floor. On the contract's wide questions with the app's embedder, the cosine alone caught 1 of 23 unanswerable questions (their best cosines, 0.24-0.56, overlap the answerable ones', from 0.35), coverage 4, either 4; none of the 86 answerable ones was refused by any rule. Re-measure before letting the cosine decide alone.
