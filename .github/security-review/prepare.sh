#!/usr/bin/env bash
# Write what the Claude reviewer reads into .security-review-context/, because
# it gets no shell (only Read / Glob / Grep and the inline-comment tool). Run by
# the review job of security-review.yml before Claude, from the BASE branch's
# copy of this script.
#
# The rule files come from the same base-branch copy (RULES_DIR), not from the
# PR, so a PR cannot loosen the rules it is reviewed against. Changes the PR
# makes to them are themselves reviewed.
#
# Env: GH_TOKEN REPO PR BASE_SHA HEAD_SHA SINCE (may be empty) FILES
#      (newline-separated) RULES_DIR
set -euo pipefail

dir=.security-review-context
rm -rf "$dir"
mkdir -p "$dir/rules"

specs=()
while IFS= read -r f; do
  [ -n "$f" ] || continue
  # git still C-quotes names with quotes, backslashes or control characters;
  # such a name would not match below and its changes would silently drop out
  # of the review.
  case "$f" in
    \"*) echo "::error::Unusual file name ($f): rename it, or review this PR by hand."; exit 1 ;;
  esac
  specs+=(":(literal)$f")
done <<<"$FILES"
if [ "${#specs[@]}" -eq 0 ]; then
  echo "::error::The scope job asked for a review but listed no files."
  exit 1
fi

diff_args=(-c core.quotePath=false diff --no-color --no-ext-diff --no-renames)
git "${diff_args[@]}" "$BASE_SHA...$HEAD_SHA" -- "${specs[@]}" >"$dir/pr.diff"
sections=$(grep -c '^diff --git ' "$dir/pr.diff" || true)
if [ "$sections" -ne "${#specs[@]}" ]; then
  echo "::error::pr.diff has $sections file section(s) for ${#specs[@]} file(s) to review; refusing to review a partial diff."
  exit 1
fi
if [ -n "${SINCE:-}" ]; then
  git "${diff_args[@]}" "$SINCE" "$HEAD_SHA" -- "${specs[@]}" >"$dir/since-last-review.diff"
fi
git log --no-color --date=short --format='%h %ad %an: %s' "$BASE_SHA..$HEAD_SHA" -- >"$dir/commits.txt"
printf '%s\n' "$FILES" | sed '/^$/d' >"$dir/files.txt"

cp "$RULES_DIR/custom-scan-instructions.txt" "$RULES_DIR/false-positive-filtering.txt" "$dir/rules/"

# shellcheck disable=SC2016 # literal backticks: Markdown for the reviewer
gh api --paginate "repos/$REPO/pulls/$PR/comments" \
  --jq '.[] | select(.user.login == "github-actions[bot]") | "- `\(.path):\(.line // .original_line)` \(.body | split("\n")[0])"' \
  >"$dir/earlier.tmp"
if [ -s "$dir/earlier.tmp" ]; then sed -n '1,200p' "$dir/earlier.tmp" >"$dir/earlier-comments.md"; else echo "none" >"$dir/earlier-comments.md"; fi
rm -f "$dir/earlier.tmp"

echo "Prepared for review: $(wc -l <"$dir/files.txt") file(s), pr.diff $(wc -l <"$dir/pr.diff") lines$([ -n "${SINCE:-}" ] && echo ", since-last-review.diff $(wc -l <"$dir/since-last-review.diff") lines")."
