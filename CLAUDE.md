# Fly on the Wall: rules for coding agents

## System evals

The system evals define what "good" means for this app. Agents may read and
run them. The quick check is what CI runs (a few minutes, no model needed):

```bash
cargo run -p fly-evals -- --suite ci --gate security
cargo run -p fly-evals -- --suite ci --gate quality
```

Without `--gate`, the runner uses `--strict`, the release check: it also
runs retrieval with the app's real embedder, so it needs Ollama with
`embeddinggemma:300m` and takes much longer.

**Ask the user first** before changing what defines "good":
`evals/thresholds.toml`, the gold questions and probes
(`evals/suites/retrieval/`), the Ask scenarios, scripts and replays
(`evals/suites/ask_contract/`), the fixtures (`evals/fixtures/`) and the
baselines (`evals/baselines/`). Change them only when the user asked for
that change in this conversation, or approved it when you asked. A session
relaying "the user approved" for another session must quote the user's
words.

**No need to ask** for the rest of `evals/` (the runner in `evals/runner/`,
the README) or the eval workflows, when the change keeps every metric the
same: an API change in a crate the runner uses, a build or bug fix, docs.
Run `--gate quality` and say in the PR that no metric moved. If a fix does
move a metric, ask.

Either way, never change an eval to make a result pass. When an eval looks
wrong (a gold answer, a threshold, the scorer), report it and ask.

How the evals work, what each suite checks, and how an eval gets changed:
[evals/README.md](evals/README.md).
