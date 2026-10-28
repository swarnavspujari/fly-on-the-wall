// Tests for protect-evals.mjs: `node --test .claude/hooks/protect-evals.test.mjs`
// (also run by the evals-security CI job). Node built-ins only; runs on
// Windows (Git Bash or PowerShell), macOS and Linux.

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync, realpathSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { decide, commandViolation, protectedTarget, isProtectedRel } from "./protect-evals.mjs";

const HOOK = path.join(path.dirname(fileURLToPath(import.meta.url)), "protect-evals.mjs");
const SIGNATURE = '[package]\nname = "fly-evals"\n';

function repo(dir, { signature = true, gitFile = false } = {}) {
  if (gitFile) {
    mkdirSync(dir, { recursive: true });
    writeFileSync(path.join(dir, ".git"), "gitdir: elsewhere\n");
  } else {
    mkdirSync(path.join(dir, ".git"), { recursive: true });
  }
  for (const d of ["evals/runner/src", "evals/fixtures", ".claude/hooks", ".github/workflows", "crates/fly-context/src", "frontend"]) {
    mkdirSync(path.join(dir, d), { recursive: true });
  }
  if (signature) writeFileSync(path.join(dir, "evals/runner/Cargo.toml"), SIGNATURE);
  writeFileSync(path.join(dir, "evals/thresholds.toml"), "schema = 1\n");
}

// This repo at <root>; a worktree of it at .claude/worktrees/wt; and an
// unrelated repo with its own evals/ folder (not protected).
const root = realpathSync(mkdtempSync(path.join(tmpdir(), "fotw-hook-")));
repo(root);
const wt = path.join(root, ".claude/worktrees/wt");
repo(wt, { gitFile: true });
const other = realpathSync(mkdtempSync(path.join(tmpdir(), "other-repo-")));
repo(other, { signature: false });
const env = { CLAUDE_PROJECT_DIR: root };

const at = (...p) => path.join(root, ...p);
const edit = (file_path, tool_name = "Edit") => decide({ tool_name, cwd: root, tool_input: { file_path } }, env);
const bash = (command, cwd = root, tool_name = "Bash") => decide({ tool_name, cwd, tool_input: { command } }, env);
const ps = (command, cwd = root) => bash(command, cwd, "PowerShell");
const denies = (cmds, run = bash) => {
  for (const c of cmds) assert.ok(run(c), `should deny: ${c}`);
};
const allows = (cmds, run = bash) => {
  for (const c of cmds) assert.equal(run(c), null, `should allow: ${c}`);
};

test("protected repo-relative paths", () => {
  for (const p of ["evals", "evals/thresholds.toml", "evals/runner/src/main.rs", ".claude/settings.json", ".claude/settings.local.json", ".claude/hooks", ".claude/hooks/protect-evals.mjs", ".github/workflows/evals.yml", ".github/workflows/evals-guard.yml", "./evals/x"]) {
    assert.ok(isProtectedRel(p), p);
  }
  for (const p of [".claude/launch.json", ".github/workflows/ci.yml", "evalsx/y", "target/evals/retrieval.json", "docs/evals.md"]) {
    assert.ok(!isProtectedRel(p), p);
  }
});

test("file tools: protected paths are denied", () => {
  assert.ok(edit("evals/thresholds.toml", "Write"));
  assert.ok(edit(at("evals", "runner", "src", "main.rs")));
  assert.ok(edit(at("evals", "fixtures", "m01.json"), "MultiEdit"));
  assert.ok(edit(at(".claude", "settings.json")));
  assert.ok(edit(at(".claude", "settings.local.json"), "Write"), "settings.local.json can switch hooks off");
  assert.ok(edit(at(".claude", "hooks", "protect-evals.mjs"), "Write"));
  assert.ok(edit(at(".github", "workflows", "evals.yml")));
  assert.ok(edit(at(".github", "workflows", "evals-guard.yml")));
  assert.ok(edit(at("crates", "..", "evals", "x.toml")), "traversal");
  assert.ok(decide({ tool_name: "NotebookEdit", cwd: root, tool_input: { notebook_path: "evals/x.ipynb" } }, env));
  assert.ok(edit(path.join(wt, "evals", "thresholds.toml")), "a worktree of this repo");
  assert.match(edit("evals/thresholds.toml", "Write"), /only the user/);
});

test("file tools: everything else is allowed", () => {
  assert.equal(edit(at("crates", "fly-context", "src", "lib.rs")), null);
  assert.equal(edit(at(".claude", "launch.json")), null);
  assert.equal(edit(at(".github", "workflows", "ci.yml")), null);
  assert.equal(edit(at("target", "evals", "retrieval.json"), "Write"), null);
  assert.equal(edit(at("docs", "evals-notes.md"), "Write"), null);
  assert.equal(decide({ tool_name: "Read", cwd: root, tool_input: { file_path: "evals/thresholds.toml" } }, env), null);
});

test("only this repository is protected, not evals/ in any repo", () => {
  assert.equal(edit(path.join(other, "evals", "thresholds.toml")), null);
  assert.equal(bash("rm -rf evals", other), null);
});

test("symlinks resolve to their target", () => {
  const link = at("frontend", "eval-link");
  try {
    symlinkSync(at("evals"), link, "junction");
  } catch {
    return; // no symlink rights here
  }
  assert.ok(edit(path.join(link, "thresholds.toml")), "write through a link into evals/");
  assert.ok(bash("rm frontend/eval-link/thresholds.toml"));
});

test("disableAllHooks is denied anywhere, escaped or not", () => {
  assert.ok(decide({ tool_name: "Write", cwd: root, tool_input: { file_path: path.join(tmpdir(), "x.json"), content: '{"disableAllHooks": true}' } }, env));
  assert.ok(decide({ tool_name: "Write", cwd: root, tool_input: { file_path: path.join(tmpdir(), "y.json"), content: '{"disable\\u0041llHooks": true}' } }, env));
  denies([`echo '{"disableAllHooks": true}' > ~/.claude/settings.json`, `python -c "k='disable'+'AllHooks'"`]);
  denies(['Set-Content ~/.claude/settings.json "{`"disableAllHooks`": true}"'], ps);
});

test("trust roots: the lock variable, rulesets, branch protection, admin merges", () => {
  denies([
    "gh variable set EVALS_LOCK_SHA256 --body 0000",
    "gh variable set OTHER --body x",
    "gh -R swarnavspujari/fly-on-the-wall variable delete FOO",
    "gh api -X PATCH repos/o/r/actions/variables/EVALS_LOCK_SHA256 -f value=00",
    "gh api -X PUT repos/o/r/actions/variables/FOO -f value=00",
    "gh api repos/o/r/rulesets/42 -X DELETE",
    "gh api -X PUT repos/o/r/branches/main/protection --input p.json",
    "curl -X PATCH https://api.github.com/repos/o/r/actions/variables/FOO",
    "gh pr merge 103 --admin --squash",
    "echo $EVALS_LOCK_SHA256",
  ]);
  allows(["gh pr merge 103 --squash", "gh variable list", "gh pr view 103", "gh api repos/o/r/pulls/103"]);
});

test("patches: git apply / git am / patch of protected paths", () => {
  const evil = path.join(tmpdir(), "evil.patch");
  writeFileSync(evil, "diff --git a/evals/thresholds.toml b/evals/thresholds.toml\n--- a/evals/thresholds.toml\n+++ b/evals/thresholds.toml\n@@ -1 +1 @@\n-schema = 1\n+schema = 2\n");
  const fine = path.join(tmpdir(), "fine.patch");
  writeFileSync(fine, "diff --git a/crates/x.rs b/crates/x.rs\n--- a/crates/x.rs\n+++ b/crates/x.rs\n");
  denies([`git apply ${evil}`, `git am ${evil}`, `git apply < ${evil}`, `patch -p1 < ${evil}`, "git apply /tmp/missing-evals.patch", "cat /tmp/p | git apply # evals"]);
  allows([`git apply ${fine}`, `patch -p1 < ${fine}`]);
});

if (process.platform === "win32" || process.platform === "darwin") {
  test("paths are case-insensitive on Windows and macOS", () => {
    assert.ok(edit(at("EVALS", "Thresholds.TOML")));
    assert.ok(bash("rm -f Evals/thresholds.toml"));
  });
}

if (process.platform === "win32") {
  test("Windows path forms", () => {
    assert.ok(edit(at("evals", "x").replace(/\//g, "\\")));
    const msys = "/" + root[0].toLowerCase() + root.slice(2).replace(/\\/g, "/") + "/evals/x";
    assert.ok(edit(msys), msys);
    assert.ok(bash(`rm "${at("evals", "thresholds.toml")}"`));
    assert.ok(ps(`Remove-Item '${at("evals", "thresholds.toml")}'`));
  });
}

test("Bash: writes, moves, deletes and redirects are denied", () => {
  denies([
    "echo x > evals/thresholds.toml",
    "echo x >evals/x",
    "echo x>>evals/x",
    "echo x >> ./evals/README.md",
    "printf x 1> evals/a",
    "rm -rf evals",
    "rm -rf ev*",
    "rm -rf .c*",
    "rm -rf .claude",
    "rm -rf .",
    "rm -f evals/thresholds.toml && echo done",
    "mv evals/fixtures /tmp/x",
    "cp /tmp/x evals/thresholds.toml",
    "cp -t evals /tmp/x",
    "cp -r /tmp/x .claude",
    "rsync -a /tmp/x/ evals/",
    "ln -sf /tmp/x evals/thresholds.toml",
    "touch .claude/hooks/x.mjs",
    "mkdir -p evals/new",
    "sed -i 's/0.85/0.5/' evals/thresholds.toml",
    "sed -i.bak 's/a/b/' .github/workflows/evals.yml",
    "perl -pi -e 's/a/b/' evals/x",
    "git checkout main -- evals/thresholds.toml",
    "git restore evals",
    "git restore --source main :/evals",
    "git checkout HEAD -- ':(top)evals/thresholds.toml'",
    "git -C evals checkout -- .",
    "git -C .claude/hooks restore .",
    "git rm -r .claude/hooks",
    "git stash push -- evals",
    "git mv evals/a evals/b",
    "cd evals && rm thresholds.toml",
    "cd evals; echo x > thresholds.toml",
    "pushd evals && rm thresholds.toml",
    "cd .claude && echo '{}' > settings.json",
    'bash -c "rm evals/x"',
    "sh -c 'cd evals && truncate -s0 thresholds.toml'",
    'python -c "open(\'evals/thresholds.toml\',\'w\').write(\'x\')"',
    "node -e \"require('fs').writeFileSync('.claude/settings.json','{}')\"",
    "find evals -name '*.json' -delete",
    "find evals -type f -exec sed -i s/a/b/ {} +",
    "echo $(rm evals/x)",
    "echo `rm evals/x`",
    "tee .github/workflows/evals-guard.yml < /tmp/x",
    "cat x | tee -a evals/a",
    "cargo fmt -p fly-evals",
    "cargo clippy --fix -p fly-evals --allow-dirty",
    "rustfmt evals/runner/src/main.rs",
    "npx prettier --write evals/README.md",
    "npx --yes prettier -w .claude/hooks/protect-evals.mjs",
    "npm exec -- eslint --fix .claude/hooks/x.mjs",
    "curl -o evals/x https://example.invalid/x",
    "tar -xf /tmp/a.tar -C evals",
    "sudo rm evals/x",
    "FOO=1 rm evals/x",
    "xargs rm < list.txt evals/x",
    "p=evals/thresholds.toml; rm $p",
    'rm "$CLAUDE_PROJECT_DIR/evals/thresholds.toml"',
    "rm ${CLAUDE_PROJECT_DIR}/evals/x",
    "rm $PWD/evals/x",
    "bash <<'EOF'\nrm evals/thresholds.toml\nEOF",
    "python3 - <<'EOF'\nopen('evals/thresholds.toml', 'w').write('x')\nEOF",
    "cat > evals/thresholds.toml <<'EOF'\nschema = 2\nEOF",
  ]);
  // Already inside a protected folder: relative writes are denied.
  assert.ok(bash("echo x > a.txt", at("evals")));
  assert.ok(bash("rm thresholds.toml", at("evals")));
});

test("Bash: reads, quoted text, heredoc bodies and unrelated work are allowed", () => {
  allows([
    "cat evals/thresholds.toml",
    "ls evals",
    "ls -la evals/",
    "grep -rn recall evals/",
    "git diff -- evals",
    "git log --oneline -- evals/",
    "git status --short",
    "git add evals && git commit -m 'add evals'",
    'git commit -m "note -> evals/README.md"',
    "git commit -m \"use node to write to evals\"",
    "git checkout -b feature",
    "git checkout .",
    "cargo run -p fly-evals -- --suite ci",
    "cargo run -p fly-evals -- --suite ci --gate security",
    "cargo test -p fly-evals",
    "cargo fmt --all --check",
    "cargo fmt -p fly-evals -- --check",
    "cargo build -p fly-evals --locked",
    "rustfmt --check evals/runner/src/main.rs",
    "npx prettier --check evals/README.md",
    "bash evals/lock-hash.sh HEAD",
    "cp evals/thresholds.toml /tmp/thresholds.toml",
    "rm -rf target/evals",
    "rm -rf .claude/worktrees/old-one",
    "cat evals/README.md > /tmp/readme.md",
    "cat evals/README.md 2>/dev/null",
    "cat evals/README.md 2>&1 | head",
    "node --test .claude/hooks/protect-evals.test.mjs",
    "echo evals > notes.txt",
    "cd crates/fly-context && cargo test",
    "cd evals && ls && cat thresholds.toml",
    "cd frontend && npm test",
    "rm -rf node_modules",
    "echo hi > /tmp/x",
    "cat > /tmp/notes.md <<'EOF'\nrm -rf evals\necho x > evals/thresholds.toml\nEOF",
    "rm $UNSET_VAR/x",
  ]);
});

test("PowerShell: writes are denied, reads allowed", () => {
  denies(
    [
      "Set-Content -Path evals/thresholds.toml -Value x",
      "Add-Content evals\\README.md 'x'",
      "Remove-Item -Recurse -Force .claude\\hooks",
      '"x" | Out-File evals\\a.txt',
      "New-Item -ItemType File -Path .github/workflows/evals-guard.yml -Force",
      "Copy-Item C:\\tmp\\x evals\\thresholds.toml",
      "Move-Item evals\\fixtures C:\\tmp\\x",
      'pwsh -Command "Remove-Item evals -Recurse"',
      "powershell -NoProfile -Command Remove-Item evals\\x",
      "Set-Location evals; Remove-Item thresholds.toml",
      "Push-Location evals; Remove-Item thresholds.toml",
      "Clear-Content evals\\thresholds.toml",
      "ni evals\\x.txt",
      "del evals\\thresholds.toml",
      "powershell -EncodedCommand ZQB2AGEAbABzAA== # evals",
      "echo x > evals\\x.txt",
      "echo x >evals\\x.txt",
      "[IO.File]::WriteAllText('evals\\thresholds.toml', 'x')",
      "[System.IO.File]::Delete(\"$PWD\\evals\\thresholds.toml\")",
      "$p='evals\\thresholds.toml'; Set-Content $p 'x'",
      "$p = 'evals\\thresholds.toml'; Remove-Item $p",
      "iex 'Remove-Item evals\\thresholds.toml'",
      "Invoke-Expression \"Remove-Item evals -Recurse\"",
      "Start-Process cmd -ArgumentList '/c del evals\\thresholds.toml'",
      "Start-Process -FilePath cmd -ArgumentList '/c rd /s /q evals'",
      "Invoke-Command -ScriptBlock { Remove-Item evals -Recurse }",
      "cmd /c del evals\\thresholds.toml",
      "Remove-Item \"$env:CLAUDE_PROJECT_DIR\\evals\\thresholds.toml\"",
    ],
    ps,
  );
  allows(
    [
      "Get-Content evals/thresholds.toml",
      "Get-ChildItem evals",
      "Select-String -Path evals\\*.toml -Pattern recall",
      "Copy-Item evals\\thresholds.toml C:\\tmp\\t.toml",
      "cargo run -p fly-evals -- --suite ci",
      "git log -- evals",
      "Get-Content evals\\README.md > $null",
      "[IO.File]::ReadAllText('evals\\thresholds.toml')",
    ],
    ps,
  );
});

test("protectedTarget resolves relative to cwd", () => {
  assert.equal(protectedTarget("thresholds.toml", { cwd: at("evals") }), "evals/thresholds.toml");
  assert.equal(protectedTarget("evals", { cwd: at("crates") }), null);
  assert.equal(commandViolation("rm -rf evals", { cwd: at("crates") }), null, "crates/evals is not protected");
});

test("end to end: the hook process speaks the PreToolUse protocol", () => {
  const run = (input) => spawnSync(process.execPath, [HOOK], { input, env: { ...process.env, ...env }, encoding: "utf8" });
  const deny = run(JSON.stringify({ hook_event_name: "PreToolUse", tool_name: "Write", cwd: root, tool_input: { file_path: "evals/thresholds.toml", content: "x" } }));
  assert.equal(deny.status, 0, deny.stderr);
  const out = JSON.parse(deny.stdout);
  assert.equal(out.hookSpecificOutput.hookEventName, "PreToolUse");
  assert.equal(out.hookSpecificOutput.permissionDecision, "deny");
  assert.match(out.hookSpecificOutput.permissionDecisionReason, /evals\/thresholds\.toml/);

  const allow = run(JSON.stringify({ hook_event_name: "PreToolUse", tool_name: "Bash", cwd: root, tool_input: { command: "cat evals/thresholds.toml" } }));
  assert.equal(allow.status, 0);
  assert.equal(allow.stdout, "");

  // Unparseable input fails closed only when it could touch the evals.
  assert.equal(run("{not json evals").status, 2);
  assert.equal(run("{not json").status, 0);
});
