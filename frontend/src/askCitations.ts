/* Ask citations: how a validated answer is laid out (paragraphs and bullets
   of sentences), which chips a sentence gets, and whether a cited line is
   still where Ask read it. Pure, so it's unit-tested (askCitations.test.ts).

   An Ask answer isn't stored, so it has no citation locator rows (schema
   v3 records those for note blocks and items). Each cited line carries the
   same locator fields, though: meeting, segment id, start/end and its text.
   `locateLine` applies the storage layer's rule to them (fly-storage
   citations.rs): the same id saying the same thing is fine; a missing id is
   re-found by time overlap plus matching words ("reanchored" when the words
   and speaker are the same, otherwise "changed — check"). */

import type { AskSentence, AskSource, AskSourceLine, Transcript } from "./types";

export interface AnswerBlock {
  /** A bullet item (the harness starts each with "- "). */
  bullet: boolean;
  sentences: { index: number; text: string }[];
}

/** Sentences → paragraphs and bullets, as the harness joined them: a
 *  sentence starting "- " begins a bullet, the rest continue the block. */
export function layoutSentences(sentences: AskSentence[]): AnswerBlock[] {
  const blocks: AnswerBlock[] = [];
  sentences.forEach((s, index) => {
    const bullet = s.text.startsWith("- ");
    const text = bullet ? s.text.slice(2) : s.text;
    const last = blocks[blocks.length - 1];
    if (!last || bullet) {
      blocks.push({ bullet, sentences: [{ index, text }] });
    } else {
      last.sentences.push({ index, text });
    }
  });
  return blocks;
}

export interface CiteGroup {
  meetingId: string;
  meetingTitle: string;
  lines: AskSourceLine[];
}

/** One chip per meeting a sentence cites, in citation order. Ids missing
 *  from `sources` (shouldn't happen: the harness lists every cited line)
 *  are left out rather than shown as a chip that opens nothing. */
export function citeGroups(sentence: AskSentence, sources: AskSource[]): CiteGroup[] {
  const groups: CiteGroup[] = [];
  for (const id of sentence.citations) {
    for (const src of sources) {
      const line = src.lines.find((l) => l.segment_id === id);
      if (!line) continue;
      let g = groups.find((x) => x.meetingId === src.meeting_id);
      if (!g) {
        g = { meetingId: src.meeting_id, meetingTitle: src.meeting_title, lines: [] };
        groups.push(g);
      }
      if (!g.lines.includes(line)) g.lines.push(line);
      break;
    }
  }
  return groups;
}

/** Every segment id an answer cites, first-cited first (for Insert). */
export function citedIds(sentences: AskSentence[]): string[] {
  return [...new Set(sentences.flatMap((s) => s.citations))];
}

/** "1:05" / "1:02:05" for a transcript time. */
export function clock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

export type CiteStatus = "ok" | "reanchored" | "changed";

export interface LocatedLine {
  status: CiteStatus;
  /** Where the line is now (its own id, the re-found one, or the nearest
   *  match for "changed"); null when the transcript has nothing near it. */
  segmentId: string | null;
}

/** Same padding and threshold as fly-storage's re-anchoring. */
const PAD_MS = 2_000;
const MIN_SCORE = 0.6;

const words = (s: string) =>
  s
    .toLocaleLowerCase()
    .replace(/…|\.\.\.$/g, " ")
    .split(/[^\p{L}\p{N}]+/u)
    .filter(Boolean);

/** Share of the snippet's words the segment says (the snippet may be
 *  clipped, so this is containment, not equality). */
function coverage(snippet: string, text: string): number {
  const want = words(snippet);
  if (want.length === 0) return 0;
  const have = new Set(words(text));
  return want.filter((w) => have.has(w)).length / want.length;
}

/** Whether a cited line still says what Ask read, and where it is now. */
export function locateLine(line: AskSourceLine, transcript: Transcript): LocatedLine {
  const speakerOf = (key: string) => transcript.speakers.find((s) => s.key === key)?.label ?? key;
  const same = transcript.segments.find((s) => s.id === line.segment_id);
  if (same) {
    return {
      status: coverage(line.snippet, same.text) >= 0.999 ? "ok" : "changed",
      segmentId: same.id,
    };
  }
  const lo = line.start_ms - PAD_MS;
  const hi = line.end_ms + PAD_MS;
  let best: { id: string; score: number; speaker: string } | null = null;
  for (const s of transcript.segments) {
    if (s.end_ms < lo || s.start_ms > hi) continue;
    const score = coverage(line.snippet, s.text);
    if (!best || score > best.score) best = { id: s.id, score, speaker: speakerOf(s.speaker_key) };
  }
  if (best && best.score >= MIN_SCORE) {
    const reanchored = best.score >= 0.999 && best.speaker === line.speaker;
    return { status: reanchored ? "reanchored" : "changed", segmentId: best.id };
  }
  return { status: "changed", segmentId: best?.id ?? null };
}

/** The worst status of a chip's lines (one changed line marks the chip). */
export function worstStatus(statuses: CiteStatus[]): CiteStatus {
  if (statuses.includes("changed")) return "changed";
  if (statuses.includes("reanchored")) return "reanchored";
  return "ok";
}

/** What a chip says about its lines when they moved or changed. */
export const CITE_STATUS_COPY: Record<Exclude<CiteStatus, "ok">, string> = {
  reanchored: "Moved since Ask read it. It opens where the line is now.",
  changed: "Source changed since Ask read it. Check it in the transcript.",
};
