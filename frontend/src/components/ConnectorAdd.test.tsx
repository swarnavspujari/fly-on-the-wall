// @vitest-environment jsdom
// The ways to add a connector (Browse with a preview, Add custom, Add local,
// Import from Claude Desktop), driven through Settings › Connectors against
// the dev mock backend. Native dialogs are the mock's window.confirm.
import { resetMockAppSettings, resetMockConnectors } from "../devMock";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import { api } from "../api";
import type { ConnectorSaveResult } from "../types";
import {
  buttonIn,
  click,
  fieldIn,
  flush,
  liveText,
  mountSection,
  pickMenu,
  type Mounted,
  type,
  unmount,
} from "./connectorsTesting";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
// Whole screens against the mock; the 5 s default is tight when the suite
// runs alongside other work.
vi.setConfig({ testTimeout: 30_000 });

let m: Mounted | null = null;
let nativeDialog: MockInstance<(message?: string) => boolean>;
const host = () => m!.host;
const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]')!;
const inDialog = (text: string) => buttonIn(dialog(), text);
const lastDialogText = () => String(nativeDialog.mock.lastCall?.[0] ?? "");

async function open() {
  await api.setAppSetting("ui.beta.connectors", "1");
  m = await mountSection();
}
async function addMenu(item: string) {
  await pickMenu(
    [...host().querySelectorAll('[aria-haspopup="menu"]')].find((b) =>
      b.textContent?.includes("Add connector"),
    ) ?? null,
    item,
  );
}

beforeEach(() => {
  vi.restoreAllMocks();
  resetMockAppSettings();
  resetMockConnectors();
  localStorage.removeItem("fotwMockClaudeImport");
  nativeDialog = vi.spyOn(window, "confirm").mockReturnValue(false);
});

afterEach(() => {
  unmount(m);
  m = null;
});

describe("Browse connectors", () => {
  it("searches, and offers a custom connector when nothing matches", async () => {
    await open();
    await click(buttonIn(host(), "Browse connectors"));
    expect(dialog().textContent).toContain("Browse connectors");
    const cards = () => [...dialog().querySelectorAll("[data-directory-id]")];
    expect(cards()).toHaveLength(4);
    expect(cards()[0].textContent).toContain("Made by Example Notes Inc.");
    const search = dialog().querySelector<HTMLInputElement>('input[type="search"]')!;
    await type(search, "issues");
    expect(cards().map((c) => c.getAttribute("data-directory-id"))).toEqual(["sample-tracker"]);
    await type(search, "zzz");
    expect(dialog().textContent).toContain("Nothing matches ‘zzz’.");
    await click(inDialog("Add custom connector"));
    expect(dialog().textContent).toContain("Add custom connector");
  });

  it("[+] previews the tools first; Connect asks natively, then opens the detail page", async () => {
    await open();
    await click(buttonIn(host(), "Browse connectors"));
    await click(dialog().querySelector('[aria-label="Add Open Docs"]'));
    expect(dialog().textContent).toContain("Made by Open Docs Project");
    expect(dialog().textContent).toContain("Connects to mcp.open-docs.example");
    expect(dialog().textContent).toContain("Found 9 tools: 4 read-only, 5 can make changes");
    expect(dialog().textContent).toContain("Read-only tools (4)");
    expect(dialog().textContent).toContain("Tools that can change or delete (5)");
    expect(dialog().textContent).toContain("Every tool starts on Needs approval.");

    const add = vi.spyOn(api, "addConnector");
    await click(inDialog("Connect"));
    expect(add).toHaveBeenCalledTimes(1);
    expect(lastDialogText()).toContain('Add "Open Docs" connector?');
    expect(lastDialogText()).toContain("sent to mcp.open-docs.example");
    expect(dialog()).not.toBeNull(); // declined: still on the preview

    nativeDialog.mockReturnValueOnce(true);
    await click(inDialog("Connect"));
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(host().querySelector("h3")!.textContent).toBe("Open Docs");
    expect(liveText(host())).toBe("Open Docs connected");
    const checked = [...host().querySelectorAll('[role="radio"][aria-checked="true"]')];
    expect(checked.length).toBeGreaterThan(0);
    expect(checked.every((r) => r.textContent === "Needs approval")).toBe(true);
  });

  it("a sign-in connector: wait for the browser; Cancel goes to the detail page", async () => {
    await open();
    let finish: (r: ConnectorSaveResult) => void = () => {};
    vi.spyOn(api, "signInConnector").mockImplementation(
      () =>
        new Promise((r) => {
          finish = r;
        }),
    );
    await click(buttonIn(host(), "Browse connectors"));
    await click(dialog().querySelector('[aria-label="Add Example Notes"]'));
    expect(dialog().textContent).toContain("Sign-in happens in your browser.");
    expect(dialog().textContent).toContain("You'll see its tools after you sign in.");
    nativeDialog.mockReturnValueOnce(true);
    await click(inDialog("Connect"));
    expect(dialog().textContent).toContain("Finish signing in in your browser…");
    await click(inDialog("Cancel"));
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(host().querySelector("h3")!.textContent).toBe("Example Notes");
    expect(host().textContent).toContain("Sign in to use Example Notes.");
    finish({ saved: true, connector: null, error: null });
  });

  it("a failed sign-in shows its words on the detail page", async () => {
    await open();
    vi.spyOn(api, "signInConnector").mockRejectedValue("Sign-in was cancelled.");
    await click(buttonIn(host(), "Browse connectors"));
    await click(dialog().querySelector('[aria-label="Add Sample Tracker"]'));
    nativeDialog.mockReturnValueOnce(true);
    await click(inDialog("Connect"));
    expect(host().querySelector("h3")!.textContent).toBe("Sample Tracker");
    expect(host().querySelector('[role="alert"]')!.textContent).toBe("Sign-in was cancelled.");
  });

  it("an API-key connector needs the key; it goes to the keychain", async () => {
    await open();
    await click(buttonIn(host(), "Browse connectors"));
    await click(dialog().querySelector('[aria-label="Add Demo Search"]'));
    expect(dialog().textContent).toContain("Enter your key to see its tools.");
    await click(inDialog("Connect"));
    expect(dialog().textContent).toContain("Enter your key.");
    const key = fieldIn(dialog(), "Demo Search API key");
    expect(key.type).toBe("password");
    await type(key, "demo-key-1");
    const add = vi.spyOn(api, "addConnector");
    nativeDialog.mockReturnValueOnce(true);
    await click(inDialog("Connect"));
    expect(add.mock.calls[0][0].secrets).toEqual({ apiKey: "demo-key-1" });
    expect(host().querySelector("h3")!.textContent).toBe("Demo Search");
    expect(host().innerHTML).not.toContain("demo-key-1");
  });
});

describe("Add custom connector", () => {
  it("checks the form, then the server, then asks natively", async () => {
    await open();
    await addMenu("Custom (web address)");
    expect(dialog().textContent).toContain(
      "Fly on the Wall hasn't checked this connector. Only add ones from people you trust.",
    );
    expect(dialog().querySelector('input[type="radio"]:checked')).toBeNull();
    const probe = vi.spyOn(api, "probeConnector");
    await type(fieldIn(dialog(), "Name"), "Team tracker");
    await type(fieldIn(dialog(), "Web address"), "http://tracker.example.com/mcp");
    await click(inDialog("Continue"));
    expect(dialog().textContent).toContain("Use an address that starts with https://");
    expect(dialog().textContent).toContain("Choose how it signs in.");
    expect(probe).not.toHaveBeenCalled();

    await type(fieldIn(dialog(), "Web address"), "https://tracker.example.com/mcp");
    await click(
      [...dialog().querySelectorAll<HTMLInputElement>('input[type="radio"]')].find((r) =>
        r.parentElement?.textContent?.includes("No sign-in"),
      ) ?? null,
    );
    const add = vi.spyOn(api, "addConnector");
    await click(inDialog("Continue"));
    expect(probe).toHaveBeenCalledTimes(1);
    expect(dialog().textContent).toContain("Found 12 tools: 4 read-only, 8 can make changes");
    expect(add).toHaveBeenCalledTimes(1);
    expect(lastDialogText()).toContain('Add "Team tracker" connector?');
    expect(dialog()).not.toBeNull(); // declined

    nativeDialog.mockReturnValueOnce(true);
    await click(inDialog("Continue"));
    expect(host().querySelector("h3")!.textContent).toBe("Team tracker");
    expect(host().textContent).toContain("Custom");
  });

  it("says when an address isn't a connector", async () => {
    await open();
    await addMenu("Custom (web address)");
    await type(fieldIn(dialog(), "Name"), "Nope");
    await type(fieldIn(dialog(), "Web address"), "https://not-a-connector.example/mcp");
    await click(
      [...dialog().querySelectorAll<HTMLInputElement>('input[type="radio"]')].find((r) =>
        r.parentElement?.textContent?.includes("No sign-in"),
      ) ?? null,
    );
    const add = vi.spyOn(api, "addConnector");
    await click(inDialog("Continue"));
    expect(dialog().querySelector('[role="alert"]')!.textContent).toBe(
      "That address didn't answer like a connector.",
    );
    expect(add).not.toHaveBeenCalled();
  });

  it("API key and a secret header: password fields, values only to the keychain", async () => {
    await open();
    await addMenu("Custom (web address)");
    await type(fieldIn(dialog(), "Name"), "Team tracker");
    await type(fieldIn(dialog(), "Web address"), "https://tracker.example.com/mcp");
    await click(
      [...dialog().querySelectorAll<HTMLInputElement>('input[type="radio"]')].find((r) =>
        r.parentElement?.textContent?.includes("API key or token"),
      ) ?? null,
    );
    const key = fieldIn(dialog(), "API key or token");
    expect(key.type).toBe("password");
    expect(dialog().textContent).toContain(
      "Saved in your system keychain, never in Fly on the Wall's files",
    );
    await type(key, "tk-123");
    await click(inDialog("Advanced"));
    await click(inDialog("Add header"));
    await type(
      dialog().querySelector<HTMLInputElement>('[aria-label="Header 1 name"]')!,
      "X-Workspace",
    );
    await click(inDialog("Continue"));
    expect(dialog().textContent).toContain("Enter a value. It's saved in your keychain.");
    const value = dialog().querySelector<HTMLInputElement>('[aria-label="Header 1 value"]')!;
    expect(value.type).toBe("password");
    await type(value, "ws-9");
    const add = vi.spyOn(api, "addConnector");
    nativeDialog.mockReturnValueOnce(true);
    await click(inDialog("Continue"));
    expect(add.mock.calls[0][0]).toEqual({
      config: {
        name: "Team tracker",
        transport: {
          kind: "http",
          url: "https://tracker.example.com/mcp",
          headers: [{ name: "X-Workspace", value: null }],
        },
        auth: { kind: "api_key" },
      },
      secrets: { apiKey: "tk-123", clientSecret: null, values: { "X-Workspace": "ws-9" } },
    });
    expect(host().innerHTML).not.toContain("tk-123");
    expect(host().innerHTML).not.toContain("ws-9");
  });
});

describe("Add local connector", () => {
  it("keeps the warning up, previews what will run, and asks natively", async () => {
    await open();
    await addMenu("Local (program on this computer)");
    expect(dialog().querySelector('[role="note"]')!.textContent).toBe(
      "This runs a program on your computer with the same access you have. Only add one you trust.",
    );
    await type(fieldIn(dialog(), "Name"), "Notes server");
    await click(inDialog("npx (Node)"));
    await click(inDialog("Add argument"));
    await type(dialog().querySelector<HTMLInputElement>('[aria-label="Argument 1"]')!, "-y");
    await click(inDialog("Add argument"));
    await type(
      dialog().querySelector<HTMLInputElement>('[aria-label="Argument 2"]')!,
      "@example/notes-server",
    );
    await click(inDialog("Add setting"));
    await type(
      dialog().querySelector<HTMLInputElement>('[aria-label="Setting 1 name"]')!,
      "NOTES_TOKEN",
    );
    await type(dialog().querySelector<HTMLInputElement>('[aria-label="Setting 1 value"]')!, "n-42");
    expect(dialog().querySelector("[data-will-run]")!.textContent).toBe(
      "npx -y @example/notes-server",
    );
    expect(dialog().textContent).toContain("With settings: NOTES_TOKEN=••••");
    expect(dialog().textContent).not.toContain("n-42");

    nativeDialog.mockReturnValueOnce(true);
    await click(inDialog("Add connector"));
    expect(lastDialogText()).toContain("Run a program on this computer?");
    expect(lastDialogText()).toContain('"Notes server" will run: npx -y @example/notes-server');
    expect(host().querySelector("h3")!.textContent).toBe("Notes server");
  });

  it("Paste a command fills the rows; Choose a file… uses the native picker", async () => {
    await open();
    await addMenu("Local (program on this computer)");
    await click(inDialog("Paste a command"));
    await type(
      dialog().querySelector<HTMLTextAreaElement>('[aria-label="Command to paste"]')!,
      'TOKEN=abc uvx example-server --dir "My Notes"',
    );
    await click(inDialog("Fill in"));
    expect(dialog().querySelector('[role="radio"][aria-checked="true"]')!.textContent).toBe(
      "uvx (Python)",
    );
    expect(dialog().querySelector("[data-will-run]")!.textContent).toBe(
      'uvx example-server --dir "My Notes"',
    );
    expect(dialog().querySelector<HTMLInputElement>('[aria-label="Setting 1 name"]')!.value).toBe(
      "TOKEN",
    );
    const pick = vi.spyOn(api, "pickConnectorProgram");
    await click(inDialog("Choose a file…"));
    expect(pick).toHaveBeenCalledTimes(1);
    expect(dialog().querySelector("[data-will-run]")!.textContent).toBe(
      '/opt/example/bin/example-server example-server --dir "My Notes"',
    );
  });

  it("a program that stops right away: saved, and the output is under Show details", async () => {
    await open();
    await addMenu("Local (program on this computer)");
    await type(fieldIn(dialog(), "Name"), "Crashy");
    await click(inDialog("npx (Node)"));
    await click(inDialog("Add argument"));
    await type(dialog().querySelector<HTMLInputElement>('[aria-label="Argument 1"]')!, "--crash");
    nativeDialog.mockReturnValueOnce(true);
    await click(inDialog("Add connector"));
    expect(host().querySelector("h3")!.textContent).toBe("Crashy");
    const alert = host().querySelector<HTMLElement>('[role="alert"]')!;
    expect(alert.textContent).toContain("The program stopped right away.");
    expect(alert.querySelector("pre")).toBeNull();
    await click(buttonIn(alert, "Show details"));
    expect(alert.querySelector("pre")!.textContent).toContain("Error: DOCS_TOKEN is missing");
    expect(host().textContent).toContain("Couldn't start");
  });
});

describe("Import from Claude Desktop", () => {
  it("nothing is pre-checked; Review steps through one at a time", async () => {
    await open();
    await addMenu("Import from Claude Desktop");
    expect(dialog().textContent).toContain("We found 2 connectors in Claude Desktop.");
    const boxes = () => [...dialog().querySelectorAll<HTMLInputElement>('input[type="checkbox"]')];
    expect(boxes().every((b) => !b.checked)).toBe(true);
    expect(inDialog("Review 0")!.disabled).toBe(true);
    await click(dialog().querySelector('[aria-label="Details for Tracker"]'));
    expect(dialog().textContent).toContain("uvx example-tracker-mcp");
    expect(dialog().textContent).toContain("TRACKER_TOKEN=••••");
    expect(dialog().textContent).toContain("(will move to your keychain)");
    await click(boxes()[0]);
    await click(boxes()[1]);
    await click(inDialog("Review 2"));
    expect(dialog().textContent).toContain("1 of 2");
    expect(dialog().textContent).toContain("Files");
    const imp = vi.spyOn(api, "importClaudeDesktopConnector");
    nativeDialog.mockReturnValueOnce(true);
    await click(inDialog("Import"));
    expect(imp).toHaveBeenLastCalledWith("files");
    expect(dialog().textContent).toContain("2 of 2");
    await click(inDialog("Skip"));
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(host().textContent).toContain("Files");
    expect(liveText(host())).toBe("1 connector imported");
  });

  it("says when there's no Claude Desktop setup", async () => {
    localStorage.setItem("fotwMockClaudeImport", "none");
    await open();
    await addMenu("Import from Claude Desktop");
    expect(dialog().textContent).toContain("No Claude Desktop setup found on this computer.");
    await act(async () => inDialog("Close")!.click());
    await flush();
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    localStorage.removeItem("fotwMockClaudeImport");
  });
});
