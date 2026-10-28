/* Enhanced-notes trust signals: which blocks are yours, which AI lines link
   to the transcript, and which have no source. Pure, so the copy and the
   counting are unit-tested (enhancedTrust.test.ts). */

import type { BlockOrigin, NoteBlock, Transcript } from "./types";

/** The transcripts Enhance could have cited: the RAW transcripts of every
 *  meeting recorded into the note (enhance_note reads the newest one when it
 *  runs, and a note re-recorded into later has several). Blocks don't say
 *  which meeting they cite, so a source counts if any of them has it;
 *  segment ids are UUIDs, unique across meetings. */
export type CitedTranscript =
  /** No meeting, or none transcribed yet: Enhance wrote from your notes. */
  | { state: "none" }
  /** Still loading (or one couldn't be read): claim nothing either way. */
  | { state: "checking" }
  | {
      state: "ready";
      /** The meetings' transcripts that exist (at least one). */
      transcripts: Transcript[];
      /** Every meeting has a transcript. Until then a cited line that isn't
       *  found may be in one still to come, so it isn't called missing. */
      complete: boolean;
    };

const segmentIds = new WeakMap<Transcript, Set<string>>();
/** The segment ids of a transcript (cached per transcript object). */
function idsOf(t: Transcript): Set<string> {
  let s = segmentIds.get(t);
  if (!s) {
    s = new Set(t.segments.map((x) => x.id));
    segmentIds.set(t, s);
  }
  return s;
}

/** Enhance ran before there was a transcript to cite: one exists now, but
 *  no AI line has a source. The legend says so once, instead of a Keep /
 *  Remove on every line. (A backend flag would be exact; this is the
 *  heuristic until there is one.) */
export function enhancedBeforeTranscript(blocks: NoteBlock[], cited: CitedTranscript): boolean {
  if (cited.state !== "ready") return false;
  let ai = 0;
  for (const b of blocks) {
    if (b.origin.kind === "user") continue;
    if (b.origin.source_segment_ids.length > 0) return false;
    ai += 1;
  }
  return ai > 0;
}

/**
 * - "user": your words (typed, edited or kept).
 * - "sourced": AI text linked to transcript lines (or not checkable yet).
 * - "unsourced": AI text with no link, though there is a transcript.
 * - "missing": AI text whose cited lines are all gone from the transcript
 *   (e.g. after re-transcription).
 * - "notes": AI text written from your notes, with no transcript to cite
 *   (or enhanced before the transcript was ready; see enhancedBeforeTranscript).
 * - "pending": AI text with no link while the transcript is still loading.
 */
export type BlockTrust = "user" | "sourced" | "unsourced" | "missing" | "notes" | "pending";

/** `early`: enhancedBeforeTranscript for the whole document. */
export function blockTrust(origin: BlockOrigin, cited: CitedTranscript, early = false): BlockTrust {
  if (origin.kind === "user") return "user";
  const ids = origin.source_segment_ids;
  if (cited.state === "ready") {
    if (ids.length === 0) return early ? "notes" : "unsourced";
    if (ids.some((id) => cited.transcripts.some((t) => idsOf(t).has(id)))) return "sourced";
    return cited.complete ? "missing" : "sourced";
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
  /** enhancedBeforeTranscript: its AI lines count as notes. */
  early: boolean;
}

export function trustSummary(blocks: NoteBlock[], cited: CitedTranscript): TrustSummary {
  const early = enhancedBeforeTranscript(blocks, cited);
  const s: TrustSummary = { ai: 0, sourced: 0, unsourced: 0, missing: 0, notes: 0, early };
  for (const b of blocks) {
    const t = blockTrust(b.origin, cited, early);
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
  if (s.early) return `${lines} · no sources yet`;
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

export const EARLY_COPY =
  "Enhanced before the transcript was ready. Enhance again to link sources.";

/** The legend's second line. */
export function trustSubline(s: TrustSummary, cited: CitedTranscript): string {
  if (s.early) return EARLY_COPY;
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

/** Resolve cited segment ids to quotes from the transcripts Enhance read
 *  (any of the note's meetings). `missing` counts ids none of them has. */
export function sourceQuotes(
  ids: string[],
  transcripts: Transcript[],
): { found: SourceQuote[]; missing: number } {
  const found: SourceQuote[] = [];
  let missing = 0;
  for (const id of ids) {
    const transcript = transcripts.find((t) => idsOf(t).has(id));
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

/** Where a chip's zoom goes: the meeting holding most of the cited lines,
 *  with its ids first (the transcript view scrolls to the first one). Null
 *  when none of them is found. */
export function sourceTarget(
  ids: string[],
  transcripts: Transcript[],
): { meetingId: string; ids: string[] } | null {
  let best: Transcript | null = null;
  let bestCount = 0;
  for (const t of transcripts) {
    const have = idsOf(t);
    const count = ids.filter((id) => have.has(id)).length;
    if (count > bestCount) {
      best = t;
      bestCount = count;
    }
  }
  if (!best) return null;
  const have = idsOf(best);
  return {
    meetingId: best.meeting_id,
    ids: [...ids.filter((id) => have.has(id)), ...ids.filter((id) => !have.has(id))],
  };
}
