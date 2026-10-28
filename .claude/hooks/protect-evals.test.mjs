// Tests for protect-evals.mjs: `node --test .claude/hooks/protect-evals.test.mjs`
// (also run by the evals-run CI job).
// Node built-ins only. Runs on Windows (Git Bash or PowerShell) and macOS.

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { decide, commandViolation, protectedTarget, isProtectedRel } from "./protect-evals.mjs";

const HOOK = path.join(path.dirname(fileURLToPath(import.meta.url)), "protect-evals.mjs");

// A throwaway repo: <root>/.git, evals/, .claude/hooks, and a nested
// worktree at .claude/worktrees/wt with its own .git file.
const root = realpathSync(mkdtempSync(path.join(tmpdir(), "fotw-hook-")));
for (const d of [".git", "evals/runner/src", ".claude/hooks", ".github/workflows", "crates/fly-context/src", ".claude/worktrees/wt/evals"]) {
  mkdirSync(path.join(root, d), { recursive: true });
}
writeFileSync(path.join(root, ".claude/worktrees/wt/.git"), "gitdir: elsewhere\n");
const env = { CLAUDE_PROJECT_DIR: root };

const edit = (file_path, tool_name = "Edit") => decide({ tool_name, cwd: root, tool_input: { file_path } }, env);
const bash = (command, cwd = root, tool_name = "Bash") => decide({ tool_name, cwd, tool_input: { command } }, env);
const ps = (command, cwd = root) => bash(command, cwd, "PowerShell");
const at = (...p) => path.join(root, ...p);

test("protected repo-relative paths", () => {
  for (const p of ["evals", "evals/thresholds.toml", "evals/runner/src/main.rs", ".claude/settings.json", ".claude/hooks", ".claude/hooks/protect-evals.mjs", ".github/workflows/evals.yml", ".github/workflows/evals-guard.yml", "./evals/x"]) {
    assert.ok(isProtectedRel(p), p);
  }
  for (const p of [".claude/settings.local.json", ".claude/launch.json", ".github/workflows/ci.yml", "crates/fly-evals-like/x", "evalsx/y", "target/evals/retrieval.json", "docs/evals.md"]) {
    assert.ok(!isProtectedRel(p), p);
  }
});

test("file tools: protected paths are denied", () => {
  assert.ok(edit("evals/thresholds.toml", "Write"));
  assert.ok(edit(at("evals", "runner", "src", "main.rs")));
  assert.ok(edit(at("evals", "fixtures", "m01.json"), "MultiEdit"));
  assert.ok(edit(at(".claude", "settings.json")));
  assert.ok(edit(at(".claude", "hooks", "protect-evals.mjs"), "Write"));
  assert.ok(edit(at(".github", "workflows", "evals.yml")));
  assert.ok(edit(at(".github", "workflows", "evals-guard.yml")));
  assert.ok(edit(at("crates", "..", "evals", "x.toml")), "traversal");
  assert.ok(decide({ tool_name: "NotebookEdit", cwd: root, tool_input: { notebook_path: "evals/x.ipynb" } }, env));
  // Another checkout under the project (a worktree) is protected too.
  assert.ok(edit(at(".claude", "worktrees", "wt", "evals", "thresholds.toml")));
  const reason = edit("evals/thresholds.toml", "Write");
  assert.match(reason, /eval lock/);
  assert.match(reason, /only the user/);
});

test("file tools: everything else is allowed", () => {
  assert.equal(edit(at("crates", "fly-context", "src", "lib.rs")), null);
  assert.equal(edit(at(".claude", "settings.local.json")), null);
  assert.equal(edit(at(".github", "workflows", "ci.yml")), null);
  assert.equal(edit(at("target", "evals", "retrieval.json"), "Write"), null);
  assert.equal(edit(at("docs", "evals-notes.md"), "Write"), null);
  assert.equal(decide({ tool_name: "Read", cwd: root, tool_input: { file_path: "evals/thresholds.toml" } }, env), null);
});

test("disableAllHooks is denied anywhere", () => {
  assert.ok(decide({ tool_name: "Write", cwd: root, tool_input: { file_path: at(".claude", "settings.local.json"), content: '{"disableAllHooks": true}' } }, env));
  assert.ok(bash(`echo '{"disableAllHooks": true}' > ~/.claude/settings.json`));
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
  const denied = [
    "echo x > evals/thresholds.toml",
    "echo x >> ./evals/README.md",
    "printf x 1> evals/a",
    "rm -rf evals",
    "rm -f evals/thresholds.toml && echo done",
    "mv evals/fixtures /tmp/x",
    "cp /tmp/x evals/thresholds.toml",
    "cp -t evals /tmp/x",
    "rsync -a /tmp/x/ evals/",
    "ln -sf /tmp/x evals/thresholds.toml",
    "touch .claude/hooks/x.mjs",
    "mkdir -p evals/new",
    "sed -i 's/0.85/0.5/' evals/thresholds.toml",
    "sed -i.bak 's/a/b/' .github/workflows/evals.yml",
    "perl -pi -e 's/a/b/' evals/x",
    "git checkout main -- evals/thresholds.toml",
    "git restore evals",
    "git rm -r .claude/hooks",
    "git stash push -- evals",
    "git mv evals/a evals/b",
    "cd evals && rm thresholds.toml",
    "cd evals; echo x > thresholds.toml",
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
    "curl -o evals/x https://example.invalid/x",
    "tar -xf /tmp/a.tar -C evals",
    "sudo rm evals/x",
    "FOO=1 rm evals/x",
    "xargs rm < list.txt evals/x",
  ];
  for (const c of denied) assert.ok(bash(c), `should deny: ${c}`);
  // Already inside a protected folder: relative writes are denied.
  assert.ok(bash("echo x > a.txt", at("evals")));
  assert.ok(bash("rm thresholds.toml", at("evals")));
});

test("Bash: reads and unrelated work are allowed", () => {
  const allowed = [
    "cat evals/thresholds.toml",
    "ls evals",
    "ls -la evals/",
    "grep -rn recall evals/",
    "git diff -- evals",
    "git log --oneline -- evals/",
    "git status --short",
    "git add evals && git commit -m 'add evals'",
    "git checkout -b feature",
    "cargo run -p fly-evals -- --suite ci",
    "cargo test -p fly-evals",
    "cargo fmt --all --check",
    "cargo fmt -p fly-evals -- --check",
    "cargo build -p fly-evals --locked",
    "bash evals/lock-hash.sh HEAD",
    "cp evals/thresholds.toml /tmp/thresholds.toml",
    "rm -rf target/evals",
    "cat evals/README.md > /tmp/readme.md",
    "cat evals/README.md 2>/dev/null",
    "node --test .claude/hooks/",
    "echo evals > notes.txt",
    "cd crates/fly-context && cargo test",
    "cd evals && ls && cat thresholds.toml",
    "cd frontend && npm test",
    "rm -rf node_modules",
    "echo hi > /tmp/x",
  ];
  for (const c of allowed) assert.equal(bash(c), null, `should allow: ${c}`);
});

test("PowerShell: writes are denied, reads allowed", () => {
  const denied = [
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
    "Clear-Content evals\\thresholds.toml",
    "ni evals\\x.txt",
    "del evals\\thresholds.toml",
    "powershell -EncodedCommand ZQB2AGEAbABzAA== # evals",
    "echo x > evals\\x.txt",
  ];
  for (const c of denied) assert.ok(ps(c), `should deny: ${c}`);
  const allowed = [
    "Get-Content evals/thresholds.toml",
    "Get-ChildItem evals",
    "Select-String -Path evals\\*.toml -Pattern recall",
    "Copy-Item evals\\thresholds.toml C:\\tmp\\t.toml",
    "cargo run -p fly-evals -- --suite ci",
    "git log -- evals",
    "Get-Content evals\\README.md > $null",
  ];
  for (const c of allowed) assert.equal(ps(c), null, `should allow: ${c}`);
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
