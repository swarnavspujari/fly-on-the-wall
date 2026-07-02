import { useEffect, useRef, useState } from "react";
import type {
  Folder,
  Meeting,
  ModelProgress,
  Note,
  RecordingStatus,
  Template,
  Transcript,
} from "../types";
import { api } from "../api";
import { fmtElapsed } from "./RecordingBar";
import TranscriptPanel from "./TranscriptPanel";
import EnhancedDoc from "./EnhancedDoc";
import AskPanel from "./AskPanel";

interface Props {
  note: Note;
  meeting: Meeting | null;
  transcript: Transcript | null;
  pipeStage: string | null;
  pipelineError: string | null;
  modelProgress: ModelProgress | null;
  recStatus: RecordingStatus;
  folders: Folder[];
  templates: Template[];
  onNoteChanged: (note: Note) => void;
  onMoveNote: (folderId: string | null) => void;
  onStartRecording: () => void;
  onTranscribe: () => void;
  onRelabel: (speakerKey: string, label: string) => void;
}

const URL_RE = /^https?:\/\/\S+$/;

export default function Editor({
  note,
  meeting,
  transcript,
  pipeStage,
  pipelineError,
  modelProgress,
  recStatus,
  folders,
  templates,
  onNoteChanged,
  onMoveNote,
  onStartRecording,
  onTranscribe,
  onRelabel,
}: Props) {
  const [title, setTitle] = useState(note.title);
  const [scratchpad, setScratchpad] = useState(note.scratchpad);
  const [saveState, setSaveState] = useState<"saved" | "saving">("saved");
  const [view, setView] = useState<"scratch" | "enhanced">("scratch");
  const [templateId, setTemplateId] = useState("tpl-general");
  const [enhancing, setEnhancing] = useState(false);
  const [enhanceError, setEnhanceError] = useState<string | null>(null);
  const [askOpen, setAskOpen] = useState(false);
  const [zoomIds, setZoomIds] = useState<string[]>([]);
  const saveTimer = useRef<number | null>(null);
  const noteIdRef = useRef(note.id);

  // Swap editor contents when a different note is opened.
  useEffect(() => {
    if (noteIdRef.current !== note.id) {
      noteIdRef.current = note.id;
      setTitle(note.title);
      setScratchpad(note.scratchpad);
      setSaveState("saved");
      setView(note.blocks.length > 0 ? "enhanced" : "scratch");
      setEnhanceError(null);
      setAskOpen(false);
      setZoomIds([]);
    }
  }, [note]);

  // External scratchpad changes (e.g. "insert into note" from Ask).
  useEffect(() => {
    if (noteIdRef.current === note.id && note.scratchpad !== scratchpad && saveState === "saved") {
      setScratchpad(note.scratchpad);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [note.scratchpad]);

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

  // Paste a bare URL → insert it as a markdown link (spec §9).
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

  const enhance = async () => {
    setEnhancing(true);
    setEnhanceError(null);
    try {
      const updated = await api.enhanceNote(note.id, templateId);
      onNoteChanged(updated);
      setView("enhanced");
    } catch (e) {
      setEnhanceError(String(e));
    } finally {
      setEnhancing(false);
    }
  };

  const insertFromAsk = (content: string) => {
    const next = scratchpad ? `${scratchpad}\n\n${content}` : content;
    setScratchpad(next);
    scheduleSave(title, next);
  };

  const zoom = (segmentIds: string[]) => {
    setZoomIds(segmentIds);
  };

  return (
    <div className="flex h-full min-w-0 flex-1">
      <div className="flex min-w-0 flex-1 flex-col bg-zinc-900">
        <div className="flex items-center gap-3 border-b border-zinc-800 px-6 py-3">
          <input
            value={title}
            onChange={(e) => {
              setTitle(e.target.value);
              scheduleSave(e.target.value, scratchpad);
            }}
            className="min-w-0 flex-1 bg-transparent text-xl font-semibold text-zinc-100 outline-none placeholder:text-zinc-600"
            placeholder="Untitled"
          />
          {!recStatus.active && (
            <button
              onClick={onStartRecording}
              title="Record this meeting (mic + system audio)"
              className="rounded-md bg-red-600/90 px-2.5 py-1 text-xs font-medium text-white hover:bg-red-500"
            >
              ● Record
            </button>
          )}
          <select
            value={templateId}
            onChange={(e) => setTemplateId(e.target.value)}
            className="max-w-36 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-400 outline-none"
            title="Template for Enhance"
          >
            {templates.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
          <button
            onClick={() => void enhance()}
            disabled={enhancing}
            title="Merge your notes with the transcript into structured notes"
            className="rounded-md bg-indigo-600 px-2.5 py-1 text-xs font-medium text-white hover:bg-indigo-500 disabled:opacity-60"
          >
            {enhancing ? "Enhancing…" : note.blocks.length > 0 ? "✨ Re-enhance" : "✨ Enhance"}
          </button>
          <button
            onClick={() => setAskOpen((o) => !o)}
            title="Ask questions about this meeting"
            className={`rounded-md border px-2.5 py-1 text-xs font-medium ${
              askOpen
                ? "border-indigo-500 text-indigo-300"
                : "border-zinc-700 text-zinc-300 hover:bg-zinc-800"
            }`}
          >
            💬 Ask
          </button>
          <select
            value={note.folder_id ?? ""}
            onChange={(e) => onMoveNote(e.target.value === "" ? null : e.target.value)}
            className="max-w-28 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-400 outline-none"
            title="Move to folder"
          >
            <option value="">Unfiled</option>
            {folders.map((f) => (
              <option key={f.id} value={f.id}>
                {f.name}
              </option>
            ))}
          </select>
          <span className="w-12 shrink-0 text-right text-xs text-zinc-600">
            {saveState === "saving" ? "saving…" : "saved"}
          </span>
        </div>

        {meeting?.recording && (
          <div className="flex items-center gap-2 border-b border-zinc-800 bg-zinc-900/80 px-6 py-2 text-xs text-zinc-400">
            <span>🎙 Meeting recording · {fmtElapsed(meeting.recording.duration_ms)}</span>
            {meeting.recording.mixed_path && (
              <button
                className="text-indigo-300 hover:underline"
                onClick={() => void api.openAttachment(meeting.recording!.mixed_path!)}
              >
                ▶ Play
              </button>
            )}
            {meeting.recording.mic_path && (
              <button
                className="text-zinc-500 hover:text-zinc-300"
                title="Reveal recording files"
                onClick={() => void api.revealAttachment(meeting.recording!.mic_path!)}
              >
                📂 Files
              </button>
            )}
          </div>
        )}

        {meeting && (
          <TranscriptPanel
            meeting={meeting}
            transcript={transcript}
            stage={pipeStage}
            modelProgress={modelProgress}
            pipelineError={pipelineError}
            highlightIds={zoomIds}
            onTranscribe={onTranscribe}
            onRelabel={onRelabel}
          />
        )}

        {enhanceError && (
          <div className="border-b border-red-900/50 bg-red-950/40 px-6 py-1.5 text-xs text-red-300">
            ⚠ {enhanceError}
          </div>
        )}

        {note.blocks.length > 0 && (
          <div className="flex gap-1 border-b border-zinc-800 px-6 pt-2">
            {(
              [
                ["scratch", "Scratchpad"],
                ["enhanced", "Enhanced ✨"],
              ] as const
            ).map(([id, label]) => (
              <button
                key={id}
                onClick={() => setView(id)}
                className={`rounded-t-md px-3 py-1.5 text-xs font-medium ${
                  view === id ? "bg-zinc-800 text-zinc-100" : "text-zinc-500 hover:text-zinc-300"
                }`}
              >
                {label}
              </button>
            ))}
          </div>
        )}

        {view === "enhanced" && note.blocks.length > 0 ? (
          <EnhancedDoc note={note} onNoteChanged={onNoteChanged} onZoom={zoom} />
        ) : (
          <textarea
            value={scratchpad}
            onChange={(e) => {
              setScratchpad(e.target.value);
              scheduleSave(title, e.target.value);
            }}
            onPaste={handlePaste}
            placeholder="Jot rough notes here during the meeting — then hit Enhance to merge them with the transcript."
            className="flex-1 resize-none bg-transparent px-6 py-4 font-mono text-sm leading-6 text-zinc-200 outline-none placeholder:text-zinc-600"
          />
        )}

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

      {askOpen && (
        <AskPanel noteId={note.id} onInsert={insertFromAsk} onClose={() => setAskOpen(false)} />
      )}
    </div>
  );
}
