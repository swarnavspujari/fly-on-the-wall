import { useEffect, useRef, useState } from "react";
import type {
  CaptureTarget,
  Folder,
  Meeting,
  ModelProgress,
  Note,
  RecordingStatus,
  ScreenStatus,
  Template,
  Transcript,
} from "../types";
import { api } from "../api";
import { fmtElapsed } from "./RecordingBar";
import { Btn } from "./ui";
import TranscriptPanel from "./TranscriptPanel";
import LivePane from "./LivePane";
import EnhancedDoc from "./EnhancedDoc";
import AskPanel from "./AskPanel";

interface Props {
  note: Note;
  meeting: Meeting | null;
  transcript: Transcript | null;
  pipeStage: string | null;
  pipeDetail: string | null;
  pipelineError: string | null;
  modelProgress: ModelProgress | null;
  recStatus: RecordingStatus;
  screenStatus: ScreenStatus;
  folders: Folder[];
  templates: Template[];
  onNoteChanged: (note: Note) => void;
  onMoveNote: (folderId: string | null) => void;
  onStartRecording: () => void;
  onStartScreen: (target: CaptureTarget) => void;
  onStopScreen: () => void;
  onTranscribe: () => void;
  onRelabel: (speakerKey: string, label: string) => void;
}

const URL_RE = /^https?:\/\/\S+$/;

const SELECT_CLS =
  "cursor-pointer rounded-lg border border-line bg-surface px-2 py-1 text-xs text-ink-2 outline-none focus:border-coral";

export default function Editor({
  note,
  meeting,
  transcript,
  pipeStage,
  pipeDetail,
  pipelineError,
  modelProgress,
  recStatus,
  screenStatus,
  folders,
  templates,
  onNoteChanged,
  onMoveNote,
  onStartRecording,
  onStartScreen,
  onStopScreen,
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
      <div className="flex min-w-0 flex-1 flex-col bg-surface">
        <div className="border-b border-line px-6 pb-3 pt-2.5">
          <div className="print:hidden mb-1.5 flex flex-wrap items-center justify-end gap-2">
            {!recStatus.active && (
              <Btn
                variant="record"
                size="sm"
                onClick={onStartRecording}
                title="Record this meeting (mic + system audio)"
              >
                <span className="h-2 w-2 rounded-full bg-white" /> Record
              </Btn>
            )}
            {screenStatus.active ? (
              <Btn
                variant="dark"
                size="sm"
                onClick={onStopScreen}
                title="Stop the screen recording"
              >
                <span className="h-2 w-2 rounded-[3px] bg-white" />
                <span className="font-mono text-[11px]">{fmtElapsed(screenStatus.elapsed_ms)}</span>
                Stop
              </Btn>
            ) : (
              <select
                value=""
                onChange={(e) => {
                  const choice = e.target.value;
                  e.target.value = "";
                  if (choice === "full") {
                    onStartScreen({ kind: "full_screen" });
                  } else if (choice === "window") {
                    const title = prompt("Exact window title to capture:");
                    if (title) onStartScreen({ kind: "window", title });
                  } else if (choice === "region") {
                    const spec = prompt("Region as x,y,width,height:", "0,0,1280,720");
                    const parts = spec?.split(",").map((n) => parseInt(n.trim(), 10)) ?? [];
                    if (parts.length === 4 && parts.every((n) => Number.isFinite(n))) {
                      onStartScreen({
                        kind: "region",
                        x: parts[0],
                        y: parts[1],
                        width: parts[2],
                        height: parts[3],
                      });
                    }
                  }
                }}
                title="Record the screen (attached to this note)"
                className={SELECT_CLS}
              >
                <option value="">Screen…</option>
                <option value="full">Full screen</option>
                <option value="window">Window…</option>
                <option value="region">Region…</option>
              </select>
            )}
            <select
              value={templateId}
              onChange={(e) => setTemplateId(e.target.value)}
              className={`max-w-36 ${SELECT_CLS}`}
              title="Template for Enhance"
            >
              {templates.map((t) => (
                <option key={t.id} value={t.id}>
                  {t.name}
                </option>
              ))}
            </select>
            <Btn
              variant="soft"
              size="sm"
              onClick={() => void enhance()}
              disabled={enhancing}
              title="Merge your notes with the transcript into structured notes"
            >
              <span className="h-[7px] w-[7px] rounded-full bg-coral" />
              {enhancing ? "Enhancing…" : note.blocks.length > 0 ? "Re-enhance" : "Enhance"}
            </Btn>
            <Btn
              variant={askOpen ? "soft" : "outline"}
              size="sm"
              onClick={() => setAskOpen((o) => !o)}
              title="Ask questions about this meeting"
            >
              Ask
            </Btn>
            <select
              value={note.folder_id ?? ""}
              onChange={(e) => onMoveNote(e.target.value === "" ? null : e.target.value)}
              className={`max-w-28 ${SELECT_CLS}`}
              title="Move to folder"
            >
              <option value="">Unfiled</option>
              {folders.map((f) => (
                <option key={f.id} value={f.id}>
                  {f.name}
                </option>
              ))}
            </select>
            <span className="w-12 shrink-0 text-right text-xs text-mute">
              {saveState === "saving" ? "saving…" : "saved"}
            </span>
          </div>
          <input
            value={title}
            onChange={(e) => {
              setTitle(e.target.value);
              scheduleSave(e.target.value, scratchpad);
            }}
            className="w-full bg-transparent font-display text-[26px] font-bold tracking-tight text-ink outline-none placeholder:text-mute"
            placeholder="Untitled"
          />
        </div>

        {meeting?.recording && (
          <div className="border-b border-line px-6 py-2">
            <div className="flex items-center gap-2 rounded-[12px] border border-line bg-peach-2 px-3 py-1.5">
              <span className="text-[12.5px] text-ink-2">Meeting recording</span>
              <span className="font-mono text-[11px] text-mute">
                {fmtElapsed(meeting.recording.duration_ms)}
              </span>
              {meeting.recording.mixed_path && (
                <Btn
                  variant="ghost"
                  size="xs"
                  onClick={() => void api.openAttachment(meeting.recording!.mixed_path!)}
                >
                  Play
                </Btn>
              )}
              {meeting.recording.mic_path && (
                <Btn
                  variant="ghost"
                  size="xs"
                  title="Reveal recording files"
                  onClick={() => void api.revealAttachment(meeting.recording!.mic_path!)}
                >
                  Files
                </Btn>
              )}
            </div>
          </div>
        )}

        {recStatus.active && recStatus.note_id === note.id && recStatus.meeting_id ? (
          <LivePane meetingId={recStatus.meeting_id} />
        ) : null}

        {meeting && !(recStatus.active && recStatus.note_id === note.id) && (
          <TranscriptPanel
            meeting={meeting}
            transcript={transcript}
            stage={pipeStage}
            stageDetail={pipeDetail}
            modelProgress={modelProgress}
            pipelineError={pipelineError}
            highlightIds={zoomIds}
            onTranscribe={onTranscribe}
            onRelabel={onRelabel}
          />
        )}

        {enhanceError && (
          <div className="mx-6 my-2 rounded-[12px] border border-line bg-peach-2 p-3 text-[13px] text-clay">
            ⚠ {enhanceError}
          </div>
        )}

        {note.blocks.length > 0 && (
          <div className="print:hidden flex gap-5 border-b border-line px-6">
            {(
              [
                ["scratch", "Scratchpad"],
                ["enhanced", "Enhanced"],
              ] as const
            ).map(([id, label]) => (
              <button
                key={id}
                onClick={() => setView(id)}
                className={`-mb-px cursor-pointer border-b-2 bg-transparent px-0.5 py-2 text-[13px] font-semibold transition-colors ${
                  view === id
                    ? "border-coral text-ink"
                    : "border-transparent text-mute hover:text-ink-2"
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
            className="flex-1 resize-none bg-transparent px-6 py-4 text-[15px] leading-[1.75] text-ink outline-none placeholder:text-mute"
          />
        )}

        <div className="print:hidden border-t border-line px-6 py-3">
          <div className="flex flex-wrap items-center gap-2">
            <Btn variant="outline" size="sm" onClick={() => void attach()}>
              Attach file
            </Btn>
            <Btn
              variant="ghost"
              size="sm"
              title="Save this note as a Markdown file"
              onClick={() => void api.exportNote(note.id).catch((e) => alert(String(e)))}
            >
              Export .md
            </Btn>
            <Btn
              variant="ghost"
              size="sm"
              title="Print or save as PDF (only the note content prints)"
              onClick={() => window.print()}
            >
              Print / PDF
            </Btn>
            {note.attachments.map((a) => (
              <span
                key={a.id}
                className="flex items-center gap-1 rounded-[12px] border border-line bg-peach-2 py-1 pl-2.5 pr-1"
              >
                <button
                  className="cursor-pointer text-[12.5px] text-ink-2 hover:text-clay"
                  title="Open"
                  onClick={() => void api.openAttachment(a.rel_path)}
                >
                  {a.file_name}
                </button>
                <Btn
                  variant="ghost"
                  size="xs"
                  title="Reveal in Explorer"
                  onClick={() => void api.revealAttachment(a.rel_path)}
                >
                  Reveal
                </Btn>
                <button
                  className="cursor-pointer rounded-md px-1.5 py-0.5 text-[11px] font-semibold text-mute transition-colors hover:bg-peach hover:text-rec"
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
