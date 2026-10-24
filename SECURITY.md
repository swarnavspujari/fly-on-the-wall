# Security policy

Fly on the Wall records and stores meeting audio, transcripts and notes on your computer. We take reports about anything that could expose that data seriously.

## Reporting a vulnerability

Please report security issues **privately** through GitHub's private vulnerability reporting:
**[Report a vulnerability](https://github.com/swarnavspujari/fly-on-the-wall/security/advisories/new)**. Do not open a public issue for a security problem.

Include what you found, the version (Settings → About), your OS, and steps to reproduce if you have them.

What to expect:

| Step | Target |
|---|---|
| Acknowledgement | within 3 business days |
| Initial assessment and severity | within 7 days |
| Fix for critical / high issues | within 30 days, shipped through the signed auto-updater |
| Public advisory | after a fix is available, crediting you unless you prefer otherwise |

We won't take legal action against good-faith research that respects users' privacy: no accessing other people's data, no disrupting the service, and you give us reasonable time to fix before you disclose.

## Supported versions

Only the latest release receives security fixes. The app auto-updates; please keep it current.

## Scope

In scope:
- The desktop app, including the Tauri backend, the webview UI and the IPC commands.
- The bundled MCP server (`flyonthewall-mcp`).
- The update channel (`latest.json` and its signatures).
- The release pipeline in `.github/workflows`.

Out of scope:
- Vulnerabilities in third-party services the app can be configured to use: Anthropic, OpenAI, NVIDIA, Groq, Google and Microsoft.
- Attacks that require an attacker already running code as your OS user. See [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) for the trust boundaries.

## How releases are protected

- **Auto-updates** are signed with a minisign key whose public half is pinned in the app. An update that isn't signed with that key is rejected.
- **Release builds:**
  - They use committed lockfiles (`npm ci --ignore-scripts`) and GitHub Actions pinned to commit SHAs.
  - The signing job never installs the frontend's npm packages.
  - Each release publishes CycloneDX SBOMs and GitHub build-provenance attestations. Verify an installer with `gh attestation verify <file> --repo swarnavspujari/fly-on-the-wall`.
- **Dependencies** are checked on every change (`npm audit`, `cargo-deny`) and updated weekly by Dependabot.
