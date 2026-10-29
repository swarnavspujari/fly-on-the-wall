// @vitest-environment jsdom
// Drives Settings › Connectors (beta) against the dev mock backend
// (devMock.ts): the fake "Acme CRM" (web, OAuth) and "Local docs" (local
// program). Where the backend asks in a native dialog, the mock asks with the
// browser's confirm, so a test answers it by stubbing window.confirm.
import { resetMockAppSettings, resetMockConnectors } from "../devMock";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import { api } from "../api";
import type { Connector, ConnectorsOverview } from "../types";
import { ConfirmProvider } from "./confirm";
import { confirmChoice, confirmDialog, confirmTitle } from "./confirmTesting";
import { ConnectorCallStack } from "./ConnectorCallCard";
import ConnectorsSection from "./ConnectorsSection";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;
/** The mock's stand-in for the backend's native dialogs. Unanswered, it declines. */
let nativeDialog: MockInstance<(message?: string) => boolean>;

const flush = async () => {
  for (let i = 0; i < 6; i++) await act(async () => {});
};

async function mount() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <ConfirmProvider>
        <ConnectorsSection />
        <ConnectorCallStack />
      </ConfirmProvider>,
    ),
  );
  await flush();
}

async function betaOn() {
  await api.setAppSetting("ui.beta.connectors", "1");
}

const within = (el: Element | null) => {
  if (!el) throw new Error("container not found");
  return {
    el: el as HTMLElement,
    button: (text: string) =>
      [...el.querySelectorAll<HTMLButtonElement>("button")].find(
        (b) => b.textContent?.trim() === text,
      ) ?? null,
    text: () => el.textContent ?? "",
  };
};
const connector = (id: string) => within(host!.querySelector(`[data-connector-id="${id}"]`));
const tool = (name: string) => within(host!.querySelector(`[data-tool="${name}"]`));
const onSwitch = (name: string) =>
  host!.querySelector<HTMLButtonElement>(`[data-tool="${name}"] [role="switch"]`)!;
const readOnlyBox = (name: string) =>
  host!.querySelector<HTMLInputElement>(`[data-tool="${name}"] input[type="checkbox"]`)!;
const card = () => host!.querySelector<HTMLElement>('[role="alertdialog"]');
/** A tool's badges: kind, then Destructive (if so), then status. */
const badges = (name: string) =>
  [...host!.querySelectorAll(`[data-tool="${name}"] .fly-badge`)].map((b) => b.textContent);
const byText = (text: string) => within(host!).button(text);
const betaSwitch = () =>
  host!.querySelector<HTMLButtonElement>('[aria-label="Let Ask use connectors (beta)"]')!;
const inputByLabel = (label: string) => {
  const l = [...host!.querySelectorAll("label")].find((x) => x.textContent === label);
  if (!l) throw new Error(`no field ${label}`);
  return document.getElementById(l.getAttribute("for")!) as HTMLInputElement;
};

async function click(el: Element | null) {
  if (!el) throw new Error("element not found");
  await act(async () => (el as HTMLElement).click());
  await flush();
}

async function type(el: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const proto =
    el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")!.set!;
  await act(async () => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
}

beforeEach(() => {
  vi.restoreAllMocks();
  resetMockAppSettings();
  resetMockConnectors();
  nativeDialog = vi.spyOn(window, "confirm").mockReturnValue(false);
});

afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
  host = null;
});

describe("Settings › Connectors (beta)", () => {
  it("is off at first; the switch writes only the beta flag", async () => {
    await mount();
    expect(host!.textContent).toContain("Every call is shown to you first");
    expect(host!.textContent).toContain("Notes you keep local never use connectors.");
    expect(betaSwitch().getAttribute("aria-checked")).toBe("false");
    expect(host!.textContent).not.toContain("Acme CRM");
    expect(host!.textContent).not.toContain("Add a connector");

    const set = vi.spyOn(api, "setAppSetting");
    await click(betaSwitch());
    expect(set).toHaveBeenLastCalledWith("ui.beta.connectors", "1");
    expect(betaSwitch().getAttribute("aria-checked")).toBe("true");
    expect(host!.textContent).toContain("Acme CRM");
    expect(host!.textContent).toContain("Local docs");
    expect(host!.textContent).toContain("Call log");

    await click(betaSwitch());
    expect(set).toHaveBeenLastCalledWith("ui.beta.connectors", "0");
    expect(host!.textContent).not.toContain("Acme CRM");
    expect(set.mock.calls.every(([key]) => key === "ui.beta.connectors")).toBe(true);
    expect(nativeDialog).not.toHaveBeenCalled();
  });

  it("the mock refuses connectors.* settings, like the backend", async () => {
    await expect(api.setAppSetting("connectors.servers", "[]")).rejects.toContain(
      "can't be changed from the app UI",
    );
  });

  it("shows each connector, its sign-in, variables and tools", async () => {
    await betaOn();
    await mount();
    const acme = connector("c-acme");
    expect(acme.text()).toContain("mcp.acme-crm.example");
    expect(acme.text()).toContain("Approved · config 3f9a1c0b7e21");
    expect(acme.text()).toContain("Not signed in");
    expect(acme.text()).toContain("Asks for: contacts.read deals.write");
    expect(acme.button("Sign in")).not.toBeNull();

    const search = tool("search_contacts");
    expect(search.text()).toContain("Search contacts");
    expect(badges("search_contacts")).toEqual(["Read", "OK"]);
    expect(search.text()).toContain("pinned 4c1f9e02ab37");
    expect(onSwitch("search_contacts").getAttribute("aria-checked")).toBe("true");
    expect(readOnlyBox("search_contacts").checked).toBe(true);
    expect(search.button("Try a tool")).not.toBeNull();

    expect(badges("update_deal_stage")).toEqual(["May change data", "OK"]);
    expect(readOnlyBox("update_deal_stage").disabled).toBe(true);
    expect(tool("update_deal_stage").text()).toContain("The server says this tool can change data");
    expect(badges("delete_contact")).toEqual(["May change data", "Destructive", "OK"]);
    expect(tool("get_deal").button("Try a tool")).toBeNull(); // off

    expect(tool("log_meeting_note").text()).toContain("Changed — review");
    expect(onSwitch("log_meeting_note").disabled).toBe(true);
    expect(tool("export_everything").text()).toContain(
      "Can't be used: its input schema isn't valid JSON Schema",
    );
    expect(onSwitch("export_everything").disabled).toBe(true);
    expect(tool("legacy_report").text()).toContain("No longer offered");

    const docs = connector("c-docs");
    expect(docs.text()).toContain("local program docs-mcp");
    expect(docs.text()).not.toContain("Sign-in");
    expect(docs.text()).toContain("DOCS_TOKEN");
    expect(docs.text()).toContain("Not set");
    expect(docs.text()).toContain("None listed yet");
  });

  it("turning a tool on is a draft; Save asks natively, and a decline is quiet", async () => {
    await betaOn();
    await mount();
    const save = vi.spyOn(api, "saveConnectorTools");
    await click(onSwitch("get_deal"));
    const acme = connector("c-acme");
    expect(acme.text()).toContain("1 unsaved tool change");
    expect(acme.text()).toContain("You'll confirm in a system dialog.");
    expect(save).not.toHaveBeenCalled();

    // declined in the native dialog: nothing saved, the draft stays
    await click(acme.button("Save"));
    expect(save).toHaveBeenLastCalledWith("c-acme", [
      { tool: "get_deal", enabled: true, readOnly: false },
    ]);
    expect(nativeDialog).toHaveBeenCalledTimes(1);
    expect(String(nativeDialog.mock.lastCall?.[0])).toContain("get_deal");
    expect(acme.text()).toContain("Not saved.");
    expect(acme.el.querySelector('[role="alert"]')).toBeNull();
    expect(onSwitch("get_deal").getAttribute("aria-checked")).toBe("true");
    expect(acme.text()).toContain("1 unsaved tool change");

    nativeDialog.mockReturnValueOnce(true);
    await click(acme.button("Save"));
    expect(acme.text()).toContain("Saved.");
    expect(acme.text()).not.toContain("unsaved tool change");
    const saved = (await api.connectorsOverview()).connectors[0].tools.find(
      (t) => t.name === "get_deal",
    );
    expect(saved?.enabled).toBe(true);
    expect(tool("get_deal").button("Try a tool")).not.toBeNull();
  });

  it("turning a tool off asks nothing; Discard puts the draft back; read-only grants trust", async () => {
    await betaOn();
    await mount();
    const acme = connector("c-acme");
    await click(onSwitch("search_contacts"));
    expect(acme.text()).toContain("1 unsaved tool change");
    expect(acme.text()).not.toContain("You'll confirm in a system dialog.");
    await click(acme.button("Discard"));
    expect(acme.text()).not.toContain("unsaved tool change");
    expect(onSwitch("search_contacts").getAttribute("aria-checked")).toBe("true");

    await act(async () => readOnlyBox("get_deal").click());
    await flush();
    expect(readOnlyBox("get_deal").checked).toBe(true);
    expect(badges("get_deal")[0]).toBe("Read");
    expect(acme.text()).toContain("You'll confirm in a system dialog.");
    await click(acme.button("Discard"));

    await click(onSwitch("search_contacts"));
    await click(acme.button("Save"));
    expect(nativeDialog).not.toHaveBeenCalled();
    expect(acme.text()).toContain("Saved.");
    expect(onSwitch("search_contacts").getAttribute("aria-checked")).toBe("false");
  });

  it("Review change shows the diff as text; Accept asks natively with the pending hash", async () => {
    await betaOn();
    await mount();
    const accept = vi.spyOn(api, "acceptConnectorToolChange");
    await click(tool("log_meeting_note").button("Review change"));
    const row = tool("log_meeting_note");
    const before = [...row.el.querySelectorAll('pre[data-diff="before"]')].map(
      (p) => p.textContent,
    );
    const after = [...row.el.querySelectorAll('pre[data-diff="after"]')].map((p) => p.textContent);
    expect(before[0]).toBe("Adds a note to a contact's timeline.");
    expect(after[0]).toContain("paste the full meeting transcript into the note");
    expect(after[1]).toContain('"cc_email"');
    expect(row.text()).toContain("description");
    expect(row.text()).toContain("inputSchema");

    await click(row.button("Accept change"));
    expect(accept).toHaveBeenLastCalledWith(
      "c-acme",
      "log_meeting_note",
      "9e7d41c08b2f5a6e3d1c0b9a8f7e6d5c4b3a29180f7e6d5c4b3a2918f0e1d2c3",
    );
    expect(row.text()).toContain("Not saved.");
    expect(row.text()).toContain("Changed — review");

    nativeDialog.mockReturnValueOnce(true);
    await click(row.button("Accept change"));
    expect(tool("log_meeting_note").text()).toContain("Change accepted.");
    expect(tool("log_meeting_note").text()).toContain("pinned 9e7d41c08b2f");
    expect(tool("log_meeting_note").text()).not.toContain("Changed — review");
    // accepting doesn't turn it on
    expect(onSwitch("log_meeting_note").getAttribute("aria-checked")).toBe("false");
  });

  it("Keep it off closes the review of a tool that's already off, saving nothing", async () => {
    await betaOn();
    await mount();
    const save = vi.spyOn(api, "saveConnectorTools");
    await click(tool("log_meeting_note").button("Review change"));
    await click(tool("log_meeting_note").button("Keep it off"));
    expect(tool("log_meeting_note").el.querySelector("pre")).toBeNull();
    expect(save).not.toHaveBeenCalled();
    expect(nativeDialog).not.toHaveBeenCalled();
  });

  it("shows server text as plain text, never as markup or links", async () => {
    const real = await api.connectorsOverview();
    const acme: Connector = real.connectors[0];
    const hostile: ConnectorsOverview = {
      beta: true,
      connectors: [
        {
          ...acme,
          tools: [
            {
              ...acme.tools[4],
              name: "<b>tool</b>",
              title: "<img src=x onerror=alert(1)>",
              description: "**Bold** [click](https://evil.example) <script>alert(1)</script>",
              diff: [{ field: "<i>f</i>", before: "<a href=x>a</a>", after: "<strong>s</strong>" }],
            },
          ],
        },
      ],
    };
    vi.spyOn(api, "connectorsOverview").mockResolvedValue(hostile);
    await mount();
    await click(byText("Review change"));
    expect(host!.querySelector("img, script, a, b, i, strong")).toBeNull();
    expect(host!.textContent).toContain("<img src=x onerror=alert(1)>");
    expect(host!.textContent).toContain("**Bold** [click](https://evil.example)");
    expect(host!.querySelector('pre[data-diff="before"]')!.textContent).toBe("<a href=x>a</a>");
  });

  it("Remove asks in-app first and says the keychain entries go too", async () => {
    await betaOn();
    await mount();
    const remove = vi.spyOn(api, "removeConnector");
    await click(connector("c-docs").button("Remove"));
    expect(confirmTitle()).toBe("Remove “Local docs”?");
    expect(confirmDialog()!.textContent).toContain("system keychain");
    await act(async () => confirmChoice(false).click());
    await flush();
    expect(remove).not.toHaveBeenCalled();

    await click(connector("c-docs").button("Remove"));
    await act(async () => confirmChoice(true).click());
    await flush();
    expect(remove).toHaveBeenCalledWith("c-docs");
    expect(host!.querySelector('[data-connector-id="c-docs"]')).toBeNull();
    expect(nativeDialog).not.toHaveBeenCalled();
  });

  it("Add a connector checks the form, asks natively, then lists the new tools", async () => {
    await betaOn();
    await mount();
    const add = vi.spyOn(api, "addConnector");
    const sync = vi.spyOn(api, "syncConnectorTools");
    await click(byText("Add a connector"));
    await type(inputByLabel("Name"), "Team wiki");
    await type(inputByLabel("Web address"), "http://wiki.example.com/mcp");
    await click(byText("Add connector"));
    expect(host!.textContent).toContain("Use an https:// address.");
    expect(add).not.toHaveBeenCalled();

    await type(inputByLabel("Web address"), "https://wiki.example.com/mcp");
    await click(byText("Add connector"));
    expect(add).toHaveBeenLastCalledWith({
      name: "Team wiki",
      transport: { kind: "http", url: "https://wiki.example.com/mcp" },
      auth: { kind: "none" },
      clientSecret: null,
    });
    expect(String(nativeDialog.mock.lastCall?.[0])).toContain("Team wiki");
    expect(host!.textContent).toContain("Not saved.");
    expect(sync).not.toHaveBeenCalled();

    nativeDialog.mockReturnValueOnce(true);
    await click(byText("Add connector"));
    expect(sync).toHaveBeenCalledTimes(1);
    const id = sync.mock.calls[0][0];
    expect(host!.textContent).toContain("Added Team wiki. Its tools start off");
    const wiki = connector(id);
    expect(wiki.text()).toContain("wiki.example.com");
    expect(wiki.text()).toContain("Search");
    expect(
      [...wiki.el.querySelectorAll('[role="switch"]')].map((s) => s.getAttribute("aria-checked")),
    ).toEqual(["false", "false"]);
  });

  it("Add a local program: arguments and variable names are checked", async () => {
    await betaOn();
    await mount();
    const add = vi.spyOn(api, "addConnector");
    await click(byText("Add a connector"));
    await click(within(host!.querySelector('[aria-label="Type"]')).button("Local program"));
    await type(inputByLabel("Name"), "Notes folder");
    await type(inputByLabel("Program"), "notes-mcp");
    await type(inputByLabel("Arguments (optional)"), '--dir "My Notes"');
    await type(inputByLabel("Environment variable names (optional)"), "NOTES_TOKEN, 9bad");
    await click(byText("Add connector"));
    expect(host!.textContent).toContain("Not a valid variable name: 9bad");
    expect(add).not.toHaveBeenCalled();

    await type(inputByLabel("Environment variable names (optional)"), "NOTES_TOKEN");
    nativeDialog.mockReturnValueOnce(true);
    await click(byText("Add connector"));
    expect(add).toHaveBeenLastCalledWith({
      name: "Notes folder",
      transport: {
        kind: "stdio",
        command: "notes-mcp",
        args: ["--dir", "My Notes"],
        envNames: ["NOTES_TOKEN"],
      },
      auth: { kind: "none" },
      clientSecret: null,
    });
    expect(host!.textContent).toContain("local program notes-mcp");
    expect(host!.textContent).toContain("NOTES_TOKEN");
  });

  it("a local program's variable: a password field, set or not set, never shown", async () => {
    await betaOn();
    await mount();
    const setEnv = vi.spyOn(api, "setConnectorEnv");
    const field = host!.querySelector<HTMLInputElement>('[aria-label="Value for DOCS_TOKEN"]')!;
    expect(field.type).toBe("password");
    const docs = connector("c-docs");
    expect(docs.button("Save")!.disabled).toBe(true);
    await type(field, "tok-123");
    await click(docs.button("Save"));
    expect(setEnv).toHaveBeenLastCalledWith("c-docs", "DOCS_TOKEN", "tok-123");
    expect(docs.text()).toContain("Set");
    expect(docs.text()).not.toContain("Not set");
    expect(field.value).toBe("");
    expect(host!.innerHTML).not.toContain("tok-123");

    await click(docs.button("Clear"));
    expect(setEnv).toHaveBeenLastCalledWith("c-docs", "DOCS_TOKEN", "");
    expect(docs.text()).toContain("Not set");
  });

  it("sign-in waits for the browser, then shows signed in; sign out", async () => {
    await betaOn();
    const signedIn = { ...(await api.connectorsOverview()).connectors[0], signedIn: true };
    let finish: (c: Connector) => void = () => {};
    vi.spyOn(api, "signInConnector").mockImplementation(
      () =>
        new Promise<Connector>((r) => {
          finish = r;
        }),
    );
    await mount();
    const acme = connector("c-acme");
    await click(acme.button("Sign in"));
    expect(acme.text()).toContain("Waiting for you to sign in in your browser…");
    expect(acme.button("Refresh tools")!.disabled).toBe(true);
    await act(async () => finish(signedIn));
    await flush();
    expect(acme.text()).toContain("Signed in");
    expect(acme.text()).not.toContain("Waiting for you");

    await click(acme.button("Sign out"));
    expect(acme.text()).toContain("Not signed in");
  });

  it("Refresh tools and errors show next to the connector", async () => {
    await betaOn();
    await mount();
    await click(connector("c-acme").button("Refresh tools"));
    expect(connector("c-acme").text()).toContain("Tools are up to date.");
    vi.spyOn(api, "syncConnectorTools").mockRejectedValueOnce("couldn't reach the server");
    await click(connector("c-acme").button("Refresh tools"));
    expect(connector("c-acme").el.querySelector('[role="alert"]')!.textContent).toBe(
      "couldn't reach the server",
    );
  });

  it("Try a tool: arguments from the schema, the confirm card, a plain-text result", async () => {
    await betaOn();
    await mount();
    await click(tool("search_contacts").button("Try a tool"));
    const area = tool("search_contacts").el.querySelector("textarea")!;
    expect(area.value).toBe('{\n  "query": ""\n}');
    await type(area, '{ "query": "Example" }');
    await click(tool("search_contacts").button("Run"));

    // the call waits in the same card Ask uses
    expect(card()).not.toBeNull();
    expect(card()!.textContent).toContain("Acme CRM wants to run Search contacts");
    expect(tool("search_contacts").text()).toContain("Check the call in the card that opened");
    await click(within(card()).button("Run"));
    expect(card()).toBeNull();
    const result = tool("search_contacts").el.querySelector("pre[data-result]")!;
    expect(result.getAttribute("data-result")).toBe("ok");
    expect(result.textContent).toContain('Found 2 contacts for "Example"');
    expect(result.textContent).toContain("dana@example.com");

    // Don't run: nothing runs, and the tester says so
    await click(tool("search_contacts").button("Run"));
    await click(within(card()).button("Don't run"));
    expect(
      tool("search_contacts").el.querySelector("pre[data-result]")!.getAttribute("data-result"),
    ).toBe("error");
    expect(tool("search_contacts").text()).toContain("You chose not to run it.");
  });

  it("Try a write tool: the card notes the second check, which the mock asks natively", async () => {
    await betaOn();
    await mount();
    await click(tool("update_deal_stage").button("Try a tool"));
    await type(
      tool("update_deal_stage").el.querySelector("textarea")!,
      '{ "deal_id": "D-1042", "stage": "won" }',
    );
    await click(tool("update_deal_stage").button("Run"));
    expect(card()!.textContent).toContain("May change data");
    expect(card()!.textContent).toContain("You'll confirm once more in a system dialog.");
    await click(within(card()).button("Run"));
    expect(nativeDialog).toHaveBeenCalledTimes(1);
    expect(card()).toBeNull();
    // declined at the native check: it didn't run
    expect(tool("update_deal_stage").text()).toContain("You chose not to run it.");
  });

  it("call log: newest first, plain labels, no arguments; Clear asks first", async () => {
    await betaOn();
    await mount();
    const log = within(
      [...host!.querySelectorAll(".fly-srow")].find((r) => r.textContent?.startsWith("Call log")) ??
        null,
    );
    await click(log.button("Show"));
    const rows = [...log.el.querySelectorAll("tr[data-log-row]")].map((r) =>
      [...r.querySelectorAll("td")].map((td) => td.textContent),
    );
    expect(rows).toHaveLength(4);
    expect(rows[0]).toEqual([
      "10 min ago",
      "Acme CRM",
      "search_contacts edited",
      "Read",
      "Ran",
      "1.2 s",
      "12,000 chars, cut",
    ]);
    expect(rows[1][4]).toBe("Refused: the tool isn't turned on");
    expect(rows[2]).toEqual([
      "2 h ago",
      "Acme CRM",
      "update_deal_stage",
      "May change data",
      "You didn't run it",
      "0 ms",
      "—",
    ]);
    expect(rows[3][6]).toBe("1,532 chars");

    await click(log.button("Clear log"));
    expect(confirmTitle()).toBe("Clear the call log?");
    await act(async () => confirmChoice(false).click());
    await flush();
    expect(log.el.querySelectorAll("tr[data-log-row]")).toHaveLength(4);
    await click(log.button("Clear log"));
    await act(async () => confirmChoice(true).click());
    await flush();
    expect(log.text()).toContain("No calls yet.");
    expect(await api.connectorCallLog()).toEqual([]);
  });
});
