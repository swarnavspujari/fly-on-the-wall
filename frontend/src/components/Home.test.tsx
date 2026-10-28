// @vitest-environment jsdom
import { act } from "react";
import type { ComponentProps, ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { privacySummary } from "../homeModel";
import type { CalendarEvent, CalendarStatus, NoteSummary } from "../types";
import Home from "./Home";
import NoteList from "./NoteList";
import PrivacyPill from "./PrivacyPill";
import UndoToast from "./UndoToast";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let mounted: { root: Root; host: HTMLElement }[] = [];

function render(ui: ReactNode) {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  act(() => root.render(ui));
  mounted.push({ root, host });
  return host;
}

const NOW = new Date(2026, 9, 27, 14, 0);

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
  vi.setSystemTime(NOW);
});

afterEach(() => {
  for (const { root, host } of mounted) {
    act(() => root.unmount());
    host.remove();
  }
  mounted = [];
  vi.useRealTimers();
});

const click = (el: Element | null | undefined) => {
  if (!el) throw new Error("element not found");
  act(() => {
    (el as HTMLElement).click();
  });
};
const button = (host: HTMLElement, label: string) =>
  Array.from(host.querySelectorAll("button")).find((b) => b.textContent?.trim() === label);

const CONNECTED: CalendarStatus = {
  google_client_id: "",
  google_has_secret: false,
  google_connected: true,
  google_configured: true,
  ms_client_id: "",
  ms_connected: false,
  ms_configured: true,
};
const NONE: CalendarStatus = { ...CONNECTED, google_connected: false };

const EVENT: CalendarEvent = {
  id: "e1",
  provider: "google",
  title: "Weekly product sync",
  start: new Date(NOW.getTime() + 4 * 60_000).toISOString(),
  end: new Date(NOW.getTime() + 34 * 60_000).toISOString(),
  attendees: [
    { email: "me@x.com", name: "Me", is_self: true, declined: false },
    { email: "dana@x.com", name: "Dana Kim", is_self: false, declined: false },
    { email: "marco@x.com", name: "Marco Ruiz", is_self: false, declined: false },
  ],
  join_url: "https://acme.zoom.us/j/1",
};

type HomeProps = ComponentProps<typeof Home>;
function homeProps(over: Partial<HomeProps> = {}): HomeProps {
  return {
    recording: false,
    upcoming: [],
    calendar: CONNECTED,
    needsReconnect: [],
    calendarBusy: null,
    hasNotes: true,
    onRecord: vi.fn(),
    onStartFromEvent: vi.fn(),
    onConnectCalendar: vi.fn(),
    onOpenSettings: vi.fn(),
    onImport: vi.fn(),
    onNewNote: vi.fn(),
    ...over,
  };
}

describe("Home", () => {
  it("greets by time of day", () => {
    const host = render(<Home {...homeProps()} />);
    expect(host.querySelector("h1")?.textContent).toBe("Good afternoon");
  });

  it("Up next: the next video meeting with when, app, guests and Start recording", () => {
    const p = homeProps({ upcoming: [EVENT] });
    const host = render(<Home {...p} />);
    const card = host.querySelector('[aria-label="Up next"]')!;
    expect(card.textContent).toContain("In 4 min · Zoom");
    expect(card.textContent).toContain("Weekly product sync");
    expect(card.textContent).toContain("3 people");
    click(button(host, "Start recording"));
    expect(p.onStartFromEvent).toHaveBeenCalledWith(EVENT);
  });

  it("a link-less event isn't offered, and the empty copy says why", () => {
    const p = homeProps({ upcoming: [{ ...EVENT, join_url: null }] });
    const host = render(<Home {...p} />);
    expect(host.textContent).not.toContain("Weekly product sync");
    expect(host.textContent).toContain("Ready when your call starts");
    expect(host.textContent).toContain("Up next shows events with a join link");
    click(button(host, "Record"));
    expect(p.onRecord).toHaveBeenCalledTimes(1);
  });

  it("no calendar: prompt with a connect button per configured provider", () => {
    const p = homeProps({ calendar: NONE });
    const host = render(<Home {...p} />);
    expect(host.textContent).toContain("Connect a calendar to see your meetings here.");
    click(button(host, "Connect Google Calendar"));
    expect(p.onConnectCalendar).toHaveBeenCalledWith("google");
    expect(button(host, "Connect Outlook")).toBeTruthy();
  });

  it("no calendar and none configured: point to Settings", () => {
    const p = homeProps({
      calendar: { ...NONE, google_configured: false, ms_configured: false },
    });
    const host = render(<Home {...p} />);
    click(button(host, "Set up a calendar in Settings"));
    expect(p.onOpenSettings).toHaveBeenCalledTimes(1);
  });

  it("while the sign-in window is open, connect buttons wait", () => {
    const host = render(<Home {...homeProps({ calendar: NONE, calendarBusy: "google" })} />);
    const waiting = button(host, "Waiting for browser…");
    expect(waiting?.hasAttribute("disabled")).toBe(true);
    expect(button(host, "Connect Outlook")?.hasAttribute("disabled")).toBe(true);
  });

  it("expired calendar: a banner with Reconnect, and no connect prompt", () => {
    const p = homeProps({ needsReconnect: ["google"] });
    const host = render(<Home {...p} />);
    expect(host.textContent).toContain("Google Calendar disconnected.");
    expect(host.textContent).toContain("Your meetings show here again once you reconnect.");
    expect(host.textContent).not.toContain("No video meetings");
    click(button(host, "Reconnect"));
    expect(p.onConnectCalendar).toHaveBeenCalledWith("google");
    expect(host.textContent).not.toContain("Connect a calendar");
  });

  it("empty state: import and new note, and no 'Bring notes' (no backend yet)", () => {
    const p = homeProps({ hasNotes: false });
    const host = render(<Home {...p} />);
    expect(host.textContent).toContain("No meetings yet");
    click(button(host, "Import recordings"));
    expect(p.onImport).toHaveBeenCalledTimes(1);
    click(button(host, "New note"));
    expect(p.onNewNote).toHaveBeenCalledTimes(1);
    expect(host.textContent).not.toMatch(/another computer|Bring notes/i);
  });

  it("no empty state when there are notes (or the view can't tell)", () => {
    for (const hasNotes of [true, null]) {
      const host = render(<Home {...homeProps({ hasNotes })} />);
      expect(host.textContent).not.toContain("No meetings yet");
    }
  });

  it("while recording, the record buttons give way to a status", () => {
    const host = render(<Home {...homeProps({ recording: true, upcoming: [EVENT] })} />);
    expect(button(host, "Start recording")).toBeUndefined();
    expect(host.textContent).toContain("Recording");
  });
});

const note = (
  id: string,
  title: string,
  happened: Date,
  meeting_id: string | null,
): NoteSummary => ({
  id,
  title,
  folder_id: null,
  meeting_id,
  updated_at: happened.toISOString(),
  happened_at: happened.toISOString(),
});

describe("NoteList", () => {
  const notes = [
    note("n1", "Acme renewal", new Date(2026, 9, 27, 11), "m1"),
    note("n2", "1:1 with Dana", new Date(2026, 9, 26, 16), "m2"),
    note("n3", "Board prep", new Date(2026, 9, 23, 9), null),
  ];
  type ListProps = ComponentProps<typeof NoteList>;
  const listProps = (over: Partial<ListProps> = {}): ListProps => ({
    notes,
    searchQuery: "",
    searchHits: [],
    selectedNoteId: null,
    onSearchChange: vi.fn(),
    onOpenNote: vi.fn(),
    onNewNote: vi.fn(),
    onDeleteNote: vi.fn(),
    live: {
      recordingNoteId: null,
      runningMeetingIds: new Set(["m1"]),
      failedMeetingIds: new Set(["m2"]),
    },
    ...over,
  });

  it("groups by date with a heading per group", () => {
    const host = render(<NoteList {...listProps()} />);
    const headings = Array.from(host.querySelectorAll('[role="heading"]')).map(
      (h) => h.textContent,
    );
    expect(headings).toEqual(["Today", "Yesterday", "Previous 7 days"]);
  });

  it("shows a status chip per meeting: live state, then the stored row", () => {
    const host = render(<NoteList {...listProps()} />);
    const text = host.textContent ?? "";
    expect(text).toContain("Transcribing");
    expect(text).toContain("Failed");
    expect(text).toContain("Notes only");
  });

  it("stored states: enhanced, ready, queued", () => {
    const host = render(
      <NoteList
        {...listProps({
          notes: [
            { ...notes[0], has_transcript: true, has_enhanced_doc: true },
            { ...notes[1], has_transcript: true },
            { ...notes[2], meeting_id: "m3", has_recording: true, job_status: "queued" },
          ],
          live: {
            recordingNoteId: null,
            runningMeetingIds: new Set(),
            failedMeetingIds: new Set(),
          },
        })}
      />,
    );
    const chips = Array.from(host.querySelectorAll(".fly-badge")).map((b) => b.textContent);
    expect(chips).toEqual(["Enhanced", "Ready to enhance", "Queued"]);
  });

  it("delete asks no confirm(): the Undo toast covers it", () => {
    const confirmSpy = vi.spyOn(window, "confirm");
    const p = listProps();
    const host = render(<NoteList {...p} />);
    click(host.querySelector('[aria-label="Delete note “Board prep”"]'));
    expect(p.onDeleteNote).toHaveBeenCalledWith("n3");
    expect(confirmSpy).not.toHaveBeenCalled();
    expect(p.onOpenNote).not.toHaveBeenCalled();
    confirmSpy.mockRestore();
  });

  it("search results stay ungrouped", () => {
    const host = render(
      <NoteList
        {...listProps({
          searchQuery: "acme",
          searchHits: [
            {
              kind: "note",
              note_id: "n1",
              title: "Acme renewal",
              snippet: "[[acme]]",
              start_ms: null,
            },
          ],
        })}
      />,
    );
    expect(host.querySelectorAll('[role="heading"]')).toHaveLength(0);
    expect(host.textContent).toContain("Acme renewal");
  });
});

describe("PrivacyPill", () => {
  const providers = [
    {
      id: "ollama",
      default_model: "l",
      is_local: true,
      has_key: true,
      model: null,
      base_url: null,
    },
    {
      id: "openai",
      default_model: "g",
      is_local: false,
      has_key: true,
      model: null,
      base_url: null,
    },
  ];

  it("local: green lock, 'On this computer'", () => {
    const s = privacySummary(
      { tier: "balanced", use_groq: false },
      { provider: "ollama", providers },
    )!;
    const host = render(<PrivacyPill summary={s} onOpen={vi.fn()} />);
    const pill = host.querySelector("button")!;
    expect(pill.textContent).toBe("On this computer");
    expect(pill.className).toContain("text-success-text");
  });

  it("cloud: names the provider; hover lists what goes there and the keep-local exception", () => {
    const s = privacySummary(
      { tier: "balanced", use_groq: true },
      { provider: "openai", providers },
    )!;
    const onOpen = vi.fn();
    const host = render(<PrivacyPill summary={s} onOpen={onOpen} />);
    const pill = host.querySelector("button")!;
    expect(pill.textContent).toBe("Cloud on · OpenAI, Groq");
    expect(pill.className).toContain("text-warning-text");
    const tip = host.querySelector('[role="tooltip"]')!;
    expect(tip.hasAttribute("hidden")).toBe(true);
    expect(pill.getAttribute("aria-describedby")).toBe(tip.id);
    act(() => {
      pill.parentElement!.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
    });
    expect(tip.hasAttribute("hidden")).toBe(false);
    for (const t of ["Sent to OpenAI", "Ask", "Enhance", "Transcript cleanup", "Action items"]) {
      expect(tip.textContent).toContain(t);
    }
    expect(tip.textContent).toContain("Sent to Groq");
    expect(tip.textContent).toContain("Except notes you keep local.");
    click(pill);
    expect(onOpen).toHaveBeenCalledTimes(1);
  });
});

describe("UndoToast", () => {
  it("names the note; Undo and close call back", () => {
    const onUndo = vi.fn();
    const onDismiss = vi.fn();
    const host = render(<UndoToast title="Acme renewal" onUndo={onUndo} onDismiss={onDismiss} />);
    expect(host.textContent).toContain("Deleted “Acme renewal”");
    click(button(host, "Undo"));
    expect(onUndo).toHaveBeenCalledTimes(1);
    click(host.querySelector('[aria-label="Close and delete now"]'));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});
