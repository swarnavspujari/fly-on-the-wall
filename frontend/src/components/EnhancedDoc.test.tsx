// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { Note, Transcript } from "../types";
import EnhancedDoc from "./EnhancedDoc";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const note: Note = {
  id: "n1",
  title: "Launch sync",
  folder_id: null,
  meeting_id: "m1",
  scratchpad: "",
  attachments: [],
  created_at: "2026-10-27T10:00:00Z",
  updated_at: "2026-10-27T10:00:00Z",
  blocks: [
    { id: "b1", origin: { kind: "user" }, markdown: "Pricing needs a decision." },
    {
      id: "b2",
      origin: { kind: "ai", source_segment_ids: ["t1"] },
      markdown: "Dana will send two pricing options by Thursday.",
    },
    {
      id: "b3",
      origin: { kind: "ai", source_segment_ids: [] },
      markdown: "Launch likely moves to May.",
    },
  ],
};

const transcript: Transcript = {
  meeting_id: "m1",
  language: "en",
  engine: "whisper",
  speakers: [{ key: "s1", label: "Dana" }],
  segments: [
    {
      id: "t1",
      speaker_key: "s1",
      start_ms: 842_000,
      end_ms: 850_000,
      text: "I can put together two pricing options by Thursday.",
      words: [],
    },
  ],
};

let root: Root | null = null;
let host: HTMLElement | null = null;

function render(props: Partial<Parameters<typeof EnhancedDoc>[0]> = {}) {
  const onNoteChanged = vi.fn();
  const onZoom = vi.fn();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <EnhancedDoc
        note={note}
        onNoteChanged={onNoteChanged}
        onZoom={onZoom}
        transcripts={[null, transcript]}
        {...props}
      />,
    ),
  );
  return { onNoteChanged, onZoom };
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.restoreAllMocks();
});

const button = (scope: HTMLElement, label: string) =>
  [...scope.querySelectorAll("button")].find((b) => b.textContent === label)!;
const flush = async () => {
  for (let i = 0; i < 3; i++) await act(async () => {});
};

describe("EnhancedDoc trust", () => {
  it("shows one legend and no per-block AI labels", () => {
    render();
    expect(host!.textContent).toContain("2 AI lines · 1 without a source");
    const legend = host!.querySelector('[aria-label="Legend"]')!;
    expect(legend.textContent).toContain("You");
    expect(legend.textContent).toContain("AI, sourced");
    expect(legend.textContent).toContain("AI, no source");
    // the old uppercase "AI" tag on each block is gone
    const tags = [...host!.querySelectorAll("span")].filter((s) => s.textContent === "AI");
    expect(tags).toHaveLength(0);
  });

  it("flags an unsourced AI line without hover, with Keep and Remove", () => {
    render();
    const unsourced = host!.querySelector<HTMLElement>('[data-trust="unsourced"]')!;
    expect(unsourced.textContent).toContain("Launch likely moves to May.");
    expect(unsourced.textContent).toContain("No source");
    expect(unsourced.textContent).not.toMatch(/made up/i);
    // visible by default: not inside the hover-only row
    const label = [...unsourced.querySelectorAll("span")].find((s) =>
      s.textContent?.startsWith("No source"),
    )!;
    expect(label.closest(".hidden")).toBeNull();
    expect(button(unsourced, "Keep")).toBeTruthy();
    expect(button(unsourced, "Remove")).toBeTruthy();
    // sourced and user blocks get neither
    for (const sel of ['[data-trust="sourced"]', '[data-trust="user"]']) {
      const el = host!.querySelector<HTMLElement>(sel)!;
      expect(button(el, "Keep")).toBeUndefined();
      expect(el.textContent).not.toContain("No source");
    }
  });

  it("Keep makes the line yours through keep_note_block", async () => {
    const kept: Note = {
      ...note,
      blocks: note.blocks.map((b) => (b.id === "b3" ? { ...b, origin: { kind: "user" } } : b)),
    };
    const keep = vi.spyOn(api, "keepNoteBlock").mockResolvedValue(kept);
    const { onNoteChanged } = render();
    const unsourced = host!.querySelector<HTMLElement>('[data-trust="unsourced"]')!;
    act(() => button(unsourced, "Keep").click());
    await flush();
    expect(keep).toHaveBeenCalledWith("n1", "b3");
    expect(onNoteChanged).toHaveBeenCalledWith(kept);
  });

  it("Remove deletes the line by editing it to empty text", async () => {
    const removed: Note = { ...note, blocks: note.blocks.filter((b) => b.id !== "b3") };
    const edit = vi.spyOn(api, "editNoteBlock").mockResolvedValue(removed);
    const { onNoteChanged } = render();
    const unsourced = host!.querySelector<HTMLElement>('[data-trust="unsourced"]')!;
    act(() => button(unsourced, "Remove").click());
    await flush();
    expect(edit).toHaveBeenCalledWith("n1", "b3", "");
    expect(onNoteChanged).toHaveBeenCalledWith(removed);
  });

  it("says so when Keep fails, and leaves the line alone", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(api, "keepNoteBlock").mockRejectedValue("database is locked");
    const { onNoteChanged } = render();
    const unsourced = host!.querySelector<HTMLElement>('[data-trust="unsourced"]')!;
    act(() => button(unsourced, "Keep").click());
    await flush();
    expect(onNoteChanged).not.toHaveBeenCalled();
    expect(unsourced.querySelector('[role="alert"]')!.textContent).toContain(
      "Couldn't keep this line",
    );
  });

  it("previews the cited quote on hover; a click still opens the transcript", () => {
    const { onZoom } = render();
    const sourced = host!.querySelector<HTMLElement>('[data-trust="sourced"]')!;
    const chip = sourced.querySelector<HTMLElement>(".fly-cite")!;
    expect(host!.querySelector('[data-testid="source-preview"]')).toBeNull();
    act(() => {
      chip.parentElement!.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
    });
    const preview = host!.querySelector<HTMLElement>('[data-testid="source-preview"]')!;
    expect(preview.textContent).toContain("Dana");
    expect(preview.textContent).toContain("14:02");
    expect(preview.textContent).toContain("two pricing options by Thursday");
    act(() => chip.click());
    expect(onZoom).toHaveBeenCalledWith(["t1"]);
  });

  it("keyboard focus opens the preview and Esc closes it", () => {
    render();
    const chip = host!.querySelector<HTMLElement>('[data-trust="sourced"] .fly-cite')!;
    act(() => chip.focus());
    expect(host!.querySelector('[data-testid="source-preview"]')).not.toBeNull();
    expect(chip.getAttribute("aria-describedby")).toBeTruthy();
    act(() => {
      chip.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
      );
    });
    expect(host!.querySelector('[data-testid="source-preview"]')).toBeNull();
  });

  it("says when a cited line is no longer in the transcript", () => {
    render({ transcripts: [null, null] });
    const chip = host!.querySelector<HTMLElement>('[data-trust="sourced"] .fly-cite')!;
    act(() => chip.focus());
    expect(host!.querySelector('[data-testid="source-preview"]')!.textContent).toContain(
      "aren't in the transcript shown now",
    );
  });

  it("an edited AI block reads as yours", () => {
    render({
      note: {
        ...note,
        blocks: [{ id: "b2", origin: { kind: "user" }, markdown: "My own take on pricing." }],
      },
    });
    expect(host!.querySelector('[data-trust="user"]')!.textContent).toContain("My own take");
    expect(host!.textContent).toContain("Everything here is yours");
  });
});
