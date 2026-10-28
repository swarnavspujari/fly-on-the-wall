#!/usr/bin/env node
// Claude Code PreToolUse hook: AI agents may read and run the system evals,
// never change them (evals/README.md, "The charter").
//
// Denies, with a reason Claude sees:
//   * Edit / Write / MultiEdit / NotebookEdit on a protected path;
//   * Bash / PowerShell commands that write, move, delete or redirect into a
//     protected path, or check one out / restore / remove it with git;
//   * anything mentioning `disableAllHooks` (which would switch this off).
// Reads (cat, ls, grep, git diff/log, cargo run -p fly-evals, ...) pass.
//
// Protected: evals/**, .github/workflows/evals.yml,
// .github/workflows/evals-guard.yml, .claude/settings.json, .claude/hooks/**.
//
// This is a guardrail, not a security boundary: the command check is a
// pattern match, and a program can write files the hook never sees. The
// boundary is the evals-lock CI check (.github/workflows/evals-guard.yml).
// Node built-ins only; works with Git Bash and PowerShell on Windows and on
// macOS / Linux.

import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

const CASE_INSENSITIVE = process.platform === "win32" || process.platform === "darwin";

export const PROTECTED_DIRS = ["evals", ".claude/hooks"];
export const PROTECTED_FILES = [
  ".github/workflows/evals.yml",
  ".github/workflows/evals-guard.yml",
  ".claude/settings.json",
];

const REASON =
  "Blocked by the eval lock: %WHAT% is protected (evals/**, .github/workflows/evals.yml, " +
  ".github/workflows/evals-guard.yml, .claude/settings.json, .claude/hooks/**). AI agents may " +
  "read and run the evals but never change them; only the user does, outside Claude Code " +
  '(evals/README.md, "Changing an eval"). If the change is needed, stop and ask the user.';

const reason = (what) => REASON.replace("%WHAT%", what);

const fold = (s) => (CASE_INSENSITIVE ? s.toLowerCase() : s);

/** A repo-relative POSIX path is protected. */
export function isProtectedRel(rel) {
  const r = fold(rel.replace(/\\/g, "/").replace(/^\.\/+/, "").replace(/\/+$/, ""));
  if (PROTECTED_FILES.some((f) => r === fold(f))) return true;
  return PROTECTED_DIRS.some((d) => r === fold(d) || r.startsWith(fold(d) + "/"));
}

/** Git Bash style /c/Users/... → C:/Users/... on Windows. */
function nativePath(p) {
  if (process.platform === "win32") {
    const m = /^\/([a-zA-Z])\/(.*)$/.exec(p);
    if (m) return `${m[1]}:/${m[2]}`;
  }
  return p;
}

/** Candidate repository roots for an absolute path: every ancestor holding
 * `.git` (worktrees and other clones too), plus the project dir. The cwd is
 * a root only when nothing else is found. */
function rootsFor(abs, projectDir, cwd) {
  const roots = new Set();
  if (projectDir) roots.add(path.resolve(nativePath(projectDir)));
  let dir = path.dirname(abs);
  for (;;) {
    if (existsSync(path.join(dir, ".git"))) roots.add(dir);
    const up = path.dirname(dir);
    if (up === dir) break;
    dir = up;
  }
  if (!roots.size && cwd) roots.add(path.resolve(nativePath(cwd)));
  return [...roots];
}

/** The protected repo-relative path `p` points at, or null. */
export function protectedTarget(p, { cwd, projectDir } = {}) {
  if (typeof p !== "string" || !p.trim()) return null;
  const base = cwd ? path.resolve(nativePath(cwd)) : process.cwd();
  const abs = path.resolve(base, nativePath(p.trim()));
  for (const root of rootsFor(abs, projectDir, cwd)) {
    const rel = path.relative(root, abs);
    if (!rel || rel.startsWith("..") || path.isAbsolute(rel)) continue;
    if (isProtectedRel(rel)) return rel.replace(/\\/g, "/");
  }
  return null;
}

// --- commands -------------------------------------------------------------

/** Split a command line into simple commands at ; & && || | and newlines,
 * outside quotes. */
function segments(command) {
  const out = [];
  let cur = "";
  let quote = null;
  for (let i = 0; i < command.length; i++) {
    const c = command[i];
    if (quote) {
      if (c === quote) quote = null;
      else if (c === "\\" && quote === '"' && i + 1 < command.length) {
        cur += c + command[++i];
        continue;
      }
      cur += c;
      continue;
    }
    if (c === '"' || c === "'") {
      quote = c;
      cur += c;
    } else if (
      c === ";" ||
      c === "\n" ||
      c === "\r" ||
      ((c === "&" || c === "|") && command[i - 1] !== ">" && command[i + 1] !== ">")
    ) {
      out.push(cur);
      cur = "";
    } else {
      cur += c;
    }
  }
  out.push(cur);
  return out.map((x) => x.trim()).filter(Boolean);
}

/** Split into words, honoring quotes (enough for a pattern check). */
function words(segment) {
  const out = [];
  const re = /"((?:[^"\\]|\\.)*)"|'([^']*)'|(\S+)/g;
  let m;
  while ((m = re.exec(segment))) out.push(m[1] ?? m[2] ?? m[3]);
  return out;
}

// A textual mention of a protected path: `evals` as a whole word or path
// segment (not `fly-evals` or `evals.rs`), the two workflows, the settings
// file, the hooks folder. Only a cheap pre-filter: words are then resolved
// as real paths, so `target/evals` is not protected.
const MENTION = [
  /(^|[\s'"=:(`/])evals(?=$|[/\s'"`;|&)<>,*])/i,
  /workflows\/evals(-guard)?\.ya?ml/i,
  /\.claude\/settings\.json/i,
  /\.claude\/hooks(?=$|[/\s'"`;|&)<>,*])/i,
];

const mentions = (text) => MENTION.some((re) => re.test(text.replace(/\\/g, "/")));

/** The word, resolved as a path from the command's working directory, is
 * protected. */
function wordProtected(w, ctx) {
  const n = w.replace(/\\/g, "/");
  if (!mentions(n)) return false;
  // --out=evals/x, -Path:evals/x
  const candidates = [n, ...n.split("=").slice(1), n.replace(/^-[A-Za-z]+:/, "")];
  return candidates.some((c) => c && protectedTarget(c, ctx) !== null);
}

// Verbs whose path arguments are written, removed or moved.
const DESTRUCTIVE = new Set([
  "rm", "rmdir", "mv", "touch", "truncate", "chmod", "chown", "shred", "unlink", "mkdir", "tee",
  "dd", "patch",
  // PowerShell cmdlets and aliases
  "remove-item", "move-item", "rename-item", "set-content", "add-content", "out-file",
  "clear-content", "new-item", "tee-object", "set-itemproperty", "expand-archive",
  "del", "erase", "ri", "rd", "move", "mi", "ren", "rni", "sc", "ac", "ni", "md",
]);
// Verbs that write only their destination (the last argument).
const COPY = new Set(["cp", "rsync", "install", "ln", "copy-item", "copy", "cpi", "scp", "xcopy", "robocopy"]);
const GIT_WRITES = new Set([
  "checkout", "restore", "rm", "mv", "reset", "stash", "apply", "am", "clean", "switch",
  "update-index", "update-ref", "filter-branch", "filter-repo", "read-tree", "checkout-index",
]);
const SHELLS = new Set(["bash", "sh", "zsh", "dash", "ksh", "fish", "pwsh", "powershell", "cmd"]);
const INTERPRETER = /(^|[\s;&|(`])(node|nodejs|deno|bun|python[0-9.]*|py|perl|ruby|php)(\.exe)?(\s|$)/i;
const WRITE_API =
  /(write|append|unlink|rename|remov|rmtree|rmsync|rmdir|mkdir|copy|move|truncat|chmod|symlink|set-content|out-file|\bopen\s*\([^)]*['"][wax+])/i;
const NULL_SINK = /^(\/dev\/null|\$null|nul)$/i;
const CD = new Set(["cd", "pushd", "set-location", "sl", "chdir"]);

const verbOf = (w) =>
  path
    .basename(w.replace(/\\/g, "/").replace(/^[({]+/, ""))
    .toLowerCase()
    .replace(/\.exe$/, "");

/** Why a command must be denied, or null. */
export function commandViolation(command, ctx = {}, depth = 0) {
  if (typeof command !== "string" || depth > 4) return null;
  if (/disableAllHooks/i.test(command)) return "disabling hooks (disableAllHooks)";
  let vctx = { ...ctx, cwd: ctx.cwd ? path.resolve(nativePath(ctx.cwd)) : process.cwd() };
  const here = () => protectedTarget(".", vctx) !== null;
  // `fly-evals` is no path, but `cargo fmt -p fly-evals` rewrites evals/.
  if (!mentions(command) && !here() && !/\b(cd|pushd|chdir|set-location|sl)\b|fly-evals/i.test(command)) {
    return null;
  }
  // Inline code for an interpreter can write anything it names.
  if (INTERPRETER.test(command) && WRITE_API.test(command) && (mentions(command) || here())) {
    return "inline code that writes a protected path";
  }
  // $(...) and backticks run commands of their own.
  for (const m of command.matchAll(/\$\(([^()]*)\)|`([^`]*)`/g)) {
    const why = commandViolation(m[1] ?? m[2], vctx, depth + 1);
    if (why) return why;
  }
  for (const segment of segments(command)) {
    const ws = words(segment);
    let i = 0;
    while (
      i < ws.length &&
      (/^[A-Za-z_][A-Za-z0-9_]*=/.test(ws[i]) ||
        /^(sudo|command|exec|env|nohup|time|then|do|else|!|\(|\{)$/.test(ws[i]))
    ) {
      i++;
    }
    if (i >= ws.length) continue;
    const verb = verbOf(ws[i]);
    const args = ws.slice(i + 1);
    const pathArgs = args.filter((a) => !/^-/.test(a));
    const touches = () => here() || args.some((a) => wordProtected(a, vctx));

    // cd moves where relative paths point.
    if (CD.has(verb)) {
      const to = pathArgs[0];
      if (to && to !== "-") {
        vctx = { ...vctx, cwd: path.resolve(vctx.cwd, nativePath(to)) };
      }
      continue;
    }
    // Nested shells and eval/xargs: check the command they run.
    if (SHELLS.has(verb) || verb === "eval" || verb === "xargs") {
      const k = args.findIndex((a) => /^(-c|-lc|-ic|-command|-encodedcommand|\/c|\/k)$/i.test(a));
      if (verb === "eval" || verb === "xargs" || k >= 0) {
        if (k >= 0 && /^-encodedcommand$/i.test(args[k])) return "an encoded PowerShell command";
        const script = verb === "eval" || verb === "xargs" ? args.join(" ") : args.slice(k + 1).join(" ");
        const why = commandViolation(script, vctx, depth + 1);
        if (why) return why;
        continue;
      }
    }
    // Redirects into a protected path: > >> 2> &> >|
    for (const m of segment.matchAll(/(?:^|[^<>&0-9])(?:[0-9]|&)?>{1,2}\|?\s*("[^"]*"|'[^']*'|[^\s;|&<>]+)/g)) {
      const target = m[1].replace(/^["']|["']$/g, "");
      if (!NULL_SINK.test(target) && protectedTarget(target, vctx) !== null) {
        return `a redirect into ${target}`;
      }
    }
    if (DESTRUCTIVE.has(verb) && touches()) return `${verb} on a protected path`;
    if (verb === "sed" && args.some((a) => /^(-i|--in-place|-[a-zA-Z]*i)/.test(a)) && touches()) {
      return "sed -i on a protected path";
    }
    if (verb === "perl" && args.some((a) => /^-[a-zA-Z]*i/.test(a)) && touches()) {
      return "perl -i on a protected path";
    }
    if ((verb === "awk" || verb === "gawk") && args.includes("inplace") && touches()) {
      return "awk -i inplace on a protected path";
    }
    if (COPY.has(verb)) {
      const t = args.findIndex((a) => /^(-t|--target-directory|-destination)$/i.test(a));
      const dest = t >= 0 ? args[t + 1] : pathArgs[pathArgs.length - 1];
      if (dest && protectedTarget(dest, vctx) !== null) return `${verb} into a protected path`;
      continue;
    }
    if (verb === "git") {
      const sub = args.find((a) => !/^-/.test(a));
      if (sub && GIT_WRITES.has(sub.toLowerCase()) && touches()) return `git ${sub} on a protected path`;
      continue;
    }
    if (verb === "find" && args.some((a) => /^-(delete|exec|execdir|ok)$/.test(a)) && touches()) {
      return "find -delete/-exec on a protected path";
    }
    if (verb === "cargo") {
      const sub = (args.find((a) => !/^[-+]/.test(a)) || "").toLowerCase();
      const fixing = (sub === "fmt" && !args.includes("--check")) || sub === "fix" || args.includes("--fix");
      if (fixing && (args.some((a) => /^fly-evals$|evals[/\\]runner/i.test(a)) || here())) {
        return `cargo ${sub} rewriting the eval runner`;
      }
      continue;
    }
    if (verb === "curl" && args.some((a) => /^(-o|--output|-O|--remote-name)$/.test(a)) && touches()) {
      return "curl downloading into a protected path";
    }
    if (verb === "wget" && touches()) return "wget downloading into a protected path";
    if (["tar", "unzip", "7z", "bsdtar"].includes(verb) && touches()) {
      return `${verb} extracting into a protected path`;
    }
  }
  return null;
}

// --- the hook ---------------------------------------------------------------

/** The decision for one PreToolUse payload: null (allow) or a deny reason. */
export function decide(input, env = process.env) {
  const tool = input?.tool_name ?? "";
  const ti = input?.tool_input ?? {};
  const ctx = { cwd: input?.cwd, projectDir: env.CLAUDE_PROJECT_DIR };
  if (JSON.stringify(ti).includes("disableAllHooks")) {
    return reason("switching hooks off (disableAllHooks)");
  }
  if (["Edit", "Write", "MultiEdit", "NotebookEdit"].includes(tool)) {
    const target = protectedTarget(ti.file_path ?? ti.notebook_path, ctx);
    return target ? reason(target) : null;
  }
  if (tool === "Bash" || tool === "PowerShell") {
    const why = commandViolation(ti.command, ctx);
    return why ? reason(why) : null;
  }
  return null;
}

function main() {
  let raw = "";
  try {
    raw = readFileSync(0, "utf8");
    const why = decide(JSON.parse(raw));
    if (why) {
      process.stdout.write(
        JSON.stringify({
          hookSpecificOutput: {
            hookEventName: "PreToolUse",
            permissionDecision: "deny",
            permissionDecisionReason: why,
          },
        }) + "\n",
      );
    }
    process.exit(0);
  } catch (e) {
    // Fail closed only when the call could touch the evals; otherwise don't
    // stand in the way of unrelated work.
    if (/evals|\.claude|disableAllHooks/i.test(raw)) {
      process.stderr.write(`eval lock: couldn't check this call (${e?.message ?? e}); blocked to be safe.\n`);
      process.exit(2);
    }
    process.exit(0);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
