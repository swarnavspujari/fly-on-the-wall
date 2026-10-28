// @vitest-environment jsdom
import { act, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { LlmSettings } from "../types";
import { AskComposer, AskSheet, useAskChat } from "./AskPanel";
import { ConfirmProvider } from "./confirm";
import { answerConsent, consentRequest, holdConsentRequests } from "./cloudConsentTesting";
import { confirmChoice, confirmDialog, confirmTitle } from "./confirmTesting";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const fixture = (): LlmSettings => ({
  provider: "anthropic",
  providers: [
    {
      id: "ollama",
      default_model: "llama3.1",
      is_local: true,
      has_key: true,
      model: null,
      base_url: null,
    },
    {
      id: "anthropic",
      default_model: "claude-sonnet-5-5",
      is_local: false,
      has_key: true,
      model: "claude-sonnet-5-5",
      base_url: null,
    },
    {
      id: "openai",
      default_model: "gpt-4o",
      is_local: false,
      has_key: false,
      model: null,
      base_url: null,
    },
    {
      id: "nim",
      default_model: "meta/llama-3.1-70b-instruct",
      is_local: false,
      has_key: false,
      model: null,
      base_url: null,
    },
  ],
});

// The saved settings, as Settings (or anything else) leaves them.
let saved: LlmSettings;
let consents: Record<string, string>;

function Harness({
  noteId,
  canPin = false,
  onInsert = () => {},
  keptLocal = false,
}: {
  noteId: string;
  canPin?: boolean;
  onInsert?: (s: string) => void;
  keptLocal?: boolean;
}) {
  const chat = useAskChat(noteId);
  const ref = useRef<HTMLTextAreaElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <div style={{ position: "relative" }}>
      <textarea data-testid="note-text" />
      <AskComposer
        chat={chat}
        inputRef={ref}
        open={open}
        onOpen={() => setOpen(true)}
        placeholder="Ask about this meeting"
      />
      {open && (
        <AskSheet
          chat={chat}
          inputRef={ref}
          hasMeeting
          keptLocal={keptLocal}
          pinned={false}
          canPin={canPin}
          onTogglePin={() => {}}
          onClose={() => setOpen(false)}
          onInsert={onInsert}
        />
      )}
    </div>
  );
}

let root: Root | null = null;
let host: HTMLElement | null = null;

function render(ui: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<ConfirmProvider>{ui}</ConfirmProvider>));
  return {
    rerender: (next: React.ReactNode) =>
      act(() => root!.render(<ConfirmProvider>{next}</ConfirmProvider>)),
  };
}

beforeEach(() => {
  saved = fixture();
  consents = { "consent.cloud_llm.anthropic": "true" };
  vi.spyOn(api, "getLlmSettings").mockImplementation(async () => structuredClone(saved));
  vi.spyOn(api, "setLlmSettings").mockImplementation(async (u) => {
    saved.provider = u.provider;
    saved.providers = saved.providers.map((p) =>
      p.id === u.provider ? { ...p, model: u.model, base_url: u.base_url } : p,
    );
  });
  vi.spyOn(api, "getAppSetting").mockImplementation(async (k) => consents[k] ?? null);
  // as in the backend, consent can't be set from here: request_cloud_consent
  // records it
  vi.spyOn(api, "setAppSetting").mockImplementation(async (k, v) => {
    if (k.startsWith("consent.cloud_llm.")) throw "can't be changed from the app UI";
    consents[k] = v;
  });
  holdConsentRequests((p) => {
    consents[`consent.cloud_llm.${p}`] = "true";
  });
});

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.restoreAllMocks();
});

const flush = async () => {
  for (let i = 0; i < 5; i++) await act(async () => {});
};
const input = () => host!.querySelector<HTMLTextAreaElement>("form[aria-label=Ask] textarea")!;
const sheet = () => host!.querySelector<HTMLElement>('[data-testid="ask-sheet"]');
const button = (label: string) =>
  [...host!.querySelectorAll("button")].find((b) => b.textContent?.trim() === label);
const type = (value: string) => {
  const set = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
  act(() => {
    set.call(input(), value);
    input().dispatchEvent(new Event("input", { bubbles: true }));
  });
};
const key = (target: Element, k: string, init: KeyboardEventInit = {}) => {
  const e = new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...init });
  act(() => {
    target.dispatchEvent(e);
  });
  return e;
};
const openSheet = async () => {
  act(() => input().click());
  await flush();
};
const answer = async (yes: boolean) => {
  act(() => confirmChoice(yes).click());
  await flush();
};
/** Answer the (native) cloud-consent dialog. */
const consent = async (yes: boolean) => {
  await act(async () => answerConsent(yes));
  await flush();
};
const CONSENT_REFUSED =
  "Anthropic isn't allowed to receive your meeting content yet, so nothing was sent. To allow it, open Settings › Ask and Enhance and click Test. To keep everything on this computer, switch Runs on to This computer (Ollama).";

describe("Ask composer and sheet", () => {
  it("opens on a click, not on focus, and leaves focus in the composer", async () => {
    render(<Harness noteId="n1" />);
    // tabbing in (focus with a relatedTarget) doesn't open it
    act(() => {
      input().dispatchEvent(
        new FocusEvent("focusin", { bubbles: true, relatedTarget: host!.firstElementChild }),
      );
      input().focus();
    });
    expect(sheet()).toBeNull();
    await openSheet();
    expect(sheet()).not.toBeNull();
    expect(document.activeElement).toBe(input());
    expect(input().getAttribute("aria-controls")).toBe(sheet()!.id);
    expect(sheet()!.textContent).toContain("Not saved");
  });

  it("typing opens it too", async () => {
    render(<Harness noteId="n1" />);
    type("w");
    await flush();
    expect(sheet()).not.toBeNull();
  });

  it("keeps the quick prompts, and a prompt asks the meeting", async () => {
    const ask = vi.spyOn(api, "askMeeting").mockResolvedValue("Two decisions.");
    render(<Harness noteId="n1" />);
    await openSheet();
    for (const q of [
      "What did I miss?",
      "What decisions were made?",
      "List the action items",
      "Draft a follow-up email",
    ]) {
      expect(button(q)).toBeTruthy();
    }
    act(() => button("What decisions were made?")!.click());
    await flush();
    expect(ask).toHaveBeenCalledWith("n1", [
      { role: "user", content: "What decisions were made?" },
    ]);
    expect(sheet()!.querySelector('[data-testid="ask-answer"]')!.textContent).toContain(
      "Two decisions.",
    );
    expect(document.activeElement).toBe(input());
    // announced politely, sheet open or not
    expect(host!.querySelector('[data-testid="ask-live"]')!.textContent).toBe("Answer 1 is ready.");
  });

  it("Enter sends; Shift+Enter adds a line", async () => {
    const ask = vi.spyOn(api, "askMeeting").mockResolvedValue("ok");
    render(<Harness noteId="n1" />);
    await openSheet();
    type("first line");
    const shift = key(input(), "Enter", { shiftKey: true });
    expect(shift.defaultPrevented).toBe(false); // the textarea adds the newline
    expect(ask).not.toHaveBeenCalled();
    type("first line\nsecond line");
    const enter = key(input(), "Enter");
    expect(enter.defaultPrevented).toBe(true);
    await flush();
    expect(ask).toHaveBeenCalledWith("n1", [{ role: "user", content: "first line\nsecond line" }]);
  });

  it("sends once, however fast you press Enter", async () => {
    const ask = vi.spyOn(api, "askMeeting").mockResolvedValue("ok");
    render(<Harness noteId="n1" />);
    await openSheet();
    type("Summary?");
    act(() => {
      input().dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
      );
      input().dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
      );
    });
    act(() => button("What did I miss?")?.click());
    await flush();
    expect(ask).toHaveBeenCalledTimes(1);
  });

  it("writes nothing until you click Insert into note", async () => {
    vi.spyOn(api, "askMeeting").mockResolvedValue("Send the SOW today.");
    const onInsert = vi.fn();
    render(<Harness noteId="n1" onInsert={onInsert} />);
    await openSheet();
    type("Next steps?");
    key(input(), "Enter");
    await flush();
    expect(onInsert).not.toHaveBeenCalled();
    expect(sheet()!.textContent).toContain("Ask only changes your note when you click this.");
    act(() => button("Insert into note")!.click());
    expect(onInsert).toHaveBeenCalledWith("Send the SOW today.");
    expect(sheet()!.textContent).toContain("Inserted into your notes");
    expect(button("Insert into note")).toBeUndefined();
  });

  it("shows a human error naming the provider it went to, and retries", async () => {
    const ask = vi
      .spyOn(api, "askMeeting")
      .mockRejectedValueOnce(
        'this note is set to "Keep local", so it isn\'t sent to anthropic. Switch the AI provider to Ollama (runs on this device) in Settings, or turn off Keep local for this note',
      )
      .mockResolvedValueOnce("It's in the transcript.");
    render(<Harness noteId="n1" />);
    await openSheet();
    type("What did legal ask for?");
    key(input(), "Enter");
    await flush();
    // Settings changes the provider afterwards: the error still names Anthropic.
    saved.provider = "ollama";
    act(() => {
      host!.querySelector<HTMLSelectElement>('[data-testid="ask-sheet"] select')!.focus();
    });
    await flush();
    const err = host!.querySelector<HTMLElement>('[data-testid="ask-error"]')!;
    expect(err.textContent).toContain("This note is kept local");
    expect(err.textContent).toContain("Ask won't send it to Anthropic");
    expect(err.querySelector("details")!.textContent).toContain('set to "Keep local"');
    expect(input().value).toBe("What did legal ask for?");
    expect(sheet()!.querySelector('[data-testid="ask-answer"]')).toBeNull();
    expect(host!.querySelector('[data-testid="ask-live"]')!.textContent).toContain(
      "couldn't answer",
    );
    act(() => button("Try again")!.click());
    await flush();
    expect(ask).toHaveBeenLastCalledWith("n1", [
      { role: "user", content: "What did legal ask for?" },
    ]);
    expect(sheet()!.textContent).toContain("It's in the transcript.");
  });

  it("'isn't confirmed yet' offers to ask for consent, then asks again", async () => {
    delete consents["consent.cloud_llm.anthropic"];
    const ask = vi
      .spyOn(api, "askMeeting")
      .mockRejectedValueOnce(CONSENT_REFUSED)
      .mockResolvedValueOnce("Answer after consent.");
    render(<Harness noteId="n1" />);
    await openSheet();
    type("Status?");
    key(input(), "Enter");
    await flush();
    expect(button("Try again")).toBeUndefined();
    // Cancel writes nothing and doesn't ask the question again
    act(() => button("Review and allow Anthropic")!.click());
    await flush();
    expect(consentRequest()).toEqual({ provider: "anthropic", atLaunch: false });
    expect(consents["consent.cloud_llm.anthropic"]).toBeUndefined();
    await consent(false);
    expect(consentRequest()).toBeNull();
    expect(consents["consent.cloud_llm.anthropic"]).toBeUndefined();
    expect(ask).toHaveBeenCalledTimes(1);
    // Allow: consent is saved, then the question goes again
    act(() => button("Review and allow Anthropic")!.click());
    await flush();
    await consent(true);
    expect(consents["consent.cloud_llm.anthropic"]).toBe("true");
    expect(ask).toHaveBeenCalledTimes(2);
    expect(sheet()!.textContent).toContain("Answer after consent.");
  });

  it("drops a reply that lands after you switch notes", async () => {
    let resolve!: (s: string) => void;
    vi.spyOn(api, "askMeeting").mockReturnValue(new Promise((r) => (resolve = r)));
    const { rerender } = render(<Harness noteId="n1" />);
    await openSheet();
    type("Summary?");
    key(input(), "Enter");
    rerender(<Harness noteId="n2" />);
    await act(async () => resolve("Answer for n1"));
    await flush();
    expect(host!.textContent).not.toContain("Answer for n1");
    expect(input().value).toBe("");
  });

  it("Esc closes the sheet", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    key(input(), "Escape");
    expect(sheet()).toBeNull();
  });

  it("Esc while typing elsewhere closes the sheet without stealing focus or reopening", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    const note = host!.querySelector<HTMLTextAreaElement>('[data-testid="note-text"]')!;
    act(() => note.focus());
    key(note, "Escape");
    await flush();
    expect(sheet()).toBeNull();
    expect(document.activeElement).toBe(note);
  });
});

describe("Runs on", () => {
  const runsOn = () => host!.querySelector<HTMLSelectElement>('[data-testid="ask-sheet"] select')!;
  const model = () => host!.querySelector<HTMLSelectElement>('select[aria-label="Claude model"]');
  const choose = (sel: HTMLSelectElement, value: string) =>
    act(() => {
      sel.value = value;
      sel.dispatchEvent(new Event("change", { bubbles: true }));
    });

  it("lists every provider and the Claude model, and says it's not just Ask's", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    expect([...runsOn().options].map((o) => o.textContent)).toEqual([
      "This computer (Ollama)",
      "Anthropic (Claude)",
      "OpenAI",
      "NVIDIA NIM",
    ]);
    expect(runsOn().value).toBe("anthropic");
    expect([...model()!.options].map((o) => o.value)).toEqual([
      "claude-sonnet-5-5",
      "claude-haiku-5-5",
    ]);
    const helper = sheet()!.textContent!;
    expect(helper).toContain("Also used for Enhance, transcript cleanup and action items.");
    const describedBy = runsOn().getAttribute("aria-describedby")!.split(" ");
    expect(document.getElementById(describedBy[0])!.textContent).toContain("Also used for");
  });

  it("asks for consent before a new cloud provider, and keeps yours if you decline", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    choose(runsOn(), "openai");
    await flush();
    // it waits for the answer: nothing is saved while the question is open
    expect(consentRequest()).toEqual({ provider: "openai", atLaunch: false });
    expect(consents["consent.cloud_llm.openai"]).toBeUndefined();
    expect(api.setLlmSettings).not.toHaveBeenCalled();
    await consent(false);
    expect(consents["consent.cloud_llm.openai"]).toBeUndefined();
    expect(api.setLlmSettings).not.toHaveBeenCalled();
    expect(runsOn().value).toBe("anthropic");
    expect(sheet()!.textContent).toContain("Kept your current provider");

    choose(runsOn(), "openai");
    await flush();
    await consent(true);
    expect(api.setLlmSettings).toHaveBeenCalledWith({
      provider: "openai",
      model: null,
      base_url: null,
      api_key: null,
    });
    expect(consents["consent.cloud_llm.openai"]).toBe("true");
    expect(runsOn().value).toBe("openai");
    expect(sheet()!.textContent).toContain(
      "Ask, Enhance, transcript cleanup and action items now run on OpenAI.",
    );
    expect(sheet()!.textContent).toContain("No API key for OpenAI yet");
  });

  it("declining its consent question keeps the sheet open, with focus where it was", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    act(() => runsOn().focus());
    await flush();
    choose(runsOn(), "openai");
    await flush();
    expect(consentRequest()).toEqual({ provider: "openai", atLaunch: false });
    await consent(false);
    expect(consentRequest()).toBeNull();
    expect(sheet()).not.toBeNull();
    expect(document.activeElement).toBe(runsOn());
    expect(consents["consent.cloud_llm.openai"]).toBeUndefined();
    expect(api.setLlmSettings).not.toHaveBeenCalled();
  });

  it("switches to this computer without asking", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    choose(runsOn(), "ollama");
    await flush();
    expect(confirmDialog()).toBeNull();
    expect(api.requestCloudConsent).not.toHaveBeenCalled();
    expect(api.setLlmSettings).toHaveBeenCalledWith(
      expect.objectContaining({ provider: "ollama" }),
    );
    expect(sheet()!.textContent).toContain("llama3.1");
  });

  it("a stale sheet never undoes a provider change made in Settings", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    expect(runsOn().value).toBe("anthropic");
    // meanwhile, Settings switches to Ollama
    saved.provider = "ollama";
    choose(model()!, "claude-haiku-5-5");
    await flush();
    expect(confirmTitle()).toBe("Switch to Anthropic (Claude)?");
    expect(confirmDialog()!.textContent).toContain("changed in Settings");
    await answer(false);
    expect(api.setLlmSettings).not.toHaveBeenCalled();
    expect(saved.provider).toBe("ollama");
    expect(runsOn().value).toBe("ollama"); // the select now matches what's saved
  });

  it("a stale sheet switches back only when you say so", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    saved.provider = "ollama";
    choose(model()!, "claude-haiku-5-5");
    await flush();
    expect(api.setLlmSettings).not.toHaveBeenCalled();
    await answer(true);
    expect(api.setLlmSettings).toHaveBeenCalledWith({
      provider: "anthropic",
      model: "claude-haiku-5-5",
      base_url: null,
      api_key: null,
    });
    expect(sheet()!.textContent).toContain("now run on Anthropic (Claude) · Claude Haiku 5.5");
  });

  it("a Claude model pick asks for consent when it isn't recorded", async () => {
    delete consents["consent.cloud_llm.anthropic"];
    render(<Harness noteId="n1" />);
    await openSheet();
    choose(model()!, "claude-haiku-5-5");
    await flush();
    expect(consentRequest()).toEqual({ provider: "anthropic", atLaunch: false });
    await consent(false);
    expect(consents["consent.cloud_llm.anthropic"]).toBeUndefined();
    expect(api.setLlmSettings).not.toHaveBeenCalled();
    expect(model()!.value).toBe("claude-sonnet-5-5");
  });

  it("says so when saving fails, and keeps what's saved", async () => {
    vi.mocked(api.setLlmSettings).mockRejectedValueOnce("keychain locked");
    render(<Harness noteId="n1" />);
    await openSheet();
    choose(runsOn(), "ollama");
    await flush();
    expect(sheet()!.textContent).toContain("Couldn't switch, so nothing changed.");
    expect(runsOn().value).toBe("anthropic");
  });

  it("writes nothing when it can't re-read what's saved", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    vi.mocked(api.getLlmSettings).mockRejectedValue("database is locked");
    choose(runsOn(), "ollama");
    await flush();
    choose(model()!, "claude-haiku-5-5");
    await flush();
    expect(api.setLlmSettings).not.toHaveBeenCalled();
    expect(confirmDialog()).toBeNull();
    expect(sheet()!.textContent).toContain(
      "Couldn't read your current provider, so nothing changed.",
    );
    expect(runsOn().value).toBe("anthropic");
  });

  it("re-reads the provider when the pointer comes back to the sheet", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    expect(runsOn().value).toBe("anthropic");
    saved.provider = "ollama"; // changed in Settings with the sheet open
    act(() => {
      sheet()!.dispatchEvent(new PointerEvent("pointerover", { bubbles: true }));
    });
    await flush();
    expect(runsOn().value).toBe("ollama");
  });

  it("is disabled while a question is running", async () => {
    vi.spyOn(api, "askMeeting").mockReturnValue(new Promise(() => {}));
    render(<Harness noteId="n1" />);
    await openSheet();
    type("Still going?");
    key(input(), "Enter");
    await flush();
    expect(runsOn().disabled).toBe(true);
    expect(model()!.disabled).toBe(true);
  });

  it("warns when a kept-local note meets a cloud provider", async () => {
    render(<Harness noteId="n1" keptLocal />);
    await openSheet();
    const text = "This note is kept local, so Ask won't send it to Anthropic.";
    expect(sheet()!.textContent).toContain(text);
    const ids = runsOn().getAttribute("aria-describedby")!.split(" ");
    expect(ids.map((id) => document.getElementById(id)!.textContent)).toContain(text);
  });

  it("an Ollama server on another machine isn't 'this computer'", async () => {
    saved.provider = "ollama";
    saved.providers = saved.providers.map((p) =>
      p.id === "ollama" ? { ...p, base_url: "http://192.168.1.20:11434" } : p,
    );
    render(<Harness noteId="n1" keptLocal />);
    await openSheet();
    expect(sheet()!.textContent).toContain("Off this computer · 192.168.1.20:11434");
    expect(sheet()!.textContent).toContain(
      "This note is kept local, so Ask won't send it to another computer.",
    );
  });

  it("offers Pin to side only when the window is wide enough", async () => {
    const { rerender } = render(<Harness noteId="n1" />);
    await openSheet();
    expect(button("Pin to side")).toBeUndefined();
    rerender(<Harness noteId="n1" canPin />);
    await flush();
    expect(button("Pin to side")).toBeTruthy();
  });
});
