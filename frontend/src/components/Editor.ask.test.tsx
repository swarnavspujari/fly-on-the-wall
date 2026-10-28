// @vitest-environment jsdom
// Ask inside the real Editor, against the dev mock backend (the same stub
// port-1420 QA uses): pinning, its remembered setting, and Esc from outside.
import "../devMock";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { Note } from "../types";
import Editor from "./Editor";
import { ConfirmProvider } from "./confirm";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const note: Note = {
  id: "n3",
  title: "Design weekly — notes",
  folder_id: null,
  meeting_id: null,
  scratchpad: "",
  attachments: [],
  created_at: "2026-10-27T10:00:00Z",
  updated_at: "2026-10-27T10:00:00Z",
  blocks: [],
};

const noop = () => {};

function Ed() {
  return (
    <Editor
      note={note}
      meeting={null}
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
      importStaged={null}
      folders={[]}
      templates={[]}
      dataDir={null}
      onNoteChanged={noop}
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

const mount = async () => {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <ConfirmProvider>
        <Ed />
      </ConfirmProvider>,
    ),
  );
  for (let i = 0; i < 5; i++) await act(async () => {});
};
const unmount = () => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
};
const flush = async () => {
  for (let i = 0; i < 5; i++) await act(async () => {});
};
const composer = () => host!.querySelector<HTMLTextAreaElement>("form[aria-label=Ask] textarea")!;
const sheet = () => host!.querySelector<HTMLElement>('[data-testid="ask-sheet"]');

beforeEach(() => {
  // a window at least 1440 px wide
  vi.stubGlobal(
    "matchMedia",
    vi.fn(() => ({ matches: true, addEventListener: noop, removeEventListener: noop })),
  );
});

afterEach(() => {
  unmount();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Editor › Ask", () => {
  it("pins beside the note on wide windows and remembers it", async () => {
    const setSetting = vi.spyOn(api, "setAppSetting");
    await mount();
    act(() => composer().click());
    await flush();
    expect(sheet()!.className).toContain("fly-sheet--bottom");
    const pin = [...host!.querySelectorAll("button")].find((b) => b.textContent === "Pin to side")!;
    act(() => pin.click());
    await flush();
    expect(setSetting).toHaveBeenCalledWith("ui.ask.pinned", "true");
    expect(sheet()!.className).toContain("fly-ask-sheet--pinned");
    // a column beside the note, not inside the dock
    expect(sheet()!.closest("form[aria-label=Ask]")).toBeNull();
    expect(sheet()!.parentElement!.style.width).toBe("var(--ask-pinned-w)");

    // a fresh editor (another session) opens it pinned straight away
    unmount();
    await mount();
    act(() => composer().click());
    await flush();
    expect(sheet()!.className).toContain("fly-ask-sheet--pinned");

    // unpin again so the mock's remembered setting doesn't leak
    const unpin = [...host!.querySelectorAll("button")].find((b) => b.textContent === "Unpin")!;
    act(() => unpin.click());
    await flush();
    expect(setSetting).toHaveBeenLastCalledWith("ui.ask.pinned", "false");
  });

  it("Esc while editing the title closes the sheet and leaves focus there", async () => {
    await mount();
    act(() => composer().click());
    await flush();
    expect(sheet()).not.toBeNull();
    const title = host!.querySelector<HTMLInputElement>('input[placeholder="Untitled"]')!;
    act(() => title.focus());
    act(() => {
      title.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
      );
    });
    await flush();
    expect(sheet()).toBeNull();
    expect(document.activeElement).toBe(title);
  });

  it("the dock lets clicks through to the note except on its controls", async () => {
    await mount();
    const dock = composer().closest("form")!.parentElement!;
    expect(dock.className).toContain("pointer-events-none");
    expect(dock.className).toContain("[&>*]:pointer-events-auto");
  });
});
