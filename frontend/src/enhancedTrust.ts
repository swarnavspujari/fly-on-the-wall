/* Enhanced-notes trust signals: which blocks are yours, which AI lines link
   to the transcript, and which have no source. Pure, so the copy and the
   counting are unit-tested (enhancedTrust.test.ts). */

import { flagCopy } from "./askModel";
import type {
  BlockFlag,
  BlockOrigin,
  EnhanceReport,
  NoteBlock,
  PartialRead,
  Transcript,
} from "./types";

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
 * - "flagged": AI text the Enhance check found something in that the lines it
 *   cites don't hold (a number, date, name or quote), or that states a fact
 *   without citing anything. It never shows as sourced, whatever it cites.
 */
export type BlockTrust =
  "user" | "sourced" | "unsourced" | "missing" | "notes" | "pending" | "flagged";

/** `early`: enhancedBeforeTranscript for the whole document. `flags`: the
 *  block's own (the backend's verdict, so it holds even while a transcript is
 *  still loading). */
export function blockTrust(
  origin: BlockOrigin,
  cited: CitedTranscript,
  early = false,
  flags: BlockFlag[] = [],
): BlockTrust {
  if (origin.kind === "user") return "user";
  const ids = origin.source_segment_ids;
  const flagged = flags.length > 0;
  if (cited.state === "ready") {
    if (ids.length === 0) return flagged ? "flagged" : early ? "notes" : "unsourced";
    if (ids.some((id) => cited.transcripts.some((t) => idsOf(t).has(id)))) {
      return flagged ? "flagged" : "sourced";
    }
    if (cited.complete) return "missing";
    return flagged ? "flagged" : "sourced";
  }
  if (flagged) return "flagged";
  if (ids.length > 0) return "sourced"; // can't check: never downgrade
  return cited.state === "none" ? "notes" : "pending";
}

/** Lines you should check before keeping: Keep / Remove apply to these. */
export const needsReview = (t: BlockTrust) =>
  t === "unsourced" || t === "missing" || t === "flagged";

export interface TrustSummary {
  /** AI blocks still in the document. */
  ai: number;
  sourced: number;
  unsourced: number;
  missing: number;
  /** The Enhance check found a claim its cited lines don't back up. */
  flagged: number;
  notes: number;
  /** enhancedBeforeTranscript: its AI lines count as notes. */
  early: boolean;
  /** Enhance read only part of the meeting these lines come from (saved with
   *  them, so it still shows after you navigate away). */
  partial: PartialRead | null;
}

export function trustSummary(blocks: NoteBlock[], cited: CitedTranscript): TrustSummary {
  const early = enhancedBeforeTranscript(blocks, cited);
  const s: TrustSummary = {
    ai: 0,
    sourced: 0,
    unsourced: 0,
    missing: 0,
    flagged: 0,
    notes: 0,
    early,
    partial: null,
  };
  for (const b of blocks) {
    const t = blockTrust(b.origin, cited, early, b.flags);
    if (t === "user") continue;
    s.ai += 1;
    if (b.partial && !s.partial) s.partial = b.partial;
    if (t === "sourced") s.sourced += 1;
    else if (t === "unsourced") s.unsourced += 1;
    else if (t === "missing") s.missing += 1;
    else if (t === "flagged") s.flagged += 1;
    else if (t === "notes") s.notes += 1;
  }
  return s;
}

/** The legend's one-line summary. "No source" is the honest claim: Enhance
 *  drops a source it can't match to a transcript line, so a line without one
 *  may still be right. Never say it was made up. */
export function trustLine(s: TrustSummary, cited: CitedTranscript): string {
  const line = baseTrustLine(s, cited);
  return s.partial && s.ai > 0 ? `${line} · ${partialCopy(s.partial)}` : line;
}

/** "written from 412 of 1,980 lines": a partial read must never look whole. */
export function partialCopy(p: PartialRead): string {
  return `written from ${p.shown.toLocaleString("en-US")} of ${p.total.toLocaleString("en-US")} lines`;
}

function baseTrustLine(s: TrustSummary, cited: CitedTranscript): string {
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
  if (s.flagged > 0) {
    parts.push(`${s.flagged} not backed by ${s.flagged === 1 ? "its source" : "their sources"}`);
  }
  if (parts.length === 1) parts.push("all sourced");
  return parts.join(" · ");
}

export const EARLY_COPY =
  "Enhanced before the transcript was ready. Enhance again to link sources.";

/** The legend's second line. */
export function trustSubline(s: TrustSummary, cited: CitedTranscript): string {
  const line = baseTrustSubline(s, cited);
  return s.partial
    ? `The meeting was longer than your model can read at once, so these come from excerpts of it. ${line}`
    : line;
}

function baseTrustSubline(s: TrustSummary, cited: CitedTranscript): string {
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

/** Why one line is flagged. The kinds Enhance shares with Ask say what the
 *  Ask sheet says (askModel `flagCopy`), so both views word the same failure
 *  the same way; the two Enhance-only reasons are worded here. */
export function blockFlagCopy(flag: BlockFlag): string {
  switch (flag.kind) {
    case "invalid_citation":
      return "It pointed to a line Enhance didn't read.";
    case "not_from_notes":
      // Says which part: a merged line, an added heading, a link or a figure.
      return flag.detail
        ? `“${flag.detail}” isn't in your notes, so this isn't shown as yours.`
        : "This isn't in your notes, so it isn't shown as yours.";
    case "aimed_at_ai":
      return "It leans on a line that reads as aimed at an AI assistant, or says what only that line says.";
    case "instruction":
      return "It rests on a line phrased as an order or a request to add to the notes, which may not be a decision of the meeting.";
    case "reversed":
      return flag.detail
        ? `It says “${flag.detail}” where its source says the opposite.`
        : "It says the opposite of its source.";
    default:
      return flagCopy({ kind: flag.kind, detail: flag.detail });
  }
}

/** The visible explanation under a flagged line. A line that cites nothing
 *  says so; one that cites lines says what they don't hold. */
export function flaggedCopy(flags: BlockFlag[]): { label: string; why: string } {
  const reasons = [...new Set(flags.map(blockFlagCopy))];
  const noSource = flags.length > 0 && flags.every((f) => f.kind === "uncited_claim");
  return {
    label: noSource ? "No source given" : "Not backed by its source",
    why: `${reasons.join(" ")} Check it before you keep it.`,
  };
}

/** What a finished Enhance tells you beyond the note itself: that it read
 *  part of a long meeting, left something out, or didn't show a line as
 *  yours. Nothing when it read everything and kept everything. */
export function enhanceNotices(r: EnhanceReport): string[] {
  const out: string[] = [];
  if (r.segments_total > 0 && r.segments_shown < r.segments_total) {
    out.push(
      `This meeting is longer than your model can read at once, so Enhance read ${r.segments_shown} of ${r.segments_total} lines: evenly across the meeting, plus the parts your notes mention. The whole transcript is in the Transcript tab.` +
        (r.retried_smaller
          ? " Your model ran out of memory at the larger size, so it read less."
          : ""),
    );
  }
  if (r.dropped > 0) {
    out.push(
      `Left out ${r.dropped} ${r.dropped === 1 ? "line" : "lines"} that rested only on ${r.dropped === 1 ? "a transcript line" : "transcript lines"} reading as aimed at an AI assistant. Nothing in ${r.dropped === 1 ? "it was" : "them was"} acted on. The text is below, in case ${r.dropped === 1 ? "it was" : "they were"} meant for the meeting.`,
    );
  }
  if (r.demoted_user > 0) {
    out.push(
      `${r.demoted_user} ${r.demoted_user === 1 ? "line" : "lines"} that looked like yours ${r.demoted_user === 1 ? "isn't" : "aren't"} in your notes, so ${r.demoted_user === 1 ? "it shows" : "they show"} as AI text to check.`,
    );
  }
  return out;
}

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
