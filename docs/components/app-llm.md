# LLM: Enhance, Ask, polish, extraction (`app-llm`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0), plus backend-only cloud consent (#102). Line references into `llm_commands.rs`, `lib.rs`, `api.ts`, `askModel.ts`, `CloudConsent.tsx` and `devMock.ts` are as of #102.
> Ask runs through the Ask harness ([fly-ask](fly-ask.md), spec v2.5 PR 3.3) since that PR; the Ask rows below describe it. Line numbers elsewhere are as of #102.

## Purpose
Every LLM call the app makes: Enhance (scratchpad + transcript → provenance-tagged blocks), Ask (ephemeral chat over a note, a meeting, a folder, a person or everything, through the [fly-ask](fly-ask.md) harness: validated citations, "not found", proposals), transcript polish (a cleaned variant stored next to the raw one), and structured item extraction into `meeting_items`. It owns `build_provider`, the only place a provider is constructed, and so the only place cloud consent and **Keep local** are enforced (spec §2 item 1). It also owns templates, the block-level edit/keep commands, and two startup migrations of LLM settings.

## Files it owns
- `src-tauri/src/llm_commands.rs` — `build_provider`, policy helpers, Enhance, Ask, polish, templates, Test connection, startup migrations. (The settings and consent commands in this file, including `request_cloud_consent` and its native dialog, belong to [app-settings-consent](app-settings-consent.md).)
- `src-tauri/src/extraction.rs` — extraction prompt, schema, parse/validate, `run_extraction`, backfill commands.

## Surface area
### Public API
Policy and construction (`llm_commands.rs`):

| Item | Line | Used by |
|---|---|---|
| `PROVIDERS: &[(id, default_model, is_local)]` (`ollama`, `openai`, `anthropic`, `nim`) | 17-27 | `build_provider`, `get_llm_settings`, `ollama.rs:255` |
| `enum DataScope { NoMeetingData, Note(&str), Meeting(&str) }` | 501-510 | every caller of `build_provider` |
| `build_provider(&AppState, DataScope) -> Result<Box<dyn LLMProvider>, String>` | 569-575 (delegates) | Enhance, polish, extraction, Test connection |
| `build_provider_with_reach(&AppState, DataScope) -> Result<BuiltProvider{provider, on_device}, String>` (the one constructor; `build_provider` calls it) | after `build_provider` | Ask (`on_device` decides Keep-local visibility in the harness), `build_provider` |
| `split_ask_history(Vec<AskMessage>) -> (Vec<fly_ask::Turn>, question)` | Ask section | `ask_meeting` |
| `ask_tier(&AppState, &dyn LLMProvider, endpoint: Option<&str>) -> fly_ask::Tier` (probe + tool smoke test, cached per server, model and digest) | Ask section | `ask_meeting` |
| `ask_approvals::APPROVALS` (`register_proposal`, `register_answer`, `approve`) | `src-tauri/src/ask_approvals.rs` | `ask_meeting`, `insert_ask_answer`, `add_action_item` |
| `ensure_provider_ready(&AppState)` (starts managed Ollama when provider = `ollama`) | 482-497 | the same five callers, always **before** `build_provider` |
| `check_cloud_policy(provider, is_local, on_device, consented, keep_local)` (pure) | 541-566 | `build_provider`, tests |
| `keep_local_refusal`, `is_consent_refusal`, `record_consent_skip` | 514-519, 438-455 | `build_provider`; `scheduler.rs:404`; `extraction.rs:317` |
| `is_loopback_url`, `validate_base_url` | 522-533, 1134-1163 | `build_provider`, `set_llm_settings`, `embeddings.rs:172,198` |
| `migrate_anthropic_model(&Storage)`, `reset_unseen_cloud_consent(&Storage)`, `CLOUD_CONSENT_RESET_FLAG` | 42-59, 72-119 | `startup.rs:187-195` (startup) |
| `run_polish(&AppState, meeting_id) -> Result<PolishResult, String>` | 821-907 | `polish_transcript`, `scheduler.rs:393` (background AI pass) |
| `build_ask_system(&Note, Option<&Transcript>, &PromptProfile)` (the pre-harness whole-transcript prompt) | Ask section | `tests/llm_bench.rs:670` only (the July baseline) |

Extraction (`extraction.rs`): `build_extraction_prompt` (78-112), `extraction_items_schema` (117-133), `parse_extraction_response` (153-160), `validate_items` (165-221), `extract_items(&dyn LLMProvider, …)` (225-266, pure w.r.t. storage), `run_extraction(&AppState, meeting_id)` (271-309), `extract_after_transcribe` (313-325, best-effort; called from `scheduler.rs:219` and `asr_commands.rs:161`).

#### `build_provider`: what runs, in order (`llm_commands.rs:569-653`)
1. Lock storage (573). Read `llm.provider`, default `ollama` (581). Read `llm.<provider>.model` (else the `PROVIDERS` default) and `llm.<provider>.base_url` (587-588).
2. **Base-URL re-check at use:** if a base URL is stored and the provider isn't `anthropic`, `validate_base_url` (592-594). Fails with "base URL for … must use https:// …".
3. `is_local` = provider is `ollama` in `PROVIDERS` (599-601). An unknown provider id counts as not local.
4. `consented` = setting `consent.cloud_llm.<provider>` == `"true"` (602). Only `request_cloud_consent` writes that row, after a click on allow in a native dialog; the webview can read it but not set it (comment at 595-598; [app-settings-consent](app-settings-consent.md#consent-flow)).
5. `keep_local` from the scope (605-609): `NoMeetingData` → false; `Note(id)` → `Storage::note_keep_local` (any meeting of the note flagged, or its folder chain); `Meeting(id)` → `Storage::meeting_keep_local` (meeting flag or folder chain; an unknown meeting or a lookup error answers **true**, `crates/fly-storage/src/privacy.rs:209-241`).
6. `on_device` = local kind AND (no base URL OR a loopback base URL) (612).
7. `check_cloud_policy` (613, body 541-566): keep-local and not on-device → keep-local refusal; then local → OK; then not consented → consent refusal.
8. Drop the storage lock (614). Only now read the API key from the keychain (616-622); missing → `missing_key_error`: "No API key for {Vendor} yet. Add one in Settings › Ask and Enhance."
9. Construct the provider (624-652). Anthropic always uses its fixed host (stored base URL ignored). Unknown ids fall through to the Ollama arm (649).

No network call happens inside `build_provider`. But each caller runs `ensure_provider_ready` first. For `ollama` that probes `GET <root>/api/version`, possibly a **remote** root from `llm.ollama.base_url`, and may spawn `ollama serve` (`ollama.rs:79-132`). It sends no meeting content.

#### Callers of `build_provider`
| Caller | Scope | Line | Background? |
|---|---|---|---|
| `enhance_note` | `Note(note_id)` | `llm_commands.rs:668` | no |
| `ask_meeting` | from its `scope`: `Note(id)` (default) → `Note`, `Meeting(id)` → `Meeting`, `Folder`/`Person`/`All` → `Wide` (no single Keep-local switch decides; the harness excludes Keep-local meetings in its queries for an off-device model) | `llm_commands.rs` `ask_meeting` | no |
| `run_polish` ← `polish_transcript` (no UI caller) and `scheduler::polish_after_transcribe` (AI pass) | `Meeting(meeting_id)` | `llm_commands.rs:823` | AI pass: yes |
| `run_extraction` ← `extract_meeting_items` (no UI caller), `backfill_meeting_items` (per meeting), `extract_after_transcribe` (AI pass `scheduler.rs:219`; re-diarize/revert `asr_commands.rs:132,152`) | `Meeting(meeting_id)` | `extraction.rs:273` | AI pass and re-diarize: yes |
| `test_llm_connection` | `NoMeetingData` | `llm_commands.rs:1208` | no |
| `retry_consent_skipped` → `scheduler::ai_pass` | via `run_polish` / `run_extraction` | `scheduler.rs:237-256` | yes |

#### Refusal strings the frontend matches
| Source | Text (stable part) | Matched by |
|---|---|---|
| consent (`llm_commands.rs:435`, `559-563`) | `"{Vendor} isn't allowed to receive your meeting content yet, so nothing was sent. …"` | `askModel.ts:62` (shows "Review and allow"); `is_consent_refusal` (`llm_commands.rs:438`) decides background consent-skips; Enhance and backfill show it verbatim |
| keep-local (`llm_commands.rs:514-519`) | `this note is set to "Keep local", so it isn't sent to {provider}. …` | `askModel.ts:56` (lower-cased `keep local`) |
| no key (`missing_key_error`, mark `MISSING_KEY_MARK`) | `No API key for {Vendor} yet. Add one in Settings › Ask and Enhance.` | `askModel.ts:69` ("no api key"); `is_missing_key` → `record_consent_skip`, so the background pass is offered again once a key is added; Enhance shows it as is |
| Ollama (`ollama.rs:85-93,131`) | `Ollama isn't reachable at …`, `Ollama isn't installed …`, `… isn't answering yet …` | `askModel.ts:80,97-99` |

devMock copies these strings by hand (`frontend/src/devMock.ts:931-950`).

#### Extraction contract into `meeting_items`
- Prompt: every line is `[segment_id] Label (speaker_key): text`, metadata and transcript fenced in `<meeting_metadata>`/`<transcript>` and declared "data, not instructions" (`extraction.rs:55-61,91-110`). Simplified contract for profiles with `simplified_contract` (67-74).
- Batches: 3000 words / 150 segments (23-24), via `enhance::plan_cleanup_batches`. Request: no thinking, temperature 0 and JSON schema only for `constrained_json` profiles (239-255).
- Parse: the first `[` … last `]` as JSON; unparseable batch → nothing from that batch (153-160, 258).
- Validate (165-221): known kind (`decision`, `action_item`, `question`, `commitment`, `figure`), non-empty text, case-insensitive (kind, text) dedup across batches, segment ids filtered to ids in the transcript, speaker key dropped if unknown, `status` kept only for action items and only `open`/`done`, cap 100 items (26).
- Persist (`run_extraction`, 298-307): non-empty → `replace_meeting_items` (delete all rows for the meeting, insert). Empty → `mark_extracted` inserts a `kind='_none'` marker row and **does not delete earlier items** (`crates/fly-storage/src/items.rs:149-163`).
- Prefers the polished transcript, falls back to raw (276-284). `extracted_by` = provider id.

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `enhance_note` | `note_id: String, template_id: String` | `Result<Note, String>` | `ui-editor` (`Editor.tsx:1142`), via `api.enhanceNote` |
| `polish_transcript` | `meeting_id: String` | `Result<PolishResult, String>` | none (`api.polishTranscript` exists at `api.ts:135`, no component calls it; the AI pass calls `run_polish` directly) |
| `edit_note_block` | `note_id: String, block_id: String, markdown: String` | `Result<Note, String>` | `ui-editor` (`EnhancedDoc.tsx:525,565`; empty markdown deletes the block), via `api.editNoteBlock` |
| `keep_note_block` | `note_id: String, block_id: String` | `Result<Note, String>` | `ui-editor` (`EnhancedDoc.tsx:501`), via `api.keepNoteBlock` |
| `ask_meeting` | `note_id: Option<String>` (only the default scope; `""` or none with an explicit `scope`, as Home sends), `history: Vec<AskMessage{role, content, on_device?, citations?}>, scope: Option<fly_ask::AskScope>` (`{kind, id}` with kind `note`, `meeting`, `folder` or `person`, or `{kind: "all"}`; default the note), `ask_id: Option<String>` (streams `ask:event`) | `Result<AskReply, String>`: the harness answer (`status`, `text`, `sentences[{text, citations, status, flags}]`, `sources`, `proposals`, `refusals`, `trace`, `on_device`, `run_log`) + `trace_lines` + `insert_id` (the id `insert_ask_answer` takes for this answer; null when no sentence is sourced). Each proposal and the answer's insert are held by id until they expire (`ask_approvals`). A model failure (empty, cut off, a cut context, provider error, budget) is `Err(message)` as before | `ui-ask` (`AskPanel.tsx`), via `api.askMeeting` (the panel shows `text` and keeps `on_device`/`citations` per answer) |
| `cancel_ask` | `ask_id: String` | `Result<(), String>`: sets the cancel flag of the `ask_meeting` running under that id (registered for the run, forgotten when it returns; ids over 128 chars aren't addressable). The harness stops at once, even before the first token (the request is dropped), proposes nothing, logs `status_cancelled`, and `ask_meeting` returns "Ask was cancelled.". An id that isn't running does nothing | `ui-ask` (PR 3.4: Stop), via `api.cancelAsk` |
| `list_ask_people` | — | `Result<Vec<String>, String>`: the people the router can match (attendees, speaker labels, item owners) across the library, for the composer's `@` suggestions (the user's screen only; never sent to a model) | `ui-ask` (PR 3.4: `@`), via `api.listAskPeople` |
| `insert_ask_answer` | `proposal_id: String` (an answer's `insert_id` or an `insert_into_note` proposal's `id`) | `Result<Note, String>`: the held AI block appended (validated again: links stripped, only ids of that note's transcripts kept; locators recorded by the block write). Idempotent: a second call returns the note without adding. Unknown, expired or wrong-kind ids are refused. Every call writes an audit row | none yet (PR 3.4: "Insert into note" and approved `insert_into_note` proposals), via `api.insertAskAnswer` |
| `add_action_item` | `proposal_id: String` (an `add_action_item` proposal's `id`) | `Result<MeetingItem, String>`: the held open action item with `extracted_by = "user:ask"` (validated again: ≥ 1 source line of that meeting; owner = nobody or a speaker/attendee). Idempotent, refuses unknown/expired/wrong-kind ids, writes an audit row | none yet (PR 3.4: approved `add_action_item` proposals), via `api.addActionItem` |
| `list_templates` | — | `Result<Vec<Template>, String>` | `ui-app-shell` (`App.tsx:266,1150`), `ui-settings` (`SettingsModal.tsx:372,806,811`), via `api.listTemplates` |
| `save_template` | `template: Template` (empty id → new id, `built_in=false`) | `Result<(), String>` | `ui-settings` (`SettingsModal.tsx:804`), via `api.saveTemplate` |
| `delete_template` | `id: String` | `Result<(), String>` | `ui-settings` (`SettingsModal.tsx:810`), via `api.deleteTemplate` |
| `test_llm_connection` | — | `Result<String, String>` (`"{id} connection OK"`) | `ui-settings` (`SettingsModal.tsx:791`, after `setLlmSettings` and the consent prompt), via `api.testLlmConnection` |
| `extract_meeting_items` | `meeting_id: String` | `Result<usize, String>` | none (`api.extractMeetingItems` at `api.ts:189`, no caller) |
| `backfill_meeting_items` | — | `Result<BackfillResult{processed, extracted, failed, first_error}, String>` | `ui-settings` (`SettingsModal.tsx:566`), via `api.backfillMeetingItems` |
| `pending_backfill_count` | — | `Result<usize, String>` | `ui-settings` (`SettingsModal.tsx:384,585`), via `api.pendingBackfillCount` |

devMock mirrors all of these except `polish_transcript` and `extract_meeting_items`, which fall to `default: return null` (`devMock.ts:1507`). Drift: mock `enhance_note` checks consent and the key, not keep-local (`devMock.ts:1120-1124`). Mock `backfill_meeting_items` ignores keep-local (`devMock.ts:1413-1419`). Mock Ask applies keep-local only to cloud providers, never to a remote Ollama (`devMock.ts:966-970`).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `llm:consent-skips` | emitted by `scheduler::ai_pass` (`scheduler.rs:229`) from state this component writes (`record_consent_skip`) | `string[]`, every meeting id currently skipped | `ui-confirm-consent` (`CloudConsent.tsx:122`), `ui-notices` (`AppNotices.tsx:169`) |
| `ask:event` | emitted by `ask_meeting` when called with an `ask_id` | `{ask_id, event: fly_ask::AskEvent}`: `stage`, `sources` (sanitized, + the kept-local count for the trace), `turn_start`, `frame` (the reply so far, links and reasoning removed) | `ui-ask` (PR 3.4; nothing listens yet) |
| `pipeline:progress` stage `polishing` | emitted by the scheduler around `run_polish` (`scheduler.rs:390`) | `PipelineProgress` | `app-pipeline` / `ui-editor` |
| — | `extract_after_transcribe` from re-diarize/revert (`asr_commands.rs:158-163`) records consent skips but **emits nothing** | — | the notice only catches up on the next mount or the next AI pass |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_writable_setting_key`) | Meaning |
|---|---|---|---|
| `llm.provider` | read (`llm_commands.rs:487,581`) | no | selected provider, default `ollama` |
| `llm.<id>.model` | read (587); write `llm.anthropic.model` in `migrate_anthropic_model` (49) | no | model override |
| `llm.<id>.base_url` | read (588) | no | custom endpoint; re-validated at use |
| `consent.cloud_llm.<id>` | read (602); bulk delete in `reset_unseen_cloud_consent` (102); written only by `record_cloud_consent` ([app-settings-consent](app-settings-consent.md)) | no: readable, but `set_app_setting` refuses it (`commands.rs:68-79`) | `"true"` = cloud consent recorded |
| `ask.tool_smoke.<provider>.<hash>` | read/write (`ask_tier`) | no | cached tool smoke result for a large local model (`passed` or the failure code), keyed by a hash of the Ollama server, model name and model digest (`/api/tags`; not cached without a digest); decides tier M vs S |
| `migration.cloud_consent_reset_v2_4` | read/write (75, 95, 104) | no | one-time v2.4 consent reset ran |

Full registry: [app-settings-consent](app-settings-consent.md#settings-keys).

### Database
| Table | Columns | Read / write |
|---|---|---|
| `notes` | `blocks_json`, `scratchpad`, `title`, `meeting_id` | read (Enhance, Ask); write `blocks_json` via `update_note_blocks` / `edit_note_block` / `keep_note_block` (`crates/fly-storage/src/notes.rs:211-246`) |
| `transcripts` | `segments`/speakers JSON, `cleaned_json` | read raw + cleaned; polish writes `cleaned_json` (`crates/fly-storage/src/transcripts.rs:96-115`) |
| `meetings`, `folders` | `keep_local`, `title`, `attendees_json`, `started_at` | read (keep-local, extraction metadata) |
| `meeting_items` | `id, meeting_id, kind, text, owner, status, speaker_key, segment_ids_json, created_at, extracted_by` | write (`replace_meeting_items` — keeps `user:` items, `mark_extracted`, `add_meeting_item` for `add_action_item`); read `meetings_missing_items` for backfill; Ask reads through fly-context |
| `templates` | `id, name, system_prompt, structure_hint, built_in` | CRUD; built-ins are seeded once and can be edited or deleted (no backend guard) |
| `settings` | see above | read; startup migrations write |
| `notes_fts`, `embedding_chunks` | — | written indirectly by fly-storage's `sync_note_derived` on every block write (`notes.rs:320-329`); `state.embed_notify` nudges the indexer (`llm_commands.rs:738,756`) |

### Files and directories written
All through fly-storage:
- `<data_dir>/notes/<date> <title>.md` (note mirror) and the meeting folder's `note.json` after Enhance / edit / keep (`notes.rs:329-340`).
- `<data_dir>/logs/ask-runs.jsonl`: one Ask run log per answer (typed, allowlisted, no free text — [fly-ask](fly-ask.md#run-log)); past 2 MB it rolls once to `ask-runs.1.jsonl` (`record_ask_run`, best effort).
- `<data_dir>/logs/ask-approvals.jsonl`: one audit row per approval attempt (`at`, `approval_id`, `kind`, `outcome` = applied / already_applied / expired / unknown / wrong_kind / rejected, `note_id` or `meeting_id`, `created_id`; no text); rolls like the run log (`record_ask_audit`).
- `<data_dir>/recordings/<meeting folder>/transcript.cleaned.{md,json,vtt}` after polish, or `<data_dir>/transcripts/<meeting_id>.cleaned.*` when the meeting has no folder (`transcripts.rs:108-112,299-311`).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `api.anthropic.com` (fixed) | Enhance, Ask, polish, extraction, Test when provider = `anthropic` | consent + keep-local (`build_provider`) |
| `api.openai.com/v1` or a custom https base URL (http only for loopback) | same, provider = `openai` | consent + keep-local + base-URL check on save and on use |
| `integrate.api.nvidia.com/v1` or custom | same, provider = `nim` | same |
| `http://localhost:11434/v1` or `llm.ollama.base_url` (may be another machine) | same, provider = `ollama` | no consent; keep-local refused unless the base URL is loopback |
| `<ollama root>/api/version` (probe) | `ensure_provider_ready`, before `build_provider`, provider = `ollama` | none (no content) |

Background AI pass after every successful transcription (`scheduler.rs:169-173`) and re-extraction after re-diarize/revert make the same calls with no click. The consent gate in `build_provider` is the only thing between them and the network.

### External processes
- `ollama serve` may be spawned by `ensure_provider_ready` → `ollama::ensure_running` (owned by [app-ollama](app-ollama.md)), only when the root is the default `http://localhost:11434` and Ollama is installed.

### Secrets and keychain entries
| Entry | Use |
|---|---|
| `openai_api_key`, `anthropic_api_key`, `nim_api_key` (`fly_secrets::keys`) | read in `build_provider` after the policy check (`llm_commands.rs:616-648`). Written/deleted by `set_llm_settings` ([app-settings-consent](app-settings-consent.md)). |

## Dependencies
- **Inbound:** `lib.rs` (registration 240-256, startup migrations 111-117); `scheduler.rs` (AI pass: `run_polish`, `extract_after_transcribe`, `record_consent_skip`); `asr_commands.rs:158-163` (re-extraction); `embeddings.rs` and `ollama.rs` (reach in for `is_loopback_url` and `PROVIDERS`); `tests/llm_bench.rs`, `tests/anthropic_live.rs` (`build_ask_system`, extraction fns).
- **Outbound:** [fly-llm](fly-llm.md) (`LLMProvider`, `ChatRequest`, providers, `successor_model`), [fly-core](fly-core.md) (`enhance::*` prompts/parsers/cleanup guard, `prompt_profile::profile_for`, `ItemKind`, `MeetingItem`), [fly-storage](fly-storage.md) (notes, transcripts, items, templates, settings, keep-local), [fly-secrets](fly-secrets.md), [app-ollama](app-ollama.md) (`ensure_running`), `AppState` (`storage`, `secrets`, `consent_skipped`, `embed_notify`).

## Invariants
- `build_provider` is the only LLM constructor. Spec §2 item 1. Enforced by `llm_commands.rs:624-652`, the only `OpenAiCompatProvider::`/`AnthropicProvider::new` call sites in `src-tauri/src`.
- Cloud processing needs recorded per-provider consent, checked in the backend, not only the UI. THREAT_MODEL T10. Enforced at `llm_commands.rs:602`, `554-564`. Only the backend writes the consent row (`record_cloud_consent`, after a click on allow in `request_cloud_consent`'s native dialog). The webview can read it, but `set_app_setting` refuses it (`commands.rs:68-79`), so a script in the webview can't forge it.
- Keep-local data never goes to a cloud provider or to an Ollama on another machine, and unknown/failed lookups count as keep-local. T10. Enforced at `llm_commands.rs:548-550,605-612` and `fly-storage/src/privacy.rs:209-241`.
- The keep-local check runs before the consent check, and both run before any key is read (`llm_commands.rs:548-564,613-622`).
- A stored key never goes to a plain-HTTP non-loopback host: checked on save (`llm_commands.rs:1170`) and again on use (592-594). Anthropic's host is fixed (645-648). T4.
- Meeting content is fenced and declared "not instructions" in every prompt. T1. Ask: `build_ask_system` (955-963). Extraction: 59-61. Enhance/polish in fly-core.
- Polish never overwrites the raw transcript, and refuses to save a variant whose ids/speakers/timestamps drifted (`llm_commands.rs:889-898`; DECISIONS.md M4 provenance contract).
- Enhance never saves an empty or unusable block list (720-731).
- Background polish/extraction never fail or stall the transcription job (10-minute cap `scheduler.rs:382`; `extract_after_transcribe` swallows errors `extraction.rs:313-325`).
- Ask history (`split_ask_history`): empty turns dropped, at most 40 messages, must end on the question; an assistant turn without `on_device` is treated as on device, so it never reaches an off-device model.
- Ask gets Keep-local visibility from `build_provider_with_reach`'s `on_device`, never `provider.is_local()` (a remote Ollama is local in kind). The harness never constructs a provider, reads a key or writes; the two Ask writes (`insert_ask_answer`, `add_action_item`) are separate user-clicked commands that take only an id the backend handed out, validate again, are idempotent and audited.
- Connectors: `ask_meeting` gets the turn's tool set from `connectors::turn_for_ask(app, turn_id = ask_id, tier, keep_local_in_scope)` (beta; tiers M/C). A scope holding a Keep-local note gets none (the hub offers nothing and refuses calls); `trace.connectors_off_keep_local` says so when a non-Keep-local turn would have offered tools (a registry-only probe). The harness reads ShareableOnly while connectors are offered, drops on-device history, calls `mark_keep_local` if Keep-local content reaches the context, and proposes or calls nothing after `sealed()`.
- The MCP binary never calls an LLM; it only reads `meeting_items` (spec §2 item 5; `extraction.rs:1-8`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/llm_commands.rs:1217-1646` | 16 | pure `check_cloud_policy` matrix, consent refusal wording, consent-skip classifier, one-time consent reset (real storage), reset flag not webview-settable, the consent dialog's parts (see [app-settings-consent](app-settings-consent.md#tests)), `build_provider` refuses a cloud provider without consent on a real `AppState` (a decline changes nothing, an allow lets it build), loopback detection, base-URL validation |
| unit | `src-tauri/src/extraction.rs:387-525` | 4 | prompt carries ids/keys, parse tolerance, validation drops, `extract_items` with `MockLLMProvider` |
| integration (runs in CI) | `src-tauri/tests/enhance_flow.rs` | 1 | prompt → mock → `parse_enhanced_blocks` → storage provenance, FTS, mirror, reclaim (hand-copied command glue) |
| integration (runs in CI) | `src-tauri/tests/polish_flow.rs` | 2 | cleanup prompt → mock → id-keyed merge, drop-content guard, raw kept |
| integration (runs in CI) | `src-tauri/tests/llm_bench.rs` (`bench_fixtures_parse`) | 1 | bench fixtures parse |
| ignored e2e/harness | `src-tauri/tests/llm_bench.rs` (`llm_bench`), `polish_harness.rs`, `anthropic_live.rs` | 1 + 1 + 5 | real-model contract rates, latency, request shapes accepted by the Anthropic API (metrics printed, not asserted) |
| unit + integration (runs in CI) | `crates/fly-ask` (`cargo test -p fly-ask`) | 51 | the Ask harness: citations, claim check, floor, failures, Keep local off device, tiers, proposals, run log — see [fly-ask](fly-ask.md#tests) |
| vitest | `frontend/src/askModel.test.ts`, `components/AskPanel.test.tsx`, `Editor.enhanced.test.tsx`, `EnhancedDoc.test.tsx` | — | UI side only, against `vi.spyOn(api)` / devMock |

**Known gaps:**
- One test calls `build_provider` itself (`build_provider_refuses_cloud_without_consent`), for the consent gate with `NoMeetingData`. None picks a `Note` or `Meeting` scope, and none asserts that each caller passes the right scope. A wrong scope would pass every test (risk: T10).
- No test proves a refused call makes **zero** network requests; tests only check the returned `Err` of the pure policy function (spec §4.2a item 14 asks for "zero keep-local text in a cloud request").
- Keep-local is untested on every real caller path: Ask, Enhance, polish, extraction, background AI pass, backfill, re-diarize re-extraction.
- `migrate_anthropic_model`, `run_extraction` persistence (including the stale-items-on-empty behavior), the Enhance empty-blocks guard, Ask history trimming, `record_consent_skip` → `llm:consent-skips` are all untested.
- The refusal-string contract with `askModel.ts` and `devMock.ts` is kept by hand-copied literals on both sides; a wording change in Rust breaks Ask's "Review and allow" with all tests green.

## Changing it safely
- Run `cargo test -p fly-app` (unit + `enhance_flow`, `polish_flow`). Prompt or profile changes: also run `llm_bench` and `anthropic_live` locally with `--ignored` (see their file headers). CI never runs them.
- Never construct a provider outside `build_provider`. A new LLM feature (including the agentic Ask) takes a `DataScope` and calls `ensure_provider_ready` + `build_provider`. Pick `Note` when content spans a note's meetings and `Meeting` for one transcript.
- Changing a refusal string: update `askModel.ts` `describeAskError`, `devMock.ts` `MOCK_ASK_ERRORS`, `askModel.test.ts`, `AskPanel.test.tsx` and the Rust test `missing_consent_says_what_to_do_in_plain_words` together.
- New command: register in `lib.rs`, add to `api.ts`, mirror in `devMock.ts` (spec §2 item 4), and add a row here.
- Batch sizes (`POLISH_MAX_BATCH_*`, `EXTRACT_MAX_BATCH_*`) are duplicated in `tests/llm_bench.rs:401-404` and `tests/polish_harness.rs:24-25`. Change them together.
- Trap: `ensure_provider_ready` runs before `build_provider`, so a down remote Ollama reports "isn't reachable" instead of the keep-local refusal.
- Trap: the `run_polish` doc comment still says the default is Anthropic `claude-sonnet-5`. The real default provider is `ollama` (`llm_commands.rs:581`) and the Anthropic default model is `claude-sonnet-5-5`.
- Trap: an empty re-extraction (`mark_extracted`) leaves earlier items in place, so items can go stale after a re-diarize.
