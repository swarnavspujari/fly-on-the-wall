# Calendar providers (`fly-calendar`)

> Layer: crate. Described at main `82b7edf` (v2.4.0).
> In flight: None. (Spec PR 4.y plans a calendar-start "Ask me" prompt in the app, not in this crate.)

## Purpose
Reads the user's upcoming meetings from Google Calendar and Microsoft 365 (Graph), read-only. Owns the OAuth 2.0 authorization-code flow for installed apps (PKCE + a loopback redirect on `127.0.0.1`), token refresh, event/attendee parsing, and meeting-link detection (Zoom, Teams, Meet, GoTo). It has no Tauri dependency: the app supplies the secret store, the "open this URL" callback and the client ids.

## Files it owns
- `crates/fly-calendar/Cargo.toml` — deps: fly-core, fly-secrets, reqwest, tokio, sha2, uuid, base64, urlencoding.
- `crates/fly-calendar/src/lib.rs` — `CalendarProvider` trait, `CalendarEvent` / `CalendarAttendee` / `CalendarInfo`, `CalendarError`, `merge_upcoming`.
- `crates/fly-calendar/src/oauth.rs` — PKCE + loopback flow, `wait_for_redirect` (stray-connection hardening), token exchange/refresh, branded loopback pages.
- `crates/fly-calendar/src/google.rs` — `GoogleCalendarProvider` (calendar.readonly) and its JSON parsers.
- `crates/fly-calendar/src/msgraph.rs` — `MsGraphProvider` (Calendars.Read, public client) and its JSON parsers.
- `crates/fly-calendar/src/links.rs` — `detect_meeting_link` / `meeting_platform` (pure, allow-listed hosts + join paths).
- `crates/fly-calendar/tests/connect_flow.rs` — offline token-store lifecycle tests.

## Surface area
### Public API
- **Trait** `CalendarProvider` (`lib.rs:82-98`): `id()`, `display_name()`, `is_connected()`, `connect()`, `disconnect()`, `list_calendars()`, `upcoming(from, to)`.
- **Types** `CalendarEvent { id, provider, title, start, end, attendees, join_url }`, `CalendarAttendee { email, name, is_self, declined }`, `CalendarInfo { id, name, primary }`, `CalendarError { NotConnected, Auth, Provider, Network }` (`lib.rs:14-65`).
- **Fn** `merge_upcoming(Vec<CalendarEvent>)` — de-dupe by (provider, id), sort by start; link-less events kept (`lib.rs:72-77`).
- **Providers** `google::GoogleCalendarProvider { client_id, client_secret, secrets, open_url, disabled_calendars }` (`google.rs:16-24`), `msgraph::MsGraphProvider { client_id, secrets, open_url, disabled_calendars }` (`msgraph.rs:16-23`), `google::OpenUrl = Arc<dyn Fn(String)>` (`google.rs:14`).
- **OAuth** `oauth::{OAuthConfig, TokenSet, interactive_auth, refresh, parse_token_response, parse_redirect_request, CONNECTED_PAGE, FAILED_PAGE}`; `TokenSet::is_expired()` treats a token as expired 60 s early (`oauth.rs:96-100`).
- **Parsers** `google::{parse_google_events, parse_google_calendar_list}`, `msgraph::{parse_graph_events, parse_graph_calendar_list}`, `links::{detect_meeting_link, meeting_platform}`.

### Tauri commands
None (crates expose no commands). The app wraps this crate in [app-calendar](app-calendar.md).

### Events
None.

### Settings keys
None read or written here. Disabled-calendar ids arrive as `disabled_calendars` from the app.

### Database
None.

### Files and directories written
None.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| `accounts.google.com/o/oauth2/v2/auth` | Opened in the system browser via the app's `open_url` callback (`google.rs:29`) | User clicks Connect |
| `oauth2.googleapis.com/token` | Code exchange at connect; refresh when the stored token is within 60 s of expiry (`google.rs:30`, `oauth.rs:279-315`) | Connect, then automatic |
| `www.googleapis.com/calendar/v3/...` | `calendarList` + per-calendar `events` (max 25 each, no paging) (`google.rs:71`, `google.rs:85-90`) | A provider is connected; the app polls |
| `login.microsoftonline.com/common/oauth2/v2.0/authorize` | System browser (`msgraph.rs:28`) | User clicks Connect |
| `login.microsoftonline.com/common/oauth2/v2.0/token` | Code exchange and refresh (`msgraph.rs:29`) | Connect, then automatic |
| `graph.microsoft.com/v1.0/...` | `/me` (self addresses), `/me/calendars`, `calendarView` (`$top=25`, `Prefer: outlook.timezone="UTC"`) (`msgraph.rs:68`, `msgraph.rs:84-95`, `msgraph.rs:105`, `msgraph.rs:155`) | A provider is connected; the app polls |
| `127.0.0.1:<ephemeral>` (inbound) | Loopback listener during connect, max 5 min (`oauth.rs:129-136`, `oauth.rs:170`) | User clicks Connect |

Scopes are read-only: Google `calendar.readonly` (`google.rs:33`), Graph `offline_access Calendars.Read` (`msgraph.rs:32`). No keep-local gate applies (calendar data is not meeting content).

### External processes
None. (The system browser is opened by the app's `open_url` callback.)

### Secrets and keychain entries
| Key | Holder | Content |
|---|---|---|
| `google_oauth_token` (`fly_secrets::keys::GOOGLE_OAUTH_TOKEN`) | Written on connect and refresh, deleted on `disconnect()` (`google.rs:54-60`, `google.rs:140-154`) | `TokenSet` JSON (access, refresh, expiry) |
| `ms_oauth_token` (`fly_secrets::keys::MS_OAUTH_TOKEN`) | Same (`msgraph.rs:52-58`, `msgraph.rs:194-208`) | `TokenSet` JSON |

The Google client secret is passed in by the app (see [app-calendar](app-calendar.md)); this crate never stores it.

## Dependencies
- **Inbound:** `src-tauri/src/calendar_commands.rs` (builds providers, calls `connect`, `list_calendars`, `upcoming`, `merge_upcoming`); `src-tauri/src/recording.rs` (`CalendarAttendee` in `MeetingSeed` / `seed_attendees`).
- **Outbound:** [fly-secrets](fly-secrets.md) (`SecretStore`, key names), reqwest (HTTPS), tokio (loopback listener), sha2/base64/uuid (PKCE, state).

## Invariants
- PKCE S256 on every flow; `state` is a fresh UUID and must match before a code is accepted (`oauth.rs:121-137`, `oauth.rs:337-354`). Source: DECISIONS.md "M5 — Calendars" (hand-rolled OAuth).
- A loopback connection without this flow's `state` gets a 400 and does not end the sign-in, so another local process can't abort or hijack it; only the matching request decides the outcome (`oauth.rs:189-223`).
- Read-only scopes only (`google.rs:33`, `msgraph.rs:32`). Source: DECISIONS.md "M5" (BYO / public-client registrations).
- Tokens live only in the keychain via `SecretStore`, never on disk or in logs. Source: ARCHITECTURE.md "Secrets (fly-secrets)"; enforced by every write going through `self.secrets.set` (`google.rs:55`, `msgraph.rs:53`).
- Network error strings drop the URL (`e.without_url()`, `oauth.rs:302`, `google.rs:104`, `msgraph.rs:158`) so tokens/query strings don't reach error text.
- A 401 from a data endpoint maps to `NotConnected`; an expired token with no refresh token is `NotConnected` (`google.rs:110-112`, `google.rs:50-53`). The app turns `NotConnected | Auth` into a "Reconnect" prompt.
- One failing calendar never sinks the others (`google.rs:183-186`, `msgraph.rs:230-238`).
- `links.rs` matches hosts by exact name or real subdomain (`notzoom.us` never matches `zoom.us`) and requires a join path (`links.rs:54`, `links.rs:57-94`).
- Log hygiene (no calendar ids, emails or titles in logs; `src-tauri/src/logging.rs:6-13`). **Violated today:** `msgraph.rs:236` logs `cal.id`; the lexical test bans `cal_id` but not `cal.id`.

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| unit | `src/lib.rs` | 2 | `merge_upcoming` order and de-dupe |
| unit | `src/oauth.rs` | 7 | Redirect parsing + state check; loopback ignores stray/hostile connections, silent preconnects, provider error with matching state, timeout; token JSON parse |
| unit | `src/google.rs` | 4 | Event, attendee (self/declined/name), Meet link, link fallback, calendar list parsing |
| unit | `src/msgraph.rs` | 4 | Same for Graph, plus organizer fold-in and `/me` self matching |
| unit | `src/links.rs` | 15 | Join-URL classification per platform, look-alike hosts, HTML bodies, trailing punctuation, case preservation |
| integration | `tests/connect_flow.rs` | 6 | Token presence ↔ `is_connected`, `disconnect()`, expired-without-refresh → `NotConnected`, unreadable token → `Auth`, branded pages |

**Known gaps:**
- No test runs `interactive_auth` or `refresh` against a fake token server: the auth URL (PKCE challenge, scopes, `access_type=offline`), the code exchange body, refresh-token carry-over (`oauth.rs:289-291`) and keychain rewrite after refresh are untested. A regression here silently logs every user out after one hour.
- Revocation is not modeled: a revoked refresh token returns `Auth("token endpoint returned 400 …")`, which the app treats as "reconnect", but a 5xx from the token endpoint is also `Auth` and also prompts a reconnect. Neither path is tested.
- The lifecycle tests "connect" by writing the token straight into `MemorySecretStore` and exercise `provider.disconnect()`, which the app never calls (the command deletes the key directly). See the audit.
- `parse_graph_events` assumes UTC because `graph_get` sends `Prefer: outlook.timezone="UTC"`; no test pins the header, so dropping it would shift every Graph time silently.
- No paging: more than 25 events per calendar per day (or 100 Graph calendars / 250 Google calendars) are dropped without a log. Untested.
- Concurrent refreshes (two `upcoming_meetings` calls racing past expiry) may both refresh; Graph rotates refresh tokens. Untested.

## Changing it safely
- Run `cargo test -p fly-calendar` (unit + `connect_flow`). Interactive connect can't run in CI; do it by hand against both providers (DECISIONS.md "M5").
- Changing scopes or the auth URL means every existing user must reconnect; ship it with the app's Reconnect flow.
- Keep error strings free of URLs and token bodies beyond the 300-char endpoint snippet; they surface in the UI and in `tracing::warn!` lines in the app.
- New fields on `CalendarAttendee` / `CalendarEvent` cross IPC: update `frontend/src/types.ts`, `devMock.ts` (`upcoming()` fixture) and `recording::seed_attendees`.
- Never log calendar ids, attendee emails or event titles (the lexical log test only knows a few spellings).
