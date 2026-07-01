import { useEffect, useRef, useState } from "react";
import type { Folder, Note } from "../types";
import { api } from "../api";

interface Props {
  note: Note;
  folders: Folder[];
  onNoteChanged: (note: Note) => void;
  onMoveNote: (folderId: string | null) => void;
}

const URL_RE = /^https?:\/\/\S+$/;

export default function Editor({ note, folders, onNoteChanged, onMoveNote }: Props) {
  const [title, setTitle] = useState(note.title);
  const [scratchpad, setScratchpad] = useState(note.scratchpad);
  const [saveState, setSaveState] = useState<"saved" | "saving">("saved");
  const saveTimer = useRef<number | null>(null);
  const noteIdRef = useRef(note.id);

  // Swap editor contents when a different note is opened.
  useEffect(() => {
    if (noteIdRef.current !== note.id) {
      noteIdRef.current = note.id;
      setTitle(note.title);
      setScratchpad(note.scratchpad);
      setSaveState("saved");
    }
  }, [note]);

  const scheduleSave = (nextTitle: string, nextScratchpad: string) => {
    setSaveState("saving");
    if (saveTimer.current !== null) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      void (async () => {
        try {
          let updated = note;
          if (nextTitle.trim() && nextTitle !== note.title) {
            updated = await api.updateNoteTitle(note.id, nextTitle);
          }
          if (nextScratchpad !== note.scratchpad) {
            updated = await api.updateNoteScratchpad(note.id, nextScratchpad);
          }
          onNoteChanged(updated);
          setSaveState("saved");
        } catch (e) {
          console.error("save failed", e);
        }
      })();
    }, 600);
  };

  // Paste a bare URL → insert it as a markdown link (spec §9 attachments & links).
  const handlePaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const text = e.clipboardData.getData("text/plain").trim();
    if (!URL_RE.test(text)) return;
    e.preventDefault();
    let label = text;
    try {
      const u = new URL(text);
      label = u.hostname + (u.pathname !== "/" ? u.pathname : "");
    } catch {
      /* keep raw url as label */
    }
    const md = `[${label}](${text})`;
    const ta = e.currentTarget;
    const next = scratchpad.slice(0, ta.selectionStart) + md + scratchpad.slice(ta.selectionEnd);
    setScratchpad(next);
    scheduleSave(title, next);
  };

  const attach = async () => {
    const updated = await api.attachFile(note.id);
    if (updated) onNoteChanged(updated);
  };

  const removeAttachment = async (attachmentId: string) => {
    const updated = await api.removeAttachment(note.id, attachmentId);
    onNoteChanged(updated);
  };

  return (
    <div className="flex h-full flex-1 flex-col bg-zinc-900">
      <div className="flex items-center gap-3 border-b border-zinc-800 px-6 py-3">
        <input
          value={title}
          onChange={(e) => {
            setTitle(e.target.value);
            scheduleSave(e.target.value, scratchpad);
          }}
          className="flex-1 bg-transparent text-xl font-semibold text-zinc-100 outline-none placeholder:text-zinc-600"
          placeholder="Untitled"
        />
        <select
          value={note.folder_id ?? ""}
          onChange={(e) => onMoveNote(e.target.value === "" ? null : e.target.value)}
          className="rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-400 outline-none"
          title="Move to folder"
        >
          <option value="">Unfiled</option>
          {folders.map((f) => (
            <option key={f.id} value={f.id}>
              {f.name}
            </option>
          ))}
        </select>
        <span className="w-14 text-right text-xs text-zinc-600">
          {saveState === "saving" ? "saving…" : "saved"}
        </span>
      </div>

      <textarea
        value={scratchpad}
        onChange={(e) => {
          setScratchpad(e.target.value);
          scheduleSave(title, e.target.value);
        }}
        onPaste={handlePaste}
        placeholder="Jot rough notes here during the meeting — Looma will merge them with the transcript when you hit Enhance."
        className="flex-1 resize-none bg-transparent px-6 py-4 font-mono text-sm leading-6 text-zinc-200 outline-none placeholder:text-zinc-600"
      />

      <div className="border-t border-zinc-800 px-6 py-3">
        <div className="flex flex-wrap items-center gap-2">
          <button
            onClick={() => void attach()}
            className="rounded-md border border-zinc-700 px-2.5 py-1 text-xs text-zinc-300 hover:bg-zinc-800"
          >
            📎 Attach file
          </button>
          {note.attachments.map((a) => (
            <span
              key={a.id}
              className="group flex items-center gap-1.5 rounded-md bg-zinc-800 px-2 py-1 text-xs text-zinc-300"
            >
              <button
                className="hover:text-indigo-300"
                title="Open"
                onClick={() => void api.openAttachment(a.rel_path)}
              >
                {a.file_name}
              </button>
              <button
                className="text-zinc-500 hover:text-zinc-200"
                title="Reveal in Explorer"
                onClick={() => void api.revealAttachment(a.rel_path)}
              >
                📂
              </button>
              <button
                className="text-zinc-500 hover:text-red-400"
                title="Remove attachment"
                onClick={() => void removeAttachment(a.id)}
              >
                ✕
              </button>
            </span>
          ))}
        </div>
      </div>
    </div>
  );
}
