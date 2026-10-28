/* Enhanced-notes trust signals: which blocks are yours, which AI lines link
   to the transcript, and which have no source. Pure, so the copy and the
   counting are unit-tested (enhancedTrust.test.ts). */

import type { BlockOrigin, NoteBlock, Transcript } from "./types";

/** "user" = your words (typed, edited or kept). "sourced" = AI text linked
 *  to transcript lines. "unsourced" = AI text with no transcript link. */
export type BlockTrust = "user" | "sourced" | "unsourced";

export function blockTrust(origin: BlockOrigin): BlockTrust {
  if (origin.kind === "user") return "user";
  return origin.source_segment_ids.length > 0 ? "sourced" : "unsourced";
}

export interface TrustSummary {
  /** AI blocks still in the document. */
  ai: number;
  /** AI blocks with no transcript link. */
  unsourced: number;
}

export function trustSummary(blocks: NoteBlock[]): TrustSummary {
  let ai = 0;
  let unsourced = 0;
  for (const b of blocks) {
    const t = blockTrust(b.origin);
    if (t === "user") continue;
    ai += 1;
    if (t === "unsourced") unsourced += 1;
  }
  return { ai, unsourced };
}

/** The legend's one-line summary. "No source" is the honest claim: Enhance
 *  drops a source it can't match to a transcript line, so a line without one
 *  may still be right. Never say it was made up. */
export function trustLine({ ai, unsourced }: TrustSummary): string {
  if (ai === 0) return "Everything here is yours";
  const lines = `${ai} AI line${ai === 1 ? "" : "s"}`;
  if (unsourced === 0) return `${lines} · all sourced`;
  return `${lines} · ${unsourced} without a source`;
}

/** Tooltip for an AI line with no source. */
export const NO_SOURCE_TITLE =
  "Enhance didn't link this line to the transcript. It may have given no source, or one " +
  "that didn't match a transcript line. Check it before you keep it.";

/** The `##` sections of a template's structure hint, the headings Enhance
 *  will add: "## Action items (owner — due)" → "Action items". Deeper
 *  headings ("### <name>") are per-section detail, not sections. */
export function templateSections(structureHint: string): string[] {
  const out: string[] = [];
  for (const line of structureHint.split(/\r?\n/)) {
    const m = /^\s{0,3}##\s+(.+?)\s*#*\s*$/.exec(line);
    if (!m) continue;
    const name = m[1]
      .replace(/<[^>]*>/g, "")
      .replace(/\s*\([^)]*\)\s*$/, "")
      .trim();
    if (name && !out.includes(name)) out.push(name);
  }
  return out;
}

export interface SourceQuote {
  id: string;
  speakerKey: string;
  speaker: string;
  /** Position in the transcript's speaker list (drives the avatar color). */
  speakerIndex: number;
  startMs: number;
  text: string;
}

/** Resolve cited segment ids to quotes. The first transcript that has a
 *  segment wins (pass the AI-cleaned one first: it shares segment ids with
 *  the raw one). `missing` counts ids no transcript has any more, e.g.
 *  after the meeting was re-transcribed. */
export function sourceQuotes(
  ids: string[],
  transcripts: (Transcript | null | undefined)[],
): { found: SourceQuote[]; missing: number } {
  const found: SourceQuote[] = [];
  let missing = 0;
  for (const id of ids) {
    let hit: SourceQuote | null = null;
    for (const t of transcripts) {
      const seg = t?.segments.find((s) => s.id === id);
      if (!t || !seg) continue;
      const idx = t.speakers.findIndex((s) => s.key === seg.speaker_key);
      hit = {
        id,
        speakerKey: seg.speaker_key,
        speaker: t.speakers[idx]?.label ?? seg.speaker_key,
        speakerIndex: Math.max(idx, 0),
        startMs: seg.start_ms,
        text: seg.text.trim(),
      };
      break;
    }
    if (hit) found.push(hit);
    else missing += 1;
  }
  return { found, missing };
}
