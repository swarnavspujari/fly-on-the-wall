// @vitest-environment jsdom
import { act } from "react";
import type { ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { api, CONNECTORS_PROPOSAL_EVENT, CONNECTORS_SETTLED_EVENT } from "../api";
import type { ConnectorCallProposal, ConnectorDecideResult } from "../types";
import { ConnectorCallStack } from "./ConnectorCallCard";

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
  inputSchema: { type: "object", required: ["query"] },
  ...over,
});

let root: Root | null = null;
let host: HTMLElement | null = null;
let decide: Mock<typeof api.decideConnectorCall>;

const flush = async () => {
  for (let i = 0; i < 5; i++) await act(async () => {});
};

async function render(ui: ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(ui));
  await flush();
}

async function emit(event: string, payload: unknown) {
  await act(async () => eventHandlers[event]?.({ payload }));
  await flush();
}

const card = () => host!.querySelector<HTMLElement>('[role="alertdialog"]');
const heading = () => {
  const id = card()?.getAttribute("aria-labelledby");
  return id ? document.getElementById(id)?.textContent : null;
};
const button = (text: string) =>
  [...host!.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent?.trim() === text,
  ) ?? null;
const textarea = () => card()!.querySelector("textarea")!;

async function click(el: HTMLElement | null) {
  if (!el) throw new Error("element not found");
  await act(async () => el.click());
  await flush();
}

async function typeArgs(value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
  await act(async () => {
    setter.call(textarea(), value);
    textarea().dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
}

beforeEach(() => {
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
});

describe("connector call card", () => {
  it("shows what will run, where, and starts on Don't run", async () => {
    vi.spyOn(api, "pendingConnectorCalls").mockResolvedValue([proposal()]);
    await render(<ConnectorCallStack />);
    expect(heading()).toBe("Acme CRM wants to run Search contacts");
    expect(card()!.textContent).toContain("tool: search_contacts");
    const subId = card()!.getAttribute("aria-describedby")!;
    expect(document.getElementById(subId)!.textContent).toBe("Sends to mcp.acme-crm.example");
    expect(card()!.textContent).toContain("Read");
    expect(card()!.textContent).not.toContain("Destructive");
    expect(card()!.textContent).not.toContain("system dialog");
    expect(textarea().value).toBe('{\n  "query": "Example Co"\n}');
    expect(document.activeElement).toBe(button("Don't run"));
    expect(decide).not.toHaveBeenCalled();
  });

  it("shows server and model text as plain text only", async () => {
    vi.spyOn(api, "pendingConnectorCalls").mockResolvedValue([
      proposal({
        serverName: "<b>Acme</b>",
        toolTitle: "**Search** [now](https://evil.example)",
        destination: "<img src=x onerror=alert(1)>",
        args: { q: "<script>alert(1)</script>" },
      }),
    ]);
    await render(<ConnectorCallStack />);
    expect(heading()).toBe("<b>Acme</b> wants to run **Search** [now](https://evil.example)");
    expect(card()!.textContent).toContain("Sends to <img src=x onerror=alert(1)>");
    expect(card()!.querySelector("b, img, a, script, strong")).toBeNull();
    expect(textarea().value).toContain("<script>alert(1)</script>");
  });

  it("warns about writes and destructive tools", async () => {
    vi.spyOn(api, "pendingConnectorCalls").mockResolvedValue([
      proposal({ kind: "write", destructive: true, toolTitle: null, tool: "delete_contact" }),
    ]);
    await render(<ConnectorCallStack />);
    expect(heading()).toBe("Acme CRM wants to run delete_contact");
    expect(card()!.textContent).not.toContain("tool: ");
    expect(card()!.textContent).toContain("May change data");
    expect(card()!.querySelector('[role="note"]')!.textContent).toContain("Destructive");
    expect(card()!.textContent).toContain("You'll confirm once more in a system dialog.");
  });

  it("runs unchanged arguments as null and edited ones as the parsed object", async () => {
    vi.spyOn(api, "pendingConnectorCalls").mockResolvedValue([
      proposal(),
      proposal({ proposalId: "p2" }),
    ]);
    await render(<ConnectorCallStack />);
    expect(host!.textContent).toContain("1 more call waiting");
    await click(button("Run"));
    expect(decide).toHaveBeenLastCalledWith("p1", true, null);
    // the next card takes its place
    expect(card()!.getAttribute("data-proposal-id")).toBe("p2");
    expect(host!.textContent).not.toContain("more call");

    await typeArgs('{ "query": "Dana" }');
    expect(button("Undo edits")).not.toBeNull();
    await click(button("Run"));
    expect(decide).toHaveBeenLastCalledWith("p2", true, { query: "Dana" });
    expect(card()).toBeNull();
  });

  it("reformatted but equal arguments still count as unchanged", async () => {
    vi.spyOn(api, "pendingConnectorCalls").mockResolvedValue([proposal()]);
    await render(<ConnectorCallStack />);
    await typeArgs('{"query":"Example Co"}');
    await click(button("Run"));
    expect(decide).toHaveBeenLastCalledWith("p1", true, null);
  });

  it("won't run invalid JSON, and keeps the card open on the backend's error", async () => {
    vi.spyOn(api, "pendingConnectorCalls").mockResolvedValue([proposal()]);
    await render(<ConnectorCallStack />);
    await typeArgs("{");
    expect(card()!.textContent).toContain("That isn't valid JSON");
    expect(textarea().getAttribute("aria-invalid")).toBe("true");
    expect(button("Run")!.disabled).toBe(true);
    await typeArgs("[1, 2]");
    expect(card()!.textContent).toContain("Arguments must be a JSON object");
    expect(button("Run")!.disabled).toBe(true);

    decide.mockResolvedValueOnce({ ok: false, error: "missing required argument: query" });
    await typeArgs("{}");
    await click(button("Run"));
    expect(decide).toHaveBeenLastCalledWith("p1", true, {});
    expect(card()).not.toBeNull();
    expect(card()!.querySelector('[role="alert"]')!.textContent).toBe(
      "missing required argument: query",
    );
    expect(button("Run")!.disabled).toBe(false);
    expect(button("Don't run")!.disabled).toBe(false);
  });

  it("disables both buttons while the decision is in flight", async () => {
    vi.spyOn(api, "pendingConnectorCalls").mockResolvedValue([proposal({ kind: "write" })]);
    let answer: (r: ConnectorDecideResult) => void = () => {};
    decide.mockImplementationOnce(
      () =>
        new Promise<ConnectorDecideResult>((r) => {
          answer = r;
        }),
    );
    await render(<ConnectorCallStack />);
    await click(button("Run"));
    expect(button("Running…")!.disabled).toBe(true);
    expect(button("Don't run")!.disabled).toBe(true);
    // Esc does nothing while the backend's dialog is up
    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    expect(decide).toHaveBeenCalledTimes(1);
    await act(async () => answer({ ok: true, error: null }));
    await flush();
    expect(card()).toBeNull();
  });

  it("Don't run and Esc decline; a decline that fails still closes the card", async () => {
    vi.spyOn(api, "pendingConnectorCalls").mockResolvedValue([
      proposal(),
      proposal({ proposalId: "p2" }),
      proposal({ proposalId: "p3" }),
    ]);
    await render(<ConnectorCallStack />);
    await click(button("Don't run"));
    expect(decide).toHaveBeenLastCalledWith("p1", false, null);

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    await flush();
    expect(decide).toHaveBeenLastCalledWith("p2", false, null);

    decide.mockRejectedValueOnce("That call is no longer waiting for an answer.");
    await click(button("Don't run"));
    expect(decide).toHaveBeenLastCalledWith("p3", false, null);
    expect(card()).toBeNull();
  });

  it("a Run that fails shows the error and keeps the card", async () => {
    vi.spyOn(api, "pendingConnectorCalls").mockResolvedValue([proposal()]);
    decide.mockRejectedValueOnce("connector unavailable");
    await render(<ConnectorCallStack />);
    await click(button("Run"));
    expect(card()!.querySelector('[role="alert"]')!.textContent).toBe("connector unavailable");
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
    expect(host!.querySelectorAll('[role="alertdialog"]')).toHaveLength(1);
    expect(host!.textContent).toContain("1 more call waiting");
    await emit(CONNECTORS_SETTLED_EVENT, { proposalId: "p1", outcome: "withdrawn" });
    expect(card()!.getAttribute("data-proposal-id")).toBe("p2");
    await emit(CONNECTORS_SETTLED_EVENT, { proposalId: "p2", outcome: "timed_out" });
    expect(card()).toBeNull();
    // a settled proposal never comes back
    await emit(CONNECTORS_PROPOSAL_EVENT, proposal());
    expect(card()).toBeNull();
  });

  it("filters by Ask turn", async () => {
    vi.spyOn(api, "pendingConnectorCalls").mockResolvedValue([
      proposal({ proposalId: "p1", turnId: "t1" }),
      proposal({ proposalId: "p2", turnId: "t2" }),
    ]);
    await render(<ConnectorCallStack turnId="t2" />);
    expect(card()!.getAttribute("data-proposal-id")).toBe("p2");
    expect(host!.textContent).not.toContain("more call");
  });

  it("a pending list read before a settle can't bring the card back", async () => {
    let resolvePending: (list: ConnectorCallProposal[]) => void = () => {};
    vi.spyOn(api, "pendingConnectorCalls").mockImplementation(
      () =>
        new Promise((r) => {
          resolvePending = r;
        }),
    );
    await render(<ConnectorCallStack />);
    await emit(CONNECTORS_SETTLED_EVENT, { proposalId: "p1", outcome: "declined" });
    await act(async () => resolvePending([proposal()]));
    await flush();
    expect(card()).toBeNull();
  });

  it("listens before it reads what's pending, and stops on unmount", async () => {
    const order: string[] = [];
    const { listen } = await import("@tauri-apps/api/event");
    vi.mocked(listen).mockClear();
    vi.spyOn(api, "pendingConnectorCalls").mockImplementation(async () => {
      order.push(`pending after ${vi.mocked(listen).mock.calls.length} listens`);
      return [];
    });
    await render(<ConnectorCallStack />);
    expect(order).toEqual(["pending after 2 listens"]);
    expect(eventHandlers[CONNECTORS_PROPOSAL_EVENT]).toBeDefined();
    act(() => root!.unmount());
    root = null;
    await flush();
    expect(eventHandlers[CONNECTORS_PROPOSAL_EVENT]).toBeUndefined();
    expect(eventHandlers[CONNECTORS_SETTLED_EVENT]).toBeUndefined();
  });
});
