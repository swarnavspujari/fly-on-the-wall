# Threat model

Last reviewed: 2026-10-23. Review it again when a release adds a new data flow, such as a cloud service, an IPC command that touches files, or a new import format.

## What we protect

| Asset | Where it lives | Sensitivity |
|---|---|---|
| Meeting audio | `recordings/` in the data folder | Highest: verbatim, includes other people |
| Transcripts and notes | `flyonthewall.db` (SQLite), `notes/`, `transcripts/` mirrors | High |
| API keys and OAuth tokens | OS keychain (`fly-secrets`) | High: billable, and they unlock calendars |
| Updater signing key | GitHub Actions secret, release environment only | Critical: it can push code to every install |
| Integrity of user files outside the data folder | the user's disk | High |

## Trust boundaries

```
 untrusted content ─┐                         ┌─ cloud LLM / ASR (opt-in, per provider)
 (transcript speech,│   ┌────────────────┐    │
  calendar text,    ├──▶│ Rust backend   │────┤─ calendar APIs (OAuth, read-only scopes)
  imported folders, │   │ (trusted)      │    │
  AI output)        │   └──────▲─────────┘    └─ GitHub releases (signed updates)
                    │          │ IPC (allow-listed commands, validated args)
                    └──▶ Webview UI (semi-trusted: renders untrusted content)
                               │
            MCP client (Claude Desktop, etc.) ◀── flyonthewall-mcp (read + one label write)
```

- **Speech is attacker-controlled.** Anyone in a meeting can say text that ends up in a transcript and then in an LLM prompt (prompt injection). The same goes for calendar titles and descriptions.
- **AI output is untrusted.** It is rendered as Markdown in the webview.
- **Imported recording folders are untrusted.** This includes `manifest.json` and `note.json` copied in from another machine.
- **The webview is semi-trusted.** It renders the content above, so the backend must not trust IPC arguments blindly.
- **Same-user malware is out of scope.** Anything running as the user can read the data folder and the keychain. We rely on OS full-disk encryption (BitLocker / FileVault) for data at rest.

## Threats and mitigations

| # | Threat | Mitigation | Status |
|---|---|---|---|
| T1 | Prompt injection makes the AI emit a remote image or link that leaks meeting content | Remote images are never loaded (react-markdown and DOMPurify overrides, plus CSP `img-src`). Links open only in the system browser, http(s)/mailto only. Prompts fence meeting data in XML tags and say it is never instructions. | Mitigated |
| T2 | Script injection (XSS) in the webview drives privileged IPC | Strict CSP (`script-src 'self'`) and DOMPurify on notes HTML; no raw HTML in Markdown rendering. Tauri capabilities are minimal: no fs, shell or http plugins exposed to JS. | Mitigated |
| T3 | XSS or IPC abuse opens, launches or deletes arbitrary files | Attachment paths must stay inside the data folder. Recursive deletes only ever touch `recordings/<folder>`. The generic settings commands are allow-listed to `consent.*`, `capture.*` and `ui.*`; cloud-AI consent (`consent.cloud_llm.*`) is read-only there. | Mitigated |
| T4 | A stored API key is redirected to an attacker host | Custom base URLs for keyed providers must be https (http only for loopback), checked both on save and on use. Anthropic and Groq hosts are fixed. | Mitigated |
| T5 | The app window is navigated to a phishing page | A navigation guard plugin allows only the app's own origin and sends external links to the browser. | Mitigated |
| T6 | Webview script reads the DB or logs over the asset protocol | The asset scope covers only `recordings/` and `attachments/`. | Mitigated |
| T7 | A crafted manifest in a copied-in folder makes a delete escape the data folder | `contained_recordings_dir` guard on every `remove_dir_all`. | Mitigated. Follow-up: rebase all manifest paths during self-heal. |
| T8 | A compromised npm package or GitHub Action steals the updater key | Lockfiles with `npm ci --ignore-scripts`; actions pinned by SHA; the frontend is built in a job with no secrets; the release environment can require reviewers; Dependabot and audit gates. | Mitigated. Owner action needed: environment reviewers. |
| T9 | A tampered installer at first install | Updates are minisign-verified; macOS builds are notarized; releases carry provenance attestations. | Partial: Windows installers aren't Authenticode-signed yet. |
| T10 | Meeting data sent to a cloud processor without the user realising | Local by default (Ollama, local whisper). A one-time disclosure appears per cloud LLM provider, in a native dialog; only the backend records the answer, so a webview script can't forge consent. The MCP copy states that results go to the client's model provider. Per-note and per-folder "Keep local", enforced in the backend: cloud LLM providers are refused and Groq is skipped for those meetings. | Mitigated. The MCP server can still return keep-local meetings to its client. |
| T11 | Deleted meetings recoverable from the DB file | Atomic deletes, and SQLite `secure_delete=ON`. | Mitigated. Audio files rely on FDE. |
| T12 | Recording without other participants' consent (two-party-consent laws) | First-run notice. A non-blocking "Let participants know" strip at record start (copy a chat notice, tick "Participants notified", stored on the meeting). | Mitigated; the user stays responsible for consent. |
| T13 | Data recoverable from a stolen or lost laptop | Relies on OS full-disk encryption; Settings → Privacy warns when BitLocker / FileVault is clearly off. Optional audio retention (delete recordings after 7–365 days) and "Delete all my data" (data folder + keychain entries) limit what's there to find. | Accepted risk; documented. |

## Incident response

- **Updater key compromise:**
  1. Revoke the GitHub secret.
  2. Ship a bridge release, signed with the old key, that pins a new public key.
  3. Rotate the key.
  4. Publish an advisory.
- **OAuth client compromise:** rotate the Google and Microsoft client secrets in their consoles and CI, then ship a release.
- **Vulnerable dependency:** Dependabot or an audit gate flags it. Patch, release through the updater, and publish an advisory if users were exposed.
