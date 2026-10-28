import { describe, expect, it } from "vitest";
import {
  failureFrom,
  failuresReducer,
  isBackgroundFailure,
  shouldAddFailure,
  type PipelineFailure,
} from "./pipelineFailures";
import type { NoteSummary, PipelineProgress } from "./types";

const ev = (meeting_id: string, over: Partial<PipelineProgress> = {}): PipelineProgress => ({
  meeting_id,
  stage: "transcribing",
  detail: null,
  done: false,
  error: null,
  ...over,
});
const failed = (meeting_id: string) =>
  ev(meeting_id, { stage: "error", done: true, error: "boom" });

const held = (meetingId: string, noteId = `n-${meetingId}`): PipelineFailure => ({
  meetingId,
  noteId,
  noteTitle: "Sync",
  error: "boom",
  hadTranscript: false,
});

describe("isBackgroundFailure", () => {
  it("a terminal error for a meeting that isn't open", () => {
    expect(isBackgroundFailure(failed("m1"), null)).toBe(true);
    expect(isBackgroundFailure(failed("m1"), "m2")).toBe(true);
  });

  it("not the open meeting's (its inline notice shows it), not progress, success or cancel", () => {
    expect(isBackgroundFailure(failed("m1"), "m1")).toBe(false);
    expect(isBackgroundFailure(ev("m1"), null)).toBe(false);
    expect(isBackgroundFailure(ev("m1", { stage: "done", done: true }), null)).toBe(false);
    expect(
      isBackgroundFailure(ev("m1", { stage: "cancelled", done: true, error: "x" }), null),
    ).toBe(false);
  });
});

describe("failuresReducer", () => {
  const start = [held("m1"), held("m2")];

  it("add appends, replacing an older failure for the same meeting", () => {
    const once = failuresReducer([], { type: "add", failure: held("m1") });
    const twice = failuresReducer(once, {
      type: "add",
      failure: { ...held("m1"), error: "newer" },
    });
    expect(twice).toEqual([{ ...held("m1"), error: "newer" }]);
  });

  it("a background failure event itself changes nothing (it's added after the lookup)", () => {
    expect(
      failuresReducer(start, { type: "event", event: failed("m1"), openMeetingId: null }),
    ).toBe(start);
  });

  it.each([
    ["a retry's progress", ev("m1")],
    ["success", ev("m1", { stage: "done", done: true })],
    ["cancel", ev("m1", { stage: "cancelled", done: true })],
  ])("%s clears that meeting's failure only", (_what, event) => {
    expect(failuresReducer(start, { type: "event", event, openMeetingId: null })).toEqual([
      held("m2"),
    ]);
  });

  it("a newer failure while its note is open replaces the held one (shown inline)", () => {
    expect(
      failuresReducer(start, { type: "event", event: failed("m1"), openMeetingId: "m1" }),
    ).toEqual([held("m2")]);
  });

  it("opening the note doesn't clear it: only events, retry, dismiss or delete do", () => {
    // There is no "open" action; an event for ANOTHER meeting leaves it alone.
    expect(failuresReducer(start, { type: "event", event: ev("m9"), openMeetingId: "m1" })).toBe(
      start,
    );
  });

  it("retry and dismiss clear one meeting", () => {
    expect(failuresReducer(start, { type: "retry", meetingId: "m1" })).toEqual([held("m2")]);
    expect(failuresReducer(start, { type: "dismiss", meetingId: "m2" })).toEqual([held("m1")]);
  });

  it("deleting a note clears its failures", () => {
    expect(failuresReducer(start, { type: "noteDeleted", noteId: "n-m2" })).toEqual([held("m1")]);
  });

  it("no-op actions return the same array (no re-render)", () => {
    expect(failuresReducer(start, { type: "dismiss", meetingId: "m9" })).toBe(start);
    expect(failuresReducer(start, { type: "noteDeleted", noteId: "nope" })).toBe(start);
    expect(failuresReducer(start, { type: "event", event: ev("m9"), openMeetingId: null })).toBe(
      start,
    );
  });
});

describe("failureFrom", () => {
  const owner: NoteSummary = {
    id: "n1",
    title: "Acme",
    folder_id: null,
    meeting_id: "m1",
    updated_at: "",
    happened_at: "",
    has_transcript: true,
  };

  it("names the note and records whether it already had a transcript", () => {
    expect(failureFrom(failed("m1"), owner)).toEqual({
      meetingId: "m1",
      noteId: "n1",
      noteTitle: "Acme",
      error: "boom",
      hadTranscript: true,
    });
  });

  it("an unknown note, or a cached summary without the field, is null (unknown)", () => {
    expect(failureFrom(failed("m1"), undefined)).toMatchObject({
      noteId: null,
      noteTitle: null,
      hadTranscript: null,
    });
    const legacy: NoteSummary = { ...owner, has_transcript: undefined };
    expect(failureFrom(failed("m1"), legacy).hadTranscript).toBeNull();
  });
});

describe("shouldAddFailure (after an async note lookup)", () => {
  it("adds when nothing happened to the meeting meanwhile", () => {
    expect(shouldAddFailure("m1", 3, 3, null)).toBe(true);
    expect(shouldAddFailure("m1", 3, 3, "m2")).toBe(true);
  });

  it("skips when a newer event arrived (e.g. a retry started)", () => {
    expect(shouldAddFailure("m1", 3, 4, null)).toBe(false);
  });

  it("skips when its note was opened meanwhile (the inline view owns it)", () => {
    expect(shouldAddFailure("m1", 3, 3, "m1")).toBe(false);
  });
});
