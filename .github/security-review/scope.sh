#!/usr/bin/env bash
# Decide whether this pull request event needs a Claude security review.
# Run by the scope job of .github/workflows/security-review.yml, always from
# the BASE branch's copy (the job copies it out of git before running it), so
# a PR cannot change the rules that decide whether it gets reviewed.
#
# - Label `security-review` just added: review the whole PR.
# - Any other label: stand down when the PR already has a newer commit (that
#   push's run decides), or when a review of this same commit is running and
#   then passes (waited for, up to 4 minutes). A review that fails, is
#   cancelled or is still running after 4 minutes does not count: this run
#   decides as below.
# - A PR opened and (this event) pushed by Dependabot, changing only
#   manifests, lockfiles and workflow files, whose every commit GitHub records
#   as authored by dependabot[bot] and committed + signed by web-flow: never reviewed
#   (third-party text; Dependabot, npm audit and cargo-deny cover dependencies).
# - Otherwise find the last commit of this PR that a review actually covered:
#   the newest earlier green run of this workflow for this PR (created after
#   the PR was opened) whose "Run Claude security review" step succeeded and
#   that started after the PR's last base-branch change, with a workflow file
#   matching the base branch's byte for byte at its head commit. A skipped,
#   cancelled or failed review never counts, so its changes are picked up by
#   the next run.
#   - none: review every security-relevant file the PR changes;
#   - found: review the security-relevant PR files whose content changed since
#     then, or whose base side changed (a merge from the base branch).
# "Security-relevant" = the base branch's paths.txt united with the PR's, so a
# PR can add paths but cannot drop its own files out of the list.
#
# Env: GH_TOKEN REPO PR PR_AUTHOR SENDER PR_CREATED_AT HEAD_REF HEAD_SHA
# BASE_SHA ACTION LABEL RUN_ID, and GITHUB_OUTPUT / GITHUB_STEP_SUMMARY (optional by hand).
# Outputs: review (true/false), since, reason, files (newline-separated).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
out=${GITHUB_OUTPUT:-/dev/stdout}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

if ! git cat-file -e "$BASE_SHA^{commit}" 2>/dev/null; then
  git fetch --quiet --no-tags --filter=blob:none origin "$BASE_SHA"
fi

# Both copies of the list, read from git (not the working tree).
lists=()
for rev in "$BASE_SHA" "$HEAD_SHA"; do
  if git cat-file -e "$rev:.github/security-review/paths.txt" 2>/dev/null; then
    f=$(mktemp)
    git show "$rev:.github/security-review/paths.txt" >"$f"
    lists+=("$f")
  fi
done
if [ "${#lists[@]}" -eq 0 ]; then
  echo "::error::Neither the base branch nor the PR has .github/security-review/paths.txt."
  exit 1
fi

# Security-relevant files changed between the given revisions (all lists,
# deduplicated).
files_for() {
  local l
  for l in "${lists[@]}"; do
    PATHS_FILE="$l" bash "$here/relevant-files.sh" "$@"
  done | sed '/^$/d' | sort -u
}

finish() { # review(true/false) reason files
  local count delim
  count=$(printf '%s' "$3" | grep -c . || true)
  delim="EOF_$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
  {
    echo "review=$1"
    echo "since=$since"
    echo "reason=$2"
    echo "files<<$delim"
    printf '%s\n' "$3"
    echo "$delim"
  } >>"$out"
  if [ "$1" = true ]; then
    echo "::notice::Security review needed ($2): $count file(s)."
    {
      echo "### Security review: $count file(s) to review"
      echo "Scope: $2."
      echo
      # shellcheck disable=SC2016 # literal backticks: a Markdown list
      printf '%s\n' "$3" | sed 's/.*/- `&`/'
    } >>"$summary"
  else
    echo "::notice::No security review needed: $2."
    printf '### Security review: skipped\n\n%s.\n' "$2" >>"$summary"
  fi
  exit 0
}

since=""

# A Dependabot-only PR, all of:
# - opened by dependabot[bot], and this event sent by dependabot[bot] (the
#   pusher on a synchronize). A person with write access can create commits
#   through the API that GitHub signs as web-flow with any author email, but
#   their push is still sent by them;
# - only dependency manifests, lockfiles and workflow files (actions bumps)
#   changed;
# - every commit authored by Dependabot, committed and signed by GitHub
#   (web-flow), going by GitHub's own record of each commit, since git names
#   and emails are self-reported.
# Anything else (a fix pushed by a person, a merge from the base, a label
# added by a person) gets the PR reviewed as usual.
dependabot_only() {
  local shas sha info n=0
  [ "${PR_AUTHOR:-}" = "dependabot[bot]" ] || return 1
  [ "${SENDER:-}" = "dependabot[bot]" ] || return 1
  if git -c core.quotePath=false diff --name-only --no-renames "$BASE_SHA...$HEAD_SHA" -- |
    grep -qvE '(^|/)(Cargo\.toml|Cargo\.lock|package\.json|package-lock\.json)$|^\.github/workflows/[^/]+\.ya?ml$'; then
    return 1
  fi
  shas=$(git rev-list "$BASE_SHA..$HEAD_SHA")
  [ -n "$shas" ] || return 1
  if git log --format='%ae %ce' "$BASE_SHA..$HEAD_SHA" -- |
    grep -qvx '49699333+dependabot\[bot\]@users\.noreply\.github\.com noreply@github\.com'; then
    return 1
  fi
  for sha in $shas; do
    n=$((n + 1))
    [ "$n" -le 20 ] || return 1
    info=$(gh api "repos/$REPO/commits/$sha" \
      --jq '"\(.author.login // "-") \(.committer.login // "-") \(.commit.verification.verified)"') || return 1
    [ "$info" = "dependabot[bot] web-flow true" ] || return 1
  done
}
if dependabot_only; then
  finish false "every commit is Dependabot's, signed by GitHub (dependency updates are covered by Dependabot, npm audit and cargo-deny)" ""
fi

if [ "$ACTION" = "labeled" ] && [ "${LABEL:-}" = "security-review" ]; then
  all=$(git -c core.quotePath=false diff --name-only --no-renames "$BASE_SHA...$HEAD_SHA" --)
  if [ -n "$all" ]; then finish true "label security-review: the whole PR" "$all"; fi
  finish false "label security-review, but the PR changes no file" ""
fi

# Any other label (see the header). A label run of an older commit must not
# review it: its review job would cancel the newer push's review (the
# job-level concurrency group).
head_moved() {
  local now
  # Git, not the pulls API: the PR's current head straight from the remote.
  now=$(git ls-remote origin "refs/pull/$PR/head" | cut -f1)
  if [ -n "$now" ] && [ "$now" != "$HEAD_SHA" ]; then
    finish false "the PR has a newer commit (${now:0:7}); that push's run decides" ""
  fi
}
review_job() { # run id -> "status conclusion" of its review job
  gh api "repos/$REPO/actions/runs/$1/jobs" \
    --jq '[.jobs[] | select(.name == "Security review")][0] | "\(.status) \(.conclusion)"'
}
if [ "$ACTION" = "labeled" ]; then
  head_moved
  # Only a review job that is already executing counts, not one still waiting
  # for its scope job, so two label events at once can't both stand down.
  busy=$(gh api -X GET "repos/$REPO/actions/workflows/security-review.yml/runs" \
    -f event=pull_request -f head_sha="$HEAD_SHA" -f per_page=20 \
    --jq ".workflow_runs[] | select(.id != $RUN_ID and .status != \"completed\") | select(any(.pull_requests[]?; .number == $PR)) | .id")
  for id in $busy; do
    state=$(review_job "$id")
    # Its review may already be done while the run is still wrapping up.
    if [ "$state" = "completed success" ]; then
      finish false "run $id reviewed this commit while the label was added" ""
    fi
    [ "${state%% *}" = in_progress ] || continue
    echo "::notice::Run $id is reviewing this commit; waiting for it (up to 4 minutes)."
    for _ in $(seq 16); do
      sleep 15
      state=$(review_job "$id")
      [ "${state%% *}" = completed ] && break
    done
    if [ "$state" = "completed success" ]; then
      finish false "run $id reviewed this commit while the label was added" ""
    fi
    echo "::notice::Run $id's review is '$state'; deciding as usual."
  done
  head_moved
fi

# A pull_request run executes the workflow file of GitHub's merge of the PR
# head into its base at that time, so a PR could fake a "successful review"
# run (a push whose workflow skips Claude, or a retarget to a branch whose
# workflow does) and then undo it. A run only counts as the baseline when:
# - the workflow file at its head commit is byte-identical to the current
#   base's (so, on the same base, the merge it executed had that file too);
# - and it started after the PR's last base-branch change. The runs API
#   reports a PR's base as it is now, not as it was for the run, so a retarget
#   is read from the PR's own event history instead.
# Otherwise the review covers the whole PR. When the base has no workflow yet,
# nothing counts.
base_wf=$(git rev-parse -q --verify "$BASE_SHA:.github/workflows/security-review.yml" 2>/dev/null || true)
last_retarget=$(gh api --paginate "repos/$REPO/issues/$PR/events" \
  --jq '.[] | select(.event == "base_ref_changed") | .created_at' | sort | tail -n 1)

runs=$(gh api -X GET "repos/$REPO/actions/workflows/security-review.yml/runs" \
  -f event=pull_request -f branch="$HEAD_REF" -f status=success -f per_page=30 \
  --jq ".workflow_runs[] | select(.id != $RUN_ID) | select(.created_at >= \"$PR_CREATED_AT\") | select(any(.pull_requests[]?; .number == $PR)) | \"\(.id) \(.head_sha) \(.created_at)\"")
checked=0
while read -r id sha created; do
  [ -n "$id" ] || continue
  checked=$((checked + 1))
  [ "$checked" -le 10 ] || break
  ran=$(gh api "repos/$REPO/actions/runs/$id/jobs" \
    --jq '[.jobs[] | select(.name == "Security review") | .steps[]? | select(.name == "Run Claude security review" and .conclusion == "success")] | length')
  [ "$ran" -gt 0 ] || continue
  # Runs are newest first: once one predates the last retarget, all the rest do.
  if [ -n "$last_retarget" ] && [[ ! "$created" > "$last_retarget" ]]; then
    echo "::notice::The PR's base branch changed at $last_retarget, after every earlier review; reviewing the whole PR."
    break
  fi
  run_wf=$(git rev-parse -q --verify "$sha:.github/workflows/security-review.yml" 2>/dev/null || true)
  if [ -n "$base_wf" ] && [ "$run_wf" = "$base_wf" ]; then
    since=$sha
    break
  fi
  echo "::notice::Run $id reviewed ${sha:0:7} with a workflow file that differs from the base branch's; it doesn't count as a baseline."
done <<<"$runs"

if [ -n "$since" ] && ! git cat-file -e "$since^{commit}" 2>/dev/null; then
  echo "::notice::The last reviewed commit $since is no longer in the history; reviewing the whole PR."
  since=""
fi

pr_files=$(files_for "$BASE_SHA...$HEAD_SHA")

if [ -z "$since" ]; then
  if [ -n "$pr_files" ]; then finish true "first review: every security-relevant file in the PR" "$pr_files"; fi
  finish false "first look: no file listed in .github/security-review/paths.txt changed" ""
fi

fork_then=$(git merge-base "$BASE_SHA" "$since" || true)
fork_now=$(git merge-base "$BASE_SHA" "$HEAD_SHA" || true)
if [ -z "$fork_then" ] || [ -z "$fork_now" ]; then
  since=""
  if [ -n "$pr_files" ]; then finish true "no merge base with the base branch: every security-relevant file in the PR" "$pr_files"; fi
  finish false "no file listed in .github/security-review/paths.txt changed" ""
fi

changed=$(files_for "$since" "$HEAD_SHA")
base_moved=""
if [ "$fork_then" != "$fork_now" ]; then
  base_moved=$(files_for "$fork_then" "$fork_now")
fi
files=$(comm -12 <(printf '%s\n' "$pr_files" | sed '/^$/d' | sort -u) \
  <(printf '%s\n%s\n' "$changed" "$base_moved" | sed '/^$/d' | sort -u))

if [ -n "$files" ]; then finish true "changes since the last completed review (${since:0:7})" "$files"; fi
finish false "nothing security-relevant changed since the last completed review (${since:0:7})" ""
