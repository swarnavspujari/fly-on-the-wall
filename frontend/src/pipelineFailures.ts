/** Pure state for transcription failures held in the app notice stack: a
 *  pipeline run that failed while its note wasn't open. Kept free of React
 *  so vitest covers every transition (App.tsx dispatches into it). */

import type { NoteSummary, PipelineProgress } from "./types";

export interface PipelineFailure {
  meetingId: string;
  /** The failed meeting's note, when the list knows it. */
  noteId: string | null;
  noteTitle: string | null;
  error: string;
  /** The note already had a transcript: this was a re-run (re-transcribe or
   *  re-analyze speakers), so "Try again" can't know which. null = unknown. */
  hadTranscript: boolean | null;
}

export type FailureAction =
  /** Any `pipeline:progress` event. */
  | { type: "event"; event: PipelineProgress; openMeetingId: string | null }
  | { type: "add"; failure: PipelineFailure }
  /** The user started a retry from the notice. */
  | { type: "retry"; meetingId: string }
  | { type: "dismiss"; meetingId: string }
  /** The note was deleted (its Undo window expired). */
  | { type: "noteDeleted"; noteId: string };

/** A failure the stack should hold: a terminal error for a meeting whose
 *  note isn't open (the open note shows its own inline, actionable notice). */
export function isBackgroundFailure(p: PipelineProgress, openMeetingId: string | null): boolean {
  return p.done && !!p.error && p.stage !== "cancelled" && p.meeting_id !== openMeetingId;
}

const without = (list: PipelineFailure[], meetingId: string) =>
  list.some((f) => f.meetingId === meetingId)
    ? list.filter((f) => f.meetingId !== meetingId)
    : list;

/** A held failure stays until a retry starts, the run succeeds or is
 *  cancelled, or the user dismisses it. Opening its note doesn't clear it
 *  (the note's own view may not show the fix), and neither does opening
 *  another note. Unchanged state returns the same array (no re-render). */
export function failuresReducer(list: PipelineFailure[], action: FailureAction): PipelineFailure[] {
  switch (action.type) {
    case "event": {
      const p = action.event;
      // The background failure itself is added after its note is looked up
      // ("add"); every other event for the meeting (a retry's progress, a
      // success, a cancel, a newer failure the open note shows inline)
      // supersedes the held one.
      if (isBackgroundFailure(p, action.openMeetingId)) return list;
      return without(list, p.meeting_id);
    }
    case "add":
      return [...without(list, action.failure.meetingId), action.failure];
    case "retry":
    case "dismiss":
      return without(list, action.meetingId);
    case "noteDeleted":
      return list.some((f) => f.noteId === action.noteId)
        ? list.filter((f) => f.noteId !== action.noteId)
        : list;
  }
}

/** The held failure for a background error event, named from its note. */
export function failureFrom(p: PipelineProgress, owner: NoteSummary | undefined): PipelineFailure {
  return {
    meetingId: p.meeting_id,
    noteId: owner?.id ?? null,
    noteTitle: owner?.title ?? null,
    error: p.error ?? "",
    hadTranscript: owner?.has_transcript ?? null,
  };
}

/** After an async note lookup: add the failure only if nothing newer
 *  happened to that meeting meanwhile (no later event, its note not opened). */
export function shouldAddFailure(
  meetingId: string,
  seqAtEvent: number,
  seqNow: number,
  openMeetingId: string | null,
): boolean {
  return seqAtEvent === seqNow && openMeetingId !== meetingId;
}
