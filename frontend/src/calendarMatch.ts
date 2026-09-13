import type { CalendarEvent, MeetingSeed } from "./types";

/** How early a meeting counts as "in progress" for a Record press. */
const LEAD_MS = 5 * 60_000;

/**
 * The calendar event a Record press most likely belongs to: one whose window
 * (start − 5 min … end) contains `now`. When several overlap, the one that
 * started most recently wins. Null when nothing is in progress.
 */
export function pickInProgressEvent(events: CalendarEvent[], now: number): CalendarEvent | null {
  let best: CalendarEvent | null = null;
  for (const ev of events) {
    const start = new Date(ev.start).getTime();
    const end = new Date(ev.end).getTime();
    if (Number.isNaN(start) || Number.isNaN(end)) continue;
    if (
      start - LEAD_MS <= now &&
      now <= end &&
      (best == null || start > new Date(best.start).getTime())
    ) {
      best = ev;
    }
  }
  return best;
}

/** Seed for `start_recording`: title only for a brand-new note. */
export function seedFromEvent(ev: CalendarEvent, forNewNote: boolean): MeetingSeed {
  return { title: forNewNote ? ev.title : null, attendees: ev.attendees };
}
