import { describe, expect, it } from "vitest";
import {
  EARLY_COPY,
  blockTrust,
  enhancedBeforeTranscript,
  sourceQuotes,
  sourceTarget,
  templateSections,
  trustLine,
  trustSubline,
  trustSummary,
  type CitedTranscript,
} from "./enhancedTrust";
import type { NoteBlock, Transcript } from "./types";

const user = (id: string): NoteBlock => ({ id, origin: { kind: "user" }, markdown: id });
const ai = (id: string, src: string[]): NoteBlock => ({
  id,
  origin: { kind: "ai", source_segment_ids: src },
  markdown: id,
});

const raw: Transcript = {
  meeting_id: "m1",
  language: "en",
  engine: "whisper",
  speakers: [
    { key: "mic", label: "You" },
    { key: "s1", label: "Dana" },
  ],
  segments: [
    {
      id: "t1",
      speaker_key: "s1",
      start_ms: 4000,
      end_ms: 6000,
      text: " um, the price ",
      words: [],
    },
    { id: "t2", speaker_key: "mic", start_ms: 9000, end_ms: 9500, text: "ok", words: [] },
  ],
};
const ready: CitedTranscript = { state: "ready", transcripts: [raw], complete: true };
const none: CitedTranscript = { state: "none" };
const checking: CitedTranscript = { state: "checking" };

describe("block trust", () => {
  it("with a transcript: sourced, no source, or source not found", () => {
    expect(blockTrust(user("a").origin, ready)).toBe("user");
    expect(blockTrust(ai("b", ["t1"]).origin, ready)).toBe("sourced");
    expect(blockTrust(ai("c", []).origin, ready)).toBe("unsourced");
    // every cited id gone (re-transcribed): not sourced any more
    expect(blockTrust(ai("d", ["old-1", "old-2"]).origin, ready)).toBe("missing");
    // one id still resolves: still sourced
    expect(blockTrust(ai("e", ["old-1", "t2"]).origin, ready)).toBe("sourced");
  });

  it("with no transcript, AI text was written from your notes", () => {
    expect(blockTrust(ai("a", []).origin, none)).toBe("notes");
    expect(blockTrust(user("b").origin, none)).toBe("user");
  });

  it("never downgrades while the transcript is still loading", () => {
    expect(blockTrust(ai("a", ["old-1"]).origin, checking)).toBe("sourced");
    expect(blockTrust(ai("b", []).origin, checking)).toBe("pending");
  });

  it("counts AI lines by kind", () => {
    const blocks = [user("a"), ai("b", ["t1"]), ai("c", []), ai("d", ["gone"])];
    expect(trustSummary(blocks, ready)).toEqual({
      ai: 3,
      sourced: 1,
      unsourced: 1,
      missing: 1,
      notes: 0,
      early: false,
    });
  });
});

describe("legend copy", () => {
  const blocks = [user("a"), ai("b", ["t1"]), ai("c", []), ai("d", ["gone"])];

  it("says 'without a source' and 'source not found', never that a line was made up", () => {
    const s = trustSummary(blocks, ready);
    expect(trustLine(s, ready)).toBe("3 AI lines · 1 without a source · 1 with a source not found");
    expect(trustLine(trustSummary([ai("b", ["t1"])], ready), ready)).toBe(
      "1 AI line · all sourced",
    );
    expect(trustLine(trustSummary([user("a")], ready), ready)).toBe("Everything here is yours");
    for (const c of [ready, none, checking]) {
      expect(trustLine(trustSummary(blocks, c), c)).not.toMatch(/made up|invent|halluc|fake/i);
    }
  });

  it("with no transcript, talks about your notes, not a transcript", () => {
    const s = trustSummary([user("a"), ai("b", []), ai("c", [])], none);
    expect(trustLine(s, none)).toBe("2 AI lines · written from your notes");
    expect(trustSubline(s, none)).toContain("no transcript");
    expect(trustSubline(s, none)).not.toContain("Hover a source");
  });

  it("only offers the quote hover when something is sourced", () => {
    expect(trustSubline(trustSummary(blocks, ready), ready)).toContain("Hover a source");
    expect(trustSubline(trustSummary([ai("c", [])], ready), ready)).not.toContain("Hover a source");
  });

  it("claims nothing while checking", () => {
    expect(trustLine(trustSummary(blocks, checking), checking)).toBe("3 AI lines");
  });

  it("doesn't call cited lines 'from your notes' when there's no transcript here", () => {
    // e.g. a re-recorded note: its lines cite the earlier recording
    const mixed = [ai("b", ["t1"]), ai("c", []), ai("d", [])];
    const s = trustSummary(mixed, none);
    expect(trustLine(s, none)).toBe("3 AI lines · 2 written from your notes");
    expect(trustSubline(s, none)).toBe(
      "There's no transcript here to check their sources against.",
    );
  });
});

describe("templateSections", () => {
  it("takes the ## sections and drops format hints", () => {
    expect(
      templateSections("## Summary\n## Key points\n## Decisions\n## Action items (owner — due)"),
    ).toEqual(["Summary", "Key points", "Decisions", "Action items"]);
  });

  it("skips deeper headings, bullets and placeholders", () => {
    expect(
      templateSections("## Per person\n### <name>\n- Did\n- Next\n## Cross-team blockers"),
    ).toEqual(["Per person", "Cross-team blockers"]);
  });

  it("handles CRLF, duplicates and an empty hint", () => {
    expect(templateSections("## A\r\n## A\r\n##B\r\n## C ##")).toEqual(["A", "C"]);
    expect(templateSections("")).toEqual([]);
  });
});

describe("sourceQuotes", () => {
  it("quotes the raw text Enhance read and counts missing ids", () => {
    const { found, missing } = sourceQuotes(["t1", "t2", "gone"], [raw]);
    expect(found.map((q) => [q.id, q.speaker, q.text, q.speakerIndex])).toEqual([
      ["t1", "Dana", "um, the price", 1],
      ["t2", "You", "ok", 0],
    ]);
    expect(missing).toBe(1);
  });

  it("reports everything missing when there is no transcript", () => {
    expect(sourceQuotes(["t1"], [])).toEqual({ found: [], missing: 1 });
  });
});

describe("a note recorded into more than once", () => {
  // m1 is the recording Enhance read; m2 was recorded into the note later.
  const m2: Transcript = {
    ...raw,
    meeting_id: "m2",
    segments: [{ id: "u1", speaker_key: "s1", start_ms: 0, end_ms: 900, text: "new", words: [] }],
  };
  const both: CitedTranscript = { state: "ready", transcripts: [m2, raw], complete: true };

  it("a source in any of its recordings counts", () => {
    expect(blockTrust(ai("b", ["t1"]).origin, both)).toBe("sourced");
    expect(blockTrust(ai("c", ["u1"]).origin, both)).toBe("sourced");
    expect(blockTrust(ai("d", ["gone"]).origin, both)).toBe("missing");
  });

  it("a recording with no transcript yet never makes a line 'not found'", () => {
    const partial: CitedTranscript = { state: "ready", transcripts: [raw], complete: false };
    expect(blockTrust(ai("b", ["t1"]).origin, partial)).toBe("sourced");
    expect(blockTrust(ai("d", ["gone"]).origin, partial)).toBe("sourced");
    expect(trustSummary([ai("d", ["gone"])], partial).missing).toBe(0);
  });

  it("quotes and zooms into the recording that holds the lines", () => {
    expect(sourceQuotes(["t1", "u1"], [m2, raw]).found.map((q) => q.text)).toEqual([
      "um, the price",
      "new",
    ]);
    expect(sourceTarget(["gone", "t1", "t2"], [m2, raw])).toEqual({
      meetingId: "m1",
      ids: ["t1", "t2", "gone"],
    });
    expect(sourceTarget(["gone"], [m2, raw])).toBeNull();
  });
});

describe("enhanced before the transcript was ready", () => {
  const blocks = [user("a"), ai("b", []), ai("c", [])];

  it("is when a transcript exists but no AI line has a source", () => {
    expect(enhancedBeforeTranscript(blocks, ready)).toBe(true);
    expect(enhancedBeforeTranscript([...blocks, ai("d", ["t1"])], ready)).toBe(false);
    expect(enhancedBeforeTranscript(blocks, none)).toBe(false);
    expect(enhancedBeforeTranscript(blocks, checking)).toBe(false);
    expect(enhancedBeforeTranscript([user("a")], ready)).toBe(false);
  });

  it("says so once in the legend instead of flagging every line", () => {
    const s = trustSummary(blocks, ready);
    expect(s).toMatchObject({ ai: 2, unsourced: 0, notes: 2, early: true });
    expect(blockTrust(ai("b", []).origin, ready, true)).toBe("notes");
    expect(trustLine(s, ready)).toBe("2 AI lines · no sources yet");
    expect(trustSubline(s, ready)).toBe(EARLY_COPY);
    expect(EARLY_COPY).toBe(
      "Enhanced before the transcript was ready. Enhance again to link sources.",
    );
  });
});
