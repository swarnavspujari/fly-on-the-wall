/* Ask's scope: what a question searches (this meeting, all meetings, or a
   meeting, person or folder picked with @). Pure, so the matching and the
   labels are unit-tested (askScope.test.ts). The harness takes one scope per
   question (`ask_meeting`'s `scope`, fly-ask `AskScope`), so an @ pick
   replaces the scope rather than adding to it. */

import type { AskScope } from "./types";

export interface ScopeChoice {
  scope: AskScope;
  /** The scope chip: "This meeting", "All meetings", "@Q3 planning". */
  label: string;
  /** "this" = the note Ask is opened from. */
  origin: "this" | "all" | "mention";
}

/** The open note (every recording in it, plus your notes). */
export const thisScope = (noteId: string, hasMeeting: boolean): ScopeChoice => ({
  scope: { kind: "note", id: noteId },
  label: hasMeeting ? "This meeting" : "This note",
  origin: "this",
});

export const ALL_SCOPE: ScopeChoice = {
  scope: { kind: "all" },
  label: "All meetings",
  origin: "all",
};

export const sameScope = (a: AskScope, b: AskScope) =>
  a.kind === b.kind && ("id" in a ? a.id : null) === ("id" in b ? b.id : null);

export interface MentionOption {
  kind: "meeting" | "person" | "folder";
  /** Note id (meeting), folder id, or the person's name (the harness
   *  matches a name or speaker label). */
  id: string;
  label: string;
  /** Free text the person typed, not a known name. */
  typed?: boolean;
}

export interface MentionCatalog {
  meetings: { id: string; title: string }[];
  people: string[];
  folders: { id: string; name: string }[];
}

/** The longest @ query that still opens suggestions. */
const MENTION_MAX = 40;

/** The @ being typed at the caret: where it starts and what follows it.
 *  An @ counts at the start or after a space (not in an email address). */
export function mentionAt(text: string, caret: number): { start: number; query: string } | null {
  const before = text.slice(0, caret);
  const at = before.lastIndexOf("@");
  if (at === -1) return null;
  if (at > 0 && !/\s/.test(before[at - 1])) return null;
  const query = before.slice(at + 1);
  if (query.length > MENTION_MAX || /[\n@]/.test(query) || /\s{2}/.test(query)) return null;
  if (/^\s/.test(query)) return null;
  return { start: at, query };
}

const norm = (s: string) => s.toLocaleLowerCase().normalize("NFKD").replace(/\p{M}/gu, "");

/** Every word of the query starts a word of the label ("q3 pl" → "Q3
 *  planning offsite"). */
function matches(label: string, query: string): boolean {
  const words = norm(label)
    .split(/[^\p{L}\p{N}]+/u)
    .filter(Boolean);
  return norm(query)
    .split(/\s+/)
    .filter(Boolean)
    .every((q) => words.some((w) => w.startsWith(q)));
}

/** Suggestions for an @ query: meetings, then people, then folders. With
 *  text that isn't a known name, the last option asks about that person. */
export function mentionOptions(query: string, catalog: MentionCatalog, limit = 8): MentionOption[] {
  const q = query.trim();
  const per = q ? limit : 3;
  const meetings = catalog.meetings
    .filter((m) => !q || matches(m.title, q))
    .slice(0, per)
    .map((m): MentionOption => ({ kind: "meeting", id: m.id, label: m.title }));
  const seen = new Set<string>();
  const people = catalog.people
    .filter((p) => {
      const k = norm(p.trim());
      if (!k || seen.has(k)) return false;
      seen.add(k);
      return !q || matches(p, q);
    })
    .slice(0, per)
    .map((p): MentionOption => ({ kind: "person", id: p.trim(), label: p.trim() }));
  const folders = catalog.folders
    .filter((f) => !q || matches(f.name, q))
    .slice(0, per)
    .map((f): MentionOption => ({ kind: "folder", id: f.id, label: f.name }));
  const known = [...meetings, ...people, ...folders].slice(0, limit);
  const exact = known.some((o) => norm(o.label) === norm(q));
  if (q.length < 2 || exact) return known;
  // Last, so a known meeting, person or folder is what Enter picks.
  return [...known, { kind: "person", id: q, label: q, typed: true }];
}

/** The composer text with the @ query removed (it became the scope). */
export function removeMention(text: string, start: number, caret: number): string {
  const head = text.slice(0, start);
  const tail = text.slice(caret);
  return head.endsWith(" ") && tail.startsWith(" ") ? head + tail.slice(1) : head + tail;
}

/** The scope an @ pick sets. A meeting is its note (every recording in it). */
export function mentionScope(o: MentionOption): ScopeChoice {
  const scope: AskScope =
    o.kind === "meeting"
      ? { kind: "note", id: o.id }
      : o.kind === "folder"
        ? { kind: "folder", id: o.id }
        : { kind: "person", id: o.id };
  return { scope, label: `@${o.label}`, origin: "mention" };
}

/** Speaker labels worth suggesting: not "You" and not "Speaker 2". */
export function namedSpeakers(labels: string[]): string[] {
  return labels.filter((l) => l.trim() && !/^(you|speaker\s*\d+|unknown)$/i.test(l.trim()));
}
