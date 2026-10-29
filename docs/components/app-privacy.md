# Privacy, retention and delete-all (`app-privacy`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: None in `src-tauri/src/privacy.rs`. PR #92 changes the fly-storage side it calls (`privacy.rs` there gains SQL keep-local filtering for `vector_search`; see [fly-storage](fly-storage.md)).

## Purpose
The command surface for the app's privacy controls: per-note and per-folder **Keep local** flags (read/write only; enforcement lives in `build_provider`, the pipeline's Groq gate and the embedding worker), the **participants notified** record and recording-notice clipboard helper, **audio retention** (a background sweep plus an explicit sweep from Settings), **Delete all my data** (in-place DB erase, immediate content-folder delete, keychain wipe, and a restart-time wipe of the rest), and a best-effort **disk-encryption** probe for the Settings banner.

## Files it owns
- `src-tauri/src/privacy.rs` — the 12 commands, retention worker, `ImportGuard`, delete-all (marker, instance lock, startup wipe, report), disk-encryption probe, 16 unit tests.

Recording storage v2 ([app-audio-store](app-audio-store.md)) interplay: the retention sweep treats the meeting the compression job holds as busy (like a running pipeline), including a folder it has leased but not yet announced (checked under the same storage lock); compressed files keep their WAV's modified time, so retention ages them the same; `.flac` was already a media extension. `delete_all_data` halts the compression job (and waits for it to let go of its folder) before wiping, and resumes it when the wipe is refused or fails.

## Surface area
### Public API
Called by other src-tauri modules:

| Item | Caller |
|---|---|
| `startup(data_dir)` — take the per-data-dir instance lock; finish a pending wipe before logging/DB touch the folder (`privacy.rs:672`) | `src-tauri/src/lib.rs:40` |
| `record_pending_wipe(&Storage)` — persist the startup wipe's leftovers/notes (`privacy.rs:761`) | `startup.rs:194` |
| `spawn_disk_encryption_check(data_dir)` (`privacy.rs:1056`) | `startup.rs:196` |
| `spawn_retention(app)` — first sweep after 60 s, then every 24 h (`privacy.rs:281`) | `startup.rs:254` |
| `ImportGuard::enter()` — marks an import in flight so delete-all refuses (`privacy.rs:383-401`) | `src-tauri/src/import_commands.rs:446`, `606` |
| `sweep_retention(storage, now, busy, explicit) -> SweepReport` (`privacy.rs:175`) | internal + tests |
| `delete_owned_secrets(stores)`, `wipe_data_dir(dir, keep_models, only)`, `classify_bitlocker`, `classify_fdesetup`, `windows_drive` | internal + tests |
| `WIPE_MARKER`, `WIPE_CONFIRMATION` (`"delete everything"`), types `SweepReport`, `PrivacySettings`, `WipeReport`, `DiskEncryption`, `EncryptionState` | serialized to the UI |

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `get_meeting_privacy` | `meeting_id: String` | `Result<MeetingPrivacy, String>` (`keep_local`, `keep_local_from_folder`, `participants_notified`, `audio_deleted_at`) | `ui-editor` (`Editor.tsx:1034`), `ui-recording-bar` (`RecordingBar.tsx:64`), via `api.getMeetingPrivacy` |
| `set_note_keep_local` | `note_id: String, keep_local: bool` | `Result<(), String>` (errors on unknown note; sets every meeting of the note) | `ui-editor` (`Editor.tsx:1050`), via `api.setNoteKeepLocal` |
| `set_folder_keep_local` | `folder_id: String, keep_local: bool` | `Result<(), String>` | `ui-home` (`Sidebar.tsx:222`), via `api.setFolderKeepLocal` |
| `list_local_only_folders` | — | `Result<Vec<String>, String>` (folders flagged themselves) | `ui-home` (`Sidebar.tsx:196`), via `api.listLocalOnlyFolders` |
| `set_participants_notified` | `meeting_id: String, notified: bool` | `Result<(), String>` | `ui-recording-bar` (`RecordingBar.tsx:90`), via `api.setParticipantsNotified` |
| `copy_recording_notice` | `text: String` (1–2000 chars) | `Result<(), String>` | `ui-recording-bar` (`RecordingBar.tsx:79`), via `api.copyRecordingNotice` |
| `get_privacy_settings` | — | `Result<PrivacySettings, String>` (`audio_retention_days`, `retention_choices`, `retention_held`, `last_wipe`) | `ui-settings` (`PrivacySection.tsx:51`), via `api.getPrivacySettings` |
| `preview_audio_retention` | `days: u32` (7/30/90/365) | `Result<usize, String>` (meetings that would lose audio now) | `ui-settings` (`PrivacySection.tsx:98`), via `api.previewAudioRetention` |
| `set_audio_retention` | `days: Option<u32>` | `Result<SweepReport, String>` (saves, then runs an explicit sweep) | `ui-settings` (`PrivacySection.tsx:70`), via `api.setAudioRetention` |
| `dismiss_wipe_report` | — | `Result<(), String>` | `ui-settings` (`PrivacySection.tsx:298`), via `api.dismissWipeReport` |
| `delete_all_data` | `include_models: bool, confirmation: String` (must be `"delete everything"`) | `Result<WipeReport, String>` | `ui-settings` (`PrivacySection.tsx:111`); retry from `ui-notices` (`RestartRequired.tsx:75`); via `api.deleteAllData` |
| `recovered_kept_local` | — | `Vec<RecoveredMeeting{meeting_id, note_id, title}>`: meetings self-heal restored this launch and set to Keep local because their manifest didn't record the setting (`AppState::recovered_kept_local`, in memory) | `ui-notices` (`useRecoveredKeptLocal.ts`), via `api.recoveredKeptLocal` |
| `dismiss_recovered_kept_local` | `meeting_id: Option<String>` (`meetingId`; none = all) | `Vec<RecoveredMeeting>` (what's left). Doesn't change Keep local | `ui-notices` (Review takes the first off and opens its note; Dismiss clears all), via `api.dismissRecoveredKeptLocal` |
| `disk_encryption_status` | — | `Result<DiskEncryption, String>` (`state` on/off/unknown, `method`, `volume`) | `ui-settings` (`PrivacySection.tsx:55`), via `api.diskEncryptionStatus` |

All 14 are registered in `src-tauri/src/lib.rs` (`recovered_kept_local` / `dismiss_recovered_kept_local` mirrored in devMock, with `fotwMockRecoveredKeptLocal = "1"` seeding one entry). The original 12 are at `src-tauri/src/lib.rs:188-199` and mirrored in `frontend/src/devMock.ts:1420-1481`. Drift: the mock's `delete_all_data` rejects a wrong phrase with `"confirmation mismatch"` (backend: `"confirmation text doesn't match — nothing was deleted"`), never refuses for busy work, and always returns `last_wipe: null`; `set_note_keep_local` doesn't error on an unknown note.

### Events
One Tauri event: `privacy:recovered-kept-local` (`RECOVERED_KEPT_LOCAL_EVENT`), emitted by the startup self-heal in `startup.rs` (`startup::finish`) when it kept meetings local without a recorded setting, payload the whole current `RecoveredMeeting[]`; listened to by `useRecoveredKeptLocal.ts`. Two frontend `window` events carry the rest of the feature in the UI:

| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `fotw:restart-required` (window, not Tauri) | emitted by `PrivacySection.tsx:120` after `delete_all_data` returns | `{ report: WipeReport, includeModels }` | `RestartRequired.tsx:33` (blocks the app, auto-relaunches only on a clean report), `SettingsModal.tsx:413` (closes) |
| `fotw:privacy-changed` (window, not Tauri) | emitted by `Editor.tsx:1051`, `Sidebar.tsx:223` after a keep-local write | none | `Editor.tsx:1038` re-reads `get_meeting_privacy` |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `privacy.audio_retention_days` | R/W through fly-storage (`privacy.rs:360`, storage `privacy.rs:450-468`) | No | Retention days; absent = keep forever |
| `privacy.retention_last_sweep` | R/W (`privacy.rs:140`, `186-191`, `224-226`) | No | RFC 3339 time of the last sweep; clock-skew guard |
| `privacy.retention_held` | R/W (`privacy.rs:143`, `215`, `223`) | No | Meetings a background sweep held back (bulk guard) |
| `privacy.last_wipe_report` | R/W (`privacy.rs:450`, `775`, `370-377`) | No | JSON `WipeReport` of what the restart wipe left |
| `ui.privacy.disk_encryption_banner_dismissed` | frontend only, via `get_app_setting` / `set_app_setting` (`frontend/src/privacy.ts:8`) | Yes (`ui.`) | Banner dismissed; not read by `privacy.rs` |

### Database
Through [fly-storage](fly-storage.md) only:

| Table | Columns | Read / write |
|---|---|---|
| `meetings` | `keep_local`, `participants_notified`, `audio_deleted_at`, `recording_json`, `ended_at` | R/W |
| `folders` | `keep_local`, `parent_id` | R/W |
| `notes` | `folder_id` | R |
| `transcription_jobs` | `status` | R (busy check, retention skip) |
| `settings` | keys above | R/W |
| every table | — | erased by `delete_all_data` (`erase_all_content`) |

### Files and directories written
| Path | When |
|---|---|
| `<data_dir>/.instance.lock` | Created and OS-locked for the process lifetime by `startup` (`privacy.rs:633-665`) |
| `<data_dir>/.delete-all-pending.json` | Written first by `delete_all_data` (`privacy.rs:824-832`): `keep_models`, `created_at`, `pid`, `consumed` |
| `<data_dir>/.delete-all-wiping.json` | The marker renamed at startup while the wipe runs; rewritten as `consumed` if it can't be removed (`privacy.rs:702-757`) |
| `<data_dir>/recordings`, `notes`, `attachments`, `transcripts` | Deleted immediately by `delete_all_data` (`privacy.rs:782`, `843`) |
| everything else in `<data_dir>` (DB files, `logs/`, and `models/` + `bin/` unless kept) | Deleted by the restart wipe, with 5 retries 800 ms apart (`privacy.rs:586-630`, `731`) |
| `<scratch_root>` | Removed inside `erase_all_content` (fly-storage `privacy.rs:414`) — only if the DB erase got that far |
| media files in `<data_dir>/recordings/<folder>/` and that meeting's `<scratch_root>/<id>/` | Retention sweep, via fly-storage `delete_meeting_audio` |

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None | — | — |

### External processes
| Process | OS | When | Limits |
|---|---|---|---|
| `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe -NoProfile -NonInteractive -Command "(New-Object -ComObject Shell.Application).NameSpace('<X>:').Self.ExtendedProperty('System.Volume.BitLockerProtection')"` | Windows | once per session (startup check or first `disk_encryption_status`) | absolute path, validated drive letter (`privacy.rs:917-924`), `CREATE_NO_WINDOW`, 10 s then killed (`privacy.rs:928-987`) |
| `/usr/bin/fdesetup isactive` | macOS | same | 10 s then killed (`privacy.rs:990-1028`) |
| none | Linux | — | always `unknown` (`privacy.rs:1031-1037`) |

The result (including `unknown`) is cached for the session under a mutex held across the probe (`privacy.rs:1042-1053`). `copy_recording_notice` writes the system clipboard through `tauri-plugin-clipboard-manager` (`privacy.rs:123-129`).

### Secrets and keychain entries
`delete_all_data` deletes `keys::ALL` + `google_oauth_client_secret` (7 accounts) from `com.flyonthewall.app` (`state.secrets`) and from the legacy `com.looma.notetaker` service (`privacy.rs:526-546`, `808`, `846`). Failures are listed in `WipeReport.secrets_failed`. See [fly-secrets](fly-secrets.md).

## Dependencies
- **Inbound:** `lib.rs` (startup order, registration), `import_commands.rs` (`ImportGuard`); the UI components above.
- **Outbound:** [fly-storage](fly-storage.md) (`MeetingPrivacy`, keep-local, retention, `erase_all_content`), [fly-secrets](fly-secrets.md), `crate::state::AppState` (`storage`, `secrets`, `pipeline_stage`, `data_dir`), `crate::scheduler::recording_active`, `crate::calendar_commands::GOOGLE_CLIENT_SECRET_KEY`, `tauri-plugin-clipboard-manager`, `tokio` timers, std file locking (`File::try_lock`).

## Invariants
- **T10 (cloud without the user realizing):** Keep local is enforced where providers are chosen, not here: `build_provider` refuses cloud LLMs for keep-local notes/meetings (`src-tauri/src/llm_commands.rs:605-613`), the pipeline skips Groq (`src-tauri/src/pipeline.rs:148-149`, `274`), and the embedding worker uses the shareable queue (`src-tauri/src/embeddings.rs:176`, `202`). Lookups fail closed (fly-storage `privacy.rs:209-241`). A folder flag can't be overridden per note (UI: `Editor.tsx:1045`). Spec §2 item 1.
- Self-heal fails closed on Keep local (fly-storage), and the meetings it set that way are surfaced, not just logged: `startup.rs` stores `HealReport::kept_local_unconfirmed` in `AppState::recovered_kept_local` and emits the event; the review notice reads it.
- **T11 (deleted data recoverable from the DB):** `erase_all_content` deletes every row, runs `VACUUM`, and reports `checkpointed: false` when a reader blocks the WAL truncate; the report is then not `clean()` and the UI won't auto-restart (`privacy.rs:854-870`, `516-523`; `RestartRequired.tsx:59-66`).
- **T12 (consent of other participants):** the participants-notified flag and the recording notice (≤2000 chars, `privacy.rs:113-129`) are records/helpers only; nothing blocks recording.
- **T13 (stolen laptop):** retention and delete-all limit what's on disk; the disk probe never needs elevation and says nothing when unsure (`privacy.rs:894-913`).
- Retention can only be set to 7/30/90/365 through a validating command; `privacy.*` keys are not webview-settable (`privacy.rs:19-21`, `330-334`; `src-tauri/src/commands.rs:59-66`). Spec §2 item 2.
- A background sweep never runs mid-recording, skips meetings a pipeline holds, refuses when the clock went back more than 1 h, and holds back when it would delete more than half the remaining audio (≥3 meetings); an explicit sweep from Settings may proceed (`privacy.rs:175-278`).
- Delete-all refuses while recording, importing, transcribing or with queued jobs (`privacy.rs:404-425`), requires the confirmation phrase (an accident guard, not authorization, `privacy.rs:451-454`, `797-799`), and writes the marker before deleting anything (`privacy.rs:822-832`).
- The restart wipe runs only while holding the instance lock (after the requesting instance exited), only for an unconsumed marker 0–10 minutes old, and renames the marker first so no second process acts on it (`privacy.rs:637-742`). It runs before logging and the DB open (`lib.rs:36-42`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/privacy.rs` | 16 (15 off Windows: `hung_probe_is_killed_after_the_timeout` is `cfg(windows)`) | classifiers, drive validation, notice length, owned-key delete, data-dir wipe, marker freshness/consumption, lock semantics, `perform_delete_all`, `apply_erase`, busy refusal, retention guards |
| unit (storage side) | `crates/fly-storage/src/privacy.rs`, `tests/storage_safety.rs` | 13 + 1 | keep-local sets, retention candidates, erase, scratch removal (see [fly-storage](fly-storage.md)) |
| unit (enforcement) | `src-tauri/src/llm_commands.rs`, `src-tauri/src/pipeline.rs` | — | keep-local refuses cloud LLM / Groq (`llm_commands.rs:1222`, `pipeline.rs:1417`) |
| vitest | `frontend/src/privacy.test.ts` | 15 | pure helpers: notice text, retention parsing, banner rule, delete-all gating, wipe outcome |
| vitest | `frontend/src/components/Sidebar.test.tsx`, `RecordingBar.test.tsx`, `SettingsModal.test.tsx` | — | folder toggle (api mocked), participants flag, restart event closes Settings |

**Known gaps:**
- The `delete_all_data` command itself (confirmation check, `LEGACY_KEYCHAIN_SERVICE` wiring, `spawn_blocking`) is never exercised; tests call `perform_delete_all` with in-memory secret stores, so real keychain deletion is unverified (T13).
- `<scratch_root>` (16 kHz audio copies, ASR checkpoints) is removed only inside the DB erase: if the erase fails, or a scratch file is locked, nothing reports it and the restart wipe never looks there (`privacy.rs:586-607` wipes only `<data_dir>`). No test covers either case.
- `hold_instance_lock` / `startup` are untested; `instance_lock_excludes_a_second_holder` exercises std's `File::lock`, not the 20 s wait or the global early return (`privacy.rs:638`, which ignores `data_dir`).
- `spawn_retention` timing and `sweep_now`'s recording/pipeline guards aren't tested; tests inject `now` and a `busy` closure.
- `busy_reason`'s "recording" branch, and the macOS `fdesetup` probe, have no tests.

## Changing it safely
- Run `cargo test -p fly-app privacy` and `cargo test -p fly-storage`; `npm --prefix frontend test` for `privacy.test.ts` / Settings tests.
- A new command: register in `lib.rs`, wrap in `frontend/src/api.ts`, mirror in `devMock.ts` (spec §2 item 4), and add a row here.
- Any new secret must be added to `fly_secrets::keys::ALL` (or `owned_secret_keys`) so delete-all removes it.
- Any new file location outside `<data_dir>` must be removed by delete-all explicitly and reported in `WipeReport`.
- Keep `startup` ahead of `logging::init` and `AppState::init` in `lib.rs`; never let the marker age check or the instance lock be bypassed — a stale marker must never delete data created later.
- Retention changes must keep the double-age check (`ended_at` and file mtimes) and the bulk/clock guards; never make retention settable through `set_app_setting`.
- Error strings are shown to the user (`PrivacySection.tsx`, `RestartRequired.tsx`); keep devMock's messages in step.
