import { describe, expect, it } from "vitest";
import {
  KEEP_LOCAL_NOTICE,
  activityDetail,
  activityResult,
  allowedPermissions,
  argsFromFields,
  argsTemplate,
  canTry,
  candidateSummary,
  connectorBadge,
  connectorStatus,
  customDraftFrom,
  decidedLabel,
  directoryInput,
  emptyCustomDraft,
  emptyLocalDraft,
  fieldValuesFrom,
  filterDirectory,
  formatTime,
  groupTools,
  groupValue,
  groupValueLabel,
  isProblem,
  localDraftFrom,
  loosens,
  mergeProposals,
  parseArgs,
  parseCommand,
  planGroupChange,
  prettyArgs,
  quoteArg,
  relativeTime,
  reviewBanner,
  sameAddress,
  sameArgs,
  schemaFields,
  splitArgs,
  splitDetails,
  toolStatusChip,
  validateCustom,
  validateLocal,
  willRunPreview,
  type CustomDraft,
  type KVRow,
  type LocalDraft,
} from "./connectorsModel";
import type {
  Connector,
  ConnectorCallProposal,
  ConnectorDirectoryEntry,
  ConnectorTool,
} from "./types";

const tool = (t: Partial<ConnectorTool> & { name: string }): ConnectorTool => ({
  title: null,
  description: "",
  kind: "read",
  labelled: true,
  destructive: false,
  permission: "needs_approval",
  status: "ok",
  problem: null,
  pinnedHash: "a1b2c3d4e5f6",
  reviewHash: null,
  diff: [],
  inputSchema: {},
  ...t,
});

const connector = (c: Partial<Connector> = {}): Connector => ({
  id: "c1",
  name: "Acme CRM",
  kind: "web",
  transport: { kind: "http", url: "https://mcp.acme-crm.example/mcp", headers: [] },
  auth: { kind: "none" },
  destination: "mcp.acme-crm.example",
  willRun: null,
  approved: true,
  enabled: true,
  configHash: "3f9a1c0b7e21",
  status: "connected",
  reviewCount: 0,
  signedIn: null,
  secrets: { apiKeySet: false, clientSecretSet: false, values: [] },
  lastSync: null,
  tools: [],
  ...c,
});

const entry = (e: Partial<ConnectorDirectoryEntry> = {}): ConnectorDirectoryEntry => ({
  id: "example-notes",
  name: "Example Notes",
  vendor: "Example Notes Inc.",
  description: "Search and read pages.",
  url: "https://mcp.example-notes.example/mcp",
  auth: "oauth",
  keyLabel: null,
  ...e,
});

let seq = 0;
const newId = () => `r${++seq}`;
const row = (r: Partial<KVRow>): KVRow => ({
  id: newId(),
  name: "",
  value: "",
  secret: false,
  stored: false,
  ...r,
});

describe("copy and errors", () => {
  it("exports the Keep-local notice for Ask", () => {
    expect(KEEP_LOCAL_NOTICE).toBe(
      "Connectors were off for this question because it includes a Keep-local meeting, which never leaves this computer.",
    );
  });

  it("splits program output off an error", () => {
    expect(splitDetails("Couldn't find npx. Install Node.js and try again.")).toEqual({
      message: "Couldn't find npx. Install Node.js and try again.",
      details: null,
    });
    expect(splitDetails("The program stopped right away.\n---\nError: boom\n  at x")).toEqual({
      message: "The program stopped right away.",
      details: "Error: boom\n  at x",
    });
    expect(splitDetails(new Error("bad"))).toEqual({ message: "bad", details: null });
  });
});

describe("permissions", () => {
  it("loosening means more room for the AI", () => {
    expect(loosens("blocked", "needs_approval")).toBe(true);
    expect(loosens("needs_approval", "always_allow")).toBe(true);
    expect(loosens("always_allow", "needs_approval")).toBe(false);
    expect(loosens("needs_approval", "blocked")).toBe(false);
    expect(loosens("blocked", "blocked")).toBe(false);
  });

  it("only usable tools can leave Blocked", () => {
    expect(allowedPermissions(tool({ name: "a" }))).toEqual([
      "always_allow",
      "needs_approval",
      "blocked",
    ]);
    for (const status of ["changed", "new", "missing", "unusable"] as const) {
      expect(allowedPermissions(tool({ name: "a", status }))).toEqual(["blocked"]);
    }
  });

  it("groups read-only and change tools; unlabelled ones are change tools", () => {
    const tools = [
      tool({ name: "r" }),
      tool({ name: "w", kind: "write" }),
      tool({ name: "u", kind: "write", labelled: false }),
    ];
    const g = groupTools(tools);
    expect(g.read.map((t) => t.name)).toEqual(["r"]);
    expect(g.write.map((t) => t.name)).toEqual(["w", "u"]);
  });

  it("a group shows one permission or Mixed", () => {
    expect(groupValue([])).toBeNull();
    expect(groupValue([tool({ name: "a" }), tool({ name: "b" })])).toBe("needs_approval");
    expect(groupValue([tool({ name: "a" }), tool({ name: "b", permission: "blocked" })])).toBe(
      "mixed",
    );
    expect(groupValueLabel("mixed")).toBe("Mixed");
    expect(groupValueLabel("always_allow")).toBe("Always allow");
    expect(groupValueLabel(null)).toBe("No tools");
  });

  it("a group change skips tools already there and tools that must stay blocked", () => {
    const tools = [
      tool({ name: "a" }),
      tool({ name: "b", permission: "always_allow" }),
      tool({ name: "c", status: "changed", permission: "blocked" }),
    ];
    expect(planGroupChange(tools, "always_allow")).toEqual([
      { tool: "a", permission: "always_allow" },
    ]);
    expect(planGroupChange(tools, "blocked")).toEqual([
      { tool: "a", permission: "blocked" },
      { tool: "b", permission: "blocked" },
    ]);
  });

  it("labels tool states", () => {
    expect(toolStatusChip(tool({ name: "a" }))).toBeNull();
    expect(toolStatusChip(tool({ name: "a", status: "changed" }))).toBe("Changed — review");
    expect(toolStatusChip(tool({ name: "a", status: "new" }))).toBe("New — review");
    expect(toolStatusChip(tool({ name: "a", status: "missing" }))).toBe("No longer offered");
    expect(toolStatusChip(tool({ name: "a", status: "unusable", problem: "bad schema" }))).toBe(
      "Can't be used: bad schema",
    );
  });
});

describe("connectors", () => {
  it("says each status in words", () => {
    expect(connectorStatus(connector()).text).toBe("Connected · mcp.acme-crm.example");
    expect(connectorStatus(connector({ kind: "local" })).text).toBe("Connected");
    expect(connectorStatus(connector({ status: "needs_sign_in" }))).toEqual({
      text: "Needs sign-in",
      problem: true,
    });
    expect(connectorStatus(connector({ status: "review", reviewCount: 2 })).text).toBe(
      "2 tools changed — review",
    );
    expect(connectorStatus(connector({ status: "review", reviewCount: 1 })).text).toBe(
      "1 tool changed — review",
    );
    expect(connectorStatus(connector({ status: "could_not_start" })).text).toBe("Couldn't start");
    expect(connectorStatus(connector({ status: "off" }))).toEqual({ text: "Off", problem: false });
    expect(reviewBanner(2)).toBe(
      "2 tools changed since you approved them. They're blocked until you review.",
    );
    expect(reviewBanner(1)).toBe(
      "1 tool changed since you approved it. It's blocked until you review.",
    );
  });

  it("badges local and custom connectors, not directory ones", () => {
    const dir = [entry()];
    expect(connectorBadge(connector(), dir)).toBe("Custom");
    expect(
      connectorBadge(
        connector({
          transport: { kind: "http", url: "https://MCP.example-notes.example/mcp/", headers: [] },
        }),
        dir,
      ),
    ).toBeNull();
    expect(
      connectorBadge(
        connector({
          kind: "local",
          transport: { kind: "stdio", command: "npx", args: [], env: [] },
        }),
        dir,
      ),
    ).toBe("Local");
    expect(sameAddress("https://a.example/mcp/", "https://A.example/mcp")).toBe(true);
  });

  it("searches the directory by every word", () => {
    const dir = [
      entry(),
      entry({ id: "t", name: "Sample Tracker", vendor: "Sample Labs", description: "Issues." }),
    ];
    expect(filterDirectory(dir, "").length).toBe(2);
    expect(filterDirectory(dir, "sample issues").map((e) => e.id)).toEqual(["t"]);
    expect(filterDirectory(dir, "nothing here")).toEqual([]);
  });

  it("turns a directory entry into an add request", () => {
    expect(directoryInput(entry())).toEqual({
      config: {
        name: "Example Notes",
        transport: { kind: "http", url: "https://mcp.example-notes.example/mcp", headers: [] },
        auth: { kind: "oauth", scopes: [], clientId: null },
      },
      secrets: {},
    });
    expect(directoryInput(entry({ auth: "api_key" }), " key-1 ").secrets).toEqual({
      apiKey: "key-1",
    });
  });
});

describe("Add custom", () => {
  const draft = (d: Partial<CustomDraft>): CustomDraft => ({
    ...emptyCustomDraft(),
    name: "Team wiki",
    url: "https://wiki.example.com/mcp",
    signIn: "none",
    ...d,
  });

  it("needs https, a name and a sign-in choice (nothing pre-selected)", () => {
    expect(emptyCustomDraft().signIn).toBe("");
    const bad = validateCustom(draft({ name: " ", url: "http://wiki.example.com", signIn: "" }));
    expect(bad.input).toBeNull();
    expect(bad.errors).toEqual({
      name: "Give it a name.",
      url: "Use an address that starts with https://",
      signIn: "Choose how it signs in.",
    });
    expect(validateCustom(draft({ url: "wiki.example.com" })).errors.url).toBe(
      "Use an address that starts with https://",
    );
    expect(validateCustom(draft({ url: "https://dana:pw@wiki.example.com" })).errors.url).toBe(
      "Leave sign-in details out of the address.",
    );
    expect(validateCustom(draft({ name: "x".repeat(41) })).errors.name).toContain("40");
  });

  it("builds the input with headers; secret values go to the keychain", () => {
    const { input, errors } = validateCustom(
      draft({
        headers: [
          row({ name: "X-Team", value: "design" }),
          row({ name: "X-Token", value: "s3cret", secret: true }),
          row({}), // empty rows are ignored
        ],
      }),
    );
    expect(errors).toEqual({});
    expect(input).toEqual({
      config: {
        name: "Team wiki",
        transport: {
          kind: "http",
          url: "https://wiki.example.com/mcp",
          headers: [
            { name: "X-Team", value: "design" },
            { name: "X-Token", value: null },
          ],
        },
        auth: { kind: "none" },
      },
      secrets: { apiKey: null, clientSecret: null, values: { "X-Token": "s3cret" } },
    });
  });

  it("checks headers: names, repeats, and a value for every secret on add", () => {
    const a = row({ name: "Bad Name", value: "x" });
    const b = row({ name: "X-One", value: "1" });
    const c = row({ name: "x-one", value: "2" });
    const d = row({ name: "X-Secret", secret: true });
    const { errors } = validateCustom(draft({ headers: [a, b, c, d] }));
    expect(errors[`row:${a.id}`]).toContain("header name");
    expect(errors[`row:${b.id}`]).toBeUndefined();
    expect(errors[`row:${c.id}`]).toBe("This name is already listed.");
    expect(errors[`row:${d.id}`]).toBe("Enter a value. It's saved in your keychain.");
    // editing: a stored secret may stay empty
    const kept = row({ name: "X-Secret", secret: true, stored: true });
    expect(validateCustom(draft({ headers: [kept] })).errors).toEqual({});
    const auth = row({ name: "Authorization", value: "x" });
    expect(
      validateCustom(draft({ signIn: "api_key", apiKey: "k", headers: [auth] })).errors,
    ).toEqual({
      [`row:${auth.id}`]: "Sign-in sets this header.",
    });
  });

  it("API key: required on add, optional when one is saved", () => {
    expect(validateCustom(draft({ signIn: "api_key" })).errors.apiKey).toBe(
      "Enter the key or token.",
    );
    expect(
      validateCustom(draft({ signIn: "api_key" }), { apiKeySet: true }).input?.secrets,
    ).toEqual({ apiKey: null, clientSecret: null, values: {} });
    expect(
      validateCustom(draft({ signIn: "api_key", apiKey: " k-1 " })).input?.config.auth,
    ).toEqual({ kind: "api_key" });
    expect(
      validateCustom(draft({ signIn: "api_key", apiKey: " k-1 " })).input?.secrets.apiKey,
    ).toBe("k-1");
  });

  it("OAuth: client ID optional; a client secret needs one", () => {
    expect(validateCustom(draft({ signIn: "oauth" })).input?.config.auth).toEqual({
      kind: "oauth",
      scopes: [],
      clientId: null,
    });
    expect(validateCustom(draft({ signIn: "oauth", clientSecret: "s" })).errors.clientSecret).toBe(
      "A client secret needs a client ID.",
    );
    const full = validateCustom(draft({ signIn: "oauth", clientId: " app ", clientSecret: "s" }));
    expect(full.input?.config.auth).toEqual({ kind: "oauth", scopes: [], clientId: "app" });
    expect(full.input?.secrets.clientSecret).toBe("s");
  });

  it("edit: prefills the form, secrets empty and marked stored", () => {
    const c = connector({
      auth: { kind: "api_key" },
      transport: {
        kind: "http",
        url: "https://wiki.example.com/mcp",
        headers: [
          { name: "X-Team", value: "design" },
          { name: "X-Token", value: null },
        ],
      },
      secrets: {
        apiKeySet: true,
        clientSecretSet: false,
        values: [{ name: "X-Token", set: true }],
      },
    });
    const d = customDraftFrom(c, newId);
    expect(d.signIn).toBe("api_key");
    expect(d.apiKey).toBe("");
    expect(
      d.headers.map(({ name, value, secret, stored }) => ({ name, value, secret, stored })),
    ).toEqual([
      { name: "X-Team", value: "design", secret: false, stored: false },
      { name: "X-Token", value: "", secret: true, stored: true },
    ]);
    expect(validateCustom(d, { apiKeySet: true }).input?.secrets).toEqual({
      apiKey: null,
      clientSecret: null,
      values: {},
    });
  });
});

describe("Add local", () => {
  const draft = (d: Partial<LocalDraft>): LocalDraft => ({
    ...emptyLocalDraft(),
    name: "Docs search",
    program: "npx",
    ...d,
  });

  it("builds a program with arguments and settings", () => {
    const { input, errors } = validateLocal(
      draft({
        args: [
          { id: "a", value: "-y" },
          { id: "b", value: "" },
          { id: "c", value: "@example/docs-server" },
        ],
        env: [
          row({ name: "DOCS_TOKEN", value: "t0k", secret: true }),
          row({ name: "MODE", value: "fast" }),
        ],
      }),
    );
    expect(errors).toEqual({});
    expect(input).toEqual({
      config: {
        name: "Docs search",
        transport: {
          kind: "stdio",
          command: "npx",
          args: ["-y", "@example/docs-server"],
          env: [
            { name: "DOCS_TOKEN", value: null },
            { name: "MODE", value: "fast" },
          ],
        },
        auth: { kind: "none" },
      },
      secrets: { values: { DOCS_TOKEN: "t0k" } },
    });
  });

  it("needs a program (and a file when choosing one) and good setting names", () => {
    expect(validateLocal(draft({ program: "" })).errors.program).toBe("Choose the program.");
    expect(validateLocal(draft({ program: "file", file: " " })).errors.program).toBe(
      "Choose a file.",
    );
    expect(
      validateLocal(draft({ program: "file", file: "/opt/x/server" })).input?.config.transport,
    ).toMatchObject({
      command: "/opt/x/server",
    });
    const bad = row({ name: "9BAD", value: "x" });
    expect(validateLocal(draft({ env: [bad] })).errors[`row:${bad.id}`]).toContain("not starting");
    const tooMany = [...Array(33)].map((_, i) => ({ id: `${i}`, value: `a${i}` }));
    expect(validateLocal(draft({ args: tooMany })).errors.args).toBe("Use 32 arguments or fewer.");
  });

  it("previews what will run, with secrets as ••••", () => {
    expect(
      willRunPreview(
        draft({
          args: [
            { id: "a", value: "-y" },
            { id: "b", value: "@example/docs server" },
          ],
          env: [
            row({ name: "TOKEN", value: "abc", secret: true }),
            row({ name: "MODE", value: "fast" }),
          ],
        }),
      ),
    ).toEqual({
      command: 'npx -y "@example/docs server"',
      settings: ["TOKEN=••••", "MODE=fast"],
    });
    expect(willRunPreview(draft({ program: "" })).command).toBe("…");
    expect(quoteArg("")).toBe('""');
    expect(quoteArg('say "hi"')).toBe('"say \\"hi\\""');
  });

  it("Paste a command fills rows and never runs anything", () => {
    const p = parseCommand('TOKEN=abc MODE=fast npx -y "@example/docs server"', newId);
    expect("error" in p).toBe(false);
    if ("error" in p) return;
    expect(p.program).toBe("npx");
    expect(p.args.map((a) => a.value)).toEqual(["-y", "@example/docs server"]);
    expect(p.env.map(({ name, value, secret }) => ({ name, value, secret }))).toEqual([
      { name: "TOKEN", value: "abc", secret: true },
      { name: "MODE", value: "fast", secret: true },
    ]);
    expect(parseCommand("C:\\Tools\\uvx.exe server", newId)).toMatchObject({ program: "uvx" });
    expect(parseCommand("/opt/x/server --port 3", newId)).toMatchObject({
      program: "file",
      file: "/opt/x/server",
    });
    expect(parseCommand("  ", newId)).toEqual({
      error: "Paste a command, like npx -y @example/server.",
    });
    expect(parseCommand('npx "open', newId)).toEqual({
      error: "A quote in the command isn't closed.",
    });
    expect(splitArgs('a "" b')).toEqual({ args: ["a", "", "b"] });
  });

  it("edit: prefills program, arguments and settings", () => {
    const c = connector({
      kind: "local",
      transport: {
        kind: "stdio",
        command: "/opt/x/server",
        args: ["--port", "3"],
        env: [{ name: "TOKEN", value: null }],
      },
      secrets: { apiKeySet: false, clientSecretSet: false, values: [{ name: "TOKEN", set: true }] },
    });
    const d = localDraftFrom(c, newId);
    expect(d.program).toBe("file");
    expect(d.file).toBe("/opt/x/server");
    expect(d.args.map((a) => a.value)).toEqual(["--port", "3"]);
    expect(d.env[0]).toMatchObject({ name: "TOKEN", value: "", secret: true, stored: true });
    expect(validateLocal(d).errors).toEqual({});
  });

  it("describes an import candidate", () => {
    expect(
      candidateSummary({
        key: "k",
        name: "Files",
        kind: "local",
        command: "npx",
        args: ["-y", "@example/files server"],
        envNames: [],
        url: null,
        headerNames: [],
      }),
    ).toBe('npx -y "@example/files server"');
  });
});

describe("card fields", () => {
  const schema = {
    type: "object",
    properties: {
      query: { type: "string", title: "Search for", description: "Name or email" },
      limit: { type: "integer" },
      score: { type: ["number", "null"] },
      exact: { type: "boolean" },
      stage: { type: "string", enum: ["won", "lost"] },
    },
    required: ["query", "limit"],
  };

  it("makes fields for simple properties", () => {
    const f = schemaFields(schema, { query: "acme", limit: 5 });
    expect(f?.map((x) => [x.name, x.label, x.kind, x.required])).toEqual([
      ["query", "Search for", "string", true],
      ["limit", "limit", "integer", true],
      ["score", "score", "number", false],
      ["exact", "exact", "boolean", false],
      ["stage", "stage", "string", false],
    ]);
    expect(f?.[0].description).toBe("Name or email");
    expect(f?.[4].options).toEqual(["won", "lost"]);
  });

  it("falls back to code for anything else", () => {
    expect(schemaFields({ properties: { o: { type: "object" } } }, {})).toBeNull();
    expect(schemaFields({ properties: { a: { type: "array" } } }, {})).toBeNull();
    expect(schemaFields(schema, { extra: 1 })).toBeNull();
    expect(schemaFields(schema, { limit: "5" })).toBeNull();
    expect(schemaFields(schema, { limit: 1.5 })).toBeNull();
    expect(schemaFields(null, {})).toEqual([]);
    expect(schemaFields(null, { a: 1 })).toBeNull();
  });

  it("turns field values back into arguments", () => {
    const f = schemaFields(schema, { query: "acme", limit: 5 })!;
    const v = fieldValuesFrom(f, { query: "acme", limit: 5 });
    expect(v.get("limit")).toBe("5");
    expect(v.get("exact")).toBe(false);
    expect(argsFromFields(f, v, { query: "acme", limit: 5 })).toEqual({
      ok: true,
      value: { query: "acme", limit: 5 },
    });
    const edited = new Map(v).set("limit", "7").set("exact", true).set("score", "0.5");
    expect(argsFromFields(f, edited, {})).toEqual({
      ok: true,
      value: { query: "acme", limit: 7, score: 0.5, exact: true },
    });
    expect(argsFromFields(f, new Map(v).set("limit", ""), {})).toEqual({
      ok: false,
      error: "Enter a number for “limit”.",
    });
    expect(argsFromFields(f, new Map(v).set("limit", "2.5"), {})).toEqual({
      ok: false,
      error: "“limit” must be a whole number.",
    });
    expect(argsFromFields(f, new Map(v).set("score", "abc"), {})).toEqual({
      ok: false,
      error: "“score” must be a number.",
    });
  });

  it("keeps a __proto__ property as a plain key", () => {
    const s = {
      properties: JSON.parse('{"__proto__": {"type": "string"}}'),
      required: ["__proto__"],
    };
    const f = schemaFields(s, {})!;
    const out = argsFromFields(f, new Map([["__proto__", "x"]]), {});
    expect(out.ok && JSON.stringify(out.value)).toBe('{"__proto__":"x"}');
    expect(out.ok && Object.getPrototypeOf(out.value)).toBe(Object.prototype);
  });

  it("JSON editing and the decided summary", () => {
    expect(prettyArgs({ a: 1 })).toBe('{\n  "a": 1\n}');
    expect(parseArgs("[1]")).toEqual({
      ok: false,
      error: "Details must be a JSON object ({ … }), not a list or a value.",
    });
    expect(parseArgs("{").ok).toBe(false);
    expect(parseArgs("").ok).toBe(false);
    expect(sameArgs({ a: 1 }, { a: 1 })).toBe(true);
    const at = new Date(2026, 9, 28, 14, 14).getTime();
    expect(decidedLabel("allow_once", at)).toBe("Allowed once · 2:14 PM");
    expect(decidedLabel("always_allow", at)).toBe("Always allowed · 2:14 PM");
    expect(decidedLabel("deny", at)).toBe("Denied");
  });

  it("merges proposals without repeats or settled ones", () => {
    const p = (id: string) => ({ proposalId: id }) as ConnectorCallProposal;
    const cur = [p("a")];
    expect(mergeProposals(cur, [p("a")], new Set())).toBe(cur);
    expect(
      mergeProposals(cur, [p("b"), p("c"), p("b")], new Set(["c"])).map((x) => x.proposalId),
    ).toEqual(["a", "b"]);
  });
});

describe("activity", () => {
  it("names results and problems", () => {
    expect(activityResult({ outcome: "ran", approval: "user" })).toBe("Allowed by you");
    expect(activityResult({ outcome: "ran", approval: "automatic" })).toBe("Allowed automatically");
    expect(activityResult({ outcome: "declined", approval: null })).toBe("Denied");
    expect(activityResult({ outcome: "refused", approval: null })).toBe("Blocked");
    expect(activityResult({ outcome: "failed", approval: null })).toBe("Failed");
    expect(activityResult({ outcome: "server_error", approval: null })).toBe("Failed");
    expect(activityDetail({ outcome: "refused", refusal: "keep_local" })).toBe(
      "the note is kept local",
    );
    expect(activityDetail({ outcome: "refused", refusal: "toString" as never })).toBeNull();
    expect(activityDetail({ outcome: "ran", refusal: null })).toBeNull();
    expect(isProblem({ outcome: "declined" })).toBe(false);
    expect(isProblem({ outcome: "refused" })).toBe(true);
    expect(isProblem({ outcome: "server_error" })).toBe(true);
  });

  it("formats times", () => {
    const now = new Date(2026, 9, 28, 15, 0).getTime();
    expect(formatTime(new Date(2026, 9, 28, 14, 14).getTime() / 1000, now)).toBe("2:14 PM");
    expect(formatTime(new Date(2026, 9, 27, 9, 5).getTime() / 1000, now)).toBe("Oct 27, 9:05 AM");
    expect(relativeTime(now / 1000 - 10, now)).toBe("just now");
    expect(relativeTime(now / 1000 - 300, now)).toBe("5 min ago");
    expect(relativeTime(now / 1000 - 3 * 3600, now)).toBe("3 h ago");
    expect(relativeTime(now / 1000 - 26 * 3600, now)).toBe("yesterday");
  });
});

describe("Try…", () => {
  it("is offered for usable tools that aren't Blocked", () => {
    expect(canTry(tool({ name: "a" }))).toBe(true);
    expect(canTry(tool({ name: "a", permission: "always_allow" }))).toBe(true);
    expect(canTry(tool({ name: "a", permission: "blocked" }))).toBe(false);
    for (const status of ["changed", "new", "missing", "unusable"] as const) {
      expect(canTry(tool({ name: "a", status }))).toBe(false);
    }
  });

  it("starts from the required properties with empty values", () => {
    expect(
      argsTemplate({
        type: "object",
        properties: {
          query: { type: "string", default: "ignored" },
          limit: { type: "integer" },
          exact: { type: "boolean" },
          tags: { type: "array" },
          filter: { type: "object" },
          maybe: { type: ["null", "string"] },
          anything: {},
          optional: { type: "string" },
        },
        required: ["query", "limit", "exact", "tags", "filter", "maybe", "anything", "nope"],
      }),
    ).toEqual({
      query: "",
      limit: 0,
      exact: false,
      tags: [],
      filter: {},
      maybe: "",
      anything: null,
    });
    expect(argsTemplate(null)).toEqual({});
    expect(argsTemplate({ properties: { a: { type: "string" } }, required: "a" })).toEqual({});
    const odd = argsTemplate({
      properties: JSON.parse('{"__proto__": {"type": "string"}}'),
      required: ["__proto__"],
    });
    expect(JSON.stringify(odd)).toBe('{"__proto__":""}');
    expect(Object.getPrototypeOf(odd)).toBe(Object.prototype);
  });
});
