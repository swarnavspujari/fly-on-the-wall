import { useState } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Note } from "../types";
import { api } from "../api";
import { Button, CitationChip, SectionLabel } from "./ui";

interface Props {
  note: Note;
  onNoteChanged: (note: Note) => void;
  /** Zoom-in: select an AI block's source transcript segments (jumps to Transcript). */
  onZoom: (segmentIds: string[]) => void;
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
    <a href={href} title={title} className="text-primary-text underline">
      {children}
    </a>
  ),
  code: ({ children }) => (
    <code className="rounded bg-surface-3 px-1 font-mono text-[12.5px]">{children}</code>
  ),
};

/** The enhanced document: provenance-colored blocks. Your text is plain ink;
 *  AI text sits on a violet-soft card with a left rule, an uppercase label and
 *  a CitationChip (click to jump to the transcript source). Editing an AI block
 *  reclaims it as your text. */
export default function EnhancedDoc({ note, onNoteChanged, onZoom }: Props) {
  const [editing, setEditing] = useState<{ id: string; markdown: string } | null>(null);
  const [saving, setSaving] = useState(false);

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
      <div className="mb-4 flex flex-wrap items-baseline gap-2">
        <SectionLabel>Enhanced notes</SectionLabel>
        <span className="text-[11px]" style={{ color: "var(--text-3)" }}>
          · your notes + transcript, merged · click a citation to jump to the source
        </span>
      </div>

      {note.blocks.map((b) => {
        const isAI = b.origin.kind === "ai";
        const sources = b.origin.kind === "ai" ? b.origin.source_segment_ids : [];
        return (
          <div key={b.id} className="group mb-3.5">
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
                  {isAI && (
                    <span className="text-[11px]" style={{ color: "var(--text-3)" }}>
                      editing makes this your text
                    </span>
                  )}
                </div>
              </div>
            ) : (
              <div
                className={
                  isAI
                    ? "-mx-1 rounded-lg px-4 py-2 text-text"
                    : "border-l-2 border-transparent pl-3 text-text"
                }
                style={
                  isAI
                    ? {
                        background: "var(--primary-soft)",
                        borderLeft: "2px solid var(--primary-border)",
                      }
                    : undefined
                }
              >
                {isAI && (
                  <div className="mb-1 flex items-start justify-between gap-2.5">
                    <span
                      className="text-[10.5px] font-semibold uppercase"
                      style={{ letterSpacing: "0.08em", color: "var(--primary-soft-text)" }}
                    >
                      AI
                    </span>
                    {sources.length > 0 && (
                      <CitationChip count={sources.length} onClick={() => onZoom(sources)} />
                    )}
                  </div>
                )}
                <ReactMarkdown remarkPlugins={[remarkGfm]} components={MD_COMPONENTS}>
                  {b.markdown}
                </ReactMarkdown>
                <div
                  className="mt-1 hidden items-center gap-2 text-[11px] group-hover:flex"
                  style={{ color: "var(--text-3)" }}
                >
                  <button
                    className="cursor-pointer font-semibold hover:text-text"
                    onClick={() => setEditing({ id: b.id, markdown: b.markdown })}
                  >
                    Edit
                  </button>
                  {isAI ? (
                    sources.length > 0 ? (
                      <span>from transcript</span>
                    ) : (
                      <span>AI (no source)</span>
                    )
                  ) : (
                    <span>your text</span>
                  )}
                </div>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
