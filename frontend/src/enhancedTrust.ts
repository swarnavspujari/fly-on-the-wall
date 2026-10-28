/* Enhanced-notes trust signals: which blocks are yours, which AI lines link
   to the transcript, and which have no source. Pure, so the copy and the
   counting are unit-tested (enhancedTrust.test.ts). */

import type { BlockOrigin, NoteBlock, Transcript } from "./types";

/** The transcript Enhance could cite: the RAW transcript of the note's
 *  meeting (`note.meeting_id`), which is what enhance_note reads. */
export type CitedTranscript =
  /** No meeting, or no transcript yet: Enhance wrote from your notes. */
  | { state: "none" }
  /** Still loading (or it couldn't be read): claim nothing either way. */
  | { state: "checking" }
  | { state: "ready"; transcript: Transcript };

/**
 * - "user": your words (typed, edited or kept).
 * - "sourced": AI text linked to transcript lines (or not checkable yet).
 * - "unsourced": AI text with no link, though there is a transcript.
 * - "missing": AI text whose cited lines are all gone from the transcript
 *   (e.g. after re-transcription).
 * - "notes": AI text written from your notes, with no transcript to cite.
 * - "pending": AI text with no link while the transcript is still loading.
 */
export type BlockTrust = "user" | "sourced" | "unsourced" | "missing" | "notes" | "pending";

export function blockTrust(origin: BlockOrigin, cited: CitedTranscript): BlockTrust {
  if (origin.kind === "user") return "user";
  const ids = origin.source_segment_ids;
  if (cited.state === "ready") {
    if (ids.length === 0) return "unsourced";
    const known = new Set(cited.transcript.segments.map((s) => s.id));
    return ids.some((id) => known.has(id)) ? "sourced" : "missing";
  }
  if (ids.length > 0) return "sourced"; // can't check: never downgrade
  return cited.state === "none" ? "notes" : "pending";
}

/** Lines you should check before keeping: Keep / Remove apply to these. */
export const needsReview = (t: BlockTrust) => t === "unsourced" || t === "missing";

export interface TrustSummary {
  /** AI blocks still in the document. */
  ai: number;
  sourced: number;
  unsourced: number;
  missing: number;
  notes: number;
}

export function trustSummary(blocks: NoteBlock[], cited: CitedTranscript): TrustSummary {
  const s: TrustSummary = { ai: 0, sourced: 0, unsourced: 0, missing: 0, notes: 0 };
  for (const b of blocks) {
    const t = blockTrust(b.origin, cited);
    if (t === "user") continue;
    s.ai += 1;
    if (t === "sourced") s.sourced += 1;
    else if (t === "unsourced") s.unsourced += 1;
    else if (t === "missing") s.missing += 1;
    else if (t === "notes") s.notes += 1;
  }
  return s;
}

/** The legend's one-line summary. "No source" is the honest claim: Enhance
 *  drops a source it can't match to a transcript line, so a line without one
 *  may still be right. Never say it was made up. */
export function trustLine(s: TrustSummary, cited: CitedTranscript): string {
  if (s.ai === 0) return "Everything here is yours";
  const lines = `${s.ai} AI line${s.ai === 1 ? "" : "s"}`;
  if (cited.state === "none") {
    // Lines citing another recording's transcript (a re-recorded note) can't
    // be checked here, but weren't written from your notes either.
    if (s.sourced === 0) return `${lines} · written from your notes`;
    return s.notes > 0 ? `${lines} · ${s.notes} written from your notes` : lines;
  }
  if (cited.state === "checking") return lines;
  const parts = [lines];
  if (s.unsourced > 0) parts.push(`${s.unsourced} without a source`);
  if (s.missing > 0) parts.push(`${s.missing} with a source not found`);
  if (parts.length === 1) parts.push("all sourced");
  return parts.join(" · ");
}

/** The legend's second line. */
export function trustSubline(s: TrustSummary, cited: CitedTranscript): string {
  if (cited.state === "none") {
    return s.sourced === 0
      ? "There's no transcript to check them against, so they come from your notes."
      : "There's no transcript here to check their sources against.";
  }
  return s.sourced > 0
    ? "Your notes and the transcript, merged. Hover a source to see the quote."
    : "Your notes and the transcript, merged.";
}

/** Visible explanation under a line you should check. */
export const REVIEW_COPY: Record<"unsourced" | "missing", { label: string; why: string }> = {
  unsourced: {
    label: "No source",
    why: "Enhance didn't link this line to a transcript line. Check it before you keep it.",
  },
  missing: {
    label: "Source not found",
    why:
      "The line it cited isn't in the current transcript (it may have been re-transcribed). " +
      "Check it before you keep it.",
  },
};

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

/** Resolve cited segment ids to quotes from the transcript Enhance read.
 *  `missing` counts ids that transcript no longer has. */
export function sourceQuotes(
  ids: string[],
  transcript: Transcript | null,
): { found: SourceQuote[]; missing: number } {
  const found: SourceQuote[] = [];
  let missing = 0;
  for (const id of ids) {
    const seg = transcript?.segments.find((s) => s.id === id);
    if (!transcript || !seg) {
      missing += 1;
      continue;
    }
    const idx = transcript.speakers.findIndex((s) => s.key === seg.speaker_key);
    found.push({
      id,
      speakerKey: seg.speaker_key,
      speaker: transcript.speakers[idx]?.label ?? seg.speaker_key,
      speakerIndex: Math.max(idx, 0),
      startMs: seg.start_ms,
      text: seg.text.trim(),
    });
  }
  return { found, missing };
}
