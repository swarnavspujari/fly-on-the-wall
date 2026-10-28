// @vitest-environment jsdom
// The Enhanced view inside the real Editor, against the dev mock backend
// (the same stub port-1420 QA uses).
import "../devMock";
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { Note, Transcript } from "../types";
import Editor from "./Editor";
import { REMOVE_UNDO_MS } from "./EnhancedDoc";

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
  blocks: [{ id: "only", origin: { kind: "ai", source_segment_ids: [] }, markdown: "Lone line." }],
};

const noop = () => {};

function Harness() {
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
    act(() => root!.render(<Harness />));
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
