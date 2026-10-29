import { describe, expect, it } from "vitest";
import {
  citedIds,
  citeGroups,
  clock,
  layoutSentences,
  locateLine,
  worstStatus,
} from "./askCitations";
import type { AskSentence, AskSource, Transcript } from "./types";

const s = (text: string, citations: string[] = []): AskSentence => ({
  text,
  citations,
  status: citations.length ? "sourced" : "unsourced",
});

describe("layoutSentences", () => {
  it("joins sentences into paragraphs and bullets, as the harness did", () => {
    const blocks = layoutSentences([
      s("Here's what stands out."),
      s("Two things."),
      s("- Pricing is the blocker."),
      s("It came up twice."),
      s("- Legal needs wording."),
    ]);
    expect(blocks).toEqual([
      {
        bullet: false,
        sentences: [
          { index: 0, text: "Here's what stands out." },
          { index: 1, text: "Two things." },
        ],
      },
      {
        bullet: true,
        sentences: [
          { index: 2, text: "Pricing is the blocker." },
          { index: 3, text: "It came up twice." },
        ],
      },
      { bullet: true, sentences: [{ index: 4, text: "Legal needs wording." }] },
    ]);
  });
});

const SOURCES: AskSource[] = [
  {
    meeting_id: "m1",
    note_id: "n1",
    meeting_title: "Renewal review",
    started_at: null,
    label: "",
    lines: [
      { segment_id: "a", speaker: "Dana", start_ms: 0, end_ms: 1, snippet: "x" },
      { segment_id: "b", speaker: "You", start_ms: 2, end_ms: 3, snippet: "y" },
    ],
  },
  {
    meeting_id: "m2",
    note_id: "n2",
    meeting_title: "Weekly 1:1",
    started_at: null,
    label: "",
    lines: [{ segment_id: "c", speaker: "Dana", start_ms: 0, end_ms: 1, snippet: "z" }],
  },
];

describe("citeGroups", () => {
  it("one chip per cited meeting, in citation order; unknown ids are left out", () => {
    const groups = citeGroups(s("x", ["c", "a", "b", "nope"]), SOURCES);
    expect(groups.map((g) => [g.meetingId, g.lines.map((l) => l.segment_id)])).toEqual([
      ["m2", ["c"]],
      ["m1", ["a", "b"]],
    ]);
  });

  it("collects every cited id once, first-cited first", () => {
    expect(citedIds([s("1", ["b", "a"]), s("2", ["a", "c"])])).toEqual(["b", "a", "c"]);
  });
});

const seg = (id: string, speaker_key: string, start_ms: number, text: string) => ({
  id,
  speaker_key,
  start_ms,
  end_ms: start_ms + 4000,
  text,
  words: [],
});
const T: Transcript = {
  meeting_id: "m1",
  language: "en",
  engine: "t",
  speakers: [
    { key: "s1", label: "Dana" },
    { key: "s2", label: "Marc" },
  ],
  segments: [
    seg("n1", "s1", 10_000, "We will ship on Friday."),
    seg("n2", "s2", 20_000, "We will NOT ship on Friday, sorry."),
    seg("n3", "s1", 40_000, "Legal needs the wording."),
  ],
};
const line = (segment_id: string, start_ms: number, snippet: string, speaker = "Dana") => ({
  segment_id,
  speaker,
  start_ms,
  end_ms: start_ms + 4000,
  snippet,
});

describe("locateLine", () => {
  it("the same id saying the same thing is fine", () => {
    expect(locateLine(line("n1", 10_000, "We will ship on Friday."), T)).toEqual({
      status: "ok",
      segmentId: "n1",
    });
    // a clipped snippet still counts
    expect(locateLine(line("n3", 40_000, "Legal needs…"), T).status).toBe("ok");
  });

  it("the same id saying something else (an edit) is changed", () => {
    expect(locateLine(line("n3", 40_000, "Legal signed off."), T)).toEqual({
      status: "changed",
      segmentId: "n3",
    });
  });

  it("a missing id is re-found by time and words: reanchored when the same, else changed", () => {
    expect(locateLine(line("old1", 10_500, "We will ship on Friday."), T)).toEqual({
      status: "reanchored",
      segmentId: "n1",
    });
    // same words, but now someone else says them
    expect(locateLine(line("old1", 10_500, "We will ship on Friday.", "Marc"), T).status).toBe(
      "changed",
    );
    // nothing near it says those words: changed, pointing at the nearest
    expect(locateLine(line("old2", 21_000, "Completely different words here."), T)).toEqual({
      status: "changed",
      segmentId: "n2",
    });
    // nothing anywhere near it
    expect(locateLine(line("old3", 90_000, "x"), T)).toEqual({
      status: "changed",
      segmentId: null,
    });
  });

  it("a chip takes its worst line", () => {
    expect(worstStatus(["ok", "reanchored"])).toBe("reanchored");
    expect(worstStatus(["reanchored", "changed", "ok"])).toBe("changed");
    expect(worstStatus([])).toBe("ok");
  });
});

describe("clock", () => {
  it("formats transcript times", () => {
    expect(clock(65_000)).toBe("1:05");
    expect(clock(3_725_000)).toBe("1:02:05");
    expect(clock(-5)).toBe("0:00");
  });
});
