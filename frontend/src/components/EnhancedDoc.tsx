import { useEffect, useId, useRef, useState } from "react";
import type { CSSProperties, ReactNode, RefObject } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Meeting, Note, Transcript } from "../types";
import { api } from "../api";
import {
  REVIEW_COPY,
  blockTrust,
  enhancedBeforeTranscript,
  needsReview,
  sourceQuotes,
  sourceTarget,
  trustLine,
  trustSubline,
  trustSummary,
  type BlockTrust,
  type CitedTranscript,
} from "../enhancedTrust";
import { fmtElapsed } from "./RecordingBar";
import { Avatar, Button, CitationChip, EscapeLayerScope, useEscapeLayer } from "./ui";
import { SAFE_IMG, SafeLink } from "./markdownSafe";

interface Props {
  note: Note;
  onNoteChanged: (note: Note) => void;
  /** Zoom-in: select an AI block's source transcript segments (jumps to
   *  Transcript), in `meetingId`'s recording when it's known. */
  onZoom: (segmentIds: string[], meetingId?: string) => void;
  /** The raw transcript the editor has loaded (the selected recording). Used
   *  for its meeting; the note's other meetings' transcripts are fetched. */
  transcript?: Transcript | null;
  /** The note's meetings (a note recorded into again has several). Sources
   *  are checked against all of them. Without it, only `note.meeting_id`. */
  meetings?: Meeting[];
  /** Enhance is running: its result replaces every block, so edits made now
   *  would be silently undone. */
  enhancing?: boolean;
}

/** How long a removed line can be brought back before it is deleted. */
export const REMOVE_UNDO_MS = 5000;

/* Markdown → design-language mapping shared by every block: headings become
   uppercase violet mini-headings, bullets become 5px violet dots. */
const MD_COMPONENTS: Components = {
  h1: ({ children }) => (
    <h1 className="mb-2 mt-4 text-[11px] font-semibold uppercase tracking-[0.08em] text-primary-soft-text">
      {children}
    </h1>
  ),
  h2: ({ children }) => (
    <h2 className="mb-2 mt-4 text-[11px] font-semibold uppercase tracking-[0.08em] text-primary-soft-text">
      {children}
    </h2>
  ),
  h3: ({ children }) => (
    <h3 className="mb-2 mt-4 text-[11px] font-semibold uppercase tracking-[0.08em] text-primary-soft-text">
      {children}
    </h3>
  ),
  p: ({ children }) => <p className="my-1 text-[15px] leading-[1.65] text-text">{children}</p>,
  ul: ({ children }) => <ul className="my-1 list-none p-0">{children}</ul>,
  ol: ({ children }) => <ol className="my-1 list-none p-0">{children}</ol>,
  li: ({ children }) => (
    <li className="my-1 flex gap-[11px]">
      <span className="relative top-[9px] h-[5px] w-[5px] flex-none rounded-full bg-primary" />
      <span className="min-w-0 flex-1">{children}</span>
    </li>
  ),
  strong: ({ children }) => <strong className="font-semibold text-text">{children}</strong>,
  a: ({ href, title, children }) => (
    <SafeLink href={href} title={title} className="text-primary-text underline">
      {children}
    </SafeLink>
  ),
  img: SAFE_IMG,
  code: ({ children }) => (
    <code className="rounded bg-surface-3 px-1 font-mono text-[12.5px]">{children}</code>
  ),
};

/** What each provenance looks like; the legend draws the same swatches. */
const WARN_CARD: CSSProperties = {
  background: "var(--warning-soft)",
  borderLeft: "2px solid var(--warning)",
};
const NEUTRAL_CARD: CSSProperties = {
  background: "var(--surface-2)",
  borderLeft: "2px solid var(--line-strong)",
};
const TRUST_STYLE: Record<BlockTrust, { label: string; card: CSSProperties }> = {
  user: { label: "You", card: { borderLeft: "2px solid transparent" } },
  sourced: {
    label: "AI, sourced",
    card: { background: "var(--primary-soft)", borderLeft: "2px solid var(--primary-border)" },
  },
  unsourced: { label: "AI, no source", card: WARN_CARD },
  missing: { label: "AI, source not found", card: WARN_CARD },
  notes: { label: "Written by AI from your notes", card: NEUTRAL_CARD },
  pending: { label: "AI", card: NEUTRAL_CARD },
};
const SWATCH: Record<"user" | "sourced" | "check" | "notes", CSSProperties> = {
  user: { border: "1px solid var(--line-strong)" },
  sourced: { background: "var(--primary-soft)", border: "1px solid var(--primary-border)" },
  check: { background: "var(--warning-soft)", border: "1px solid var(--warning)" },
  notes: { background: "var(--surface-2)", border: "1px solid var(--line-strong)" },
};

const isSelfSpeaker = (key: string) => key === "mic" || key === "you" || key === "self";

function Markdown({ children }: { children: string }) {
  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} components={MD_COMPONENTS}>
      {children}
    </ReactMarkdown>
  );
}

/** The raw transcripts Enhance could have cited, one per meeting of the
 *  note: the loaded one for its meeting, the others fetched. "checking"
 *  until every one is known, so nothing is downgraded (or called
 *  notes-only) on a guess. */
function useCitedTranscripts(
  note: Note,
  meetings: Meeting[] | undefined,
  loaded: Transcript | null,
): CitedTranscript {
  // The editor's list is this note's once it holds the note's own (newest)
  // meeting. While a note opens it can still be the previous note's (or
  // empty), so then the note's meetings are fetched here instead.
  const given = meetings?.filter((m) => m.note_id === note.id).map((m) => m.id);
  const trusted = !given || !note.meeting_id || given.includes(note.meeting_id);
  const [ownList, setOwnList] = useState<{ noteId: string; ids: string[] } | null>(null);
  useEffect(() => {
    if (trusted) return undefined;
    let live = true;
    const noteId = note.id;
    api
      .getMeetingsForNote(noteId)
      .then((ms) => live && setOwnList({ noteId, ids: ms.map((m) => m.id) }))
      .catch(() => {}); // stays "checking": claim nothing
    return () => {
      live = false;
    };
  }, [trusted, note.id]);
  const list = trusted ? (given ?? []) : ownList?.noteId === note.id ? ownList.ids : null;
  const key = [...new Set([...(list ?? []), ...(note.meeting_id ? [note.meeting_id] : [])])].join(
    "|",
  );
  const [fetched, setFetched] = useState<Record<string, Transcript | null>>({});
  useEffect(() => {
    const want = key ? key.split("|") : [];
    const toFetch = want.filter((id) => loaded?.meeting_id !== id);
    if (toFetch.length === 0) return undefined;
    let live = true;
    for (const id of toFetch) {
      api
        .getTranscript(id)
        .then((t) => {
          if (!live) return;
          // keep only this note's meetings (transcripts can be large)
          setFetched((f) => {
            const next: Record<string, Transcript | null> = {};
            for (const k of want) if (k in f) next[k] = f[k];
            next[id] = t;
            return next;
          });
        })
        .catch(() => {}); // stays unknown: claim nothing
    }
    return () => {
      live = false;
    };
  }, [key, loaded]);
  if (!list) return { state: "checking" };
  const ids = key ? key.split("|") : [];
  if (ids.length === 0) return { state: "none" };
  const transcripts: Transcript[] = [];
  let complete = true;
  for (const id of ids) {
    const t = loaded?.meeting_id === id ? loaded : fetched[id];
    if (t === undefined) return { state: "checking" };
    if (t) transcripts.push(t);
    else complete = false; // not transcribed yet
  }
  return transcripts.length === 0 ? { state: "none" } : { state: "ready", transcripts, complete };
}

/** One legend for the whole document instead of a label on every block. */
function TrustLegend({
  note,
  cited,
  legendRef,
}: {
  note: Note;
  cited: CitedTranscript;
  legendRef: RefObject<HTMLDivElement | null>;
}) {
  const summary = trustSummary(note.blocks, cited);
  const items: [keyof typeof SWATCH, string][] = summary.early
    ? [
        ["user", "You"],
        ["notes", "AI, no sources yet"],
      ]
    : cited.state === "none"
      ? [
          ["user", "You"],
          ["notes", "AI, from your notes"],
          ...(summary.sourced > 0
            ? [["sourced", "AI, sourced"] as [keyof typeof SWATCH, string]]
            : []),
        ]
      : [
          ["user", "You"],
          ["sourced", "AI, sourced"],
          ["check", "AI, no source"],
        ];
  return (
    <div
      ref={legendRef}
      tabIndex={-1}
      data-testid="trust-legend"
      className="mb-4 flex flex-wrap items-center gap-x-4 gap-y-2 rounded-[14px] border border-line px-4 py-3 outline-none focus-visible:shadow-[var(--focus-ring)]"
      style={{ background: "var(--surface)" }}
    >
      <div className="min-w-0">
        <div className="text-[13px] font-semibold text-text" aria-live="polite">
          {trustLine(summary, cited)}
        </div>
        {summary.ai > 0 && (
          <div className="print:hidden text-[12px]" style={{ color: "var(--text-3)" }}>
            {trustSubline(summary, cited)}
          </div>
        )}
      </div>
      <ul
        aria-label="Legend"
        className="ml-auto flex flex-wrap items-center gap-x-3.5 gap-y-1 text-[12px]"
        style={{ color: "var(--text-3)" }}
      >
        {items.map(([k, label]) => (
          <li key={k} className="inline-flex items-center gap-1.5">
            <span
              aria-hidden="true"
              className="h-2.5 w-2.5 flex-none rounded-[3px]"
              style={SWATCH[k]}
            />
            {label}
          </li>
        ))}
      </ul>
    </div>
  );
}

/** The citation chip: hover or focus previews the cited lines (the raw
 *  transcript text Enhance read); a click opens them in the transcript of
 *  the recording that holds them. */
function SourceChip({
  ids,
  cited,
  onZoom,
}: {
  ids: string[];
  cited: CitedTranscript;
  onZoom: (segmentIds: string[], meetingId?: string) => void;
}) {
  const transcripts = cited.state === "ready" ? cited.transcripts : [];
  const zoom = () => {
    const target = sourceTarget(ids, transcripts);
    if (target) onZoom(target.ids, target.meetingId);
    else onZoom(ids);
  };
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement>(null);
  const hideTimer = useRef<number | null>(null);
  // Set while Esc hands focus back to the chip, so that focus doesn't reopen it.
  const quietFocus = useRef(false);
  useEffect(
    () => () => {
      if (hideTimer.current !== null) window.clearTimeout(hideTimer.current);
    },
    [],
  );
  const previewId = useId();
  const depth = useEscapeLayer(() => {
    setOpen(false);
    const wrap = wrapRef.current;
    if (wrap && wrap.contains(document.activeElement)) {
      quietFocus.current = true;
      wrap.querySelector<HTMLElement>(".fly-cite")?.focus();
      quietFocus.current = false;
    }
  }, open);
  const show = () => {
    if (hideTimer.current !== null) window.clearTimeout(hideTimer.current);
    hideTimer.current = null;
    setOpen(true);
  };
  const hideSoon = () => {
    if (hideTimer.current !== null) window.clearTimeout(hideTimer.current);
    hideTimer.current = window.setTimeout(() => setOpen(false), 150);
  };
  const { found, missing } = open ? sourceQuotes(ids, transcripts) : { found: [], missing: 0 };
  const shown = found.slice(0, 3);
  const more = found.length - shown.length;

  return (
    <span
      ref={wrapRef}
      className="relative flex-none"
      onMouseEnter={show}
      onMouseLeave={hideSoon}
      onFocus={() => {
        if (!quietFocus.current) show();
      }}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) hideSoon();
      }}
    >
      <CitationChip
        count={ids.length}
        onClick={zoom}
        title=""
        aria-label={`${ids.length} source${ids.length === 1 ? "" : "s"}. Open in transcript`}
        aria-describedby={open ? previewId : undefined}
      />
      {open && (
        <div
          id={previewId}
          data-testid="source-preview"
          className="print:hidden absolute right-0 z-20 flex w-[320px] max-w-[80vw] flex-col gap-2.5 p-3.5"
          style={{
            top: "calc(100% + 6px)",
            background: "var(--surface)",
            border: "1px solid var(--line)",
            borderRadius: "var(--radius-2xl)",
            boxShadow: "var(--shadow-pop)",
            animation: "fly-fade-up 160ms var(--ease-out)",
          }}
        >
          <EscapeLayerScope depth={depth}>
            {cited.state !== "ready" ? (
              <span className="text-[12px]" style={{ color: "var(--text-3)" }}>
                {cited.state === "checking"
                  ? "Loading the transcript…"
                  : "There's no transcript here to quote these lines from."}
              </span>
            ) : (
              <>
                {shown.map((q) => (
                  <div key={q.id} className="flex flex-col gap-1">
                    <div className="flex items-center gap-2">
                      <Avatar
                        name={q.speaker}
                        self={isSelfSpeaker(q.speakerKey)}
                        index={q.speakerIndex}
                        shape="circle"
                        size="xs"
                      />
                      <span className="text-[12.5px] font-semibold text-text">{q.speaker}</span>
                      <span className="font-mono text-[11px]" style={{ color: "var(--text-3)" }}>
                        {fmtElapsed(q.startMs)}
                      </span>
                    </div>
                    <span
                      className="text-[13.5px] leading-[1.55]"
                      style={{ color: "var(--text-2)" }}
                    >
                      “{q.text}”
                    </span>
                  </div>
                ))}
                {more > 0 && (
                  <span className="text-[12px]" style={{ color: "var(--text-3)" }}>
                    +{more} more {more === 1 ? "line" : "lines"}
                  </span>
                )}
                {missing > 0 && (
                  <span className="text-[12px]" style={{ color: "var(--text-3)" }}>
                    {!cited.complete
                      ? `${missing} cited ${missing === 1 ? "line isn't" : "lines aren't"} in a transcript here yet.`
                      : found.length === 0
                        ? "These lines aren't in the current transcript. It may have been re-transcribed."
                        : `${missing} cited ${missing === 1 ? "line isn't" : "lines aren't"} in the current transcript.`}
                  </span>
                )}
              </>
            )}
            <button
              type="button"
              onClick={() => {
                setOpen(false);
                zoom();
              }}
              className="cursor-pointer self-start border-0 bg-transparent p-0 text-[12.5px] font-semibold"
              style={{ color: "var(--primary-text)" }}
            >
              Open in transcript →
            </button>
          </EscapeLayerScope>
        </div>
      )}
    </span>
  );
}

/** The enhanced document: provenance-colored blocks under one legend. Your
 *  text is plain ink; sourced AI text sits on violet-soft with a citation
 *  chip; AI text with no source (or a source no longer in the transcript)
 *  sits on warning-soft with Keep / Remove, no hover needed. With no
 *  transcript at all, AI text was written from your notes and is shown
 *  neutral, without a warning. Editing or keeping an AI block makes it
 *  your text. */
export default function EnhancedDoc({
  note,
  onNoteChanged,
  onZoom,
  transcript = null,
  meetings,
  enhancing = false,
}: Props) {
  const cited = useCitedTranscripts(note, meetings, transcript);
  const early = enhancedBeforeTranscript(note.blocks, cited);
  const [editing, setEditing] = useState<{ id: string; markdown: string } | null>(null);
  const [saving, setSaving] = useState(false);
  // The block a Keep / Remove is running on, and the last failure.
  const [pending, setPending] = useState<string | null>(null);
  const [failed, setFailed] = useState<{ id: string; message: string } | null>(null);
  // Remove is deferred: the line hides behind "Removed · Undo" and is only
  // deleted when the timer runs out. A note switch or leaving the view
  // cancels it; nothing is deleted without the full wait. One Undo at a
  // time: a second Remove deletes the first line now.
  const [removing, setRemoving] = useState<{ noteId: string; id: string } | null>(null);
  const removeTimer = useRef<number | null>(null);
  // Lines whose delete is running: they stay hidden ("Removing…") until it
  // settles, even once a newer Remove holds the Undo.
  const [deleting, setDeleting] = useState<{ noteId: string; id: string }[]>([]);
  // Deletes run one at a time, so their replies land in order.
  const deleteQueue = useRef<Promise<unknown>>(Promise.resolve());
  const noteRef = useRef(note);
  useEffect(() => {
    noteRef.current = note;
  });
  useEffect(
    () => () => {
      // Note switch or unmount: cancel, never delete early.
      if (removeTimer.current !== null) window.clearTimeout(removeTimer.current);
      removeTimer.current = null;
      setRemoving(null);
    },
    [note.id],
  );
  const activeRemove = removing && removing.noteId === note.id ? removing : null;

  // Focus never drops to the page: after Keep it moves to the next line to
  // check, else the legend. After Remove it sits on Undo (in place, so the
  // view doesn't jump away from it) and moves on the same way once the line
  // is really gone. After Undo it returns to the restored line.
  const rootRef = useRef<HTMLDivElement>(null);
  const legendRef = useRef<HTMLDivElement>(null);
  const [focusPlan, setFocusPlan] = useState<
    | { kind: "next"; after: string; later: string[] }
    | { kind: "undo"; id: string }
    | { kind: "block"; id: string }
    | null
  >(null);
  /** The "next" plan for a line: remembers what came after it. */
  const nextAfter = (blockId: string) => {
    const ids = noteRef.current.blocks.map((b) => b.id);
    return { kind: "next" as const, after: blockId, later: ids.slice(ids.indexOf(blockId) + 1) };
  };
  useEffect(() => {
    const root = rootRef.current;
    if (!focusPlan || !root) return;
    const keeps = [...root.querySelectorAll<HTMLElement>("[data-review-keep]")];
    if (focusPlan.kind === "undo") {
      const el = root.querySelector<HTMLElement>(`[data-undo-for="${focusPlan.id}"]`);
      if (!el) return; // not rendered yet
      el.focus();
    } else if (focusPlan.kind === "block") {
      const el = keeps.find((k) => k.dataset.reviewKeep === focusPlan.id);
      if (!el) return;
      el.focus();
    } else {
      // Wait until the acted-on line has left the review list.
      if (keeps.some((k) => k.dataset.reviewKeep === focusPlan.after)) return;
      const next =
        keeps.find((k) => focusPlan.later.includes(k.dataset.reviewKeep ?? "")) ?? keeps[0] ?? null;
      (next ?? legendRef.current)?.focus();
    }
    setFocusPlan(null);
  }, [focusPlan, note]);

  const keep = async (blockId: string) => {
    setPending(blockId);
    setFailed(null);
    const plan = nextAfter(blockId);
    try {
      onNoteChanged(await api.keepNoteBlock(note.id, blockId));
      setFocusPlan(plan);
    } catch (e) {
      console.error(e);
      setFailed({ id: blockId, message: `Couldn't keep this line. Try again. (${String(e)})` });
    } finally {
      setPending(null);
    }
  };

  const commitRemove = (noteId: string, blockId: string) => {
    removeTimer.current = null;
    setRemoving((r) => (r?.id === blockId ? null : r));
    const current = noteRef.current;
    if (current.id !== noteId || !current.blocks.some((b) => b.id === blockId)) {
      return; // the note moved on (switched, or re-enhanced)
    }
    // Undo is about to go: if it had focus, move on to the next line now.
    const undoHadFocus =
      document.activeElement instanceof HTMLElement &&
      document.activeElement.dataset.undoFor === blockId;
    if (undoHadFocus) setFocusPlan(nextAfter(blockId));
    const entry = { noteId, id: blockId };
    setDeleting((d) => [...d, entry]);
    const run = deleteQueue.current.then(() => api.editNoteBlock(noteId, blockId, ""));
    deleteQueue.current = run.catch(() => {});
    // A reply for a note that's no longer open must not reopen it.
    run
      .then((updated) => {
        if (noteRef.current.id === noteId) onNoteChanged(updated);
      })
      .catch((e) => {
        console.error(e);
        if (noteRef.current.id !== noteId) return;
        setFailed({ id: blockId, message: `Couldn't remove this line. Try again. (${String(e)})` });
      })
      .finally(() => setDeleting((d) => d.filter((x) => x !== entry)));
  };

  const startRemove = (blockId: string) => {
    setFailed(null);
    // One Undo at a time: a second Remove deletes the first line now.
    if (activeRemove && removeTimer.current !== null) {
      window.clearTimeout(removeTimer.current);
      commitRemove(activeRemove.noteId, activeRemove.id);
    }
    const noteId = note.id;
    setRemoving({ noteId, id: blockId });
    removeTimer.current = window.setTimeout(() => commitRemove(noteId, blockId), REMOVE_UNDO_MS);
    setFocusPlan({ kind: "undo", id: blockId });
  };

  const undoRemove = () => {
    if (removeTimer.current !== null) window.clearTimeout(removeTimer.current);
    removeTimer.current = null;
    const id = activeRemove?.id;
    setRemoving(null);
    if (id) setFocusPlan({ kind: "block", id });
  };

  const saveEdit = async () => {
    if (!editing || enhancing) return;
    setSaving(true);
    try {
      const updated = await api.editNoteBlock(note.id, editing.id, editing.markdown);
      onNoteChanged(updated);
      setEditing(null);
    } catch (e) {
      console.error(e);
    } finally {
      setSaving(false);
    }
  };

  const lockedTitle = enhancing ? "Wait for Enhance to finish" : undefined;

  return (
    <div ref={rootRef}>
      <TrustLegend note={note} cited={cited} legendRef={legendRef} />

      {note.blocks.map((b) => {
        const trust = blockTrust(b.origin, cited, early);
        const sources = b.origin.kind === "ai" ? b.origin.source_segment_ids : [];
        const busy = pending === b.id || enhancing;
        const review = needsReview(trust) ? REVIEW_COPY[trust as "unsourced" | "missing"] : null;

        const undoable = activeRemove?.id === b.id;
        if (undoable || deleting.some((d) => d.noteId === note.id && d.id === b.id)) {
          return (
            <div
              key={b.id}
              role="status"
              data-testid="removed-line"
              className="print:hidden mb-3.5 flex items-center gap-2 rounded-lg border border-dashed border-line px-4 py-2 text-[13px]"
              style={{ color: "var(--text-2)" }}
            >
              <span className="mr-auto">{undoable ? "Removed" : "Removing…"}</span>
              {undoable && (
                <Button variant="ghost" size="xs" onClick={undoRemove} data-undo-for={b.id}>
                  Undo
                </Button>
              )}
            </div>
          );
        }

        let body: ReactNode = <Markdown>{b.markdown}</Markdown>;
        if (trust === "sourced") {
          body = (
            <div className="flex items-start gap-2.5">
              <div className="min-w-0 flex-1">{body}</div>
              <SourceChip ids={sources} cited={cited} onZoom={onZoom} />
            </div>
          );
        }
        const whyId = `why-${b.id}`;
        return (
          <div key={b.id} className="group mb-3.5" data-trust={trust}>
            {editing?.id === b.id ? (
              <div>
                <textarea
                  autoFocus
                  value={editing.markdown}
                  onChange={(e) => setEditing({ id: b.id, markdown: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === "Escape") {
                      // Claim Esc so the layer stack doesn't also close a sheet.
                      e.preventDefault();
                      setEditing(null);
                    }
                    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) void saveEdit();
                  }}
                  rows={Math.max(3, editing.markdown.split("\n").length + 1)}
                  onFocus={(e) => (e.currentTarget.style.borderColor = "var(--primary)")}
                  onBlur={(e) => (e.currentTarget.style.borderColor = "var(--line)")}
                  className="w-full rounded-lg border px-3 py-2.5 text-[14px] outline-none"
                  style={{
                    background: "var(--surface-2)",
                    borderColor: "var(--line)",
                    color: "var(--text)",
                  }}
                />
                <div className="mt-1.5 flex items-center gap-2">
                  <Button
                    variant="primary"
                    size="xs"
                    onClick={() => void saveEdit()}
                    disabled={saving || enhancing}
                    title={lockedTitle}
                  >
                    {saving ? "Saving…" : "Save (Ctrl+Enter)"}
                  </Button>
                  <Button variant="ghost" size="xs" onClick={() => setEditing(null)}>
                    Cancel
                  </Button>
                  {trust !== "user" && (
                    <span className="text-[11px]" style={{ color: "var(--text-3)" }}>
                      editing makes this your text
                    </span>
                  )}
                </div>
              </div>
            ) : (
              <div
                className={
                  trust === "user" ? "pl-3 text-text" : "-mx-1 rounded-lg px-4 py-2 text-text"
                }
                style={TRUST_STYLE[trust].card}
              >
                <span className="sr-only">{TRUST_STYLE[trust].label}: </span>
                {body}
                {review && (
                  <div className="mt-1.5 flex flex-wrap items-center gap-2">
                    <span
                      id={whyId}
                      className="mr-auto min-w-0 flex-1 text-[12px]"
                      style={{ color: "var(--warning-text)" }}
                    >
                      <strong className="font-semibold">{review.label}</strong> · {review.why}
                    </span>
                    <span className="print:hidden flex items-center gap-1.5">
                      <Button
                        variant="outline"
                        size="xs"
                        disabled={busy}
                        data-review-keep={b.id}
                        aria-describedby={whyId}
                        title={lockedTitle ?? "Keep this line as your own text"}
                        onClick={() => void keep(b.id)}
                      >
                        Keep
                      </Button>
                      <Button
                        variant="ghost"
                        size="xs"
                        disabled={busy}
                        aria-describedby={whyId}
                        title={lockedTitle ?? "Remove this line from your notes"}
                        onClick={() => startRemove(b.id)}
                      >
                        Remove
                      </Button>
                    </span>
                  </div>
                )}
                {failed?.id === b.id && (
                  <div
                    role="alert"
                    className="mt-1 text-[12px]"
                    style={{ color: "var(--error-text)" }}
                  >
                    {failed.message}
                  </div>
                )}
                {!enhancing && (
                  <div
                    className="print:hidden mt-1 hidden items-center gap-2 text-[11px] group-focus-within:flex group-hover:flex"
                    style={{ color: "var(--text-3)" }}
                  >
                    <button
                      className="cursor-pointer font-semibold hover:text-text"
                      onClick={() => setEditing({ id: b.id, markdown: b.markdown })}
                    >
                      Edit
                    </button>
                  </div>
                )}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
