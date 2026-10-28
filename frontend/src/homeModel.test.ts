import { describe, expect, it } from "vitest";
import {
  AI_USES,
  dateGroupLabel,
  eventGuests,
  greeting,
  groupNotesByDate,
  isLoopbackUrl,
  meetingApp,
  nextVideoEvent,
  noteStatus,
  privacySummary,
  runStateOf,
  SEARCH_USE,
  rowDateLabel,
  upNextState,
  upNextWhen,
  type LiveNoteState,
} from "./homeModel";
import type { CalendarEvent, LlmSettings, NoteSummary, PipelineProgress } from "./types";

// Local-time constructors keep these independent of the runner's timezone.
const NOW = new Date(2026, 9, 27, 14, 0); // Tue Oct 27 2026, 2:00 PM
const at = (y: number, m: number, d: number, h = 10) => new Date(y, m, d, h, 0).toISOString();

describe("dateGroupLabel", () => {
  it.each([
    [at(2026, 9, 27, 0), "Today"],
    [at(2026, 9, 27, 23), "Today"],
    [at(2026, 9, 26, 23), "Yesterday"],
    [at(2026, 9, 25), "Previous 7 days"],
    [at(2026, 9, 21), "Previous 7 days"],
    [at(2026, 9, 20), "Previous 30 days"],
    [at(2026, 8, 28), "Previous 30 days"],
    [at(2026, 8, 27), "September"],
    [at(2025, 11, 3), "December 2025"],
    [at(2026, 9, 28), "Upcoming"],
    ["not a date", "Undated"],
  ])("%s → %s", (iso, label) => {
    expect(dateGroupLabel(iso, NOW)).toBe(label);
  });
});

const note = (id: string, happened_at: string, meeting_id: string | null = null): NoteSummary => ({
  id,
  title: id,
  folder_id: null,
  meeting_id,
  updated_at: happened_at,
  happened_at,
});

describe("groupNotesByDate", () => {
  it("keeps the list order and groups consecutive runs", () => {
    const groups = groupNotesByDate(
      [
        note("a", at(2026, 9, 27, 13)),
        note("b", at(2026, 9, 27, 9)),
        note("c", at(2026, 9, 26)),
        note("d", at(2026, 9, 23)),
        note("e", at(2026, 6, 1)),
      ],
      NOW,
    );
    expect(groups.map((g) => [g.label, g.notes.map((n) => n.id)])).toEqual([
      ["Today", ["a", "b"]],
      ["Yesterday", ["c"]],
      ["Previous 7 days", ["d"]],
      ["July", ["e"]],
    ]);
  });

  it("falls back to updated_at for summaries cached before happened_at existed", () => {
    const legacy = { ...note("x", ""), happened_at: undefined } as unknown as NoteSummary;
    legacy.updated_at = at(2026, 9, 27);
    expect(groupNotesByDate([legacy], NOW)[0].label).toBe("Today");
  });

  it("no notes → no groups", () => {
    expect(groupNotesByDate([], NOW)).toEqual([]);
  });
});

describe("rowDateLabel", () => {
  it("today and yesterday show only the time (the group names the day)", () => {
    expect(rowDateLabel(at(2026, 9, 27, 9), NOW)).not.toMatch(/Oct|·/);
    expect(rowDateLabel(at(2026, 9, 26, 9), NOW)).not.toMatch(/Oct|·/);
  });

  it("within a week: weekday + time; older: date + time; another year adds it", () => {
    expect(rowDateLabel(at(2026, 9, 23, 9), NOW)).toMatch(/·/);
    expect(rowDateLabel(at(2026, 9, 23, 9), NOW)).not.toMatch(/23/);
    expect(rowDateLabel(at(2026, 6, 1, 9), NOW)).toMatch(/1/);
    expect(rowDateLabel(at(2025, 6, 1, 9), NOW)).toMatch(/2025/);
  });
});

describe("noteStatus", () => {
  const idle: LiveNoteState = {
    recordingNoteId: null,
    runningMeetingIds: new Set(),
    queuedMeetingIds: new Set(),
    failedMeetingIds: new Set(),
  };
  const meeting = (over: Partial<NoteSummary> = {}): NoteSummary => ({
    ...note("n1", NOW.toISOString(), "m1"),
    has_recording: true,
    has_transcript: false,
    has_enhanced_doc: false,
    job_status: null,
    ...over,
  });
  const label = (n: NoteSummary, live: LiveNoteState = idle) => noteStatus(n, live)?.label ?? null;

  it.each([
    ["no meeting", note("n1", NOW.toISOString()), "Notes only"],
    ["recorded, nothing queued", meeting(), "Not transcribed"],
    ["queued (waiting or requeued after a crash)", meeting({ job_status: "queued" }), "Queued"],
    ["running", meeting({ job_status: "running" }), "Transcribing"],
    ["failed", meeting({ job_status: "failed" }), "Failed"],
    [
      "done with a transcript",
      meeting({ job_status: "done", has_transcript: true }),
      "Ready to enhance",
    ],
    ["no job, transcript", meeting({ has_transcript: true }), "Ready to enhance"],
    ["enhanced", meeting({ has_transcript: true, has_enhanced_doc: true }), "Enhanced"],
    [
      "audio deleted, transcript kept",
      meeting({ has_recording: false, has_transcript: true }),
      "Ready to enhance",
    ],
    [
      "staged import: no audio attached yet, no job",
      meeting({ has_recording: false }),
      "Not transcribed",
    ],
    ["summary cached before the fields existed", note("n1", NOW.toISOString(), "m1"), null],
  ])("stored: %s → %s", (_what, n, expected) => {
    expect(label(n)).toBe(expected);
  });

  it("tones: failed is danger, enhanced success, ready primary, recording live", () => {
    expect(noteStatus(meeting({ job_status: "failed" }), idle)?.tone).toBe("danger");
    expect(noteStatus(meeting({ has_enhanced_doc: true }), idle)?.tone).toBe("success");
    expect(noteStatus(meeting({ has_transcript: true }), idle)?.tone).toBe("primary");
    expect(noteStatus(meeting(), { ...idle, recordingNoteId: "n1" })?.tone).toBe("live");
  });

  describe("live state beats the stored row", () => {
    const running = { ...idle, runningMeetingIds: new Set(["m1"]) };
    const queued = { ...idle, queuedMeetingIds: new Set(["m1"]) };
    const failed = { ...idle, failedMeetingIds: new Set(["m1"]) };

    it("a waiting (queued) event shows Queued, not Transcribing", () => {
      expect(label(meeting(), queued)).toBe("Queued");
      expect(label(meeting({ has_transcript: true }), queued)).toBe("Queued");
      // a run that started beats the queue
      expect(label(meeting(), { ...queued, runningMeetingIds: new Set(["m1"]) })).toBe(
        "Transcribing",
      );
      // a retry queued after this session's failure
      expect(label(meeting(), { ...queued, failedMeetingIds: new Set(["m1"]) })).toBe("Queued");
    });

    it("recording beats everything", () => {
      const n = meeting({ job_status: "failed", has_enhanced_doc: true });
      expect(label(n, { ...running, recordingNoteId: "n1" })).toBe("Recording");
    });

    it("a run in progress beats a stored failure, queue or finished doc", () => {
      for (const n of [
        meeting({ job_status: "failed" }),
        meeting({ job_status: "queued" }),
        meeting({ job_status: "done", has_transcript: true, has_enhanced_doc: true }),
      ]) {
        expect(label(n, running)).toBe("Transcribing");
      }
    });

    it("a retry in flight beats this session's failure", () => {
      expect(label(meeting(), { ...running, failedMeetingIds: new Set(["m1"]) })).toBe(
        "Transcribing",
      );
    });

    it("this session's failure beats a stored queued/running job and an old transcript", () => {
      for (const n of [
        meeting({ job_status: "running" }),
        meeting({ job_status: "queued" }),
        meeting({ has_transcript: true, has_enhanced_doc: true }),
      ]) {
        expect(label(n, failed)).toBe("Failed");
      }
    });

    it("live state for another meeting doesn't touch this row", () => {
      const other = { ...idle, runningMeetingIds: new Set(["m9"]), recordingNoteId: "n9" };
      expect(label(meeting({ has_transcript: true }), other)).toBe("Ready to enhance");
    });
  });
});

const ev = (over: Partial<CalendarEvent>): CalendarEvent => ({
  id: "e",
  provider: "google",
  title: "Sync",
  start: new Date(NOW.getTime() + 10 * 60_000).toISOString(),
  end: new Date(NOW.getTime() + 40 * 60_000).toISOString(),
  attendees: [],
  join_url: "https://zoom.us/j/1",
  ...over,
});

describe("Up next", () => {
  const nowMs = NOW.getTime();

  it("offers only events with a join link (the Sidebar rule)", () => {
    expect(nextVideoEvent([ev({ join_url: null }), ev({ join_url: "" })], nowMs)).toBeNull();
  });

  it("an in-progress meeting comes before the next one; ended ones are skipped", () => {
    const ended = ev({
      id: "ended",
      start: new Date(nowMs - 60 * 60_000).toISOString(),
      end: new Date(nowMs - 5 * 60_000).toISOString(),
    });
    const live = ev({
      id: "live",
      start: new Date(nowMs - 5 * 60_000).toISOString(),
      end: new Date(nowMs + 25 * 60_000).toISOString(),
    });
    const later = ev({ id: "later" });
    expect(nextVideoEvent([later, ended, live], nowMs)?.id).toBe("live");
  });

  it.each([
    ["https://acme.zoom.us/j/123", "Zoom"],
    ["https://meet.google.com/abc-defg-hij", "Google Meet"],
    ["https://teams.microsoft.com/l/meetup-join/x", "Teams"],
    ["https://teams.live.com/meet/1", "Teams"],
    ["https://acme.webex.com/meet/x", "Webex"],
    ["https://evilzoom.us.example.com/x", null],
    ["not a url", null],
    [null, null],
  ])("meetingApp(%s) → %s", (url, app) => {
    expect(meetingApp(url)).toBe(app);
  });

  it("when: now, minutes away, or a clock time", () => {
    expect(upNextWhen(ev({ start: new Date(nowMs - 60_000).toISOString() }), nowMs)).toBe(
      "Now · Zoom",
    );
    expect(upNextWhen(ev({}), nowMs)).toBe("In 10 min · Zoom");
    expect(
      upNextWhen(ev({ start: new Date(nowMs + 90 * 60_000).toISOString(), join_url: "x" }), nowMs),
    ).toMatch(/^At /);
  });

  it("guests leave out you and anyone who declined", () => {
    const e = ev({
      attendees: [
        { email: "me@x.com", name: "Me", is_self: true, declined: false },
        { email: "dana@x.com", name: "Dana", is_self: false, declined: false },
        { email: "sam@x.com", is_self: false, declined: false },
        { email: "no@x.com", name: "No", is_self: false, declined: true },
      ],
    });
    expect(eventGuests(e)).toEqual(["Dana", "sam@x.com"]);
  });

  it("card state: event, no calendar, or nothing left today", () => {
    expect(upNextState(true, [], ev({}))).toBe("event");
    expect(upNextState(false, [], null)).toBe("no-calendar");
    expect(upNextState(true, [], null)).toBe("empty");
    // expired: Reconnect, never "connect" or a guess that nothing's on
    expect(upNextState(true, ["google"], null)).toBe("expired");
    expect(upNextState(false, ["google"], null)).toBe("expired");
    // a meeting from another calendar still shows
    expect(upNextState(true, ["google"], ev({}))).toBe("event");
    // unknown yet: neutral copy, never a wrong prompt
    expect(upNextState(null, [], null)).toBe("empty");
  });
});

describe("runStateOf", () => {
  const p = (over: Partial<PipelineProgress>): PipelineProgress => ({
    meeting_id: "m1",
    stage: "transcribing",
    detail: null,
    done: false,
    error: null,
    ...over,
  });

  it("the scheduler's 'waiting' stage means queued", () => {
    expect(runStateOf(p({ stage: "waiting" }))).toBe("queued");
  });

  it("any other live stage means running", () => {
    for (const stage of ["starting", "transcribing", "diarizing", "polishing"]) {
      expect(runStateOf(p({ stage }))).toBe("running");
    }
  });

  it("done, failed or cancelled ends it", () => {
    expect(runStateOf(p({ stage: "done", done: true }))).toBeNull();
    expect(runStateOf(p({ stage: "error", done: true, error: "x" }))).toBeNull();
    expect(runStateOf(p({ stage: "cancelled", done: true }))).toBeNull();
  });
});

describe("greeting", () => {
  it.each([
    [8, "Good morning"],
    [13, "Good afternoon"],
    [19, "Good evening"],
    [2, "Good evening"],
  ])("%i:00 → %s", (h, text) => {
    expect(greeting(new Date(2026, 9, 27, h))).toBe(text);
  });
});

describe("privacySummary", () => {
  const providers = [
    {
      id: "ollama",
      default_model: "llama3.1",
      is_local: true,
      has_key: true,
      model: null,
      base_url: null,
    },
    {
      id: "anthropic",
      default_model: "c",
      is_local: false,
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
    { id: "nim", default_model: "n", is_local: false, has_key: true, model: null, base_url: null },
  ];
  const llm = (provider: string, base_url: string | null = null): LlmSettings => ({
    provider,
    providers: providers.map((p) => (p.id === provider ? { ...p, base_url } : p)),
  });
  const local = { tier: "balanced", use_groq: false };

  it("is null until both settings load (never claims local early)", () => {
    expect(privacySummary(null, llm("ollama"))).toBeNull();
    expect(privacySummary(local, null)).toBeNull();
  });

  it("Ollama on this computer + local transcription → On this computer", () => {
    for (const url of [null, "http://localhost:11434", "http://127.0.0.1:11434"]) {
      const s = privacySummary(local, llm("ollama", url))!;
      expect(s.offDevice).toBe(false);
      expect(s.label).toBe("On this computer");
      expect(s.uses).toEqual([]);
    }
  });

  it.each([
    ["anthropic", null, "Anthropic"],
    ["openai", null, "OpenAI"],
    ["nim", null, "NVIDIA"],
    ["openai", "https://gateway.example.com/v1", "gateway.example.com"],
  ])("%s (%s) → Cloud on · %s, with every AI use listed", (id, url, where) => {
    const s = privacySummary(local, llm(id, url))!;
    expect(s.offDevice).toBe(true);
    expect(s.label).toBe(`Cloud on · ${where}`);
    expect(s.uses).toEqual([{ where, what: AI_USES }]);
    expect(s.footnote).toBe("Except notes you keep local.");
  });

  it("lists Ask, Enhance, transcript cleanup and action items", () => {
    expect(AI_USES).toEqual(["Ask", "Enhance", "Transcript cleanup", "Action items"]);
  });

  it("Groq transcription counts, alone or with a cloud AI provider", () => {
    const groqOnly = privacySummary({ tier: "cloud", use_groq: true }, llm("ollama"))!;
    expect(groqOnly.label).toBe("Cloud on · Groq");
    expect(groqOnly.uses).toEqual([{ where: "Groq", what: ["Transcription (audio)"] }]);
    const hybrid = privacySummary({ tier: "balanced", use_groq: true }, llm("anthropic"))!;
    expect(hybrid.label).toBe("Cloud on · Anthropic, Groq");
  });

  it("an Ollama server on another machine is off this computer, not 'cloud'", () => {
    const s = privacySummary(local, llm("ollama", "http://192.168.1.20:11434"))!;
    expect(s.offDevice).toBe(true);
    expect(s.label).toBe("Off this computer · 192.168.1.20:11434");
  });

  it("a remote Ollama gets the search index even with a cloud provider", () => {
    const settings: LlmSettings = {
      provider: "anthropic",
      providers: providers.map((p) =>
        p.id === "ollama" ? { ...p, base_url: "http://gpu-box.lan:11434" } : p,
      ),
    };
    const s = privacySummary(local, settings)!;
    expect(s.uses).toEqual([
      { where: "Anthropic", what: AI_USES },
      { where: "gpu-box.lan:11434", what: [SEARCH_USE] },
    ]);
    expect(s.label).toBe("Cloud on · Anthropic, gpu-box.lan:11434");
  });

  it("a remote Ollama is never 'On this computer', even with local transcription", () => {
    const s = privacySummary(local, llm("ollama", "http://192.168.1.20:11434"))!;
    // the AI uses and the search index go to the same host: one row
    expect(s.uses).toEqual([{ where: "192.168.1.20:11434", what: [...AI_USES, SEARCH_USE] }]);
    expect(s.label).not.toBe("On this computer");
  });

  it("a provider this build doesn't list is assumed off-device", () => {
    const s = privacySummary(local, { provider: "mistral", providers })!;
    expect(s.label).toBe("Cloud on · mistral");
  });
});

describe("isLoopbackUrl", () => {
  it.each([
    ["http://localhost:11434", true],
    ["http://127.0.0.1:1234", true],
    ["http://[::1]:8080", true],
    ["http://127.1:11434", true], // normalized to 127.0.0.1
    ["http://127.5.6.7", true],
    ["http://127.example.com:11434", false],
    ["http://127.0.0.1.nip.io", false],
    ["http://192.168.1.2:11434", false],
    ["https://api.openai.com", false],
    ["garbage", false],
  ])("%s → %s", (url, loop) => {
    expect(isLoopbackUrl(url)).toBe(loop);
  });
});
