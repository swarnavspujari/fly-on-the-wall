# MCP sidecar packaging and setup (`app-mcp-sidecar`)

> Layer: backend feature (src-tauri). Described at main `82b7edf` (v2.4.0).
> In flight: None. (PR #92's schema bump changes what an already-running sidecar can open; see [fly-mcp](fly-mcp.md).)

## Purpose
Ships the `flyonthewall-mcp` binary (crate [fly-mcp](fly-mcp.md)) next to the app executable as a Tauri `externalBin`, and gives the user a ready-to-paste client config. The app never starts the server itself: MCP clients (Claude Desktop, Claude Code, Cursor, VS Code) spawn it. This component is the build, CI, installer and Settings plumbing around that binary.

## Files it owns
- `src-tauri/src/commands.rs:626-645` — the `mcp_config` command only (the rest of `commands.rs` belongs to other components).
- `scripts/prepare-sidecars.mjs` — copies `target/release/flyonthewall-mcp[.exe]` to `src-tauri/binaries/flyonthewall-mcp-<host triple>[.exe]`.
- `package.json` — `prepare-sidecars` script: `cargo build --release -p fly-mcp && node scripts/prepare-sidecars.mjs`.
- `src-tauri/tauri.conf.json` — `bundle.externalBin: ["binaries/flyonthewall-mcp"]` (line 48); `build.beforeDevCommand` / `beforeBuildCommand` run `prepare-sidecars` (lines 7-9).
- `src-tauri/installer-hooks.nsi` — NSIS pre-install and pre-uninstall `taskkill /F /IM flyonthewall-mcp.exe /T`.
- `.gitignore:18` — `src-tauri/binaries/` is never committed.
- CI wiring (read-only here): `.github/workflows/ci.yml` "Stub the MCP sidecar" step (lines 227-241) and the `sidecar` job (lines 299-347); `.github/workflows/release.yml` "Stage sidecar binaries" (lines 280-281).
- Frontend helpers it feeds: `frontend/src/settingsModel.ts:263-285` (`mcpCommand`, `claudeCodeCommand`). The Settings screen itself is [ui-settings](ui-settings.md).

## Surface area
### Public API
None beyond the command. Nothing else in `src-tauri` calls `mcp_config`.

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `mcp_config` | none | `Result<String, String>` — pretty JSON `{"mcpServers":{"flyonthewall":{"command":"<app dir>/flyonthewall-mcp[.exe]","args":[]}}}` | `ui-settings` (`SettingsModal.tsx:375` on open; rendered at `SettingsModal.tsx:2136-2212`), via `api.mcpConfig` (`api.ts:94`) |

How the command builds the path: `std::env::current_exe()`'s parent joined with `flyonthewall-mcp.exe` on Windows, `flyonthewall-mcp` elsewhere (`commands.rs:629-638`). It passes no `--data-dir`, so the server uses its own default, `dirs::data_dir()/FlyOnTheWall` (`crates/fly-mcp/src/main.rs:9-11`), the same rule the app uses (`src-tauri/src/state.rs:135-140`).

What Settings shows (Settings › Other AI apps, `SettingsModal.tsx:2136-2212`):
- **Claude Desktop:** steps (Settings › Developer › Edit Config; the macOS config path), the raw snippet with a copy button, and "Fly on the Wall never edits Claude's files itself."
- **Claude Code, Cursor and VS Code** (advanced): `claude mcp add flyonthewall -- "<path>"` built by `claudeCodeCommand` (paths with spaces or quotes are quoted), plus prose for `~/.cursor/mcp.json` and `.vscode/mcp.json` (`"type": "stdio"`).
- **What it can see:** "It can read every note, including notes you keep local." This is **stale**: the server has refused Keep-local content since `a5e1995` (see [fly-mcp](fly-mcp.md) Invariants).

devMock mirrors `mcp_config` with a fixed Windows path (`devMock.ts:854-862`, `devMock.ts:1326-1327`), whatever the OS.

### Events
None.

### Settings keys
None.

### Database
None.

### Files and directories written
- Build time: `src-tauri/binaries/flyonthewall-mcp-<triple>[.exe]` (gitignored).
- Install time: the bundler places the binary next to the app executable (Windows install dir; macOS `Fly on the Wall.app/Contents/MacOS/` (inferred from Tauri's externalBin layout); Linux `usr/bin` inside the AppImage or deb (inferred)).
- Runtime: nothing. The command only formats a string.

### Network egress
None.

### External processes
| Process | When | Where |
|---|---|---|
| `rustc -vV` | `prepare-sidecars.mjs` reads the host triple | dev / CI / release machines (`prepare-sidecars.mjs:7-12`) |
| `cargo build --release -p fly-mcp` | `npm run prepare-sidecars` | dev / CI / release machines (`package.json`) |
| `taskkill /F /IM flyonthewall-mcp.exe /T` | Before NSIS install (including auto-update) and uninstall | User machine, Windows (`installer-hooks.nsi:9-17`) |
| `flyonthewall-mcp --data-dir <tmp> ` | CI smoke: one `tools/list` round trip | `sidecar` job, Linux (`ci.yml:331-347`) |

At runtime the app spawns nothing here; the MCP client spawns the binary.

### Secrets and keychain entries
None. The binary never holds keys (spec §2 item 5).

## Dependencies
- **Inbound:** [ui-settings](ui-settings.md) (snippet display); MCP clients (spawn the shipped binary).
- **Outbound:** [fly-mcp](fly-mcp.md) (the binary), Tauri bundler `externalBin`, NSIS installer hooks, the CI/release workflows.

## Invariants
- **The snippet points at the binary next to the app executable.** DECISIONS.md "M9" 2026-07-02 ("ships as a Tauri externalBin sidecar … exactly where the Settings MCP snippet points"); `commands.rs:629-638`, `tauri.conf.json:48`, `prepare-sidecars.mjs:15-19`. The staged name must be `flyonthewall-mcp-<triple>`; the CI `sidecar` job fails if `externalBin` and the script disagree (`ci.yml:333-339`).
- **The app never edits MCP client config files.** UI copy `SettingsModal.tsx:2204`; `mcp_config` only returns a string.
- **A running server must not block install or uninstall.** DECISIONS.md 2026-07-13 (exit on stdin EOF, NSIS taskkill as backstop); `installer-hooks.nsi:9-17`.
- **Secret-bearing release steps run no npm script code.** `release.yml:21-26`. The "Stage sidecar binaries" step does run `npm run prepare-sidecars`, but with no secrets in its env (`release.yml:280-281`); the signing env is only on "Build installers".
- **The shipped binary never holds keys or calls a cloud LLM.** Spec §2 item 5; enforced in [fly-mcp](fly-mcp.md).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest | `frontend/src/settingsModel.test.ts` ("MCP instructions") | 2 | `mcpCommand` parses the snippet (and rejects junk); `claudeCodeCommand` quoting |
| CI job | `.github/workflows/ci.yml` `sidecar` | — | Release-profile build, staged file name matches `externalBin`, binary answers `tools/list` (Linux only) |
| integration (runs in CI) | `crates/fly-mcp/tests/*.rs` | 3 | The binary itself (debug build), see [fly-mcp](fly-mcp.md) |
| manual | `docs/TESTING.md:74-75` | — | Paste the snippet into Claude Desktop, search and read notes |

**Known gaps:**
- `mcp_config` has no Rust test (path join, `.exe` suffix, JSON shape, `"flyonthewall"` key). The vitest uses a hand-written snippet, and devMock's copy is hand-maintained, so a backend shape change would pass both.
- The Rust CI job stubs the sidecar with an **empty** file (`ci.yml:227-241`); tauri-build only checks that it exists. No CI build of the app ever contains a working sidecar.
- The `sidecar` job runs on Linux only and only when `^(scripts/|crates/fly-mcp/|Cargo\.toml$|package\.json$|src-tauri/tauri\.conf\.json$)` changes (`ci.yml:115`) or weekly. Changes to fly-storage or fly-core (the server's dependencies) don't trigger it; the rust job's debug stdio tests still run.
- Release legs stage the binary on Windows and macOS with no smoke test of the staged file (`release.yml:280-281`).
- Linux AppImage: `current_exe()` is inside the AppImage's temporary mount, so the snippet's path likely stops existing when the app quits (inferred; not verified on Linux).
- `docs/TESTING.md:74-75` still names the old Settings label ("Chat with your notes (MCP)").

## Changing it safely
- Renaming the binary means changing, together: `crates/fly-mcp/Cargo.toml` `[[bin]]`, `prepare-sidecars.mjs`, `tauri.conf.json` `externalBin`, `commands.rs` `mcp_config`, `installer-hooks.nsi`, the CI stub step, devMock's snippet, and the settingsModel tests.
- Changing the snippet shape: update devMock (`devMock.ts:854-862`) and the "MCP instructions" vitest; `mcpCommand` reads the first `mcpServers` entry only.
- Run `npm run prepare-sidecars` before `tauri dev` / `tauri build` (both config hooks do it). Without it, tauri-build fails on the missing `externalBin`.
- Don't move data-dir resolution in only one place: the app (`state.rs`) and the server (`main.rs`) must agree, because the snippet passes no `--data-dir`.
- When the server's Keep-local behavior changes, fix the "What it can see" copy (`SettingsModal.tsx:2209-2212`) and docs/THREAT_MODEL.md T10 in the same PR.
