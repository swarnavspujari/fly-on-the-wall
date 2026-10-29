# Managed Ollama (`app-ollama`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: None.

## Purpose
Runs the local LLM runtime for the default "ollama" provider (Enhance, Ask, polish, extraction) and for semantic-search embeddings. The runtime is an opt-in download through the artifact manager ([app-models-gpu](app-models-gpu.md)). When nothing answers at the default address, the app spawns its own `ollama serve` child and kills it on exit. It also reports status and pulls / deletes models through Ollama's HTTP API. A server the user runs themselves (desktop app, service, custom URL) is always respected and never stopped.

## Files it owns
- `src-tauri/src/ollama.rs` — root URL, `server_alive`, `ensure_running` (spawn), `shutdown`, `ollama_status` / `ollama_pull` / `ollama_delete`, `pull_model` stream parser, 2 tests.
- `src-tauri/src/state.rs:37-39`, `state.rs:113` — the `AppState.ollama: Mutex<Option<Child>>` field (the rest of `state.rs` belongs to [app-shell](app-shell.md)).
- `src-tauri/src/lib.rs:169-171` (command registration) and `lib.rs:202-211` (the `RunEvent::Exit` hook that calls `shutdown`).

## Surface area
### Public API
- `ARTIFACT_ID = "ollama-bin"` (`ollama.rs:20`) — used by `asr_commands.rs:278` to keep the runtime out of the transcription model list. Its comment says "Windows-only", but `models.rs:192-201` also pins a macOS build; Linux has none.
- `root_url(state)` (`pub(crate)`, `ollama.rs:32-45`) — `llm.ollama.base_url` normalized (no trailing `/`, no `/v1`), else `http://localhost:11434`.
- `server_alive(root)` (`pub(crate)`, `ollama.rs:60-73`) — `GET <root>/api/version`, 1.5 s timeout.
- `ensure_running(state)` (`ollama.rs:79-132`) — reuse a listening server; refuse to manage a custom URL; else spawn the managed child and wait up to 30 s for it to answer.
- `shutdown(state)` (`ollama.rs:136-142`) — kill and wait the child this app spawned, if any.
- `pull_model(root, model, progress)` (`ollama.rs:312-394`) — `POST /api/pull` streaming JSON lines mapped to `ModelProgress` (id `ollama:<model>`).
- Types `OllamaModel { name, size }`, `OllamaStatus { installed, can_install, running, managed, base_url, models }` (`ollama.rs:148-169`); mirrored in `frontend/src/types.ts:281-299`.

Callers of `ensure_running`: `llm_commands::ensure_provider_ready` when `llm.provider` is `ollama` (`llm_commands.rs:482-497`), and the semantic-search embedding worker **regardless of provider** whenever there is an embedding backlog (`embeddings.rs:126-140`, `embeddings.rs:187`). Query-time search only calls `server_alive` and never spawns (`embeddings.rs:78-88`).

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `ollama_status` | none | `Result<OllamaStatus, String>` | `ui-settings` (`SettingsModal.tsx:366`, on open and after every Ollama action), via `api.ollamaStatus` (`api.ts:198`) |
| `ollama_pull` | `model: String` | `Result<(), String>`; progress on `model:progress` with id `ollama:<model>` | `ui-settings` (`SettingsModal.tsx:624`, download / guided swap), via `api.ollamaPull` (`api.ts:201`) |
| `ollama_delete` | `model: String` | `Result<(), String>`; refuses the model the app is configured to use | `ui-settings` (`SettingsModal.tsx:652`, after an in-app confirm), via `api.ollamaDelete` (`api.ts:204`) |

How status is computed (`ollama.rs:171-188`): `running` = `/api/version` answered 2xx within 1.5 s; `models` = `/api/tags` (3 s timeout) only when running, else empty (errors become empty); `installed` = managed probe `bin/ollama/ollama[.exe]` exists or `ollama` is on PATH; `can_install` = this OS has an `ollama-bin` artifact (Windows, macOS); `managed` = the app holds a child handle (a child that already exited still counts until the next `ensure_running` reaps it); `base_url` = `root_url`.

`ollama_delete` reads `llm.provider` (default `ollama`) and `llm.ollama.model` (default from `llm_commands::PROVIDERS`) and refuses a tag-insensitive match (`same_model`, `ollama.rs:230-238`, `ollama.rs:265-269`). Then it may **start** the managed server just to delete (`ollama.rs:271`). 404 → "model … is not installed".

The runtime install itself is `download_model("ollama-bin")` ([app-models-gpu](app-models-gpu.md)).

devMock mirrors all three (`devMock.ts:1341-1364`) with drift: `can_install` is always `true` (the backend says `false` on Linux), `running` equals `installed`, `ollama_delete` never refuses the in-use model (the UI hides that button instead, `SettingsModal.tsx:1629-1642`), and `ollama_pull` emits no `model:progress`.

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `model:progress` | Emitted by `ollama_pull` (`ollama.rs:299-306`) and by the embedding worker's auto-pull | `ModelProgress` with `id: "ollama:<model>"`, stage `downloading` / `done` / `error`, bytes of the largest layer seen (`ollama.rs:361-367`) | `App.tsx:450` → Settings progress |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `llm.ollama.base_url` | R | No (written by `set_llm_settings`, [app-settings-consent](app-settings-consent.md)) | Custom server URL; non-empty means "user-run, never manage" |
| `llm.provider` | R (`ollama_delete`) | No | In-use check |
| `llm.ollama.model` | R (`ollama_delete`) | No | In-use check |
| env `OLLAMA_MODELS` (set on the child) | W | n/a | `<data_dir>/models/ollama` (`ollama.rs:106`) |

### Database
| Table | Columns | Read / write |
|---|---|---|
| `settings` | the keys above | R |

### Files and directories written
- `<data_dir>/models/ollama/` — pulled model blobs and manifests, written by the managed `ollama serve` child (`ollama.rs:21-23`, `106`). A user-run server keeps models wherever it is configured to.
- The runtime under `<data_dir>/bin/ollama/` is installed by [app-models-gpu](app-models-gpu.md).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `localhost:11434` (default root) | Status (Settings open), `ensure_running` polling (every 300 ms up to 30 s), `/api/tags`, `/api/pull`, `/api/delete` | Loopback |
| The `llm.ollama.base_url` host | Same calls when a custom URL is set | The user set the URL; Keep-local for remote hosts is enforced by the LLM and embedding callers, not here |
| `registry.ollama.ai` and its blob CDN (from the **Ollama child**, not the app) | `ollama_pull` (user click); the embedding worker's automatic pull of `embeddinggemma:300m` (~600 MB) the first time indexing runs (`embeddings.rs:130-140`) | User action for pulls; **none** for the embedding auto-pull beyond Ollama being installed |

`model` is passed through unvalidated (`ollama.rs:332`); an Ollama model name can carry a registry host, so a webview script could make the child fetch from another registry (inferred).

### External processes
| Process | When | Notes |
|---|---|---|
| `<data_dir>/bin/ollama/ollama[.exe] serve`, or `ollama serve` from PATH | `ensure_running` when nothing answers at `http://localhost:11434` and no custom URL is set (`ollama.rs:94-122`) | stdio nulled; `CREATE_NO_WINDOW` on Windows; only `OLLAMA_MODELS` is set, the rest of the environment (for example a global `OLLAMA_HOST`) is inherited (inferred risk: a different bind address or port makes the 30 s wait fail) |

Lifecycle: the `Child` sits in `AppState.ollama`. `shutdown` kills and waits it on `RunEvent::Exit` (`lib.rs:202-211`). A child that died is reaped and respawned on the next `ensure_running` (`ollama.rs:97-102`). There is no Job Object (Windows), process group or parent-death signal, so the child **survives** any exit that skips `RunEvent::Exit`:
- a crash, Task Manager kill or power loss;
- **every Windows auto-update**: tauri-plugin-updater 2.14.0 runs the NSIS installer and calls `std::process::exit(0)` after its `on_before_exit` hook, which only runs `AppHandle::cleanup_before_exit` (`tauri-plugin-updater-2.14.0/src/updater.rs:1009-1050`, `src/lib.rs:119-122`; `tauri-2.12.5/src/app.rs:1128-1140`). `RunEvent::Exit` never fires. The NSIS hook only kills `flyonthewall-mcp.exe`.

An orphan keeps holding RAM/VRAM and port 11434. On the next launch `server_alive` finds it, the app treats it as user-run (`managed: false`), and nothing ever stops it.

### Secrets and keychain entries
None. Ollama needs no key.

## Dependencies
- **Inbound:** `llm_commands.rs` (provider preflight), `embeddings.rs` (worker spawn + auto-pull; query-time liveness), `asr_commands.rs` (`ARTIFACT_ID`), `lib.rs` (registration, exit hook), [ui-settings](ui-settings.md).
- **Outbound:** [app-models-gpu](app-models-gpu.md) (`artifact`, `installed_path`, `find_on_path`, `ModelProgress`, `ProgressSink`), `llm_commands::PROVIDERS` (default model), reqwest, futures-util, tokio, [fly-storage](fly-storage.md) settings.

## Invariants
- **A user-run server is never managed or stopped.** Spawn only when the default root is silent; a custom base URL is never managed; only the child we spawned is killed. Module doc `ollama.rs:1-5`; enforced at `ollama.rs:81-89`, `ollama.rs:136-142`.
- **The managed child must not outlive the app.** `lib.rs:202-211` ("must not outlive the app"). Holds only for a clean exit (see the orphan cases above).
- **Everything the app stores lives under `<data_dir>`**, including pulled models (`ollama.rs:21-23`; `state.rs:135-140` "spec §10").
- **Ollama is installed on demand, never bundled.** DECISIONS.md 2026-07-02 ("Ollama is installed on demand, not bundled"); the 1.5 GB pin is opt-in (`models.rs:103-117`).
- **Local by default.** DECISIONS.md 2026-07-01 ("Ollama is the default provider"). Remote Ollama hosts are handled by the callers' Keep-local rules (`embeddings.rs:166-172`, `llm_commands.rs:539`).
- **Search never spawns a server or pulls on the query path** (`embeddings.rs:78-88`).
- **You can't delete the model the app is using** (`ollama.rs:240-269`).
- **No `ensure_running` lock across awaits.** The `Mutex` guard is dropped before the readiness loop (`ollama.rs:95-122`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/ollama.rs` | 2 | `same_model` tag-insensitive matching; `normalize_root` (`/`, `/v1`, whitespace) |
| vitest | `frontend/src/components/SettingsModal.test.tsx` ("deleting a local model asks first, and Cancel keeps it") | 1 | UI confirm flow and the in-use row has no Delete button; backend replaced by a spy |
| ignored e2e/harness | `src-tauri/tests/llm_bench.rs` | 1 | Talks to a running Ollama for the LLM bench; never spawns the managed server |
| manual | `docs/TESTING.md:72`, `docs/TESTING.md:92` | — | Provider switch incl. Ollama; actionable message with Ollama not running |

**Known gaps:**
- **Child lifecycle is untested.** No test spawns a fake `ollama` (a script that serves `/api/version`) to assert spawn, reap-and-respawn, custom-URL refusal, the 30 s timeout, or that `shutdown` kills it. Risk: the orphan cases above, which are real today.
- **`pull_model` has no test**, although it is `pub` "so tests can drive it" (`ollama.rs:310-311`). Error lines, the success line, partial-line buffering across chunks, and the largest-layer progress rule are unverified. A local `TcpListener` serving canned NDJSON would cover it in CI.
- `ollama_delete`'s in-use refusal and `ollama_status`'s field rules have no backend test; devMock diverges on both.
- No test pins that the embedding worker may spawn the server under a cloud provider (behavior that contradicts the `ensure_running` comment "when the selected provider is 'ollama'", `ollama.rs:77-78`).

## Changing it safely
- Run `cargo test -p fly-app --lib ollama`.
- Keep `OllamaStatus` in step with `frontend/src/types.ts:288-299` and `devMock.ts:1341-1350`.
- Any new spawn path must store the `Child` in `AppState.ollama` so `shutdown` can find it. If you fix the orphan cases: a Windows Job Object with kill-on-close, plus `tauri_plugin_updater::Builder::on_before_exit(|| ollama::shutdown(..))` for the update path.
- Don't call `ensure_running` from a query or UI-latency path; use `server_alive`.
- Don't manage a custom base URL. Users who point at a remote box expect the app to leave it alone.
- Changing `MODELS_SUBDIR` strands every pulled model (they would re-download).
- The progress id prefix `ollama:` is what Settings keys off; change both sides together.
