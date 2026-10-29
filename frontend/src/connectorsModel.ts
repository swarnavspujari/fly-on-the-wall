// Pure logic behind Settings › Connectors (beta) and the connector-call
// confirm card: the add form → ConnectorConfigInput, the per-tool permission
// draft, args-JSON editing, the Try tester's args template, and call-log
// labels. Kept free of React so it is unit-tested.
//
// Everything a server sends (tool names, descriptions, schemas, args) is
// untrusted: nothing here interprets it beyond reading plain values, and
// lookups keyed by a tool name use Maps / own-property checks, never `obj[name]`.

import type {
  ConnectorAuth,
  ConnectorCallLogEntry,
  ConnectorCallProposal,
  ConnectorConfigInput,
  ConnectorRefusal,
  ConnectorTool,
  ConnectorToolPermission,
} from "./types";

/** The beta flag: the only setting the UI writes for connectors ("1" / "0"). */
export const CONNECTORS_BETA_KEY = "ui.beta.connectors";

/** Shown wherever a change will be confirmed by the backend's native dialog. */
export const SYSTEM_DIALOG_NOTE = "You'll confirm in a system dialog.";

/* ------------------------------------------------------------- add form */

export const NAME_MAX = 40;
const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

export interface AddDraft {
  name: string;
  /** "http" = Web address, "stdio" = Local program. */
  kind: "http" | "stdio";
  url: string;
  command: string;
  /** Program arguments as typed: whitespace-separated, "double quotes" group. */
  args: string;
  /** Environment variable names, separated by spaces or commas. */
  envNames: string;
  /** Web servers only; a local program never signs in. */
  auth: "none" | "oauth";
  scopes: string;
  clientId: string;
  clientSecret: string;
}

export type AddField = "name" | "url" | "command" | "args" | "envNames" | "clientSecret";
export type AddErrors = Partial<Record<AddField, string>>;

export function emptyAddDraft(): AddDraft {
  return {
    name: "",
    kind: "http",
    url: "",
    command: "",
    args: "",
    envNames: "",
    auth: "none",
    scopes: "",
    clientId: "",
    clientSecret: "",
  };
}

/** Splits program arguments on whitespace; "double quotes" keep spaces
 *  together (no escapes, so Windows paths with backslashes stay as typed). */
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
  if (quoted) return { error: "A quote in the arguments isn't closed." };
  if (started) args.push(cur);
  return { args };
}

/** Words separated by spaces and/or commas, without repeats. */
export function splitList(input: string): string[] {
  return [...new Set(input.split(/[\s,]+/).filter(Boolean))];
}

/** Checks the add form. `config` is null whenever `errors` has an entry. */
export function validateAddDraft(d: AddDraft): {
  config: ConnectorConfigInput | null;
  errors: AddErrors;
} {
  const errors: AddErrors = {};
  const name = d.name.trim();
  if (!name) errors.name = "Give it a name.";
  else if ([...name].length > NAME_MAX)
    errors.name = `Keep the name to ${NAME_MAX} characters or fewer.`;

  let transport: ConnectorConfigInput["transport"] | null = null;
  let auth: ConnectorAuth = { kind: "none" };
  let clientSecret: string | null = null;

  if (d.kind === "http") {
    const url = d.url.trim();
    let parsed: URL | null = null;
    if (!url) errors.url = "Enter the server's web address.";
    else {
      try {
        parsed = new URL(url);
      } catch {
        errors.url = "That isn't a web address. It starts with https://";
      }
    }
    if (parsed) {
      if (parsed.protocol !== "https:")
        errors.url = "Use an https:// address. Connectors only talk to web servers over HTTPS.";
      else if (!parsed.hostname) errors.url = "That address has no server name.";
      else if (parsed.username || parsed.password)
        errors.url = "Leave sign-in details out of the address.";
      else transport = { kind: "http", url };
    }
    if (d.auth === "oauth") {
      const clientId = d.clientId.trim() || null;
      const secret = d.clientSecret.trim();
      if (secret && !clientId) errors.clientSecret = "A client secret needs a client ID.";
      else if (secret) clientSecret = secret;
      auth = { kind: "oauth", scopes: splitList(d.scopes), clientId };
    }
  } else {
    const command = d.command.trim();
    if (!command) errors.command = "Enter the program to run.";
    const split = splitArgs(d.args);
    if ("error" in split) errors.args = split.error;
    const envNames = splitList(d.envNames);
    const bad = envNames.filter((n) => !ENV_NAME.test(n));
    if (bad.length > 0)
      errors.envNames = `Not a valid variable name: ${bad.join(", ")}. Use letters, digits and _, not starting with a digit.`;
    if (command && "args" in split && bad.length === 0)
      transport = { kind: "stdio", command, args: split.args, envNames };
  }

  if (Object.keys(errors).length > 0 || !transport) return { config: null, errors };
  return { config: { name, transport, auth, clientSecret }, errors };
}

/* ------------------------------------------------------------- tool permissions */

/** Unsaved per-tool edits, keyed by tool name (a Map: names come from the
 *  server, so they never index a plain object). */
export type ToolDraft = ReadonlyMap<string, { enabled: boolean; readOnly: boolean }>;

/** A tool's value in the draft, else what's saved. */
export function draftValue(tool: ConnectorTool, draft: ToolDraft) {
  return draft.get(tool.name) ?? { enabled: tool.enabled, readOnly: tool.readOnly };
}

/** The save_connector_tools payload: only tools whose permissions changed. A
 *  read-only mark the server forbids (canMarkReadOnly false) is never sent. */
export function toolPermissionChanges(
  tools: ConnectorTool[],
  draft: ToolDraft,
): ConnectorToolPermission[] {
  const out: ConnectorToolPermission[] = [];
  for (const t of tools) {
    const d = draft.get(t.name);
    if (!d) continue;
    const readOnly = t.canMarkReadOnly ? d.readOnly : t.readOnly;
    if (d.enabled !== t.enabled || readOnly !== t.readOnly) {
      out.push({ tool: t.name, enabled: d.enabled, readOnly });
    }
  }
  return out;
}

/** True when saving `changes` grants trust (turns a tool on, or marks one
 *  read-only), so the backend will ask in a native dialog first. */
export function grantsTrust(tools: ConnectorTool[], changes: ConnectorToolPermission[]): boolean {
  return changes.some((c) => {
    const t = tools.find((x) => x.name === c.tool);
    if (!t) return true; // unknown tool: assume the backend asks
    return (c.enabled && !t.enabled) || (c.readOnly && !t.readOnly);
  });
}

/** The kind a tool will have with this read-only mark: "read" only when it's
 *  marked read-only and the server doesn't say it writes. */
export function effectiveKind(tool: ConnectorTool, readOnly: boolean): "read" | "write" {
  return readOnly && tool.canMarkReadOnly ? "read" : "write";
}

export const kindLabel = (kind: "read" | "write") => (kind === "read" ? "Read" : "May change data");

/** The status badge for a tool. */
export function toolStatusLabel(tool: ConnectorTool): {
  label: string;
  tone: "success" | "warning" | "danger" | "neutral";
} {
  switch (tool.status) {
    case "ok":
      return { label: "OK", tone: "success" };
    case "changed":
      return { label: "Changed — review", tone: "warning" };
    case "missing":
      return { label: "No longer offered", tone: "neutral" };
    case "unusable":
      return {
        label: tool.problem ? `Can't be used: ${tool.problem}` : "Can't be used",
        tone: "danger",
      };
  }
}

/* ------------------------------------------------------------- args JSON */

export const prettyArgs = (args: unknown): string => JSON.stringify(args ?? {}, null, 2);

/** Parses edited arguments: they must be one JSON object. */
export function parseArgs(
  text: string,
): { ok: true; value: Record<string, unknown> } | { ok: false; error: string } {
  if (!text.trim()) return { ok: false, error: "Arguments must be a JSON object, like {}." };
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (e) {
    const why = e instanceof Error ? e.message : String(e);
    return { ok: false, error: `That isn't valid JSON: ${why}` };
  }
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return { ok: false, error: "Arguments must be a JSON object ({ … }), not a list or a value." };
  }
  return { ok: true, value: value as Record<string, unknown> };
}

/** Same arguments as proposed (so the call is sent with `args: null`). */
export function sameArgs(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

const isRecord = (v: unknown): v is Record<string, unknown> =>
  v !== null && typeof v === "object" && !Array.isArray(v);

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

/** A starting point for the Try tester: the schema's required properties,
 *  each with an empty value of its type. Server defaults are ignored. */
export function argsTemplate(schema: unknown): Record<string, unknown> {
  if (!isRecord(schema) || !isRecord(schema.properties)) return {};
  const props = schema.properties;
  const required = Array.isArray(schema.required)
    ? schema.required.filter((r): r is string => typeof r === "string")
    : [];
  // fromEntries defines own properties, so a "__proto__" key stays a key.
  return Object.fromEntries(
    [...new Set(required)]
      .filter((name) => Object.prototype.hasOwnProperty.call(props, name))
      .map((name) => [name, emptyFor(props[name])]),
  );
}

/* ------------------------------------------------------------- proposals */

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

/* ------------------------------------------------------------- call log */

const REFUSALS: Record<ConnectorRefusal, string> = {
  not_offered: "the tool isn't turned on",
  keep_local: "the note is kept local",
  after_result: "not allowed after another tool's result",
  budget: "too many calls in one answer",
  invalid_arguments: "the arguments didn't fit the tool",
  tool_changed: "the tool changed since you approved it",
  not_approved: "the connector isn't approved",
  timed_out: "no answer in time",
};

export function refusalLabel(code: ConnectorRefusal | null): string {
  return (
    (code && Object.prototype.hasOwnProperty.call(REFUSALS, code) && REFUSALS[code]) || "refused"
  );
}

export function outcomeLabel(e: Pick<ConnectorCallLogEntry, "outcome" | "refusal">): string {
  switch (e.outcome) {
    case "ran":
      return "Ran";
    case "server_error":
      return "Server error";
    case "declined":
      return "You didn't run it";
    case "refused":
      return e.refusal ? `Refused: ${refusalLabel(e.refusal)}` : "Refused";
    case "failed":
      return "Failed";
    default:
      return String(e.outcome);
  }
}

/** Newest first (the backend keeps the log oldest first). */
export const newestFirst = <T>(log: readonly T[]): T[] => [...log].reverse();

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
  return `${Math.round(ms / 60_000)} min`;
}

export function formatChars(n: number, truncated: boolean): string {
  const base = `${n.toLocaleString("en-US")} char${n === 1 ? "" : "s"}`;
  return truncated ? `${base}, cut` : base;
}

/** "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago", else a date. */
export function relativeTime(unixSeconds: number, nowMs: number = Date.now()): string {
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
