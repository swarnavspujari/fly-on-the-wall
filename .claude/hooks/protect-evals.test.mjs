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
const decision = (v) => v?.decision ?? null;
const isAsk = (v, msg) => assert.equal(decision(v), "ask", msg);
const isDeny = (v, msg) => assert.equal(decision(v), "deny", msg);
/** Protected-path changes: the user is asked. */
const asks = (cmds, run = bash) => {
  for (const c of cmds) isAsk(run(c), `should ask: ${c}`);
};
/** Trust roots and hook switches: always denied. */
const denies = (cmds, run = bash) => {
  for (const c of cmds) isDeny(run(c), `should deny: ${c}`);
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

test("file tools: protected paths ask the user", () => {
  isAsk(edit("evals/thresholds.toml", "Write"));
  isAsk(edit(at("evals", "runner", "src", "main.rs")));
  isAsk(edit(at("evals", "fixtures", "m01.json"), "MultiEdit"));
  // An edit whose result can't be worked out might unregister the hook.
  isDeny(edit(at(".claude", "settings.json")), "an unreadable settings.json edit is denied");
  isAsk(edit(at(".claude", "settings.local.json"), "Write"), "settings.local.json can switch hooks off");
  isAsk(edit(at(".claude", "hooks", "protect-evals.mjs"), "Write"));
  isAsk(edit(at(".github", "workflows", "evals.yml")));
  isAsk(edit(at(".github", "workflows", "evals-guard.yml")));
  isAsk(edit(at("crates", "..", "evals", "x.toml")), "traversal");
  isAsk(decide({ tool_name: "NotebookEdit", cwd: root, tool_input: { notebook_path: "evals/x.ipynb" } }, env));
  isAsk(edit(path.join(wt, "evals", "thresholds.toml")), "a worktree of this repo");
  assert.match(edit("evals/thresholds.toml", "Write").reason, /Eval change/);
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
  isAsk(edit(path.join(link, "thresholds.toml")), "write through a link into evals/");
  isAsk(bash("rm frontend/eval-link/thresholds.toml"));
});

test("disableAllHooks is denied anywhere, escaped or not", () => {
  isDeny(decide({ tool_name: "Write", cwd: root, tool_input: { file_path: path.join(tmpdir(), "x.json"), content: '{"disableAllHooks": true}' } }, env));
  isDeny(decide({ tool_name: "Write", cwd: root, tool_input: { file_path: path.join(tmpdir(), "y.json"), content: '{"disable\\u0041llHooks": true}' } }, env));
  // Even in the local settings file, which is otherwise only asked about.
  isDeny(decide({ tool_name: "Write", cwd: root, tool_input: { file_path: at(".claude", "settings.local.json"), content: '{"disableAllHooks": true}' } }, env));
  isAsk(decide({ tool_name: "Write", cwd: root, tool_input: { file_path: at(".claude", "settings.local.json"), content: '{"permissions": {}}' } }, env));
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
  denies([
    "gh api graphql -f query='mutation { updateBranchProtectionRule(input: {}) { clientMutationId } }'",
    "gh api graphql -f query='mutation { deleteRepositoryRuleset(input: {}) { clientMutationId } }'",
    // GraphQL that can't be inspected, or any mutation.
    "gh api graphql -F query=@q.graphql",
    "gh api graphql --input body.json",
    "gh api graphql -f query='mutation { addStar(input: {}) { clientMutationId } }'",
    "curl -X POST https://api.github.com/graphql --data @body.json",
    "curl https://api.github.com/graphql -d @-",
    // Nested in a shell, still denied rather than asked.
    'bash -c "gh variable set FOO --body x"',
    "echo \"gh pr merge 103 --admin\" | sh",
  ]);
  allows(["gh pr merge 103 --squash", "gh variable list", "gh pr view 103", "gh api repos/o/r/pulls/103", "gh api graphql -f query='{ viewer { login } }'"]);
});

test("patches: git apply / git am / patch of protected paths", () => {
  const evil = path.join(tmpdir(), "evil.patch");
  writeFileSync(evil, "diff --git a/evals/thresholds.toml b/evals/thresholds.toml\n--- a/evals/thresholds.toml\n+++ b/evals/thresholds.toml\n@@ -1 +1 @@\n-schema = 1\n+schema = 2\n");
  const fine = path.join(tmpdir(), "fine.patch");
  writeFileSync(fine, "diff --git a/crates/x.rs b/crates/x.rs\n--- a/crates/x.rs\n+++ b/crates/x.rs\n");
  asks([`git apply ${evil}`, `git am ${evil}`, `git apply < ${evil}`, `patch -p1 < ${evil}`, "git apply /tmp/missing-evals.patch", "cat /tmp/p | git apply # evals"]);
  allows([`git apply ${fine}`, `patch -p1 < ${fine}`, `git apply -p1 ${fine}`]);
  // Prefix tricks: a relative patch aimed at evals/ by --directory, a
  // deep patch whose -p2 strip lands in evals/, and applying from inside.
  const rel = path.join(tmpdir(), "rel.patch");
  writeFileSync(rel, "diff --git a/thresholds.toml b/thresholds.toml\n--- a/thresholds.toml\n+++ b/thresholds.toml\n@@ -1 +1 @@\n-schema = 1\n+schema = 2\n");
  const deep = path.join(tmpdir(), "deep.patch");
  writeFileSync(deep, "diff --git a/b/evals/thresholds.toml b/b/evals/thresholds.toml\n--- a/b/evals/thresholds.toml\n+++ b/b/evals/thresholds.toml\n@@ -1 +1 @@\n-schema = 1\n+schema = 2\n");
  asks([
    `git apply --directory=evals ${rel}`,
    `git apply --directory evals ${rel}`,
    `git apply -p2 ${deep}`,
    `patch -p2 < ${deep}`,
    `patch -d evals -p1 < ${rel}`,
    `cd evals && git apply ${rel}`,
    `cd evals && patch -p1 < ${rel}`,
  ]);
  allows([`git apply ${rel}`]);
});

if (process.platform === "win32" || process.platform === "darwin") {
  test("paths are case-insensitive on Windows and macOS", () => {
    isAsk(edit(at("EVALS", "Thresholds.TOML")));
    isAsk(bash("rm -f Evals/thresholds.toml"));
  });
}

if (process.platform === "win32") {
  test("Windows path forms", () => {
    isAsk(edit(at("evals", "x").replace(/\//g, "\\")));
    const msys = "/" + root[0].toLowerCase() + root.slice(2).replace(/\\/g, "/") + "/evals/x";
    isAsk(edit(msys), msys);
    isAsk(bash(`rm "${at("evals", "thresholds.toml")}"`));
    isAsk(ps(`Remove-Item '${at("evals", "thresholds.toml")}'`));
  });
}

test("Bash: writes, moves, deletes and redirects ask the user", () => {
  asks([
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
    // Fed to a shell or interpreter through a pipe.
    "cat <<'EOF' | bash\nrm evals/thresholds.toml\nEOF",
    "echo \"rm evals/thresholds.toml\" | bash",
    "printf 'rm -rf evals\\n' | sh -s",
    "echo \"open('evals/x', 'w').write('x')\" | python -",
    "cat <<'EOF' | python3\nopen('evals/thresholds.toml', 'w').write('x')\nEOF",
    // Launchers before the reader, shell options with values, unreadable feeds.
    "echo \"rm evals/thresholds.toml\" | timeout 5 bash",
    "echo \"rm evals/thresholds.toml\" | nice -n 5 sh",
    "echo \"rm evals/thresholds.toml\" | bash -o pipefail",
    "cat /dev/zero | bash",
    "cat /no/such/file | sh",
    // A heredoc whose own reader is a shell, piped onward to something else.
    "bash <<'EOF' | cat\nrm evals/thresholds.toml\nEOF",
    // Interpreters whose flags aren't inline code still read stdin.
    "echo \"open('evals/x', 'w').write('x')\" | python -E",
    "echo \"open('evals/x', 'w')\" | perl -p",
    "echo \"File.write('evals/x', 'x')\" | ruby -r json",
    // A value-taking flag clustered with the inline letter is not inline code.
    "echo \"File.write('evals/x', 'x')\" | ruby -re",
    "echo \"open('evals/x', 'w').write('x')\" | python -Wc",
    "echo \"open('evals/x', 'w').write('x')\" | python -c",
    // Inline flags count only before the script / `-` (in argument order).
    "echo \"open('evals/x', 'w').write('x')\" | python -W -c -",
    "echo \"open('evals/x', 'w').write('x')\" | python - -c x",
    "echo \"require('fs').writeFileSync('evals/x', 'x')\" | node - -e x",
    "echo \"rm evals/thresholds.toml\" | bash -s -c x",
    // Options that take no value, shell long options that do, deno/bun subcommands.
    "echo \"open('evals/x', 'w').write('x')\" | python -E - x",
    "echo \"open('evals/x', 'w').write('x')\" | python -I - x",
    "echo \"rm evals/thresholds.toml\" | bash --rcfile /dev/null -s",
    "echo \"rm evals/thresholds.toml\" | bash --init-file /dev/null",
    "echo \"Deno.writeTextFileSync('evals/x', 'x')\" | deno run -",
    "echo \"require('fs').writeFileSync('evals/x', 'x')\" | bun run -",
    "echo \"open('evals/x', 'w').write('x')\" | python script.py -",
    // Launchers whose options take values.
    "echo \"rm evals/thresholds.toml\" | sudo -u nobody bash",
    "echo \"rm evals/thresholds.toml\" | timeout -s KILL 5 bash",
    "echo \"rm evals/thresholds.toml\" | env -u FOO sh",
    // Stdin from a file redirect.
    "bash < /no/such/script.sh",
    "cat < /no/such/script.sh | sh",
  ]);
  // A redirected script the hook can read and that writes a protected path.
  const bad = path.join(tmpdir(), "bad-script.sh");
  writeFileSync(bad, "rm -rf evals\n");
  const good = path.join(tmpdir(), "good-script.sh");
  writeFileSync(good, "echo hello\n");
  asks([`bash < ${bad}`, `cat < ${bad} | bash`, `sh -s < ${bad}`]);
  allows([`bash < ${good}`, `cat < ${good} | bash`]);
  // Already inside a protected folder: relative writes are denied.
  isAsk(bash("echo x > a.txt", at("evals")));
  isAsk(bash("rm thresholds.toml", at("evals")));
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
    "echo hello | bash",
    "cat README.md | python -c 'import sys; print(len(sys.stdin.read()))'",
    "cat <<'EOF' | bash\necho hi\nEOF",
  ]);
});

test("PowerShell: writes ask the user, reads allowed", () => {
  asks(
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

test("the guard itself: its own prompt, and unregistering it is denied", () => {
  const settings = at(".claude", "settings.json");
  const real = JSON.stringify(
    {
      disableAllHooks: false,
      hooks: {
        PreToolUse: [
          {
            matcher: "Edit|Write|MultiEdit|NotebookEdit|Bash|PowerShell",
            hooks: [{ type: "command", command: 'node "${CLAUDE_PROJECT_DIR}/.claude/hooks/protect-evals.mjs"', timeout: 10 }],
          },
        ],
      },
    },
    null,
    2,
  );
  writeFileSync(settings, real);
  const tool = (tool_name, tool_input) => decide({ tool_name, cwd: root, tool_input }, env);
  // The result is what counts, worked out and parsed.
  isDeny(tool("Write", { file_path: settings, content: "{}" }), "no registration");
  isDeny(tool("Write", { file_path: settings, content: real.replace("protect-evals.mjs", "noop.mjs") }), "another command");
  isDeny(tool("Write", { file_path: settings, content: real.replace("|Bash", "") }), "a tool left out of the matcher");
  isDeny(tool("Write", { file_path: settings, content: real + "," }), "unparseable");
  isDeny(tool("Edit", { file_path: settings, old_string: "PowerShell", new_string: "Grep" }), "edited out of the matcher");
  isDeny(tool("Edit", { file_path: settings, old_string: '"PreToolUse"', new_string: '"PostToolUse"' }), "moved to another event");
  isDeny(tool("Edit", { file_path: settings, old_string: "not in the file", new_string: "x" }), "an edit that can't apply");
  isDeny(
    tool("MultiEdit", { file_path: settings, edits: [{ old_string: '"timeout": 10', new_string: '"timeout": 15' }, { old_string: '"hooks": {', new_string: '"x": {' }] }),
    "a later edit drops it",
  );
  // A change that keeps the registration is asked, with its own reason.
  const keep = tool("Edit", { file_path: settings, old_string: '"timeout": 10', new_string: '"timeout": 15' });
  isAsk(keep);
  assert.match(keep.reason, /Eval guard change/);
  // (A rewrite that mentions disableAllHooks at all is denied by that rule,
  // even with false; this one leaves the key out.)
  const withoutSwitch = JSON.stringify({ hooks: JSON.parse(real).hooks });
  isAsk(tool("Write", { file_path: settings, content: withoutSwitch.replace('"timeout":10', '"timeout":20') }));
  isDeny(tool("Write", { file_path: settings, content: real }), "mentions disableAllHooks");
  assert.match(tool("Edit", { file_path: at(".claude", "hooks", "protect-evals.mjs"), old_string: "a", new_string: "b" }).reason, /Eval guard change/);
  assert.match(bash("rm .claude/hooks/protect-evals.mjs").reason, /Eval guard change/);
  // An ordinary eval edit isn't labeled a guard change.
  const evalEdit = tool("Edit", { file_path: at("evals", "thresholds.toml"), old_string: "a", new_string: "b" });
  isAsk(evalEdit);
  assert.match(evalEdit.reason, /^Eval change/);
});

test("protectedTarget resolves relative to cwd", () => {
  assert.equal(protectedTarget("thresholds.toml", { cwd: at("evals") }), "evals/thresholds.toml");
  assert.equal(protectedTarget("evals", { cwd: at("crates") }), null);
  assert.equal(commandViolation("rm -rf evals", { cwd: at("crates") }), null, "crates/evals is not protected");
});

test("end to end: the hook process speaks the PreToolUse protocol", () => {
  const run = (input) => spawnSync(process.execPath, [HOOK], { input, env: { ...process.env, ...env }, encoding: "utf8" });
  const asked = run(JSON.stringify({ hook_event_name: "PreToolUse", tool_name: "Write", cwd: root, tool_input: { file_path: "evals/thresholds.toml", content: "x" } }));
  assert.equal(asked.status, 0, asked.stderr);
  const out = JSON.parse(asked.stdout);
  assert.equal(out.hookSpecificOutput.hookEventName, "PreToolUse");
  assert.equal(out.hookSpecificOutput.permissionDecision, "ask");
  assert.match(out.hookSpecificOutput.permissionDecisionReason, /Eval change: evals\/thresholds\.toml/);

  const denied = run(JSON.stringify({ hook_event_name: "PreToolUse", tool_name: "Bash", cwd: root, tool_input: { command: "gh variable set EVALS_LOCK_SHA256 --body 00" } }));
  assert.equal(denied.status, 0, denied.stderr);
  assert.equal(JSON.parse(denied.stdout).hookSpecificOutput.permissionDecision, "deny");

  const allow = run(JSON.stringify({ hook_event_name: "PreToolUse", tool_name: "Bash", cwd: root, tool_input: { command: "cat evals/thresholds.toml" } }));
  assert.equal(allow.status, 0);
  assert.equal(allow.stdout, "");

  // Unparseable input asks the user only when it could touch the evals.
  const unparsed = run("{not json evals");
  assert.equal(unparsed.status, 0);
  assert.equal(JSON.parse(unparsed.stdout).hookSpecificOutput.permissionDecision, "ask");
  const unrelated = run("{not json");
  assert.equal(unrelated.status, 0);
  assert.equal(unrelated.stdout, "");
});
