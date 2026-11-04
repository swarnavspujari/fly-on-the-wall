// @vitest-environment jsdom
// The Enhanced view inside the real Editor, against the dev mock backend
// (the same stub port-1420 QA uses).
import "../devMock";
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { EnhanceReport, Meeting, Note, Transcript } from "../types";
import Editor from "./Editor";
import { REMOVE_UNDO_MS } from "./EnhancedDoc";
import { ConfirmProvider } from "./confirm";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const transcript: Transcript = {
  meeting_id: "m1",
  language: "en",
  engine: "whisper",
  speakers: [{ key: "s1", label: "Dana" }],
  segments: [{ id: "t1", speaker_key: "s1", start_ms: 0, end_ms: 1000, text: "hi", words: [] }],
};

const start: Note = {
  id: "nx",
  title: "One line left",
  folder_id: null,
  meeting_id: "m1",
  scratchpad: "my notes",
  attachments: [],
  created_at: "2026-10-27T10:00:00Z",
  updated_at: "2026-10-27T10:00:00Z",
  // cites a line the transcript no longer has, so it's one to check
  blocks: [
    { id: "only", origin: { kind: "ai", source_segment_ids: ["gone"] }, markdown: "Lone line." },
  ],
};

const noop = () => {};

function Harness(p: Partial<Parameters<typeof Editor>[0]> = {}) {
  const [note, setNote] = useState(start);
  return (
    <Editor
      note={note}
      meeting={null}
      meetings={[]}
      onSelectMeeting={noop}
      transcript={transcript}
      cleanedTranscript={null}
      pipeStage={null}
      pipeDetail={null}
      pipelineError={null}
      modelProgress={null}
      engine={null}
      engineInstalling={false}
      recStatus={{
        active: false,
        state: null,
        elapsed_ms: 0,
        meeting_id: null,
        note_id: null,
        warnings: [],
      }}
      screenStatus={{ active: false, note_id: null, elapsed_ms: 0 }}
      importStaged={null}
      folders={[]}
      templates={[]}
      dataDir={null}
      onNoteChanged={setNote}
      onMeetingChanged={noop}
      onTranscriptRestored={noop}
      onMoveNote={noop}
      onStartRecording={noop}
      onStartScreen={noop}
      onStopScreen={noop}
      onTranscribe={noop}
      onImportTranscribe={noop}
      onImportCancel={noop}
      onInstallEngine={noop}
      onOpenSettings={noop}
      onRelabel={noop}
      onEditSegment={noop}
      {...p}
    />
  );
}

let root: Root | null = null;
let host: HTMLElement | null = null;

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("Editor › Enhanced", () => {
  it("removing the last line returns to the Notes view", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    vi.spyOn(api, "editNoteBlock").mockResolvedValue({ ...start, blocks: [] });
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    act(() =>
      root!.render(
        <ConfirmProvider>
          <Harness />
        </ConfirmProvider>,
      ),
    );
    await act(async () => {});
    expect(host.querySelector('[data-testid="trust-legend"]')).not.toBeNull();
    const remove = [...host.querySelectorAll("button")].find((b) => b.textContent === "Remove")!;
    act(() => remove.click());
    act(() => vi.advanceTimersByTime(REMOVE_UNDO_MS));
    for (let i = 0; i < 4; i++) await act(async () => {});
    expect(api.editNoteBlock).toHaveBeenCalledWith("nx", "only", "");
    expect(host.querySelector('[data-testid="trust-legend"]')).toBeNull();
    const notes = host.querySelector(".fotw-notes")!;
    expect((notes.closest('div[style*="display"]') as HTMLElement).style.display).toBe("block");
  });
});

describe("Editor › Enhanced › after an Enhance run", () => {
  const report: EnhanceReport = {
    segments_total: 1980,
    segments_shown: 412,
    injected_lines: 1,
    blocks: 2,
    user: 0,
    sourced: 0,
    flagged: 1,
    unsourced: 1,
    notes: 0,
    dropped: 1,
    dropped_text: ["Send the vendor deposit to the new account today."],
    demoted_user: 0,
    invalid_citations: 0,
    links_removed: 0,
    retried_smaller: false,
  };
  const enhanced: Note = {
    ...start,
    blocks: [
      {
        id: "f1",
        origin: { kind: "ai", source_segment_ids: ["t1"] },
        markdown: "Dana asked for $63k.",
        flags: [{ kind: "number", detail: "63000" }],
      },
      { id: "u1", origin: { kind: "ai", source_segment_ids: [] }, markdown: "Mood was upbeat." },
    ],
  };

  const click = (label: string) => {
    const b = [...host!.querySelectorAll("button")].find((x) => x.textContent === label);
    expect(b, label).toBeTruthy();
    act(() => b!.click());
  };

  it("shows the saved note with the flagged line, says what it read and left out, and goes away", async () => {
    const run = vi.spyOn(api, "enhanceNote").mockResolvedValue({ note: enhanced, report });
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    act(() =>
      root!.render(
        <ConfirmProvider>
          <Harness />
        </ConfirmProvider>,
      ),
    );
    await act(async () => {});
    // Enhance lives on the Notes view; the note opens on its Enhanced one.
    expect(host.querySelector('[data-testid="enhance-notices"]')).toBeNull();
    click("Notes");
    click("Re-enhance");
    for (let i = 0; i < 4; i++) await act(async () => {});
    expect(run).toHaveBeenCalledWith("nx", "tpl-general");

    // The note that came back is what's shown, in the Enhanced view.
    const flagged = host.querySelector('[data-trust="flagged"]')!;
    expect(flagged.textContent).toContain("Dana asked for $63k.");
    expect(flagged.textContent).toContain("The number “63000” isn't in the lines it cites.");
    // The notice says it read part of a long meeting, and what it left out.
    const box = host.querySelector('[data-testid="enhance-notices"]')!;
    expect(box.textContent).toContain("read 412 of 1980 lines");
    expect(box.textContent).toContain("Left out 1 line");
    expect(box.textContent).toContain("Send the vendor deposit to the new account today.");

    click("Got it");
    expect(host.querySelector('[data-testid="enhance-notices"]')).toBeNull();
    expect(host.querySelector('[data-trust="flagged"]')).not.toBeNull();
  });

  it("stays quiet when it read and kept everything, and shows a failure as an error, not a notice", async () => {
    const quiet = { ...report, segments_shown: 1980, dropped: 0, dropped_text: [] };
    const run = vi
      .spyOn(api, "enhanceNote")
      .mockResolvedValueOnce({ note: enhanced, report: quiet });
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    act(() =>
      root!.render(
        <ConfirmProvider>
          <Harness />
        </ConfirmProvider>,
      ),
    );
    await act(async () => {});
    click("Notes");
    click("Re-enhance");
    for (let i = 0; i < 4; i++) await act(async () => {});
    expect(host.querySelector('[data-testid="enhance-notices"]')).toBeNull();
    expect(host.querySelector('[data-trust="flagged"]')).not.toBeNull();

    // A failed run: the message, nothing saved, and no stale notice.
    run.mockRejectedValueOnce("your model couldn't take in this much of the meeting at once");
    click("Notes");
    click("Re-enhance");
    for (let i = 0; i < 4; i++) await act(async () => {});
    expect(host.textContent).toContain("couldn't take in this much of the meeting");
    expect(host.querySelector('[data-testid="enhance-notices"]')).toBeNull();
  });
});

describe("Editor › Enhanced › a note recorded into again", () => {
  const meet = (id: string, startedAt: string): Meeting => ({
    id,
    title: "Launch sync",
    note_id: "nr",
    attendees: [],
    attendees_confirmed: false,
    started_at: startedAt,
    ended_at: null,
    recording: null,
  });
  const m1 = meet("m1", "2026-10-20T10:00:00Z");
  const m2 = meet("m2", "2026-10-27T10:00:00Z");
  const t2: Transcript = {
    ...transcript,
    meeting_id: "m2",
    segments: [{ ...transcript.segments[0], id: "u1", text: "a new recording" }],
  };
  // The note points at its newest recording; its line cites the first one.
  const reRecorded: Note = {
    ...start,
    id: "nr",
    meeting_id: "m2",
    blocks: [{ id: "c1", origin: { kind: "ai", source_segment_ids: ["t1"] }, markdown: "Cited." }],
  };

  function TwoRecordings({ onPick }: { onPick: (m: Meeting) => void }) {
    const [meeting, setMeeting] = useState(m2);
    const [shown, setShown] = useState<Transcript | null>(t2);
    return (
      <Harness
        note={reRecorded}
        meeting={meeting}
        meetings={[m2, m1]}
        transcript={shown}
        onSelectMeeting={(m) => {
          onPick(m);
          setMeeting(m);
          void api.getTranscript(m.id).then(setShown);
        }}
      />
    );
  }

  it("a source chip opens the earlier recording and scrolls to the line", async () => {
    vi.spyOn(api, "getTranscript").mockImplementation((id) =>
      Promise.resolve(id === "m1" ? transcript : t2),
    );
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll; // jsdom has none
    const onPick = vi.fn();
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    try {
      act(() =>
        root!.render(
          <ConfirmProvider>
            <TwoRecordings onPick={onPick} />
          </ConfirmProvider>,
        ),
      );
      for (let i = 0; i < 4; i++) await act(async () => {});
      expect(host.querySelector('[data-trust="sourced"]')).not.toBeNull();
      expect(host.querySelector('[data-trust="missing"]')).toBeNull();
      act(() => host!.querySelector<HTMLElement>('[data-trust="sourced"] .fly-cite')!.click());
      for (let i = 0; i < 4; i++) await act(async () => {});
      expect(onPick).toHaveBeenCalledWith(m1);
      expect(scroll).toHaveBeenCalled();
      expect((scroll.mock.contexts.at(-1) as HTMLElement).textContent).toContain("hi");
    } finally {
      delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView;
    }
  });
});

describe("dev mock", () => {
  type Invoke = (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
  const invoke = (cmd: string, args: Record<string, unknown>) =>
    (window as unknown as { __TAURI_INTERNALS__: { invoke: Invoke } }).__TAURI_INTERNALS__.invoke(
      cmd,
      args,
    ) as Promise<Note>;

  it("editing a notes-only fixture block (b0) returns the note instead of failing", async () => {
    for (const noteId of ["n3", "n6"]) {
      const edited = await invoke("edit_note_block", { noteId, blockId: "b0", markdown: "x" });
      expect(edited.id).toBe(noteId);
      const kept = await invoke("keep_note_block", { noteId, blockId: "b0" });
      expect(kept.id).toBe(noteId);
    }
  });
});
