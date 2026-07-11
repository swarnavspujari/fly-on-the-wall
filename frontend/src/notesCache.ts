// Persisted last-known notes list (the "all" view) so the list paints
// immediately on launch and reconciles when fresh data lands. Only durable
// note summaries live here — transient state (recording, queue/transcription
// progress) always renders from live status polls and events, never from
// this snapshot.
import type { NoteSummary } from "./types";

const KEY = "flyonthewall.notes-cache.v1";

export function readNotesCache(): NoteSummary[] {
  try {
    const raw = localStorage.getItem(KEY);
    return raw ? (JSON.parse(raw) as NoteSummary[]) : [];
  } catch {
    return [];
  }
}

export function writeNotesCache(notes: NoteSummary[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify(notes));
  } catch {
    // best-effort (quota, disabled storage) — worst case: no instant paint
  }
}
