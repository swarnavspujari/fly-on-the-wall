// @vitest-environment jsdom
import { act, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { AskReply, AskStreamEvent, LlmSettings, Note, Transcript } from "../types";
import { AskComposer, AskSheet, useAskChat } from "./AskPanel";
import type { OpenSource } from "./AskAnswer";
import { ConfirmProvider } from "./confirm";
import { answerConsent, consentRequest, holdConsentRequests } from "./cloudConsentTesting";
import { confirmChoice, confirmDialog, confirmTitle } from "./confirmTesting";

// The `ask:event` stream: tests fire events at the chat's listener.
const eventHandlers: Record<string, (e: { payload: unknown }) => void> = {};
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, handler: (e: { payload: unknown }) => void) => {
    eventHandlers[event] = handler;
    return () => delete eventHandlers[event];
  }),
}));
const stream = (askId: string, event: AskStreamEvent) =>
  act(() => eventHandlers["ask:event"]?.({ payload: { ask_id: askId, event } }));

/** An `ask_meeting` reply with this text (one unsourced sentence). */
const reply = (text: string, onDevice = true, extra: Partial<AskReply> = {}): AskReply => ({
  status: "answered",
  text,
  sentences: [{ text, citations: [], status: "unsourced" }],
  sources: [],
  proposals: [],
  refusals: [],
  trace_lines: [],
  on_device: onDevice,
  ...extra,
});

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

/** Synthetic meetings for @ and citations. */
const NOTES = [
  {
    id: "n1",
    title: "Renewal review",
    folder_id: null,
    meeting_id: "m1",
    updated_at: "",
    happened_at: "",
  },
  {
    id: "n4",
    title: "Q3 planning offsite",
    folder_id: "f1",
    meeting_id: "m4",
    updated_at: "",
    happened_at: "",
  },
  {
    id: "n9",
    title: "Notes only",
    folder_id: null,
    meeting_id: null,
    updated_at: "",
    happened_at: "",
  },
];
const FOLDERS = [{ id: "f1", name: "Clients", parent_id: null, created_at: "" }];
const seg = (id: string, speaker_key: string, start_ms: number, text: string) => ({
  id,
  speaker_key,
  start_ms,
  end_ms: start_ms + 5000,
  text,
  words: [],
});
const TRANSCRIPT: Transcript = {
  meeting_id: "m1",
  language: "en",
  engine: "test",
  speakers: [
    { key: "s1", label: "Dana" },
    { key: "mic", label: "You" },
  ],
  segments: [
    seg("a1", "s1", 4000, "Procurement flagged the renewal price."),
    seg("a2", "mic", 12000, "I'll send the revised SOW today."),
    seg("a3", "s1", 20000, "Legal needs the data-residency wording."),
  ],
};

// The saved settings, as Settings (or anything else) leaves them.
let saved: LlmSettings;
let consents: Record<string, string>;

function Harness({
  noteId,
  canPin = false,
  onNoteChanged = () => {},
  onOpenSource,
  keptLocal = false,
  people,
}: {
  /** null = Ask over every meeting (Home). */
  noteId: string | null;
  canPin?: boolean;
  onNoteChanged?: (n: Note) => void;
  onOpenSource?: OpenSource;
  keptLocal?: boolean;
  people?: string[];
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
        people={people}
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
          insertNoteId={noteId}
          onNoteChanged={onNoteChanged}
          onOpenSource={onOpenSource}
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
  vi.spyOn(api, "cancelAsk").mockResolvedValue(undefined);
  vi.spyOn(api, "getTranscript").mockResolvedValue(TRANSCRIPT);
  vi.spyOn(api, "listRecentNotes").mockResolvedValue(NOTES);
  vi.spyOn(api, "listFolders").mockResolvedValue(FOLDERS);
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
/** One meeting's cited lines, as the harness lists them. */
const SOURCE_M1 = {
  meeting_id: "m1",
  meeting_title: "Renewal review",
  started_at: null,
  label: "Renewal review",
  lines: [
    {
      segment_id: "a1",
      speaker: "Dana",
      start_ms: 4000,
      end_ms: 9000,
      snippet: "Procurement flagged the renewal price.",
    },
    {
      segment_id: "a2",
      speaker: "You",
      start_ms: 12000,
      end_ms: 17000,
      snippet: "I'll send the revised SOW today.",
    },
  ],
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
    const ask = vi.spyOn(api, "askMeeting").mockResolvedValue(reply("Two decisions."));
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
    expect(ask).toHaveBeenCalledWith(
      "n1",
      [{ role: "user", content: "What decisions were made?" }],
      { kind: "note", id: "n1" },
      expect.any(String),
    );
    expect(sheet()!.querySelector('[data-testid="ask-answer"]')!.textContent).toContain(
      "Two decisions.",
    );
    expect(document.activeElement).toBe(input());
    // announced politely, sheet open or not
    expect(host!.querySelector('[data-testid="ask-live"]')!.textContent).toBe("Answer 1 is ready.");
  });

  it("Enter sends; Shift+Enter adds a line", async () => {
    const ask = vi.spyOn(api, "askMeeting").mockResolvedValue(reply("ok"));
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
    expect(ask).toHaveBeenCalledWith(
      "n1",
      [{ role: "user", content: "first line\nsecond line" }],
      { kind: "note", id: "n1" },
      expect.any(String),
    );
  });

  it("sends once, however fast you press Enter", async () => {
    const ask = vi.spyOn(api, "askMeeting").mockResolvedValue(reply("ok"));
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

  it("writes nothing until you click Insert into note, then adds an AI block with its sources", async () => {
    vi.spyOn(api, "askMeeting").mockResolvedValue(
      reply("Send the SOW today.", true, {
        sentences: [{ text: "Send the SOW today.", citations: ["a2"], status: "sourced" }],
        sources: [SOURCE_M1],
      }),
    );
    const updated = { id: "n1" } as Note;
    const insert = vi.spyOn(api, "insertAskAnswer").mockResolvedValue(updated);
    const onNoteChanged = vi.fn();
    render(<Harness noteId="n1" onNoteChanged={onNoteChanged} />);
    await openSheet();
    type("Next steps?");
    key(input(), "Enter");
    await flush();
    expect(insert).not.toHaveBeenCalled();
    expect(sheet()!.textContent).toContain("Ask only changes your note when you click this.");
    act(() => button("Insert into note")!.click());
    await flush();
    expect(insert).toHaveBeenCalledWith("n1", "Send the SOW today.", ["a2"]);
    expect(onNoteChanged).toHaveBeenCalledWith(updated);
    expect(sheet()!.textContent).toContain("Added to your note as an AI block with its sources");
    expect(button("Insert into note")).toBeUndefined();
    expect(document.activeElement).toBe(input());
  });

  it("shows a human error naming the provider it went to, and retries", async () => {
    const ask = vi
      .spyOn(api, "askMeeting")
      .mockRejectedValueOnce(
        'this note is set to "Keep local", so it isn\'t sent to anthropic. Switch the AI provider to Ollama (runs on this device) in Settings, or turn off Keep local for this note',
      )
      .mockResolvedValueOnce(reply("It's in the transcript."));
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
    expect(ask).toHaveBeenLastCalledWith(
      "n1",
      [{ role: "user", content: "What did legal ask for?" }],
      { kind: "note", id: "n1" },
      expect.any(String),
    );
    expect(sheet()!.textContent).toContain("It's in the transcript.");
  });

  it("'isn't confirmed yet' offers to ask for consent, then asks again", async () => {
    delete consents["consent.cloud_llm.anthropic"];
    const ask = vi
      .spyOn(api, "askMeeting")
      .mockRejectedValueOnce(CONSENT_REFUSED)
      .mockResolvedValueOnce(reply("Answer after consent."));
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
    let resolve!: (r: AskReply) => void;
    vi.spyOn(api, "askMeeting").mockReturnValue(new Promise((r) => (resolve = r)));
    const { rerender } = render(<Harness noteId="n1" />);
    await openSheet();
    type("Summary?");
    key(input(), "Enter");
    rerender(<Harness noteId="n2" />);
    // the answer still being written for n1 is stopped
    expect(api.cancelAsk).toHaveBeenCalledTimes(1);
    await act(async () => resolve(reply("Answer for n1")));
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

/* ---------------------------------------------------------------- harness */

const live = () => host!.querySelector('[data-testid="ask-live"]')!.textContent;
const chips = () => [...sheet()!.querySelectorAll<HTMLElement>("[data-cite-status]")];
const scopeChip = () => host!.querySelector<HTMLButtonElement>('[data-testid="ask-scope-chip"]')!;
/** Send a question and hold its reply; returns the ask id and the resolver. */
async function askHeld(question: string) {
  let resolve!: (r: AskReply) => void;
  const ask = vi.spyOn(api, "askMeeting").mockReturnValue(
    new Promise<AskReply>((res) => {
      resolve = res;
    }),
  );
  type(question);
  key(input(), "Enter");
  await flush();
  const id = ask.mock.calls[ask.mock.calls.length - 1][3]!;
  return { ask, id, resolve: (r: AskReply) => act(async () => resolve(r)) };
}
const sourced = (text: string, citations: string[]) => ({
  text,
  citations,
  status: "sourced" as const,
});
const TRACE = {
  tier: "S" as const,
  scope: "note" as const,
  kept_local_not_searched: 0,
  meetings_in_scope: 1,
  passages: 6,
  items: 2,
  tool_calls: [],
  retried: false,
  context_truncated: false,
  floor_not_found: false,
  budget_forced: false,
};

describe("Ask harness answers", () => {
  it("streams the stage, what it found and the text so far, then the checked answer", async () => {
    render(<Harness noteId="n1" />);
    act(() => input().focus());
    await openSheet();
    const { id, resolve } = await askHeld("What did legal ask for?");
    stream(id, { type: "stage", stage: "searching" });
    expect(sheet()!.textContent).toContain("Searching this meeting…");
    stream(id, { type: "sources", sources: [SOURCE_M1], kept_local_not_searched: 0 });
    expect(sheet()!.textContent).toContain("Reading Renewal review");
    stream(id, { type: "turn_start", turn: 1 });
    stream(id, { type: "frame", turn: 1, text: "Legal needs the wording" });
    const streaming = sheet()!.querySelector('[data-testid="ask-streaming"]')!;
    expect(streaming.getAttribute("aria-busy")).toBe("true");
    expect(streaming.textContent).toContain("Legal needs the wording");
    // a newer model turn supersedes the old one's frames
    stream(id, { type: "turn_start", turn: 2 });
    stream(id, { type: "frame", turn: 1, text: "stale words" });
    expect(sheet()!.textContent).not.toContain("stale words");
    // another question's events go nowhere
    stream("someone-else", { type: "frame", turn: 2, text: "not ours" });
    expect(sheet()!.textContent).not.toContain("not ours");
    // streaming never takes focus or chatters in the live region
    expect(document.activeElement).toBe(input());
    expect(live()).toBe("");
    await resolve(
      reply("Legal needs the data-residency wording.", true, {
        sentences: [sourced("Legal needs the data-residency wording.", ["a1"])],
        sources: [SOURCE_M1],
        trace: TRACE,
      }),
    );
    await flush();
    expect(sheet()!.querySelector('[data-testid="ask-streaming"]')).toBeNull();
    // (no onOpenSource here, so the chip isn't a link)
    expect(chips()[0].getAttribute("aria-label")).toBe("1 source from Renewal review");
    expect(live()).toBe("Answer 1 is ready.");
  });

  it("Stop cancels the answer, keeps nothing from it and offers to ask again", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    const { ask, id, resolve } = await askHeld("Summary?");
    stream(id, { type: "frame", turn: 1, text: "Half an answer" });
    expect(button("Send")).toBeUndefined();
    act(() => host!.querySelector<HTMLButtonElement>('[data-testid="ask-stop"]')!.click());
    await flush();
    expect(api.cancelAsk).toHaveBeenCalledWith(id);
    expect(sheet()!.textContent).toContain("You stopped this answer. Nothing from it was kept.");
    expect(sheet()!.textContent).not.toContain("Half an answer");
    expect(document.activeElement).toBe(input());
    expect(live()).toContain("Stopped");
    // the reply that lands later is dropped
    await resolve(reply("Too late."));
    await flush();
    expect(sheet()!.textContent).not.toContain("Too late.");
    expect(host!.querySelector('[data-testid="ask-error"]')).toBeNull();
    // Ask again sends the same question over the same scope
    ask.mockResolvedValueOnce(reply("Fresh answer."));
    act(() => button("Ask again")!.click());
    await flush();
    expect(ask).toHaveBeenLastCalledWith(
      "n1",
      [{ role: "user", content: "Summary?" }],
      { kind: "note", id: "n1" },
      expect.any(String),
    );
    expect(sheet()!.textContent).toContain("Fresh answer.");
  });

  it("a backend 'cancelled' isn't shown as an error", async () => {
    vi.spyOn(api, "askMeeting").mockRejectedValue("Ask was cancelled.");
    render(<Harness noteId="n1" />);
    await openSheet();
    type("Summary?");
    key(input(), "Enter");
    await flush();
    expect(host!.querySelector('[data-testid="ask-error"]')).toBeNull();
    expect(sheet()!.textContent).toContain("You stopped this answer.");
  });

  it("'not found' has its own plain copy and offers to search all meetings", async () => {
    const ask = vi.spyOn(api, "askMeeting").mockResolvedValueOnce(
      reply("Not found in your meetings. (2 kept-local meetings were not searched.)", true, {
        status: "not_found",
        sentences: [],
        trace: { ...TRACE, passages: 0, items: 0, floor_not_found: true },
      }),
    );
    render(<Harness noteId="n1" />);
    await openSheet();
    type("Who mentioned the zebra?");
    key(input(), "Enter");
    await flush();
    const nf = sheet()!.querySelector('[data-testid="ask-not-found"]')!;
    expect(nf.textContent).toContain("Not found in this meeting");
    expect(nf.textContent).toContain("so it didn't guess");
    expect(sheet()!.textContent).not.toContain("2 kept-local");
    expect(button("Insert into note")).toBeUndefined();
    ask.mockResolvedValueOnce(reply("Elsewhere."));
    act(() => button("Search all meetings")!.click());
    await flush();
    expect(ask).toHaveBeenLastCalledWith(
      "n1",
      [
        { role: "user", content: "Who mentioned the zebra?" },
        expect.objectContaining({ role: "assistant" }),
        { role: "user", content: "Who mentioned the zebra?" },
      ],
      { kind: "all" },
      expect.any(String),
    );
    expect(scopeChip().textContent).toBe("All meetings");
  });

  it("a model failure gets its own plain copy and gives the question back", async () => {
    vi.spyOn(api, "askMeeting").mockRejectedValueOnce(
      "The model's answer was cut off before it finished. Try again, or ask a narrower question.",
    );
    render(<Harness noteId="n1" />);
    await openSheet();
    type("Everything about Q3?");
    key(input(), "Enter");
    await flush();
    const err = host!.querySelector<HTMLElement>('[data-testid="ask-error"]')!;
    expect(err.textContent).toContain("The answer was cut off");
    expect(err.textContent).toContain("Try again, or ask a narrower question.");
    expect(input().value).toBe("Everything about Q3?");
  });

  it("marks unsourced and flagged sentences, and says why in words", async () => {
    vi.spyOn(api, "askMeeting").mockResolvedValue(
      reply("…", true, {
        sentences: [
          sourced("Procurement flagged the price.", ["a1"]),
          {
            text: "They want $84,000.",
            citations: ["a1"],
            status: "flagged",
            flags: [{ kind: "number", detail: "84000" }],
          },
          { text: "They'll sign this month.", citations: [], status: "unsourced" },
        ],
        sources: [SOURCE_M1],
      }),
    );
    render(<Harness noteId="n1" />);
    await openSheet();
    type("Price?");
    key(input(), "Enter");
    await flush();
    const [ok, flagged, unsourced] = [
      ...sheet()!.querySelectorAll<HTMLElement>("[data-sentence-status]"),
    ];
    expect(ok.dataset.sentenceStatus).toBe("sourced");
    expect(ok.querySelector('[data-testid="ask-flag"]')).toBeNull();
    expect(flagged.querySelector('[data-testid="ask-flag"]')!.textContent).toBe(
      "Check: The number “84000” isn't in the lines it cites.",
    );
    const described = flagged.querySelector("[aria-describedby]")!;
    expect(
      document.getElementById(described.getAttribute("aria-describedby")!)!.textContent,
    ).toContain("84000");
    expect(unsourced.querySelector('[data-testid="ask-unsourced"]')!.textContent).toBe("No source");
    expect(unsourced.querySelector("[data-cite-status]")).toBeNull();
  });

  it("an unverified answer says so above its lines", async () => {
    vi.spyOn(api, "askMeeting").mockResolvedValue(
      reply("x", true, {
        status: "unverified",
        sentences: [
          { text: "x", citations: [], status: "flagged", flags: [{ kind: "uncited_claim" }] },
        ],
      }),
    );
    render(<Harness noteId="n1" />);
    await openSheet();
    type("?");
    key(input(), "Enter");
    await flush();
    expect(sheet()!.querySelector("[role=note]")!.textContent).toContain(
      "Ask couldn't tie this answer to your meetings",
    );
  });

  it("a chip opens the transcript at its lines, and shows when a line moved or changed", async () => {
    const onOpenSource = vi.fn();
    vi.spyOn(api, "askMeeting").mockResolvedValue(
      reply("…", true, {
        sentences: [
          sourced("The price came up.", ["a1"]),
          sourced("You'll send the SOW.", ["old-a2"]),
          sourced("Legal signed off.", ["gone"]),
        ],
        sources: [
          {
            ...SOURCE_M1,
            lines: [
              SOURCE_M1.lines[0],
              // re-transcribed since: same words, same speaker, new id
              { ...SOURCE_M1.lines[1], segment_id: "old-a2" },
              // the line now says something else
              {
                segment_id: "gone",
                speaker: "Dana",
                start_ms: 20000,
                end_ms: 25000,
                snippet: "Legal signed off on everything.",
              },
            ],
          },
        ],
      }),
    );
    render(<Harness noteId="n1" onOpenSource={onOpenSource} />);
    await openSheet();
    type("Status?");
    key(input(), "Enter");
    await flush();
    const [ok, moved, changed] = chips();
    expect(ok.dataset.citeStatus).toBe("ok");
    expect(ok.getAttribute("role")).toBe("button");
    expect(moved.dataset.citeStatus).toBe("reanchored");
    expect(moved.textContent).toBe("1 source · moved");
    expect(changed.dataset.citeStatus).toBe("changed");
    expect(changed.getAttribute("aria-label")).toContain("Source changed since Ask read it");
    expect(sheet()!.querySelector('[data-testid="ask-sources"]')!.textContent).toContain(
      "Source changed since Ask read it. Check it in the transcript.",
    );
    act(() => moved.click());
    await flush();
    expect(onOpenSource).toHaveBeenLastCalledWith({ meetingId: "m1", segmentIds: ["a2"] });
    key(ok, "Enter");
    await flush();
    expect(onOpenSource).toHaveBeenLastCalledWith({ meetingId: "m1", segmentIds: ["a1"] });
  });

  it("the trace pill starts collapsed, lists the steps, and holds no meeting text", async () => {
    saved.provider = "ollama";
    vi.spyOn(api, "askMeeting").mockResolvedValue(
      reply("…", true, {
        sentences: [sourced("Procurement flagged the price.", ["a1"])],
        sources: [SOURCE_M1],
        trace: TRACE,
        timing: { first_token_ms: 1200, total_ms: 9800 },
        run_log: {
          reach: "reach_on_device",
          provider: "provider_ollama",
          prefetch: { searches: 2, semantic: true, memory_facts: 3 },
        },
      }),
    );
    render(<Harness noteId="n1" />);
    await openSheet();
    type("Price?");
    key(input(), "Enter");
    await flush();
    const pill = sheet()!.querySelector<HTMLButtonElement>('[data-testid="ask-trace"]')!;
    expect(pill.textContent).toContain("Searched this meeting · read 6 passages · used memory");
    expect(pill.getAttribute("aria-expanded")).toBe("false");
    expect(sheet()!.querySelector('[data-testid="ask-trace-details"]')).toBeNull();
    act(() => pill.click());
    const details = sheet()!.querySelector('[data-testid="ask-trace-details"]')!;
    expect(pill.getAttribute("aria-controls")).toBe(details.id);
    for (const line of [
      "Ran 2 searches by keyword and meaning",
      "Read 6 passages and 2 extracted items",
      "Used 3 remembered facts",
      "Model: llama3.1",
      "One step: the model answered from what Ask found.",
      "Ran on this computer",
      "Answered in 9.8 s · first words after 1.2 s",
    ]) {
      expect(details.textContent).toContain(line);
    }
    expect(details.textContent).not.toContain("Procurement");
    const runsOn = sheet()!.querySelector('[data-testid="ask-runs-on"]')!;
    expect(runsOn.textContent).toContain("On this computer");
    expect(runsOn.textContent).toContain("one step");
  });

  it("says where a cloud answer ran and that it could look further", async () => {
    vi.spyOn(api, "askMeeting").mockResolvedValue(
      reply("…", false, {
        sentences: [sourced("Procurement flagged the price.", ["a1"])],
        sources: [SOURCE_M1],
        trace: { ...TRACE, tier: "C", tool_calls: [{ tool: "tool_search_passages", ok: true }] },
        run_log: { reach: "reach_cloud", provider: "provider_anthropic" },
      }),
    );
    render(<Harness noteId="n1" />);
    await openSheet();
    type("Price?");
    key(input(), "Enter");
    await flush();
    const runsOn = sheet()!.querySelector<HTMLElement>('[data-testid="ask-runs-on"]')!;
    expect(runsOn.textContent).toContain("Anthropic");
    expect(runsOn.textContent).toContain("multi-step");
    expect(runsOn.title).toBe("Multi-step: the model could search again, up to 8 times.");
    expect(sheet()!.querySelector('[data-testid="ask-trace"]')!.textContent).toContain(
      "looked further 1 time",
    );
  });

  it("a wide cloud search says kept-local meetings weren't searched, without a count", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    act(() => scopeChip().click());
    act(() => button("All meetings")!.click());
    const { id, resolve } = await askHeld("Who owns pricing?");
    stream(id, { type: "sources", sources: [SOURCE_M1], kept_local_not_searched: 3 });
    const note = () => sheet()!.querySelector('[data-testid="ask-kept-local"]')!;
    expect(note().textContent).toBe(
      "Kept-local meetings weren't searched. They never leave this computer.",
    );
    await resolve(
      reply("…", false, {
        sentences: [sourced("Dana owns pricing.", ["a1"])],
        sources: [SOURCE_M1],
        trace: {
          ...TRACE,
          tier: "C",
          scope: "all",
          meetings_in_scope: 9,
          kept_local_not_searched: 3,
        },
      }),
    );
    await flush();
    expect(note().textContent).not.toMatch(/\d/);
    expect(sheet()!.querySelector('[data-testid="ask-trace"]')!.textContent).toContain(
      "Searched 9 meetings",
    );
  });

  it("proposals are buttons that act once, only on a click", async () => {
    const soon = new Date(Date.now() + 600_000).toISOString();
    const past = new Date(Date.now() - 1000).toISOString();
    const proposal = {
      gate: "review" as const,
      meeting_id: "m1",
      note_id: "n1",
      segment_ids: ["a2"],
      executed: false,
    };
    vi.spyOn(api, "askMeeting").mockResolvedValue(
      reply("…", true, {
        sentences: [sourced("You'll send the SOW.", ["a2"])],
        sources: [SOURCE_M1],
        proposals: [
          {
            ...proposal,
            id: "p1",
            tool: "add_action_item",
            text: "Send the SOW",
            owner: "You",
            expires_at: soon,
          },
          {
            ...proposal,
            id: "p2",
            tool: "insert_into_note",
            markdown: "## Next\n- Send the SOW",
            expires_at: soon,
          },
          { ...proposal, id: "p3", tool: "add_action_item", text: "Old idea", expires_at: past },
        ],
        refusals: [
          { tool: "add_action_item", reason: "unknown_owner" },
          { tool: "add_action_item", reason: "duplicate" },
        ],
      }),
    );
    let done!: () => void;
    const add = vi
      .spyOn(api, "addActionItem")
      .mockReturnValue(new Promise((r) => (done = () => r({} as never))));
    const updated = { id: "n1" } as Note;
    const insert = vi.spyOn(api, "insertAskAnswer").mockResolvedValue(updated);
    const onNoteChanged = vi.fn();
    const { rerender } = render(<Harness noteId="n1" onNoteChanged={onNoteChanged} />);
    await openSheet();
    type("Next steps?");
    key(input(), "Enter");
    await flush();
    const cards = () => [...sheet()!.querySelectorAll<HTMLElement>('[data-testid="ask-proposal"]')];
    const cardButton = (i: number, label: string) =>
      [...cards()[i].querySelectorAll("button")].find((b) => b.textContent === label)!;
    expect(cards()).toHaveLength(3);
    expect(cards()[0].textContent).toContain("Owner: You");
    expect(add).not.toHaveBeenCalled();
    expect(insert).not.toHaveBeenCalled();
    // two quick clicks add it once
    const addBtn = cardButton(0, "Add action item");
    act(() => {
      addBtn.click();
      addBtn.click();
    });
    await flush();
    expect(add).toHaveBeenCalledTimes(1);
    expect(add).toHaveBeenCalledWith("m1", "Send the SOW", "You", ["a2"]);
    expect(cards()[0].querySelector("button")!.textContent).toBe("Adding…");
    expect(cards()[0].querySelector("button")!.disabled).toBe(true);
    await act(async () => done());
    await flush();
    expect(cards()[0].textContent).toContain("Added to action items");
    // the note insert
    act(() => cardButton(1, "Insert into note").click());
    await flush();
    expect(insert).toHaveBeenCalledWith("n1", "## Next\n- Send the SOW", ["a2"]);
    expect(onNoteChanged).toHaveBeenCalledWith(updated);
    // expired: can't be approved
    expect(cards()[2].querySelector("button")!.disabled).toBe(true);
    expect(cards()[2].textContent).toContain("This suggestion expired.");
    // a refusal is said, a duplicate isn't
    expect(sheet()!.querySelector('[data-testid="ask-refusals"]')!.textContent).toBe(
      "Ask suggested an action item but didn't offer it: its owner isn't someone in that meeting.",
    );
    // closing and reopening the sheet can't approve it again
    key(input(), "Escape");
    rerender(<Harness noteId="n1" onNoteChanged={onNoteChanged} />);
    await openSheet();
    expect(cards()[0].textContent).toContain("Added to action items");
    expect(add).toHaveBeenCalledTimes(1);
  });

  it("a failed proposal says nothing changed and can be tried again", async () => {
    const soon = new Date(Date.now() + 600_000).toISOString();
    vi.spyOn(api, "askMeeting").mockResolvedValue(
      reply("…", true, {
        sentences: [sourced("x", ["a2"])],
        sources: [SOURCE_M1],
        proposals: [
          {
            id: "p1",
            tool: "add_action_item",
            gate: "review",
            meeting_id: "m1",
            note_id: "n1",
            text: "Send the SOW",
            segment_ids: ["a2"],
            expires_at: soon,
            executed: false,
          },
        ],
      }),
    );
    vi.spyOn(api, "addActionItem").mockRejectedValueOnce(
      "an action item needs a source line from this meeting",
    );
    render(<Harness noteId="n1" />);
    await openSheet();
    type("?");
    key(input(), "Enter");
    await flush();
    act(() => button("Add action item")!.click());
    await flush();
    const card = sheet()!.querySelector('[data-testid="ask-proposal"]')!;
    expect(card.querySelector("[role=alert]")!.textContent).toContain(
      "Couldn't add it, so nothing changed.",
    );
    expect(button("Try again")!.disabled).toBe(false);
    expect(card.textContent).toContain("No owner");
  });

  it("@ suggests meetings, people and folders, and the pick becomes the scope", async () => {
    const ask = vi.spyOn(api, "askMeeting").mockResolvedValue(reply("ok"));
    render(<Harness noteId="n1" people={["Dana"]} />);
    await openSheet();
    type("@q3");
    await flush();
    const list = () => host!.querySelector<HTMLElement>('[data-testid="ask-mentions"]');
    expect(list()!.getAttribute("role")).toBe("listbox");
    expect(input().getAttribute("aria-controls")).toBe(list()!.id);
    expect(input().getAttribute("aria-activedescendant")).toBe(
      list()!.querySelector("[role=option]")!.id,
    );
    expect(list()!.querySelector("[role=group]")!.getAttribute("aria-label")).toBe("Meetings");
    expect(list()!.textContent).toContain("Q3 planning offsite");
    key(input(), "Enter");
    await flush();
    expect(list()).toBeNull();
    expect(scopeChip().textContent).toBe("@Q3 planning offsite");
    expect(input().value).toBe("");
    expect(ask).not.toHaveBeenCalled();
    type("What's still open?");
    key(input(), "Enter");
    await flush();
    expect(ask).toHaveBeenLastCalledWith(
      "n1",
      [{ role: "user", content: "What's still open?" }],
      { kind: "note", id: "n4" },
      expect.any(String),
    );
    expect(sheet()!.textContent).toContain("Searched: @Q3 planning offsite");
    // × goes back to this meeting
    act(() =>
      host!
        .querySelector<HTMLButtonElement>('[aria-label^="Stop searching Q3 planning offsite"]')!
        .click(),
    );
    expect(scopeChip().textContent).toBe("This meeting");
    // a person from this meeting, then a name typed freely
    type("@dan");
    await flush();
    expect(list()!.textContent).toContain("Dana");
    type("@Priya");
    await flush();
    const typed = [...list()!.querySelectorAll<HTMLElement>("[role=option]")].find(
      (o) => o.textContent === "Meetings with “Priya”",
    )!;
    act(() => typed.click());
    expect(scopeChip().textContent).toBe("@Priya");
    // Esc closes the suggestions, not the sheet
    type("@cli");
    await flush();
    key(input(), "Escape");
    expect(list()).toBeNull();
    expect(sheet()).not.toBeNull();
    // a folder, picked with the keyboard
    type("@clie");
    await flush();
    key(input(), "Tab");
    expect(scopeChip().textContent).toBe("@Clients");
    type("Status?");
    key(input(), "Enter");
    await flush();
    expect(ask.mock.calls[ask.mock.calls.length - 1][2]).toEqual({ kind: "folder", id: "f1" });
  });

  it("the scope chip switches between this meeting and all meetings", async () => {
    const ask = vi.spyOn(api, "askMeeting").mockResolvedValue(reply("ok"));
    render(<Harness noteId="n1" />);
    await openSheet();
    const chip = scopeChip();
    expect(chip.getAttribute("aria-label")).toBe(
      "Searches: This meeting. Change what Ask searches",
    );
    act(() => chip.click());
    expect(chip.getAttribute("aria-expanded")).toBe("true");
    const items = [...host!.querySelectorAll<HTMLElement>("[role=menuitemradio]")];
    expect(items.map((i) => [i.textContent, i.getAttribute("aria-checked")])).toEqual([
      ["This meeting", "true"],
      ["All meetings", "false"],
    ]);
    // Esc closes the menu, not the sheet
    key(items[0], "Escape");
    expect(host!.querySelector("[role=menu]")).toBeNull();
    expect(sheet()).not.toBeNull();
    act(() => chip.click());
    act(() => button("All meetings")!.click());
    expect(chip.textContent).toBe("All meetings");
    type("Who owns pricing?");
    key(input(), "Enter");
    await flush();
    expect(ask.mock.calls[0][2]).toEqual({ kind: "all" });
  });

  it("follow-ups send each earlier answer with where it ran and what it cited", async () => {
    const ask = vi
      .spyOn(api, "askMeeting")
      .mockResolvedValueOnce(
        reply("Dana owns it.", false, {
          sentences: [sourced("Dana owns it.", ["a1", "a2"])],
          sources: [SOURCE_M1],
        }),
      )
      .mockResolvedValueOnce(reply("Friday."));
    render(<Harness noteId="n1" />);
    await openSheet();
    type("Who owns pricing?");
    key(input(), "Enter");
    await flush();
    type("By when?");
    key(input(), "Enter");
    await flush();
    expect(ask.mock.calls[1][1]).toEqual([
      { role: "user", content: "Who owns pricing?" },
      { role: "assistant", content: "Dana owns it.", on_device: false, citations: ["a1", "a2"] },
      { role: "user", content: "By when?" },
    ]);
    // two different ask ids: each stream and Stop is its own
    expect(ask.mock.calls[0][3]).not.toBe(ask.mock.calls[1][3]);
  });

  it("Ask over every meeting: its own title and prompts, no Insert", async () => {
    const ask = vi.spyOn(api, "askMeeting").mockResolvedValue(
      reply("Dana owns pricing.", true, {
        sentences: [sourced("Dana owns pricing.", ["a1"])],
        sources: [SOURCE_M1],
      }),
    );
    render(<Harness noteId={null} />);
    await openSheet();
    expect(sheet()!.getAttribute("aria-label")).toBe("Ask your meetings");
    expect(scopeChip().textContent).toBe("All meetings");
    expect(button("What are my open action items?")).toBeTruthy();
    expect(button("Draft a follow-up email")).toBeUndefined();
    type("Who owns pricing?");
    key(input(), "Enter");
    await flush();
    expect(ask).toHaveBeenCalledWith(
      "",
      [{ role: "user", content: "Who owns pricing?" }],
      { kind: "all" },
      expect.any(String),
    );
    expect(sheet()!.textContent).toContain("Dana owns pricing.");
    expect(button("Insert into note")).toBeUndefined();
  });

  it("the composer says where Ask runs before the first question", async () => {
    render(<Harness noteId="n1" />);
    await flush();
    const label = () => host!.querySelector<HTMLElement>('[data-testid="ask-runs-on-label"]')!;
    expect(label().textContent).toBe("Anthropic");
    saved.provider = "ollama";
    act(() => window.dispatchEvent(new Event("fotw:llm-settings-changed")));
    await flush();
    expect(label().textContent).toBe("On this computer");
  });
});
