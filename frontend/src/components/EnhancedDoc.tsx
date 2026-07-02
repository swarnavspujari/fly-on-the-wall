import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Note, NoteBlock } from "../types";
import { api } from "../api";

interface Props {
  note: Note;
  onNoteChanged: (note: Note) => void;
  /** Zoom-in: select an AI block's source transcript segments. */
  onZoom: (segmentIds: string[]) => void;
}

/** The enhanced document: provenance-colored blocks. Your text is plain;
 *  AI text is tinted and cites its transcript sources (click to zoom).
 *  Editing an AI block reclaims it as your text. */
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

  const blockClass = (b: NoteBlock) =>
    b.origin.kind === "user"
      ? "border-l-2 border-zinc-600 pl-3 text-zinc-100"
      : "border-l-2 border-indigo-500/70 pl-3 text-indigo-100/90";

  return (
    <div className="flex-1 overflow-y-auto px-6 py-4">
      {note.blocks.map((b) => (
        <div key={b.id} className="group mb-3">
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
                className="w-full rounded border border-indigo-500 bg-zinc-950 p-2 font-mono text-sm text-zinc-100 outline-none"
              />
              <div className="mt-1 flex gap-2 text-xs">
                <button
                  onClick={() => void saveEdit()}
                  disabled={saving}
                  className="rounded bg-indigo-600 px-2 py-0.5 text-white hover:bg-indigo-500"
                >
                  {saving ? "Saving…" : "Save (Ctrl+Enter)"}
                </button>
                <button
                  onClick={() => setEditing(null)}
                  className="rounded border border-zinc-700 px-2 py-0.5 text-zinc-400"
                >
                  Cancel
                </button>
                {b.origin.kind === "ai" && (
                  <span className="text-zinc-500">editing makes this your text</span>
                )}
              </div>
            </div>
          ) : (
            <div className={blockClass(b)}>
              <div className="prose-sm max-w-none [&_h1]:text-lg [&_h1]:font-semibold [&_h2]:mt-2 [&_h2]:text-base [&_h2]:font-semibold [&_h3]:font-medium [&_li]:ml-4 [&_li]:list-disc [&_p]:my-1 [&_strong]:font-semibold">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{b.markdown}</ReactMarkdown>
              </div>
              <div className="mt-0.5 hidden gap-2 text-[11px] text-zinc-500 group-hover:flex">
                <button
                  className="hover:text-zinc-300"
                  onClick={() => setEditing({ id: b.id, markdown: b.markdown })}
                >
                  ✎ edit
                </button>
                {b.origin.kind === "ai" ? (
                  b.origin.source_segment_ids.length > 0 ? (
                    <button
                      className="text-indigo-400 hover:text-indigo-300"
                      title="Show the transcript this came from"
                      onClick={() => b.origin.kind === "ai" && onZoom(b.origin.source_segment_ids)}
                    >
                      🔍 {b.origin.source_segment_ids.length} source
                      {b.origin.source_segment_ids.length > 1 ? "s" : ""}
                    </button>
                  ) : (
                    <span className="text-indigo-500/70">AI (no source)</span>
                  )
                ) : (
                  <span>your text</span>
                )}
              </div>
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
