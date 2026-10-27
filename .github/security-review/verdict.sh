#!/usr/bin/env bash
# Turn the review step's structured output into the check result and one
# summary comment on the PR. Run by the review job of security-review.yml,
# from the BASE branch's copy of this script.
#
# Red (exit 1) when the review did not finish: the step failed, or Claude
# returned no readable verdict (or one that lists no reviewed file). Green
# otherwise, unless FAIL_ON_HIGH_FINDINGS is "true" and a high or critical
# finding was reported.
#
# Env: OUTCOME VERDICT PR REPO HEAD_SHA SINCE RUN_URL FAIL_ON_HIGH_FINDINGS
#      GH_TOKEN; NO_COMMENT=1 skips posting (local test).
# Credential-shaped strings in the review text are replaced with [REDACTED]
# before posting.
set -euo pipefail

summary=${GITHUB_STEP_SUMMARY:-/dev/stdout}
auth="Workload Identity Federation"

if [ "${OUTCOME:-}" != "success" ] ||
  ! jq -e 'type == "object" and (.findings | type == "array") and (.reviewed_files | type == "array") and (.reviewed_files | length > 0)' >/dev/null 2>&1 <<<"${VERDICT:-}"; then
  if [ -n "${VERDICT:-}" ]; then state=unreadable; else state=missing; fi
  msg="The Claude security review did not finish (review step: ${OUTCOME:-not run}; verdict: $state). This check stays red until a review completes: read the review step's log, fix the cause, then re-run the job."
  echo "::error::$msg"
  printf '### Security review: did not finish\n\n%s\n\nAuth: %s.\n' "$msg" "$auth" >>"$summary"
  exit 1
fi

n=$(jq '.findings | length' <<<"$VERDICT")
high=$(jq '[.findings[] | select(.severity == "critical" or .severity == "high")] | length' <<<"$VERDICT")
files=$(jq '.reviewed_files | length' <<<"$VERDICT")
if [ -n "${SINCE:-}" ]; then scope="commits since \`${SINCE:0:7}\`"; else scope="the whole PR"; fi

if [ "$n" -eq 0 ]; then
  title="no findings"
elif [ "$high" -gt 0 ]; then
  title="$n finding(s), $high high or critical"
else
  title="$n finding(s)"
fi

body=$(mktemp)
{
  echo "<!-- claude-security-review sha=$HEAD_SHA -->"
  echo "### Claude security review: $title"
  echo
  echo "Reviewed \`${HEAD_SHA:0:7}\` ($scope, $files file(s)) · [run]($RUN_URL)"
  echo
  jq -r '.summary' <<<"$VERDICT"
  if [ "$n" -gt 0 ]; then
    echo
    echo "| Severity | Where | Finding | Inline comment |"
    echo "|---|---|---|---|"
    jq -r '
      def cell: gsub("\\|"; "\\|") | gsub("[\r\n]+"; " ");
      {"critical": 0, "high": 1, "medium": 2, "low": 3} as $rank
      | .findings | sort_by($rank[.severity]) | .[]
      | "| \(.severity | ascii_upcase) | `\(.file):\(.line)` | \(.title | cell) | \(if .inline_comment_posted then "yes" else "no (details below)" end) |"
    ' <<<"$VERDICT"
    jq -r '
      .findings[] | select(.inline_comment_posted | not)
      | "\n**[\(.severity | ascii_upcase)] \(.title)** · `\(.file):\(.line)`\n\n**Exploit scenario:** \(.exploit_scenario)\n\n**Fix:** \(.fix)"
    ' <<<"$VERDICT"
    echo
    echo "Answer each finding in the PR (fixed, or why not) before merging."
  fi
} >"$body"

# Never post something shaped like a credential (Anthropic keys and access
# tokens, GitHub tokens, JWTs such as the OIDC identity token, git's basic-auth
# header and token URLs), whatever the review text says. Inline comments are
# redacted by the action itself.
sed -E -i \
  -e 's/sk-ant-[A-Za-z0-9_-]{8,}/[REDACTED]/g' \
  -e 's/(gh[pousr]_|github_pat_)[A-Za-z0-9_]{20,}/[REDACTED]/g' \
  -e 's/eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/[REDACTED]/g' \
  -e 's/([Bb][Aa][Ss][Ii][Cc] +)[A-Za-z0-9+\/=]{16,}/\1[REDACTED]/g' \
  -e 's/(x-access-token:)[^@[:space:]]+/\1[REDACTED]/g' \
  "$body"

# GitHub caps a comment at 65,536 characters.
if [ "$(wc -c <"$body")" -gt 60000 ]; then
  head -c 60000 "$body" >"$body.cut"
  printf '\n\n(cut at 60,000 characters; the full list is in the run summary)\n' >>"$body.cut"
  post="$body.cut"
else
  post="$body"
fi

if [ "${NO_COMMENT:-}" != "1" ]; then
  gh pr comment "$PR" --repo "$REPO" --body-file "$post" >/dev/null
fi
{ cat "$body"; printf '\nAuth: %s.\n' "$auth"; } >>"$summary"
echo "::notice::Security review done: $title (auth: $auth)."

if [ "${FAIL_ON_HIGH_FINDINGS:-false}" = "true" ] && [ "$high" -gt 0 ]; then
  echo "::error::$high high or critical finding(s); FAIL_ON_HIGH_FINDINGS is on, so this check is red until they are fixed."
  exit 1
fi
