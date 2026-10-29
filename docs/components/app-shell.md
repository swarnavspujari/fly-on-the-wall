# App shell and composition root (`app-shell`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0); startup gate added for v2.5 (first-launch upgrade progress).
> In flight: None in code. The spec's later Library phase plans to move the asset scope deliberately (spec §2 item 3); not started.

## Purpose
The process entry point and the only place platform implementations are chosen. Resolves the data folder (moving a pre-rebrand `Looma` folder into place first), starts file logging, registers plugins in a fixed order, builds `AppState`, narrows the asset protocol to `recordings/` and `attachments/`, starts the background workers, and registers every Tauri command. Also owns the webview security config: CSP, capabilities, and the navigation guard. When opening the library will run a slow one-time schema step, it opens it off the main thread and reports progress so the window shows an upgrade screen instead of freezing.

## Files it owns
- `src-tauri/src/main.rs` — 6-line shim; `windows_subsystem = "windows"` in release.
- `src-tauri/src/lib.rs` — `run()`: pre-builder startup, plugin order, `setup` (picks the synchronous or background open), command registration (108 commands, `lib.rs:93-202`), Exit hook.
- `src-tauri/src/startup.rs` — the library open on either path: the upgrade check, the background open with progress, the startup gate (`LibraryGate`, `library_status`, `library:upgrade`), the error dialog, and `finish` (everything after the open, in order).
- `src-tauri/src/state.rs` — `AppState`, data-dir resolution, Looma→FlyOnTheWall folder move, legacy keychain copy.
- `src-tauri/src/logging.rs` — tracing subscriber, rolling file logs, panic hook, the log-hygiene rule and its lexical test.
- `src-tauri/src/nav_guard.rs` — the `nav-guard` plugin (`on_navigation`).
- `src-tauri/tauri.conf.json` — CSP, devCsp, asset protocol, updater config, bundle (`externalBin`), window.
- `src-tauri/tauri.updater.conf.json`, `tauri.macos.conf.json`, `tauri.linux.conf.json` — release overrides (updater artifacts; dmg/app; appimage/deb).
- `src-tauri/capabilities/default.json` — webview permissions.
- `src-tauri/installer-hooks.nsi` — NSIS hooks that stop a running `flyonthewall-mcp.exe` before install/uninstall.
- `src-tauri/src/commands.rs` (part) — `ping`, `app_info`, `reveal_data_dir`, `reveal_logs_dir`, `open_external_url`, `is_openable_external_url`.

## Surface area
### Public API
- `pub fn run()` (`lib.rs:29`), called by `main.rs:4`.
- `pub struct AppState` (`state.rs:15-58`), `AppState::init()` (`:61-64`), `AppState::init_with_progress(&mut dyn FnMut(UpgradeProgress))` for the background open (`:66-74`), `AppState::init_with(data_dir, secrets)` for tests (`:90-100`), `secrets_arc()` (`:123-125`). `init` and `init_with_progress` share `prepare()` (data dir + legacy keychain copy, `:76-88`).
- `pub fn prepared_data_dir()` (`state.rs:147-152`); `pub(crate) const LEGACY_KEYCHAIN_SERVICE = "com.looma.notetaker"` (`:131`, also used by `privacy.rs:808`).
- `startup` (crate-private module): `upgrade_pending()` (`startup.rs:89-108`), `open_in_background(AppHandle)` (`:113-150`), `storage_failed(&AppHandle, &anyhow::Error) -> !` (`:152-172`), `finish(&AppHandle, AppState)` (`:177-255`), `LibraryGate` (`:57-68`), `LibraryStatus { phase, done, total }` / `LibraryPhase` (`:27-53`), `UPGRADE_EVENT = "library:upgrade"` (`:25`).
- `logging::init(Option<&Path>)`, `logging::LOGS_DIR = "logs"` (`logging.rs:19`, `:29`).
- `nav_guard::is_app_url`, `nav_guard::init` (`nav_guard.rs:14`, `:32`); `commands::is_openable_external_url` (`commands.rs:575-580`, also used by the nav guard).

**`AppState` fields** (all built in `state.rs:102-119`):
| Field | Type | Who locks / mutates it |
|---|---|---|
| `storage` | `Mutex<Storage>` | Nearly every module: asr_commands, calendar_commands, commands, embeddings, extraction, gpu, import_commands, live, llm_commands, ollama, pipeline, privacy, recording, scheduler, screen_commands, and `lib.rs` startup tasks. The single SQLite connection. |
| `data_dir` | `PathBuf` (immutable) | Read by asr_commands, commands, gpu, import_commands, live, ollama, pipeline, privacy, recording, screen_commands, lib.rs |
| `audio` | `Box<dyn AudioCapture>` (`CpalAudioCapture`) | recording.rs only |
| `recording` | `Mutex<Option<ActiveRecording>>` | Set/taken by recording.rs; read by commands.rs (`delete_note` guard), live.rs, scheduler.rs (`recording_active`, used by privacy.rs) |
| `secrets` | `Arc<dyn SecretStore>` (keychain) | asr_commands, calendar_commands (+ `secrets_arc` for providers), llm_commands, pipeline, privacy (delete-all), state.rs (migration) |
| `pipeline_stage` | `Mutex<HashMap<meeting_id, stage>>` | Written by pipeline.rs and scheduler.rs; read by asr_commands (`pipeline_stage` cmd), commands.rs (`delete_note`), privacy.rs (busy check, cleared on delete-all) |
| `cancel_requests` | `Mutex<HashSet<meeting_id>>` | Inserted by scheduler.rs (`cancel`), polled by pipeline.rs between batches; cleared by scheduler.rs only (when a run ends, cancelled or not, and when `enqueue` creates a new job), so a retry never drops a pending cancel |
| `screen` | `Mutex<Option<ActiveScreenRecording>>` | Set/taken by screen_commands.rs; read by scheduler.rs:469 |
| `jobs_notify` | `tokio::sync::Notify` | Notified by recording.rs, screen_commands.rs, scheduler.rs; awaited by the scheduler worker |
| `ollama` | `Mutex<Option<Child>>` | ollama.rs only (spawn, status, `shutdown` on Exit) |
| `live_status` | `Mutex<Option<LiveStatus>>` | live.rs only |
| `consent_skipped` | `Mutex<BTreeSet<meeting_id>>` | Inserted/retried by scheduler.rs; read/drained by llm_commands.rs:449-463 |
| `consent_prompt` | `tokio::sync::Mutex<ConsentPacing>` | llm_commands.rs only: `request_cloud_consent` holds it while the cloud-consent dialog is open (one dialog at a time) and keeps each provider's recent declines in it |
| `embed_notify` | `tokio::sync::Notify` | Notified by commands.rs, llm_commands.rs, scheduler.rs; awaited by embeddings.rs |
| `foreground_ai` (private) | `AtomicUsize`, via `foreground_ai()` → RAII `ForegroundAi` guard and `foreground_ai_active()` | Guards held by llm_commands.rs (`ask_meeting`, `enhance_note`, `run_polish`) and extraction.rs (`run_extraction`); read by `background_yield_reason()` |

**Background work's one yield rule:** `AppState::background_should_yield()` / `background_yield_reason() -> Option<BackgroundYield { Recording, Transcription, ForegroundAi }>`. It is true while an audio or screen recording runs, while a transcription pipeline stage other than "waiting" runs (polish included), or while a `foreground_ai()` guard is alive (a model request: Ask, Enhance, polish, extraction). Every background job calls it (the embedding backlog today; audio compression next) rather than restating the rule.

Observed lock nesting: `recording` → `storage` (`recording.rs:134-140`), `recording` → `screen` (`scheduler.rs:469`), `consent_prompt` → `storage` (`llm_commands.rs:294-308`). No reverse order was found.

**Webview security config**
- **CSP** (`tauri.conf.json:36`): `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' asset: http(s)://asset.localhost data: blob:; media-src 'self' asset: http(s)://asset.localhost blob:; font-src 'self' data:; connect-src 'self' ipc: http(s)://ipc.localhost; object-src 'none'; base-uri 'self'; form-action 'none'; frame-src 'none'; frame-ancestors 'none'; worker-src 'self' blob:`.
- **devCsp** (`:37`) adds `'unsafe-inline'` + `http://localhost:1420` to `script-src` and `ws://`/`http://localhost:1420` to `connect-src`.
- `dangerousDisableAssetCspModification: ["style-src"]` (`:38`) keeps Tauri from adding nonces to `style-src`, so `'unsafe-inline'` styles work.
- **Asset protocol** enabled with an empty static scope (`:39-42`); the two folders are added at runtime (`startup.rs:182-186`). Cargo feature `protocol-asset`. One more file at a time joins it: a compressed recording's rebuilt playback mix in scratch (`audio_store::prepare_playback` calls `allow_file` on exactly that file; scratch as a whole stays out).
- **Capabilities** (`capabilities/default.json:6`), window `main` only: `core:default`, `updater:default`, `process:allow-restart`. No `fs`, `shell`, `http`, `dialog`, `opener` or `clipboard` permissions: dialogs, opening and clipboard go through Rust commands (DECISIONS.md "M1 — Native dialogs/openers via Rust plugin APIs").
- **Navigation guard** (`nav_guard.rs:32-48`): allows `tauri:`, `asset:`, `ipc:`, `about:blank`, `http(s)://tauri.localhost`, and `localhost`/`127.0.0.1:1420` in debug builds only; anything else is blocked, and http(s)/mailto targets are handed to the system browser.

**Plugin order** (`lib.rs:44-76`)
1. `single-instance` (desktop; must be first so a second launch focuses the existing window).
2. `dialog`, 3. `opener`, 4. `nav-guard`, 5. `clipboard-manager`.
6. In `setup`: `updater`, `process` (desktop).

**Startup sequence**
Before the builder (`lib.rs:35-42`): `prepared_data_dir()` (legacy move) → `privacy::startup` (instance lock, finish a pending "Delete all my data") → `logging::init`.
In `setup` (`lib.rs:69-92`), after the two plugins: `startup::upgrade_pending()` asks `Storage::pending_upgrade` (read-only) whether the open will run the one-time schema step. Any error counts as "no".
- **No upgrade (every normal launch): synchronous, as before.** `manage(LibraryGate::ready())` → `AppState::init` (on failure: `storage_failed`, the error dialog, `exit(1)`) → `finish`.
- **Upgrade pending: background.** `open_in_background` manages an `upgrading` gate and returns from `setup` at once, so the window paints. On a `spawn_blocking` thread: `AppState::init_with_progress` (the same open with retry, reporting `UpgradeProgress`) → each report updates the gate and emits `library:upgrade` → `finish` → gate `ready` + event. On failure: gate `failed` + event, then the same `storage_failed` dialog and exit.

`finish` (`startup.rs:177-255`), the same on both paths and in today's order: asset scope → `migrate_anthropic_model`, `reset_unseen_cloud_consent`, `record_pending_wipe` (under the storage lock) → disk-encryption check → `manage(AppState)` → off-thread: hardware detection, stale-download sweep (1 h), recording self-heal → `scheduler::spawn`, `embeddings::spawn`, `privacy::spawn_retention`, `audio_store::spawn` (recording compression, its own low-priority thread, first pass ~2 min after launch).

**Before `AppState` exists** (background path only), the window is already up. Commands taking `State<AppState>` return Tauri's "state not managed" error rather than panicking, and the frontend mounts nothing that calls them until `library_status` says `ready` (see [ui-app-shell](ui-app-shell.md)). The single-instance callback only touches the window. No tray, menu or window-event handler exists. The Exit hook uses `try_state`: quitting mid-upgrade just exits, and the step's transaction rolls back and re-runs on the next launch.

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `library_status` | — (takes `AppHandle`, never `State<AppState>`, so it works before the library is open) | `LibraryStatus { phase: "ready" \| "upgrading" \| "failed", done, total }` (`startup.rs:72-84`). `done`/`total` are progress units, both 0 until the first report. | `ui-app-shell` (`LibraryGate.tsx`, before `<App/>` mounts), via `api.libraryStatus` |
| `ping` | — | `String` ("pong") | None (devMock only) |
| `app_info` | — | `Result<AppInfo { version, data_dir, os, appimage }, String>` | `ui-app-shell` (`App.tsx:259`, gates auto-update), `ui-settings` (`SettingsModal.tsx:377`), via `api.appInfo` |
| `reveal_data_dir` | — | `Result<(), String>` | `ui-settings` (`SettingsModal.tsx:2093`), `ui-editor` (`Editor.tsx:1264`, fallback), via `api.revealDataDir` |
| `reveal_logs_dir` | — | `Result<(), String>` (creates `logs/` first) | `ui-settings` (`SettingsModal.tsx:2228`), via `api.revealLogsDir` |
| `open_external_url` | `url: String` | `Result<(), String>` (http, https, mailto only; ≤ 4096 chars) | `ui-editor` (`safeContent.ts:39` `openExternal`, used by the app-wide link guard installed from `main.tsx` and by the markdown renderers), direct `invoke` (not in `api.ts`; devMock has no case) |

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `library:upgrade` | Emits (`startup.rs:64-67`, from `open_in_background` only) | `LibraryStatus` (same shape as `library_status`) on every throttled progress report (about every 100 ms), then once `ready` or `failed` | `ui-app-shell` (`LibraryGate.tsx`, listens before it calls `library_status`) |
| (Tauri) `RunEvent::Exit` | Listens (`lib.rs:202-211`) | — | Calls `ollama::shutdown` only, and only when `AppState` exists (`try_state`) |

### Settings keys
None read or written directly. Startup calls `llm_commands::migrate_anthropic_model`, `reset_unseen_cloud_consent` and `privacy::record_pending_wipe` (`startup.rs:187-195`), which own their keys (see [app-settings-consent](app-settings-consent.md), [app-llm](app-llm.md), [app-privacy](app-privacy.md)).

### Database
None directly. `AppState::init_with` opens `flyonthewall.db` with `Storage::open_with_retry(.., OpenRetry::STARTUP)` (`state.rs:97`). The background path uses `Storage::open_with_retry_and_progress` (`state.rs:70-71`) after `Storage::pending_upgrade` (a read-only peek, `startup.rs:89-108`). See [fly-storage](fly-storage.md).

### Files and directories written
- `<data_dir>` = `dirs::data_dir()/FlyOnTheWall` (`%APPDATA%\FlyOnTheWall` on Windows), `./FlyOnTheWall` if the OS has no data dir (`state.rs:137-141`).
- Renames `<os data dir>/Looma` → `<data_dir>` once, only when the new folder doesn't exist (`state.rs:157-172`).
- `<data_dir>/logs/flyonthewall.<date>.log`, rotated daily, newest 5 kept (`logging.rs:19-23`, `:73-82`).
- Creates `<data_dir>/recordings/` and `<data_dir>/attachments/` at startup (`startup.rs:182-186`).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `github.com/swarnavspujari/fly-on-the-wall/releases/latest/download/latest.json` | Updater check (plugin registered here, `lib.rs:70-76`; config `tauri.conf.json:12-21`) | Driven by the UI; see [app-updater](app-updater.md) |

Startup itself makes no requests. Background workers started here (embeddings → Ollama on localhost, retention, self-heal) are owned by their modules.

### External processes
- System browser / file manager via `tauri-plugin-opener` (`open_external_url`, `reveal_*`).
- `taskkill /F /IM flyonthewall-mcp.exe` from the NSIS installer hooks (`installer-hooks.nsi`).
- Indirect: `hw::detect_and_cache` (nvidia-smi) on a blocking thread at startup (`startup.rs:198-209`).

### Secrets and keychain entries
- New service `com.flyonthewall.app`, legacy service `com.looma.notetaker`.
- On **every** launch, `copy_secrets` copies 7 keys from the legacy service into the new one when the new one lacks them (`state.rs:76-88`, `:176-199`): `openai_api_key`, `anthropic_api_key`, `nim_api_key`, `groq_api_key`, `google_oauth_token`, `ms_oauth_token`, `google_oauth_client_secret`. Failures are swallowed.

## Dependencies
- **Inbound:** `main.rs`; every `app-*` module reads `AppState`; integration tests build state with `AppState::init_with` (`src-tauri/tests/scheduling.rs:40`, `pipeline_e2e.rs:136`, `rediarize_e2e.rs:91`).
- **Outbound:** all `app-*` modules (registration), [fly-storage](fly-storage.md), [fly-secrets](fly-secrets.md), [fly-audio](fly-audio.md) (`CpalAudioCapture`), Tauri plugins (single-instance, dialog, opener, clipboard-manager, updater, process), tracing-subscriber / tracing-appender.

## Invariants
- Platform impls are chosen only here (`lib.rs:1-2`, `state.rs:20-21`, `:106`). Source: ARCHITECTURE.md golden rule.
- The data dir is migrated before anything creates files in it (`lib.rs:30-42`, `state.rs:143-152`).
- Asset protocol scope stays `recordings/` + `attachments/` (`startup.rs:182-186`, `tauri.conf.json:39-42`). Source: spec §2 item 3; THREAT_MODEL T6.
- CSP and nav guard unchanged (`tauri.conf.json:36`, `nav_guard.rs`, `lib.rs:67`). Source: spec §2 item 3; THREAT_MODEL T2, T5.
- Webview capabilities stay minimal: no fs/shell/http plugins exposed to JS (`capabilities/default.json:6`). Source: THREAT_MODEL T2.
- Only http, https and mailto links leave the app, re-validated in Rust (`commands.rs:575-591`). Source: THREAT_MODEL T1, T5.
- A database that won't open shows a dialog and exits; never a silent crash. Both paths reach the same `storage_failed` (`startup.rs:152-172`).
- The background open runs only when `Storage::pending_upgrade` says the schema step will run; every other launch keeps the synchronous open inside `setup` (`lib.rs:80-90`). Both paths run the same `finish`, in the same order.
- Nothing that can run before `finish` calls `app.state::<AppState>()` (it panics when unmanaged). Use `try_state`, as the Exit hook does. Commands may take `State<AppState>`: unmanaged, they return an error instead.
- No user content in logs: no titles, folder names, transcript/note/prompt text, attendee names or emails, calendar ids, tokens or keys (`logging.rs:6-13`).
- The managed `ollama serve` child never outlives the app (`lib.rs:202-211`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/state.rs` | 4 | Legacy folder move (moves, never clobbers, fresh-install no-op); legacy secret copy fills only missing keys |
| unit | `src-tauri/src/startup.rs` | 1 | `LibraryStatus` JSON shape (`phase` lowercase, `done`, `total`), which `LibraryGate.tsx` reads |
| unit | `src-tauri/src/logging.rs` | 3 | Appender writes into `logs/`; lexical scan of every log call in `src-tauri/src` and `crates/`; panic payload text |
| unit | `src-tauri/src/nav_guard.rs` | 3 | `is_app_url` allow/deny lists; `is_openable_external_url` |
| integration | `src-tauri/tests/scheduling.rs` (and others) | — | Use `AppState::init_with` with a temp dir + `MemorySecretStore` |

**Known gaps:**
- **CSP is never tested.** No test reads `tauri.conf.json`; a PR that adds `'unsafe-inline'` to `script-src` or a remote `connect-src` passes CI (THREAT_MODEL T1/T2).
- **Capabilities are never asserted.** Adding `fs:default` or `shell:allow-open` to `capabilities/default.json` passes CI (T2).
- **Asset scope is never asserted.** Nothing checks that only the two folders are allowed (T6).
- The nav-guard tests call `is_app_url` only. Nothing checks the plugin is registered (`lib.rs:66`), that `on_navigation` returns `false`, that `localhost:1420` is refused in release builds (tests compile with `debug_assertions`), or what happens to new-window requests. `asset:` URLs are allowed unconditionally (`nav_guard.rs:17`), so on macOS/Linux an HTML file under `attachments/` could load in the app window if a link ever got past the frontend guard (inferred).
- The log-hygiene test is lexical. It passes today while `crates/fly-calendar/src/msgraph.rs:236` logs a calendar id (`cal.id`, the ban list has `cal_id`) and `crates/fly-capture-screen/src/lib.rs:73` logs an error that can embed a window title. It can't see user content inside `{e}` errors. Rotation, the 5-file cap and the panic hook are untested.
- **Deleted secrets come back (inferred).** The legacy keychain copy runs on every launch and fills any missing key, so for a pre-rebrand user a disconnected calendar or a removed API key is restored at the next start. The test pins "fills missing" and has no "stays deleted" case.
- **Exit doesn't stop captures.** `RunEvent::Exit` only stops Ollama; an audio or screen capture running at quit isn't stopped or finalized here (inferred; untested).
- No test checks that `lib.rs` registers every command `api.ts` calls (or the reverse).
- `ping` has no UI caller.
- **The background open path has no automated test** (it needs a Tauri runtime). The pieces are covered: the peek and the progress stream in `crates/fly-storage/tests/migration_v3.rs`, and the gate in `frontend/src/components/LibraryGate.test.tsx`. The end-to-end path was checked by hand against a synthetic v2 library.

## Changing it safely
- A new command: register in `lib.rs`, wrap in `frontend/src/api.ts`, mirror in `devMock.ts` (spec §2 item 4), and add one row to exactly one `app-*.md`.
- A new `AppState` field: initialize it in `init_with` (tests build state through it) and document who locks it. Keep lock order `recording` → `storage` / `screen`.
- Touching CSP, capabilities, the asset scope or the nav guard is a security change: re-check THREAT_MODEL T1/T2/T5/T6, and test in a release build on Windows and macOS (dev builds use `devCsp` and allow `localhost:1420`).
- Anything that creates files in the data dir must run after `prepared_data_dir()`.
- New log calls: ids, counts, sizes only. Run `cargo test -p fly-app logging` (the lexical check also scans `crates/`).
- Startup work belongs off the main thread (`spawn_blocking`), like the existing tasks.
- Startup work that needs the library or `AppState` goes in `startup::finish`, never directly in `setup`: on the upgrade path `setup` returns before the library is open.
- A new command the frontend calls before `<App/>` mounts must not take `State<AppState>` (see `library_status`).
