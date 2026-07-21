import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import type {
  Attachment,
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
import {
  AudioWaveform,
  Calendar,
  Check,
  ChevronDown,
  Copy,
  FileDown,
  Folder as FolderIcon,
  LayoutTemplate,
  List,
  MessageSquare,
  Mic,
  Monitor,
  Paperclip,
  Pause,
  Play,
  Printer,
  Sparkles,
  Square,
  X,
} from "lucide-react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { api } from "../api";
import { fmtElapsed } from "./RecordingBar";
import { Button, Modal, SectionLabel } from "./ui";
import AttendeeEditor from "./AttendeeEditor";
import NotesEditor from "./NotesEditor";
import TranscriptPanel from "./TranscriptPanel";
import LivePane from "./LivePane";
import EnhancedDoc from "./EnhancedDoc";
import AskPanel from "./AskPanel";

interface Props {
  note: Note;
  meeting: Meeting | null;
  /** ALL meetings attached to this note, newest first (a note re-recorded
   *  into has several). The transcript view offers a picker when there is
   *  more than one, so earlier recordings stay reachable. */
  meetings: Meeting[];
  onSelectMeeting: (meeting: Meeting) => void;
  transcript: Transcript | null;
  /** LLM-polished variant (null until the cleanup pass has run). */
  cleanedTranscript: Transcript | null;
  pipeStage: string | null;
  pipeDetail: string | null;
  pipelineError: string | null;
  modelProgress: ModelProgress | null;
  /** whisper-cli engine readiness (null until first settings fetch). */
  engine: { installed: boolean; managed: boolean } | null;
  engineInstalling: boolean;
  recStatus: RecordingStatus;
  screenStatus: ScreenStatus;
  folders: Folder[];
  templates: Template[];
  /** Absolute app data dir — lets the audio player stream local recordings. */
  dataDir: string | null;
  onNoteChanged: (note: Note) => void;
  /** Attendee edits (editor Save, "Someone else…") update the open meeting. */
  onMeetingChanged: (meeting: Meeting) => void;
  /** Undo of a re-diarize restored this transcript (cleaned refreshes too). */
  onTranscriptRestored: (transcript: Transcript) => void;
  onMoveNote: (folderId: string | null) => void;
  onStartRecording: () => void;
  onStartScreen: (target: CaptureTarget) => void;
  onStopScreen: () => void;
  onTranscribe: () => void;
  onInstallEngine: () => void;
  onOpenSettings: (focus: "engine" | "groq") => void;
  onRelabel: (speakerKey: string, label: string) => void;
  onEditSegment: (segmentId: string, text: string) => void;
}

type View = "notes" | "transcript" | "enhanced";

const VIDEO_RE = /\.(mp4|webm|mov|mkv|m4v)$/i;
const isVideo = (a: Attachment) =>
  (a.mime?.startsWith("video/") ?? false) || VIDEO_RE.test(a.file_name);

/** Rel path inside the data dir → asset-protocol URL the webview can stream. */
const toAssetSrc = (dataDir: string | null, relPath: string): string | null =>
  dataDir
    ? convertFileSrc(
        `${dataDir.replace(/[\\/]+$/, "")}/${relPath.replace(/^[\\/]+/, "")}`.replace(/\\/g, "/"),
      )
    : null;

const CHIP =
  "inline-flex items-center gap-1.5 rounded-full border border-line bg-surface-2 px-2.5 py-1 text-[12.5px] font-medium text-text-2";

function fmtWhen(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const date = d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  const time = d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  return `${date} · ${time}`;
}

/* ---- Audio player: embeds the local recording via the asset protocol so it
   plays and scrubs in-app (round violet play/pause, progress-filled waveform,
   mono timecodes). Falls back to opening the OS player if the src is missing. ---- */
const BAR_COUNT = 56;
function AudioPlayer({
  src,
  durationMs,
  onFallback,
}: {
  src: string | null;
  durationMs: number;
  onFallback: () => void;
}) {
  const audioRef = useRef<HTMLAudioElement>(null);
  const [playing, setPlaying] = useState(false);
  const [cur, setCur] = useState(0);
  const [dur, setDur] = useState(durationMs / 1000);

  useEffect(() => {
    setDur(durationMs / 1000);
    setCur(0);
    setPlaying(false);
  }, [src, durationMs]);

  const toggle = () => {
    const a = audioRef.current;
    if (!a || !src) {
      onFallback();
      return;
    }
    if (a.paused) void a.play().catch(onFallback);
    else a.pause();
  };
  const seekToFraction = (frac: number) => {
    const a = audioRef.current;
    const f = Math.max(0, Math.min(1, frac));
    if (a && dur > 0 && Number.isFinite(dur)) a.currentTime = f * dur;
    else setCur(f * dur);
  };
  const pct = dur > 0 ? cur / dur : 0;

  return (
    <div
      className="mb-4 flex items-center gap-3 rounded-xl border border-line px-3.5 py-2.5"
      style={{ background: "var(--surface-2)" }}
    >
      {src && (
        <audio
          ref={audioRef}
          src={src}
          preload="metadata"
          onPlay={() => setPlaying(true)}
          onPause={() => setPlaying(false)}
          onEnded={() => setPlaying(false)}
          onTimeUpdate={(e) => setCur(e.currentTarget.currentTime)}
          onLoadedMetadata={(e) => {
            if (Number.isFinite(e.currentTarget.duration)) setDur(e.currentTarget.duration);
          }}
        />
      )}
      <Button
        variant="primary"
        size="sm"
        onClick={toggle}
        title={playing ? "Pause" : "Play recording"}
        aria-label={playing ? "Pause" : "Play recording"}
        className="!h-[38px] !w-[38px] !rounded-full !p-0"
      >
        {playing ? (
          <Pause size={16} strokeWidth={2} />
        ) : (
          <Play size={16} strokeWidth={2} style={{ marginLeft: 2 }} />
        )}
      </Button>
      <span className="w-10 flex-none font-mono text-[12px]" style={{ color: "var(--text-2)" }}>
        {fmtElapsed(cur * 1000)}
      </span>
      <div
        className="flex flex-1 cursor-pointer items-center gap-[2px]"
        style={{ height: 30 }}
        title="Seek"
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          seekToFraction((e.clientX - r.left) / r.width);
        }}
      >
        {Array.from({ length: BAR_COUNT }).map((_, i) => {
          const h = 5 + (Math.sin(i * 1.7) * 0.5 + 0.5) * 17;
          const on = i / BAR_COUNT <= pct;
          return (
            <span
              key={i}
              className="flex-1 rounded-[2px]"
              style={{
                height: h,
                background: on ? "var(--primary)" : "var(--line-strong)",
                opacity: on ? 1 : 0.45,
              }}
            />
          );
        })}
      </div>
      <span
        className="w-12 flex-none text-right font-mono text-[12px]"
        style={{ color: "var(--text-3)" }}
      >
        {fmtElapsed(dur * 1000)}
      </span>
    </div>
  );
}

function VideoTile({
  att,
  big,
  thumbSrc,
  onOpen,
}: {
  att: Attachment;
  big?: boolean;
  thumbSrc: string | null;
  onOpen: (att: Attachment) => void;
}) {
  const [thumbOk, setThumbOk] = useState(true);
  return (
    <div
      className="flex-none overflow-hidden rounded-xl border border-line"
      style={{ width: big ? "100%" : 200 }}
    >
      <button
        onClick={() => onOpen(att)}
        title={`Play ${att.file_name}`}
        aria-label={`Play ${att.file_name}`}
        className="relative block w-full cursor-pointer border-0 p-0"
        style={{
          paddingTop: "56%",
          // striped placeholder stays underneath as the loading/failure state
          background:
            "repeating-linear-gradient(135deg, rgba(128,124,140,.10) 0 8px, rgba(128,124,140,.2) 8px 16px)",
        }}
      >
        {thumbSrc && thumbOk && (
          <img
            src={thumbSrc}
            alt=""
            className="absolute inset-0 h-full w-full object-cover"
            onError={() => setThumbOk(false)}
          />
        )}
        <span className="absolute inset-0 grid place-items-center">
          <span
            className="grid place-items-center rounded-full"
            style={{
              width: big ? 52 : 40,
              height: big ? 52 : 40,
              background: "rgba(255,255,255,.92)",
              color: "#0D0D12",
              boxShadow: "var(--shadow-md)",
            }}
          >
            <Play size={big ? 20 : 15} strokeWidth={2} style={{ marginLeft: 3 }} />
          </span>
        </span>
      </button>
      <div
        className="overflow-hidden text-ellipsis whitespace-nowrap px-2.5 py-1.5 text-[12px] font-semibold"
        style={{ color: "var(--text)", background: "var(--surface-2)" }}
      >
        {att.file_name}
      </div>
    </div>
  );
}

function VideoStrip({
  videos,
  thumbFor,
  onOpen,
}: {
  videos: Attachment[];
  thumbFor: (att: Attachment) => string | null;
  onOpen: (att: Attachment) => void;
}) {
  const [open, setOpen] = useState(false);
  if (videos.length === 0) return null;
  if (videos.length === 1) {
    return (
      <div className="mb-3.5">
        {/* keyed so per-tile img-error state resets when the note (and video) changes */}
        <VideoTile
          key={videos[0].id}
          att={videos[0]}
          big
          thumbSrc={thumbFor(videos[0])}
          onOpen={onOpen}
        />
      </div>
    );
  }
  return (
    <div className="mb-3.5">
      <button
        onClick={() => setOpen((o) => !o)}
        className="inline-flex cursor-pointer items-center gap-2 rounded-full border border-line px-3 py-1.5 text-[12.5px] font-semibold"
        style={{ background: "var(--surface-2)", color: "var(--text)" }}
      >
        <Monitor size={14} strokeWidth={1.75} style={{ color: "var(--text-3)" }} />
        {videos.length} screen recordings
        <ChevronDown
          size={14}
          strokeWidth={1.75}
          style={{ transform: open ? "rotate(180deg)" : "none", transition: "transform .15s" }}
        />
      </button>
      {open && (
        <div className="mt-2.5 flex flex-wrap gap-2.5">
          {videos.map((v) => (
            <VideoTile key={v.id} att={v} thumbSrc={thumbFor(v)} onOpen={onOpen} />
          ))}
        </div>
      )}
    </div>
  );
}

/* ---- Video lightbox: in-app playback for screen recordings via the same
   asset protocol the audio player streams through (range requests → seeking
   works on large MP4s). Modal gives Esc / overlay / × close; the OS player
   stays available as a secondary action and as the fallback when the file
   won't stream (moved/deleted). ---- */
function VideoLightbox({
  att,
  src,
  poster,
  onClose,
}: {
  att: Attachment;
  src: string | null;
  poster: string | null;
  onClose: () => void;
}) {
  const [failed, setFailed] = useState(false);
  return (
    <Modal
      open
      onClose={onClose}
      title={att.file_name}
      width={880}
      footer={
        <Button
          variant="outline"
          size="sm"
          title="Open this recording in your system's video player"
          onClick={() => void api.openAttachment(att.rel_path)}
        >
          Open in system player
        </Button>
      }
    >
      {src && !failed ? (
        <video
          src={src}
          poster={poster ?? undefined}
          controls
          autoPlay
          className="block w-full rounded-lg"
          style={{ maxHeight: "68vh", background: "#000" }}
          onError={() => setFailed(true)}
        />
      ) : (
        <div
          className="rounded-lg border border-line px-4 py-6 text-center text-[13px]"
          style={{ color: "var(--text-2)" }}
        >
          This recording can't be played here — the file may be missing or in a format the app can't
          decode. Try the system player below.
        </div>
      )}
    </Modal>
  );
}

/* ---- Screen-record control: outline Button + capture popover, or the live
   recording chip while capturing. Wired to the existing screen handlers. ---- */
function ScreenControl({
  screenStatus,
  onStartScreen,
  onStopScreen,
}: {
  screenStatus: ScreenStatus;
  onStartScreen: (target: CaptureTarget) => void;
  onStopScreen: () => void;
}) {
  const [menu, setMenu] = useState(false);

  if (screenStatus.active) {
    return (
      <span
        className="inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1.5"
        style={{
          borderColor: "var(--rec)",
          background: "color-mix(in srgb, var(--rec) 12%, transparent)",
        }}
      >
        <span
          className="h-2 w-2 rounded-full"
          style={{ background: "var(--rec)", animation: "fly-pulse-dot 1.2s ease infinite" }}
        />
        <span className="whitespace-nowrap text-xs font-semibold" style={{ color: "var(--rec)" }}>
          Screen · {fmtElapsed(screenStatus.elapsed_ms)}
        </span>
        <button
          onClick={onStopScreen}
          title="Stop screen recording"
          className="flex cursor-pointer border-0 bg-transparent p-0"
          style={{ color: "var(--rec)" }}
        >
          <Square size={11} strokeWidth={2} />
        </button>
      </span>
    );
  }

  const pick = (choice: "full" | "window" | "region") => {
    setMenu(false);
    if (choice === "full") {
      onStartScreen({ kind: "full_screen" });
    } else if (choice === "window") {
      const t = prompt("Exact window title to capture:");
      if (t) onStartScreen({ kind: "window", title: t });
    } else {
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
  };

  return (
    <span className="relative inline-flex">
      <Button
        variant="outline"
        size="sm"
        onClick={() => setMenu((m) => !m)}
        startIcon={<Monitor size={14} strokeWidth={1.75} />}
        title="Record the screen (attached to this note)"
      >
        Record screen
      </Button>
      {menu && (
        <>
          <span className="fixed inset-0 z-10" onClick={() => setMenu(false)} aria-hidden="true" />
          <span
            className="absolute right-0 z-20 min-w-[154px] p-1"
            style={{
              top: "calc(100% + 6px)",
              background: "var(--surface)",
              border: "1px solid var(--line)",
              borderRadius: "var(--radius-lg)",
              boxShadow: "var(--shadow-pop)",
            }}
          >
            <SectionLabel className="block px-2.5 pb-1 pt-1.5">Capture</SectionLabel>
            {(
              [
                ["full", "Full screen"],
                ["window", "Window…"],
                ["region", "Region…"],
              ] as const
            ).map(([v, l]) => (
              <button
                key={v}
                onClick={() => pick(v)}
                className="block w-full cursor-pointer border-0 bg-transparent px-2.5 py-1.5 text-left text-[13px] text-text hover:bg-surface-3"
                style={{ borderRadius: "var(--radius-sm)" }}
              >
                {l}
              </button>
            ))}
          </span>
        </>
      )}
    </span>
  );
}

/* ---- Title context row: passive context chips + a real "Show in folder"
   action Button set off by a divider. ---- */
function MetaRow({
  note,
  meeting,
  hasTranscript,
  folders,
  templates,
  templateId,
  setTemplateId,
  onMoveNote,
  onMeetingChanged,
  onTranscriptRestored,
  onShowInFolder,
  onOpenAttachment,
  onRemoveAttachment,
}: {
  note: Note;
  meeting: Meeting | null;
  hasTranscript: boolean;
  folders: Folder[];
  templates: Template[];
  templateId: string;
  setTemplateId: (id: string) => void;
  onMoveNote: (folderId: string | null) => void;
  onMeetingChanged: (meeting: Meeting) => void;
  onTranscriptRestored: (transcript: Transcript) => void;
  onShowInFolder: () => void;
  onOpenAttachment: (relPath: string) => void;
  onRemoveAttachment: (attachmentId: string) => void;
}) {
  const [opening, setOpening] = useState(false);
  const tplName =
    templates.find((t) => t.id === templateId)?.name ?? templates[0]?.name ?? "Template";
  const folderName = note.folder_id
    ? (folders.find((f) => f.id === note.folder_id)?.name ?? "Unfiled")
    : "Unfiled";
  const rec = meeting?.recording ?? null;
  const mins = rec ? Math.max(1, Math.round(rec.duration_ms / 60000)) : 0;
  const fileAtts = note.attachments.filter((a) => !isVideo(a));
  const when = fmtWhen(meeting?.started_at ?? note.created_at);

  return (
    <div className="flex flex-wrap items-center gap-2">
      {when && (
        <span className={CHIP}>
          <Calendar size={13} strokeWidth={1.75} style={{ color: "var(--text-3)" }} /> {when}
        </span>
      )}

      {/* template chip — a styled native select */}
      <span
        className={`${CHIP} relative cursor-pointer !p-0 hover:bg-surface-3`}
        title="Note template"
      >
        <span className="pointer-events-none inline-flex items-center gap-1.5 py-1 pl-2.5 pr-7">
          <LayoutTemplate size={13} strokeWidth={1.75} style={{ color: "var(--text-3)" }} />{" "}
          {tplName}
        </span>
        <ChevronDown
          size={12}
          strokeWidth={1.75}
          className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2"
          style={{ color: "var(--text-3)" }}
        />
        <select
          value={templateId}
          onChange={(e) => setTemplateId(e.target.value)}
          aria-label="Note template"
          className="absolute inset-0 h-full w-full cursor-pointer border-0 opacity-0"
        >
          {templates.map((t) => (
            <option key={t.id} value={t.id}>
              {t.name}
            </option>
          ))}
        </select>
      </span>

      {/* folder chip — a styled native select that files this note */}
      <span
        className={`${CHIP} relative cursor-pointer !p-0 hover:bg-surface-3`}
        title="Move to folder"
      >
        <span className="pointer-events-none inline-flex items-center gap-1.5 py-1 pl-2.5 pr-7">
          <FolderIcon size={13} strokeWidth={1.75} style={{ color: "var(--text-3)" }} />{" "}
          {folderName}
        </span>
        <ChevronDown
          size={12}
          strokeWidth={1.75}
          className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2"
          style={{ color: "var(--text-3)" }}
        />
        <select
          value={note.folder_id ?? ""}
          onChange={(e) => onMoveNote(e.target.value === "" ? null : e.target.value)}
          aria-label="Move to folder"
          className="absolute inset-0 h-full w-full cursor-pointer border-0 opacity-0"
        >
          <option value="">Unfiled</option>
          {folders.map((f) => (
            <option key={f.id} value={f.id}>
              {f.name}
            </option>
          ))}
        </select>
      </span>

      {meeting && (
        <AttendeeEditor
          meeting={meeting}
          hasTranscript={hasTranscript}
          onMeetingChanged={onMeetingChanged}
          onTranscriptRestored={onTranscriptRestored}
        />
      )}

      {rec && (
        <span className={CHIP}>
          <Mic size={13} strokeWidth={1.75} style={{ color: "var(--rec)" }} /> Recorded · {mins} min
        </span>
      )}

      {fileAtts.map((a) => (
        <span
          key={a.id}
          className={`${CHIP} group cursor-pointer border-dashed hover:bg-surface-3`}
        >
          <button
            className="inline-flex cursor-pointer items-center gap-1.5 border-0 bg-transparent p-0 text-inherit"
            title="Open"
            onClick={() => onOpenAttachment(a.rel_path)}
          >
            <Paperclip size={13} strokeWidth={1.75} style={{ color: "var(--text-3)" }} />{" "}
            {a.file_name}
          </button>
          <button
            title="Remove attachment"
            onClick={() => onRemoveAttachment(a.id)}
            className="ml-0.5 hidden cursor-pointer rounded p-0.5 text-text-3 hover:text-rec group-hover:inline-flex"
          >
            <X size={11} strokeWidth={2} />
          </button>
        </span>
      ))}

      {/* Secondary action — a real Button (not a chip): reveals this note's local folder */}
      <span
        className="mx-1 h-[18px] w-px flex-none"
        style={{ background: "var(--line)" }}
        aria-hidden="true"
      />
      <Button
        variant="outline"
        size="sm"
        title="Show in folder — open this note's folder on this machine (audio, transcript & files stay local)"
        onClick={() => {
          setOpening(true);
          onShowInFolder();
          window.setTimeout(() => setOpening(false), 1300);
        }}
        startIcon={
          <FolderIcon size={14} strokeWidth={1.75} style={{ color: "var(--primary-text)" }} />
        }
      >
        {opening ? "Opening…" : "Show in folder"}
      </Button>
    </div>
  );
}

/* ---- Floating Granola-style view switcher ---- */
function SwitchSeg({
  active,
  onClick,
  icon,
  label,
  busy,
}: {
  active: boolean;
  onClick: () => void;
  icon: ReactNode;
  label: string;
  busy?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      title={label}
      className={`inline-flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-full border-0 px-[15px] py-2 text-[12.5px] font-semibold transition-colors ${
        active ? "bg-primary text-on-primary" : "bg-transparent text-text-2 hover:bg-surface-3"
      }`}
    >
      {icon}
      {label}
      {busy && (
        <span
          className="ml-0.5 h-1.5 w-1.5 rounded-full bg-current"
          style={{ animation: "fly-pulse-dot 1.2s ease infinite" }}
        />
      )}
    </button>
  );
}

function ViewSwitcher({
  view,
  setView,
  hasMeeting,
  enhanced,
  transcriptBusy,
  onEnhance,
  enhancing,
}: {
  view: View;
  setView: (v: View) => void;
  hasMeeting: boolean;
  enhanced: boolean;
  transcriptBusy: boolean;
  onEnhance: () => void;
  enhancing: boolean;
}) {
  return (
    <div className="print:hidden absolute bottom-5 left-1/2 z-[8] flex -translate-x-1/2 items-center gap-2.5">
      <div
        className="flex items-center gap-[3px] rounded-full border border-line bg-surface p-1"
        style={{ boxShadow: "var(--shadow-lg)" }}
      >
        {hasMeeting && (
          <SwitchSeg
            active={view === "transcript"}
            onClick={() => setView("transcript")}
            icon={<AudioWaveform size={16} strokeWidth={1.75} />}
            label="Transcript"
            busy={transcriptBusy}
          />
        )}
        <SwitchSeg
          active={view === "notes"}
          onClick={() => setView("notes")}
          icon={<List size={16} strokeWidth={1.75} />}
          label="Notes"
        />
        {enhanced && (
          <SwitchSeg
            active={view === "enhanced"}
            onClick={() => setView("enhanced")}
            icon={<Sparkles size={16} strokeWidth={1.75} />}
            label="Enhanced"
          />
        )}
      </div>
      {view === "notes" && (
        <button
          onClick={onEnhance}
          disabled={enhancing}
          title={enhanced ? "Re-enhance with AI" : "Enhance with AI"}
          className="inline-flex items-center gap-1.5 whitespace-nowrap rounded-full border px-4 py-2.5 text-[12.5px] font-semibold"
          style={{
            background: "var(--primary-soft)",
            color: "var(--primary-soft-text)",
            borderColor: "var(--primary-border)",
            boxShadow: "var(--shadow-lg)",
            cursor: enhancing ? "default" : "pointer",
            opacity: enhancing ? 0.7 : 1,
          }}
        >
          <Sparkles size={16} strokeWidth={1.75} />
          {enhancing ? "Enhancing…" : enhanced ? "Re-enhance" : "Enhance"}
        </button>
      )}
    </div>
  );
}

export default function Editor({
  note,
  meeting,
  meetings,
  onSelectMeeting,
  transcript,
  cleanedTranscript,
  pipeStage,
  pipeDetail,
  pipelineError,
  modelProgress,
  engine,
  engineInstalling,
  recStatus,
  screenStatus,
  folders,
  templates,
  dataDir,
  onNoteChanged,
  onMeetingChanged,
  onTranscriptRestored,
  onMoveNote,
  onStartRecording,
  onStartScreen,
  onStopScreen,
  onTranscribe,
  onInstallEngine,
  onOpenSettings,
  onRelabel,
  onEditSegment,
}: Props) {
  const [title, setTitle] = useState(note.title);
  const [scratchpad, setScratchpad] = useState(note.scratchpad);
  // Bumped when the editable content must reload (note switch / external insert);
  // NOT bumped on the user's own typing, so the caret never jumps.
  const [notesRev, setNotesRev] = useState(0);
  const [saveState, setSaveState] = useState<"saved" | "saving">("saved");
  const [view, setView] = useState<View>(() => (note.blocks.length > 0 ? "enhanced" : "notes"));
  const [templateId, setTemplateId] = useState("tpl-general");
  const [enhancing, setEnhancing] = useState(false);
  const [enhanceError, setEnhanceError] = useState<string | null>(null);
  const [askOpen, setAskOpen] = useState(false);
  const [zoomIds, setZoomIds] = useState<string[]>([]);
  // Poster rel paths keyed by attachment id — doubles as a session cache when
  // switching between notes. Requested ids are tracked so each video is asked
  // for at most once per session.
  const [videoThumbs, setVideoThumbs] = useState<Record<string, string>>({});
  const thumbReqs = useRef<Set<string>>(new Set());
  const [lightboxAtt, setLightboxAtt] = useState<Attachment | null>(null);
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");
  const [exportState, setExportState] = useState<"idle" | "saved" | "failed">("idle");
  const saveTimer = useRef<number | null>(null);
  const noteIdRef = useRef(note.id);

  const enhanced = note.blocks.length > 0;
  const isRecordingThisNote = recStatus.active && recStatus.note_id === note.id;
  const hasMeeting = meeting != null || (isRecordingThisNote && recStatus.meeting_id != null);

  // Swap editor contents when a different note is opened.
  useEffect(() => {
    if (noteIdRef.current !== note.id) {
      noteIdRef.current = note.id;
      setTitle(note.title);
      setScratchpad(note.scratchpad);
      setNotesRev((r) => r + 1);
      setSaveState("saved");
      setView(note.blocks.length > 0 ? "enhanced" : "notes");
      setEnhanceError(null);
      setAskOpen(false);
      setZoomIds([]);
      setLightboxAtt(null);
    }
  }, [note]);

  // Lazily create missing poster frames (ffmpeg sidecar, native side) when a
  // note's videos are first shown; failures just keep the CSS placeholder.
  useEffect(() => {
    for (const v of note.attachments.filter(isVideo)) {
      if (thumbReqs.current.has(v.id)) continue;
      thumbReqs.current.add(v.id);
      api
        .ensureVideoThumbnail(v.rel_path)
        .then((rel) => {
          if (rel) setVideoThumbs((m) => ({ ...m, [v.id]: rel }));
        })
        .catch(() => {});
    }
  }, [note.attachments]);

  // Keep the active view valid for this note.
  useEffect(() => {
    if (view === "transcript" && !hasMeeting) setView("notes");
    if (view === "enhanced" && !enhanced) setView("notes");
  }, [note.id, hasMeeting, enhanced, view]);

  // External scratchpad changes (e.g. "insert into note" from Ask).
  useEffect(() => {
    if (noteIdRef.current === note.id && note.scratchpad !== scratchpad && saveState === "saved") {
      setScratchpad(note.scratchpad);
      setNotesRev((r) => r + 1);
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

  const onNotesChange = (md: string) => {
    setScratchpad(md);
    scheduleSave(title, md);
  };

  // Copy the whole note as plain markdown (the same flattened document that
  // lives in the notes/ folder on disk). The clipboard write happens on the
  // native side (webview clipboard APIs are patchy); feedback is inline on
  // the button.
  const copyTimer = useRef<number | null>(null);
  const copyMarkdown = async () => {
    try {
      await api.copyNoteMarkdown(note.id);
      setCopyState("copied");
    } catch (e) {
      console.error("copy markdown failed", e);
      setCopyState("failed");
    }
    if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
    copyTimer.current = window.setTimeout(() => setCopyState("idle"), 1800);
  };

  // Save-as copy of the note's markdown mirror (native save dialog).
  const exportTimer = useRef<number | null>(null);
  const exportMarkdown = async () => {
    let next: "idle" | "saved" | "failed";
    try {
      const path = await api.exportNote(note.id);
      if (path == null) return; // user cancelled the dialog — no feedback
      next = "saved";
    } catch (e) {
      console.error("export markdown failed", e);
      next = "failed";
    }
    setExportState(next);
    if (exportTimer.current !== null) window.clearTimeout(exportTimer.current);
    exportTimer.current = window.setTimeout(() => setExportState("idle"), 1800);
  };

  const insertFromAsk = (content: string) => {
    const next = scratchpad ? `${scratchpad}\n\n${content}` : content;
    setScratchpad(next);
    setNotesRev((r) => r + 1);
    scheduleSave(title, next);
  };

  const zoom = (segmentIds: string[]) => {
    setZoomIds(segmentIds);
    if (hasMeeting) setView("transcript");
  };

  // "Someone else…" in the transcript speaker dropdown: add the person to the
  // attendee list AND assign them to the speaker in one step. The attendee
  // update marks the list user-confirmed (they're telling us who was there).
  const assignNewAttendee = async (speakerKey: string, name: string) => {
    if (!meeting) return;
    onRelabel(speakerKey, name);
    const already = meeting.attendees.some(
      (a) => (a.name.trim() || a.email || "").toLowerCase() === name.toLowerCase(),
    );
    if (!already && name.toLowerCase() !== "you") {
      try {
        const updated = await api.updateMeetingAttendees(meeting.id, [
          ...meeting.attendees,
          { name },
        ]);
        onMeetingChanged(updated);
      } catch (e) {
        console.error("adding attendee failed", e);
      }
    }
  };

  const rec = meeting?.recording ?? null;
  // full-quality playback mix first; older recordings fall back to the
  // 16 kHz ASR mixdown, then raw channels
  const audioPath =
    rec?.playback_path || rec?.mixed_path || rec?.mic_path || rec?.system_path || null;
  // Absolute path → asset URL so the <audio> element can stream it in-app.
  const audioSrc =
    dataDir && audioPath
      ? convertFileSrc(
          `${dataDir.replace(/[\\/]+$/, "")}/${audioPath.replace(/^[\\/]+/, "")}`.replace(
            /\\/g,
            "/",
          ),
        )
      : null;
  const videos = note.attachments.filter(isVideo);
  const playAudio = () => {
    if (audioPath) void api.openAttachment(audioPath);
  };
  const openAttachmentRel = (relPath: string) => void api.openAttachment(relPath);
  const videoThumbSrc = (att: Attachment) =>
    videoThumbs[att.id] ? toAssetSrc(dataDir, videoThumbs[att.id]) : null;
  const openVideo = (att: Attachment) => {
    // No data dir → no asset URL possible; keep the old system-player path.
    if (!dataDir) {
      void api.openAttachment(att.rel_path);
      return;
    }
    setLightboxAtt(att);
  };
  const showInFolder = () => {
    const p = rec?.mixed_path || rec?.mic_path || note.attachments[0]?.rel_path || null;
    if (p) void api.revealAttachment(p);
    else void api.revealDataDir();
  };

  const showTranscript = view === "transcript" && hasMeeting;
  const showEnhanced = view === "enhanced" && enhanced;
  const showNotes = !showTranscript && !showEnhanced;

  return (
    <div className="flex h-full min-w-0 flex-1">
      <div className="relative flex min-w-0 flex-1 flex-col bg-surface">
        {/* controls bar */}
        <div className="print:hidden flex flex-wrap items-center justify-end gap-2 border-b border-line px-6 py-2.5">
          {!recStatus.active && (
            <Button
              variant="record"
              size="sm"
              onClick={onStartRecording}
              title="Record this meeting (mic + system audio)"
              startIcon={<span className="h-2 w-2 rounded-full bg-white" />}
            >
              Record
            </Button>
          )}
          <ScreenControl
            screenStatus={screenStatus}
            onStartScreen={onStartScreen}
            onStopScreen={onStopScreen}
          />
          <Button
            variant={askOpen ? "soft" : "outline"}
            size="sm"
            onClick={() => setAskOpen((o) => !o)}
            title="Ask questions about this meeting"
            startIcon={<MessageSquare size={13} strokeWidth={1.75} />}
          >
            Ask
          </Button>
          <span className="h-5 w-px" style={{ background: "var(--line)" }} />
          <Button
            variant="ghost"
            size="sm"
            title="Attach file"
            onClick={() => void attach()}
            className="!px-2"
          >
            <Paperclip size={15} strokeWidth={1.75} />
          </Button>
          <Button
            variant="ghost"
            size="sm"
            title="Copy this note as Markdown"
            onClick={() => void copyMarkdown()}
            className="!px-2"
            style={
              copyState === "copied"
                ? { color: "var(--success-text)" }
                : copyState === "failed"
                  ? { color: "var(--error-text)" }
                  : undefined
            }
          >
            {copyState === "copied" ? (
              <>
                <Check size={15} strokeWidth={2} /> Copied
              </>
            ) : copyState === "failed" ? (
              <>
                <Copy size={15} strokeWidth={1.75} /> Copy failed
              </>
            ) : (
              <Copy size={15} strokeWidth={1.75} />
            )}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            title="Save this note as a Markdown file"
            onClick={() => void exportMarkdown()}
            className="!px-2"
            style={
              exportState === "saved"
                ? { color: "var(--success-text)" }
                : exportState === "failed"
                  ? { color: "var(--error-text)" }
                  : undefined
            }
          >
            {exportState === "saved" ? (
              <>
                <Check size={15} strokeWidth={2} /> Saved
              </>
            ) : exportState === "failed" ? (
              <>
                <FileDown size={15} strokeWidth={1.75} /> Save failed
              </>
            ) : (
              <FileDown size={15} strokeWidth={1.75} />
            )}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            title="Print or save as PDF (only the note content prints)"
            onClick={() => window.print()}
            className="!px-2"
          >
            <Printer size={15} strokeWidth={1.75} />
          </Button>
          <span className="ml-0.5 w-14 text-right text-xs" style={{ color: "var(--text-3)" }}>
            {saveState === "saving" ? "saving…" : "saved"}
          </span>
        </div>

        {/* title + meta (fixed context above the toggled content) */}
        <div className="px-6 pb-3.5 pt-5">
          <div className="mx-auto max-w-[var(--content-max)]">
            <input
              value={title}
              onChange={(e) => {
                setTitle(e.target.value);
                scheduleSave(e.target.value, scratchpad);
              }}
              placeholder="Untitled"
              className="mb-3 w-full bg-transparent font-display text-[28px] font-bold tracking-[-0.02em] text-text outline-none placeholder:text-text-3"
            />
            <MetaRow
              note={note}
              meeting={meeting}
              hasTranscript={transcript != null}
              folders={folders}
              templates={templates}
              templateId={templateId}
              setTemplateId={setTemplateId}
              onMoveNote={onMoveNote}
              onMeetingChanged={onMeetingChanged}
              onTranscriptRestored={onTranscriptRestored}
              onShowInFolder={showInFolder}
              onOpenAttachment={openAttachmentRel}
              onRemoveAttachment={(id) => void removeAttachment(id)}
            />
          </div>
        </div>

        {/* single content region — one view at a time */}
        <div className="min-h-0 flex-1 overflow-y-auto">
          <div
            className={`mx-auto max-w-[var(--content-max)] px-6 ${showNotes ? "" : "pb-28 pt-5"}`}
          >
            {enhanceError && (
              <div
                className="mt-4 mb-4 rounded-xl border border-line px-4 py-3 text-[13px]"
                style={{ background: "var(--error-soft)", color: "var(--error-text)" }}
              >
                {enhanceError}
              </div>
            )}

            {/* Transcript view — live pane stays mounted while recording so it keeps
                listening; it's only shown when the Transcript view is active. */}
            {isRecordingThisNote && recStatus.meeting_id && (
              <div style={{ display: showTranscript ? "block" : "none" }}>
                <LivePane meetingId={recStatus.meeting_id} />
              </div>
            )}
            {showTranscript && !isRecordingThisNote && meeting && (
              <div>
                {meetings.length > 1 && (
                  <div className="mb-3.5 flex flex-wrap items-center gap-1.5">
                    <span
                      className="text-[10.5px] font-semibold uppercase"
                      style={{ letterSpacing: ".07em", color: "var(--text-3)" }}
                    >
                      Recordings
                    </span>
                    {meetings.map((m, i) => {
                      const active = m.id === meeting.id;
                      return (
                        <button
                          key={m.id}
                          onClick={() => !active && onSelectMeeting(m)}
                          title={
                            active
                              ? "Currently shown"
                              : "Show this recording's transcript and audio"
                          }
                          className={`inline-flex cursor-pointer items-center gap-1.5 rounded-full border px-2.5 py-1 text-[12px] font-semibold ${
                            active
                              ? "border-transparent bg-primary text-on-primary"
                              : "border-line bg-surface-2 text-text-2 hover:bg-surface-3"
                          }`}
                        >
                          <Mic size={11} strokeWidth={1.75} />
                          {fmtWhen(m.started_at)}
                          {i === 0 ? " · latest" : ""}
                        </button>
                      );
                    })}
                  </div>
                )}
                {audioPath && (
                  <AudioPlayer
                    src={audioSrc}
                    durationMs={rec?.duration_ms ?? 0}
                    onFallback={playAudio}
                  />
                )}
                {videos.length > 0 && (
                  <VideoStrip videos={videos} thumbFor={videoThumbSrc} onOpen={openVideo} />
                )}
                <TranscriptPanel
                  meeting={meeting}
                  transcript={transcript}
                  cleaned={cleanedTranscript}
                  stage={pipeStage}
                  stageDetail={pipeDetail}
                  modelProgress={modelProgress}
                  pipelineError={pipelineError}
                  engine={engine}
                  engineInstalling={engineInstalling}
                  highlightIds={zoomIds}
                  onTranscribe={onTranscribe}
                  onInstallEngine={onInstallEngine}
                  onOpenSettings={onOpenSettings}
                  onRelabel={onRelabel}
                  onAssignNewAttendee={(key, name) => void assignNewAttendee(key, name)}
                  onEditSegment={onEditSegment}
                />
              </div>
            )}

            {/* Enhanced view */}
            {showEnhanced && (
              <EnhancedDoc note={note} onNoteChanged={onNoteChanged} onZoom={zoom} />
            )}
          </div>

          {/* Notes view — full-width (format bar spans, editable is centered);
              stays mounted via a display toggle so content/caret survive view switches. */}
          <div style={{ display: showNotes ? "block" : "none" }}>
            <NotesEditor
              markdown={scratchpad}
              revision={notesRev}
              onChange={onNotesChange}
              placeholder="Write your notes… they'll merge with the transcript when you enhance."
            />
          </div>
        </div>

        {/* floating view switcher (Granola-style) */}
        <ViewSwitcher
          view={view}
          setView={setView}
          hasMeeting={hasMeeting}
          enhanced={enhanced}
          transcriptBusy={!!pipeStage || isRecordingThisNote}
          onEnhance={() => void enhance()}
          enhancing={enhancing}
        />
      </div>

      {askOpen && (
        <AskPanel noteId={note.id} onInsert={insertFromAsk} onClose={() => setAskOpen(false)} />
      )}

      {lightboxAtt && (
        <VideoLightbox
          att={lightboxAtt}
          src={toAssetSrc(dataDir, lightboxAtt.rel_path)}
          poster={videoThumbSrc(lightboxAtt)}
          onClose={() => setLightboxAtt(null)}
        />
      )}
    </div>
  );
}
