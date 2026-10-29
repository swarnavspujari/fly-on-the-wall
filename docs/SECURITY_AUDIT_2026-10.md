# Security audit — October 2026

Date: 2026-10-23 · Scope: whole repo (Tauri 2 / Rust workspace, React + Vite frontend, GitHub Actions, landing site, scripts) · Method: dependency audits (`npm audit`, `cargo audit`), advisory research (Node, npm supply-chain incidents, Tauri, RustSec, GitHub Actions, bundled binaries), and a staff-level code review of the IPC surface, storage, LLM providers, MCP server, frontend rendering and CI.

There is no PHP in this project. Node.js locally is v24.22.1, the patched release from the 2026-10-20 security release.

## Fixed in this change

| # | Severity | Issue | Fix |
|---|---|---|---|
| 1 | High | **Recursive delete outside the data folder.** A `manifest.json` in a copied-in recording folder (the advertised "move meetings between machines" flow) is trusted by the startup self-heal. Deleting that meeting's note then ran `remove_dir_all` on whatever folder its paths pointed at, e.g. `C:/Users/<you>/Documents`. The staged-import cleanup had the same pattern. | `fly_storage::contained_recordings_dir` only allows `recordings/<folder>` inside the data dir. Both `purge_meeting` and `purge_staged_import` now refuse anything else. Unit-tested with absolute, UNC and `..` paths. |
| 2 | Medium | **`open_attachment` / `reveal_attachment` / `ensure_video_thumbnail` accepted any path.** `data_dir.join(rel_path)` with an absolute or `..` path escapes the base. `open_attachment` then launches it (ShellExecute), so any script injection in the webview becomes code execution. The thumbnail command also wrote a `.jpg` next to the given path. | `Storage::attachment_abs_path` now returns an error unless the path is purely relative with no `..`, root or drive components. |
| 3 | Medium | **DOM XSS in the notes editor's Highlight fallback.** It ran `execCommand("insertHTML", "<mark>" + selectedText + "</mark>")`, so text that merely *displays* as markup (e.g. imported or AI-written notes) was re-parsed as live HTML. | The `<mark>` is built with DOM APIs (`extractContents` / `insertNode`); text is never re-parsed. |
| 4 | Critical (npm rating) / High | **Vulnerable frontend dependencies:** 12 advisories, including DOMPurify (critical aggregate: hook and SAFE_FOR_TEMPLATES bypasses), Vite / launch-editor (command execution on Windows dev boxes), postcss, nanoid, source-map-js, zod, vitest. | `npm update` + `npm audit fix` in `frontend/`; both packages now report **0 vulnerabilities**. DOMPurify is 3.4.17 and Vite 8.3.x. |
| 5 | High | **Tauri 2.11.5:** GHSA-w28w-mhc8-qvjv, a missing ACL check on `__TAURI_CHANNEL__\|fetch`. | `tauri` 2.11.6. |
| 6 | High | **Rust TLS stack:** rustls 0.23.41 (RUSTSEC-2026-0285/-0346/-0348/-0349, including a key-exchange downgrade) and rustls-webpki / webpki-root-certs (RUSTSEC-2026-0382/-0383, distrusted intermediates accepted). This is reachable on every Anthropic, Groq and calendar call. Also h2 (RUSTSEC-2026-0258) and quinn-proto. | `cargo update` to rustls 0.23.46, rustls-webpki 0.103.16, webpki-root-certs 1.0.11, h2 0.4.20, quinn-proto 0.11.19. |
| 7 | Low | Tauri JS packages could drift a minor version ahead of the Rust crates. The release workflow deletes lockfiles, and the Tauri CLI refuses mismatched minors. | `@tauri-apps/*` pinned with `~` to the minor line the Rust crates use. |

## Claude model: Sonnet 5.5 / Haiku 5.5

- The default Anthropic model is now **`claude-sonnet-5-5`**. **`claude-haiku-5-5`** is offered as the faster, lower-cost option in a new Settings dropdown, visible in both Simple and Technical views.
- **Existing users are migrated on launch.** A saved previous-generation Sonnet id (`claude-sonnet-5`, `-4-x`, `3.x`) becomes `claude-sonnet-5-5`, and a saved Haiku id becomes `claude-haiku-5-5`. Opus, Fable and custom ids are left as the user chose.
- **Request-shape fixes:**
  - **Turning thinking off.** Polish and Extraction turn thinking off. `thinking: {type: "disabled"}` returns HTTP 400 on Sonnet 5.5, so those features would have broken with a plain model-string swap. Sonnet 5.5 now gets `{type: "between_tools"}`. Haiku 5.5 keeps `disabled`, which it accepts at its default effort. Opus 5.5, Fable and Mythos get `effort: low`, since thinking can't be turned off there. Any future model that rejects the shape gets one automatic retry without it.
  - **"Test connection" no longer fails a valid key.** It previously sent `max_tokens: 5`. On 5.5 models thinking is on by default and counts toward that cap.
  - **Clearer errors.** A refusal (`stop_reason: "refusal"`) or a thinking-only truncation now produces a readable error instead of "no text blocks".
  - **Shared HTTP client.** The provider uses one shared client with connect and overall timeouts, instead of a new client per call with no timeout.
- `temperature` was already omitted for these models (allowlist), and the app stays on `anthropic-version: 2023-06-01`. The `2026-10-21` API version removes sampling parameters entirely; moving to it is a separate, optional step.

## Round 2 (same day): open findings fixed

The second pass followed staff-engineer and SOC 2 (Security, Confidentiality and Privacy criteria) guidance. Each change was reviewed by three independent reviewers (backend, frontend, CI); their findings were fixed before this was written.

**Webview and IPC**
- **Content Security Policy.** `script-src 'self'`; images and media only from the app and its asset protocol; no frames, objects or form posts. Tauri hashes the inline theme script at build time, which was checked in a debug build.
- **Navigation guard plugin.** The app window can only show the app. Links open in the system browser, http(s)/mailto only, validated again in Rust (`open_external_url`). An app-wide click guard also covers middle-click.
- **Remote images are never fetched.** This covers AI output (react-markdown overrides), notes (DOMPurify hook) and SVG/`<style>`/`srcset` vectors. Blocked images become placeholders that keep the URL, so editing a note never loses it.
- **Asset protocol** narrowed to `recordings/` and `attachments/`.
- **`get/set_app_setting`** allow-listed to `consent.*`, `capture.*` and `ui.*`.
- **Keyed providers' base URLs** must be https (loopback http allowed), checked on save and on use.
- **Path containment hardened against Windows normalisation tricks** (trailing dots and spaces, `a:stream`, `NUL`/`COM1`, `\?\`). This covers attachments (now only `attachments/` / `recordings/`) and recursive deletes. Programs and scripts are never launched from "open attachment".

**Data handling**
- Deletes are atomic transactions. SQLite `secure_delete=ON`, plus a WAL checkpoint after each delete.
- **Cloud processing needs recorded consent per provider.** A one-time disclosure names the processor and what is sent, and it's enforced in the backend so no UI path can skip it.
  - *Correction (v2.4):* until v2.4 the disclosure used `window.confirm`, which tauri-plugin-dialog makes async and which had no dialog permission. So it never showed, and the Promise it returned counted as "yes". Every consent recorded before v2.4, including the one-time migration for users already on a cloud provider, was recorded without the disclosure being seen.
  - v2.4 asks with an in-app dialog, clears all earlier consents once (`reset_unseen_cloud_consent`), and asks again at launch when the saved provider is a cloud one. A lint rule bans `confirm`/`alert`.
  - *Follow-up (after v2.4):* consent is now written only by the backend. `request_cloud_consent` shows the same disclosure in a native dialog the webview can't draw or click, and records only a click on its allow button. The decline button is the default, so a stray Enter declines. The webview can still read `consent.cloud_llm.*` but can't set it (`is_webview_writable_setting_key`), so a script can't skip the disclosure. Two declines for a provider within a minute stop its dialog for a minute, so a request loop can't keep a modal up. Consents recorded since v2.4 came from the then-working in-app prompt and are kept; there is no second reset.
- **MCP settings copy corrected.** Results are sent to the MCP client's model provider; they don't "stay on this machine".

**Supply chain and release (CC8)**
- **Lockfiles.** Release and CI use the committed lockfiles with `npm ci --ignore-scripts`. The regenerated frontend lockfile now carries every platform's native packages.
- **Secrets isolated.** The frontend is built in a job with no secrets. The signing job installs only the Tauri CLI, runs no npm scripts (`beforeBuildCommand` nulled), restores no caches, and sits behind `environment: release`.
- **Actions pinned.** Every action is pinned to a verified commit SHA, with least-privilege `permissions` per job and `persist-credentials: false`.
- **Release evidence.** Each release publishes CycloneDX SBOMs (npm + Cargo) and GitHub build-provenance attestations.
- **Gates.** CI runs `npm audit` (root + frontend) and `cargo-deny` (advisories, bans, sources; `deny.toml` documents each accepted advisory). Dependabot covers actions, npm and cargo.
- **Tauri 2.12.5** (Rust + JS in lockstep). tokio 1.54.1 (RUSTSEC-2026-0386) and clipboard-win 5.4.3 (yanked) updated.
- **Policy docs.** `SECURITY.md` (private reporting, SLAs) and `docs/THREAT_MODEL.md` (assets, trust boundaries, threat→control map, incident response).

**Claude prompting (per Anthropic's Sonnet/Haiku 5.5 guidance)**
- **Per-task effort.**
  - Sonnet 5.5: Enhance `medium`, Polish `high` (thinking off via `between_tools`), Extraction `low`, Ask `low`.
  - Haiku 5.5: `medium` / `low` / `low` / `low`.
  - Larger budgets, because thinking counts against `max_tokens`. Ask was at risk of empty answers at 2,048.
- **Structured outputs** (`output_config.format`) for Polish and Extraction. Array schemas are wrapped and unwrapped transparently. Enhance stays free-form until a measured bench run.
- **Server-side refusal fallback** (`fallbacks: "default"`) on Sonnet 5.5 via the Claude API. Haiku 5.5 has none and surfaces a clear message instead.
- **Stop reason checked first.** A refusal is always an error, and a cut-off structured answer is an error.
- **Graceful fallback.** A feature a model rejects is dropped and retried (matched on the API's error message only).
- **Prompt text.**
  - Meeting data is fenced in XML tags and declared non-instructional, and fence-closing tags inside the data are neutralised.
  - All-caps commands are restated with reasons.
  - A real person's name used as the shipped transcript-cleanup example was replaced with a neutral one.
  - Ask puts the transcript before the notes so note edits don't invalidate the prompt cache. History always starts and ends on a user turn.

## Round 3 (v2.3.0): remaining code items closed

- **Self-heal:** manifest paths are always rebased onto the scanned `recordings/<folder>`. Directory parts from a manifest are ignored, and names failing the safe-path rules reject the manifest. The portability backfill refuses rows whose folder isn't inside `recordings/`.
- **MCP server:**
  - It opens the existing database without migrating or seeding (`Storage::open_existing`, 5 s busy timeout, schema checks).
  - `set_speaker_label` only accepts existing speakers, with sanitised labels of at most 100 characters.
  - Query and result sizes are capped.
  - Keep-local meetings and notes are never returned: direct lookups are refused, and lists, search, items and briefings filter them out.
- **Calendar OAuth:** the loopback listener ignores stray local connections until the redirect with the matching `state` arrives.
- **Logs:** meeting titles, folder names, transcript text, window titles and calendar ids are kept out of log calls, and a test enforces the rule.
- **Privacy controls:**
  - *Keep local* per meeting or folder: no cloud LLM or cloud transcription, enforced in the backend.
  - Optional audio retention (7/30/90/365 days; transcripts and notes are kept).
  - *Delete all my data* (database, files, keychain entries; two confirmations).
  - A recording-notice helper with a "participants notified" flag.
  - A disk-encryption check with a warning banner.
- **Dependencies:**
  - zip 9.0.5 for the app's own extraction, hardened against path, symlink, permission and size tricks.
  - whisper.cpp v1.9.5 (CVE-2026-110810), CI-built on all platforms and published as prereleases.
  - ffmpeg n8.1.3, with every pin re-hashed and download-tested.
- **Live tests:** `src-tauri/tests/anthropic_live.rs` checks every Claude 5.5 request shape with the fallback switched off. Run it with your own key.

## Still open

| Item | Why it's open | Owner |
|---|---|---|
| Require reviewers on the `release` environment and move the signing secrets into it | GitHub setting | Repo owner |
| Rulesets for `main` and `v*` tags; secret scanning + push protection; private vulnerability reporting | GitHub settings | Repo owner |
| Windows Authenticode signing | The owner chose to skip it for now (it costs about $10/month on Azure; SignPath Foundation is free for OSS). Updates stay minisign-verified. | Repo owner (later) |
| Run the live Claude 5.5 test once with a real key | Needs the owner's API key | Repo owner |
| tauri-plugin-updater's own zip 4 copy (RUSTSEC-2026-0367/8/9) and quick-xml (build-time only) | Waiting on upstream; the updater only unpacks signature-verified updates | Upstream |
| Screen-recording videos aren't covered by audio retention; the WebView2 profile isn't wiped by delete-all; the FileVault check hasn't run on a real Mac | Small follow-ups | Code |

## Verified good practices
- API keys live in the OS keychain and are never returned to the webview (only `has_key`).
- Anthropic and Groq hosts are fixed.
- Tauri capabilities are minimal (no fs, shell or http plugins exposed to JS).
- The updater pubkey is pinned on an HTTPS endpoint.
- All runtime downloads are SHA-256 pinned.
- Every sidecar spawn uses argument vectors (no shell).
- SQL is parameterised and FTS queries sanitised.
- Scratchpad HTML goes through DOMPurify.
- No `pull_request_target`, and no secrets reach fork PRs.
- No committed secrets were found in the tree or history scan.
- No known-compromised npm versions (2025–2026 worm incidents, axios) are in either lockfile.
