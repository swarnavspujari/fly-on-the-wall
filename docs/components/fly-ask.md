# The Ask harness (`fly-ask`)

> Layer: crate. Added by spec v2.5 PR 3.3 (up to date with main `e4a6a4a`). The streaming Ask UI that renders its sentences, sources, frames and proposals is PR 3.4. Spec v2.5 row 3.7 added the `enhance` module: the same checkers applied to Enhance (see "Enhance, checked" below).

## Purpose
Turns a question into an answer the user can trust because code, not the model, checks it (spec §4.2, §4.2a). One `ask()` call:

1. **Scope**: this meeting, a note, a folder (with subfolders), a person, or everything. Visibility comes from the caller's `on_device` (`build_provider_with_reach`), never from `provider.is_local()`.
2. **Route**: a deterministic self-query router pulls dates ("last week", "Friday's meeting"), people, a folder (`@Clients`) and list intents ("open action items") from the question, validated against known values. Dates are soft filters.
3. **Prefetch** through `fly_context::Context::search_passages_scored` only: the plain search's top 8 transcript passages always go in whole; ranks 9-20 and the router, follow-up, subfolder, wide-scope 4-per-meeting and (one long meeting) single-term searches fill the rest, clipped around their best line. Plus a memory briefing when the question names a known person or recurring meeting (fly-context `memory_prefetch`, facts with their verbatim source lines), else extracted items with their source lines, and the user's notes (demoted: they can't be cited). A short meeting is read whole.
4. **Truncation**: a server that read less of the prompt than was sent (round 1: under 70% of it; later rounds: under 70% of what's new since the previous request, which Ollama can't have cached), or Ollama's default-context fallback, ends the Ask as `model_failed(Truncated)`.
4a. **Floor**: a wide-scope question whose topic words aren't in what was found is "not found" with no model call. An unscoped question that fly-context also measured semantically (an embedder with a measured floor, 4+ content words: `PassageScores::similarity_floor`) must clear that cosine floor as well (`top_similarity`).
5. **Synthesize**, streamed as sanitized frames (no markers, links or reasoning tags while streaming; the final frame, `turn` 0, is the validated answer). Tier S writes from the prefetch alone; tiers M and C may call fly-context's nine tools (six read tools and the three memory tools, all scope-clamped) and the two proposal tools, through fly-llm's tool loop.
6. **Validate**: a citation counts only if the model was shown that line — its marker and its whole text in the same request (`Evidence::add_shown`). `[seg:a, b]` lists, a trailing "Sources: [seg:…]" line and a bare `[id]` of a shown line all cite; sentences are sourced, unsourced or flagged; numbers (spoken forms normalized), dates, names and quotes are checked against the cited lines; links and images are removed. No valid citation → one retry → otherwise shown fully flagged. An empty or cut-off reply is a model failure, not "not found".
7. **Proposals** (`insert_into_note`, `add_action_item`) are offered to the model only when the user's own question asks for that change (`proposals::Requested::from_question`: a write verb with an action item, "remind me", or the note as the target; never from meeting text), and are validated and returned, never executed. One whose cited lines include speech aimed at an assistant (`proposals::addressed_to_assistant`: instruction overrides, requests to write into the notes, an assistant or note taker addressed with a command, a clause opening with approve, pay, transfer and the like) is refused (`injected_instruction`), whatever else it cites. It's strict on purpose: a cited line such as "Buy kiln shelves before the open house." also blocks its proposal (the user can still add that item by hand). The app holds each by id until it expires; approving applies exactly that.
7a. **Connectors** (`AskEnv::external`, fly-llm `ExternalTools`): offered only on tool tiers and when the explicit scope holds no Keep-local note (otherwise `trace.connectors_off_keep_local`, with a trace line saying so); `mark_keep_local` is called if a Keep-local meeting's line reaches the context anyway (`mark_if_keep_local`, after the prefetch and each tool read); the whole turn then reads Keep-local content excluded and drops on-device history; once `sealed()`, no proposal or further connector call is made (`sealed`).
8. **Run log**: one typed, allowlisted record per answer, returned for the app to persist.
9a. **Enhance, checked** (`enhance.rs`, row 3.7): see its own section below. It reuses `claims`, `text::strip_links` and `proposals::addressed_to_assistant`, not the `ask()` flow.
9. **Cancel** (`AskEnv::cancel`, the app's `cancel_ask`): checked before each call and raced against every wait (the prompt read before the first token included, and connector calls), every 100 ms; dropping the request stops Ollama. A stopped Ask returns `cancelled` with no sentences and no proposals.

## Files it owns
- `crates/fly-ask/src/lib.rs` — `ask()`, `AskEnv`/`AskInput`/`AskAnswer`, `Turn`, `Trace`, the retry and failure logic, the M/C re-query.
- `scope.rs` (scope resolution, Keep-local exclusion and its user-only count), `router.rs`, `prefetch.rs`, `evidence.rs` (the retrieved set, prompt rendering, sanitized `SourceView`s), `prompt.rs`, `tier.rs` (tiers, limits, budgets, `measure_tier`, `tool_smoke`), `provider.rs` (`Metered`: budgets, streaming frames, truncation detection), `agent.rs` (tool execution for M/C), `validate.rs`, `claims.rs`, `text.rs` (markers, sentences, links, bare emails, refusals), `proposals.rs`, `runlog.rs`, `events.rs`, `store.rs`.
- `enhance.rs` and `enhance/tests.rs` — the Enhance checks (preselection, citation and claim validation, injection, the run).
- `crates/fly-ask/tests/harness.rs` — end-to-end against a synthetic fixture database with scripted models.

## Surface area
### Public API
| Item | Use |
|---|---|
| `ask<S: StoreAccess, E: QueryEmbedder>(AskEnv, AskInput) -> AskAnswer` | the app's `ask_meeting`; the evals adapter |
| `AskEnv { store, provider, on_device, tier, limits, embedder, events, cancel, external }` | `provider` comes from `build_provider`; `limits: None` = the tier's own; `cancel` is the app's `cancel_ask` flag; `external` the turn's connector tools (`connectors::turn_for_ask`) |
| `AskInput { question, history: &[Turn], scope: AskScope, now }` | `Turn { question, answer, citations, on_device }` |
| `AskScope` (serde `{kind, id}`: `all`, `meeting`, `note`, `folder`, `person`) | the command's `scope` argument |
| `AskAnswer { status, text, sentences, sources (each with its `note_id`), proposals, refusals, trace (incl. `connectors_off_keep_local`), failure, timing, on_device, run_log }` | `AnswerStatus`: `answered`, `not_found`, `unverified`, `refused`, `model_failed`, `cancelled` |
| `StoreAccess` (impl for `Storage` and `Mutex<Storage>`) | the lock is taken for one synchronous read at a time, never across a model call |
| `QueryEmbedder` (RPITIT; `NoEmbedder`) | the app's embeds through local Ollama `embeddinggemma`; the future is `Send` when the impl's is |
| `Tier`, `Limits`, `Tier::limits`, `Tier::budget_with`, `measure_tier(TierInputs)`, `tier::smoke_worth_running`, `tool_smoke(&dyn LLMProvider) -> SmokeResult` | `ask_tier` in the app |
| `AskEvent` (`stage`, `sources`, `turn_start`, `frame`), `EventSink` | streamed to the UI (`ask:event`) |
| `text::strip_links`, `text::take_markers`, `text::defuse_markers`, `claims::unsupported` | `insert_ask_answer` / `add_action_item` validation. `strip_links` also removes a bare email (`dana@example.com`; `@dana`, `a@b`, `user@localhost` stay) |
| `enhance::{prepare, check, enhance, tally, Prepared, Checked, EnhanceReport, EnhanceError, EnhanceRequest}` | `enhance_with` in the app (`llm_commands.rs`); the evals' `enhance_contract` suite |

### Enhance, checked (`enhance.rs`)
Enhance asks a model for blocks (`{type, markdown, sources}`; the contract and the per-model format schemas are fly-core's and unchanged). This module decides what the user is told about them.

- **What the model reads** (`prepare(note, transcript, template, profile, prompt_chars)`). A transcript that fits `prompt_chars` goes in whole, byte for byte as before. Otherwise the lines are chosen deterministically: the closing lines, evenly spaced excerpts across the whole meeting (about 1,500 characters each, up to 80), and the lines that speak to the topics in the user's notes (`salient_terms` of the scratchpad, rarer terms weigh more, each hit with the reply after it); 60% of the room for coverage when the notes name topics. Gaps are marked in the prompt (`(… 38 lines left out, about 11 minutes …)`), the model is told it reads excerpts, and the numbers it cites count the lines shown, so a citation can only name a line that was sent. Lines the detector reads as addressed to an AI never take the topic budget. The size is the context actually sent: the app passes fly-llm's `max_prompt_chars(max_tokens)` for Ollama (`num_ctx` sizing run backwards, 84,480 characters at the 4,096-token reply budget) and 300,000 for a hosted model.
- **Truncation** (`enhance()`): `chat_with_usage` reports `Usage::context_fallback` when an Ollama runner failed with the sized `num_ctx` and the reply came from the server's default context. That is treated as truncation: one retry with a prompt sized for a 16,384-token context, then `EnhanceError::Truncated` (nothing saved, the note's enhanced document untouched). Only that definitive signal is used; Ollama's `prompt_eval_count` can read low on a cached prompt (a re-run), so the Ask heuristic would block re-enhance.
- **What the model said** (`check(output, &Prepared)`):
  - a citation counts only if it names a line that was sent (numbers, `"3"`, `"seg:3"` read; anything else is invalid), once each; a block with no valid source is unsourced;
  - numbers (spoken forms normalized), dates, names and quotes are checked against the cited lines **and the note's title and notes** with `claims::unsupported`; a block that fails is `flagged` (`BlockFlag { kind, detail }`, saved with the block), keeps its sources so the user can check them, and never shows as sourced; a block that cites nothing is flagged `uncited_claim` only if it states a number, date or quote the notes don't hold;
  - the claim text is prepared for the shared rules, which are not changed: list syntax is stripped; a capital that only starts a sentence ("Approved the budget") is lowercased unless the word is a person in this meeting (a speaker label, or capitalized mid-sentence in the transcript or notes) or does something and no word of the meeting starts like it ("Wanda will…"); initialisms (ARR, Q1) are not names; `Friday".` and `(Tomas).` are reordered so the claim check does not read `friday"` as a name; the same fix is applied to the sources (`settle`: `Nov 10),` holds 10);
  - links, images, HTML and bare emails are removed from AI text (`text::strip_links`);
  - a `user` block must restate the scratchpad (not the title, which an invite can set): every link, address and piece of HTML as a whole word as written, every number, date, quote and name, and at least half its content words; otherwise it is shown as an AI block flagged `not_from_notes`.
- **Injection.** A line for which `addressed_to_assistant` is true is never evidence: a block whose every valid citation is such a line is dropped (counted, and its text kept in `EnhanceReport::dropped_text`, a few, cut short, so a wrong call is not a loss); in a block that also cites other lines the injected one is removed from its sources and supports no claim. When every block is dropped the run errors (`NothingLeft`) instead of replacing the note with nothing. **Known limit:** the detector is deliberately strict (a clause opening "Approve the budget" or "pay …", "add an action item") and has no knowledge of who is speaking to whom, so an ordinary instruction between colleagues can be dropped from the notes; the report names it.
- **`EnhanceReport`** (serde, returned to the UI): lines total and shown, blocks by what the user sees (user, sourced, flagged, unsourced, notes), dropped (with text), demoted user blocks, invalid citations, links removed, `retried_smaller`. Counts go to the log; the text never does.

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
- **Outbound:** [fly-context](fly-context.md) (every read: `search_passages`, the read tools, Keep-local filtering), [fly-llm](fly-llm.md) (`LLMProvider`, `run_tool_loop_async`, streaming accumulator), [fly-storage](fly-storage.md) (scope resolution: meetings, folders, keep-local sets, transcripts for item citations), [fly-core](fly-core.md) (`fence_safe`, prompt profiles, models). Path dependencies stay these four so the evals can link it. `enhance.rs` uses `fly_core::enhance` (the prompt and the parser), `fly_context::passages::matched_terms` (topic matching) and `fly_llm::LLMProvider::chat_with_usage`.

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
| unit | `crates/fly-ask/src/enhance/tests.rs` | 32 | Enhance: a fitting meeting is the old prompt; a long one is preselected to the budget (coverage of every third, the opening and the last line, the notes' topics, deterministic, sizes always fit); invalid, duplicate and odd citations; spoken numbers pass and changed ones flag; dates, names, quotes; structure words, list numbers, initialisms, bracketed sources; facts from the notes; uncited claims; export labels; injection (dropped, supports nothing, nothing-left error, uncited echo); links and emails; forged user blocks (links, addresses, names, titles, lookalike hosts); no-transcript notes; prose and unusable replies; the run (request fields per profile, fallback retry and failure, provider failure) |
| unit | `crates/fly-ask/src/*.rs` | 54 | markers and sentence splitting, link stripping, refusals, defusing; spoken numbers (a currency sign glued to the word before it, "Speaker 2" as a label, a name vouched for by the cited meeting's title), dates, names, quotes; clip/coverage; the cosine signal; router dates/people/intents; tiers, sizes, budgets, smoke test; `Metered` caps, forcing, cancel, frames, truncation heuristic; proposal gate (injected and unsourced proposals refused); run-log free text |
| integration (runs in CI) | `crates/fly-ask/tests/harness.rs` | 24 | Stop (after a proposal, and before the first token); tier S one-call answer; corrupted numbers/names flagged; out-of-set citations retried then flagged; refusal vs empty reply; the coverage floor and the cosine floor (under it: not found; over it: the model is asked); the kept-local count never reaching the next turn's model; a cut context (Ollama's default-context fallback) as `model_failed(Truncated)`; a giant line cut short instead of failing the budget; connectors (offered only on non-Keep-local scopes, the turn read without Keep-local content, nothing proposed or called once sealed); Keep local for cloud and remote Ollama (and the count stays out of prompts); a Keep-local scope refused; tier C tools clamped to scope, proposals never executed; tool results join the retrieved set; history privacy; links never render (frames and sources too) |
| system evals | `evals/` `ask_contract` (registered in `evals/runner/src/harnesses.rs`) | 439 scenarios | every gate scored from what the harness sent; see [evals/README.md](../../evals/README.md) |
| ignored live | `src-tauri/tests/ask_live.rs` (`cargo test -p fly-app --features ask-live --test ask_live -- --ignored --nocapture`) | — | real local models through the app's provider; records replays outside the repo. The `ask-live` feature (off by default) is the only thing that links fly-evals into fly-app |

## Changing it safely
- Enhance: `cargo test -p fly-ask --lib enhance`, then `cargo run -p fly-evals -- --suite enhance_contract` (report only; the zero-tolerance metrics must stay 0, and its sourced-rate on the fixtures' own notes shows what a change does to false flags). `fly_core::BlockFlagKind` and the frontend's `BlockFlag` mirror this module's flags: change them together.
- Run `cargo test -p fly-ask`. For prompt, budget or validation changes also run the Ask contract (`cargo run -p fly-evals -- --suite ask_contract`) and the live test against the local models.
- The claim rules in `claims.rs` follow the eval scorer's on purpose (number tokenizing in particular): a looser reading of the source side would accept numbers the scorer rejects. Two tokenizing rules both copies share: a currency sign glued to the word before it splits off (ASR writes `now$52,000`, which must still read as 52,000), and the digit in the app's own label "Speaker 2" is a name, not a number (the label must be a cited line's speaker). **Known limit:** it's a near-copy of `evals/runner/src/claims.rs` (same 107 common words), so the two can drift. With a live model, most unsourced sentences fail only the name rule. A name in the cited meeting's title now counts (a project the question named; titles vouch for names only, and not for proposals, which stay checked against the lines alone); names from the attendee list, or a capitalized word after a list bullet, can still trip it, and relaxing those is an eval change.
- Tests and comments here use invented content only: never text, names, ids or numbers from the eval fixtures or gold questions (the prompt's citation example is `[seg:example-1]`).
- Keep the plain top-8 passages whole in the prefetch: the evals' `must_cite_request_shortfall` gate compares against plain retrieval.
- New run-log fields: typed values only; enums through the `code::` serializers.
- Trap: fly-context's default is 3 passages per meeting, and slots it would leave empty go to further passages of the meetings already found, so a narrow scope already gets more than 3. An explicit `per_meeting` is a strict cap: the harness sets it (4) only on the wide scope's second search, never on a meeting scope.
- The cosine floor only adds "not found" answers to the coverage floor. On the contract's wide questions with the app's embedder, the cosine alone caught 1 of 23 unanswerable questions (their best cosines, 0.24-0.56, overlap the answerable ones', from 0.35), coverage 4, either 4; none of the 86 answerable ones was refused by any rule. Re-measure before letting the cosine decide alone.
