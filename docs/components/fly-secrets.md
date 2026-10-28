# Secrets (`fly-secrets`)

> Layer: crate. Described at main `82b7edf` (v2.4.0).
> In flight: None.

## Purpose
The single place API keys and OAuth tokens are stored. It defines the `SecretStore` trait, the OS-keychain implementation (Windows Credential Manager, macOS Keychain, Linux Secret Service via `keyring` 3), an in-memory store for tests, and the well-known key names. Nothing secret is written to disk in plaintext and values are never logged.

## Files it owns
- `crates/fly-secrets/src/lib.rs` — trait, `KeychainSecretStore`, `MemorySecretStore`, `keys` module, one unit test.
- `crates/fly-secrets/Cargo.toml` — depends on `keyring` (workspace: `version = "3"`, features `windows-native`, `apple-native`, `sync-secret-service`, `Cargo.toml:48`; locked at 3.6.3).

## Surface area
### Public API
| Item | What it is |
|---|---|
| `trait SecretStore: Send + Sync` | `get(key) -> Result<Option<String>>`, `set(key, value)`, `delete(key)` (`lib.rs:42-46`) |
| `KeychainSecretStore::new()` / `Default` | Service `com.flyonthewall.app` (`lib.rs:54-56`) |
| `KeychainSecretStore::with_service(name)` | Any service name; used for the pre-rebrand service (`lib.rs:60-64`) |
| `MemorySecretStore` | `Mutex<HashMap>`; tests only by convention (it's `pub`, not `cfg(test)`) (`lib.rs:102-120`) |
| `SecretError::{NotFound, Keychain}`, `Result` | `NotFound` is defined but never returned: a missing entry is `Ok(None)` on get and `Ok(())` on delete (`lib.rs:79-97`) |
| `keys::{OPENAI_API_KEY, ANTHROPIC_API_KEY, NIM_API_KEY, GROQ_API_KEY, GOOGLE_OAUTH_TOKEN, MS_OAUTH_TOKEN, ALL}` | Account names (`lib.rs:21-40`) |

### Tauri commands
None (crates expose no commands). Keys are entered through `set_llm_settings` ([app-settings-consent](app-settings-consent.md)), `set_asr_settings` ([app-pipeline](app-pipeline.md)), `set_calendar_settings` / `connect_calendar` / `disconnect_calendar` ([app-calendar](app-calendar.md)), and erased by `delete_all_data` ([app-privacy](app-privacy.md)).

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| None | — | — | — |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| None | — | — | Secrets never go in the `settings` table. Non-secret companions (`llm.<provider>.base_url`, `calendar.google.client_id`, …) are owned by the app docs. |

### Database
| Table | Columns | Read / write |
|---|---|---|
| None | — | — |

### Files and directories written
None directly. The OS keychain persists entries in its own store.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None | — | — |

### External processes
None. On Linux the Secret Service is reached over D-Bus by `keyring` (inferred from the `sync-secret-service` feature).

### Secrets and keychain entries
Every entry in the workspace. Service = `com.flyonthewall.app` unless noted; the account is the key name. On Windows, `keyring` 3 stores a generic credential whose target is `<account>.<service>`, e.g. `groq_api_key.com.flyonthewall.app` (inferred from `keyring` defaults).

| Account (key) | Holds | Written by | Read by | Deleted by |
|---|---|---|---|---|
| `openai_api_key` | OpenAI API key | `set_llm_settings` (`src-tauri/src/llm_commands.rs:877-889`) | `build_provider` (`llm_commands.rs:306-334`), `get_llm_settings` `has_key` only (`llm_commands.rs:797-798`) | empty key in `set_llm_settings`; delete-all |
| `anthropic_api_key` | Anthropic API key | same | same; also `src-tauri/tests/llm_bench.rs:181-183` (ignored harness, real keychain) | same |
| `nim_api_key` | NVIDIA NIM API key | same | same | same |
| `groq_api_key` | Groq ASR key | `set_asr_settings` (`src-tauri/src/asr_commands.rs:385-397`) | pipeline (`src-tauri/src/pipeline.rs:294`), `get_asr_settings` `has_groq_key` (`asr_commands.rs:299-304`) | empty key in `set_asr_settings`; delete-all |
| `google_oauth_token` | Google OAuth token set (JSON) | `GoogleCalendarProvider::connect` and refresh (`crates/fly-calendar/src/google.rs:55-58`, `142-147`) | `google.rs:39-44`, `133-137`; `get_calendar_settings` (`src-tauri/src/calendar_commands.rs:150-155`), `is_connected` (`calendar_commands.rs:247-249`) | `disconnect_calendar` (`calendar_commands.rs:241-245`), `google.rs:151-153`; delete-all |
| `ms_oauth_token` | Microsoft Graph token set (JSON) | `crates/fly-calendar/src/msgraph.rs:53-56`, `196-199` | `msgraph.rs:39-40`, `187-190`; `calendar_commands.rs:158-163` | `disconnect_calendar`; `msgraph.rs:205-207`; delete-all |
| `google_oauth_client_secret` | User-supplied Google OAuth client secret (not in `keys`; `calendar_commands.rs:24`) | `set_calendar_settings` (`calendar_commands.rs:190-202`) | `effective_google_secret` (`calendar_commands.rs:45-52`), `get_calendar_settings` (`calendar_commands.rs:144-149`) | empty value in `set_calendar_settings`; delete-all |

Legacy service `com.looma.notetaker` (`src-tauri/src/state.rs:107`): at every `AppState::init` the 7 accounts above are copied into the new service when the new one lacks them (`state.rs:57-69`, `152-175`); the legacy entries are never deleted except by "Delete all my data", which deletes all 7 from both services (`src-tauri/src/privacy.rs:526-546`, `808`, `846`).

The MCP binary (`fly-mcp`) does not depend on this crate (spec §2 item 5).

## Dependencies
- **Inbound:** `fly-app` (`src-tauri/src/state.rs:9`, `25`, `59`; `llm_commands.rs`, `asr_commands.rs`, `pipeline.rs`, `calendar_commands.rs`, `privacy.rs`); `fly-calendar` (`google.rs:6`, `msgraph.rs:7`, providers hold `Arc<dyn SecretStore>`). Tests across the workspace use `MemorySecretStore`.
- **Outbound:** `keyring` 3.6.3 (platform stores), `thiserror`, `tracing` (declared; unused in `lib.rs`).

## Invariants
- Every API key and OAuth token goes through `SecretStore`; no plaintext secrets on disk (ARCHITECTURE.md "Secrets (fly-secrets)", `ARCHITECTURE.md:56-59`; enforced by construction: `state.rs:25`, no secret keys in `settings`).
- Secret values are never logged; only key names (`lib.rs:3-5`; e.g. `src-tauri/src/privacy.rs:538` logs the key name only).
- The webview never receives a stored secret: settings commands return `has_key` / `has_groq_key` / `google_has_secret` booleans (`llm_commands.rs:797`, `asr_commands.rs:299`, `calendar_commands.rs:144`).
- The MCP server holds no keys (spec §2 item 5; `crates/fly-mcp/Cargo.toml` has no `fly-secrets`).
- "Delete all my data" removes every owned entry from the current and legacy services (THREAT_MODEL T13; `privacy.rs:526-546`). The owned list is `keys::ALL` plus the Google client secret, assembled by hand in two places (`privacy.rs:526-530`, `state.rs:152-163`).
- Where a stored key may be sent is decided by the LLM layer's base-URL rules, not here (THREAT_MODEL T4; [app-llm](app-llm.md)).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `crates/fly-secrets/src/lib.rs` | 1 | `MemorySecretStore` round trip only |
| unit (consumers) | `src-tauri/src/state.rs` | 1 | `copy_secrets` fill-missing / never-overwrite (memory stores) |
| unit (consumers) | `src-tauri/src/privacy.rs` | 2 | owned keys deleted; delete-all clears both stores (memory stores) |
| integration (consumers) | `crates/fly-calendar/tests/connect_flow.rs` | — | token get/set/delete through the trait (memory store) |
| ignored e2e/harness | `src-tauri/tests/llm_bench.rs` | — | reads the real keychain for the Anthropic key; never in CI |

**Known gaps:**
- `KeychainSecretStore` has no test at all: the `NoEntry` → `Ok(None)` / `Ok(())` mapping, the service name, and the `keyring` feature set are unverified on every OS. A `keyring` upgrade (the `Cargo.toml:47` comment notes v4 splits backends into separate crates) could silently fall back to a non-persistent store (inferred: `keyring` 3 without a platform feature uses its mock store).
- No test pins that the owned-key list (`privacy.rs:526`) covers every account used in the code; adding a new secret means editing `keys::ALL` or both hand-built lists, and nothing fails if you forget.
- Legacy-service cleanup after migration doesn't exist (entries linger until delete-all) and isn't tested.

## Changing it safely
- Run `cargo test -p fly-secrets -p fly-calendar -p fly-app`; `cargo clippy --workspace --all-targets -D warnings`.
- New secret: add the constant to `keys`, and to `keys::ALL` so delete-all erases it; if it predates nothing, don't add it to `migrated_secret_keys` (`state.rs:152`). Keep returning booleans, never values, to the webview.
- Never log values; never add secrets to `settings` or devMock fixtures with real-looking keys.
- Changing the service name strands every user's keys unless a copy step like `copy_secrets` (`state.rs:167`) runs first.
- Upgrading `keyring` (v4 splits platform stores into separate crates): re-check that each OS still gets a persistent backend, ideally with a real-keychain smoke test on Windows and macOS runners.
