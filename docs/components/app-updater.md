# Auto-updater (`app-updater`)

> Layer: backend feature (src-tauri) — plugin configuration, a frontend hook, and two app-owned Windows install commands. Described at main `82b7edf` (v2.4.0).
> In flight: None.

## Purpose

Keeps installed apps current from GitHub Releases. tauri-plugin-updater fetches a signed `latest.json`, verifies the minisign signature of the platform bundle, and installs it; tauri-plugin-process relaunches. The frontend hook `useUpdater` drives one silent check after launch and user-initiated download / install. An update must never interrupt a recording. The release workflow signs each platform's bundle and publishes `latest.json` only when all four platforms succeeded.

## Files it owns

- `src-tauri/tauri.conf.json:12-22` — `plugins.updater`: minisign `pubkey`, `endpoints` = `https://github.com/swarnavspujari/fly-on-the-wall/releases/latest/download/latest.json`, `windows.installMode = "passive"`.
- `src-tauri/tauri.updater.conf.json` — `bundle.createUpdaterArtifacts: true`, layered in by release legs only.
- `src-tauri/src/lib.rs:69-76` — registers `tauri_plugin_updater` and `tauri_plugin_process` in `setup` (desktop only).
- `src-tauri/capabilities/default.json:6` — `updater:default`, `process:allow-restart`.
- `src-tauri/Cargo.toml:53` (`tauri-plugin-updater = "2"`, locked 2.14.1) and tauri-plugin-process (locked 2.4.0); `frontend/package.json:20-21` (`@tauri-apps/plugin-updater ~2.14.0`, `@tauri-apps/plugin-process ~2.4.0`).
- `src-tauri/src/windows_update.rs` — `windows_update_download` / `windows_update_install` and the pure hook + recovery piece (see below); registered in `lib.rs` (desktop only) with a managed `PendingUpdate`.
- `frontend/src/updater.ts` — `useUpdater` hook and the `Updater` / `UpdatePhase` types.
- `frontend/src/components/UpdateBanner.tsx` — the notice (Update now / Restart now / Try again / Later).
- `frontend/src/notices.ts:111-153` — `updateNoticeVisible`, `updateCopy`; `NOTICE_PRIORITY.update = 1` (`notices.ts:12-18`).
- Release wiring (read-only here): `.github/workflows/release.yml` "Updater preflight" (`release.yml:181-198`), "Generate updater manifest fragment" (`release.yml:338-355`), per-leg "Attach to release" as prerelease (`release.yml:364-375`), job `updater-manifest` (`release.yml:378-424`).

Settings' update row (`SettingsModal.tsx:1166-1247`) and the notice stack (`AppNotices.tsx:198-203`) consume this; they belong to [ui-settings](ui-settings.md) and [ui-notices](ui-notices.md).

## Surface area

### Public API

- `useUpdater(os, recordingActive, appImage = false): Updater` (`updater.ts:49-183`). Called once in `App.tsx:187` with `info.os`, `recStatus.active || screenStatus.active`, `info.appimage` (from `app_info`, [app-shell](app-shell.md)).
- `Updater { phase, version, progress, error, errorSource, supported, dismissed, check, downloadAndInstall, restart, dismiss }` (`updater.ts:26-44`).
- `UpdatePhase`: `idle | checking | upToDate | available | downloading | ready | installing | error` (`updater.ts:16-24`).
- `supported` = Windows, macOS, or Linux **AppImage** (`APPIMAGE` env set); a deb install is unsupported (`updater.ts:54`).

Lifecycle (`updater.ts:72-165`):

1. 5 s after mount, one **silent** `check({ timeout: 30_000 })`. A failure goes back to `idle` with a console warning (`updater.ts:85-95`, `98-104`).
2. "Check now" in Settings runs a non-silent check; a failure shows in Settings only (`errorSource: "check"`, hidden from the banner by `notices.ts:117`).
3. "Update now" / "Download and install" → `update.download(progress)`. On completion, if a recording is active **at that moment**, park in `ready`; otherwise `install()` immediately (`updater.ts:122-160`).
4. `install()` → on Windows `invoke("windows_update_install")`, elsewhere `update.install()`, then `relaunch()`. On Windows the plugin runs the NSIS installer and exits the process itself; on macOS / Linux `relaunch()` restarts. If the install fails, `phase` is `error` with `errorSource: "install"`, and the next "Try again" **checks again** (the held `Update` may be stale) before downloading; a check that finds nothing ends in `upToDate`.
5. `restart()` refuses while recording (`updater.ts:162-165`).

### Tauri commands

Two app-owned commands, used on **Windows only** (`useUpdater` routes `os === "windows"` through them; macOS / Linux keep the plugin's JS path). They exist because the plugin's JS `install()` runs `AppHandle::cleanup_before_exit()` _before_ it launches the installer — that clears the resource table (dropping single-instance's mutex, the one-copy lock) and hides every window — and the plugin `Builder` has no way to replace that hook for the JS path.

| Command                                                     | Does                                                                                                                                                                                                                                                                                                                                       | Returns                                                                      |
| ----------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------- |
| `windows_update_download(on_event: Channel<DownloadEvent>)` | Builds the updater from `updater_builder()` (config pubkey, endpoints and install mode still apply) with `on_before_exit` = hide the main window only, checks again, downloads and minisign-verifies the bundle, keeps `(Update, bytes)` in `PendingUpdate`; progress on the channel in the plugin's `Started`/`Progress`/`Finished` shape | `()` or an error string (`no update is available` when the check finds none) |
| `windows_update_install()`                                  | Takes the pending update and runs `Update::install` on a blocking thread. Success: the plugin launches the installer and calls `process::exit(0)`. Failure (for example antivirus blocks `ShellExecuteW`): `unminimize` + `show` + `set_focus` on the main window and the error goes back                                                  | `()` or an error string                                                      |

Capability: none. App commands are not ACL-gated in this app (`build.rs` has no `AppManifest`), so nothing was added to `capabilities/default.json`; the commands take no path or URL from the webview, and the bundle is the signed one from the configured endpoint. The download/install split keeps the "never interrupt a recording" rule: the frontend can park in `ready` between them. The pure decision (`before_exit`, `recover_on_failure`) is unit-tested with a fake window; `ShellExecuteW` itself is not unit-testable.

The webview also calls the plugins' own commands: `plugin:updater|check`, `plugin:updater|download`, `plugin:updater|install` (via the JS `Update` object) and `plugin:process|restart` (via `relaunch`). devMock mirrors `check` and `download` behind `localStorage.fotwMockUpdate` = `"available"` (finds 2.4.0; download hangs) or `"fail"` (download rejects) (`devMock.ts:1482-1495`); `install` and `restart` fall through to `null`.

### Events

None app-defined. Download progress arrives through the plugin's channel callback (`Started` / `Progress` / `Finished`), handled in `updater.ts:133-146`.

### Settings keys

None. `localStorage`: none. devMock toggle `fotwMockUpdate` (`devMock.ts:30-32`). There is no setting to turn off the automatic launch check.

### Database

None.

### Files and directories written

By the plugin (inferred from its implementation, not app code): the downloaded bundle in a temp location, then the NSIS installer run (Windows), the `.app` bundle swapped in place (macOS), or the AppImage replaced (Linux). The NSIS hook first kills a running `flyonthewall-mcp.exe` ([app-mcp-sidecar](app-mcp-sidecar.md)).

### Network egress

| Host                                                                                                               | When                                                       | Gate (consent / keep-local / user action)                     |
| ------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------- | ------------------------------------------------------------- |
| `github.com/swarnavspujari/fly-on-the-wall/releases/latest/download/latest.json` (redirects to GitHub's asset CDN) | 5 s after every launch on a supported install; "Check now" | None for the launch check (no opt-out); click for "Check now" |
| `github.com/swarnavspujari/fly-on-the-wall/releases/download/v<version>/<asset>` (the `url` in `latest.json`)      | "Update now" / "Download and install"                      | User click; deferred while recording                          |

No meeting data is sent. GitHub sees the IP address and request on each launch.

### External processes

- Windows: the NSIS installer launched by the plugin with `ShellExecuteW`, passive mode; the plugin then calls `std::process::exit(0)` (`tauri-plugin-updater-2.14.1/src/updater.rs:1009-1050`). Our before-exit hook only hides the main window, so single-instance's lock stays held until the process really exits. **If the launch fails** (antivirus), `install_inner` returns `Err` and skips the exit; `windows_update_install` then restores the window (visible, focused) and the app remains the one instance — a relaunch focuses it instead of starting a second copy. If antivirus kills the installer _after_ it started, the app has already exited. This protects updates _from_ the release that ships it onward; the code that runs an update is the installed version's.
- macOS / Linux: the app restarts itself through tauri-plugin-process (`request_restart`).

### Secrets and keychain entries

None in the app. The minisign **public** key is in `tauri.conf.json:14`. The private key and password are the `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` repo secrets, present only on the release "Build installers" step (`release.yml:288-291`) and checked by "Updater preflight".

## Dependencies

- **Inbound:** `App.tsx` (hook), [ui-notices](ui-notices.md) (`AppNotices` renders `UpdateBanner`), [ui-settings](ui-settings.md) (update row), [app-shell](app-shell.md) (`app_info.os`, `app_info.appimage`).
- **Outbound:** tauri-plugin-updater, tauri-plugin-process, GitHub Releases, `release.yml`, the NSIS installer hooks.

## Invariants

- **An update never interrupts a recording.** `updater.ts:10-14`. The banner is not mounted while recording (`AppNotices.tsx:198`); Settings disables install / restart (`SettingsModal.tsx:1216`, `1234`); a download that finishes mid-recording parks in `ready` (`updater.ts:148-153`); `restart` refuses (`updater.ts:163`). "Recording" means mic or screen capture only (`App.tsx:186`); an in-flight transcription, import or LLM job is not deferred for (inferred: the scheduler re-queues jobs after relaunch).
- **Install and relaunch run only after an explicit click.** `updater.ts:10-14`; the launch check never downloads.
- **Updates are minisign-verified against the pinned public key.** docs/THREAT_MODEL.md T9; docs/SECURITY_AUDIT_2026-10.md "Verified good practices" ("The updater pubkey is pinned on an HTTPS endpoint"). Key rotation: THREAT_MODEL "Incident response › Updater key compromise".
- **The signing key is never exposed to npm code.** THREAT_MODEL T8; `release.yml:21-26`, `release.yml:283-291`.
- **`latest.json` is never partial.** The merge refuses fewer than 4 fragments (`release.yml:398-411`); leg releases stay prerelease and `make_latest: false` until the manifest is attached (`release.yml:366-373`, `release.yml:415-424`); the attest job depends on `updater-manifest` (`release.yml:493-497`).
- **Only app releases may be "Latest".** Any other release on this repo (for example the `tools-whisper*` engine releases) must be a **prerelease**, or `/releases/latest/download/latest.json` 404s for every installed app (`.github/workflows/build-whisper-sidecar.yml:16-17`, `90-93`, `179-181`; the v2.3.0 incident in `release.yml:368-371`).
- **Tag must equal the app version** (`release.yml:193-197`), or installed apps would re-download the same update forever.

## Tests

| Kind             | File                                                                                 | Tests                                                      | Covers                                                                                              |
| ---------------- | ------------------------------------------------------------------------------------ | ---------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| vitest           | `frontend/src/updater.test.ts`                                                       | 7                                                          | Install failure state, retry re-check, Windows command routing, recording parks `ready`             |
| cargo            | `src-tauri/src/windows_update.rs` (`tests`)                                          | 3                                                          | Hook only hides the window; failed install restores it and keeps the error; success leaves it alone |
| vitest           | `frontend/src/notices.test.ts` (`updateNoticeVisible`, `updateCopy`, priority order) | 6 (+2 priority)                                            | When the banner shows (not for check errors, not after Later); copy per phase                       |
| vitest           | `frontend/src/components/AppNotices.test.tsx` ("update notice" + ordering)           | 7 (+ordering cases, +"while recording … the update waits") | Banner buttons call the right `Updater` methods; progress bar; hidden while recording               |
| vitest           | `frontend/src/components/SettingsModal.test.tsx`                                     | 0 updater-specific                                         | Uses a static idle `Updater`                                                                        |
| manual           | `docs/TESTING.md:100-107`                                                            | —                                                          | Check against a real release; banner → download → restart; no banner while recording                |
| manual (Windows) | deny-execute ACL on the updater's temp dir                                           | —                                                          | Blocked launch: window returns with the error, relaunch focuses it (unverified; steps in the PR)    |

**Known gaps:**

- **`useUpdater` is only partly tested.** `frontend/src/updater.test.ts` (7 tests) covers install failure → `error`/`install`, "Try again" re-checking, Windows-vs-JS routing, and a recording starting mid-download parking in `ready`. Silent vs. loud check errors and the progress math are still unexecuted.
- **No Rust or workflow test.** The `latest.json` merge, the four-fragment rule and the pubkey/endpoint config are checked only by running a release.
- **Windows update orphans the managed Ollama server.** The plugin exits via `std::process::exit(0)`, so `RunEvent::Exit` (which stops `ollama serve`, `lib.rs:202-211`) never runs; our `on_before_exit` hook (`windows_update.rs`) only hides the window and does not stop Ollama. See [app-ollama](app-ollama.md).
- devMock's `install` resolves to `null` without restarting, so port-1420 QA never reaches "installing".
- Stale comment: `App.tsx:184` says "Auto-update (Windows-only)"; macOS and Linux AppImage are supported.

## Changing it safely

- **Never publish a non-prerelease** release on this repo that isn't the app (tools, test builds): it hijacks `/releases/latest` and breaks every updater.
- Bumping the version: `tauri.conf.json`, `src-tauri/Cargo.toml`, `package.json` files and the tag must agree ("Updater preflight").
- Rotating the key: follow THREAT_MODEL "Updater key compromise" (bridge release signed with the old key that pins the new pubkey).
- Adding a platform: add a matrix leg with `updaterPlatform` / `updaterAsset` / `updaterSig`, and update the fragment count check (`release.yml:402-405`).
- Anything that must stop before an update (child processes, open files): on Windows it needs `tauri_plugin_updater::Builder::on_before_exit`, because `RunEvent::Exit` won't fire; on Windows also consider the NSIS hook.
- Extending "recording" to other long jobs: change `recordingActive` in `App.tsx:186` (both the banner and Settings read it).
- Keep devMock's `plugin:updater|*` cases in step with the JS API shape (`rid`, `version`, `currentVersion`).
