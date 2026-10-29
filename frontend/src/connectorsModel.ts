// Pure logic behind Settings › Connectors and the connector-call cards:
// permissions and groups, statuses, the Add / Edit forms → ConnectorInput,
// "Paste a command", the call cards' argument fields, and activity labels.
// Kept free of React so it is unit-tested.
//
// Everything a server sends (tool names, descriptions, schemas, arguments)
// is untrusted: nothing here interprets it beyond reading plain values, and
// lookups keyed by a server-provided name use Maps or own-property checks.

import type {
  Connector,
  ConnectorCallLogEntry,
  ConnectorCallProposal,
  ConnectorCardAction,
  ConnectorDirectoryEntry,
  ConnectorImportCandidate,
  ConnectorInput,
  ConnectorKV,
  ConnectorPermission,
  ConnectorPermissionChange,
  ConnectorProbe,
  ConnectorRefusal,
  ConnectorTool,
} from "./types";

/* ------------------------------------------------------------- copy */

/** The beta flag: the only setting the UI writes for connectors ("1" / "0"). */
export const CONNECTORS_BETA_KEY = "ui.beta.connectors";

/** For Ask: shown when a question includes a Keep-local meeting. */
export const KEEP_LOCAL_NOTICE =
  "Connectors were off for this question because it includes a Keep-local meeting, which never leaves this computer.";

export const ALWAYS_ALLOW_WRITE_WARNING =
  "Someone speaking in a meeting could word things so the AI uses this without asking you.";
export const UNLABELLED_CHIP = "Not labelled — treated as a change tool";
export const CUSTOM_TRUST_NOTE =
  "Fly on the Wall hasn't checked this connector. Only add ones from people you trust.";
export const LOCAL_WARNING =
  "This runs a program on your computer with the same access you have. Only add one you trust.";
export const KEYCHAIN_NOTE = "Saved in your system keychain, never in Fly on the Wall's files";
export const SAVED_IN_KEYCHAIN = "Saved in your keychain";
export const MASK = "••••";

/** Errors are strings; text after a "\n---\n" line is program output, shown
 *  under "Show details". */
export function splitDetails(error: unknown): { message: string; details: string | null } {
  const text =
    typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
  const i = text.indexOf("\n---\n");
  if (i < 0) return { message: text, details: null };
  return { message: text.slice(0, i), details: text.slice(i + 5) };
}

/* ------------------------------------------------------------- permissions */

/** Left to right as the control shows them. */
export const PERMISSIONS: readonly ConnectorPermission[] = [
  "always_allow",
  "needs_approval",
  "blocked",
];

export const PERMISSION_LABELS: Record<ConnectorPermission, string> = {
  always_allow: "Always allow",
  needs_approval: "Needs approval",
  blocked: "Blocked",
};

const RANK: Record<ConnectorPermission, number> = {
  blocked: 0,
  needs_approval: 1,
  always_allow: 2,
};

/** True when `to` gives the AI more room than `from` (a native dialog asks). */
export const loosens = (from: ConnectorPermission, to: ConnectorPermission) =>
  RANK[to] > RANK[from];

/** A tool that is changed, new, missing or unusable can only be Blocked. */
export function allowedPermissions(tool: ConnectorTool): readonly ConnectorPermission[] {
  return tool.status === "ok" ? PERMISSIONS : ["blocked"];
}

export const needsReview = (t: ConnectorTool) => t.status === "changed" || t.status === "new";

/** Read-only tools, and tools that can change or delete (unlabelled ones
 *  arrive as kind "write", so they land here). */
export function groupTools(tools: ConnectorTool[]): {
  read: ConnectorTool[];
  write: ConnectorTool[];
} {
  return {
    read: tools.filter((t) => t.kind === "read"),
    write: tools.filter((t) => t.kind !== "read"),
  };
}

/** The group control's value: one permission, "mixed", or null (no tools). */
export function groupValue(tools: ConnectorTool[]): ConnectorPermission | "mixed" | null {
  if (tools.length === 0) return null;
  const first = tools[0].permission;
  return tools.every((t) => t.permission === first) ? first : "mixed";
}

export function groupValueLabel(v: ConnectorPermission | "mixed" | null): string {
  if (v === "mixed") return "Mixed";
  return v ? PERMISSION_LABELS[v] : "No tools";
}

/** Changes that set every tool in a group to `to`, skipping tools already
 *  there and tools that can only be Blocked. */
export function planGroupChange(
  tools: ConnectorTool[],
  to: ConnectorPermission,
): ConnectorPermissionChange[] {
  return tools
    .filter((t) => t.permission !== to && allowedPermissions(t).includes(to))
    .map((t) => ({ tool: t.name, permission: to }));
}

/** "Can change data" (+ "Destructive") marker text, never colour alone. */
export const kindLabel = (kind: "read" | "write") =>
  kind === "read" ? "Read-only" : "Can change data";

/** The chip for a tool that isn't usable as offered, if any. */
export function toolStatusChip(t: ConnectorTool): string | null {
  switch (t.status) {
    case "changed":
      return "Changed — review";
    case "new":
      return "New — review";
    case "missing":
      return "No longer offered";
    case "unusable":
      return t.problem ? `Can't be used: ${t.problem}` : "Can't be used";
    default:
      return null;
  }
}

/* ------------------------------------------------------------- connectors */

/** The list row / header status line. `problem` adds an icon (never colour
 *  alone). */
export function connectorStatus(c: Connector): { text: string; problem: boolean } {
  switch (c.status) {
    case "connected":
      return {
        text: c.kind === "web" ? `Connected · ${c.destination}` : "Connected",
        problem: false,
      };
    case "needs_sign_in":
      return { text: "Needs sign-in", problem: true };
    case "review":
      return {
        text: `${c.reviewCount} tool${c.reviewCount === 1 ? "" : "s"} changed — review`,
        problem: true,
      };
    case "could_not_start":
      return { text: "Couldn't start", problem: true };
    case "unreachable":
      return { text: `Couldn't reach ${c.destination}`, problem: true };
    case "not_checked":
      return { text: "Not checked yet", problem: false };
    case "off":
      return { text: "Off", problem: false };
    case "not_approved":
      return { text: "Not approved — edit it to approve again", problem: true };
    default:
      return { text: String(c.status), problem: false };
  }
}

/** The changed-tools banner. */
export function reviewBanner(n: number): string {
  return n === 1
    ? "1 tool changed since you approved it. It's blocked until you review."
    : `${n} tools changed since you approved them. They're blocked until you review.`;
}

const normUrl = (u: string) => u.trim().replace(/\/+$/, "").toLowerCase();

/** Two web addresses name the same server (case and a trailing / aside). */
export const sameAddress = (a: string, b: string) => normUrl(a) === normUrl(b);

/** Local / Custom badge; a connector from the directory has none. */
export function connectorBadge(
  c: Connector,
  directory: readonly ConnectorDirectoryEntry[],
): "Local" | "Custom" | null {
  if (c.kind === "local" || c.transport.kind === "stdio") return "Local";
  const url = normUrl(c.transport.url);
  return directory.some((d) => normUrl(d.url) === url) ? null : "Custom";
}

/** Directory entries whose name, maker or description has every word. */
export function filterDirectory(
  entries: readonly ConnectorDirectoryEntry[],
  query: string,
): ConnectorDirectoryEntry[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return [...entries];
  return entries.filter((e) => {
    const hay = `${e.name} ${e.vendor} ${e.description}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

/** "Found 12 tools: 4 read-only, 8 can make changes". */
export function probeSummary(p: ConnectorProbe): string {
  const n = p.tools.length;
  if (n === 0) {
    return p.needsSignIn
      ? "It asks you to sign in with your browser. You'll see its tools after you sign in."
      : "Found no tools.";
  }
  const read = p.tools.filter((t) => t.kind === "read").length;
  return `Found ${n} tool${n === 1 ? "" : "s"}: ${read} read-only, ${n - read} can make changes`;
}

/** A directory entry's configuration, as the Add form would send it. */
export function directoryInput(e: ConnectorDirectoryEntry, apiKey = ""): ConnectorInput {
  return {
    config: {
      name: e.name,
      transport: { kind: "http", url: e.url, headers: [] },
      auth:
        e.auth === "oauth"
          ? { kind: "oauth", scopes: [], clientId: null }
          : e.auth === "api_key"
            ? { kind: "api_key" }
            : { kind: "none" },
    },
    secrets: e.auth === "api_key" ? { apiKey: apiKey.trim() || null } : {},
  };
}

/* ------------------------------------------------------------- forms */

export const NAME_MAX = 40;
export const MAX_ARGS = 32;
const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;
const HEADER_NAME = /^[A-Za-z0-9!#$%&'*+.^_`|~-]+$/;

/** One header (web) or setting (local) row. */
export interface KVRow {
  id: string;
  name: string;
  value: string;
  secret: boolean;
  /** Editing: a secret value is already in the keychain ("" keeps it). */
  stored: boolean;
}

export type FormErrors = Record<string, string>;
export type SignInChoice = "" | "oauth" | "api_key" | "none";

export interface CustomDraft {
  name: string;
  url: string;
  /** Nothing is pre-selected. */
  signIn: SignInChoice;
  apiKey: string;
  headers: KVRow[];
  clientId: string;
  clientSecret: string;
}

export interface LocalDraft {
  name: string;
  program: "" | "npx" | "uvx" | "file";
  /** The chosen file, for program "file". */
  file: string;
  args: { id: string; value: string }[];
  env: KVRow[];
}

export const emptyCustomDraft = (): CustomDraft => ({
  name: "",
  url: "",
  signIn: "",
  apiKey: "",
  headers: [],
  clientId: "",
  clientSecret: "",
});

export const emptyLocalDraft = (): LocalDraft => ({
  name: "",
  program: "",
  file: "",
  args: [],
  env: [],
});

function checkName(name: string, errors: FormErrors) {
  if (!name) errors.name = "Give it a name.";
  else if ([...name].length > NAME_MAX)
    errors.name = `Keep the name to ${NAME_MAX} characters or fewer.`;
}

/** Validates key/value rows; fully empty rows are ignored. */
function checkRows(
  rows: KVRow[],
  nameRule: RegExp,
  nameError: string,
  errors: FormErrors,
  caseInsensitive: boolean,
): KVRow[] {
  const used = rows.filter((r) => r.name.trim() || r.value || r.stored);
  const seen = new Set<string>();
  for (const r of used) {
    const name = r.name.trim();
    const key = caseInsensitive ? name.toLowerCase() : name;
    if (!name) errors[`row:${r.id}`] = "Give it a name.";
    else if (!nameRule.test(name)) errors[`row:${r.id}`] = nameError;
    else if (seen.has(key)) errors[`row:${r.id}`] = "This name is already listed.";
    else if (r.secret && !r.value && !r.stored)
      errors[`row:${r.id}`] = "Enter a value. It's saved in your keychain.";
    seen.add(key);
  }
  return used;
}

function rowsToConfig(rows: KVRow[]): {
  kv: ConnectorKV[];
  values: Record<string, string>;
} {
  const kv = rows.map((r) => ({ name: r.name.trim(), value: r.secret ? null : r.value }));
  const values = Object.fromEntries(
    rows.filter((r) => r.secret && r.value).map((r) => [r.name.trim(), r.value]),
  );
  return { kv, values };
}

/** The Add custom (or Edit) form → ConnectorInput. `stored` says which
 *  secrets an edited connector already has ("" keeps them). */
export function validateCustom(
  d: CustomDraft,
  stored: { apiKeySet?: boolean; clientSecretSet?: boolean } = {},
): { input: ConnectorInput | null; errors: FormErrors } {
  const errors: FormErrors = {};
  const name = d.name.trim();
  checkName(name, errors);

  const url = d.url.trim();
  let parsed: URL | null = null;
  try {
    parsed = new URL(url);
  } catch {
    parsed = null;
  }
  if (!parsed || parsed.protocol !== "https:" || !parsed.hostname)
    errors.url = "Use an address that starts with https://";
  else if (parsed.username || parsed.password)
    errors.url = "Leave sign-in details out of the address.";

  if (!d.signIn) errors.signIn = "Choose how it signs in.";
  if (d.signIn === "api_key" && !d.apiKey.trim() && !stored.apiKeySet)
    errors.apiKey = "Enter the key or token.";

  const rows = checkRows(
    d.headers,
    HEADER_NAME,
    "Use letters, digits and - in a header name.",
    errors,
    true,
  );
  for (const r of rows) {
    if (
      !errors[`row:${r.id}`] &&
      d.signIn !== "none" &&
      r.name.trim().toLowerCase() === "authorization"
    )
      errors[`row:${r.id}`] = "Sign-in sets this header.";
  }

  const clientId = d.clientId.trim() || null;
  const clientSecret = d.clientSecret.trim();
  if (d.signIn === "oauth" && clientSecret && !clientId)
    errors.clientSecret = "A client secret needs a client ID.";

  if (Object.keys(errors).length > 0) return { input: null, errors };
  const { kv, values } = rowsToConfig(rows);
  return {
    input: {
      config: {
        name,
        transport: { kind: "http", url, headers: kv },
        auth:
          d.signIn === "oauth"
            ? { kind: "oauth", scopes: [], clientId }
            : d.signIn === "api_key"
              ? { kind: "api_key" }
              : { kind: "none" },
      },
      secrets: {
        apiKey: d.signIn === "api_key" ? d.apiKey.trim() || null : null,
        clientSecret: d.signIn === "oauth" && clientId ? clientSecret || null : null,
        values,
      },
    },
    errors,
  };
}

const programCommand = (d: LocalDraft) => (d.program === "file" ? d.file.trim() : d.program);

/** The Add local (or Edit) form → ConnectorInput. */
export function validateLocal(d: LocalDraft): {
  input: ConnectorInput | null;
  errors: FormErrors;
} {
  const errors: FormErrors = {};
  const name = d.name.trim();
  checkName(name, errors);
  if (!d.program) errors.program = "Choose the program.";
  else if (d.program === "file" && !d.file.trim()) errors.program = "Choose a file.";
  const args = d.args.map((a) => a.value).filter((v) => v !== "");
  if (args.length > MAX_ARGS) errors.args = `Use ${MAX_ARGS} arguments or fewer.`;
  const rows = checkRows(
    d.env,
    ENV_NAME,
    "Use letters, digits and _, not starting with a digit.",
    errors,
    false,
  );
  if (Object.keys(errors).length > 0) return { input: null, errors };
  const { kv, values } = rowsToConfig(rows);
  return {
    input: {
      config: {
        name,
        transport: { kind: "stdio", command: programCommand(d), args, env: kv },
        auth: { kind: "none" },
      },
      secrets: { values },
    },
    errors,
  };
}

/** One argument as it would be typed (quoted when it has spaces). */
export function quoteArg(a: string): string {
  return a === "" || /[\s"]/.test(a) ? `"${a.replace(/"/g, '\\"')}"` : a;
}

/** "Will run: …" for the Add local form; secret values show as ••••. */
export function willRunPreview(d: LocalDraft): { command: string; settings: string[] } {
  const cmd = programCommand(d);
  const args = d.args.map((a) => a.value).filter((v) => v !== "");
  return {
    command: [cmd ? quoteArg(cmd) : "…", ...args.map(quoteArg)].join(" "),
    settings: d.env
      .filter((r) => r.name.trim())
      .map((r) => `${r.name.trim()}=${r.secret ? MASK : r.value}`),
  };
}

/** Splits text on whitespace; "double quotes" keep spaces together (no
 *  escapes, so Windows paths with backslashes stay as typed). */
export function splitArgs(input: string): { args: string[] } | { error: string } {
  const args: string[] = [];
  let cur = "";
  let started = false; // `""` is an empty argument, not nothing
  let quoted = false;
  for (const ch of input) {
    if (quoted) {
      if (ch === '"') quoted = false;
      else cur += ch;
    } else if (ch === '"') {
      quoted = true;
      started = true;
    } else if (/\s/.test(ch)) {
      if (started) args.push(cur);
      cur = "";
      started = false;
    } else {
      cur += ch;
      started = true;
    }
  }
  if (quoted) return { error: "A quote in the command isn't closed." };
  if (started) args.push(cur);
  return { args };
}

/** "Paste a command": fills the rows. It never runs anything. Leading
 *  NAME=value words become settings (marked secret). */
export function parseCommand(
  text: string,
  newId: () => string,
):
  | { program: LocalDraft["program"]; file: string; args: LocalDraft["args"]; env: KVRow[] }
  | { error: string } {
  const split = splitArgs(text.trim());
  if ("error" in split) return split;
  const tokens = split.args;
  const env: KVRow[] = [];
  let i = 0;
  while (i < tokens.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(tokens[i])) {
    const eq = tokens[i].indexOf("=");
    env.push({
      id: newId(),
      name: tokens[i].slice(0, eq),
      value: tokens[i].slice(eq + 1),
      secret: true,
      stored: false,
    });
    i++;
  }
  if (i >= tokens.length) return { error: "Paste a command, like npx -y @example/server." };
  const prog = tokens[i];
  const base = (prog.split(/[\\/]/).pop() ?? prog).toLowerCase().replace(/\.(cmd|exe)$/, "");
  const program = base === "npx" ? "npx" : base === "uvx" ? "uvx" : "file";
  return {
    program,
    file: program === "file" ? prog : "",
    args: tokens.slice(i + 1).map((value) => ({ id: newId(), value })),
    env,
  };
}

function rowsFrom(kv: ConnectorKV[], c: Connector, newId: () => string): KVRow[] {
  return kv.map((h) => ({
    id: newId(),
    name: h.name,
    value: h.value ?? "",
    secret: h.value === null,
    stored: h.value === null && !!c.secrets.values.find((v) => v.name === h.name)?.set,
  }));
}

/** Edit: a web connector's form, secrets left empty (they stay as stored). */
export function customDraftFrom(c: Connector, newId: () => string): CustomDraft {
  const t = c.transport.kind === "http" ? c.transport : null;
  return {
    name: c.name,
    url: t?.url ?? "",
    signIn: c.auth.kind,
    apiKey: "",
    headers: t ? rowsFrom(t.headers, c, newId) : [],
    clientId: c.auth.kind === "oauth" ? (c.auth.clientId ?? "") : "",
    clientSecret: "",
  };
}

/** Edit: a local connector's form. */
export function localDraftFrom(c: Connector, newId: () => string): LocalDraft {
  const t = c.transport.kind === "stdio" ? c.transport : null;
  const cmd = t?.command ?? "";
  const program = cmd === "npx" || cmd === "uvx" ? cmd : cmd ? "file" : "";
  return {
    name: c.name,
    program,
    file: program === "file" ? cmd : "",
    args: (t?.args ?? []).map((value) => ({ id: newId(), value })),
    env: t ? rowsFrom(t.env, c, newId) : [],
  };
}

/** What an import candidate will run or reach. */
export function candidateSummary(c: ConnectorImportCandidate): string {
  if (c.kind === "web") return c.url ?? "";
  return [c.command ?? "", ...c.args.map(quoteArg)].join(" ").trim();
}

/* ------------------------------------------------------------- call cards */

export const prettyArgs = (args: unknown): string => JSON.stringify(args ?? {}, null, 2);

/** Parses edited arguments: they must be one JSON object. */
export function parseArgs(
  text: string,
): { ok: true; value: Record<string, unknown> } | { ok: false; error: string } {
  if (!text.trim()) return { ok: false, error: "Details must be a JSON object, like {}." };
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (e) {
    const why = e instanceof Error ? e.message : String(e);
    return { ok: false, error: `That isn't valid JSON: ${why}` };
  }
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return { ok: false, error: "Details must be a JSON object ({ … }), not a list or a value." };
  }
  return { ok: true, value: value as Record<string, unknown> };
}

/** Same arguments as proposed (so the call is sent with `args: null`). */
export function sameArgs(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

const isRecord = (v: unknown): v is Record<string, unknown> =>
  v !== null && typeof v === "object" && !Array.isArray(v);
const own = (o: object, k: string) => Object.prototype.hasOwnProperty.call(o, k);

/* ------------------------------------------------------------- try a tool */

/** "Try…" is offered for a tool that is usable and not Blocked. */
export const canTry = (t: ConnectorTool) => t.status === "ok" && t.permission !== "blocked";

function emptyFor(prop: unknown): unknown {
  if (!isRecord(prop)) return null;
  let type = prop.type;
  if (Array.isArray(type)) type = type.find((t) => t !== "null") ?? "null";
  switch (type) {
    case "string":
      return "";
    case "number":
    case "integer":
      return 0;
    case "boolean":
      return false;
    case "array":
      return [];
    case "object":
      return {};
    default:
      return null;
  }
}

/** The Try tester's starting point: the schema's required properties, each
 *  with an empty value of its type. Server defaults are ignored. */
export function argsTemplate(schema: unknown): Record<string, unknown> {
  if (!isRecord(schema) || !isRecord(schema.properties)) return {};
  const props = schema.properties;
  const required = Array.isArray(schema.required)
    ? schema.required.filter((r): r is string => typeof r === "string")
    : [];
  // fromEntries defines own properties, so a "__proto__" key stays a key.
  return Object.fromEntries(
    [...new Set(required)]
      .filter((name) => own(props, name))
      .map((name) => [name, emptyFor(props[name])]),
  );
}

export type FieldKind = "string" | "number" | "integer" | "boolean";

export interface FieldSpec {
  name: string;
  label: string;
  kind: FieldKind;
  required: boolean;
  description: string | null;
  /** A string enum: a dropdown. */
  options: string[] | null;
}

export type FieldValues = ReadonlyMap<string, string | boolean>;

/** Editable fields for a schema whose properties are all simple (string,
 *  number, integer, boolean), or null when the JSON editor is needed. */
export function schemaFields(schema: unknown, args: Record<string, unknown>): FieldSpec[] | null {
  const props = isRecord(schema) && isRecord(schema.properties) ? schema.properties : null;
  if (!props) return Object.keys(args).length === 0 ? [] : null;
  const required = new Set(
    isRecord(schema) && Array.isArray(schema.required)
      ? schema.required.filter((r): r is string => typeof r === "string")
      : [],
  );
  const specs: FieldSpec[] = [];
  for (const name of Object.keys(props)) {
    const p = props[name];
    if (!isRecord(p)) return null;
    let type = p.type;
    if (Array.isArray(type)) {
      const real = type.filter((t) => t !== "null");
      type = real.length === 1 ? real[0] : undefined;
    }
    if (type !== "string" && type !== "number" && type !== "integer" && type !== "boolean")
      return null;
    const options =
      type === "string" && Array.isArray(p.enum) && p.enum.every((o) => typeof o === "string")
        ? (p.enum as string[])
        : null;
    specs.push({
      name,
      label: typeof p.title === "string" && p.title.trim() ? p.title.trim() : name,
      kind: type,
      required: required.has(name),
      description:
        typeof p.description === "string" && p.description.trim()
          ? p.description.trim().slice(0, 300)
          : null,
      options,
    });
  }
  // Every argument must fit a field of its type.
  for (const [k, v] of Object.entries(args)) {
    const spec = specs.find((s) => s.name === k);
    if (!spec || v === null) return null;
    const fits =
      spec.kind === "string"
        ? typeof v === "string"
        : spec.kind === "boolean"
          ? typeof v === "boolean"
          : typeof v === "number" && (spec.kind === "number" || Number.isInteger(v));
    if (!fits) return null;
  }
  return specs;
}

export function fieldValuesFrom(
  specs: FieldSpec[],
  args: Record<string, unknown>,
): Map<string, string | boolean> {
  const values = new Map<string, string | boolean>();
  for (const s of specs) {
    const v = own(args, s.name) ? args[s.name] : undefined;
    values.set(s.name, s.kind === "boolean" ? v === true : v === undefined ? "" : String(v));
  }
  return values;
}

/** The arguments the fields describe. An empty optional field is left out. */
export function argsFromFields(
  specs: FieldSpec[],
  values: FieldValues,
  original: Record<string, unknown>,
): { ok: true; value: Record<string, unknown> } | { ok: false; error: string } {
  const out: [string, unknown][] = [];
  for (const s of specs) {
    const v = values.get(s.name);
    if (s.kind === "boolean") {
      if (v === true || s.required || own(original, s.name)) out.push([s.name, v === true]);
      continue;
    }
    const text = typeof v === "string" ? v : "";
    if (s.kind === "string") {
      if (text !== "" || s.required || own(original, s.name)) out.push([s.name, text]);
      continue;
    }
    if (text.trim() === "") {
      if (s.required) return { ok: false, error: `Enter a number for “${s.label}”.` };
      continue;
    }
    const n = Number(text);
    if (!Number.isFinite(n)) return { ok: false, error: `“${s.label}” must be a number.` };
    if (s.kind === "integer" && !Number.isInteger(n))
      return { ok: false, error: `“${s.label}” must be a whole number.` };
    out.push([s.name, n]);
  }
  // fromEntries defines own properties, so a "__proto__" key stays a key.
  return { ok: true, value: Object.fromEntries(out) };
}

/** "2:14 PM" (newer ICU puts a narrow no-break space before PM; a plain
 *  space reads the same and keeps the text predictable). */
const clock = (ms: number) =>
  new Date(ms)
    .toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" })
    .replace(/\s/g, " ");

/** What a decided card collapses to. */
export function decidedLabel(action: ConnectorCardAction, atMs: number): string {
  switch (action) {
    case "allow_once":
      return `Allowed once · ${clock(atMs)}`;
    case "always_allow":
      return `Always allowed · ${clock(atMs)}`;
    default:
      return "Denied";
  }
}

/** Adds `incoming` proposals not already shown and not already settled,
 *  keeping arrival order. */
export function mergeProposals(
  current: ConnectorCallProposal[],
  incoming: ConnectorCallProposal[],
  settled: ReadonlySet<string>,
): ConnectorCallProposal[] {
  const seen = new Set(current.map((p) => p.proposalId));
  const added = incoming.filter((p) => {
    if (seen.has(p.proposalId) || settled.has(p.proposalId)) return false;
    seen.add(p.proposalId);
    return true;
  });
  return added.length > 0 ? [...current, ...added] : current;
}

/* ------------------------------------------------------------- activity */

const REFUSALS: Record<ConnectorRefusal, string> = {
  not_offered: "the tool is blocked",
  keep_local: "the note is kept local",
  after_result: "not allowed after another tool's result",
  budget: "too many calls in one answer",
  invalid_arguments: "the details didn't fit the tool",
  tool_changed: "the tool changed since you approved it",
  not_approved: "the connector isn't approved",
  timed_out: "no answer in time",
};

export function refusalLabel(code: ConnectorRefusal | null): string | null {
  return code && own(REFUSALS, code) ? REFUSALS[code] : null;
}

/** Allowed by you / Allowed automatically / Denied / Blocked / Failed. */
export function activityResult(e: Pick<ConnectorCallLogEntry, "outcome" | "approval">): string {
  switch (e.outcome) {
    case "ran":
      return e.approval === "automatic"
        ? "Allowed automatically"
        : e.approval === "user"
          ? "Allowed by you"
          : "Allowed";
    case "declined":
      return "Denied";
    case "refused":
      return "Blocked";
    default:
      return "Failed";
  }
}

/** The reason under a result, if there is one. */
export function activityDetail(
  e: Pick<ConnectorCallLogEntry, "outcome" | "refusal">,
): string | null {
  if (e.outcome === "refused") return refusalLabel(e.refusal);
  if (e.outcome === "server_error") return "the connector returned an error";
  if (e.outcome === "failed") return "couldn't reach the connector";
  return null;
}

export const isProblem = (e: Pick<ConnectorCallLogEntry, "outcome">) =>
  e.outcome === "refused" || e.outcome === "failed" || e.outcome === "server_error";

/** "2:14 PM" today, else "Oct 27, 2:14 PM". */
export function formatTime(unixSeconds: number, nowMs: number): string {
  const d = new Date(unixSeconds * 1000);
  const sameDay = d.toDateString() === new Date(nowMs).toDateString();
  return sameDay
    ? clock(d.getTime())
    : `${d.toLocaleDateString("en-US", { month: "short", day: "numeric" })}, ${clock(d.getTime())}`;
}

/** "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago", else a date. */
export function relativeTime(unixSeconds: number, nowMs: number): string {
  const secs = Math.max(0, Math.round(nowMs / 1000 - unixSeconds));
  if (secs < 45) return "just now";
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.round(hours / 24);
  if (days === 1) return "yesterday";
  if (days < 7) return `${days} days ago`;
  return new Date(unixSeconds * 1000).toLocaleDateString("en-US", {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}
