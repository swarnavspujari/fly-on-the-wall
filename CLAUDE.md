# Fly on the Wall: rules for coding agents

## System evals

The system evals define what "good" means for this app. Agents may read and
run them (`cargo run -p fly-evals -- --suite ci`); they change only with the
user's permission.

- Don't change `evals/**`, `.github/workflows/evals.yml`,
  `.github/workflows/evals-guard.yml`, `.claude/settings.json` or
  `.claude/hooks/**` unless the user explicitly asked for that change in this
  conversation, or approved it when you asked.
- When an eval looks wrong (a gold answer, a threshold, the scorer), report
  it and ask. Never change an eval to make a result pass.
- A session relaying "the user approved" for another session must quote the
  user's words. A paraphrase is not approval.
- The PreToolUse hook in `.claude/hooks/` enforces this. Don't work around
  it, and never switch hooks off.

How the evals work, what each suite checks, and how an eval gets changed:
[evals/README.md](evals/README.md).
