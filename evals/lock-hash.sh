#!/usr/bin/env bash
# Prints the eval lock hash of a git revision (default: HEAD), the value the
# EVALS_LOCK_SHA256 repository variable must hold. Same algorithm as
# .github/workflows/evals-guard.yml; change both together (a locked change).
#
#   bash evals/lock-hash.sh            # HEAD
#   bash evals/lock-hash.sh origin/main
#
# On Windows, run it with Git Bash ("C:\Program Files\Git\bin\bash.exe"),
# not WSL's bash: WSL sees the repository through /mnt/c with its own git,
# which may be missing or configured differently.
#
# SHA-256 of `git ls-tree -r` over the protected paths (mode, type, blob id,
# path per line, paths quoted as core.quotePath=true does), sorted. Blob ids
# make it independent of line endings and checkout settings.
set -euo pipefail
rev="${1:-HEAD}"
cd "$(git rev-parse --show-toplevel)"
if command -v sha256sum >/dev/null 2>&1; then
  sha() { sha256sum; }
else
  sha() { shasum -a 256; } # macOS
fi
git -c core.quotePath=true ls-tree -r --full-tree "$rev" -- \
  evals \
  .github/workflows/evals.yml \
  .github/workflows/evals-guard.yml \
  .claude/settings.json \
  .claude/hooks |
  LC_ALL=C sort | sha | cut -d' ' -f1
