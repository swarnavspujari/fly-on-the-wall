import { describe, expect, it } from "vitest";
import {
  argsTemplate,
  effectiveKind,
  emptyAddDraft,
  formatChars,
  formatDuration,
  grantsTrust,
  kindLabel,
  mergeProposals,
  newestFirst,
  outcomeLabel,
  parseArgs,
  prettyArgs,
  refusalLabel,
  relativeTime,
  sameArgs,
  splitArgs,
  splitList,
  toolPermissionChanges,
  toolStatusLabel,
  validateAddDraft,
  type AddDraft,
  type ToolDraft,
} from "./connectorsModel";
import type { ConnectorCallProposal, ConnectorTool } from "./types";

const tool = (t: Partial<ConnectorTool> & { name: string }): ConnectorTool => ({
  title: null,
  description: "",
  enabled: false,
  readOnly: false,
  kind: "write",
  canMarkReadOnly: true,
  destructive: false,
  status: "ok",
  problem: null,
  pinnedHash: "a1b2c3d4e5f6",
  pendingHash: null,
  diff: [],
  inputSchema: {},
  ...t,
});

const draft = (d: Partial<AddDraft>): AddDraft => ({ ...emptyAddDraft(), ...d });

describe("add form", () => {
  it("builds a web connector, https only", () => {
    const { config, errors } = validateAddDraft(
      draft({ name: "  Acme CRM ", url: " https://mcp.acme-crm.example/mcp " }),
    );
    expect(errors).toEqual({});
    expect(config).toEqual({
      name: "Acme CRM",
      transport: { kind: "http", url: "https://mcp.acme-crm.example/mcp" },
      auth: { kind: "none" },
      clientSecret: null,
    });
    expect(validateAddDraft(draft({ name: "A", url: "http://mcp.example.com" })).errors.url).toBe(
      "Use an https:// address. Connectors only talk to web servers over HTTPS.",
    );
    expect(validateAddDraft(draft({ name: "A", url: "mcp.example.com" })).errors.url).toContain(
      "isn't a web address",
    );
    expect(validateAddDraft(draft({ name: "A", url: "" })).errors.url).toBe(
      "Enter the server's web address.",
    );
    expect(
      validateAddDraft(draft({ name: "A", url: "https://dana:pw@mcp.example.com" })).errors.url,
    ).toBe("Leave sign-in details out of the address.");
  });

  it("checks the name is 1 to 40 characters", () => {
    const url = "https://mcp.example.com";
    expect(validateAddDraft(draft({ name: "   ", url })).errors.name).toBe("Give it a name.");
    expect(validateAddDraft(draft({ name: "x".repeat(40), url })).config).not.toBeNull();
    const long = validateAddDraft(draft({ name: "x".repeat(41), url }));
    expect(long.config).toBeNull();
    expect(long.errors.name).toBe("Keep the name to 40 characters or fewer.");
  });

  it("OAuth: scopes split on spaces and commas; a secret needs a client ID", () => {
    const base = draft({ name: "Docs", url: "https://docs.example.com/mcp", auth: "oauth" });
    expect(validateAddDraft({ ...base, scopes: "read, write  read" }).config?.auth).toEqual({
      kind: "oauth",
      scopes: ["read", "write"],
      clientId: null,
    });
    const orphan = validateAddDraft({ ...base, clientSecret: "s3cret" });
    expect(orphan.config).toBeNull();
    expect(orphan.errors.clientSecret).toBe("A client secret needs a client ID.");
    const full = validateAddDraft({ ...base, clientId: " app-123 ", clientSecret: "s3cret" });
    expect(full.config?.auth).toEqual({ kind: "oauth", scopes: [], clientId: "app-123" });
    expect(full.config?.clientSecret).toBe("s3cret");
    // a secret typed while sign-in is None is never sent
    expect(
      validateAddDraft({ ...base, auth: "none", clientId: "x", clientSecret: "s3cret" }).config
        ?.clientSecret,
    ).toBeNull();
  });

  it("builds a local program with arguments and variable names", () => {
    const { config, errors } = validateAddDraft(
      draft({
        name: "Local docs",
        kind: "stdio",
        command: " docs-mcp ",
        args: '--root "My Notes" -v ""',
        envNames: "DOCS_TOKEN, _HOME2 DOCS_TOKEN",
      }),
    );
    expect(errors).toEqual({});
    expect(config).toEqual({
      name: "Local docs",
      transport: {
        kind: "stdio",
        command: "docs-mcp",
        args: ["--root", "My Notes", "-v", ""],
        envNames: ["DOCS_TOKEN", "_HOME2"],
      },
      auth: { kind: "none" },
      clientSecret: null,
    });
  });

  it("explains a bad local program", () => {
    const bad = validateAddDraft(
      draft({ name: "X", kind: "stdio", command: "", args: '"open', envNames: "2BAD ok-not" }),
    );
    expect(bad.config).toBeNull();
    expect(bad.errors.command).toBe("Enter the program to run.");
    expect(bad.errors.args).toBe("A quote in the arguments isn't closed.");
    expect(bad.errors.envNames).toContain("Not a valid variable name: 2BAD, ok-not");
  });
});

describe("splitting", () => {
  it("splits arguments on whitespace, keeping double-quoted spaces", () => {
    expect(splitArgs("")).toEqual({ args: [] });
    expect(splitArgs("  a   b\tc ")).toEqual({ args: ["a", "b", "c"] });
    expect(splitArgs('--path "C:\\Program Files\\x" --flag')).toEqual({
      args: ["--path", "C:\\Program Files\\x", "--flag"],
    });
    expect(splitArgs('pre"fix part"post')).toEqual({ args: ["prefix partpost"] });
    expect(splitArgs('a ""')).toEqual({ args: ["a", ""] });
    expect(splitArgs('"unclosed')).toEqual({ error: "A quote in the arguments isn't closed." });
  });

  it("splits lists on spaces and commas without repeats", () => {
    expect(splitList(" a,b ,, c a ")).toEqual(["a", "b", "c"]);
    expect(splitList("")).toEqual([]);
  });
});

describe("tool permissions", () => {
  const tools = [
    tool({ name: "search", enabled: true, readOnly: true, kind: "read" }),
    tool({ name: "update", enabled: false, canMarkReadOnly: false }),
    tool({ name: "notes", enabled: true }),
  ];

  it("sends only tools whose permissions changed", () => {
    const d: ToolDraft = new Map([
      ["search", { enabled: true, readOnly: true }], // unchanged
      ["update", { enabled: true, readOnly: true }], // read-only not allowed: dropped
      ["notes", { enabled: false, readOnly: false }],
      ["gone", { enabled: true, readOnly: true }], // no such tool
    ]);
    expect(toolPermissionChanges(tools, d)).toEqual([
      { tool: "update", enabled: true, readOnly: false },
      { tool: "notes", enabled: false, readOnly: false },
    ]);
    expect(toolPermissionChanges(tools, new Map())).toEqual([]);
  });

  it("tool names like __proto__ are just names", () => {
    const odd = [tool({ name: "__proto__" }), tool({ name: "constructor" })];
    expect(toolPermissionChanges(odd, new Map())).toEqual([]);
    expect(
      toolPermissionChanges(odd, new Map([["constructor", { enabled: true, readOnly: false }]])),
    ).toEqual([{ tool: "constructor", enabled: true, readOnly: false }]);
  });

  it("enabling or marking read-only grants trust; turning off doesn't", () => {
    expect(grantsTrust(tools, [{ tool: "update", enabled: true, readOnly: false }])).toBe(true);
    expect(grantsTrust(tools, [{ tool: "notes", enabled: true, readOnly: true }])).toBe(true);
    expect(grantsTrust(tools, [{ tool: "notes", enabled: false, readOnly: false }])).toBe(false);
    expect(grantsTrust(tools, [{ tool: "search", enabled: true, readOnly: false }])).toBe(false);
    expect(grantsTrust(tools, [])).toBe(false);
  });

  it("is read only when marked read-only and the server allows it", () => {
    expect(effectiveKind(tools[0], true)).toBe("read");
    expect(effectiveKind(tools[0], false)).toBe("write");
    expect(effectiveKind(tools[1], true)).toBe("write");
    expect(kindLabel("read")).toBe("Read");
    expect(kindLabel("write")).toBe("May change data");
  });

  it("labels each status", () => {
    expect(toolStatusLabel(tool({ name: "a" })).label).toBe("OK");
    expect(toolStatusLabel(tool({ name: "a", status: "changed" })).label).toBe("Changed — review");
    expect(toolStatusLabel(tool({ name: "a", status: "missing" })).label).toBe("No longer offered");
    expect(toolStatusLabel(tool({ name: "a", status: "unusable", problem: "bad schema" }))).toEqual(
      { label: "Can't be used: bad schema", tone: "danger" },
    );
  });
});

describe("args JSON", () => {
  it("pretty prints and parses one JSON object", () => {
    expect(prettyArgs({ a: 1 })).toBe('{\n  "a": 1\n}');
    expect(prettyArgs(undefined)).toBe("{}");
    expect(parseArgs(' { "q": "acme" } ')).toEqual({ ok: true, value: { q: "acme" } });
    expect(parseArgs("{}")).toEqual({ ok: true, value: {} });
  });

  it("explains what's wrong", () => {
    const bad = parseArgs("{ q: 1 }");
    expect(bad.ok).toBe(false);
    if (!bad.ok) expect(bad.error).toMatch(/^That isn't valid JSON: /);
    for (const text of ["[1, 2]", "null", "3", '"x"']) {
      expect(parseArgs(text)).toEqual({
        ok: false,
        error: "Arguments must be a JSON object ({ … }), not a list or a value.",
      });
    }
    expect(parseArgs("  ")).toEqual({
      ok: false,
      error: "Arguments must be a JSON object, like {}.",
    });
  });

  it("compares arguments", () => {
    expect(sameArgs({ a: [1, { b: 2 }] }, { a: [1, { b: 2 }] })).toBe(true);
    expect(sameArgs({ a: 1 }, { a: 2 })).toBe(false);
  });

  it("templates the required properties with empty values", () => {
    expect(
      argsTemplate({
        type: "object",
        properties: {
          query: { type: "string", default: "ignored" },
          limit: { type: "integer" },
          score: { type: "number" },
          exact: { type: "boolean" },
          tags: { type: "array" },
          filter: { type: "object" },
          maybe: { type: ["null", "string"] },
          anything: {},
          optional: { type: "string" },
        },
        required: [
          "query",
          "limit",
          "score",
          "exact",
          "tags",
          "filter",
          "maybe",
          "anything",
          "notInProperties",
        ],
      }),
    ).toEqual({
      query: "",
      limit: 0,
      score: 0,
      exact: false,
      tags: [],
      filter: {},
      maybe: "",
      anything: null,
    });
  });

  it("templates nothing from a missing or odd schema", () => {
    expect(argsTemplate(null)).toEqual({});
    expect(argsTemplate("x")).toEqual({});
    expect(argsTemplate({ type: "object" })).toEqual({});
    expect(argsTemplate({ properties: { a: { type: "string" } }, required: "a" })).toEqual({});
  });

  it("keeps a __proto__ property as a plain key", () => {
    const t = argsTemplate({
      properties: JSON.parse('{"__proto__": {"type": "string"}}'),
      required: ["__proto__"],
    });
    expect(Object.keys(t)).toEqual(["__proto__"]);
    expect(Object.getPrototypeOf(t)).toBe(Object.prototype);
    expect(JSON.stringify(t)).toBe('{"__proto__":""}');
  });
});

describe("proposals", () => {
  const p = (proposalId: string): ConnectorCallProposal => ({
    proposalId,
    turnId: "t1",
    serverId: "c-acme",
    serverName: "Acme CRM",
    destination: "mcp.acme-crm.example",
    tool: "search_contacts",
    toolTitle: null,
    kind: "read",
    destructive: false,
    args: {},
    inputSchema: {},
  });

  it("adds new ones in order, skipping shown and settled ones", () => {
    const cur = [p("a")];
    expect(mergeProposals(cur, [p("a")], new Set())).toBe(cur);
    expect(
      mergeProposals(cur, [p("b"), p("c"), p("b")], new Set(["c"])).map((x) => x.proposalId),
    ).toEqual(["a", "b"]);
  });
});

describe("call log labels", () => {
  it("names outcomes and refusals", () => {
    expect(outcomeLabel({ outcome: "ran", refusal: null })).toBe("Ran");
    expect(outcomeLabel({ outcome: "declined", refusal: null })).toBe("You didn't run it");
    expect(outcomeLabel({ outcome: "server_error", refusal: null })).toBe("Server error");
    expect(outcomeLabel({ outcome: "failed", refusal: null })).toBe("Failed");
    expect(outcomeLabel({ outcome: "refused", refusal: "keep_local" })).toBe(
      "Refused: the note is kept local",
    );
    expect(outcomeLabel({ outcome: "refused", refusal: null })).toBe("Refused");
    for (const code of [
      "not_offered",
      "keep_local",
      "after_result",
      "budget",
      "invalid_arguments",
      "tool_changed",
      "not_approved",
      "timed_out",
    ] as const) {
      expect(refusalLabel(code)).not.toBe("refused");
    }
    // an unknown code from a newer backend still reads sensibly
    expect(refusalLabel("toString" as never)).toBe("refused");
  });

  it("formats durations, sizes and order", () => {
    expect(formatDuration(840)).toBe("840 ms");
    expect(formatDuration(1210)).toBe("1.2 s");
    expect(formatDuration(125_000)).toBe("2 min");
    expect(formatChars(1, false)).toBe("1 char");
    expect(formatChars(12000, true)).toBe("12,000 chars, cut");
    expect(newestFirst([1, 2, 3])).toEqual([3, 2, 1]);
  });

  it("says how long ago", () => {
    const now = Date.UTC(2026, 9, 28, 12, 0, 0);
    const at = (secsAgo: number) => now / 1000 - secsAgo;
    expect(relativeTime(at(10), now)).toBe("just now");
    expect(relativeTime(at(-30), now)).toBe("just now");
    expect(relativeTime(at(5 * 60), now)).toBe("5 min ago");
    expect(relativeTime(at(3 * 3600), now)).toBe("3 h ago");
    expect(relativeTime(at(26 * 3600), now)).toBe("yesterday");
    expect(relativeTime(at(4 * 86400), now)).toBe("4 days ago");
    expect(relativeTime(at(30 * 86400), now)).toBe("Sep 28, 2026");
  });
});
