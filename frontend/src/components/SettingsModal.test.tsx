// @vitest-environment jsdom
// Drives the real Settings overlay against the dev mock backend (devMock.ts),
// the same stub used for port-1420 QA. The mock is stateful per module, so
// tests that change settings restore them.
import { resetMockAppSettings } from "../devMock";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
  type MockInstance,
} from "vitest";
import { api } from "../api";
import { RESTART_REQUIRED_EVENT } from "../privacy";
import type { Updater } from "../updater";
import { ConfirmProvider } from "./confirm";
import { confirmChoice, confirmDialog, confirmTitle } from "./confirmTesting";
import RestartRequired from "./RestartRequired";
import SettingsModal from "./SettingsModal";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const updater: Updater = {
  phase: "idle",
  version: null,
  progress: null,
  error: null,
  supported: true,
  dismissed: false,
  check: () => {},
  downloadAndInstall: () => {},
  restart: () => {},
  dismiss: () => {},
};

let root: Root | null = null;
let host: HTMLElement | null = null;

async function flush() {
  for (let i = 0; i < 5; i++) await act(async () => {});
}

async function open(props: Partial<Parameters<typeof SettingsModal>[0]> = {}) {
  const onClose = vi.fn();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <ConfirmProvider>
        <SettingsModal
          modelProgress={null}
          updater={updater}
          recordingActive={false}
          appVersion="2.3.0"
          onClose={onClose}
          {...props}
        />
      </ConfirmProvider>,
    ),
  );
  await flush();
  return onClose;
}

const $ = <T extends Element = HTMLElement>(sel: string) => host!.querySelector<T>(sel);
const $$ = (sel: string) => [...host!.querySelectorAll<HTMLElement>(sel)];
// Search hides sections (they stay mounted), so count only visible ones.
const headings = () =>
  $$(".fly-settings-section:not([hidden]) > .fly-settings-h").map((h) => h.textContent);
const byText = (sel: string, text: string) =>
  $$(sel).find((el) => el.textContent?.trim() === text) ?? null;
// The discard dialog is a sibling Modal inside the overlay.
const dialogTitle = () => $(".fly-modal-title")?.textContent ?? null;

async function click(el: Element | null) {
  if (!el) throw new Error("element not found");
  await act(async () => (el as HTMLElement).click());
  await flush();
}

async function pressEscape(target: EventTarget = document.body) {
  await act(async () => {
    target.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
  });
  await flush();
}

async function type(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  await act(async () => {
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
}

async function answer(yes: boolean) {
  await act(async () => confirmChoice(yes).click());
  await flush();
}

/* The dev mock asks for cloud consent with the browser's confirm: its
   stand-in for the app's native dialog (request_cloud_consent). Unanswered,
   it declines. */
let consentDialog: MockInstance<(message?: string) => boolean>;
/** Answer the next consent dialog. `whileOpen` runs while it's showing (an
 *  expect() thrown there would only read as "no", so return what to check). */
function nextConsent(yes: boolean, whileOpen?: () => void) {
  consentDialog.mockImplementationOnce(() => {
    whileOpen?.();
    return yes;
  });
}
/** What the last consent dialog said. */
const consentText = () => String(consentDialog.mock.lastCall?.[0] ?? "");

async function choose(select: HTMLSelectElement, value: string) {
  await act(async () => {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await flush();
}

beforeAll(() => {
  Element.prototype.scrollIntoView = vi.fn();
});

beforeEach(() => {
  vi.restoreAllMocks();
  // consent given in one test must not leak into the next (module state)
  resetMockAppSettings();
  consentDialog = vi.spyOn(window, "confirm").mockReturnValue(false);
});

afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
  host = null;
  localStorage.removeItem("fotwMockConsentReset");
});

/** Start with no cloud consent at all, as after the v2.4 reset. */
function withoutConsent() {
  localStorage.setItem("fotwMockConsentReset", "1");
  resetMockAppSettings();
}

describe("Settings overlay", () => {
  it("shows the seven sections, the data tiles and the real data folder", async () => {
    await open();
    expect(headings()).toEqual([
      "Recording",
      "Transcripts",
      "Ask and Enhance",
      "Calendars",
      "Your files and privacy",
      "Other AI apps",
      "General",
    ]);
    expect($('[role="dialog"]')!.getAttribute("aria-modal")).toBe("true");
    const tiles = $$(".fly-settings-tile").map((t) => t.textContent);
    expect(tiles[0]).toContain("Recordings");
    // the mock's provider is Anthropic: the AI tile names it and covers
    // cleanup, action items and keep-local
    expect(tiles[2]).toContain("Anthropic cloud");
    expect(tiles[2]).toContain("transcript cleanup and action items");
    expect(tiles[2]).toContain("except notes you keep local");
    expect(host!.textContent).toContain("C:\\Users\\you\\AppData\\Roaming\\FlyOnTheWall");
    // 220 px nav lists every section; search is focused on open
    expect($$(".fly-settings-navitem")).toHaveLength(7);
    expect(document.activeElement).toBe($("input[type=search]"));
  });

  it("search filters sections by title and keywords", async () => {
    await open();
    const search = $<HTMLInputElement>("input[type=search]")!;
    await type(search, "mcp");
    expect(headings()).toEqual(["Other AI apps"]);
    expect($$(".fly-settings-navitem").map((b) => b.textContent)).toEqual(["Other AI apps"]);
    await type(search, "retention");
    expect(headings()).toEqual(["Your files and privacy"]);
    await type(search, "zzzz");
    expect(headings()).toEqual([]);
    expect($(".fly-settings-nomatch")?.textContent).toBe("No settings match.");
  });

  it("Esc clears the search first, then closes Settings", async () => {
    const onClose = await open();
    const search = $<HTMLInputElement>("input[type=search]")!;
    await type(search, "groq");
    await pressEscape(search);
    expect(search.value).toBe("");
    expect(onClose).not.toHaveBeenCalled();
    await pressEscape(search);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("Back and Done return you without a prompt when nothing is unsaved", async () => {
    const onClose = await open();
    await click(byText("button", "Back"));
    await click(byText("button", "Done"));
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("unsaved transcript changes: save bar, layered Esc, save from the prompt", async () => {
    const setAsr = vi.spyOn(api, "setAsrSettings");
    const onClose = await open();
    expect($(".fly-settings-savebar")).toBeNull();
    await click(byText('[role="radio"]', "Fast"));
    expect($(".fly-settings-savebar")?.textContent).toContain(
      "Your transcript changes aren't saved yet.",
    );
    expect(setAsr).not.toHaveBeenCalled();

    // Esc asks first …
    await pressEscape();
    expect(dialogTitle()).toBe("Save your transcript changes?");
    // … and a second Esc closes only the prompt (topmost layer)
    await pressEscape();
    expect(dialogTitle()).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    expect($(".fly-settings")).not.toBeNull();

    await click(byText("button", "Done"));
    await click(byText(".fly-modal-foot button", "Save"));
    expect(setAsr).toHaveBeenCalledWith(
      expect.objectContaining({ tier: "light", use_groq: false, groq_key: null }),
    );
    expect(onClose).toHaveBeenCalledTimes(1);

    // restore the mock
    await api.setAsrSettings({ ...setAsr.mock.calls[0][0], tier: "balanced" });
  });

  it("Groq shows the key field and cloud badge; Discard puts everything back", async () => {
    const setAsr = vi.spyOn(api, "setAsrSettings");
    const onClose = await open();
    await click(byText('[role="radio"]', "Groq, then this computer"));
    expect(host!.textContent).toContain("Audio leaves this computer");
    expect(host!.textContent).toContain("Groq API key");
    await click(byText(".fly-settings-savebar button", "Discard"));
    expect($(".fly-settings-savebar")).toBeNull();
    expect(host!.textContent).not.toContain("Groq API key");
    await pressEscape();
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(setAsr).not.toHaveBeenCalled();
  });

  it("discard from the close prompt leaves the saved settings alone", async () => {
    const setAsr = vi.spyOn(api, "setAsrSettings");
    const onClose = await open();
    await click(byText('[role="radio"]', "Best"));
    await pressEscape();
    await click(byText(".fly-modal-foot button", "Discard"));
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(setAsr).not.toHaveBeenCalled();
  });

  it("'Transcribe when I stop' applies at once with the saved transcript settings", async () => {
    const setAsr = vi.spyOn(api, "setAsrSettings");
    await open();
    const sw = $('[aria-label="Transcribe when I stop"]')!;
    expect(sw.getAttribute("aria-checked")).toBe("true");
    await click(byText('[role="radio"]', "Fast")); // an unsaved draft …
    await click(sw);
    // … is not sent along: only auto_transcribe changes
    expect(setAsr).toHaveBeenCalledWith(
      expect.objectContaining({ auto_transcribe: false, tier: "balanced" }),
    );
    expect(sw.getAttribute("aria-checked")).toBe("false");
    expect($(".fly-settings-savebar")).not.toBeNull();
    await click(sw); // restore
  });

  it("picking a cloud provider asks for consent first", async () => {
    const setLlm = vi.spyOn(api, "setLlmSettings");
    const setSetting = vi.spyOn(api, "setAppSetting");
    await open();
    const runsOn = $<HTMLSelectElement>('select[aria-label="Runs on"]')!;
    expect(runsOn.value).toBe("anthropic");

    // the question waits for you: nothing is saved while it's open
    let savedWhileOpen: number | null = null;
    nextConsent(false, () => {
      savedWhileOpen = setLlm.mock.calls.length;
    });
    await choose(runsOn, "openai");
    expect(consentDialog).toHaveBeenCalledTimes(1);
    expect(consentText()).toContain("Use OpenAI for Ask and Enhance?");
    expect(consentText()).toContain("transcript cleanup");
    expect(savedWhileOpen).toBe(0);
    expect(await api.getAppSetting("consent.cloud_llm.openai")).toBeNull();
    expect(setLlm).not.toHaveBeenCalled();
    expect(runsOn.value).toBe("anthropic");
    expect(host!.textContent).toContain("Kept your current provider");

    nextConsent(true);
    await choose(runsOn, "openai");
    // recorded by the backend's consent command, never by a settings write
    expect(await api.getAppSetting("consent.cloud_llm.openai")).toBe("true");
    expect(setSetting).not.toHaveBeenCalled();
    expect(setLlm).toHaveBeenCalledWith(expect.objectContaining({ provider: "openai" }));
    expect(host!.textContent).toContain("Text leaves this computer");
    expect(host!.textContent).toContain("OpenAI API key");

    // local needs no consent; restore the mock to Anthropic afterwards
    await choose(runsOn, "ollama");
    expect(consentDialog).toHaveBeenCalledTimes(2);
    expect(host!.textContent).toContain("Nothing leaves this computer");
    await choose(runsOn, "anthropic");
  });

  it("moving a provider to another server asks for consent again, naming the server", async () => {
    await open();
    const runsOn = $<HTMLSelectElement>('select[aria-label="Runs on"]')!;
    nextConsent(true);
    await choose(runsOn, "openai");
    expect(await api.getAppSetting("consent.cloud_llm.openai")).toBe("true");
    await click($('[aria-labelledby="settings-h-ai"] .fly-sgroup-more'));
    await type(
      $<HTMLInputElement>('input[aria-label="Base URL"]')!,
      "https://proxy.example:8443/v1",
    );
    await type($<HTMLInputElement>("input[type=password]")!, "sk-qa-placeholder");
    const asked = consentDialog.mock.calls.length;
    nextConsent(false);
    await click(byText("button", "Save"));
    expect(consentDialog.mock.calls.length).toBe(asked + 1);
    expect(consentText()).toContain(
      "sent to the server at proxy.example:8443 (your custom OpenAI endpoint) for processing",
    );
    expect(await api.getAppSetting("consent.cloud_llm.openai")).toBeNull();
    expect(host!.textContent).toContain("Nothing is sent to the new server until you allow it");

    // Test asks again; a yes records it for the new server
    nextConsent(true);
    await click(byText("button", "Test"));
    expect(await api.getAppSetting("consent.cloud_llm.openai")).toBe("true");

    // restore the mock's OpenAI entry and provider
    await act(async () => {
      await api.setLlmSettings({ provider: "openai", model: null, base_url: null, api_key: "" });
    });
    await choose(runsOn, "anthropic");
  });

  it("deleting a local model asks first, and Cancel keeps it", async () => {
    const del = vi.spyOn(api, "ollamaDelete");
    await open();
    const runsOn = $<HTMLSelectElement>('select[aria-label="Runs on"]')!;
    await choose(runsOn, "ollama");
    await click($('[aria-labelledby="settings-h-ai"] .fly-sgroup-more'));
    const deleteButton = () => byText("button", "Delete");
    await click(deleteButton());
    expect(confirmTitle()).toBe("Delete qwen3.5:4b?");
    expect(confirmDialog()!.dataset.tone).toBe("danger");
    expect(del).not.toHaveBeenCalled();
    await answer(false);
    expect(del).not.toHaveBeenCalled();
    expect(host!.textContent).toContain("qwen3.5:4b");

    await click(deleteButton());
    await answer(true);
    expect(del).toHaveBeenCalledWith("qwen3.5:4b");
    expect(host!.textContent).toContain("qwen3.5:4b deleted.");
    expect(deleteButton()).toBeNull(); // the only other model is the one in use
    await choose(runsOn, "anthropic");
  });

  it("More options reveals advanced rows per group", async () => {
    await open();
    expect(host!.textContent).not.toContain("Endpoint and model");
    const aiMore = $('[aria-labelledby="settings-h-ai"] .fly-sgroup-more')!;
    await click(aiMore);
    expect(host!.textContent).toContain("Endpoint and model");
    expect(aiMore.textContent).toBe("Fewer options");
    const generalMore = $('[aria-labelledby="settings-h-general"] .fly-sgroup-more')!;
    await click(generalMore);
    expect(byText("button", "Open logs folder")).not.toBeNull();
  });

  it("deep link 'engine' opens Transcripts › More options at the models list", async () => {
    await open({ initialFocus: "engine" });
    const row = $("#settings-row-engine")!;
    expect(row).not.toBeNull();
    expect(row.className).toContain("fly-settings-flash");
    expect(row.textContent).toContain("Transcription engine (whisper.cpp)");
    expect(row.textContent).toContain("whisper.cpp CLI (Vulkan GPU, v1.9.1)");
    expect(Element.prototype.scrollIntoView).toHaveBeenCalled();
  });

  it("deep link 'groq' lands on the Made on row", async () => {
    await open({ initialFocus: "groq" });
    const row = $("#settings-row-groq")!;
    expect(row.className).toContain("fly-settings-flash");
    expect(row.textContent).toContain("Groq, then this computer");
  });

  it("deep link 'llm-key' lands on Ask and Enhance with the key field focused", async () => {
    await open({ initialFocus: "llm-key" });
    const row = $("#settings-row-llm")!;
    expect(row.className).toContain("fly-settings-flash");
    expect(row.textContent).toContain("Runs on");
    const key = row.querySelector<HTMLInputElement>("input[type=password]");
    expect(key).not.toBeNull();
    expect(document.activeElement).toBe(key);
  });

  it("switching Made on back to this computer drops a typed Groq key", async () => {
    await open();
    await click(byText('[role="radio"]', "Groq"));
    const key = [...host!.querySelectorAll<HTMLInputElement>("input[type=password]")].find((i) =>
      i.placeholder.startsWith("gsk_"),
    )!;
    await type(key, "gsk_secret");
    await click(byText('[role="radio"]', "This computer"));
    expect($(".fly-settings-savebar")).toBeNull();
    await click(byText('[role="radio"]', "Groq"));
    const again = [...host!.querySelectorAll<HTMLInputElement>("input[type=password]")].find((i) =>
      i.placeholder.startsWith("gsk_"),
    )!;
    expect(again.value).toBe("");
    await click(byText(".fly-settings-savebar button", "Discard"));
  });

  it("a cloud provider without a saved key says so", async () => {
    await open();
    const runsOn = $<HTMLSelectElement>('select[aria-label="Runs on"]')!;
    expect(host!.textContent).not.toContain("No key saved yet");
    nextConsent(true); // consent first (nothing leaks in from other tests)
    await choose(runsOn, "openai");
    expect(host!.textContent).toContain("No key saved yet");
    await choose(runsOn, "anthropic");
  });

  it("searching keeps half-filled forms in hidden sections", async () => {
    await open();
    await click(byText("button", "Delete all…"));
    expect(host!.textContent).toContain("I understand this can't be undone");
    const search = $<HTMLInputElement>("input[type=search]")!;
    await type(search, "mcp");
    expect(headings()).toEqual(["Other AI apps"]);
    await type(search, "");
    expect(host!.textContent).toContain("I understand this can't be undone");
  });

  it("instant-apply controls wait while transcript changes save", async () => {
    let release: () => void = () => {};
    const setAsr = vi
      .spyOn(api, "setAsrSettings")
      .mockImplementation(() => new Promise<void>((r) => (release = r)));
    await open();
    await click(byText('[role="radio"]', "Fast"));
    await click(byText(".fly-settings-savebar button", "Save changes"));
    expect($('[aria-label="Transcribe when I stop"]')!.hasAttribute("disabled")).toBe(true);
    expect(setAsr).toHaveBeenCalledTimes(1);
    setAsr.mockRestore();
    await act(async () => release());
    await flush();
  });

  it("Other AI apps: Set up shows the snippet and never edits Claude's config", async () => {
    await open();
    await click(byText("button", "Set up"));
    expect(host!.textContent).toContain("claude_desktop_config.json");
    expect(host!.textContent).toContain('"flyonthewall"');
    // Windows (the mock's OS): the snippet carries the real sidecar path even
    // though the config-file location is only spelled out on macOS.
    expect(host!.textContent).toContain("flyonthewall-mcp.exe");
    expect(host!.textContent).toContain("Settings › Developer › Edit Config");
    expect(host!.textContent).not.toContain("Library/Application Support");
    expect(host!.textContent).toContain("including notes you keep local");
  });

  it("closes when 'Delete all my data' hands over to the restart screen", async () => {
    const onClose = await open();
    await act(async () => {
      window.dispatchEvent(new CustomEvent(RESTART_REQUIRED_EVENT, { detail: {} }));
    });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("locks the provider selects while a provider save is in flight", async () => {
    let release: () => void = () => {};
    const setLlm = vi
      .spyOn(api, "setLlmSettings")
      .mockImplementation(() => new Promise<void>((r) => (release = r)));
    await open();
    await click($('[aria-labelledby="settings-h-ai"] .fly-sgroup-more'));
    await click(byText('[aria-labelledby="settings-h-ai"] button', "Save"));
    expect($<HTMLSelectElement>('select[aria-label="Claude model"]')!.disabled).toBe(true);
    expect($<HTMLSelectElement>('select[aria-label="Runs on"]')!.disabled).toBe(true);
    // a model pick that slips through still can't send a second write
    await choose($<HTMLSelectElement>('select[aria-label="Claude model"]')!, "claude-haiku-5-5");
    expect(setLlm).toHaveBeenCalledTimes(1);
    setLlm.mockRestore();
    await act(async () => release());
    await flush();
  });
});

describe("RestartRequired", () => {
  it("takes focus so keyboard users can reach Restart", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    act(() => root!.render(<RestartRequired />));
    await act(async () => {
      window.dispatchEvent(
        new CustomEvent(RESTART_REQUIRED_EVENT, {
          detail: {
            includeModels: false,
            report: {
              removed: 1,
              failed: ["recordings/x"],
              secrets_failed: [],
              restart_required: true,
              db_erased: true,
              db_checkpointed: true,
              notes: [],
            },
          },
        }),
      );
    });
    await flush();
    expect(document.activeElement?.textContent).toBe("Restart now");
  });
});

describe("Recording › Remind me to tell people", () => {
  it("is on by default and saves under a ui. key", async () => {
    const set = vi.spyOn(api, "setAppSetting");
    await open();
    const sw = $('[aria-label="Remind me to tell people"]')!;
    expect(sw.getAttribute("aria-checked")).toBe("true");
    await click(sw);
    expect(sw.getAttribute("aria-checked")).toBe("false");
    expect(set).toHaveBeenCalledWith("ui.recording.remind_notice", "false");
    await click(sw);
    expect(set).toHaveBeenLastCalledWith("ui.recording.remind_notice", "true");
  });

  it("is found by searching for it", async () => {
    await open();
    await type($<HTMLInputElement>("input[type=search]")!, "remind");
    expect(headings()).toEqual(["Recording"]);
  });
});

describe("Settings › asking again for cloud consent", () => {
  // The refusal text sends people to "Settings › Ask and Enhance and click
  // Test"; Save, the Claude model and the backfill send content too.
  const consent = () => api.getAppSetting("consent.cloud_llm.anthropic");

  it("Test asks first: Cancel sends nothing, allow saves consent and tests", async () => {
    withoutConsent();
    const setLlm = vi.spyOn(api, "setLlmSettings");
    const test = vi.spyOn(api, "testLlmConnection");
    await open();
    nextConsent(false);
    await click(byText("button", "Test"));
    expect(consentText()).toContain("Use Anthropic for Ask and Enhance?");
    expect(await consent()).toBeNull();
    expect(setLlm).not.toHaveBeenCalled();
    expect(test).not.toHaveBeenCalled();

    nextConsent(true);
    await click(byText("button", "Test"));
    expect(await consent()).toBe("true");
    expect(setLlm).toHaveBeenCalledTimes(1);
    expect(test).toHaveBeenCalledTimes(1);
    expect(host!.textContent).toContain("Connected to anthropic");
  });

  it("saving a key asks first, and Cancel saves nothing", async () => {
    withoutConsent();
    const setLlm = vi.spyOn(api, "setLlmSettings");
    await open();
    await type($<HTMLInputElement>("input[type=password]")!, "sk-test-key");
    nextConsent(false);
    await click(byText("button", "Update"));
    expect(consentText()).toContain("Use Anthropic for Ask and Enhance?");
    expect(await consent()).toBeNull();
    expect(setLlm).not.toHaveBeenCalled();
    expect(host!.textContent).toContain("Not saved. Cloud processing wasn't confirmed.");

    nextConsent(true);
    await click(byText("button", "Update"));
    expect(await consent()).toBe("true");
    expect(setLlm).toHaveBeenCalledWith(
      expect.objectContaining({ provider: "anthropic", api_key: "sk-test-key" }),
    );
  });

  it("the Claude model select asks first, and Cancel keeps the model", async () => {
    withoutConsent();
    const setLlm = vi.spyOn(api, "setLlmSettings");
    await open();
    const model = $<HTMLSelectElement>('select[aria-label="Claude model"]')!;
    const before = model.value;
    nextConsent(false);
    await choose(model, "claude-haiku-5-5");
    expect(consentText()).toContain("Use Anthropic for Ask and Enhance?");
    expect(await consent()).toBeNull();
    expect(setLlm).not.toHaveBeenCalled();
    expect(model.value).toBe(before);

    nextConsent(true);
    await choose(model, "claude-haiku-5-5");
    expect(await consent()).toBe("true");
    expect(setLlm).toHaveBeenCalledWith(
      expect.objectContaining({ provider: "anthropic", model: "claude-haiku-5-5" }),
    );
  });

  it("indexing older meetings asks first, and Cancel extracts nothing", async () => {
    withoutConsent();
    vi.spyOn(api, "pendingBackfillCount").mockResolvedValue(3);
    const backfill = vi.spyOn(api, "backfillMeetingItems");
    await open();
    const index = () =>
      $$("button").find((b) => b.textContent?.startsWith("Extract from ")) ?? null;
    nextConsent(false);
    await click(index());
    expect(consentText()).toContain("Use Anthropic for Ask and Enhance?");
    expect(backfill).not.toHaveBeenCalled();
    expect(host!.textContent).toContain(
      "Nothing was extracted. Cloud processing wasn't confirmed.",
    );

    nextConsent(true);
    await click(index());
    expect(await consent()).toBe("true");
    expect(backfill).toHaveBeenCalledTimes(1);
  });

  it("says why when every meeting failed to extract", async () => {
    vi.spyOn(api, "pendingBackfillCount").mockResolvedValue(3);
    vi.spyOn(api, "backfillMeetingItems").mockResolvedValue({
      processed: 3,
      extracted: 0,
      failed: 3,
      first_error: "No API key for Anthropic yet. Add one in Settings › Ask and Enhance.",
    });
    await open();
    await click($$("button").find((b) => b.textContent?.startsWith("Extract from ")) ?? null);
    const status = $$("[role=status]").find((p) => p.textContent?.startsWith("Couldn't extract"))!;
    // plain words first; the backend's own text muted under them
    expect(status.firstChild?.textContent).toBe(
      "Couldn't extract from any of the 3 meetings. Check Ask and Enhance in Settings, then try again.",
    );
    const detail = status.querySelector(".text-text-3")!;
    expect(detail.textContent).toBe(
      "No API key for Anthropic yet. Add one in Settings › Ask and Enhance.",
    );
  });
});

describe("dev mock › cloud consent mirrors the backend", () => {
  it("refuses consent writes from the UI and records only a yes to its question", async () => {
    withoutConsent();
    const key = "consent.cloud_llm.anthropic";
    await expect(api.setAppSetting(key, "true")).rejects.toBe(
      `setting '${key}' can't be changed from the app UI`,
    );
    expect(await api.getAppSetting(key)).toBeNull();

    nextConsent(false);
    expect(await api.requestCloudConsent("anthropic", true)).toBe(false);
    expect(consentText()).toContain("Your AI provider is set to Anthropic");
    expect(consentText()).toContain("full meeting transcripts will be sent to Anthropic");
    expect(await api.getAppSetting(key)).toBeNull();

    nextConsent(true);
    expect(await api.requestCloudConsent("anthropic")).toBe(true);
    expect(consentText()).not.toContain("Your AI provider is set to");
    expect(await api.getAppSetting(key)).toBe("true");

    // allowed now, or local: no question
    expect(await api.requestCloudConsent("anthropic")).toBe(true);
    expect(await api.requestCloudConsent("ollama")).toBe(true);
    expect(consentDialog).toHaveBeenCalledTimes(2);
    await expect(api.requestCloudConsent("someday")).rejects.toBe("unknown provider 'someday'");
  });
});
