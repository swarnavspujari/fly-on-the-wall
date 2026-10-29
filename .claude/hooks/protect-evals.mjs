#!/usr/bin/env node
// Claude Code PreToolUse hook for the eval permission rule (CLAUDE.md,
// evals/README.md "The charter"): agents change the system evals only with
// the user's approval.
//
// Asks the user (permissionDecision "ask": they approve or decline each
// call), with a reason saying it's an eval change:
//   * Edit / Write / MultiEdit / NotebookEdit on a protected path;
//   * Bash / PowerShell commands that write, move, delete, format, extract,
//     download or redirect into a protected path (or delete a folder that
//     holds one), or check one out / restore / remove / patch it with git.
// Denies outright, with a reason Claude sees:
//   * the trust roots: anything naming EVALS_LOCK_SHA256, `gh variable`
//     changes, GitHub API calls for variables, rulesets or branch
//     protection, GraphQL mutations, and `gh pr merge --admin` (agents
//     never use an admin bypass; only the user sets the lock variable);
//   * anything that would switch hooks off (`disableAllHooks`, escaped or
//     not, in any file or command).
// Reads (cat, ls, grep, git diff/log, cargo run -p fly-evals, ...) pass, and
// text inside quoted strings or heredoc bodies is data, not a command
// (unless a shell or interpreter runs it).
//
// Protected, in checkouts of THIS repository only (any worktree or clone,
// recognized by evals/runner/Cargo.toml naming fly-evals): evals/**,
// .github/workflows/evals.yml, .github/workflows/evals-guard.yml,
// .claude/settings.json, .claude/settings.local.json, .claude/hooks/**.
//
// A speed bump, not a boundary: the command check is a pattern match, and a
// program can write files the hook never sees. The rule itself is in
// CLAUDE.md; the CI tripwires and, once the user turns it on, the evals-lock
// check (.github/workflows/evals-guard.yml) back it.
// It fails open: if node or this script is missing, Claude Code treats the
// hook error as non-blocking and the call proceeds. Failing closed would
// block every tool call in any checkout whose main copy (where
// $CLAUDE_PROJECT_DIR points) lacks the script, e.g. before this merges or
// in a stale main checkout. When it can't parse a call that looks like it
// touches the evals, it asks the user.
// Node built-ins only; works with Git Bash and PowerShell on Windows and on
// macOS / Linux.

import { existsSync, readFileSync, realpathSync, statSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

const CASE_INSENSITIVE = process.platform === "win32" || process.platform === "darwin";

export const PROTECTED_DIRS = ["evals", ".claude/hooks"];
export const PROTECTED_FILES = [
  ".github/workflows/evals.yml",
  ".github/workflows/evals-guard.yml",
  ".claude/settings.json",
  // Not in the lock hash (it's local), but it can switch every hook off.
  ".claude/settings.local.json",
];
const SIGNATURE = "evals/runner/Cargo.toml";

const ASK_REASON =
  "Eval change: %WHAT%. This changes the system evals (evals/**, the two eval workflows, " +
  ".claude/settings*.json or .claude/hooks/**). Approve only if you asked for this eval change; " +
  "an eval is never changed to make a result pass (CLAUDE.md, evals/README.md).";
const DENY_REASON =
  "Blocked: %WHAT%. Agents never touch the eval trust roots (the lock variable, repository " +
  "variables, rulesets, branch protection, admin merges) or switch hooks off; only the user " +
  "does that, outside Claude Code. Stop and tell the user what you need.";

const GUARD_REASON =
  "Eval guard change: %WHAT%. This edits the hook that guards the evals or its settings " +
  "(.claude/hooks/**, .claude/settings*.json), not an eval. Approve only if you asked to change " +
  "the guard itself.";

/** A protected-path change: the user approves or declines it. */
const ask = (what) => ({ decision: "ask", reason: ASK_REASON.replace("%WHAT%", what) });
/** A change to the guard itself: asked separately, with its own reason. */
const askGuard = (what) => ({ decision: "ask", reason: GUARD_REASON.replace("%WHAT%", what) });

/** The guard's own files (repo-relative). */
const isGuardRel = (rel) => {
  const r = fold(norm(rel));
  return r === ".claude/hooks" || r.startsWith(".claude/hooks/") || /^\.claude\/settings(\.local)?\.json$/.test(r);
};
/** A command that names one of the guard's own files. */
const namesGuard = (command) =>
  /\.claude[\\/]+(hooks\b|settings(\.local)?\.json)/i.test(String(command ?? "").replace(/["']/g, ""));

/** How .claude/settings.json registers this hook. */
const HOOK_COMMAND = 'node "${CLAUDE_PROJECT_DIR}/.claude/hooks/protect-evals.mjs"';
const GUARDED_TOOLS = ["Edit", "Write", "MultiEdit", "NotebookEdit", "Bash", "PowerShell"];

/** Settings JSON that still registers this hook for every guarded tool
 * (and doesn't switch hooks off). Unparseable means no. */
function keepsRegistration(json) {
  let v;
  try {
    v = JSON.parse(json);
  } catch {
    return false;
  }
  if (v?.disableAllHooks === true || !Array.isArray(v?.hooks?.PreToolUse)) return false;
  return v.hooks.PreToolUse.some((e) => {
    const tools = String(e?.matcher ?? "").split("|");
    return (
      GUARDED_TOOLS.every((t) => tools.includes(t)) &&
      Array.isArray(e?.hooks) &&
      e.hooks.some((h) => h?.type === "command" && h?.command === HOOK_COMMAND)
    );
  });
}

/** .claude/settings.json as a file-tool call would leave it: the Write
 * content, or the Edit / MultiEdit applied to the current file. Null when
 * it can't be worked out (the file is unreadable, or an edit's old text
 * isn't there). */
function settingsAfter(tool, ti, ctx) {
  if (tool === "Write") return String(ti.content ?? "");
  const base = ctx.cwd ? path.resolve(nativePath(ctx.cwd)) : process.cwd();
  const current = safeRead(path.resolve(base, nativePath(String(ti.file_path ?? ""))));
  if (current === null) return null;
  const apply = (text, e) => {
    const from = String(e?.old_string ?? "");
    const to = String(e?.new_string ?? "");
    if (text === null || !from || !text.includes(from)) return null;
    if (e?.replace_all) return text.split(from).join(to);
    const i = text.indexOf(from);
    return text.slice(0, i) + to + text.slice(i + from.length);
  };
  if (tool === "Edit") return apply(current, ti);
  if (tool === "MultiEdit") return (ti.edits ?? []).reduce(apply, current);
  return null;
}

/** A file-tool call that would leave .claude/settings.json without this
 * hook's registration: that switches the hook off, so it's denied. A
 * result that can't be worked out or parsed counts as dropping it. */
function dropsRegistration(tool, ti, ctx) {
  const after = settingsAfter(tool, ti, ctx);
  return after === null || !keepsRegistration(after);
}
/** A trust-root command or a hook switch: always denied. */
const deny = (what) => ({ decision: "deny", reason: DENY_REASON.replace("%WHAT%", what) });
/** A commandViolation result that is denied rather than asked. */
const hard = (why) => ({ hard: true, why });
const fold = (s) => (CASE_INSENSITIVE ? s.toLowerCase() : s);
const norm = (rel) =>
  rel
    .replace(/\\/g, "/")
    .replace(/^(\.\/)+/, "")
    .replace(/\/+$/, "");

/** A repo-relative path is protected (or inside a protected folder). */
export function isProtectedRel(rel) {
  const r = fold(norm(rel));
  if (PROTECTED_FILES.some((f) => r === fold(f))) return true;
  return PROTECTED_DIRS.some((d) => r === fold(d) || r.startsWith(fold(d) + "/"));
}

/** A repo-relative path is protected, or a folder holding a protected path
 * ("." included): deleting or moving it removes protected files. */
function isProtectedOrParentRel(rel) {
  const r = fold(norm(rel));
  if (r === "" || r === "." || isProtectedRel(r)) return true;
  return [...PROTECTED_DIRS, ...PROTECTED_FILES].some((p) => fold(p).startsWith(r + "/"));
}

/** Git Bash style /c/Users/... → C:/Users/... on Windows. */
function nativePath(p) {
  if (process.platform === "win32") {
    const m = /^\/([a-zA-Z])\/(.*)$/.exec(p);
    if (m) return `${m[1]}:/${m[2]}`;
  }
  return p;
}

const MAX_READ = 1 << 20;

/** A regular file of at most 1 MB, or null (never a FIFO, device or
 * folder, so a crafted path can't hang or exhaust the hook). */
function safeRead(file) {
  try {
    const st = statSync(file);
    if (!st.isFile() || st.size > MAX_READ) return null;
    return readFileSync(file, "utf8");
  } catch {
    return null;
  }
}

const repoCache = new Map();
/** `root` is a checkout of this repository. */
function isThisRepo(root) {
  if (!repoCache.has(root)) {
    let ok = false;
    ok = /name\s*=\s*"fly-evals"/.test(safeRead(path.join(root, SIGNATURE)) ?? "");
    repoCache.set(root, ok);
  }
  return repoCache.get(root);
}

/** `p` with symlinks resolved through its nearest existing ancestor. */
function realish(p) {
  let cur = p;
  const rest = [];
  while (!existsSync(cur)) {
    const up = path.dirname(cur);
    if (up === cur) return p;
    rest.unshift(path.basename(cur));
    cur = up;
  }
  try {
    return path.join(realpathSync.native(cur), ...rest);
  } catch {
    return p;
  }
}

/** Roots of this repository that `abs` could live under. */
function rootsFor(abs, projectDir) {
  const roots = new Set();
  const add = (r) => {
    if (isThisRepo(r)) {
      roots.add(r);
      roots.add(realish(r));
    }
  };
  if (projectDir) add(path.resolve(nativePath(projectDir)));
  for (const start of new Set([abs, realish(abs)])) {
    let dir = start;
    for (;;) {
      if (existsSync(path.join(dir, ".git"))) add(dir);
      const up = path.dirname(dir);
      if (up === dir) break;
      dir = up;
    }
  }
  return [...roots];
}

/** Expand the variables a path may use: CLAUDE_PROJECT_DIR, PWD/CD, HOME /
 * USERPROFILE / ~, and simple assignments seen earlier in the command. */
function expand(w, ctx) {
  const home = os.homedir();
  const vars = ctx.vars || {};
  return w
    .replace(/^~(?=$|[\\/])/, home)
    .replace(
      /\$\{?env:([A-Za-z_][A-Za-z0-9_]*)\}?|\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)|%([A-Za-z_][A-Za-z0-9_]*)%/gi,
      (m, a, b, c, d) => {
        const name = a || b || c || d;
        const up = name.toUpperCase();
        if (up === "CLAUDE_PROJECT_DIR") return ctx.projectDir || m;
        if (up === "PWD" || up === "CD") return ctx.cwd || process.cwd();
        if (up === "HOME" || up === "USERPROFILE") return home;
        const v = vars[name.toLowerCase()];
        return v !== undefined ? v : m;
      },
    );
}

const unexpanded = (w) => /\$\{?[A-Za-z_]|%[A-Za-z_][A-Za-z0-9_]*%/.test(w);

/** Repo-relative paths (logical and symlink-resolved) `p` lands on. */
function locate(p, ctx) {
  if (typeof p !== "string" || !p.trim()) return [];
  const base = ctx.cwd ? path.resolve(nativePath(ctx.cwd)) : process.cwd();
  const abs = path.resolve(base, nativePath(expand(p.trim(), ctx)));
  const out = [];
  for (const cand of new Set([abs, realish(abs)])) {
    for (const root of rootsFor(cand, ctx.projectDir)) {
      const rel = path.relative(root, cand);
      if (rel.startsWith("..") || path.isAbsolute(rel)) continue;
      out.push(rel.replace(/\\/g, "/") || ".");
    }
  }
  return out;
}

/** The protected repo-relative path `p` points at, or null. */
export function protectedTarget(p, ctx = {}) {
  return locate(p, ctx).find(isProtectedRel) ?? null;
}

const protectedOrParent = (p, ctx) => locate(p, ctx).some(isProtectedOrParentRel);

/** A glob could expand to a protected path (or, with `parents`, a folder
 * holding one): `rm -rf ev*`, `rm -rf .c*`. */
function globHits(arg, ctx, parents) {
  if (!/[*?[]/.test(arg)) return false;
  const base = ctx.cwd ? path.resolve(nativePath(ctx.cwd)) : process.cwd();
  const abs = path.resolve(base, nativePath(expand(arg, ctx)));
  const pattern = fold(abs.replace(/\\/g, "/"));
  const source = pattern
    .split("")
    .map((c) => (c === "*" ? "[^/]*" : c === "?" ? "[^/]" : /[.+^${}()|\\\]]/.test(c) ? "\\" + c : c))
    .join("");
  let re;
  try {
    re = new RegExp("^" + source + "$");
  } catch {
    return true; // can't tell: treat as hitting
  }
  const stem = abs.replace(/[*?[].*$/, "");
  for (const root of rootsFor(stem || abs, ctx.projectDir)) {
    for (const p of [...PROTECTED_DIRS, ...PROTECTED_FILES]) {
      const parts = p.split("/");
      for (let i = parents ? 1 : parts.length; i <= parts.length; i++) {
        const cand = fold(path.join(root, ...parts.slice(0, i)).replace(/\\/g, "/"));
        if (re.test(cand)) return true;
      }
    }
  }
  return false;
}

// --- parsing ------------------------------------------------------------

/** Is index `at` of `line` inside a quoted string? */
function inQuotes(line, at) {
  let q = null;
  for (let i = 0; i < at; i++) {
    const c = line[i];
    if (q) {
      if (c === q) q = null;
      else if (c === "\\" && q === '"') i++;
    } else if (c === '"' || c === "'") q = c;
  }
  return q !== null;
}

/** Remove heredoc bodies and PowerShell here-strings; return the command
 * without them plus each body with the verb of the command that reads it. */
function stripBodies(command) {
  const bodies = [];
  let text = command.replace(/@(['"])\r?\n([\s\S]*?)\r?\n\1@/g, (_, _q, body) => {
    bodies.push({ owners: ["here-string"], body });
    return "'HERE-STRING'";
  });
  const lines = text.split(/\r?\n/);
  const out = [];
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    out.push(line);
    const re = /(?<!<)<<(-?)\s*(['"]?)([A-Za-z_][A-Za-z0-9_]*)\2/g;
    let m;
    const pending = [];
    while ((m = re.exec(line))) {
      if (line[m.index + 2] === "<" || inQuotes(line, m.index)) continue;
      pending.push({ dash: m[1] === "-", delim: m[3], at: m.index });
    }
    for (const h of pending) {
      const body = [];
      i++;
      while (i < lines.length) {
        const l = h.dash ? lines[i].replace(/^\t+/, "") : lines[i];
        if (l.trim() === h.delim) break;
        body.push(lines[i]);
        i++;
      }
      // The body's readers: the command holding `<<`, plus every command
      // the rest of the line pipes it into (`cat <<EOF | bash`). Each one
      // is checked; any failing check denies.
      const owner = segments(line.slice(0, h.at)).pop() || "";
      const readers = [realVerb(words(owner))];
      for (const s of segments(line.slice(h.at)).slice(1)) readers.push(realVerb(words(s)));
      bodies.push({ owners: readers.filter(Boolean), body: body.join("\n") });
    }
  }
  return { text: out.join("\n"), bodies };
}

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

/** Redirect targets (`>`, `>>`, `2>`, `&>`, `>|`) and input sources (`<`),
 * outside quotes, with or without a space before the target. */
function redirects(segment) {
  const outs = [];
  const ins = [];
  let q = null;
  for (let i = 0; i < segment.length; i++) {
    const c = segment[i];
    if (q) {
      if (c === q) q = null;
      continue;
    }
    if (c === '"' || c === "'") {
      q = c;
      continue;
    }
    if (c !== ">" && c !== "<") continue;
    let j = i + 1;
    // `<<EOF` / `<<<word`: a heredoc or here-string, not an input file.
    const heredoc = c === "<" && segment[j] === "<";
    while (j < segment.length && /[<>|&]/.test(segment[j])) j++;
    if (segment[j] === "(") {
      i = j; // process substitution
      continue;
    }
    while (j < segment.length && /\s/.test(segment[j])) j++;
    let t = "";
    if (segment[j] === '"' || segment[j] === "'") {
      const k = segment.indexOf(segment[j], j + 1);
      t = segment.slice(j + 1, k < 0 ? undefined : k);
      j = k < 0 ? segment.length : k + 1;
    } else {
      while (j < segment.length && !/[\s;|&<>]/.test(segment[j])) t += segment[j++];
    }
    if (t && !heredoc && !/^\d+$/.test(t)) (c === ">" ? outs : ins).push(t);
    i = j - 1;
  }
  return { outs, ins };
}

// A textual mention of a protected name. Only a cheap hint: words are
// resolved as real paths before anything is denied on their account.
const MENTION = [
  /(^|[^A-Za-z0-9_.-])evals(?=$|[^A-Za-z0-9_-])/i,
  /workflows\/evals(-guard)?\.ya?ml/i,
  /\.claude\/settings(\.local)?\.json/i,
  /\.claude\/hooks(?=$|[^A-Za-z0-9_-])/i,
];
const mentions = (text) => MENTION.some((re) => re.test(text.replace(/\\/g, "/")));

/** The word names a protected path (after expansion; `--out=evals/x` and
 * `-Path:evals/x` forms too). */
function wordProtected(w, ctx) {
  const n = w.replace(/\\/g, "/");
  const forms = [n, ...n.split("=").slice(1), n.replace(/^-[A-Za-z]+:/, "")];
  return forms.some((c) => c && (protectedTarget(c, ctx) !== null || globHits(c, ctx, false)));
}
function wordRemovesProtected(w, ctx) {
  const n = w.replace(/\\/g, "/");
  const forms = [n, ...n.split("=").slice(1), n.replace(/^-[A-Za-z]+:/, "")];
  return forms.some((c) => c && (protectedOrParent(c, ctx) || globHits(c, ctx, true)));
}

// Verbs that remove or move their path arguments (a parent folder counts).
const REMOVERS = new Set([
  "rm", "rmdir", "mv", "unlink", "shred", "remove-item", "move-item", "rename-item",
  "del", "erase", "ri", "rd", "rmdir", "move", "mi", "ren", "rni", "rename",
]);
// Verbs that write their path arguments.
const WRITERS = new Set([
  "touch", "truncate", "chmod", "chown", "mkdir", "tee", "dd", "set-content", "add-content",
  "out-file", "clear-content", "new-item", "tee-object", "set-itemproperty", "expand-archive",
  "sc", "ac", "ni", "md", "icacls", "attrib", "takeown", "setfacl", "chattr",
]);
// Verbs that write only their destination (the last argument).
const COPY = new Set(["cp", "rsync", "install", "ln", "copy-item", "copy", "cpi", "scp", "xcopy", "robocopy"]);
const GIT_WRITES = new Set([
  "checkout", "restore", "rm", "mv", "reset", "stash", "clean", "switch",
  "update-index", "update-ref", "filter-branch", "filter-repo", "read-tree", "checkout-index",
]);
const SHELLS = new Set(["bash", "sh", "zsh", "dash", "ksh", "fish", "pwsh", "powershell", "cmd"]);
const INTERPRETERS = new Set(["node", "nodejs", "deno", "bun", "py", "perl", "ruby", "php", "osascript"]);
const isInterpreter = (v) => INTERPRETERS.has(v) || /^python[0-9.]*$/.test(v);
const RUNNERS = new Set([
  "iex", "invoke-expression", "start-process", "saps", "start", "invoke-command", "icm", "&",
  "eval", "xargs", "exec", "call", "time", "nice", "timeout", "watch", "su", "runas", "wsl",
]);
const PS_PARAMS = /^-(FilePath|ArgumentList|Args|Wait|NoNewWindow|PassThru|Verb|WindowStyle|ScriptBlock|Command|C|NoProfile|NonInteractive|NoLogo|ExecutionPolicy|WorkingDirectory|Credential|ComputerName|Session)$/i;
const WRITE_API =
  /(write|append|unlink|rename|remov|rmtree|rmsync|rmdir|mkdir|copy|move|truncat|chmod|symlink|set-content|out-file|\bopen\s*\([^)]*['"][wax+])/i;
const DOTNET_WRITE =
  /\[(?:system\.)?io\.(?:file|directory|fileinfo|directoryinfo)\]::\s*(?:write|append|delete|move|copy|create|replace|open|set|encrypt|decrypt)|(?:new-object\s+(?:-typename\s+)?|\[)(?:system\.)?io\.(?:streamwriter|filestream)/i;
const NULL_SINK = /^(\/dev\/null|\$null|nul)$/i;
const CD = new Set(["cd", "pushd", "push-location", "set-location", "sl", "chdir"]);
// Formatters that rewrite files: verb → does this invocation write?
const FORMATTERS = {
  rustfmt: (a) => !a.includes("--check"),
  prettier: (a) => a.some((x) => /^(--write|-w)$/.test(x)),
  eslint: (a) => a.includes("--fix"),
  biome: (a) => a.some((x) => /^(--write|--apply|--apply-unsafe|--fix)$/.test(x)),
  dprint: (a) => a.includes("fmt"),
  black: (a) => !a.includes("--check") && !a.includes("--diff"),
  ruff: (a) => (a.includes("format") && !a.includes("--check")) || a.includes("--fix"),
  gofmt: (a) => a.includes("-w"),
  "clang-format": (a) => a.includes("-i"),
  taplo: (a) => a.some((x) => /^(fmt|format)$/.test(x)) && !a.includes("--check"),
  shfmt: (a) => a.includes("-w"),
};
// Launchers that run their (later) arguments as a command, with options
// or numbers in between: `timeout 5 bash`, `nice -n 5 sh`.
const LAUNCHERS = new Set([
  "timeout", "nice", "time", "stdbuf", "ionice", "chrt", "taskset", "setsid", "exec",
  "builtin", "nohup", "env", "sudo", "doas", "command", "unbuffer", "script",
]);
// Inline-code flags: shells run their next argument as a command,
// interpreters as code.
const SHELL_INLINE = /^(-c|-lc|-ic|-xc|-ec|-command|-c[a-z]*|\/c|\/k)$/i;
// Per interpreter: the flags that take code inline. An interpreter whose
// flag isn't listed is treated as reading stdin (fail closed): `perl -p`
// loops over stdin, `ruby -r x` still reads its script from stdin.
const INLINE_FLAGS = {
  python: /^-[bBdEhiIOqsSuvx]*c$/,
  py: /^-[bBdEhiIOqsSuvx]*c$/,
  node: /^(-e|--eval|-p|--print)$/,
  nodejs: /^(-e|--eval|-p|--print)$/,
  bun: /^(-e|--eval|-p|--print)$/,
  deno: /^(eval)$/,
  perl: /^-[aclnpsTtUuWwX0-9]*[eE]$/,
  ruby: /^-[acdlnpsvwyW]*e$/,
  php: /^-r$/,
  osascript: /^-e$/,
};
// Per interpreter: options whose value is the NEXT word. python -E/-I
// take none; perl attaches its values (-Idir, -Mmod); php -f names the
// script itself. Shells: -o/-O/+o/+O and --rcfile / --init-file.
const VALUE_OPTS = {
  python: /^-[WXmQ]$/,
  py: /^-[WXmQ]$/,
  node: /^(-r|--require|--import|--loader|--experimental-loader|--input-type|-C|--conditions)$/,
  nodejs: /^(-r|--require|--import|--loader|--experimental-loader|--input-type|-C|--conditions)$/,
  bun: /^(-r|--require|--preload|--cwd|--config|-c)$/,
  deno: /^(--config|-c|--import-map|--location|--cert)$/,
  perl: /^$/,
  ruby: /^-[rIECX]$/,
  php: /^-[dcz]$/,
  osascript: /^-l$/,
};
const SHELL_VALUE_OPTS = /^([-+][oO]|--rcfile|--init-file)$/;
const valueOpt = (v, x, shell) => {
  if (shell) return SHELL_VALUE_OPTS.test(x);
  const key = /^python[0-9.]*$/.test(v) ? "python" : v;
  return VALUE_OPTS[key] ? VALUE_OPTS[key].test(x) : false;
};

const inlineFlag = (v, x) => {
  const key = /^python[0-9.]*$/.test(v) ? "python" : v;
  return INLINE_FLAGS[key] ? INLINE_FLAGS[key].test(x) : false;
};

// Package runners whose next word is the real command.
const WRAPPERS = new Set(["npx", "bunx", "pnpx", "uvx", "sudo", "command", "env", "nohup", "doas"]);

const verbOf = (w) =>
  path
    .basename(String(w).replace(/\\/g, "/").replace(/^[({&]+/, ""))
    .toLowerCase()
    .replace(/\.(exe|cmd|bat|ps1)$/, "");

/** Index of the first word that is the command (past env assignments,
 * sudo/env/npx-style wrappers, `npm exec` and friends). */
function skipPrefixes(ws) {
  let i = 0;
  for (;;) {
    if (i >= ws.length) return i;
    const w = ws[i];
    const v = verbOf(w);
    if (/^[A-Za-z_][A-Za-z0-9_]*=/.test(w) || /^(then|do|else|!|\(|\{|--)$/.test(w)) i++;
    else if (WRAPPERS.has(v)) {
      i++;
      while (i < ws.length && /^-/.test(ws[i])) i++;
    } else if (["npm", "pnpm", "yarn"].includes(v) && ["exec", "dlx", "x"].includes((ws[i + 1] || "").toLowerCase())) {
      i += 2;
      while (i < ws.length && /^-/.test(ws[i])) i++;
    } else if (v === "pipx" && ws[i + 1] === "run") i += 2;
    else return i;
  }
}

/** The real verb of a simple command: past prefixes, wrappers and
 * launchers (and their options / numeric arguments). */
function realVerb(ws) {
  const k = realVerbIndex(ws);
  return k < ws.length ? verbOf(ws[k]) : "";
}

/** Index of the real verb (see [`realVerb`]). */
function realVerbIndex(ws) {
  // A wrapper or launcher first (`sudo -u x bash`, `env -u X sh`): its
  // options can take values, so take the first shell or interpreter after
  // it (fail closed).
  let k0 = 0;
  while (k0 < ws.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(ws[k0])) k0++;
  if (k0 < ws.length && (LAUNCHERS.has(verbOf(ws[k0])) || WRAPPERS.has(verbOf(ws[k0])))) {
    for (let j = k0 + 1; j < ws.length; j++) {
      const v = verbOf(ws[j]);
      if (SHELLS.has(v) || isInterpreter(v)) return j;
    }
  }
  let k = skipPrefixes(ws);
  if (k < ws.length && LAUNCHERS.has(verbOf(ws[k]))) {
    // Launcher options can take values (`sudo -u x`, `timeout -s KILL 5`):
    // rather than know each one, take the first shell or interpreter after
    // it (fail closed).
    for (let j = k + 1; j < ws.length; j++) {
      const v = verbOf(ws[j]);
      if (SHELLS.has(v) || isInterpreter(v)) return j;
    }
    while (k < ws.length && LAUNCHERS.has(verbOf(ws[k]))) {
      k++;
      while (k < ws.length && (/^[-+]/.test(ws[k]) || /^\d/.test(ws[k]) || /^[A-Za-z_][A-Za-z0-9_]*=/.test(ws[k]))) k++;
    }
  }
  return k;
}

/** Paths a patch (git diff / unified diff) touches, or null if unreadable. */
function patchPaths(file, ctx) {
  const base = ctx.cwd ? path.resolve(nativePath(ctx.cwd)) : process.cwd();
  const text = safeRead(path.resolve(base, nativePath(expand(file, ctx))));
  if (text === null) return null;
  const out = [];
  for (const line of text.split(/\r?\n/)) {
    let m;
    if ((m = /^diff --git (\S+) (\S+)/.exec(line))) out.push(m[1], m[2]);
    else if ((m = /^(?:\+\+\+|---) (\S+)/.exec(line))) out.push(m[1]);
    else if ((m = /^(?:rename|copy) (?:from|to) (\S+)/.exec(line))) out.push(m[1]);
  }
  return out;
}

/** A patch touches a protected path under ANY prefix strip (`-p0`..`-pN`,
 * so `a/b/evals/x` counts) and any `--directory` / `-d` prefix: fail
 * closed rather than model each tool's rules. */
function patchHitsProtected(paths, prefixes = []) {
  for (const raw of paths) {
    const parts = raw.replace(/\\/g, "/").split("/").filter(Boolean);
    for (let i = 0; i < parts.length; i++) {
      const tail = parts.slice(i).join("/");
      for (const d of ["", ...prefixes]) {
        if (isProtectedRel(d ? `${d.replace(/\/+$/, "")}/${tail}` : tail)) return true;
      }
    }
  }
  return false;
}

/** `--directory=x` / `--directory x` / `-d x` values, and whether a
 * `-p<N>` / `-p N` strip option is present. */
function patchOptions(args, dirFlags) {
  const dirs = [];
  let strip = false;
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    const eq = /^(--directory|--dir)=(.+)$/.exec(a);
    if (eq) dirs.push(eq[2]);
    else if (dirFlags.includes(a) && args[i + 1]) dirs.push(args[++i]);
    else if (/^-p\d*$/.test(a) || /^--strip(=\d+)?$/.test(a)) strip = true;
  }
  return { dirs, strip };
}

/** Commands on the eval trust roots, whatever their arguments. */
function trustRootViolation(cmd) {
  if (/EVALS_LOCK_SHA256/i.test(cmd)) return "touching the eval lock variable (EVALS_LOCK_SHA256)";
  if (/\bgh\b(?:[^\n;|&]*\s)?variable\s+(set|delete|import|remove)\b/i.test(cmd)) {
    return "changing repository variables (gh variable)";
  }
  if (
    /(actions\/variables|\/rulesets\b|branches\/\S*\/protection|environments\/\S+\/variables)/i.test(cmd) &&
    /\b(gh|curl|wget|http|https|irm|iwr|invoke-restmethod|invoke-webrequest|fetch)\b/i.test(cmd)
  ) {
    return "calling the GitHub API for variables, rulesets or branch protection";
  }
  if (/\bgh\b(?:[^\n;|&]*\s)?pr\s+merge\b[^\n;|&]*--admin\b/i.test(cmd)) {
    return "an admin merge (gh pr merge --admin)";
  }
  // GraphQL: any mutation, any query loaded from a file or stdin (it can't
  // be inspected), and anything touching rulesets, protection or variables.
  if (
    /\bgraphql\b/i.test(cmd) &&
    (/\bmutation\b/i.test(cmd) ||
      /@|--input\b|--data|--post-file|--upload-file|(^|\s)-T\b|\.graphql\b/i.test(cmd) ||
      /(branchProtectionRule|repositoryRuleset|ruleset|bypass|actionsVariable|updateRepository)/i.test(cmd))
  ) {
    return "a GraphQL mutation, or a query that can't be inspected";
  }
  return null;
}

/** Decode \uXXXX / \xXX escapes and drop PowerShell backticks and cmd
 * carets, so an escaped `disableAllHooks` is still seen. */
function decoded(text) {
  return String(text)
    .replace(/\\u\{?([0-9a-fA-F]{4})\}?/g, (_, h) => String.fromCharCode(parseInt(h, 16)))
    .replace(/\\x([0-9a-fA-F]{2})/g, (_, h) => String.fromCharCode(parseInt(h, 16)))
    .replace(/[`^]/g, "");
}
const disablesHooks = (text) => /disableallhooks/i.test(decoded(text).replace(/[\\"'+\s]/g, ""));

/** Why a command changes the evals (a string: ask the user), or touches a
 * trust root or switches hooks off ({ hard, why }: deny), or null. */
export function commandViolation(command, ctx = {}, depth = 0) {
  if (typeof command !== "string" || depth > 5) return null;
  if (disablesHooks(command)) return hard("switching hooks off (disableAllHooks)");
  const trust = trustRootViolation(decoded(command));
  if (trust) return hard(trust);

  let vctx = {
    ...ctx,
    cwd: ctx.cwd ? path.resolve(nativePath(ctx.cwd)) : process.cwd(),
    vars: { ...(ctx.vars || {}) },
  };
  const here = () => protectedTarget(".", vctx) !== null;
  const { text, bodies } = stripBodies(command);
  const anyMention = mentions(text) || bodies.some((b) => mentions(b.body));
  // For patches we can't read: any trace of the protected names.
  const looseMention = /evals|\.claude/i.test(command);

  // Bodies run by a shell or an interpreter are commands too.
  const RUNS_STDIN = ["xargs", "iex", "invoke-expression", "eval", "source", "."];
  for (const b of bodies) {
    for (const owner of b.owners) {
      if (
        SHELLS.has(owner) ||
        RUNS_STDIN.includes(owner) ||
        (owner === "here-string" && /\b(iex|invoke-expression)\b/i.test(text))
      ) {
        const why = commandViolation(b.body, vctx, depth + 1);
        if (why) return why;
      } else if (isInterpreter(owner) && WRITE_API.test(b.body) && (mentions(b.body) || here())) {
        return `${owner} code that writes a protected path`;
      }
    }
  }
  // A shell or interpreter reading stdin (`echo "rm evals/x" | bash`,
  // `cat x.py | python -`) runs what the other segments feed it.
  const segs = segments(text);
  for (const s of segs) {
    const ws = words(s);
    const v = verbOf(ws[realVerbIndex(ws)] || "");
    if (!SHELLS.has(v) && !isInterpreter(v)) continue;
    for (const src of redirects(s).ins) {
      const t = safeRead(path.resolve(vctx.cwd, nativePath(expand(src, vctx))));
      if (SHELLS.has(v)) {
        if (t === null) return "a file redirected into a shell that couldn't be checked";
        const why = commandViolation(t, vctx, depth + 1);
        if (why) return why;
      } else if (t === null ? looseMention : WRITE_API.test(t) && (mentions(t) || here())) {
        return `${v} code from a redirected file that writes a protected path`;
      }
    }
  }
  const stdinReaders = segs
    .map((s) => {
      const ws = words(s);
      const k = realVerbIndex(ws);
      const v = verbOf(ws[k] || "");
      const rest = ws.slice(k + 1);
      const shell = SHELLS.has(v);
      if (!shell && !isInterpreter(v)) return null;
      // One walk in argument order, as the program parses them: option
      // values are skipped; the first inline-code flag (with code after
      // it), `-`, `-s`, `--` or script word decides. Words after the
      // script or `-` are the script's own arguments (`python - -c x`
      // still reads stdin).
      let reads = true; // no script at all: reads stdin
      for (let j = 0; j < rest.length; j++) {
        const x = rest[j];
        if (/^[0-9&]*(<|>)+\|?$/.test(x)) {
          j++; // redirect operator and its target
          continue;
        }
        if (/^[0-9&]*[<>]/.test(x)) continue;
        // Options whose value is the next word (per program, see VALUE_OPTS).
        if (valueOpt(v, x, shell)) {
          j++;
          continue;
        }
        // deno / bun subcommands (`deno run -`, `bun run -`, `bun x`): keep
        // walking for what they run.
        if ((v === "deno" || v === "bun") && /^(run|x|exec)$/.test(x)) continue;
        if (shell ? SHELL_INLINE.test(x) : inlineFlag(v, x)) {
          reads = j + 1 >= rest.length; // inline code needs a code word
          break;
        }
        if (x === "-" || (shell && x === "-s")) {
          reads = true;
          break;
        }
        if (x === "--") {
          const next = rest[j + 1];
          reads = !next || next === "-";
          break;
        }
        if (!/^[-+]/.test(x)) {
          reads = false; // a script file
          break;
        }
      }
      // Fail closed: any `-` (or a shell's `-s`) anywhere still means stdin.
      if (!reads && (rest.includes("-") || (shell && rest.includes("-s")))) reads = true;
      return reads ? v : null;
    })
    .filter(Boolean);
  if (stdinReaders.length) {
    if (stdinReaders.some(isInterpreter) && WRITE_API.test(command) && (anyMention || here())) {
      return "code piped into an interpreter that writes a protected path";
    }
    if (stdinReaders.some((v) => SHELLS.has(v))) {
      for (const s of segs) {
        const ws = words(s);
        const k = realVerbIndex(ws);
        const v = verbOf(ws[k] || "");
        // What a feeder prints: its arguments, or a file it cats. A file we
        // can't check (missing, not regular, over 1 MB) fed to a shell is
        // denied.
        let fed = ws.slice(k + 1);
        if (["cat", "type", "get-content", "gc", "tac", "head", "tail"].includes(v)) {
          fed = [];
          // (Redirect tokens aren't files; a heredoc body was checked above.)
          const files = ws.slice(k + 1).filter((x) => !/^-/.test(x) && !/^[0-9&]*[<>]/.test(x));
          for (const f of [...files.filter((x) => !redirects(s).outs.includes(x)), ...redirects(s).ins]) {
            const t = safeRead(path.resolve(vctx.cwd, nativePath(expand(f, vctx))));
            if (t === null) return "a file fed to a shell that couldn't be checked";
            fed.push(t);
          }
        }
        for (const piece of fed) {
          const why = commandViolation(piece.replace(/\\n/g, "\n"), vctx, depth + 1);
          if (why) return why;
        }
      }
    }
  }
  // $(...) and backticks run commands of their own.
  for (const m of text.matchAll(/\$\(([^()]*)\)|`([^`]*)`/g)) {
    const why = commandViolation(m[1] ?? m[2], vctx, depth + 1);
    if (why) return why;
  }

  for (const segment of segments(text)) {
    // Simple assignments: bash `p=evals/x`, PowerShell `$p = 'evals\x'`,
    // cmd `set p=evals\x`.
    let a;
    if ((a = /^\$([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.+)$/.exec(segment)) || (a = /^set\s+([A-Za-z_][A-Za-z0-9_]*)=(.*)$/i.exec(segment))) {
      vctx.vars[a[1].toLowerCase()] = expand(a[2].trim().replace(/^["']|["']$/g, ""), vctx);
      continue;
    }
    const ws = words(segment);
    for (const w of ws) {
      const m = /^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/.exec(w);
      if (!m) break;
      vctx.vars[m[1].toLowerCase()] = expand(m[2].replace(/^["']|["']$/g, ""), vctx);
    }
    const i = skipPrefixes(ws);
    if (i >= ws.length) continue;
    const verb = verbOf(ws[i]);
    const args = ws.slice(i + 1);
    const pathArgs = args.filter((x) => !/^-/.test(x));
    const touches = () => here() || args.some((x) => wordProtected(x, vctx));
    const removes = () => here() || pathArgs.some((x) => wordRemovesProtected(x, vctx));
    // A variable we couldn't expand might point anywhere.
    const opaque = (xs) => xs.some((x) => unexpanded(expand(x, vctx))) && anyMention;

    if (CD.has(verb)) {
      const to = pathArgs[0];
      if (to && to !== "-") vctx = { ...vctx, cwd: path.resolve(vctx.cwd, nativePath(expand(to, vctx))) };
      continue;
    }
    // Shells, eval-likes and launchers: check the command they run.
    if (SHELLS.has(verb) || RUNNERS.has(verb)) {
      if (args.some((x) => /^-(e|en|enc|encodedcommand)$/i.test(x)) && PS_PARAMS.test("-Command") && /pwsh|powershell/.test(verb)) {
        return "an encoded PowerShell command";
      }
      const k = args.findIndex((x) => /^(-c|-lc|-ic|-command|\/c|\/k)$/i.test(x));
      const inner = SHELLS.has(verb)
        ? k >= 0
          ? args.slice(k + 1)
          : null
        : args.filter((x) => !PS_PARAMS.test(x));
      if (inner) {
        const why = commandViolation(inner.join(" "), vctx, depth + 1);
        if (why) return why;
        continue;
      }
    }
    // Inline interpreter code (node -e, python -c, perl -e, ...).
    if (isInterpreter(verb)) {
      const code = args.join(" ");
      if (WRITE_API.test(code) && (mentions(code) || here() || opaque(args))) {
        return `${verb} code that writes a protected path`;
      }
    }
    // .NET file APIs from PowerShell.
    if (DOTNET_WRITE.test(segment) && (mentions(segment) || here() || (/\$\w/.test(segment) && anyMention))) {
      return ".NET file APIs writing a protected path";
    }
    // Redirects into a protected path.
    const { outs, ins } = redirects(segment);
    for (const t of outs) {
      if (NULL_SINK.test(t)) continue;
      if (protectedTarget(t, vctx) !== null || globHits(t, vctx, false)) return `a redirect into ${t}`;
      if (unexpanded(expand(t, vctx)) && anyMention) return `a redirect into ${t} (a variable that may point into a protected path)`;
    }
    if (REMOVERS.has(verb) && (removes() || opaque(pathArgs))) return `${verb} on a protected path`;
    if (WRITERS.has(verb) && (touches() || opaque(pathArgs))) return `${verb} on a protected path`;
    if (verb === "sed" && args.some((x) => /^(-i|--in-place|-[a-zA-Z]*i)/.test(x)) && touches()) {
      return "sed -i on a protected path";
    }
    if (verb === "perl" && args.some((x) => /^-[a-zA-Z]*i/.test(x)) && touches()) return "perl -i on a protected path";
    if ((verb === "awk" || verb === "gawk") && args.includes("inplace") && touches()) {
      return "awk -i inplace on a protected path";
    }
    if (FORMATTERS[verb] && FORMATTERS[verb](args) && touches()) return `${verb} rewriting a protected path`;
    if (COPY.has(verb)) {
      const t = args.findIndex((x) => /^(-t|--target-directory|-destination)$/i.test(x));
      const dest = t >= 0 ? args[t + 1] : pathArgs[pathArgs.length - 1];
      const recursive = args.some((x) => /^(-[a-zA-Z]*[rRa][a-zA-Z]*|--recursive|-recurse|\/s|\/e|\/mir)$/i.test(x));
      if (
        dest &&
        (wordProtected(dest, vctx) ||
          (recursive && wordRemovesProtected(dest, vctx)) ||
          (here() && !path.isAbsolute(nativePath(dest))) ||
          opaque([dest]))
      ) {
        return `${verb} into a protected path`;
      }
      continue;
    }
    if (verb === "patch") {
      const k = args.findIndex((x) => /^(-i|--input)$/.test(x));
      const sources = [...(k >= 0 ? [args[k + 1]] : []), ...ins];
      const { dirs, strip } = patchOptions(args, ["-d", "--directory"]);
      // Applied inside a protected folder, into one, or with path
      // rewriting in a command that names one: denied whatever the file.
      if (here() || dirs.some((d) => protectedTarget(d, vctx) !== null)) return "patch applied inside a protected path";
      if ((strip || dirs.length) && looseMention) return "patch with -p/-d in a command naming a protected path";
      for (const s of sources) {
        const ps = patchPaths(s, vctx);
        if (ps === null ? looseMention : patchHitsProtected(ps, dirs)) return "patch touching a protected path";
      }
      if (!sources.length && (touches() || anyMention)) return "patch touching a protected path";
      continue;
    }
    if (verb === "git") {
      let gctx = vctx;
      let k = 0;
      while (k < args.length && /^-/.test(args[k])) {
        if (args[k] === "-C" && args[k + 1]) {
          gctx = { ...gctx, cwd: path.resolve(gctx.cwd, nativePath(expand(args[k + 1], gctx))) };
          k += 2;
        } else if (args[k] === "-c") k += 2;
        else k++;
      }
      const sub = (args[k] || "").toLowerCase();
      const rest = args.slice(k + 1);
      const gitRoot = rootsFor(path.join(gctx.cwd, "x"), gctx.projectDir)[0];
      // Pathspec magic: `:/evals`, `:(top)evals`; exclusions don't target.
      const gitPath = (x) => {
        const m = /^:(?:\(([^)]*)\))?(\/?)(.*)$/.exec(x);
        if (!m) return x;
        if (/^[!^]/.test(m[3]) || /exclude/.test(m[1] || "")) return null;
        const top = m[2] === "/" || /top/.test(m[1] || "");
        return top && gitRoot ? path.join(gitRoot, m[3]) : m[3];
      };
      const gitHere = protectedTarget(".", gctx) !== null;
      if (sub === "apply" || sub === "am") {
        const { dirs, strip } = patchOptions(rest, ["--directory"]);
        // From inside a protected folder, or with path rewriting in a
        // command that names one: denied even when a patch file is named.
        if (gitHere || dirs.some((d) => isProtectedRel(d))) return `git ${sub} inside a protected path`;
        if ((strip || dirs.length) && looseMention) return `git ${sub} with -p/--directory in a command naming a protected path`;
        const files = [...rest.filter((x) => !/^-/.test(x) && !dirs.includes(x)), ...ins];
        for (const f of files) {
          const ps = patchPaths(f, gctx);
          if (ps === null ? looseMention : patchHitsProtected(ps, dirs)) return `git ${sub} of a patch touching a protected path`;
        }
        if (!files.length && (anyMention || gitHere)) return `git ${sub} touching a protected path`;
        continue;
      }
      if (GIT_WRITES.has(sub)) {
        const hit = rest.some((x) => {
          const p = gitPath(x);
          return p !== null && (protectedTarget(p, gctx) !== null || globHits(p, gctx, false));
        });
        if (hit || (gitHere && sub !== "switch")) return `git ${sub} on a protected path`;
      }
      continue;
    }
    if (verb === "find" && args.some((x) => /^-(delete|exec|execdir|ok)$/.test(x)) && touches()) {
      return "find -delete/-exec on a protected path";
    }
    if (verb === "cargo") {
      const sub = (args.find((x) => !/^[-+]/.test(x)) || "").toLowerCase();
      const fixing = (sub === "fmt" && !args.includes("--check")) || sub === "fix" || args.includes("--fix");
      if (fixing && (args.some((x) => /^fly-evals$|evals[/\\]runner/i.test(x)) || here())) {
        return `cargo ${sub} rewriting the eval runner`;
      }
      continue;
    }
    if (verb === "curl" && args.some((x) => /^(-o|--output|-O|--remote-name)$/.test(x)) && touches()) {
      return "curl downloading into a protected path";
    }
    if (["wget", "invoke-webrequest", "iwr", "invoke-restmethod", "irm"].includes(verb) && touches()) {
      return `${verb} downloading into a protected path`;
    }
    if (["tar", "unzip", "7z", "bsdtar", "expand"].includes(verb) && touches()) {
      return `${verb} extracting into a protected path`;
    }
  }
  return null;
}

// --- the hook ---------------------------------------------------------------

/** The decision for one PreToolUse payload: null (allow), or
 * `{ decision: "ask" | "deny", reason }`. Changes to a protected path ask
 * the user; the trust roots and switching hooks off are denied outright. */
export function decide(input, env = process.env) {
  const tool = input?.tool_name ?? "";
  const ti = input?.tool_input ?? {};
  const ctx = { cwd: input?.cwd, projectDir: env.CLAUDE_PROJECT_DIR };
  if (disablesHooks(JSON.stringify(ti))) return deny("switching hooks off (disableAllHooks)");
  if (["Edit", "Write", "MultiEdit", "NotebookEdit"].includes(tool)) {
    const target = protectedTarget(ti.file_path ?? ti.notebook_path, ctx);
    if (!target) return null;
    if (fold(norm(target)) === fold(".claude/settings.json") && dropsRegistration(tool, ti, ctx)) {
      return deny("removing the eval hook's registration from .claude/settings.json (that switches it off)");
    }
    return isGuardRel(target) ? askGuard(`${target} is part of the eval guard`) : ask(`${target} is protected`);
  }
  if (tool === "Bash" || tool === "PowerShell") {
    const why = commandViolation(ti.command, ctx);
    if (!why) return null;
    if (typeof why === "object") return deny(why.why);
    return namesGuard(ti.command) ? askGuard(why) : ask(why);
  }
  return null;
}

function emit(decision, why) {
  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: decision,
        permissionDecisionReason: why,
      },
    }) + "\n",
  );
}

function main() {
  let raw = "";
  try {
    raw = readFileSync(0, "utf8");
    const verdict = decide(JSON.parse(raw));
    if (verdict) emit(verdict.decision, verdict.reason);
    process.exit(0);
  } catch (e) {
    // Couldn't check it: when the call could touch the evals, the user
    // decides; otherwise don't stand in the way of unrelated work.
    if (/evals|\.claude|disableAllHooks|EVALS_LOCK|variable|ruleset/i.test(raw)) {
      emit(
        "ask",
        `The eval hook couldn't check this call (${e?.message ?? e}); it may change the system evals. Approve only if you asked for that.`,
      );
    }
    process.exit(0);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
