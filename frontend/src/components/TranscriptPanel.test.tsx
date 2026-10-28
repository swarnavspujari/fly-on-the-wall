// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Meeting, Transcript } from "../types";
import { ConfirmProvider } from "./confirm";
import { confirmChoice, confirmDialog, confirmTitle } from "./confirmTesting";
import TranscriptPanel from "./TranscriptPanel";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const meeting: Meeting = {
  id: "m1",
  title: "Weekly sync",
  note_id: "n1",
  attendees: [],
  attendees_confirmed: false,
  started_at: "2026-10-01T15:00:00Z",
  ended_at: "2026-10-01T15:30:00Z",
  recording: {
    mic_path: "recordings/m1/mic.wav",
    system_path: null,
    mixed_path: null,
    duration_ms: 1_800_000,
  },
};

const transcript: Transcript = {
  meeting_id: "m1",
  language: "en",
  engine: "whisper",
  segments: [
    { id: "s1", speaker_key: "mic", start_ms: 0, end_ms: 2000, text: "Hello.", words: [] },
  ],
  speakers: [{ key: "mic", label: "You" }],
};

let root: Root | null = null;
let host: HTMLElement | null = null;

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
});

const flush = async () => {
  for (let i = 0; i < 3; i++) await act(async () => {});
};

function render(onTranscribe: () => void) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <ConfirmProvider>
        <TranscriptPanel
          meeting={meeting}
          transcript={transcript}
          cleaned={null}
          stage={null}
          stageDetail={null}
          modelProgress={null}
          pipelineError={null}
          engine={{ installed: true, managed: true }}
          engineInstalling={false}
          highlightIds={[]}
          onTranscribe={onTranscribe}
          onInstallEngine={() => {}}
          onOpenSettings={() => {}}
          onRelabel={() => {}}
          onAssignNewAttendee={() => {}}
          onEditSegment={() => {}}
        />
      </ConfirmProvider>,
    ),
  );
}

const rerun = () =>
  [...host!.querySelectorAll("button")].find((b) => b.textContent === "Re-run transcription")!;

describe("TranscriptPanel › Re-run transcription", () => {
  it("asks first, and Cancel leaves the transcript alone", async () => {
    const onTranscribe = vi.fn();
    render(onTranscribe);
    await act(async () => rerun().click());
    expect(confirmTitle()).toBe("Re-run transcription for this meeting?");
    expect(confirmDialog()!.textContent).toContain("will be lost");
    expect(confirmDialog()!.dataset.tone).toBe("danger");
    expect(onTranscribe).not.toHaveBeenCalled(); // nothing starts while it asks
    await act(async () => confirmChoice(false).click());
    await flush();
    expect(onTranscribe).not.toHaveBeenCalled();
    expect(host!.textContent).toContain("Hello.");
  });

  it("re-runs once you confirm", async () => {
    const onTranscribe = vi.fn();
    render(onTranscribe);
    await act(async () => rerun().click());
    await act(async () => confirmChoice(true).click());
    await flush();
    expect(onTranscribe).toHaveBeenCalledTimes(1);
  });
});
