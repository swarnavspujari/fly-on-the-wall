// @vitest-environment jsdom
// The editor's audio player on a compressed recording (recording storage v2):
// no stored mix, so the first Play rebuilds it, then plays it.
import "../devMock";
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { Meeting, Note, Transcript } from "../types";
import Editor from "./Editor";
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
  title: "Compressed call",
  folder_id: null,
  meeting_id: "m1",
  scratchpad: "",
  attachments: [],
  created_at: "2026-10-27T10:00:00Z",
  updated_at: "2026-10-27T10:00:00Z",
  blocks: [],
};

const compressed: Meeting = {
  id: "m1",
  title: "Compressed call",
  note_id: "nx",
  attendees: [],
  attendees_confirmed: false,
  started_at: "2026-10-27T10:00:00Z",
  ended_at: "2026-10-27T10:30:00Z",
  recording: {
    mic_path: "recordings/2026-10-27 Compressed call/recording.mic.flac",
    system_path: "recordings/2026-10-27 Compressed call/recording.system.flac",
    mixed_path: null,
    playback_path: null,
    duration_ms: 1_800_000,
  },
};

const noop = () => {};

function Harness(p: Partial<Parameters<typeof Editor>[0]> = {}) {
  const [note, setNote] = useState(start);
  return (
    <Editor
      note={note}
      meeting={compressed}
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
  vi.restoreAllMocks();
});

describe("Editor › audio player on a compressed recording", () => {
  it("asks for the file without building, then rebuilds the mix on Play", async () => {
    let release: (v: { path: string; needs_build: false }) => void = () => {};
    const prepare = vi.spyOn(api, "preparePlayback").mockImplementation((_id, build) =>
      build
        ? new Promise((r) => {
            release = r;
          })
        : Promise.resolve({ path: null, needs_build: true }),
    );
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
    for (let i = 0; i < 3; i++) await act(async () => {});
    const tab = [...host.querySelectorAll("button")].find((b) => b.textContent === "Transcript")!;
    act(() => tab.click());
    await act(async () => {});
    expect(prepare).toHaveBeenCalledWith("m1", false);
    expect(prepare).not.toHaveBeenCalledWith("m1", true);
    expect(host.querySelector("audio")).toBeNull();

    const play = host.querySelector('button[aria-label="Play recording"]') as HTMLButtonElement;
    act(() => play.click());
    expect(prepare).toHaveBeenCalledWith("m1", true);
    const busy = host.querySelector('button[aria-label="Preparing the recording"]');
    expect(busy).not.toBeNull();
    expect((busy as HTMLButtonElement).disabled).toBe(true);

    await act(async () => release({ path: "C:/scratch/m1/playback.wav", needs_build: false }));
    await act(async () => {});
    expect(host.querySelector("audio")!.getAttribute("src")).toBe("C:/scratch/m1/playback.wav");
    expect(host.querySelector('button[aria-label="Play recording"]')).not.toBeNull();
  });

  it("a stored file plays without any rebuild", async () => {
    const prepare = vi.spyOn(api, "preparePlayback").mockResolvedValue({
      path: "C:/data/recordings/x/recording.playback.wav",
      needs_build: false,
    });
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
    for (let i = 0; i < 3; i++) await act(async () => {});
    const tab = [...host.querySelectorAll("button")].find((b) => b.textContent === "Transcript")!;
    act(() => tab.click());
    await act(async () => {});
    expect(host.querySelector("audio")!.getAttribute("src")).toBe(
      "C:/data/recordings/x/recording.playback.wav",
    );
    expect(prepare).toHaveBeenCalledTimes(1);
  });
});
