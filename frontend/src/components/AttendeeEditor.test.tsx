// @vitest-environment jsdom
// The speaker card (spec §3.3.2) and the attendee popover, against the dev
// mock backend. The invariant under test: a calendar-seeded roster is never
// confirmed unless you confirm it, because only confirmed rosters set
// num_speakers for diarization.
import "../devMock";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { Attendee, Meeting, RecordingStatus } from "../types";
import AttendeeEditor from "./AttendeeEditor";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const SEEDED: Attendee[] = [
  { name: "Dana Osei", email: "dana@acme.com" },
  { name: "Marc Reyes", email: "marc@acme.com" },
];

let seq = 0;
const meetingOf = (over: Partial<Meeting> = {}): Meeting => ({
  id: `m-card-${++seq}`,
  title: "Acme sync",
  note_id: "n1",
  attendees: SEEDED,
  attendees_confirmed: false,
  started_at: new Date().toISOString(),
  ended_at: null,
  recording: null,
  ...over,
});

const recording = (meetingId: string | null): RecordingStatus => ({
  active: meetingId != null,
  state: meetingId != null ? "recording" : null,
  elapsed_ms: 5_000,
  meeting_id: meetingId,
  note_id: meetingId != null ? "n1" : null,
  warnings: [],
});

let root: Root | null = null;
let host: HTMLElement | null = null;
let onMeetingChanged = vi.fn();

async function flush() {
  for (let i = 0; i < 5; i++) await act(async () => {});
}

async function mount(meeting: Meeting) {
  onMeetingChanged = vi.fn();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <AttendeeEditor
        meeting={meeting}
        hasTranscript={false}
        onMeetingChanged={onMeetingChanged}
        onTranscriptRestored={() => {}}
      />,
    ),
  );
  await flush();
}

async function rerender(meeting: Meeting) {
  act(() =>
    root!.render(
      <AttendeeEditor
        meeting={meeting}
        hasTranscript={false}
        onMeetingChanged={onMeetingChanged}
        onTranscriptRestored={() => {}}
      />,
    ),
  );
  await flush();
}

const card = () => host!.querySelector<HTMLElement>('[aria-label="Who\'s on this call"]');
const popover = () => host!.querySelector<HTMLElement>('[role="dialog"][aria-label="Attendees"]');
const button = (text: string, within: Element | null = host) =>
  [...(within?.querySelectorAll<HTMLButtonElement>("button") ?? [])].find(
    (b) => b.textContent?.trim() === text,
  ) ?? null;

async function click(el: Element | null) {
  if (!el) throw new Error("element not found");
  await act(async () => (el as HTMLElement).click());
  await flush();
}

async function key(target: EventTarget, k: string) {
  await act(async () => {
    target.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true }));
  });
  await flush();
}

async function type(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  await act(async () => {
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
}

beforeEach(() => {
  vi.restoreAllMocks();
});

afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
  host = null;
});

describe("speaker card", () => {
  it("shows the calendar roster while this meeting records, and never confirms it by itself", async () => {
    const m = meetingOf();
    vi.spyOn(api, "recordingStatus").mockResolvedValue(recording(m.id));
    const update = vi.spyOn(api, "updateMeetingAttendees");
    const before = document.activeElement;
    await mount(m);
    expect(card()).not.toBeNull();
    expect(card()!.textContent).toContain("From the invite");
    expect(card()!.textContent).toContain("Dana Osei");
    expect(card()!.textContent).toContain("Marc Reyes");
    // it never takes focus: you may already be typing notes
    expect(document.activeElement).toBe(before);
    // showing (and re-rendering) the card writes nothing
    await rerender({ ...m });
    expect(update).not.toHaveBeenCalled();
    expect(onMeetingChanged).not.toHaveBeenCalled();
  });

  it("confirms the roster exactly as it is on 'Looks right'", async () => {
    const m = meetingOf();
    vi.spyOn(api, "recordingStatus").mockResolvedValue(recording(m.id));
    const confirmed = { ...m, attendees_confirmed: true };
    const update = vi.spyOn(api, "updateMeetingAttendees").mockResolvedValue(confirmed);
    await mount(m);
    await click(button("Looks right", card()));
    expect(update).toHaveBeenCalledTimes(1);
    expect(update).toHaveBeenCalledWith(m.id, SEEDED);
    expect(onMeetingChanged).toHaveBeenCalledWith(confirmed);
    await rerender(confirmed);
    expect(card()).toBeNull();
  });

  it("opens the editor with a new row for 'Someone else joined'", async () => {
    const m = meetingOf();
    vi.spyOn(api, "recordingStatus").mockResolvedValue(recording(m.id));
    const update = vi.spyOn(api, "updateMeetingAttendees");
    await mount(m);
    await click(button("Someone else joined", card()));
    expect(popover()).not.toBeNull();
    expect(card()).toBeNull();
    expect(popover()!.querySelectorAll("input[data-name-input]")).toHaveLength(SEEDED.length + 1);
    expect(update).not.toHaveBeenCalled();
  });

  it("stays hidden when this meeting isn't the one recording", async () => {
    vi.spyOn(api, "recordingStatus").mockResolvedValue(recording("some-other-meeting"));
    await mount(meetingOf());
    expect(card()).toBeNull();
  });

  it("stays hidden for a roster you already confirmed", async () => {
    const m = meetingOf({ attendees_confirmed: true });
    vi.spyOn(api, "recordingStatus").mockResolvedValue(recording(m.id));
    await mount(m);
    expect(card()).toBeNull();
  });

  it("goes away when the recording stops", async () => {
    const m = meetingOf();
    const status = vi.spyOn(api, "recordingStatus").mockResolvedValue(recording(m.id));
    await mount(m);
    expect(card()).not.toBeNull();
    status.mockResolvedValue(recording(null));
    await rerender({ ...m, ended_at: new Date().toISOString() });
    expect(card()).toBeNull();
  });

  it("dismisses with the X and with Esc, and stays dismissed for that meeting", async () => {
    const m = meetingOf();
    vi.spyOn(api, "recordingStatus").mockResolvedValue(recording(m.id));
    await mount(m);
    await click(card()!.querySelector('button[aria-label="Not now"]'));
    expect(card()).toBeNull();
    await rerender({ ...m });
    expect(card()).toBeNull();

    const m2 = meetingOf();
    vi.spyOn(api, "recordingStatus").mockResolvedValue(recording(m2.id));
    await rerender(m2);
    expect(card()).not.toBeNull();
    await key(document.body, "Escape");
    expect(card()).toBeNull();
  });

  it("without a calendar roster, asks who's on the call and confirms only what you typed", async () => {
    const m = meetingOf({ attendees: [] });
    vi.spyOn(api, "recordingStatus").mockResolvedValue(recording(m.id));
    const update = vi
      .spyOn(api, "updateMeetingAttendees")
      .mockResolvedValue({ ...m, attendees: [{ name: "Priya" }], attendees_confirmed: true });
    await mount(m);
    expect(card()!.textContent).toContain("Who's on this call?");
    // an empty list would mean "just you": never confirm it from here
    expect(button("Done", card())!.disabled).toBe(true);
    const input = card()!.querySelector<HTMLInputElement>('input[aria-label="Add a name"]')!;
    await type(input, "Priya");
    await key(input, "Enter");
    expect(card()!.textContent).toContain("Priya");
    expect(input.value).toBe("");
    await click(button("Done", card()));
    expect(update).toHaveBeenCalledWith(m.id, [{ name: "Priya" }]);
  });
});

describe("attendee popover", () => {
  it("closes with Esc through the shared layer stack", async () => {
    vi.spyOn(api, "recordingStatus").mockResolvedValue(recording(null));
    await mount(meetingOf({ attendees_confirmed: true }));
    await click(host!.querySelector('button[aria-haspopup="dialog"]'));
    expect(popover()).not.toBeNull();
    await key(document.body, "Escape");
    expect(popover()).toBeNull();
  });
});
