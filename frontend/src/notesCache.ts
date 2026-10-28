// Persisted last-known notes list (the "all" view) so the list paints
// immediately on launch and reconciles when fresh data lands. Only durable
// note summaries live here — transient state (recording, queue/transcription
// progress) always renders from live status polls and events, never from
// this snapshot.
import type { NoteSummary } from "./types";

const KEY = "flyonthewall.notes-cache.v1";

/** The transcription job's state is transient (queue progress): it never
 *  goes into, or comes back from, the snapshot. */
function durable({ job_status: _transient, ...rest }: NoteSummary): NoteSummary {
  void _transient;
  return rest;
}

export function readNotesCache(): NoteSummary[] {
  try {
    const raw = localStorage.getItem(KEY);
    return raw ? (JSON.parse(raw) as NoteSummary[]).map(durable) : [];
  } catch {
    return [];
  }
}

export function writeNotesCache(notes: NoteSummary[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify(notes.map(durable)));
  } catch {
    // best-effort (quota, disabled storage) — worst case: no instant paint
  }
}
