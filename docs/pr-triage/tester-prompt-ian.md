# Prompt for Ian's Claude session (Intel Mac smoke test)

Copy-paste everything below the line into Claude (Claude Code or claude.ai)
on the Intel MacBook, with the repo checked out at `integration/ian-prs` (or
the app installed from a build of it). Send the checklist file along with it
or let Claude read it from the repo.

---

You are running a **test-only** smoke pass of the Fly on the Wall app on
this Mac (2019 MBP 16", AMD Radeon Pro 5500M). Ground rules, non-negotiable:

- **No code changes of any kind.** Do not edit, patch, build-fix, commit,
  push, or open pull requests — even if you find a bug and the fix looks
  obvious. Your entire job is to observe and report.
- Do not update dependencies, change settings files in the repo, or touch
  anything on GitHub.
- You may: run the app, record short test meetings, toggle Settings in the
  app UI, run read-only shell commands (`file`, `otool`, `grep`, reading
  log files), and install/uninstall `whisper-cpp` via brew where a checklist
  item says so.

Your task: work through `docs/pr-triage/mac-smoke-checklist.md`, executing
every item marked [Intel] or "both machines" (skip [AS]-only items). Take
the items in order — the checklist's ordering avoids invalidating later
steps.

Report format — **lead with this summary line**:

    OVERALL: PASS (n/n) — or — OVERALL: FAIL (k of n items failed)

Then one entry per checklist item:

- **Item** (checklist section + bullet)
- **Result:** pass / fail / skipped (with reason)
- **Evidence:** the exact notice or banner text you saw, a short excerpt of
  the produced transcript where relevant, and the matching log lines
  (Settings → Technical → **Open logs folder**, newest
  `flyonthewall.*.log`).

For every **failure**, additionally include:

- macOS version (`sw_vers`)
- `file <path-to>/whisper-cli` output for whichever whisper-cli binary the
  app resolved (the log's engine path tells you which one ran)
- `otool -l <path-to>/whisper-cli | grep -A2 LC_BUILD_VERSION` (or
  `grep minos`) output
- the last 50 lines of the newest log file
- the exact error text, verbatim, not paraphrased

If an item cannot be executed at all (e.g. it needs the rehost artifact and
the release doesn't exist yet), mark it **skipped** with the reason — do not
improvise a substitute test. When the checklist is done, stop; do not
continue into fixes, suggestions are welcome only as a short "notes"
section at the end.
