import { describe, expect, it } from "vitest";
import {
  blockTrust,
  sourceQuotes,
  templateSections,
  trustLine,
  trustSummary,
} from "./enhancedTrust";
import type { NoteBlock, Transcript } from "./types";

const user = (id: string): NoteBlock => ({ id, origin: { kind: "user" }, markdown: id });
const ai = (id: string, src: string[]): NoteBlock => ({
  id,
  origin: { kind: "ai", source_segment_ids: src },
  markdown: id,
});

describe("block trust", () => {
  it("splits blocks into yours, sourced and unsourced", () => {
    expect(blockTrust(user("a").origin)).toBe("user");
    expect(blockTrust(ai("b", ["t1"]).origin)).toBe("sourced");
    expect(blockTrust(ai("c", []).origin)).toBe("unsourced");
  });

  it("counts AI lines and the ones without a source", () => {
    const blocks = [user("a"), ai("b", ["t1"]), ai("c", []), ai("d", ["t2", "t3"])];
    expect(trustSummary(blocks)).toEqual({ ai: 3, unsourced: 1 });
  });

  it("says 'without a source', never that a line was made up", () => {
    expect(trustLine({ ai: 3, unsourced: 1 })).toBe("3 AI lines · 1 without a source");
    expect(trustLine({ ai: 1, unsourced: 0 })).toBe("1 AI line · all sourced");
    expect(trustLine({ ai: 0, unsourced: 0 })).toBe("Everything here is yours");
    for (const s of [trustLine({ ai: 2, unsourced: 2 }), trustLine({ ai: 5, unsourced: 0 })]) {
      expect(s).not.toMatch(/made up|invent|halluc|fake/i);
    }
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
  const cleaned: Transcript = {
    ...raw,
    segments: [{ ...raw.segments[0], text: "The price." }],
  };

  it("prefers the cleaned text, falls back to raw, and counts missing ids", () => {
    const { found, missing } = sourceQuotes(["t1", "t2", "gone"], [cleaned, raw]);
    expect(found.map((q) => [q.id, q.speaker, q.text, q.speakerIndex])).toEqual([
      ["t1", "Dana", "The price.", 1],
      ["t2", "You", "ok", 0],
    ]);
    expect(missing).toBe(1);
  });

  it("reports everything missing when there is no transcript", () => {
    expect(sourceQuotes(["t1"], [null, null])).toEqual({ found: [], missing: 1 });
  });
});
