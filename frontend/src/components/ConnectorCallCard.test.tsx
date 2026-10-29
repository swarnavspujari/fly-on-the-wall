// @vitest-environment jsdom
import { act } from "react";
import type { ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { api, CONNECTORS_PROPOSAL_EVENT, CONNECTORS_SETTLED_EVENT } from "../api";
import type { ConnectorCallProposal } from "../types";
import { ARM_DELAY_MS, COLLAPSE_MS, ConnectorCallStack } from "./ConnectorCallCard";

// Backend events: keep each handler so a test can fire one.
const eventHandlers: Record<string, (e: { payload: unknown }) => void> = {};
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, handler: (e: { payload: unknown }) => void) => {
    eventHandlers[event] = handler;
    return () => {
      if (eventHandlers[event] === handler) delete eventHandlers[event];
    };
  }),
}));

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const proposal = (over: Partial<ConnectorCallProposal> = {}): ConnectorCallProposal => ({
  proposalId: "p1",
  turnId: "t1",
  serverId: "c-acme",
  serverName: "Acme CRM",
  destination: "mcp.acme-crm.example",
  tool: "search_contacts",
  toolTitle: "Search contacts",
  kind: "read",
  destructive: false,
  args: { query: "Example Co" },
  inputSchema: {
    type: "object",
    properties: {
      query: { type: "string", title: "Search for", description: "Name or email" },
      limit: { type: "integer" },
    },
    required: ["query"],
  },
  ...over,
});
const write = (over: Partial<ConnectorCallProposal> = {}) =>
  proposal({
    proposalId: "w1",
    tool: "delete_contact",
    toolTitle: "Delete contact",
    kind: "write",
    destructive: true,
    args: { contact_id: "C-77" },
    inputSchema: {
      type: "object",
      properties: { contact_id: { type: "string" } },
      required: ["contact_id"],
    },
    ...over,
  });

let root: Root | null = null;
let host: HTMLElement | null = null;
let decide: Mock<typeof api.decideConnectorCall>;

/** Advance fake time and let the promises it releases settle. */
async function settle(ms = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}
const arm = () => settle(ARM_DELAY_MS);

async function render(ui: ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(ui));
  await settle();
}

async function emit(event: string, payload: unknown) {
  await act(async () => eventHandlers[event]?.({ payload }));
  await settle();
}

const card = () => host!.querySelector<HTMLElement>('[role="group"][data-proposal-id]');
const heading = () => {
  const id = card()?.getAttribute("aria-labelledby");
  return id ? document.getElementById(id)?.textContent : null;
};
const button = (text: string) =>
  [...host!.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent?.trim() === text,
  ) ?? null;
const field = (label: string) => {
  const l = [...host!.querySelectorAll("label")].find((x) => x.textContent === label);
  return l ? (document.getElementById(l.getAttribute("for") ?? "") as HTMLInputElement) : null;
};
const live = () => host!.querySelector('[role="status"][aria-live="polite"]')?.textContent ?? "";

async function click(el: HTMLElement | null) {
  if (!el) throw new Error("element not found");
  await act(async () => el.click());
  await settle();
}

async function type(el: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const proto =
    el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")!.set!;
  await act(async () => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await settle();
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.spyOn(api, "pendingConnectorCalls").mockResolvedValue([]);
  decide = vi.fn<typeof api.decideConnectorCall>(async () => ({ ok: true, error: null }));
  vi.spyOn(api, "decideConnectorCall").mockImplementation(decide);
});

afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("read card", () => {
  it("asks to look something up, names the destination, and starts on Deny", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([proposal()]);
    await render(<ConnectorCallStack />);
    expect(card()!.getAttribute("role")).toBe("group");
    expect(heading()).toBe("Ask wants to look something up in Acme CRM · Search contacts");
    expect(card()!.textContent).toContain("tool: search_contacts");
    const sub = document.getElementById(card()!.getAttribute("aria-describedby")!)!;
    expect(sub.textContent).toBe("Sends to mcp.acme-crm.example");
    expect(card()!.textContent).not.toContain("Can change data");
    expect(field("Search for")!.value).toBe("Example Co");
    expect(field("limit (optional)")!.value).toBe("");
    expect(button("Allow once")).not.toBeNull();
    expect(button("Always allow this tool…")).not.toBeNull();
    expect(document.activeElement).toBe(button("Deny"));
    // announced politely
    expect(live()).toBe("Ask wants to look something up in Acme CRM · Search contacts");
  });

  it("keeps Allow once and Always allow off for a moment after it appears", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([
      proposal(),
      proposal({ proposalId: "p2" }),
    ]);
    await render(<ConnectorCallStack />);
    expect(button("Allow once")!.disabled).toBe(true);
    expect(button("Always allow this tool…")!.disabled).toBe(true);
    expect(button("Deny")!.disabled).toBe(false);
    await click(button("Allow once"));
    expect(decide).not.toHaveBeenCalled();
    await settle(ARM_DELAY_MS - 1);
    expect(button("Allow once")!.disabled).toBe(true);
    await settle(1);
    expect(button("Allow once")!.disabled).toBe(false);

    // A double click: the first approves p1, the second lands on p2, which
    // has just appeared and isn't armed yet.
    await click(button("Allow once"));
    expect(decide).toHaveBeenCalledTimes(1);
    expect(decide).toHaveBeenLastCalledWith("p1", "allow_once", null);
    expect(card()!.getAttribute("data-proposal-id")).toBe("p2");
    expect(button("Allow once")!.disabled).toBe(true);
    await click(button("Allow once"));
    await click(button("Always allow this tool…"));
    expect(decide).toHaveBeenCalledTimes(1);
    await arm();
    expect(button("Allow once")!.disabled).toBe(false);
  });

  it("sends unchanged details as null and edited ones as the new object", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([
      proposal(),
      proposal({ proposalId: "p2" }),
    ]);
    await render(<ConnectorCallStack />);
    await arm();
    await click(button("Allow once"));
    expect(decide).toHaveBeenLastCalledWith("p1", "allow_once", null);
    await arm();
    await type(field("Search for")!, "Dana");
    await type(field("limit (optional)")!, "5");
    await click(button("Allow once"));
    expect(decide).toHaveBeenLastCalledWith("p2", "allow_once", { query: "Dana", limit: 5 });
  });

  it("Show as code edits the JSON; bad JSON blocks sending", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([proposal()]);
    await render(<ConnectorCallStack />);
    await arm();
    await type(field("limit (optional)")!, "abc");
    expect(card()!.textContent).toContain("“limit” must be a number.");
    expect(button("Allow once")!.disabled).toBe(true);
    await type(field("limit (optional)")!, "");
    await click(button("Show as code"));
    const area = card()!.querySelector("textarea")!;
    expect(area.value).toBe('{\n  "query": "Example Co"\n}');
    await type(area, "{");
    expect(card()!.textContent).toContain("That isn't valid JSON");
    expect(button("Allow once")!.disabled).toBe(true);
    await type(area, '{ "query": "Sam", "limit": 2 }');
    await click(button("Show as fields"));
    expect(field("Search for")!.value).toBe("Sam");
    expect(field("limit (optional)")!.value).toBe("2");
    await click(button("Allow once"));
    expect(decide).toHaveBeenLastCalledWith("p1", "allow_once", { query: "Sam", limit: 2 });
  });

  it("a schema it can't show as fields opens as code", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([
      proposal({
        args: { filter: { owner: "dana" } },
        inputSchema: { type: "object", properties: { filter: { type: "object" } } },
      }),
    ]);
    await render(<ConnectorCallStack />);
    expect(card()!.querySelector("textarea")!.value).toContain('"owner": "dana"');
    expect(button("Show as fields")).toBeNull();
  });

  it("Always allow this tool… asks the backend (which shows the native dialog)", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([proposal()]);
    await render(<ConnectorCallStack />);
    await arm();
    await click(button("Always allow this tool…"));
    expect(decide).toHaveBeenLastCalledWith("p1", "always_allow", null);
    expect(card()).toBeNull();
    expect(host!.textContent).toMatch(/Always allowed · \d{1,2}:\d\d [AP]M/);
  });

  it("a no in the native dialog keeps the card open with no error", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([proposal()]);
    decide.mockResolvedValueOnce({ ok: false, error: null });
    await render(<ConnectorCallStack />);
    await arm();
    await click(button("Always allow this tool…"));
    expect(card()).not.toBeNull();
    expect(card()!.querySelector('[role="alert"]')).toBeNull();
    expect(button("Allow once")!.disabled).toBe(false);

    decide.mockResolvedValueOnce({
      ok: false,
      error: "These details don't fit the tool: missing query.",
    });
    await click(button("Allow once"));
    expect(card()!.querySelector('[role="alert"]')!.textContent).toBe(
      "These details don't fit the tool: missing query.",
    );
  });

  it("Deny (or Esc) closes it to a summary that goes away", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([
      proposal(),
      proposal({ proposalId: "p2" }),
    ]);
    await render(<ConnectorCallStack />);
    await click(button("Deny"));
    expect(decide).toHaveBeenLastCalledWith("p1", "deny", null);
    expect(host!.querySelector('[data-decided="p1"]')!.textContent).toContain("Denied");
    expect(live()).toBe("Ask wants to look something up in Acme CRM · Search contacts");

    await act(async () => {
      card()!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    await settle();
    expect(decide).toHaveBeenLastCalledWith("p2", "deny", null);
    expect(card()).toBeNull();
    await settle(COLLAPSE_MS);
    expect(host!.querySelector("[data-decided]")).toBeNull();
  });

  it("a deny that fails still closes the card", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([proposal()]);
    decide.mockRejectedValueOnce("That call is no longer waiting for an answer.");
    await render(<ConnectorCallStack />);
    await click(button("Deny"));
    expect(card()).toBeNull();
  });

  it("doesn't take focus from someone typing", async () => {
    const box = document.createElement("textarea");
    document.body.appendChild(box);
    box.focus();
    await render(<ConnectorCallStack />);
    await emit(CONNECTORS_PROPOSAL_EVENT, proposal());
    expect(card()).not.toBeNull();
    expect(document.activeElement).toBe(box);
    expect(live()).toContain("Ask wants to look something up in Acme CRM");
    box.remove();
  });

  it("shows server and model text as plain text only", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([
      proposal({
        serverName: "<b>Acme</b>",
        toolTitle: "**Search** [now](https://evil.example)",
        destination: "<img src=x onerror=alert(1)>",
        args: { query: "<script>alert(1)</script>" },
        inputSchema: {
          type: "object",
          properties: {
            query: { type: "string", title: "<i>Query</i>", description: "<a href=x>x</a>" },
          },
        },
      }),
    ]);
    await render(<ConnectorCallStack />);
    expect(heading()).toBe(
      "Ask wants to look something up in <b>Acme</b> · **Search** [now](https://evil.example)",
    );
    expect(card()!.textContent).toContain("Sends to <img src=x onerror=alert(1)>");
    expect(card()!.querySelector("b, i, img, a, script, strong em")).toBeNull();
    expect(field("<i>Query</i> (optional)")!.value).toBe("<script>alert(1)</script>");
  });
});

describe("write card", () => {
  it("marks a change (not by colour alone) and has no Always allow", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([write()]);
    await render(<ConnectorCallStack />);
    expect(card()!.getAttribute("data-kind")).toBe("write");
    expect(heading()).toBe("Ask wants to change something in Acme CRM · Delete contact");
    expect(card()!.textContent).toContain("Can change data");
    expect(card()!.textContent).toContain("Destructive");
    expect(button("Always allow this tool…")).toBeNull();
    expect(button("Allow once")).toBeNull();
    expect(button("Review and send…")!.disabled).toBe(true);
    await arm();
    await click(button("Review and send…"));
    expect(decide).toHaveBeenLastCalledWith("w1", "allow_once", null);
    expect(host!.textContent).toMatch(/Allowed once · \d{1,2}:\d\d [AP]M/);
  });

  it("a no in the native confirm keeps it open, quietly", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([write({ destructive: false })]);
    decide.mockResolvedValueOnce({ ok: false, error: null });
    await render(<ConnectorCallStack />);
    expect(card()!.textContent).not.toContain("Destructive");
    await arm();
    await click(button("Review and send…"));
    expect(card()).not.toBeNull();
    expect(card()!.querySelector('[role="alert"]')).toBeNull();
  });
});

describe("useConnectorProposals", () => {
  it("follows proposal and settled events, without duplicates", async () => {
    await render(<ConnectorCallStack />);
    expect(card()).toBeNull();
    await emit(CONNECTORS_PROPOSAL_EVENT, proposal());
    expect(card()!.getAttribute("data-proposal-id")).toBe("p1");
    await emit(CONNECTORS_PROPOSAL_EVENT, proposal({ proposalId: "p2" }));
    await emit(CONNECTORS_PROPOSAL_EVENT, proposal());
    expect(host!.querySelectorAll("[data-proposal-id]")).toHaveLength(1);
    expect(host!.textContent).toContain("1 more call waiting");
    await emit(CONNECTORS_SETTLED_EVENT, { proposalId: "p1", outcome: "withdrawn" });
    expect(card()!.getAttribute("data-proposal-id")).toBe("p2");
    await emit(CONNECTORS_SETTLED_EVENT, { proposalId: "p2", outcome: "timed_out" });
    expect(card()).toBeNull();
    await emit(CONNECTORS_PROPOSAL_EVENT, proposal());
    expect(card()).toBeNull();
  });

  it("filters by Ask turn", async () => {
    vi.mocked(api.pendingConnectorCalls).mockResolvedValue([
      proposal({ proposalId: "p1", turnId: "t1" }),
      proposal({ proposalId: "p2", turnId: "t2" }),
    ]);
    await render(<ConnectorCallStack turnId="t2" />);
    expect(card()!.getAttribute("data-proposal-id")).toBe("p2");
    expect(host!.textContent).not.toContain("more call");
  });

  it("a pending list read before a settle can't bring the card back", async () => {
    let resolvePending: (list: ConnectorCallProposal[]) => void = () => {};
    vi.mocked(api.pendingConnectorCalls).mockImplementation(
      () =>
        new Promise((r) => {
          resolvePending = r;
        }),
    );
    await render(<ConnectorCallStack />);
    await emit(CONNECTORS_SETTLED_EVENT, { proposalId: "p1", outcome: "declined" });
    await act(async () => resolvePending([proposal()]));
    await settle();
    expect(card()).toBeNull();
  });

  it("listens before it reads what's pending, and stops on unmount", async () => {
    const order: string[] = [];
    const { listen } = await import("@tauri-apps/api/event");
    vi.mocked(listen).mockClear();
    vi.mocked(api.pendingConnectorCalls).mockImplementation(async () => {
      order.push(`pending after ${vi.mocked(listen).mock.calls.length} listens`);
      return [];
    });
    await render(<ConnectorCallStack />);
    expect(order).toEqual(["pending after 2 listens"]);
    act(() => root!.unmount());
    root = null;
    await settle();
    expect(eventHandlers[CONNECTORS_PROPOSAL_EVENT]).toBeUndefined();
    expect(eventHandlers[CONNECTORS_SETTLED_EVENT]).toBeUndefined();
  });
});
