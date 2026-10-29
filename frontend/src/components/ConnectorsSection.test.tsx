// @vitest-environment jsdom
// Settings › Connectors (list and detail) against the dev mock backend
// (devMock.ts): "Acme CRM" (web, OAuth, two tools to review), "Docs search"
// (local) and "Team wiki" (web, API key, off). Where the backend shows a
// native dialog, the mock asks with the browser's confirm, so a test answers
// it by stubbing window.confirm.
import { resetMockAppSettings, resetMockConnectors } from "../devMock";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import { api } from "../api";
import type { Connector } from "../types";
import { confirmChoice, confirmDialog, confirmTitle } from "./confirmTesting";
import { ARM_DELAY_MS } from "./ConnectorCallCard";
import {
  buttonIn,
  click,
  fieldIn,
  flush,
  liveText,
  mountSection,
  pickMenu,
  type,
  type Mounted,
  unmount,
} from "./connectorsTesting";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
// Whole screens against the mock; the 5 s default is tight when the suite
// runs alongside other work.
vi.setConfig({ testTimeout: 30_000 });

let m: Mounted | null = null;
let nativeDialog: MockInstance<(message?: string) => boolean>;
const host = () => m!.host;
const btn = (text: string) => buttonIn(host(), text);
const row = (id: string) => host().querySelector<HTMLElement>(`[data-connector-row="${id}"]`);
const toolRow = (name: string) => host().querySelector<HTMLElement>(`[data-tool="${name}"]`)!;
const radio = (name: string, label: string) => buttonIn(toolRow(name), label) as HTMLButtonElement;
const checkedIn = (name: string) =>
  toolRow(name).querySelector('[role="radio"][aria-checked="true"]')?.textContent;

async function betaOn() {
  await api.setAppSetting("ui.beta.connectors", "1");
}
async function openDetail(id: string) {
  await click(row(id)!.querySelector("[data-open-row]"));
}

beforeEach(() => {
  vi.restoreAllMocks();
  resetMockAppSettings();
  resetMockConnectors();
  nativeDialog = vi.spyOn(window, "confirm").mockReturnValue(false);
});

afterEach(() => {
  unmount(m);
  m = null;
});

describe("Settings › Connectors: list", () => {
  it("is off at first; the switch writes only the beta flag", async () => {
    m = await mountSection();
    expect(host().textContent).toContain(
      "Let Ask and Enhance check other apps and, if you allow it, make changes there.",
    );
    expect(host().textContent).toContain(
      "Connectors use MCP, an open standard for linking AI to apps.",
    );
    expect(host().querySelector("a")!.textContent).toBe("Learn more");
    const sw = host().querySelector<HTMLButtonElement>(
      '[aria-label="Let Ask use connectors (beta)"]',
    )!;
    expect(sw.getAttribute("aria-checked")).toBe("false");
    expect(host().textContent).not.toContain("Acme CRM");

    const set = vi.spyOn(api, "setAppSetting");
    await click(sw);
    expect(set).toHaveBeenLastCalledWith("ui.beta.connectors", "1");
    expect(host().textContent).toContain("Acme CRM");
    await click(sw);
    expect(set).toHaveBeenLastCalledWith("ui.beta.connectors", "0");
    expect(host().textContent).not.toContain("Acme CRM");
    expect(set.mock.calls.every(([key]) => key === "ui.beta.connectors")).toBe(true);
    await expect(api.setAppSetting("connectors.servers", "[]")).rejects.toContain(
      "can't be changed",
    );
  });

  it("lists each connector with a badge, a status in words and a switch", async () => {
    await betaOn();
    m = await mountSection();
    expect(row("c-acme")!.textContent).toContain("Acme CRM");
    expect(row("c-acme")!.textContent).toContain("Custom");
    expect(row("c-acme")!.textContent).toContain("2 tools changed — review");
    expect(row("c-acme")!.querySelector("svg")).not.toBeNull(); // the problem icon
    expect(row("c-docs")!.textContent).toContain("Local");
    expect(row("c-docs")!.textContent).toContain("Connected");
    expect(row("c-wiki")!.textContent).toContain("Off");
    const wiki = row("c-wiki")!.querySelector<HTMLButtonElement>('[role="switch"]')!;
    expect(wiki.getAttribute("aria-label")).toBe("Use Team wiki");
    expect(wiki.getAttribute("aria-checked")).toBe("false");
    expect(btn("Browse connectors")).not.toBeNull();
    const add = host().querySelector('[aria-haspopup="menu"]')!;
    await click(add);
    expect([...document.querySelectorAll('[role="menuitem"]')].map((i) => i.textContent)).toEqual([
      "Custom (web address)",
      "Local (program on this computer)",
      "Import from Claude Desktop",
    ]);
  });

  it("switching on asks natively (a no changes nothing); off applies at once", async () => {
    await betaOn();
    m = await mountSection();
    const sw = () => row("c-wiki")!.querySelector<HTMLButtonElement>('[role="switch"]')!;
    await click(sw());
    expect(nativeDialog).toHaveBeenCalledTimes(1);
    expect(String(nativeDialog.mock.lastCall?.[0])).toContain('Add "Team wiki" connector?');
    expect(sw().getAttribute("aria-checked")).toBe("false");
    expect(row("c-wiki")!.querySelector('[role="alert"]')).toBeNull();
    nativeDialog.mockReturnValueOnce(true);
    await click(sw());
    expect(sw().getAttribute("aria-checked")).toBe("true");
    expect(liveText(host())).toBe("Team wiki turned on");

    await click(row("c-docs")!.querySelector('[role="switch"]'));
    expect(nativeDialog).toHaveBeenCalledTimes(2);
    expect(row("c-docs")!.textContent).toContain("Off");
  });

  it("empty, loading-failed and retry states", async () => {
    await betaOn();
    const real = api.connectorsOverview;
    const spy = vi
      .spyOn(api, "connectorsOverview")
      .mockResolvedValue({ beta: true, connectors: [] });
    m = await mountSection();
    expect(host().textContent).toContain("No connectors yet.");
    expect(host().textContent).toContain("Connect an app you already use, like Notion or Linear.");
    expect(btn("Browse connectors")).not.toBeNull();
    unmount(m);

    spy.mockRejectedValue("database is locked");
    m = await mountSection();
    expect(host().textContent).toContain("Couldn't load connectors.");
    spy.mockImplementation(real);
    await click(btn("Try again"));
    expect(row("c-acme")).not.toBeNull();
  });

  it("shows server text as plain text: no markup, and no links but Learn more", async () => {
    await betaOn();
    const o = await api.connectorsOverview();
    const acme: Connector = o.connectors[0];
    acme.name = "<b>Acme</b>";
    acme.tools = acme.tools.map((t) => ({
      ...t,
      title: "<img src=x onerror=alert(1)>",
      description: "**Bold** [click](https://evil.example) <script>alert(1)</script>",
      diff: t.diff.map((d) => ({ ...d, after: "<a href=x>a</a>" })),
    }));
    vi.spyOn(api, "connectorsOverview").mockResolvedValue({ beta: true, connectors: [acme] });
    m = await mountSection();
    await openDetail("c-acme");
    await click(btn("Review"));
    expect([...host().querySelectorAll("a")].map((a) => a.textContent)).toEqual(["Learn more"]);
    expect(host().querySelector("img, script, b, strong")).toBeNull();
    expect(host().textContent).toContain("<img src=x onerror=alert(1)>");
    expect(host().textContent).toContain("**Bold** [click](https://evil.example)");
  });
});

describe("Settings › Connectors: detail", () => {
  it("opens with the status, the review banner and the Tools tab", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-acme");
    const heading = host().querySelector("h3")!;
    expect(heading.textContent).toBe("Acme CRM");
    expect(document.activeElement).toBe(heading);
    expect(host().textContent).toContain(
      "2 tools changed since you approved them. They're blocked until you review.",
    );
    const tabs = [...host().querySelectorAll('[role="tab"]')];
    expect(tabs.map((t) => [t.textContent, t.getAttribute("aria-selected")])).toEqual([
      ["About", "false"],
      ["Tools", "true"],
      ["Activity", "false"],
    ]);
    expect(host().textContent).toContain("Choose when the AI may use each tool.");
    expect(host().textContent).toContain("Read-only tools (3)");
    expect(host().textContent).toContain("Tools that can change or delete (5)");
    expect(
      host().querySelector('[aria-label="Read-only tools: Mixed"]')?.getAttribute("aria-haspopup"),
    ).toBe("menu");
    expect(toolRow("sync_notes").textContent).toContain("Not labelled — treated as a change tool");
    expect(toolRow("delete_contact").textContent).toContain("Destructive");
    expect(toolRow("list_owners").textContent).toContain("New — review");
    // a changed or new tool can only be Blocked
    expect(radio("list_owners", "Needs approval").disabled).toBe(true);
    expect(radio("list_owners", "Blocked").disabled).toBe(false);
    const group = toolRow("search_contacts").querySelector('[role="radiogroup"]')!;
    expect(group.getAttribute("aria-label")).toBe("Permission for Search contacts");
    expect(checkedIn("search_contacts")).toBe("Needs approval");

    // arrow keys move along the tabs
    await act(async () => {
      tabs[1].dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    });
    await flush();
    expect(host().querySelector('[role="tab"][aria-selected="true"]')!.textContent).toBe(
      "Activity",
    );

    await click(btn("Connectors"));
    expect(document.activeElement).toBe(row("c-acme")!.querySelector("[data-open-row]"));
  });

  it("tightening applies at once; loosening asks natively and a no keeps the old value", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-acme");
    const set = vi.spyOn(api, "setConnectorToolPermissions");
    await click(radio("search_contacts", "Blocked"));
    expect(set).toHaveBeenLastCalledWith("c-acme", [
      { tool: "search_contacts", permission: "blocked" },
    ]);
    expect(nativeDialog).not.toHaveBeenCalled();
    expect(checkedIn("search_contacts")).toBe("Blocked");

    await click(radio("search_contacts", "Always allow"));
    expect(nativeDialog).toHaveBeenCalledTimes(1);
    expect(String(nativeDialog.mock.lastCall?.[0])).toContain(
      'Let the AI use "Search contacts" without asking?',
    );
    expect(checkedIn("search_contacts")).toBe("Blocked");
    expect(host().querySelector('[role="alert"]')).toBeNull();
    expect(liveText(host())).toBe("Kept as it was.");

    nativeDialog.mockReturnValueOnce(true);
    await click(radio("search_contacts", "Always allow"));
    expect(checkedIn("search_contacts")).toBe("Always allow");
  });

  it("Always allow on a change tool warns about meetings", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-acme");
    nativeDialog.mockReturnValueOnce(true);
    await click(radio("update_deal_stage", "Always allow"));
    expect(String(nativeDialog.mock.lastCall?.[0])).toContain(
      "This can change data in Acme CRM. Something said in a meeting could steer the AI into using it.",
    );
    expect(toolRow("update_deal_stage").querySelector('[role="note"]')!.textContent).toBe(
      "Someone speaking in a meeting could word things so the AI uses this without asking you.",
    );
  });

  it("a group's menu sets every tool it can", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-acme");
    await pickMenu(host().querySelector('[aria-label="Read-only tools: Mixed"]'), "Blocked");
    expect(nativeDialog).not.toHaveBeenCalled();
    expect(checkedIn("search_contacts")).toBe("Blocked");
    expect(checkedIn("get_deal")).toBe("Blocked");
    expect(host().querySelector('[aria-label="Read-only tools: Blocked"]')).not.toBeNull();
  });

  it("Review shows what changed; Keep blocked just closes; Approve asks natively", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-acme");
    const approve = vi.spyOn(api, "approveConnectorTool");
    await click(btn("Review"));
    const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]');
    expect(dialog()!.textContent).toContain("Review changes");
    expect(dialog()!.textContent).toContain("1 of 2");
    expect(dialog()!.textContent).toContain("Acme CRM added this tool after you approved it.");
    expect(dialog()!.textContent).toContain("Lists the people who own deals");
    await click(buttonIn(dialog()!, "Keep blocked"));
    expect(dialog()).toBeNull();
    expect(approve).not.toHaveBeenCalled();

    await click(buttonIn(toolRow("log_meeting_note"), "Review"));
    expect(dialog()!.textContent).toContain("Acme CRM changed this tool since you approved it.");
    const before = [...dialog()!.querySelectorAll('pre[data-diff="before"]')].map(
      (p) => p.textContent,
    );
    const after = [...dialog()!.querySelectorAll('pre[data-diff="after"]')].map(
      (p) => p.textContent,
    );
    expect(before[0]).toBe("Adds a note to a contact's timeline.");
    expect(after[0]).toContain("paste the full meeting transcript");
    expect(dialog()!.textContent).toContain("Was:");
    await click(buttonIn(dialog()!, "Show unchanged"));
    expect(dialog()!.textContent).toContain("Name");

    await click(buttonIn(dialog()!, "Approve changes"));
    expect(approve).toHaveBeenLastCalledWith(
      "c-acme",
      "log_meeting_note",
      expect.stringMatching(/^[0-9a-f]{64}$/),
    );
    expect(dialog()).not.toBeNull(); // declined: still blocked, still here
    nativeDialog.mockReturnValueOnce(true).mockReturnValueOnce(true);
    await click(buttonIn(dialog()!, "Approve changes"));
    expect(dialog()!.textContent).toContain("List owners");
    await click(buttonIn(dialog()!, "Approve changes"));
    expect(dialog()).toBeNull();
    expect(host().textContent).not.toContain("tools changed since you approved");
    expect(checkedIn("log_meeting_note")).toBe("Needs approval");
    expect(checkedIn("list_owners")).toBe("Needs approval");
  });

  it("About shows sign-in, address and headers (never secret values)", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-wiki");
    await click(btn("About"));
    const text = host().textContent!;
    expect(text).toContain("API key or token · Saved in your keychain");
    expect(text).toContain("https://wiki.example.com/mcp");
    expect(text).toContain("X-Team: design");
    expect(text).toContain("X-Wiki-Token: Saved in your keychain");
    expect(text).toContain("Approved · config 0c7f1b2d9e44");
  });

  it("sign-in waits for the browser; Cancel only hides the wait", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-acme");
    await click(btn("About"));
    await click(btn("Sign out"));
    expect(host().textContent).toContain("Sign in to use Acme CRM.");
    let finish: (v: unknown) => void = () => {};
    const signIn = vi.spyOn(api, "signInConnector").mockImplementation(
      () =>
        new Promise((r) => {
          finish = r as (v: unknown) => void;
        }),
    );
    await click(buttonIn(host().querySelector('[role="tabpanel"]')!, "Sign in"));
    expect(signIn).toHaveBeenCalledWith("c-acme");
    expect(host().textContent).toContain("Finish signing in in your browser…");
    await click(btn("Cancel"));
    expect(host().textContent).not.toContain("Finish signing in");

    // A sign-in that finishes later still lands.
    const o = await api.connectorsOverview();
    await act(async () =>
      finish({
        saved: true,
        connector: { ...o.connectors[0], signedIn: true, status: "review" },
        error: null,
      }),
    );
    await flush();
    expect(host().textContent).not.toContain("Sign in to use Acme CRM.");
  });

  it("a sign-in error shows in words", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-acme");
    vi.spyOn(api, "signInConnector").mockRejectedValue("Sign-in was cancelled.");
    await pickMenu(host().querySelector('[aria-label="More for Acme CRM"]'), "Reconnect");
    expect(host().querySelector('[role="alert"]')!.textContent).toBe("Sign-in was cancelled.");
  });

  it("Activity: newest first, results in words, a Problems only filter", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-acme");
    await click(btn("Activity"));
    const rows = () =>
      [...host().querySelectorAll("tr[data-log-row]")].map((r) =>
        [...r.querySelectorAll("td")].slice(1).map((td) => td.textContent),
      );
    expect(rows()).toEqual([
      ["get_deal", "Allowed automatically"],
      ["search_contacts", "Failedcouldn't reach the connector"],
      ["delete_contact", "Blockedthe tool is blocked"],
      ["update_deal_stage", "Denied"],
      ["search_contacts", "Allowed by you"],
    ]);
    expect(host().textContent).toContain(
      "Only tool names and outcomes are kept — never meeting text.",
    );
    await click(host().querySelector('input[type="checkbox"]'));
    expect(rows().map((r) => r[0])).toEqual(["search_contacts", "delete_contact"]);

    // The backend asks natively (a script can't clear the record).
    await click(btn("Clear activity…"));
    expect(String(nativeDialog.mock.lastCall?.[0])).toContain("Clear connector activity?");
    expect(host().textContent).not.toContain("No calls yet.");
    nativeDialog.mockReturnValueOnce(true);
    await click(btn("Clear activity…"));
    await flush();
    expect(host().textContent).toContain("No calls yet.");
  });

  it("Disconnect asks in the app with the brief's words", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-acme");
    const remove = vi.spyOn(api, "removeConnector");
    await pickMenu(host().querySelector('[aria-label="More for Acme CRM"]'), "Disconnect");
    expect(confirmTitle()).toBe("Disconnect Acme CRM?");
    expect(confirmDialog()!.textContent).toContain("Your sign-in is removed from this computer.");
    await act(async () => confirmChoice(false).click());
    await flush();
    expect(remove).not.toHaveBeenCalled();

    await pickMenu(host().querySelector('[aria-label="More for Acme CRM"]'), "Disconnect");
    await act(async () => confirmChoice(true).click());
    await flush();
    expect(remove).toHaveBeenCalledWith("c-acme");
    expect(row("c-acme")).toBeNull();
    expect(liveText(host())).toBe("Acme CRM disconnected");
    expect(nativeDialog).not.toHaveBeenCalled();
  });

  it("Edit opens the form filled in; saved secrets show as saved, never as values", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-wiki");
    await pickMenu(host().querySelector('[aria-label="More for Team wiki"]'), "Edit");
    const dialog = document.querySelector<HTMLElement>('[role="dialog"]')!;
    expect(dialog.textContent).toContain("Edit Team wiki");
    expect(fieldIn(dialog, "Name").value).toBe("Team wiki");
    expect(fieldIn(dialog, "Web address").value).toBe("https://wiki.example.com/mcp");
    expect(
      dialog.querySelector<HTMLInputElement>('input[type="radio"]:checked')!.parentElement!
        .textContent,
    ).toContain("API key or token");
    expect(dialog.textContent).toContain("Saved in your keychain");
    expect(buttonIn(dialog, "Replace")).not.toBeNull();
    const update = vi.spyOn(api, "updateConnector");
    nativeDialog.mockReturnValueOnce(true);
    await click(buttonIn(dialog, "Save changes"));
    expect(update).toHaveBeenLastCalledWith("c-wiki", {
      config: {
        name: "Team wiki",
        transport: {
          kind: "http",
          url: "https://wiki.example.com/mcp",
          headers: [
            { name: "X-Team", value: "design" },
            { name: "X-Wiki-Token", value: null },
          ],
        },
        auth: { kind: "api_key" },
      },
      secrets: { apiKey: null, clientSecret: null, values: {} },
    });
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(host().querySelector("h3")!.textContent).toBe("Team wiki");
  });

  it("Try… runs one real call through the approval card; the result is plain text", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-acme");
    // only usable tools that aren't Blocked
    expect(buttonIn(toolRow("delete_contact"), "Try…")).toBeNull();
    expect(buttonIn(toolRow("log_meeting_note"), "Try…")).toBeNull();
    await click(buttonIn(toolRow("search_contacts"), "Try…"));
    const panel = () => toolRow("search_contacts").querySelector<HTMLElement>("[data-try]")!;
    expect(panel().textContent).toContain("It goes through the same approval card as Ask");
    const area = panel().querySelector("textarea")!;
    expect(area.value).toBe('{\n  "query": ""\n}');
    await type(area, '{ "query": "Example" }');
    await click(buttonIn(panel(), "Run"));

    const card = () => host().querySelector<HTMLElement>('[role="group"][data-proposal-id]');
    expect(card()!.textContent).toContain(
      "Ask wants to look something up in Acme CRM · Search contacts",
    );
    expect(panel().textContent).toContain("Waiting for your decision on the approval card…");
    // the card's Allow once arms after a moment
    await act(() => new Promise((r) => setTimeout(r, ARM_DELAY_MS + 50)));
    await click(buttonIn(card()!, "Allow once"));
    const result = () => panel().querySelector("pre[data-result]")!;
    expect(result().getAttribute("data-result")).toBe("ok");
    expect(result().textContent).toContain('Found 2 contacts for "Example"');

    await click(buttonIn(panel(), "Run"));
    await click(buttonIn(card()!, "Deny"));
    expect(result().getAttribute("data-result")).toBe("error");
    expect(result().textContent).toBe("You chose not to run it.");
  });

  it("Try… on an Always allow tool runs without a card", async () => {
    await betaOn();
    m = await mountSection();
    await openDetail("c-acme");
    const test = vi.spyOn(api, "testConnectorTool");
    await click(buttonIn(toolRow("get_deal"), "Try…"));
    const panel = toolRow("get_deal").querySelector<HTMLElement>("[data-try]")!;
    expect(panel.textContent).toContain("The tool is on Always allow, so it runs without asking.");
    await type(panel.querySelector("textarea")!, '{ "deal_id": "D-1042" }');
    await click(buttonIn(panel, "Run"));
    expect(test).toHaveBeenLastCalledWith("c-acme", "get_deal", { deal_id: "D-1042" });
    expect(host().querySelector('[role="group"][data-proposal-id]')).toBeNull();
    expect(panel.querySelector("pre[data-result]")!.textContent).toContain("Deal D-1042");
  });
});
