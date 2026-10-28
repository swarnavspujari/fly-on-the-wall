import { useEffect, useId, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Note, Transcript } from "../types";
import { api } from "../api";
import {
  NO_SOURCE_TITLE,
  blockTrust,
  sourceQuotes,
  trustLine,
  trustSummary,
  type BlockTrust,
} from "../enhancedTrust";
import { fmtElapsed } from "./RecordingBar";
import { Avatar, Button, CitationChip, EscapeLayerScope, speakerColor, useEscapeLayer } from "./ui";
import { SAFE_IMG, SafeLink } from "./markdownSafe";

interface Props {
  note: Note;
  onNoteChanged: (note: Note) => void;
  /** Zoom-in: select an AI block's source transcript segments (jumps to Transcript). */
  onZoom: (segmentIds: string[]) => void;
  /** Transcripts to quote sources from, AI-cleaned first (same segment ids). */
  transcripts?: (Transcript | null)[];
}

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
const TRUST_STYLE: Record<
  BlockTrust,
  { label: string; card: CSSProperties; swatch: CSSProperties }
> = {
  user: {
    label: "You",
    card: { borderLeft: "2px solid transparent" },
    swatch: { border: "1px solid var(--line-strong)" },
  },
  sourced: {
    label: "AI, sourced",
    card: { background: "var(--primary-soft)", borderLeft: "2px solid var(--primary-border)" },
    swatch: { background: "var(--primary-soft)", border: "1px solid var(--primary-border)" },
  },
  unsourced: {
    label: "AI, no source",
    card: { background: "var(--warning-soft)", borderLeft: "2px solid var(--warning)" },
    swatch: { background: "var(--warning-soft)", border: "1px solid var(--warning)" },
  },
};

function Markdown({ children }: { children: string }) {
  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} components={MD_COMPONENTS}>
      {children}
    </ReactMarkdown>
  );
}

/** One legend for the whole document instead of a label on every block. */
function TrustLegend({ note }: { note: Note }) {
  return (
    <div
      className="mb-4 flex flex-wrap items-center gap-x-4 gap-y-2 rounded-[14px] border border-line px-4 py-3"
      style={{ background: "var(--surface)" }}
    >
      <div className="min-w-0">
        <div className="text-[13px] font-semibold text-text">
          {trustLine(trustSummary(note.blocks))}
        </div>
        <div className="print:hidden text-[12px]" style={{ color: "var(--text-3)" }}>
          Your notes and the transcript, merged. Hover a source to see the quote.
        </div>
      </div>
      <ul
        aria-label="Legend"
        className="ml-auto flex flex-wrap items-center gap-x-3.5 gap-y-1 text-[12px]"
        style={{ color: "var(--text-3)" }}
      >
        {(["user", "sourced", "unsourced"] as const).map((k) => (
          <li key={k} className="inline-flex items-center gap-1.5">
            <span
              aria-hidden="true"
              className="h-2.5 w-2.5 flex-none rounded-[3px]"
              style={TRUST_STYLE[k].swatch}
            />
            {TRUST_STYLE[k].label}
          </li>
        ))}
      </ul>
    </div>
  );
}

/** The citation chip: hover or focus previews the cited lines; a click
 *  opens them in the transcript (as before). */
function SourceChip({
  ids,
  transcripts,
  onZoom,
}: {
  ids: string[];
  transcripts: (Transcript | null)[];
  onZoom: (segmentIds: string[]) => void;
}) {
  const [open, setOpen] = useState(false);
  const hideTimer = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (hideTimer.current !== null) window.clearTimeout(hideTimer.current);
    },
    [],
  );
  const previewId = useId();
  const depth = useEscapeLayer(() => setOpen(false), open);
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
      className="relative flex-none"
      onMouseEnter={show}
      onMouseLeave={hideSoon}
      onFocus={show}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) hideSoon();
      }}
    >
      <CitationChip
        count={ids.length}
        onClick={() => onZoom(ids)}
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
            {shown.map((q) => (
              <div key={q.id} className="flex flex-col gap-1">
                <div className="flex items-center gap-2">
                  <Avatar
                    name={q.speaker}
                    color={speakerColor(q.speakerKey, q.speakerIndex)}
                    shape="circle"
                    size="xs"
                  />
                  <span className="text-[12.5px] font-semibold text-text">{q.speaker}</span>
                  <span className="font-mono text-[11px]" style={{ color: "var(--text-3)" }}>
                    {fmtElapsed(q.startMs)}
                  </span>
                </div>
                <span className="text-[13.5px] leading-[1.55]" style={{ color: "var(--text-2)" }}>
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
                {found.length === 0
                  ? "These lines aren't in the transcript shown now. It may have been re-transcribed."
                  : `${missing} cited ${missing === 1 ? "line isn't" : "lines aren't"} in the transcript shown now.`}
              </span>
            )}
            <button
              type="button"
              onClick={() => {
                setOpen(false);
                onZoom(ids);
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
 *  chip; AI text with no source sits on warning-soft with Keep / Remove, no
 *  hover needed. Editing or keeping an AI block makes it your text. */
export default function EnhancedDoc({ note, onNoteChanged, onZoom, transcripts = [] }: Props) {
  const [editing, setEditing] = useState<{ id: string; markdown: string } | null>(null);
  const [saving, setSaving] = useState(false);
  // The block a Keep / Remove is running on, and the last failure.
  const [pending, setPending] = useState<string | null>(null);
  const [failed, setFailed] = useState<{ id: string; message: string } | null>(null);

  const run = async (blockId: string, action: () => Promise<Note>, what: string) => {
    setPending(blockId);
    setFailed(null);
    try {
      onNoteChanged(await action());
    } catch (e) {
      console.error(e);
      setFailed({ id: blockId, message: `Couldn't ${what} this line. Try again. (${String(e)})` });
    } finally {
      setPending(null);
    }
  };

  const saveEdit = async () => {
    if (!editing) return;
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

  return (
    <div>
      <TrustLegend note={note} />

      {note.blocks.map((b) => {
        const trust = blockTrust(b.origin);
        const sources = b.origin.kind === "ai" ? b.origin.source_segment_ids : [];
        const busy = pending === b.id;
        let body: ReactNode = <Markdown>{b.markdown}</Markdown>;
        if (trust === "sourced") {
          body = (
            <div className="flex items-start gap-2.5">
              <div className="min-w-0 flex-1">{body}</div>
              <SourceChip ids={sources} transcripts={transcripts} onZoom={onZoom} />
            </div>
          );
        }
        return (
          <div key={b.id} className="group mb-3.5" data-trust={trust}>
            {editing?.id === b.id ? (
              <div>
                <textarea
                  autoFocus
                  value={editing.markdown}
                  onChange={(e) => setEditing({ id: b.id, markdown: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === "Escape") setEditing(null);
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
                    disabled={saving}
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
                {trust === "unsourced" && (
                  <div className="mt-1.5 flex flex-wrap items-center gap-2">
                    <span
                      className="mr-auto text-[12px] font-medium"
                      style={{ color: "var(--warning-text)" }}
                      title={NO_SOURCE_TITLE}
                    >
                      No source · check it before you keep it
                    </span>
                    <span className="print:hidden flex items-center gap-1.5">
                      <Button
                        variant="outline"
                        size="xs"
                        disabled={busy}
                        title="Keep this line as your own text"
                        onClick={() =>
                          void run(b.id, () => api.keepNoteBlock(note.id, b.id), "keep")
                        }
                      >
                        Keep
                      </Button>
                      <Button
                        variant="ghost"
                        size="xs"
                        disabled={busy}
                        title="Remove this line from your notes"
                        onClick={() =>
                          void run(b.id, () => api.editNoteBlock(note.id, b.id, ""), "remove")
                        }
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
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
