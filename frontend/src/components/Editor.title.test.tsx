// @vitest-environment jsdom
// The note title field: a new note's "Untitled" is a placeholder, and an
// imported note's title follows the queue's first file until it's renamed.
import "../devMock";
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { ImportFile, ImportStaged, Meeting, Note } from "../types";
import Editor from "./Editor";
import { ConfirmProvider } from "./confirm";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const noop = () => {};

const note = (title: string): Note => ({
  id: "nt",
  title,
  folder_id: null,
  meeting_id: "mt",
  scratchpad: "",
  attachments: [],
  created_at: "2026-10-27T10:00:00Z",
  updated_at: "2026-10-27T10:00:00Z",
  blocks: [],
});

const file = (id: string, file_name: string, over: Partial<ImportFile> = {}): ImportFile => ({
  id,
  file_name,
  size: 1_000_000,
  kind: "audio",
  rel_path: `recordings/imp/${file_name}`,
  error: null,
  convert_error: null,
  ...over,
});

function Harness({
  start,
  staged,
  meeting = null,
}: {
  start: Note;
  staged: ImportStaged | null;
  meeting?: Meeting | null;
}) {
  const [current, setCurrent] = useState(start);
  return (
    <Editor
      note={current}
      meeting={meeting}
      meetings={[]}
      onSelectMeeting={noop}
      transcript={null}
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
      importStaged={staged}
      folders={[]}
      templates={[]}
      dataDir={null}
      onNoteChanged={setCurrent}
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

async function mount(
  start: Note,
  staged: ImportStaged | null = null,
  meeting: Meeting | null = null,
) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <ConfirmProvider>
        <Harness start={start} staged={staged} meeting={meeting} />
      </ConfirmProvider>,
    ),
  );
  for (let i = 0; i < 4; i++) await act(async () => {});
}

const titleInput = () => host!.querySelector<HTMLInputElement>('input[placeholder="Untitled"]')!;

/** React-controlled input: set the value the way a keystroke does. */
function typeInto(input: HTMLInputElement, value: string) {
  const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  act(() => {
    set.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

describe("Editor › title", () => {
  it("a new note's Untitled is a placeholder, so typing replaces it", async () => {
    const save = vi.spyOn(api, "updateNoteTitle").mockImplementation(async (_id, t) => note(t));
    vi.useFakeTimers({ shouldAdvanceTime: true });
    await mount(note("Untitled"));
    expect(titleInput().value).toBe("");
    typeInto(titleInput(), "B");
    expect(titleInput().value).toBe("B");
    await act(async () => {
      vi.advanceTimersByTime(2000);
    });
    expect(save).toHaveBeenCalledWith("nt", "B");
  });

  it('typing that passes through "Untitled" stays on screen', async () => {
    vi.spyOn(api, "updateNoteTitle").mockImplementation(async (_id, t) => note(t));
    // Fake timers: the debounced save this typing schedules is dropped at
    // teardown instead of firing into a later test's spy.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    await mount(note("Untitled"));
    for (const step of ["U", "Untitle", "Untitled", "Untitled ", "Untitled 2"]) {
      typeInto(titleInput(), step);
      expect(titleInput().value).toBe(step);
    }
  });

  it("a real title is shown as text", async () => {
    await mount(note("Weekly sync"));
    expect(titleInput().value).toBe("Weekly sync");
  });

  it("an imported note's title follows the first file when it's removed", async () => {
    const save = vi.spyOn(api, "updateNoteTitle").mockImplementation(async (_id, t) => note(t));
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const staged: ImportStaged = {
      note_id: "nt",
      meeting_id: "mt",
      files: [
        file("a", "Kickoff part 1.m4a", {
          convert_error: "This file looks damaged or incomplete, so its audio can't be read.",
        }),
        file("b", "Kickoff part 2.m4a"),
      ],
      boundaries_ms: [],
      started: false,
    };
    await mount(note("Kickoff part 1"), staged);
    await act(async () => {
      host!.querySelector<HTMLElement>('button[aria-label="Remove Kickoff part 1.m4a"]')!.click();
    });
    expect(titleInput().value).toBe("Kickoff part 2");
    await act(async () => {
      vi.advanceTimersByTime(2000);
    });
    expect(save).toHaveBeenCalledWith("nt", "Kickoff part 2");
  });

  it("a renamed imported note keeps its name", async () => {
    const staged: ImportStaged = {
      note_id: "nt",
      meeting_id: "mt",
      files: [
        file("a", "Kickoff part 1.m4a", { convert_error: "Its audio couldn't be converted." }),
        file("b", "Kickoff part 2.m4a"),
      ],
      boundaries_ms: [],
      started: false,
    };
    await mount(note("Customer kickoff"), staged);
    await act(async () => {
      host!.querySelector<HTMLElement>('button[aria-label="Remove Kickoff part 1.m4a"]')!.click();
    });
    expect(titleInput().value).toBe("Customer kickoff");
  });

  it("after the files are joined into the recording, the title stays put", async () => {
    const save = vi.spyOn(api, "updateNoteTitle").mockImplementation(async (_id, t) => note(t));
    const staged: ImportStaged = {
      note_id: "nt",
      meeting_id: "mt",
      files: [
        file("a", "Kickoff part 1.m4a", { convert_error: "Its audio couldn't be converted." }),
        file("b", "Kickoff part 2.m4a"),
      ],
      boundaries_ms: [],
      started: false,
    };
    const joined: Meeting = {
      id: "mt",
      title: "Kickoff part 1",
      note_id: "nt",
      attendees: [],
      attendees_confirmed: false,
      started_at: "2026-10-27T10:00:00Z",
      ended_at: "2026-10-27T10:30:00Z",
      recording: {
        mic_path: null,
        system_path: null,
        mixed_path: "recordings/imp/recording.mixed.wav",
        playback_path: null,
        duration_ms: 60_000,
      },
    };
    await mount(note("Kickoff part 1"), staged, joined);
    await act(async () => {
      host!.querySelector<HTMLElement>('button[aria-label="Remove Kickoff part 1.m4a"]')!.click();
    });
    expect(titleInput().value).toBe("Kickoff part 1");
    expect(save).not.toHaveBeenCalled();
  });
});
