# stats

Data only, no code. `.github/workflows/stats.yml` on `main` rewrites these files once a day:

- `stats.json`: headline download numbers. The landing page (`docs/index.html`) fetches it.
- `downloads-badge.json`: the Shields endpoint payload for the README downloads badge.
- `stats-history.jsonl`: one line per run, the long-term log (GitHub's traffic API keeps only 14 days).

The data lives here rather than on `main` because `main` requires the `ci-ok` check, and on a personal-account repo the Actions bot can't be exempted from that rule.
