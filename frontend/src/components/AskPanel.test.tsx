// @vitest-environment jsdom
import { act, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { LlmSettings } from "../types";
import { AskComposer, AskSheet, useAskChat } from "./AskPanel";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const llm = (): LlmSettings => ({
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
  const ref = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <div style={{ position: "relative" }}>
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
  act(() => root!.render(ui));
  return { rerender: (next: React.ReactNode) => act(() => root!.render(next)) };
}

beforeEach(() => {
  vi.spyOn(api, "getLlmSettings").mockResolvedValue(llm());
  vi.spyOn(api, "getAppSetting").mockResolvedValue(null);
  vi.spyOn(api, "setAppSetting").mockResolvedValue(undefined);
});

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.restoreAllMocks();
});

const flush = async () => {
  for (let i = 0; i < 4; i++) await act(async () => {});
};
const input = () => host!.querySelector<HTMLInputElement>("form[aria-label=Ask] input")!;
const sheet = () => host!.querySelector<HTMLElement>('[data-testid="ask-sheet"]');
const button = (label: string) =>
  [...host!.querySelectorAll("button")].find((b) => b.textContent?.trim() === label);
const type = (value: string) => {
  const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  act(() => {
    set.call(input(), value);
    input().dispatchEvent(new Event("input", { bubbles: true }));
  });
};
const submit = () =>
  act(() => {
    input().form!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  });
const openSheet = async () => {
  act(() => input().click());
  await flush();
};

describe("Ask composer and sheet", () => {
  it("opens the sheet from the composer and leaves focus in it", async () => {
    render(<Harness noteId="n1" />);
    act(() => input().focus());
    await openSheet();
    expect(sheet()).not.toBeNull();
    expect(document.activeElement).toBe(input());
    expect(sheet()!.textContent).toContain("Not saved");
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
    // focus goes back to the composer, not the page
    expect(document.activeElement).toBe(input());
  });

  it("writes nothing until you click Insert into note", async () => {
    vi.spyOn(api, "askMeeting").mockResolvedValue("Send the SOW today.");
    const onInsert = vi.fn();
    render(<Harness noteId="n1" onInsert={onInsert} />);
    await openSheet();
    type("Next steps?");
    submit();
    await flush();
    expect(onInsert).not.toHaveBeenCalled();
    expect(sheet()!.textContent).toContain("Ask only changes your note when you click this.");
    act(() => button("Insert into note")!.click());
    expect(onInsert).toHaveBeenCalledWith("Send the SOW today.");
    expect(sheet()!.textContent).toContain("Inserted into your notes");
    expect(button("Insert into note")).toBeUndefined();
  });

  it("shows a human error, hands the question back, and retries", async () => {
    const ask = vi
      .spyOn(api, "askMeeting")
      .mockRejectedValueOnce(
        'this note is set to "Keep local", so it isn\'t sent to anthropic. Switch the AI provider to Ollama (runs on this device) in Settings, or turn off Keep local for this note',
      )
      .mockResolvedValueOnce("It's in the transcript.");
    render(<Harness noteId="n1" />);
    await openSheet();
    type("What did legal ask for?");
    submit();
    await flush();
    const err = host!.querySelector<HTMLElement>('[data-testid="ask-error"]')!;
    expect(err.textContent).toContain("This note is kept local");
    expect(err.textContent).toContain("Pick This computer under Runs on");
    expect(err.querySelector("details")!.textContent).toContain('set to "Keep local"');
    expect(input().value).toBe("What did legal ask for?");
    // the failed question was rolled back (the chat must end on your turn)
    expect(sheet()!.querySelector('[data-testid="ask-answer"]')).toBeNull();
    act(() => button("Try again")!.click());
    await flush();
    expect(ask).toHaveBeenLastCalledWith("n1", [
      { role: "user", content: "What did legal ask for?" },
    ]);
    expect(host!.querySelector('[data-testid="ask-error"]')).toBeNull();
    expect(sheet()!.textContent).toContain("It's in the transcript.");
  });

  it("drops a reply that lands after you switch notes", async () => {
    let resolve!: (s: string) => void;
    vi.spyOn(api, "askMeeting").mockReturnValue(new Promise((r) => (resolve = r)));
    const { rerender } = render(<Harness noteId="n1" />);
    await openSheet();
    type("Summary?");
    submit();
    rerender(<Harness noteId="n2" />);
    await act(async () => resolve("Answer for n1"));
    await flush();
    expect(host!.textContent).not.toContain("Answer for n1");
    expect(input().value).toBe("");
  });

  it("Esc closes the sheet", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    act(() => {
      input().dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
      );
    });
    expect(sheet()).toBeNull();
  });
});

describe("Runs on", () => {
  const runsOn = () => host!.querySelector<HTMLSelectElement>('[data-testid="ask-sheet"] select')!;
  const choose = (sel: HTMLSelectElement, value: string) =>
    act(() => {
      sel.value = value;
      sel.dispatchEvent(new Event("change", { bubbles: true }));
    });

  it("lists every provider and the Claude model", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    expect([...runsOn().options].map((o) => o.textContent)).toEqual([
      "This computer (Ollama)",
      "Anthropic (Claude)",
      "OpenAI",
      "NVIDIA NIM",
    ]);
    expect(runsOn().value).toBe("anthropic");
    const model = host!.querySelector<HTMLSelectElement>('select[aria-label="Claude model"]')!;
    expect([...model.options].map((o) => o.value)).toEqual([
      "claude-sonnet-5-5",
      "claude-haiku-5-5",
    ]);
  });

  it("asks for consent before a new cloud provider, and keeps yours if you decline", async () => {
    const save = vi.spyOn(api, "setLlmSettings").mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    render(<Harness noteId="n1" />);
    await openSheet();
    choose(runsOn(), "openai");
    await flush();
    expect(confirm.mock.calls[0][0]).toMatch(/^Use OpenAI for Ask and Enhance\?/);
    expect(save).not.toHaveBeenCalled();
    expect(runsOn().value).toBe("anthropic");
    expect(sheet()!.textContent).toContain("Kept your current provider");

    confirm.mockReturnValue(true);
    choose(runsOn(), "openai");
    await flush();
    expect(save).toHaveBeenCalledWith({
      provider: "openai",
      model: null,
      base_url: null,
      api_key: null,
    });
    expect(api.setAppSetting).toHaveBeenCalledWith("consent.cloud_llm.openai", "true");
    expect(runsOn().value).toBe("openai");
    expect(sheet()!.textContent).toContain("No API key for OpenAI yet");
  });

  it("switches to this computer without asking", async () => {
    const save = vi.spyOn(api, "setLlmSettings").mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, "confirm");
    render(<Harness noteId="n1" />);
    await openSheet();
    choose(runsOn(), "ollama");
    await flush();
    expect(confirm).not.toHaveBeenCalled();
    expect(save).toHaveBeenCalledWith(expect.objectContaining({ provider: "ollama" }));
    expect(sheet()!.textContent).toContain("llama3.1");
  });

  it("warns when a kept-local note meets a cloud provider", async () => {
    render(<Harness noteId="n1" keptLocal />);
    await openSheet();
    expect(sheet()!.textContent).toContain(
      "This note is kept local, so Ask won't send it to Anthropic.",
    );
  });

  it("offers Pin to side only when the window is wide enough", async () => {
    render(<Harness noteId="n1" />);
    await openSheet();
    expect(button("Pin to side")).toBeUndefined();
    act(() => root!.render(<Harness noteId="n1" canPin />));
    await flush();
    expect(button("Pin to side")).toBeTruthy();
  });
});
