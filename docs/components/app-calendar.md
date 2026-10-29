# Calendar commands (`app-calendar`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: None. (Spec PR 4.y plans a calendar-start "Ask me" prompt with a notification plugin; not started.)

## Purpose
Wires [fly-calendar](fly-calendar.md) to the UI. Resolves which OAuth client to use (the user's own "bring your own" client id/secret, else the app's bundled build-time default), runs connect through the system browser, lists calendars and their on/off toggles, fetches today's upcoming meetings across both providers (flagging providers that need a reconnect), and starts a recording from a calendar event with the invitees seeded as an **unconfirmed** attendee list.

## Files it owns
- `src-tauri/src/calendar_commands.rs` — the 8 commands, client-id/secret resolution, disabled-calendar sets, the reconnect classification.
- `src-tauri/src/calendar_defaults.rs` — bundled OAuth client values, read at compile time with `option_env!` (empty when unset).

## Surface area
### Public API
- `pub(crate) const GOOGLE_CLIENT_SECRET_KEY = "google_oauth_client_secret"` (`calendar_commands.rs:24`), used by `state.rs:185` (legacy keychain copy) and `privacy.rs:528` (delete-all).
- `calendar_defaults::{GOOGLE_CLIENT_ID, GOOGLE_CLIENT_SECRET, MS_CLIENT_ID}` (`calendar_defaults.rs:30-45`), used only by `calendar_commands.rs:36-61`.
- Serialized types returned to the UI: `CalendarStatus` (`calendar_commands.rs:117-130`), `UpcomingMeetings { events, needs_reconnect }` (`:267-272`), `CalendarToggle` (`:338-347`).

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `get_calendar_settings` | — | `Result<CalendarStatus, String>` | `ui-app-shell` (`App.tsx:125`), `ui-settings` (`SettingsModal.tsx:325`), via `api.getCalendarSettings` |
| `set_calendar_settings` | `update: { google_client_id: String, google_client_secret: Option<String> (Some("") clears), ms_client_id: String }` | `Result<(), String>` | `ui-settings` (`SettingsModal.tsx:835`, `:852`), via `api.setCalendarSettings` |
| `connect_calendar` | `provider: "google" \| "msgraph"` | `Result<(), String>` (returns after the browser redirect or a 5-min timeout) | `ui-app-shell` (`App.tsx:161`, connect/reconnect from Home and the sidebar), `ui-settings` (`SettingsModal.tsx:858`), via `api.connectCalendar` |
| `disconnect_calendar` | `provider: String` | `Result<(), String>` | `ui-settings` (`SettingsModal.tsx:872`), via `api.disconnectCalendar` |
| `upcoming_meetings` | — | `Result<UpcomingMeetings, String>` | `ui-app-shell` (`App.tsx:141`, on mount + every 5 min, `App.tsx:295-297`), via `api.upcomingMeetings` |
| `list_calendars` | — | `Result<Vec<CalendarToggle>, String>` | `ui-settings` (`SettingsModal.tsx:334`), via `api.listCalendars` |
| `set_calendar_enabled` | `provider: String, calendar_id: String, enabled: bool` | `Result<(), String>` | `ui-settings` (`SettingsModal.tsx:821`), via `api.setCalendarEnabled` |
| `start_meeting_from_event` | `title: String, attendees: Vec<CalendarAttendee>` | `Result<RecordingStatus, String>` | `ui-app-shell` (`App.tsx:866`, "Start recording" on Up next in `ui-home` and the sidebar), via `api.startMeetingFromEvent` |

### Events
None emitted directly. `start_meeting_from_event` starts the live-caption loop through `recording::start_recording_impl` (events owned by [app-recording](app-recording.md)).

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `calendar.google.client_id` | R/W | No (`commands.rs:761` asserts it) | User's own Google client id; empty = use bundled default |
| `calendar.ms.client_id` | R/W | No | User's own Azure app (client) id; empty = bundled default |
| `calendar.google.disabled_ids` | R/W | No | JSON array of Google calendar ids toggled off |
| `calendar.ms.disabled_ids` | R/W | No | JSON array of Graph calendar ids toggled off |
| `capture.mic_device_id` | R (via `start_recording_impl`) | Yes | Owned by [app-recording](app-recording.md) |

### Database
| Table | Columns | Read / write |
|---|---|---|
| `settings` | `key`, `value` | R/W (the keys above) |
| `notes`, `meetings` | via `recording::start_recording_impl` (`recording.rs:139-161`) | W on `start_meeting_from_event` (new note titled after the event; meeting with `attendees_json`, `attendees_confirmed = 0`) |

### Files and directories written
- `start_meeting_from_event` → `recordings/<date> <title>/` and the note mirror, via [app-recording](app-recording.md) / [fly-storage](fly-storage.md).
- Nothing else; tokens and the Google secret go to the keychain.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| Google / Microsoft hosts listed in [fly-calendar](fly-calendar.md) | `connect_calendar`; `upcoming_meetings` (UI polls every 5 min); `list_calendars` | User connected a provider. Not subject to cloud-LLM consent or keep-local (no meeting content is sent). |

### External processes
- The system browser, opened through `tauri-plugin-opener` with the provider's auth URL (`calendar_commands.rs:73-78`).

### Secrets and keychain entries
Service `com.flyonthewall.app` (`fly-secrets`):
| Key | Read / write | Meaning |
|---|---|---|
| `google_oauth_client_secret` | R/W/delete (`calendar_commands.rs:45-52`, `:190-202`) | User's own Google client secret (BYO) |
| `google_oauth_token` | R (status); deleted by `disconnect_calendar`; written by fly-calendar on connect/refresh | Token set JSON |
| `ms_oauth_token` | Same | Token set JSON |

Bundled defaults (`calendar_defaults.rs`): `FOTW_GOOGLE_CLIENT_ID`, `FOTW_GOOGLE_CLIENT_SECRET`, `FOTW_MS_CLIENT_ID`, compiled in from release-workflow secrets (`.github/workflows/release.yml:307-309`); empty in CI and local builds unless exported. The Google desktop secret is treated as non-confidential under PKCE (`calendar_defaults.rs:24-26`). User values always win (`calendar_commands.rs:34-61`).

## Dependencies
- **Inbound:** `lib.rs:172-179` (registration); `state.rs:185` and `privacy.rs:528` (the secret key name).
- **Outbound:** [fly-calendar](fly-calendar.md), [fly-secrets](fly-secrets.md), [fly-storage](fly-storage.md) (settings), [app-recording](app-recording.md) (`seed_attendees`, `start_recording_impl`), `tauri-plugin-opener`, `AppState.storage` / `AppState.secrets` (see [app-shell](app-shell.md)).

## Invariants
- **Calendar-seeded attendees stay unconfirmed.** `start_meeting_from_event` passes the event's invitees through `recording::seed_attendees` (drops `is_self` and `declined`, `recording.rs:109-123`) into `create_meeting`, which stores `attendees_confirmed = false` (`crates/fly-storage/src/meetings.rs:53`); only confirmed rosters set `num_speakers` (`pipeline.rs:44`). Source: spec §2 item 7.
- Calendar client ids and disabled-id lists are **not** webview-settable; only these dedicated commands write them. Source: spec §2 item 2; `commands.rs:59-66`, test `commands.rs:761`.
- `disconnect_calendar` never builds the provider, so it works when the OAuth config is missing or broken (`calendar_commands.rs:236-245`).
- `upcoming_meetings` never fails because one provider failed; auth failures (`NotConnected` or `Auth`) become `needs_reconnect` entries, others are logged (`calendar_commands.rs:274-324`).
- The window is "now − 30 min" to local midnight; never tomorrow (`calendar_commands.rs:254-262`, `:292-293`).
- Bundled client credentials never enter git; they come only from build-time env (`calendar_defaults.rs:9-13`). Note: DECISIONS.md "M5 — BYO OAuth app registrations" still says the app ships no client credentials; that entry predates `calendar_defaults.rs`.
- Tokens and the Google secret only in the keychain (ARCHITECTURE.md "Secrets").

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src-tauri/src/calendar_commands.rs` | 3 | Provider → token-key mapping; `is_auth_error` classification; a re-implemented disconnect against `MemorySecretStore` |
| unit | `src-tauri/src/recording.rs` (owned by app-recording) | 1 relevant (`seed_skips_self_and_declined_and_falls_back_to_email`, `:386`) | Seeding rule |
| unit | `crates/fly-storage/src/meetings.rs` | relevant asserts at `:478-507` | New meetings start unconfirmed |
| vitest | `frontend/src/components/AttendeeEditor.test.tsx` | 2 relevant | Seeded roster is shown and never auto-confirmed (against devMock) |
| vitest | `frontend/src/components/Home.test.tsx` | several | Up next card, connect prompt (props only, no IPC) |

**Known gaps:**
- No command in this file is invoked by any test; there is no Tauri command harness. `start_meeting_from_event` (the spec §2 item 7 entry point) is covered only piecewise: the seeding fn, the storage default, and a devMock copy of the seeding rule (`devMock.ts:1218-1238`) that could drift from `seed_attendees`.
- `disconnect_deletes_token_without_provider_config` re-implements the command instead of calling it, so the regression it describes (building the provider first) would pass.
- **Disconnect can be undone on restart (inferred, untested).** `AppState::init` copies `google_oauth_token`, `ms_oauth_token` and `google_oauth_client_secret` from the pre-rebrand `com.looma.notetaker` keychain whenever the new store lacks them (`state.rs:82-86`, `:172-180`). `disconnect_calendar` deletes only the new-service key (`calendar_commands.rs:244`), so a pre-rebrand user who disconnects is silently reconnected at next launch. Only "Delete all my data" clears the legacy service (`privacy.rs:808`).
- Transient token-endpoint failures (5xx) are `CalendarError::Auth` and show "Reconnect"; no test pins the intended behavior.
- devMock `connect_calendar` succeeds instantly (`devMock.ts:1284`); the real call blocks until the browser redirect or 5 min, which jsdom tests can't show.

## Changing it safely
- Add a command: register in `lib.rs`, wrap in `frontend/src/api.ts`, mirror in `devMock.ts` (spec §2 item 4), add a row here.
- New calendar settings keys must not use the `consent.` / `capture.` / `ui.` prefixes; give them a validating command instead.
- Any change to seeding must keep `attendees_confirmed = false` and update `devMock.ts` `start_meeting_from_event` in the same PR.
- To test bundled credentials locally, export the three `FOTW_*` vars and force a rebuild of this file (`calendar_defaults.rs:19-22`).
- Log provider and counts only; never event titles, attendee emails or calendar ids.
