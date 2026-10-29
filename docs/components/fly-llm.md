# LLM providers (`fly-llm`)

> Layer: crate. Described at main `82b7edf` (v2.4.0).
> In flight: None in this crate. The tool loop, `converse()` / `chat_stream()` and capability probing are merged but **dormant** (no app caller); the Ask harness PR 3.3 will wire them.

## Purpose
The `LLMProvider` trait and its two HTTP implementations: `AnthropicProvider` (`/v1/messages`) and `OpenAiCompatProvider` (OpenAI, NVIDIA NIM, and Ollama's native `/api/chat`). Today every feature (Enhance, Ask, transcript polish, item extraction, Test connection) uses only the text-only `chat()` call, whose request bodies are frozen by a golden test. The block-structured `converse()` / `chat_stream()` surface, the bounded tool loop, and small-model tool-call repair are there for the agentic Ask harness. The crate holds no policy: consent, Keep local and base-URL checks live in the app's `build_provider`.

## Files it owns
- `crates/fly-llm/Cargo.toml`: deps reqwest, serde(_json), thiserror, tracing, async-trait, futures-util, fly-core (declared, not used in code). No keychain or secrets crate.
- `crates/fly-llm/src/lib.rs`: `LLMProvider` trait, `LlmError`, `transport_error`, `ChatRequest` / `ChatMessage` / `Role` / `ThinkingMode`, `ProviderSettings`, `ChatStream`.
- `crates/fly-llm/src/anthropic.rs`: Anthropic Messages API: per-model request shaping, 400-relax retry, SSE decoder, response parsing, model ids.
- `crates/fly-llm/src/openai_compat.rs`: OpenAI / NIM chat-completions and Ollama native chat: bodies, `think` / `num_ctx` handling and retries, SSE and NDJSON decoders, `/api/show` probe.
- `crates/fly-llm/src/conversation.rs`: block types (`ContentBlock`, `Message`, `ToolSpec`, `ToolChoice`, `ConverseRequest`, `ChatResponse`, `StreamEvent`, `Usage`, `Capabilities`) and schema helpers (`closed_schema`, `strict_schema`).
- `crates/fly-llm/src/stream.rs`: byte→line framing (`LineBuffer`, 8 MiB line cap), `decode_body`, `replay`, `collect`, `Accumulator`.
- `crates/fly-llm/src/repair.rs`: `classify()` reads a text reply as a plain answer, a recoverable tool call, or a malformed one.
- `crates/fly-llm/src/tool_loop.rs`: `run_tool_loop` with budgets, a corrective turn, and content-free run records; `run_tool_loop_async` (the same loop with an async executor, used by the Ask harness [fly-ask](fly-ask.md) so a model's search can embed its query; `run_tool_loop` delegates to it).
- `crates/fly-llm/src/mock.rs`: `MockLLMProvider`: canned `chat()` text, or a scripted list of `converse()` replies with request recording.
- `crates/fly-llm/src/golden.rs` (`#[cfg(test)]`): byte-identity test for legacy `chat()` bodies.
- `crates/fly-llm/tests/golden/legacy_bodies.jsonl`: 105 golden cases (7 request shapes × 15 provider/model/variant combos).

## Surface area
### Public API
| Group | Items |
|---|---|
| Trait | `LLMProvider { id, model, is_local, chat, test_connection, capabilities, probe_capabilities, converse, chat_stream }` (`lib.rs:181-225`). The default `converse()` refuses tools or non-text blocks with `InvalidRequest` (`lib.rs:208-217`), and the default `chat_stream()` replays `converse()`. |
| Legacy request | `ChatRequest { messages, temperature, max_tokens, thinking, format, effort }`, `ChatMessage`, `Role`, `ThinkingMode::{Default, Disabled, Enabled}` (`lib.rs:86-159`) |
| Block request | `ConverseRequest`, `Message`, `ContentBlock::{Text, ToolUse, ToolResult, Opaque}`, `ToolSpec`, `ToolChoice::{Auto, None, Required}` (`conversation.rs:18-194`) |
| Response | `ChatResponse`, `StopReason::{EndTurn, ToolUse, MaxTokens}`, `Usage`, `StreamEvent`, `Capabilities`, `ChatStream` |
| Errors | `LlmError::{NotConfigured, Auth, Provider, Network, Unavailable, InvalidRequest, ToolProtocol}`, `transport_error()` (`lib.rs:31-82`) |
| Anthropic | `AnthropicProvider { api_key, model, base_url, relax_on_400 }` + `new()` (`anthropic.rs:116-134`), `ANTHROPIC_DEFAULT_MODEL = "claude-sonnet-5-5"`, `ANTHROPIC_FAST_MODEL = "claude-haiku-5-5"`, `successor_model()`, `MAX_STRICT_TOOLS = 20`, `parse_messages_response(_checked)`, `parse_messages_converse` |
| OpenAI-compatible | `OpenAiCompatProvider { provider_id, base_url, api_key, model, local }` + `openai()`, `nim()`, `ollama()` (`openai_compat.rs:38-76`), `parse_chat_completion`, `parse_native_chat`, `parse_native_response`, `parse_openai_response` |
| Streaming | `stream::{LineBuffer, MAX_LINE_BYTES, sse_data, replay, response_events, collect, Accumulator, parse_tool_args}` |
| Tool loop (fly-ask) | `tool_loop::{run_tool_loop, run_tool_loop_async, ToolLoopLimits (default 8 calls / 5 rounds / 6,000 chars), LoopEnd, ToolCallRecord, ToolLoopOutcome, ToolExecutor, UNKNOWN_TOOL}` |
| Repair | `repair::{classify, TextToolCall}` |
| Mock | `mock::MockLLMProvider::{new, with_response, scripted, requests, remaining}` |
| Unused by the app | `ProviderSettings` (`lib.rs:163-167`) |

The app uses only `chat()`, `test_connection()`, `model()`, `id()`, the four constructors, `ANTHROPIC_DEFAULT_MODEL`, `successor_model()` and `MockLLMProvider` (`src-tauri/src/llm_commands.rs:9,23,48,624-652`, `src-tauri/src/extraction.rs:12`).

### Tauri commands
None (crates expose no commands). The LLM commands live in [app-llm](app-llm.md).

### Events
None.

### Settings keys
None read by this crate. The app reads `llm.provider`, `llm.<provider>.model`, `llm.<provider>.base_url` and `consent.cloud_llm.<provider>` in `build_provider` (`llm_commands.rs:581-602`) and passes plain values in; see [app-settings-consent](app-settings-consent.md).

### Database
None.

### Files and directories written
None at runtime. The golden test rewrites `crates/fly-llm/tests/golden/legacy_bodies.jsonl` only when `FLY_LLM_BLESS` is set (`golden.rs:193-201`).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `https://api.anthropic.com/v1/messages` (`anthropic.rs:131,352-355`) | `AnthropicProvider` `chat` / `converse` / `chat_stream` / `test_connection` | App only: `build_provider` checks cloud consent and Keep local (`llm_commands.rs:602-613`). The host is fixed because the app never sets `base_url` (`llm_commands.rs:591,645-648`). The field is `pub`, so the crate itself doesn't enforce it. |
| `https://api.openai.com/v1/chat/completions` (`openai_compat.rs:51,264-268`), or a custom base URL | OpenAI provider calls | Consent + Keep local in `build_provider`. Custom base URL must be https (http only to loopback), checked on save (`llm_commands.rs:1170`) and on use (`llm_commands.rs:592-594`, `validate_base_url` `:1134-1163`). |
| `https://integrate.api.nvidia.com/v1/chat/completions` (`openai_compat.rs:61`), or a custom base URL | NIM provider calls | Same as OpenAI. |
| `http://localhost:11434/api/chat` and `/api/show` (`openai_compat.rs:71,192-194,253-257,323`), or the configured Ollama URL (any http(s) host) | Ollama `chat` / `converse` / `chat_stream` / `probe_capabilities` (probe is dormant) | No consent (local kind). Keep-local data is refused when the Ollama URL isn't loopback (`on_device`, `llm_commands.rs:612`). |

Retries can repeat a request to the same host: Anthropic re-POSTs after a 400 up to 4 times, each time with one optional feature removed (`anthropic.rs:294-301,332-339`). Ollama retries once without `think` (400 mentioning "think") and once without `num_ctx` (5xx) (`openai_compat.rs:338-355`). There is no retry or backoff for 429 / 529; they come back as `LlmError::Provider("<status>: <first 300 chars>")`.

Timeouts: the Anthropic client has a 20 s connect / 1,200 s total timeout (`anthropic.rs:492-501`). The OpenAI-compatible tool/stream/probe client has 20 s connect / 600 s read-idle (`openai_compat.rs:88-97`). The legacy `chat()` path builds `reqwest::Client::new()` per call, with no timeout (`openai_compat.rs:126,296`).

### External processes
None.

### Secrets and keychain entries
None read here. Keys arrive as plain `String`s from `build_provider`, which reads `fly_secrets::keys::{OPENAI,NIM,ANTHROPIC}_API_KEY` (`llm_commands.rs:616-648`). Header carriage:
| Provider | Header |
|---|---|
| Anthropic | `x-api-key: <key>`, `anthropic-version: 2023-06-01`, plus `anthropic-beta: server-side-fallback-2026-07-01` only when the body has `fallbacks` (`anthropic.rs:356-360,379`) |
| OpenAI / NIM | `Authorization: Bearer <key>` (`openai_compat.rs:270-272`) |
| Ollama | none (`api_key: None`, `openai_compat.rs:72`) |

The crate never logs keys or bodies. Its one `tracing::warn!` logs only the model name (`openai_compat.rs:349-352`). Provider error bodies (≤300 chars) do flow into `LlmError::Provider` text, which callers show and may log.

## Dependencies
- **Inbound:** `src-tauri/src/llm_commands.rs` (`build_provider`, Enhance, Ask, polish, Test connection), `src-tauri/src/extraction.rs`, and the app tests `enhance_flow.rs`, `polish_flow.rs`, `polish_harness.rs` (ignored), `llm_bench.rs` (ignored), `anthropic_live.rs` (ignored). [fly-context](fly-context.md) does not depend on it; its `ToolDef` only mirrors `ToolSpec` in a comment (`crates/fly-context/src/tools.rs:47-48`).
- **Outbound:** reqwest 0.13 (workspace), serde / serde_json (sorted-key maps: no `preserve_order` in `Cargo.lock`), futures-util, async-trait, thiserror, tracing. fly-core is declared but unused.

## Invariants
- `build_provider` is the only LLM constructor; consent and Keep local are enforced there, before any request (spec §2 item 1; `lib.rs:13-14`; enforced at `llm_commands.rs:569-653`). The crate cannot enforce this itself: constructors and fields (`base_url`, `api_key`) are `pub`.
- A stored API key only goes to an https host or loopback; the Anthropic host is fixed (THREAT_MODEL T4; `docs/SECURITY_AUDIT_2026-10.md` "Anthropic and Groq hosts are fixed"). Enforced in the app (`llm_commands.rs:591-594,645-648,1134-1163`), not here.
- `is_local()` means *local kind*, not *on this device*: an Ollama provider is `local: true` for any base URL (`openai_compat.rs:68-76`). The app derives `on_device` separately (`llm_commands.rs:612`). Callers must not use `is_local()` as a privacy decision.
- `temperature` is sent to Anthropic only for an allowlist of older model prefixes, so current and future models never get a 400 (`anthropic.rs:94-114,182-186`).
- "Thinking off" uses the shape each model accepts: `{type: disabled}`, `{type: between_tools}` on Sonnet 5.5, or `effort: low` on Opus 5.5 / Fable / Mythos (`anthropic.rs:63-81,196-205`). This keeps structured JSON from truncating (`lib.rs:121-125`).
- Legacy `chat()` bodies stay byte-identical for every provider (`lib.rs:8-9`; test `golden.rs:190-219`).
- The 400-relax step reads only `/error/message`, never the whole error body (which can echo request text), and removes each feature at most once (`anthropic.rs:507-564`).
- `Opaque` blocks (Anthropic thinking with signatures) go back only to the provider that made them and must never be logged (`conversation.rs:39-47`; `anthropic.rs:419-422`).
- Tool-loop records hold tool names and success only. An invented tool name is recorded as `<unknown tool>` and echoed to the model capped at 64 chars (`tool_loop.rs:63-73,95-98,204-218`; spec §4.2a #7, #12).
- A stream that ends without `Done` is an error, never a finished answer. Lines over 8 MiB are refused (`stream.rs:20,86-91,251-254`).
- On Ollama, `format` together with tools is refused, and `think` is sent explicitly whenever tools are present (`openai_compat.rs:375-399`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-llm/src/anthropic.rs` | 35 | per-model body shaping (temperature, thinking, effort, structured output, fallbacks, strict tools, forced choice), relax-after-400 as a pure fn, response/refusal/truncation parsing, SSE decoder, capability/builder agreement |
| ignored e2e/harness | `crates/fly-llm/src/anthropic.rs` | 1 | `anthropic_tool_round_trip_live` (needs `ANTHROPIC_API_KEY`) |
| unit | `crates/fly-llm/src/golden.rs` | 1 | 105 legacy `chat()` bodies byte-for-byte (data in `tests/golden/legacy_bodies.jsonl`) |
| unit | `crates/fly-llm/src/openai_compat.rs` | 21 | Ollama native body / `think` / `num_ctx`, OpenAI / NIM bodies and strict schema, tool-call parsing, NDJSON and SSE decoders, `/api/show` parsing |
| ignored e2e/harness | `crates/fly-llm/src/openai_compat.rs` | 2 | `ollama_num_ctx_marker_live`, `ollama_tool_loop_live` (need a running Ollama) |
| unit | `crates/fly-llm/src/repair.rs` | 6 | text-written tool calls (llama, qwen, fenced, OpenAI shape), malformed cases |
| unit | `crates/fly-llm/src/stream.rs` | 8 | line framing (split UTF-8, CRLF, cap), replay/collect, accumulator, cut stream |
| unit | `crates/fly-llm/src/tool_loop.rs` | 10 | loop mechanics with `MockLLMProvider::scripted`: budgets, repair, correction, unknown tools, truncation, opaque-block echo |

**Known gaps:**
- **No test touches the HTTP layer.** No loopback server exists, so URL paths, auth headers, the `anthropic-beta` header, 401/403→`Auth`, the 4-step 400-relax loop, the Ollama think / `num_ctx` retries and `transport_error` messages could all regress with CI green. A key-routing bug (T4) would not be caught here.
- **No rate-limit / overload behavior.** 429 and 529 become a generic `Provider` error with no retry, and no test pins that.
- **The golden covers only legacy `chat()` builders.** The request shapes in `golden.rs:45-139` are hand copies of the app's call sites, so a change in `llm_commands.rs` / `extraction.rs` passes. `converse` / tool / stream bodies aren't frozen, and `FLY_LLM_BLESS=1` rewrites the file and passes.
- **Per-model request shapes are checked against the crate's own prefix tables, never the real API** (only `#[ignore]` live tests do that). At runtime, `relax_after_400` hides drift by silently dropping features (e.g. thinking-off, so truncation returns).
- **Tool-loop tests use a scripted mock that ignores the request** (`mock.rs:115-125`) and advertises every capability (`mock.rs:100-113`), so they can't catch provider-specific rejections. Untested: args malformed twice → `ToolProtocol`, a forced final turn that still calls a tool (answer `""`), `LoopEnd::MaxTokens`.
- **No test of the legacy OpenAI-compatible path's missing timeout** (`openai_compat.rs:126,296`).
- **Consent / Keep-local-before-network can't be tested here** by design. App tests (`enhance_flow.rs`, `polish_flow.rs`, `extraction.rs:508`) inject `MockLLMProvider` directly and so bypass `build_provider`.

## Changing it safely
- Run `cargo test -p fly-llm`. For request-shape changes, also run the ignored live tests with your own key: `cargo test -p fly-llm -- --ignored anthropic_tool_round_trip_live` and `src-tauri/tests/anthropic_live.rs`.
- **Intentional legacy-body change:** bless with `FLY_LLM_BLESS=1 cargo test -p fly-llm golden`. Each jsonl line embeds the body as an escaped string, so decode both sides to review the diff, and say why in the PR. Never bless to "fix" an unexplained failure.
- **New Anthropic model family:** update these together: `TEMPERATURE_ACCEPTING_PREFIXES`, `supports_output_config`, `thinking_off_for`, `rejects_forced_tool_choice`, `successor_model`, `capabilities()`, and their tests. `forced_choice_capability_matches_the_request_builder` keeps capability and builder in sync. Update the app's default model list (`llm_commands.rs:17-27`) if the default changes.
- Keep providers constructed only in `build_provider`. Never set `AnthropicProvider.base_url` from settings (T4).
- When wiring `converse` / the tool loop (Ask PR 3.3), pick fly-context's `Visibility` from `build_provider`'s on-device decision, not from `is_local()`.
- Don't log `ContentBlock::Opaque`, tool arguments or results, or `LlmError::Provider` text in run logs (spec §4.2a #7).
- Adding a field to `ChatRequest` changes every exhaustive struct literal in the app (`llm_commands.rs:691,854,1013`, `extraction.rs:240`) and must keep the golden green.
