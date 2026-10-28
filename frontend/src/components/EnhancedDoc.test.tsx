// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { Meeting, Note, Transcript } from "../types";
import EnhancedDoc, { REMOVE_UNDO_MS } from "./EnhancedDoc";

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
    {
      id: "b4",
      origin: { kind: "ai", source_segment_ids: [] },
      markdown: "Marketing holds the announcement.",
    },
  ],
};

// The RAW transcript Enhance read (the cleaned one would say it differently).
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
      text: "um I can put together two pricing options by Thursday",
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
  const el = (p: Partial<Parameters<typeof EnhancedDoc>[0]>) => (
    <EnhancedDoc
      note={note}
      onNoteChanged={onNoteChanged}
      onZoom={onZoom}
      transcript={transcript}
      {...props}
      {...p}
    />
  );
  act(() => root!.render(el({})));
  return {
    onNoteChanged,
    onZoom,
    rerender: (p: Partial<Parameters<typeof EnhancedDoc>[0]>) => act(() => root!.render(el(p))),
  };
}

beforeEach(() => {
  vi.spyOn(api, "getTranscript").mockResolvedValue(transcript);
});

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const $ = (sel: string) => host!.querySelector<HTMLElement>(sel);
const $$ = (sel: string) => [...host!.querySelectorAll<HTMLElement>(sel)];
const button = (scope: HTMLElement, label: string) =>
  [...scope.querySelectorAll("button")].find((b) => b.textContent === label);
const legend = () => $('[data-testid="trust-legend"]')!;
const flush = async () => {
  for (let i = 0; i < 4; i++) await act(async () => {});
};
const unsourced = () => $$('[data-trust="unsourced"]');

describe("EnhancedDoc trust", () => {
  it("shows one legend and no per-block AI labels", () => {
    render();
    expect(legend().textContent).toContain("3 AI lines · 2 without a source");
    const items = $('[aria-label="Legend"]')!.textContent!;
    expect(items).toContain("You");
    expect(items).toContain("AI, sourced");
    expect(items).toContain("AI, no source");
    expect($$("span").filter((s) => s.textContent === "AI")).toHaveLength(0);
  });

  it("flags an unsourced AI line without hover, explained in visible text", () => {
    render();
    const line = unsourced()[0];
    expect(line.textContent).toContain("Launch likely moves to May.");
    expect(line.textContent).toContain("No source");
    expect(line.textContent).toContain("Enhance didn't link this line to a transcript line");
    expect(line.textContent).not.toMatch(/made up/i);
    const why = line.querySelector<HTMLElement>('[id^="why-"]')!;
    expect(why.closest(".hidden")).toBeNull();
    const keep = button(line, "Keep")!;
    expect(keep.getAttribute("aria-describedby")).toBe(why.id);
    expect(button(line, "Remove")).toBeTruthy();
    for (const sel of ['[data-trust="sourced"]', '[data-trust="user"]']) {
      expect(button($(sel)!, "Keep")).toBeUndefined();
    }
  });

  it("Keep makes the line yours and moves focus to the next line to check", async () => {
    const kept: Note = {
      ...note,
      blocks: note.blocks.map((b) => (b.id === "b3" ? { ...b, origin: { kind: "user" } } : b)),
    };
    const keep = vi.spyOn(api, "keepNoteBlock").mockResolvedValue(kept);
    const { onNoteChanged, rerender } = render();
    onNoteChanged.mockImplementation((n: Note) => rerender({ note: n }));
    act(() => button(unsourced()[0], "Keep")!.click());
    await flush();
    expect(keep).toHaveBeenCalledWith("n1", "b3");
    expect(onNoteChanged).toHaveBeenCalledWith(kept);
    expect(document.activeElement?.getAttribute("data-review-keep")).toBe("b4");
  });

  it("after the last line to check, focus goes to the legend", async () => {
    const last: Note = { ...note, blocks: note.blocks.filter((b) => b.id !== "b4") };
    const kept: Note = {
      ...last,
      blocks: last.blocks.map((b) => (b.id === "b3" ? { ...b, origin: { kind: "user" } } : b)),
    };
    vi.spyOn(api, "keepNoteBlock").mockResolvedValue(kept);
    const { onNoteChanged, rerender } = render({ note: last });
    onNoteChanged.mockImplementation((n: Note) => rerender({ note: n }));
    act(() => button(unsourced()[0], "Keep")!.click());
    await flush();
    expect(document.activeElement).toBe(legend());
  });

  it("says so when Keep fails, and leaves the line alone", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(api, "keepNoteBlock").mockRejectedValue("database is locked");
    const { onNoteChanged } = render();
    act(() => button(unsourced()[0], "Keep")!.click());
    await flush();
    expect(onNoteChanged).not.toHaveBeenCalled();
    expect(unsourced()[0].querySelector('[role="alert"]')!.textContent).toContain(
      "Couldn't keep this line",
    );
  });
});

describe("Remove with undo", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  });

  it("hides the line behind Undo and deletes it only after the wait", async () => {
    const removed: Note = { ...note, blocks: note.blocks.filter((b) => b.id !== "b3") };
    const edit = vi.spyOn(api, "editNoteBlock").mockResolvedValue(removed);
    const { onNoteChanged, rerender } = render();
    onNoteChanged.mockImplementation((n: Note) => rerender({ note: n }));
    act(() => button(unsourced()[0], "Remove")!.click());
    expect($('[data-testid="removed-line"]')!.textContent).toContain("Removed");
    expect(host!.textContent).not.toContain("Launch likely moves to May.");
    // focus sits on Undo, in place
    expect(document.activeElement?.textContent).toBe("Undo");
    act(() => vi.advanceTimersByTime(REMOVE_UNDO_MS - 1));
    expect(edit).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(1));
    await flush();
    expect(edit).toHaveBeenCalledWith("n1", "b3", "");
    expect(onNoteChanged).toHaveBeenCalledWith(removed);
    // the line is gone: focus moves on to the next line to check
    expect(document.activeElement?.getAttribute("data-review-keep")).toBe("b4");
  });

  it("after removing the last line to check, focus goes to the legend", async () => {
    const one: Note = { ...note, blocks: note.blocks.filter((b) => b.id !== "b4") };
    const removed: Note = { ...one, blocks: one.blocks.filter((b) => b.id !== "b3") };
    vi.spyOn(api, "editNoteBlock").mockResolvedValue(removed);
    const { onNoteChanged, rerender } = render({ note: one });
    onNoteChanged.mockImplementation((n: Note) => rerender({ note: n }));
    act(() => button(unsourced()[0], "Remove")!.click());
    act(() => vi.advanceTimersByTime(REMOVE_UNDO_MS));
    await flush();
    expect(document.activeElement).toBe(legend());
  });

  it("Undo brings the line back and never deletes it", async () => {
    const edit = vi.spyOn(api, "editNoteBlock");
    render();
    act(() => button(unsourced()[0], "Remove")!.click());
    act(() => button($('[data-testid="removed-line"]')!, "Undo")!.click());
    expect(host!.textContent).toContain("Launch likely moves to May.");
    expect(document.activeElement?.getAttribute("data-review-keep")).toBe("b3");
    act(() => vi.advanceTimersByTime(REMOVE_UNDO_MS * 2));
    await flush();
    expect(edit).not.toHaveBeenCalled();
  });

  it("switching notes or leaving the view cancels a pending remove", async () => {
    const edit = vi.spyOn(api, "editNoteBlock");
    const { rerender } = render();
    act(() => button(unsourced()[0], "Remove")!.click());
    rerender({ note: { ...note, id: "n2", blocks: [] } });
    act(() => vi.advanceTimersByTime(REMOVE_UNDO_MS * 2));
    await flush();
    expect(edit).not.toHaveBeenCalled();

    rerender({ note });
    expect($('[data-testid="removed-line"]')).toBeNull(); // not stuck on "Removed"
    act(() => button(unsourced()[0], "Remove")!.click());
    act(() => root!.unmount());
    root = null;
    act(() => vi.advanceTimersByTime(REMOVE_UNDO_MS * 2));
    await flush();
    expect(edit).not.toHaveBeenCalled();
  });

  it("a second Remove deletes the first line now, and keeps it hidden until that's done", async () => {
    let finish!: (n: Note) => void;
    const edit = vi
      .spyOn(api, "editNoteBlock")
      .mockImplementation((_n, id) =>
        id === "b3" ? new Promise<Note>((r) => (finish = r)) : Promise.resolve(note),
      );
    const { onNoteChanged, rerender } = render();
    onNoteChanged.mockImplementation((n: Note) => rerender({ note: n }));
    act(() => button(unsourced()[0], "Remove")!.click()); // b3
    act(() => button(unsourced()[0], "Remove")!.click()); // b4
    await flush();
    expect(edit).toHaveBeenCalledTimes(1);
    expect(edit).toHaveBeenCalledWith("n1", "b3", "");
    // b3 stays hidden while its delete runs; b4 holds the one Undo
    expect($$('[data-testid="removed-line"]').map((r) => r.textContent)).toEqual([
      "Removing…",
      "RemovedUndo",
    ]);
    expect(host!.textContent).not.toContain("Launch likely moves to May.");
    expect(button(host!, "Keep")).toBeUndefined();

    const removed: Note = { ...note, blocks: note.blocks.filter((b) => b.id !== "b3") };
    await act(async () => finish(removed));
    expect(onNoteChanged).toHaveBeenCalledWith(removed);
    expect($$('[data-testid="removed-line"]').map((r) => r.textContent)).toEqual(["RemovedUndo"]);
  });

  it("if that delete fails, the first line comes back with the error", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(api, "editNoteBlock").mockRejectedValue("database is locked");
    render();
    act(() => button(unsourced()[0], "Remove")!.click()); // b3
    act(() => button(unsourced()[0], "Remove")!.click()); // b4
    await flush();
    const line = unsourced()[0];
    expect(line.textContent).toContain("Launch likely moves to May.");
    expect(line.querySelector('[role="alert"]')!.textContent).toContain(
      "Couldn't remove this line",
    );
    expect($('[data-testid="removed-line"]')!.textContent).toBe("RemovedUndo"); // b4
  });

  it("a delete that finishes after a note switch doesn't reopen that note", async () => {
    let finish!: (n: Note) => void;
    vi.spyOn(api, "editNoteBlock").mockReturnValue(new Promise<Note>((r) => (finish = r)));
    const { onNoteChanged, rerender } = render();
    act(() => button(unsourced()[0], "Remove")!.click());
    act(() => vi.advanceTimersByTime(REMOVE_UNDO_MS));
    rerender({ note: { ...note, id: "n2", blocks: [] } });
    await act(async () => finish(note));
    expect(onNoteChanged).not.toHaveBeenCalled();
  });
});

describe("sources", () => {
  it("previews the raw quote Enhance read; a click still opens the transcript", () => {
    const { onZoom } = render();
    const chip = $('[data-trust="sourced"] .fly-cite')!;
    expect($('[data-testid="source-preview"]')).toBeNull();
    act(() => {
      chip.parentElement!.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
    });
    const preview = $('[data-testid="source-preview"]')!;
    expect(preview.textContent).toContain("Dana");
    expect(preview.textContent).toContain("14:02");
    expect(preview.textContent).toContain("um I can put together two pricing options");
    act(() => chip.click());
    expect(onZoom).toHaveBeenCalledWith(["t1"], "m1");
  });

  it("Esc in the preview closes it and hands focus back to the chip", () => {
    render();
    const chip = $('[data-trust="sourced"] .fly-cite')!;
    act(() => chip.focus());
    const open = button($('[data-testid="source-preview"]')!, "Open in transcript →")!;
    act(() => open.focus());
    act(() => {
      open.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
      );
    });
    expect($('[data-testid="source-preview"]')).toBeNull();
    expect(document.activeElement).toBe(chip);
  });

  it("a line whose cited lines are all gone reads 'source not found'", () => {
    const retranscribed: Transcript = {
      ...transcript,
      segments: [{ ...transcript.segments[0], id: "new-1" }],
    };
    render({ transcript: retranscribed });
    const line = $('[data-trust="missing"]')!;
    expect(line.textContent).toContain("Dana will send two pricing options");
    expect(line.textContent).toContain("Source not found");
    expect(button(line, "Keep")).toBeTruthy();
    expect(legend().textContent).toContain("1 with a source not found");
    expect(legend().textContent).not.toContain("all sourced");
  });

  it("checks against the cited meeting, not the selected recording, and waits for it", async () => {
    let resolve!: (t: Transcript | null) => void;
    const get = vi.spyOn(api, "getTranscript").mockReturnValue(new Promise((r) => (resolve = r)));
    // The editor has another recording selected: its transcript doesn't count.
    render({ transcript: { ...transcript, meeting_id: "m2", segments: [] } });
    expect(get).toHaveBeenCalledWith("m1");
    // still loading: nothing is downgraded or flagged
    expect($('[data-trust="sourced"]')).not.toBeNull();
    expect($('[data-trust="missing"]')).toBeNull();
    expect(button(host!, "Keep")).toBeUndefined();
    await act(async () => resolve(transcript));
    expect($('[data-trust="sourced"]')).not.toBeNull();
    expect(unsourced()).toHaveLength(2);
  });

  it("an edited AI block reads as yours", () => {
    render({
      note: {
        ...note,
        blocks: [{ id: "b2", origin: { kind: "user" }, markdown: "My own take on pricing." }],
      },
    });
    expect($('[data-trust="user"]')!.textContent).toContain("My own take");
    expect(legend().textContent).toContain("Everything here is yours");
  });
});

describe("notes with no transcript", () => {
  const notesOnly: Note = {
    ...note,
    meeting_id: null,
    blocks: [
      { id: "b1", origin: { kind: "user" }, markdown: "Rough notes." },
      { id: "b2", origin: { kind: "ai", source_segment_ids: [] }, markdown: "## Summary\nTidy." },
    ],
  };

  it("shows AI text as written from your notes, with no source warning", () => {
    render({ note: notesOnly, transcript: null });
    expect($('[data-trust="notes"]')).not.toBeNull();
    expect(host!.textContent).not.toContain("No source");
    expect(button(host!, "Keep")).toBeUndefined();
    expect(legend().textContent).toContain("1 AI line · written from your notes");
    expect(legend().textContent).toContain("no transcript");
    expect(legend().textContent).not.toContain("Hover a source");
    expect($('[aria-label="Legend"]')!.textContent).toContain("AI, from your notes");
  });

  it("a meeting that hasn't been transcribed yet counts the same", async () => {
    vi.spyOn(api, "getTranscript").mockResolvedValue(null);
    render({ note: { ...notesOnly, meeting_id: "m9" }, transcript: null });
    await flush();
    expect($('[data-trust="notes"]')).not.toBeNull();
    expect(button(host!, "Keep")).toBeUndefined();
  });
});

describe("while Enhance runs", () => {
  it("Keep, Remove and Edit wait, since Enhance replaces every block", () => {
    render({ enhancing: true });
    const line = unsourced()[0];
    expect(button(line, "Keep")!.disabled).toBe(true);
    expect(button(line, "Remove")!.disabled).toBe(true);
    expect(button(host!, "Edit")).toBeUndefined();
  });
});

describe("editing", () => {
  it("Esc in the editor cancels the edit and claims the key", () => {
    render();
    act(() => button($('[data-trust="user"]')!, "Edit")!.click());
    const ta = host!.querySelector("textarea")!;
    const e = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    act(() => {
      ta.dispatchEvent(e);
    });
    expect(e.defaultPrevented).toBe(true);
    expect(host!.querySelector("textarea")).toBeNull();
  });
});

describe("a note recorded into again", () => {
  const meet = (id: string, startedAt: string): Meeting => ({
    id,
    title: "Launch sync",
    note_id: "n1",
    attendees: [],
    attendees_confirmed: false,
    started_at: startedAt,
    ended_at: null,
    recording: null,
  });
  const m1 = meet("m1", "2026-10-20T10:00:00Z");
  const m2 = meet("m2", "2026-10-27T10:00:00Z");
  // create_meeting points the note at its newest recording; the blocks
  // still cite m1, the one Enhance read.
  const reRecorded: Note = { ...note, meeting_id: "m2" };
  const m2Transcript: Transcript = {
    ...transcript,
    meeting_id: "m2",
    segments: [{ ...transcript.segments[0], id: "u1", text: "a new recording" }],
  };
  const transcriptsById = (map: Record<string, Transcript | null>) =>
    vi.spyOn(api, "getTranscript").mockImplementation((id) => Promise.resolve(map[id] ?? null));

  it("lines sourced from the earlier recording stay sourced, and zoom into it", async () => {
    transcriptsById({ m1: transcript, m2: m2Transcript });
    const { onZoom } = render({ note: reRecorded, meetings: [m2, m1], transcript: m2Transcript });
    await flush();
    expect($('[data-trust="sourced"]')!.textContent).toContain("Dana will send");
    expect($('[data-trust="missing"]')).toBeNull();
    expect(legend().textContent).not.toContain("not found");
    act(() => $('[data-trust="sourced"] .fly-cite')!.click());
    expect(onZoom).toHaveBeenCalledWith(["t1"], "m1");
  });

  it("while the new recording isn't transcribed, nothing is missing and quotes still show", async () => {
    transcriptsById({ m1: transcript, m2: null });
    render({
      note: {
        ...reRecorded,
        blocks: [
          ...reRecorded.blocks,
          { id: "b9", origin: { kind: "ai", source_segment_ids: ["x9"] }, markdown: "Elsewhere." },
        ],
      },
      meetings: [m2, m1],
      transcript: null,
    });
    await flush();
    expect($('[data-trust="missing"]')).toBeNull();
    const chip = $('[data-trust="sourced"] .fly-cite')!;
    act(() => {
      chip.parentElement!.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
    });
    const preview = $('[data-testid="source-preview"]')!.textContent!;
    expect(preview).toContain("um I can put together two pricing options");
    expect(preview).not.toContain("Loading");
  });

  it("claims nothing until every recording's transcript has loaded", async () => {
    let resolveM1!: (t: Transcript | null) => void;
    vi.spyOn(api, "getTranscript").mockImplementation((id) =>
      id === "m1" ? new Promise((r) => (resolveM1 = r)) : Promise.resolve(m2Transcript),
    );
    render({ note: reRecorded, meetings: [m2, m1], transcript: m2Transcript });
    await flush();
    expect($('[data-trust="missing"]')).toBeNull();
    expect(button(host!, "Keep")).toBeUndefined();
    await act(async () => resolveM1(transcript));
    expect($('[data-trust="sourced"]')).not.toBeNull();
    expect(unsourced()).toHaveLength(2);
  });

  it("while the editor still lists the previous note's recordings, it asks for this note's", async () => {
    const get = transcriptsById({ m1: transcript, m2: m2Transcript });
    let listed!: (ms: Meeting[]) => void;
    const list = vi
      .spyOn(api, "getMeetingsForNote")
      .mockReturnValue(new Promise((r) => (listed = r)));
    const previous = { ...m1, id: "mx", note_id: "n9" };
    const { onZoom } = render({ note: reRecorded, meetings: [previous], transcript: m2Transcript });
    await flush();
    expect(list).toHaveBeenCalledWith("n1");
    expect(get).not.toHaveBeenCalledWith("mx");
    // claims nothing until it knows the note's recordings
    expect($('[data-trust="missing"]')).toBeNull();
    expect(button(host!, "Keep")).toBeUndefined();
    await act(async () => listed([m2, m1]));
    await flush();
    expect($('[data-trust="missing"]')).toBeNull();
    expect(unsourced()).toHaveLength(2);
    act(() => $('[data-trust="sourced"] .fly-cite')!.click());
    expect(onZoom).toHaveBeenCalledWith(["t1"], "m1");
  });
});

describe("enhanced before the transcript was ready", () => {
  it("says so once in the legend, with no Keep / Remove on every line", () => {
    const early: Note = { ...note, blocks: note.blocks.filter((b) => b.id !== "b2") };
    render({ note: early });
    expect(legend().textContent).toContain("2 AI lines · no sources yet");
    expect(legend().textContent).toContain(
      "Enhanced before the transcript was ready. Enhance again to link sources.",
    );
    expect(button(host!, "Keep")).toBeUndefined();
    expect(button(host!, "Remove")).toBeUndefined();
    expect($$('[data-trust="notes"]')).toHaveLength(2);
  });
});
