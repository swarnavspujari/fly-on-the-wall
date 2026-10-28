// @vitest-environment jsdom
// The recording bar against the dev mock backend (devMock.ts), the same stub
// used for port-1420 QA.
import "../devMock";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import { recordingNoticeText } from "../privacy";
import { REMIND_NOTICE_KEY } from "../recordingBar";
import type { AsrSettings, RecordingStatus } from "../types";
import RecordingBar from "./RecordingBar";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const MUTED =
  "System output is muted — the other participants' audio will record as silence. Unmute your speakers.";
const MACOS =
  "System audio is playing but its capture is recording only silence — macOS is blocking system-audio capture for this build. Open System Settings → Privacy & Security → Screen & System Audio Recording and allow Fly on the Wall.";

const status = (over: Partial<RecordingStatus> = {}): RecordingStatus => ({
  active: true,
  state: "recording",
  elapsed_ms: 65_000,
  meeting_id: `m-${Math.random().toString(36).slice(2)}`,
  note_id: "n1",
  warnings: [],
  ...over,
});

let root: Root | null = null;
let host: HTMLElement | null = null;

async function flush() {
  for (let i = 0; i < 5; i++) await act(async () => {});
}

async function mount(s: RecordingStatus, props: Partial<Parameters<typeof RecordingBar>[0]> = {}) {
  const handlers = {
    onPause: vi.fn(),
    onResume: vi.fn(),
    onStop: vi.fn(),
    onOpenNote: vi.fn(),
    onOpenPrivacySettings: vi.fn(),
  };
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(<RecordingBar status={s} noteTitle="Acme sync" {...handlers} {...props} />),
  );
  await flush();
  return handlers;
}

const $$ = (sel: string) => [...host!.querySelectorAll<HTMLElement>(sel)];
const byText = (text: string) =>
  $$("button, label, span, [role]").find((el) => el.textContent?.trim() === text) ?? null;
const text = () => host!.textContent ?? "";
const chipButton = (label: string) => host!.querySelector(`button[aria-label="${label}"]`);

async function click(el: Element | null) {
  if (!el) throw new Error("element not found");
  await act(async () => (el as HTMLElement).click());
  await flush();
}

async function pressEscape() {
  await act(async () => {
    document.body.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
  });
  await flush();
}

beforeEach(async () => {
  vi.restoreAllMocks();
  await api.setAppSetting(REMIND_NOTICE_KEY, "true");
});

afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
  host = null;
});

describe("RecordingBar", () => {
  it("renders nothing when no recording is active", async () => {
    await mount(status({ active: false, meeting_id: null, note_id: null, state: null }));
    expect(host!.childElementCount).toBe(0);
  });

  it("keeps everything on one row and stops with 'Stop and transcribe'", async () => {
    const h = await mount(status());
    expect($$("[data-recording-bar-row]")).toHaveLength(1);
    await click(byText("Stop and transcribe"));
    expect(h.onStop).toHaveBeenCalledTimes(1);
  });

  it("says 'Stop' when transcribing on stop is turned off", async () => {
    const real = await api.getAsrSettings();
    vi.spyOn(api, "getAsrSettings").mockResolvedValue({
      ...real,
      auto_transcribe: false,
    } as AsrSettings);
    await mount(status());
    expect(byText("Stop and transcribe")).toBeNull();
    expect(byText("Stop")).not.toBeNull();
  });

  it("turns a warning into a chip and opens the full cause from it", async () => {
    await mount(status({ warnings: [MUTED] }));
    const chip = chipButton("Others · Output muted");
    expect(chip).not.toBeNull();
    // the full sentence lives behind the chip, not in a second strip
    expect(text()).not.toContain("Unmute your speakers");
    await click(chip);
    expect(text()).toContain("Unmute your speakers");
    await pressEscape();
    expect(text()).not.toContain("Unmute your speakers");
  });

  it("offers Open Settings for the macOS warning", async () => {
    const h = await mount(status({ warnings: [MACOS] }));
    await click(chipButton("Others · Blocked by macOS"));
    await click(byText("Open Settings"));
    expect(h.onOpenPrivacySettings).toHaveBeenCalledTimes(1);
  });

  it("shows the chips with no warnings, without claiming the mic is fine", async () => {
    await mount(status());
    const chip = (label: string) =>
      $$("span[title]").find((el) => el.textContent?.trim() === label) ?? null;
    expect(chip("You")?.getAttribute("title")).toBe("Your microphone: not monitored yet");
    expect(chip("Others")?.getAttribute("title")).toContain("no problems detected");
  });

  it("gives focus back when a warning clears with its details open", async () => {
    const s = status({ warnings: [MUTED] });
    const h = await mount(s);
    await click(chipButton("Others · Output muted"));
    expect(document.activeElement?.closest('[role="dialog"]')).not.toBeNull();
    act(() =>
      root!.render(<RecordingBar status={{ ...s, warnings: [] }} noteTitle="Acme sync" {...h} />),
    );
    await flush();
    expect(host!.querySelector('[role="dialog"]')).toBeNull();
    expect(document.activeElement).not.toBe(document.body);
    expect(host!.contains(document.activeElement)).toBe(true);
  });

  it("opens the note from an icon when the title link is hidden", async () => {
    const h = await mount(status());
    await click(host!.querySelector('button[aria-label="Open the recording\'s note"]'));
    expect(h.onOpenNote).toHaveBeenCalledTimes(1);
  });

  it("re-reads 'Transcribe when I stop' when you reach for Stop", async () => {
    const real = await api.getAsrSettings();
    const asr = vi.spyOn(api, "getAsrSettings").mockResolvedValue(real);
    await mount(status());
    expect(byText("Stop and transcribe")).not.toBeNull();
    asr.mockResolvedValue({ ...real, auto_transcribe: false } as AsrSettings);
    await act(async () => {
      byText("Stop and transcribe")!.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
    });
    await flush();
    expect(byText("Stop")).not.toBeNull();
    asr.mockResolvedValue(real);
    await act(async () => byText("Stop")!.focus());
    await flush();
    expect(byText("Stop and transcribe")).not.toBeNull();
  });

  it("copies the recording notice from the bar", async () => {
    const copy = vi.spyOn(api, "copyRecordingNotice").mockResolvedValue();
    await mount(status());
    await click(byText("Copy recording notice"));
    expect(copy).toHaveBeenCalledWith(recordingNoticeText("Acme sync"));
    expect(byText("Copied")).not.toBeNull();
  });

  it("keeps the participants-notified state in the bar", async () => {
    const s = status();
    const mark = vi.spyOn(api, "setParticipantsNotified");
    await mount(s);
    const box = host!.querySelector<HTMLInputElement>('input[aria-label="Participants notified"]');
    expect(box?.checked).toBe(false);
    await click(box);
    expect(mark).toHaveBeenCalledWith(s.meeting_id, true);
    expect(box?.checked).toBe(true);
  });

  it("reminds you to tell people at the start, without taking focus", async () => {
    const before = document.activeElement;
    await mount(status());
    expect($$('[aria-label="Reminder: let participants know"]')).toHaveLength(1);
    expect(document.activeElement).toBe(before);
    // ticking "notified" is the answer to the reminder
    await click(host!.querySelector('input[aria-label="Participants notified"]'));
    expect($$('[aria-label="Reminder: let participants know"]')).toHaveLength(0);
  });

  it("ignores a stray Esc meant for something else, but closes on Esc from inside", async () => {
    await mount(status());
    // e.g. typing in the notes editor or cancelling a rename
    await pressEscape();
    expect($$('[aria-label="Reminder: let participants know"]')).toHaveLength(1);
    const inside = host!.querySelector<HTMLElement>(
      '[aria-label="Reminder: let participants know"] button',
    )!;
    inside.focus();
    await act(async () => {
      inside.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
      );
    });
    await flush();
    expect($$('[aria-label="Reminder: let participants know"]')).toHaveLength(0);
    // focus returns to the notice button the reminder hangs from
    expect(document.activeElement).toBe(chipButton("Copy recording notice"));
  });

  it("returns focus to the notice button when you hide the reminder", async () => {
    await mount(status());
    const hide = host!.querySelector<HTMLElement>('button[aria-label="Hide the reminder"]')!;
    hide.focus(); // as when you Tab to it
    await click(hide);
    expect($$('[aria-label="Reminder: let participants know"]')).toHaveLength(0);
    expect(document.activeElement).toBe(chipButton("Copy recording notice"));
  });

  it("doesn't remind when 'Remind me' is off", async () => {
    await api.setAppSetting(REMIND_NOTICE_KEY, "false");
    await mount(status());
    expect($$('[aria-label="Reminder: let participants know"]')).toHaveLength(0);
    // the notice is still one click away
    expect(byText("Copy recording notice")).not.toBeNull();
  });

  it("doesn't remind when participants were already notified", async () => {
    const s = status();
    await api.setParticipantsNotified(s.meeting_id!, true);
    await mount(s);
    expect($$('[aria-label="Reminder: let participants know"]')).toHaveLength(0);
    expect(
      host!.querySelector<HTMLInputElement>('input[aria-label="Participants notified"]')?.checked,
    ).toBe(true);
  });

  it("pauses and resumes", async () => {
    const h = await mount(status());
    await click(byText("Pause"));
    expect(h.onPause).toHaveBeenCalledTimes(1);
    if (root) act(() => root!.unmount());
    host?.remove();
    const h2 = await mount(status({ state: "paused" }));
    await click(byText("Resume"));
    expect(h2.onResume).toHaveBeenCalledTimes(1);
  });
});
