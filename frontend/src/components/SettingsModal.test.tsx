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
import TABLE from "../localModels.table.json";
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
  it("shows the eight sections, the data tiles and the real data folder", async () => {
    await open();
    expect(headings()).toEqual([
      "Recording",
      "Transcripts",
      "Ask and Enhance",
      "Calendars",
      "Your files and privacy",
      "Other AI apps",
      "Connectors Beta",
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
    expect($$(".fly-settings-navitem")).toHaveLength(8);
    expect(document.activeElement).toBe($("input[type=search]"));
  });

  it("search filters sections by title and keywords", async () => {
    await open();
    const search = $<HTMLInputElement>("input[type=search]")!;
    await type(search, "mcp");
    expect(headings()).toEqual(["Other AI apps", "Connectors Beta"]);
    expect($$(".fly-settings-navitem").map((b) => b.textContent)).toEqual([
      "Other AI apps",
      "Connectors",
    ]);
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

  it("local models say what uses them, and in-use ones have no Delete", async () => {
    await open();
    const runsOn = $<HTMLSelectElement>('select[aria-label="Runs on"]')!;
    await choose(runsOn, "ollama");
    // the labels come from the backend's status, reloaded after the switch
    const usedFor = async (name: string) =>
      (await api.ollamaStatus()).models.find((m) => m.name === name)?.used_for ?? null;
    expect(await usedFor("llama3.1:latest")).toBe("Used for Ask and Enhance.");
    expect(await usedFor("embeddinggemma:300m")).toBe("Used for search.");
    await click($('[aria-labelledby="settings-h-ai"] .fly-sgroup-more'));
    const rowOf = (name: string) =>
      $$(".fly-srow-below > div > div").find((r) => r.textContent?.startsWith(name))!;
    const llama = rowOf("llama3.1:latest");
    expect(llama.textContent).toContain("Used for Ask and Enhance.");
    const embed = rowOf("embeddinggemma:300m");
    expect(embed.textContent).toContain("Used for search.");
    for (const r of [llama, embed]) {
      expect(r.textContent).toContain("in use");
      expect([...r.querySelectorAll("button")].some((b) => b.textContent === "Delete")).toBe(false);
    }
    await choose(runsOn, "anthropic");
    expect(await usedFor("llama3.1:latest")).toBeNull();
    expect(await usedFor("embeddinggemma:300m")).toBe("Used for search.");
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
    expect(headings()).toEqual(["Other AI apps", "Connectors Beta"]);
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

describe("Settings › the local model for this computer", () => {
  // An existing install on the old default, on a computer that can run the
  // better model. The backend decides this (local_models.rs); the mock's
  // advice is replaced by this fixture so each test states its own case.
  const row = (over: Record<string, unknown> = {}) => ({
    model: "qwen3.5:9b",
    download_bytes: 6_600_000_000,
    memory_gb: 6.2,
    ram_class_gb: 16,
    fits: true,
    recommended: true,
    needs_newer_ollama: null,
    measured: {
      sourced_pct: 83,
      declines_pct: 94,
      seconds: 30,
      background_slower: [15, 25] as [number, number],
    },
    ...over,
  });
  const advice = (over: Record<string, unknown> = {}) => ({
    ram_gb: 31.7,
    ollama_version: "0.40.13",
    recommended: "qwen3.5:9b",
    held_back: null,
    default_model: "llama3.1",
    default_blocked: null,
    explicit_model: null,
    offer: { model: "qwen3.5:9b", download_bytes: 6_600_000_000 },
    models: [
      row(),
      row({
        model: "llama3.1",
        download_bytes: 4_920_000_000,
        memory_gb: 5.8,
        ram_class_gb: 12,
        recommended: false,
        measured: { sourced_pct: 62, declines_pct: 75, seconds: 25, background_slower: null },
      }),
    ],
    measured_on:
      "Measured on one test laptop (4 GB graphics memory, 32 GB RAM) while other programs were running. Your computer will differ.",
    doc_url: "https://example.com/docs/ASK-MODEL-BENCH.md",
    ...over,
  });
  const offerCard = () => $('[aria-label="Local model offer"]');
  const openLocal = async () => {
    await open();
    await choose($<HTMLSelectElement>('select[aria-label="Runs on"]')!, "ollama");
  };
  afterEach(async () => {
    // the mock is stateful: back to the cloud provider the other tests expect
    await api.setLlmSettings({
      provider: "anthropic",
      model: "claude-sonnet-5-5",
      base_url: null,
      api_key: null,
    });
  });

  it("shows the offer once, with the numbers, and downloads nothing by itself", async () => {
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(advice());
    const pull = vi.spyOn(api, "ollamaPull");
    const use = vi.spyOn(api, "useLocalModel");
    await openLocal();
    const card = offerCard()!;
    expect(card.textContent).toContain(
      "A better local model for this computer: qwen3.5:9b · 6.6 GB",
    );
    expect(card.textContent).toContain("Answers backed by a source: 83%");
    expect(card.textContent).toContain("About 30 seconds per answer");
    expect(card.textContent).toContain("Measured on one test laptop");
    // Seeing it changes nothing: no download, no switch, no dismissal.
    expect(pull).not.toHaveBeenCalled();
    expect(use).not.toHaveBeenCalled();
  });

  it("'Download and switch' downloads first, then makes it the user's choice", async () => {
    // The backend's advice follows the saved choice: the offer is there until
    // the user's click makes the model theirs.
    let chosen = false;
    vi.spyOn(api, "localModelAdvice").mockImplementation(async () =>
      chosen ? advice({ offer: null, explicit_model: "qwen3.5:9b" }) : advice(),
    );
    // (no call-through: the mock would keep the model on disk for later tests)
    const pull = vi.spyOn(api, "ollamaPull").mockResolvedValue(undefined);
    const use = vi.spyOn(api, "useLocalModel").mockImplementation(async () => {
      chosen = true;
    });
    await openLocal();
    expect(offerCard()).not.toBeNull();
    await click(byText("button", "Download and switch (6.6 GB)"));
    expect(pull).toHaveBeenCalledExactlyOnceWith("qwen3.5:9b");
    expect(use).toHaveBeenCalledExactlyOnceWith("qwen3.5:9b");
    // The model is on disk before the setting moves: no window where Ask
    // and Enhance point at a model that isn't here.
    expect(pull.mock.invocationCallOrder[0]).toBeLessThan(use.mock.invocationCallOrder[0]);
    expect(host!.textContent).toContain("Now using qwen3.5:9b.");
    expect(offerCard()).toBeNull();
  });

  it("a failed download switches nothing", async () => {
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(advice());
    vi.spyOn(api, "ollamaPull").mockRejectedValue("the download stopped");
    const use = vi.spyOn(api, "useLocalModel");
    await openLocal();
    await click(byText("button", "Download and switch (6.6 GB)"));
    expect(use).not.toHaveBeenCalled();
    expect(host!.textContent).toContain("the download stopped");
    expect(offerCard()).not.toBeNull(); // still there to try again
  });

  it("'Not now' dismisses it for that model and nothing else happens", async () => {
    let dismissed = false;
    const adv = vi
      .spyOn(api, "localModelAdvice")
      .mockImplementation(async () => (dismissed ? advice({ offer: null }) : advice()));
    const dismiss = vi.spyOn(api, "dismissLocalModelOffer").mockImplementation(async () => {
      dismissed = true;
    });
    const pull = vi.spyOn(api, "ollamaPull");
    const use = vi.spyOn(api, "useLocalModel");
    await openLocal();
    await click(byText("button", "Not now"));
    expect(dismiss).toHaveBeenCalledExactlyOnceWith("qwen3.5:9b");
    expect(adv.mock.calls.length).toBeGreaterThan(1); // advice reloaded
    expect(offerCard()).toBeNull();
    expect(pull).not.toHaveBeenCalled();
    expect(use).not.toHaveBeenCalled();
  });

  // An Ollama too old for the 9B: its row says so, and the advice names the gap.
  const oldOllama = () =>
    advice({
      ollama_version: "0.16.9",
      held_back: { model: "qwen3.5:9b", needs_ollama: "0.17.5", have_ollama: "0.16.9" },
      models: [
        row({ needs_newer_ollama: "0.17.5" }),
        row({ model: "llama3.1", download_bytes: 4_920_000_000, recommended: false }),
      ],
    });

  it("an Ollama refusal for a version it lacks says to update Ollama", async () => {
    // The app couldn't read Ollama's version (so it held nothing back); Ollama
    // itself then refuses the download. The refusal becomes plain advice.
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(advice({ ollama_version: "" }));
    vi.spyOn(api, "ollamaPull").mockRejectedValue(
      "pull model manifest: 412: The model you are attempting to pull requires a newer version of Ollama.",
    );
    const use = vi.spyOn(api, "useLocalModel");
    await openLocal();
    await click(byText("button", "Download and switch (6.6 GB)"));
    expect(use).not.toHaveBeenCalled();
    expect(host!.textContent).toContain(
      "qwen3.5:9b needs a newer Ollama. Update Ollama (ollama.com), then try again.",
    );
    expect(host!.textContent).not.toContain("pull model manifest");
  });

  it("an Ollama known to be too old gets no download button for the model it can't run", async () => {
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(oldOllama());
    await openLocal();
    const nine = $('[data-model="qwen3.5:9b"]')!;
    expect(nine.textContent).toContain("Needs Ollama 0.17.5 or newer (this computer has 0.16.9).");
    expect(nine.querySelector("button")).toBeNull();
  });

  it("the app's own disk-space message is shown as it came, and switches nothing", async () => {
    const disk =
      "Not enough free disk space for qwen3.5:9b. It needs about 8.6 GB free (a 6.6 GB download plus 2.0 GB to spare) and this drive has 3.1 GB. Free up space, or choose a smaller model.";
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(advice());
    vi.spyOn(api, "ollamaPull").mockRejectedValue(disk);
    const use = vi.spyOn(api, "useLocalModel");
    await openLocal();
    await click(byText("button", "Download and switch (6.6 GB)"));
    expect(use).not.toHaveBeenCalled();
    expect(host!.textContent).toContain(disk);
    expect(offerCard()).not.toBeNull(); // still there to try again
  });

  it("a pinned default the running Ollama can't pull says so and offers no doomed download", async () => {
    const llm = await api.getLlmSettings();
    vi.spyOn(api, "getLlmSettings").mockResolvedValue({
      ...llm,
      provider: "ollama",
      providers: llm.providers.map((p) =>
        p.id === "ollama" ? { ...p, default_model: "qwen3.5:9b", model: null } : p,
      ),
    });
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(
      advice({
        ...oldOllama(),
        offer: null,
        default_model: "qwen3.5:9b",
        default_blocked: { model: "qwen3.5:9b", needs_ollama: "0.17.5", have_ollama: "0.16.9" },
      }),
    );
    const pull = vi.spyOn(api, "ollamaPull");
    await open();
    expect(host!.textContent).toContain(
      "qwen3.5:9b needs Ollama 0.17.5 or newer. This computer has 0.16.9. Update Ollama to use it.",
    );
    const download = byText("button", "Download qwen3.5:9b (6.6 GB)") as HTMLButtonElement;
    expect(download.disabled).toBe(true);
    expect(pull).not.toHaveBeenCalled();
  });

  it("shows the after-a-meeting cost on the offer card", async () => {
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(advice());
    await openLocal();
    expect(offerCard()!.textContent).toContain("take about 1.5–2.5 times as long as with llama3.1");
  });

  it("no offer for a cloud provider, or when the backend has none", async () => {
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(advice());
    await open();
    expect(offerCard()).toBeNull(); // the mock's provider is the cloud one
    act(() => root!.unmount());
    host!.remove();
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(advice({ offer: null }));
    await openLocal();
    expect(offerCard()).toBeNull();
  });

  it("the picker lists the measured models and the one in use needs no button", async () => {
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(advice({ offer: null }));
    await openLocal();
    const choices = $(".fly-model-choices")!;
    expect(choices.textContent).toContain("qwen3.5:9b");
    expect(choices.textContent).toContain("Declines what it can’t answer: 94%");
    expect(choices.textContent).toContain("Declines what it can’t answer: 75%");
    expect(choices.textContent).toContain("recommended for this computer");
    const nine = choices.querySelector('[data-model="qwen3.5:9b"]')!;
    const llama = choices.querySelector('[data-model="llama3.1"]')!;
    expect(llama.textContent).toContain("in use");
    expect([...llama.querySelectorAll("button")]).toHaveLength(0);
    expect(nine.querySelector("button")?.textContent).toBe("Download and use (6.6 GB)");
  });

  it("a new install sees the recommended model pre-selected, with its size", async () => {
    // The backend pinned the default to the 9B on this computer; Settings
    // shows it as the model Ask and Enhance need, and the confirm step names
    // the download size. Nothing downloads until the click.
    const llm = await api.getLlmSettings();
    vi.spyOn(api, "getLlmSettings").mockResolvedValue({
      ...llm,
      provider: "ollama",
      providers: llm.providers.map((p) =>
        p.id === "ollama" ? { ...p, default_model: "qwen3.5:9b", model: null } : p,
      ),
    });
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(
      advice({ offer: null, default_model: "qwen3.5:9b" }),
    );
    const pull = vi.spyOn(api, "ollamaPull");
    await open();
    expect(host!.textContent).toContain("qwen3.5:9b isn't downloaded yet.");
    expect(byText("button", "Download qwen3.5:9b (6.6 GB)")).not.toBeNull();
    expect(pull).not.toHaveBeenCalled();
  });

  it("another size of the same family is not the pinned model: not ready, not in use", async () => {
    // qwen3.5:4b is on the disk, the pinned default is qwen3.5:9b. The backend
    // matches tags exactly when both are explicit, and so must this screen:
    // otherwise it says "ready" with no Download, and Ask can't find the model.
    const llm = await api.getLlmSettings();
    vi.spyOn(api, "getLlmSettings").mockResolvedValue({
      ...llm,
      provider: "ollama",
      providers: llm.providers.map((p) =>
        p.id === "ollama" ? { ...p, default_model: "qwen3.5:9b", model: null } : p,
      ),
    });
    const status = await api.ollamaStatus();
    vi.spyOn(api, "ollamaStatus").mockResolvedValue({
      ...status,
      installed: true,
      running: true,
      models: [
        { name: "llama3.1:latest", size: 4_920_000_000, used_for: null },
        { name: "qwen3.5:4b", size: 3_330_000_000, used_for: null },
      ],
    });
    vi.spyOn(api, "localModelAdvice").mockResolvedValue(
      advice({ offer: null, default_model: "qwen3.5:9b" }),
    );
    await open();
    expect(host!.textContent).toContain("qwen3.5:9b isn't downloaded yet.");
    expect(host!.textContent).not.toContain("qwen3.5:9b is ready.");
    expect(byText("button", "Download qwen3.5:9b (6.6 GB)")).not.toBeNull();
    await click($('[aria-labelledby="settings-h-ai"] .fly-sgroup-more'));
    const four = $$(".fly-srow-below > div > div").find((r) =>
      r.textContent?.startsWith("qwen3.5:4b"),
    )!;
    expect(four.textContent).not.toContain("in use");
    expect([...four.querySelectorAll("button")].some((b) => b.textContent === "Delete")).toBe(true);
  });

  it("the dev mock keeps the two sizes apart too", async () => {
    // Pulling the 9B with the 4B on the disk adds the 9B (it used to be skipped
    // because the family matched).
    const names = async () => (await api.ollamaStatus()).models.map((m) => m.name);
    // (an earlier test may have deleted the 4B; leave the mock as it was found)
    const had4b = (await names()).includes("qwen3.5:4b");
    if (!had4b) await api.ollamaPull("qwen3.5:4b");
    expect(await names()).not.toContain("qwen3.5:9b");
    await api.ollamaPull("qwen3.5:9b");
    expect(await names()).toEqual(expect.arrayContaining(["qwen3.5:4b", "qwen3.5:9b"]));
    await api.ollamaDelete("qwen3.5:9b");
    if (!had4b) await api.ollamaDelete("qwen3.5:4b");
  });
});

describe("dev mock › local model advice mirrors the backend", () => {
  afterEach(() => {
    for (const k of ["fotwMockRam", "fotwMockOllamaVersion", "fotwMockDiskFull"])
      localStorage.removeItem(k);
  });

  it("names the offered models only", async () => {
    const a = await api.localModelAdvice();
    expect(a.models.map((m) => m.model)).toEqual(["qwen3.5:9b", "llama3.1", "qwen3.5:4b"]);
    expect(a.models.filter((m) => m.recommended)).toHaveLength(1);
    await expect(api.useLocalModel("gemma4:12b-it-qat")).rejects.toContain("isn't one of");
    await expect(api.dismissLocalModelOffer("rm -rf")).rejects.toContain("isn't one of");
  });

  it("its table is the backend's table, not a copy that can drift", async () => {
    // localModels.table.json is generated from local_models.rs CANDIDATES and
    // pinned equal by a Rust test; the mock reads the same file.
    const a = await api.localModelAdvice();
    expect(a.models.map((m) => [m.model, m.download_bytes, m.measured])).toEqual(
      TABLE.map((c) => [c.model, c.download_bytes, c.measured]),
    );
    // The 9B is the one that costs more after a meeting; llama3.1 and the 4B don't.
    expect(a.models.map((m) => m.measured.background_slower)).toEqual([[15, 25], null, null]);
  });

  it("recommends by memory at the cut-offs, and never by anything else", async () => {
    const pick = async (gb: string) => {
      localStorage.setItem("fotwMockRam", gb);
      return (await api.localModelAdvice()).recommended;
    };
    expect(await pick("32")).toBe("qwen3.5:9b");
    expect(await pick("16")).toBe("qwen3.5:9b");
    expect(await pick("12")).toBe("llama3.1");
    expect(await pick("8")).toBe("qwen3.5:4b");
  });

  it("an Ollama too old for the 9B holds it back, blocks a pinned 9B and refuses its download", async () => {
    localStorage.setItem("fotwMockOllamaVersion", "0.16.9");
    const a = await api.localModelAdvice();
    expect(a.held_back).toEqual({
      model: "qwen3.5:9b",
      needs_ollama: "0.17.5",
      have_ollama: "0.16.9",
    });
    expect(a.recommended).toBe("llama3.1");
    expect(a.models.find((m) => m.model === "qwen3.5:9b")!.needs_newer_ollama).toBe("0.17.5");
    expect(a.models.find((m) => m.model === "llama3.1")!.needs_newer_ollama).toBeNull();
    // Ollama's own words, which the Settings screen turns into "update Ollama".
    await expect(api.ollamaPull("qwen3.5:9b")).rejects.toMatch(/newer version of Ollama/);
  });

  it("a development build of Ollama (0.0.0) is not held back", async () => {
    localStorage.setItem("fotwMockOllamaVersion", "0.0.0");
    const a = await api.localModelAdvice();
    expect(a.held_back).toBeNull();
    expect(a.models.every((m) => m.needs_newer_ollama === null)).toBe(true);
  });

  it("a full drive refuses the download with the backend's words before anything starts", async () => {
    localStorage.setItem("fotwMockDiskFull", "1");
    await expect(api.ollamaPull("qwen3.5:9b")).rejects.toMatch(
      /Not enough free disk space for qwen3\.5:9b\. It needs about 8\.6 GB free \(a 6\.6 GB download plus 2\.0 GB to spare\)/,
    );
  });
});
