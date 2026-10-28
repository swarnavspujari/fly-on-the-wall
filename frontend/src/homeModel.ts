/** Pure rules behind Home (v2.4 PR 1.4): date groups and status chips in the
 *  note list, the Up next card, and the sidebar privacy pill. Kept free of
 *  React and IPC so vitest covers them directly. */

import { providerVendor } from "./settingsModel";
import type { CalendarEvent, LlmSettings, NoteSummary, PipelineProgress } from "./types";

/* ------------------------------------------------------------- date groups */

const DAY_MS = 86_400_000;

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** Whole local calendar days from `iso` to `now` (0 = today, negative =
 *  future). Rounded, so a DST shift can't make a day 23 or 25 hours. */
export function daysAgo(iso: string, now: Date): number | null {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return null;
  return Math.round((startOfDay(now) - startOfDay(d)) / DAY_MS);
}

/** Which list group a meeting date falls in: Today, Yesterday, Previous 7
 *  days, Previous 30 days, then by month ("October", or "October 2025" in
 *  another year). Rolling windows rather than calendar weeks, so the labels
 *  read the same in every locale. A date edited into the future is
 *  "Upcoming". */
export function dateGroupLabel(iso: string, now: Date): string {
  const days = daysAgo(iso, now);
  if (days == null) return "Undated";
  if (days < 0) return "Upcoming";
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return "Previous 7 days";
  if (days < 30) return "Previous 30 days";
  const d = new Date(iso);
  return d.toLocaleDateString(undefined, {
    month: "long",
    ...(d.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  });
}

export interface NoteGroup {
  label: string;
  notes: NoteSummary[];
}

/** Consecutive runs of the (already date-sorted) list under one label. */
export function groupNotesByDate(notes: NoteSummary[], now: Date): NoteGroup[] {
  const groups: NoteGroup[] = [];
  for (const n of notes) {
    // happened_at fallback: pre-feature summaries in the localStorage cache
    const label = dateGroupLabel(n.happened_at ?? n.updated_at, now);
    const last = groups[groups.length - 1];
    if (last && last.label === label) last.notes.push(n);
    else groups.push({ label, notes: [n] });
  }
  return groups;
}

/** A row's date line, shortened by its group: the group already says the
 *  day for Today / Yesterday, and the weekday is enough within a week. */
export function rowDateLabel(iso: string, now: Date): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const time = d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  const days = daysAgo(iso, now) ?? 99;
  if (days === 0 || days === 1) return time;
  if (days > 1 && days < 7) {
    return `${d.toLocaleDateString(undefined, { weekday: "short" })} · ${time}`;
  }
  const date = d.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    ...(d.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  });
  return `${date} · ${time}`;
}

/* ------------------------------------------------------------- status chips */

export type ChipTone = "neutral" | "primary" | "success" | "warning" | "danger" | "live";

export interface NoteStatus {
  label: string;
  tone: ChipTone;
}

/** Live state the list can't read from the summary row. */
export interface LiveNoteState {
  /** Note currently being recorded. */
  recordingNoteId: string | null;
  /** Meetings with a transcription in progress (from pipeline events). */
  runningMeetingIds: ReadonlySet<string>;
  /** Meetings waiting in the transcription queue (stage "waiting"). */
  queuedMeetingIds: ReadonlySet<string>;
  /** Meetings whose last transcription failed this session. */
  failedMeetingIds: ReadonlySet<string>;
}

const CHIP = {
  recording: { label: "Recording", tone: "live" },
  transcribing: { label: "Transcribing", tone: "warning" },
  queued: { label: "Queued", tone: "neutral" },
  failed: { label: "Failed", tone: "danger" },
  enhanced: { label: "Enhanced", tone: "success" },
  ready: { label: "Ready to enhance", tone: "primary" },
  notTranscribed: { label: "Not transcribed", tone: "neutral" },
  notesOnly: { label: "Notes only", tone: "neutral" },
} as const satisfies Record<string, NoteStatus>;

/** What a `pipeline:progress` event says about its meeting: the scheduler
 *  emits stage "waiting" for a queued job, so not every non-final event is
 *  a running transcription. */
export function runStateOf(p: PipelineProgress): "running" | "queued" | null {
  if (p.done || p.stage === "cancelled") return null;
  return p.stage === "waiting" ? "queued" : "running";
}

/** The chip for one row. Live app state (this session's recording and
 *  pipeline events) beats the stored row, which was read at the last list
 *  refresh. Then the stored job: queued ("Queued": waiting, or requeued
 *  after a crash), running, failed. Then what the note has: an enhanced doc,
 *  a transcript, or neither ("Not transcribed", e.g. a staged import or
 *  auto-transcribe off). A summary that doesn't say (cached before these
 *  fields existed) shows no chip. */
export function noteStatus(n: NoteSummary, live: LiveNoteState): NoteStatus | null {
  if (live.recordingNoteId === n.id) return CHIP.recording;
  const m = n.meeting_id;
  if (m && live.runningMeetingIds.has(m)) return CHIP.transcribing;
  if (m && live.queuedMeetingIds.has(m)) return CHIP.queued;
  if (m && live.failedMeetingIds.has(m)) return CHIP.failed;
  if (m && n.job_status === "running") return CHIP.transcribing;
  if (m && n.job_status === "queued") return CHIP.queued;
  if (m && n.job_status === "failed") return CHIP.failed;
  if (n.has_enhanced_doc) return CHIP.enhanced;
  if (n.has_transcript) return CHIP.ready;
  if (!m) return CHIP.notesOnly;
  if (n.has_transcript === false) return CHIP.notTranscribed;
  return null;
}

/* ------------------------------------------------------------- up next */

/** Up next lists video meetings only (a join link); link-less events still
 *  reach App so a plain Record press can match them (calendarMatch.ts). */
export function videoEvents(events: CalendarEvent[]): CalendarEvent[] {
  return events.filter((ev) => ev.join_url != null && ev.join_url !== "");
}

/** The meeting the Up next card offers: in progress first, else the next to
 *  start. Ended meetings are skipped. */
export function nextVideoEvent(events: CalendarEvent[], nowMs: number): CalendarEvent | null {
  const live = videoEvents(events)
    .filter((ev) => new Date(ev.end).getTime() > nowMs)
    .sort((a, b) => new Date(a.start).getTime() - new Date(b.start).getTime());
  return live[0] ?? null;
}

/** The call app, from the join link's host. */
export function meetingApp(joinUrl: string | null): string | null {
  if (!joinUrl) return null;
  let host: string;
  try {
    host = new URL(joinUrl).hostname.toLowerCase();
  } catch {
    return null;
  }
  const is = (domain: string) => host === domain || host.endsWith(`.${domain}`);
  if (is("zoom.us") || is("zoom.com")) return "Zoom";
  if (is("meet.google.com")) return "Google Meet";
  if (is("teams.microsoft.com") || is("teams.live.com")) return "Teams";
  if (is("webex.com")) return "Webex";
  return null;
}

/** "Now · Zoom", "In 4 min · Teams", "At 2:30 PM". */
export function upNextWhen(ev: CalendarEvent, nowMs: number): string {
  const start = new Date(ev.start).getTime();
  const app = meetingApp(ev.join_url);
  let when: string;
  if (start <= nowMs) when = "Now";
  else if (start - nowMs < 60 * 60_000) {
    when = `In ${Math.max(1, Math.round((start - nowMs) / 60_000))} min`;
  } else {
    when = `At ${new Date(start).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}`;
  }
  return app ? `${when} · ${app}` : when;
}

/** Who's invited, without you or anyone who declined. */
export function eventGuests(ev: CalendarEvent): string[] {
  return ev.attendees
    .filter((a) => !a.is_self && !a.declined)
    .map((a) => a.name?.trim() || a.email);
}

export type UpNextState = "event" | "empty" | "expired" | "no-calendar";

/** What the Up next card says. `connected` is null until the calendar
 *  status loads: show the neutral "empty" copy rather than a wrong prompt.
 *  An expired sign-in sends no events, so "nothing today" would be a guess:
 *  it gets its own state (the Reconnect banner sits above the card). */
export function upNextState(
  connected: boolean | null,
  needsReconnect: readonly string[],
  next: CalendarEvent | null,
): UpNextState {
  if (next) return "event";
  if (needsReconnect.length > 0) return "expired";
  if (connected === false) return "no-calendar";
  return "empty";
}

export const CALENDAR_NAMES: Record<string, string> = {
  google: "Google Calendar",
  msgraph: "Outlook",
};

/* ------------------------------------------------------------- greeting */

export function greeting(now: Date): string {
  const h = now.getHours();
  if (h < 5) return "Good evening";
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}

/* ------------------------------------------------------------- privacy pill */

function urlHost(url: string): string | null {
  try {
    return new URL(url).host || null;
  } catch {
    return null;
  }
}

/** Mirrors llm_commands.rs `is_loopback_url`: localhost or a loopback IP
 *  (127.0.0.0/8 or ::1). URL parsing normalizes IPv4 hosts to dotted quads
 *  ("127.1" → "127.0.0.1"), so a name like 127.example.com isn't one. */
export function isLoopbackUrl(url: string): boolean {
  let host: string;
  try {
    host = new URL(url).hostname.toLowerCase();
  } catch {
    return false;
  }
  host = host.replace(/^\[|\]$/g, "");
  return host === "localhost" || /^127(\.\d{1,3}){3}$/.test(host) || host === "::1";
}

export interface PrivacyUse {
  /** Who receives it ("Anthropic", "Groq", a host). */
  where: string;
  /** What goes there. */
  what: string[];
}

export interface PrivacySummary {
  /** Anything leaves this computer. */
  offDevice: boolean;
  label: string;
  uses: PrivacyUse[];
  /** Per-note / per-folder Keep local still wins over every row above. */
  footnote: string;
}

/** AI features that go to the configured LLM provider. */
export const AI_USES = ["Ask", "Enhance", "Transcript cleanup", "Action items"];
/** Semantic search's embeddings (always the Ollama server). */
export const SEARCH_USE = "Search index";

/** The sidebar pill: where meeting content goes, from the SAVED settings
 *  (never a Settings draft). Every provider counts, not only Anthropic:
 *  OpenAI, NVIDIA NIM and custom OpenAI-compatible endpoints are named by
 *  vendor or host, and an Ollama server on another machine is off-device
 *  too (build_provider treats only a loopback Ollama as on-device). Null
 *  until both settings have loaded, so it never claims "local" early. */
export function privacySummary(
  asr: { tier: string; use_groq: boolean } | null,
  llm: LlmSettings | null,
): PrivacySummary | null {
  if (!asr || !llm) return null;
  const uses: PrivacyUse[] = [];
  let cloud = false;

  const p = llm.providers.find((x) => x.id === llm.provider) ?? null;
  if (p) {
    const custom = p.base_url ? urlHost(p.base_url) : null;
    if (p.is_local) {
      if (p.base_url && !isLoopbackUrl(p.base_url)) {
        uses.push({ where: custom ?? p.base_url, what: AI_USES });
      }
    } else {
      cloud = true;
      uses.push({ where: custom ?? providerVendor(p.id), what: AI_USES });
    }
  } else if (llm.provider && llm.provider !== "ollama") {
    // A provider this build doesn't list: assume it's off-device.
    cloud = true;
    uses.push({ where: providerVendor(llm.provider), what: AI_USES });
  }

  if (asr.tier === "cloud" || asr.use_groq) {
    cloud = true;
    uses.push({ where: "Groq", what: ["Transcription (audio)"] });
  }

  // Semantic search embeds note text through the Ollama server whatever
  // the AI provider is (embeddings.rs → ollama::root_url).
  const ollama = llm.providers.find((x) => x.id === "ollama");
  if (ollama?.base_url && !isLoopbackUrl(ollama.base_url)) {
    const where = urlHost(ollama.base_url) ?? ollama.base_url;
    const same = uses.find((u) => u.where === where);
    if (same) same.what = [...same.what, SEARCH_USE];
    else uses.push({ where, what: [SEARCH_USE] });
  }

  const offDevice = uses.length > 0;
  const names = uses.map((u) => u.where).join(", ");
  return {
    offDevice,
    label: !offDevice
      ? "On this computer"
      : cloud
        ? `Cloud on · ${names}`
        : `Off this computer · ${names}`,
    uses,
    footnote: offDevice
      ? "Except notes you keep local."
      : "Recordings, transcripts and AI notes stay on this computer.",
  };
}
