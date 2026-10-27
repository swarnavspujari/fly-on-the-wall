#!/usr/bin/env bash
# Print the changed files that need a Claude security review, one per line.
# The rules live in paths.txt next to this script (git pathspecs, glob magic).
#
# Usage: relevant-files.sh <git diff revisions...>
#   relevant-files.sh origin/main...HEAD     # a branch's own changes
#   relevant-files.sh <reviewed-sha> HEAD    # what changed since a review
#
# Used by the scope job of .github/workflows/security-review.yml and by hand.
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <git diff revisions...>" >&2
  exit 2
fi

# PATHS_FILE: another copy of the list (scope.sh passes the base branch's and
# the PR's copies in turn).
list=${PATHS_FILE:-"$(cd "$(dirname "$0")" && pwd)/paths.txt"}
specs=()
while IFS= read -r line || [ -n "$line" ]; do
  line="${line%$'\r'}"
  # trim surrounding spaces
  line="${line#"${line%%[![:space:]]*}"}"
  line="${line%"${line##*[![:space:]]}"}"
  case "$line" in
    "" | "#"*) continue ;;
    "!"*) specs+=(":(exclude,glob)${line#!}") ;;
    *) specs+=(":(glob)$line") ;;
  esac
done <"$list"

if [ "${#specs[@]}" -eq 0 ]; then
  echo "relevant-files.sh: $list lists no paths" >&2
  exit 1
fi

# --no-renames: a moved file shows as delete + add, so both paths are checked.
git -c core.quotePath=false -C "$(git rev-parse --show-toplevel)" diff --name-only --no-renames "$@" -- "${specs[@]}"
