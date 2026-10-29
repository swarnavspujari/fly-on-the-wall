# Threat model

Last reviewed: 2026-10-29 (connectors beta added). Review it again when a release adds a new data flow, such as a cloud service, an IPC command that touches files, or a new import format.

## What we protect

| Asset | Where it lives | Sensitivity |
|---|---|---|
| Meeting audio | `recordings/` in the data folder | Highest: verbatim, includes other people |
| Transcripts and notes | `flyonthewall.db` (SQLite), `notes/`, `transcripts/` mirrors | High |
| API keys and OAuth tokens | OS keychain (`fly-secrets`) | High: billable, and they unlock calendars and connected services |
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

 Ask agent ──(confirm card, user clicks Run)──▶ connector MCP servers (beta, user-added;
                                                 untrusted in both directions)
```

- **Speech is attacker-controlled.** Anyone in a meeting can say text that ends up in a transcript and then in an LLM prompt (prompt injection). The same goes for calendar titles and descriptions.
- **AI output is untrusted.** It is rendered as Markdown in the webview.
- **Imported recording folders are untrusted.** This includes `manifest.json` and `note.json` copied in from another machine.
- **The webview is semi-trusted.** It renders the content above, so the backend must not trust IPC arguments blindly.
- **Connector servers are untrusted.** A server the user added (beta) can return any text, change its tools after approval, or ask the client for things. With connectors, the Ask agent combines private data, attacker-controllable input and an outbound channel, so every call needs the user's click.
- **Same-user malware is out of scope.** Anything running as the user can read the data folder and the keychain. We rely on OS full-disk encryption (BitLocker / FileVault) for data at rest.

## Threats and mitigations

| # | Threat | Mitigation | Status |
|---|---|---|---|
| T1 | Prompt injection makes the AI emit a remote image or link that leaks meeting content | Remote images are never loaded (react-markdown and DOMPurify overrides, plus CSP `img-src`). Links open only in the system browser, http(s)/mailto only. Prompts fence meeting data in XML tags and say it is never instructions. | Mitigated |
| T2 | Script injection (XSS) in the webview drives privileged IPC | Strict CSP (`script-src 'self'`) and DOMPurify on notes HTML; no raw HTML in Markdown rendering. Tauri capabilities are minimal: no fs, shell or http plugins exposed to JS. | Mitigated |
| T3 | XSS or IPC abuse opens, launches or deletes arbitrary files | Attachment paths must stay inside the data folder. Recursive deletes only ever touch `recordings/<folder>`. The generic settings commands are allow-listed to `consent.*`, `capture.*` and `ui.*`; cloud-AI consent (`consent.cloud_llm.*`) is read-only there. | Mitigated |
| T4 | A stored API key is redirected to an attacker host | Custom base URLs for keyed providers must be https (http only for loopback), checked both on save and on use. A saved key stays with the server it was entered for: moving a keyed provider's base URL to another scheme, host or port is refused unless the key is entered again with it, so the app UI can't point a saved key elsewhere, and such a move clears that provider's cloud consent, so the native dialog (naming the new server) is shown again before anything is sent. Anthropic and Groq hosts are fixed. | Mitigated |
| T5 | The app window is navigated to a phishing page | A navigation guard plugin allows only the app's own origin and sends external links to the browser. | Mitigated |
| T6 | Webview script reads the DB or logs over the asset protocol | The asset scope covers only `recordings/` and `attachments/`. | Mitigated |
| T7 | A crafted manifest in a copied-in folder makes a delete escape the data folder | `contained_recordings_dir` guard on every `remove_dir_all`. Self-heal rebases every manifest path onto the folder it was found in and accepts only lowercase plain ids (`safe_path::is_safe_id`). Every write, rename or delete built from a stored id or path is contained to its own app folder (`attachments/<note>`, `notes/`, `recordings/<folder>`). | Mitigated |
| T8 | A compromised npm package or GitHub Action steals the updater key | Lockfiles with `npm ci --ignore-scripts`; actions pinned by SHA; the frontend is built in a job with no secrets; the release environment can require reviewers; Dependabot and audit gates. | Mitigated. Owner action needed: environment reviewers. |
| T9 | A tampered installer at first install | Updates are minisign-verified; macOS builds are notarized; releases carry provenance attestations. | Partial: Windows installers aren't Authenticode-signed yet. |
| T10 | Meeting data sent to a cloud processor without the user realising | Local by default (Ollama, local whisper). A one-time disclosure appears per cloud LLM provider, in a native dialog; only the backend records the answer, so a webview script can't forge consent. The MCP copy states that results go to the client's model provider. Per-note and per-folder "Keep local", enforced in the backend: cloud LLM providers are refused and Groq is skipped for those meetings. Keep local travels in each meeting's portability manifest, so self-heal and a copied folder restore it; a manifest that doesn't record it heals as kept local. | Mitigated. The MCP server can still return keep-local meetings to its client. A script in the webview (it would need an XSS first; CSP is `script-src 'self'`) can still ask for the consent dialog again after a decline; two declines within a minute stop it for a minute. |
| T11 | Deleted meetings recoverable from the DB file | Atomic deletes, and SQLite `secure_delete=ON`. | Mitigated. Audio files rely on FDE. |
| T12 | Recording without other participants' consent (two-party-consent laws) | First-run notice. A non-blocking "Let participants know" strip at record start (copy a chat notice, tick "Participants notified", stored on the meeting). | Mitigated; the user stays responsible for consent. |
| T13 | Data recoverable from a stolen or lost laptop | Relies on OS full-disk encryption; Settings → Privacy warns when BitLocker / FileVault is clearly off. Optional audio retention (delete recordings after 7–365 days) and "Delete all my data" (data folder + keychain entries) limit what's there to find. | Accepted risk; documented. |
| T14 | Prompt injection (in speech, notes or a connector result) makes the Ask agent send meeting content out through a connector | Connectors are a beta, off by default, and each tool has a permission the user sets: Blocked, Needs approval (the default) or Always allow. Needs approval shows a card with the exact arguments, which the user can edit, and runs only on a click; a call to a tool that can change data also needs a yes in a native dialog that shows the final arguments in full. Always allow can only be set by the user in a native dialog that warns meeting speech could steer the AI; for tools that can change data, only from Settings. No connector tools on small local models, or when a turn's scope includes Keep-local notes (a tripwire also stops calls, Always allow included, if such content appears mid-turn). Once a connector result or error is in the turn, no further proposal can be made in it. Per-turn call budgets. Arguments with hidden characters are refused. | Mitigated; the user decides each call, or chooses Always allow per tool. |
| T15 | A connector server injects instructions through tool descriptions or results, or changes or adds tools after approval | Tool definitions are pinned by hash; a changed tool is blocked and its diff shown for review, a tool added later is blocked until reviewed, and the model only ever sees reviewed definitions. Approving a change returns the tool to Needs approval, never Always allow. Hidden characters are removed from descriptions, and schemas containing hidden or over-long text are refused. Results keep text only, must match a declared output schema, have links reduced to their host, are size-capped and wrapped in a fence that marks them as data; server errors go through the same fence. A server's labels decide only the group (unlabelled tools count as able to change data) and whether a Needs-approval call also needs the native confirm; they never grant Always allow. A server could label a tool that changes data as read-only: its calls then still wait for the user's click on a card showing the exact arguments, but skip the second, native confirm. The client declares no sampling, roots or elicitation and refuses such requests. | Mitigated |
| T16 | A connector config or OAuth discovery reaches internal services (SSRF, DNS rebinding, metadata endpoints) | https only. Private, loopback, link-local and reserved addresses are refused in URLs, in DNS answers and at every redirect hop; discovered OAuth endpoints are checked before use; the MCP transport follows no redirects. Sign-in happens in the user's browser with PKCE and a one-shot loopback listener. | Mitigated. Through a system proxy, the proxy resolves names, so only the URL checks apply. |
| T17 | Connector trust is granted without the user: a webview script or the model adds or edits a connector, turns one back on, loosens a permission or approves a changed tool | Connector state lives in backend-only settings that the webview can't read or write. Every trust grant happens in the backend after a yes in a native dialog whose default button declines; approvals are bound to a hash of the exact configuration, and a loosening is applied only if it matches what the dialog showed. The model can't reach configuration at all. Secret values (API keys, secret headers and settings, tokens) go to the keychain and are never returned to the webview; access and refresh tokens are stored separately; removing a connector or "Delete all my data" deletes them. Local programs start only after approval, without a shell and with a cleared environment. Importing from Claude Desktop reads its config and never writes it. | Mitigated. The beta switch itself is a UI preference: it shows the feature but grants nothing. |

## Incident response

- **Updater key compromise:**
  1. Revoke the GitHub secret.
  2. Ship a bridge release, signed with the old key, that pins a new public key.
  3. Rotate the key.
  4. Publish an advisory.
- **OAuth client compromise:** rotate the Google and Microsoft client secrets in their consoles and CI, then ship a release.
- **Vulnerable dependency:** Dependabot or an audit gate flags it. Patch, release through the updater, and publish an advisory if users were exposed.
