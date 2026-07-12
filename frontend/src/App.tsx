import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { X } from "lucide-react";
import { api } from "./api";
import type {
  AppInfo,
  CalendarEvent,
  Folder,
  Meeting,
  ModelProgress,
  Note,
  CaptureTarget,
  NoteSummary,
  PipelineProgress,
  RecordingStatus,
  ScreenStatus,
  SearchHit,
  Template,
  Transcript,
} from "./types";
import { readNotesCache, writeNotesCache } from "./notesCache";
import { useTheme } from "./theme";
import logoLight from "./assets/brand/fly-on-the-wall-logo.svg";
import logoDark from "./assets/brand/fly-on-the-wall-logo-dark.svg";
import Sidebar, { type Selection } from "./components/Sidebar";
import NoteList from "./components/NoteList";
import Editor from "./components/Editor";
import RecordingBar from "./components/RecordingBar";
import SettingsModal from "./components/SettingsModal";
import FirstRunNotice from "./components/FirstRunNotice";
import UpdateBanner from "./components/UpdateBanner";
import { useUpdater } from "./updater";

const IDLE_STATUS: RecordingStatus = {
  active: false,
  state: null,
  elapsed_ms: 0,
  meeting_id: null,
  note_id: null,
  warnings: [],
};

export default function App() {
  // Theme: defaults to "system" and follows the OS (see src/theme.ts).
  const { resolved } = useTheme();
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [folders, setFolders] = useState<Folder[]>([]);
  const [selection, setSelection] = useState<Selection>({ view: "all" });
  // paint the last-known list instantly; reconciled by the first fetch
  const [notes, setNotes] = useState<NoteSummary[]>(readNotesCache);
  const [openNote, setOpenNote] = useState<Note | null>(null);
  const [openMeeting, setOpenMeeting] = useState<Meeting | null>(null);
  const [transcript, setTranscript] = useState<Transcript | null>(null);
  const [pipeStage, setPipeStage] = useState<string | null>(null);
  const [pipeDetail, setPipeDetail] = useState<string | null>(null);
  const [pipelineError, setPipelineError] = useState<string | null>(null);
  const [modelProgress, setModelProgress] = useState<ModelProgress | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchHits, setSearchHits] = useState<SearchHit[]>([]);
  const [recStatus, setRecStatus] = useState<RecordingStatus>(IDLE_STATUS);
  const [screenStatus, setScreenStatus] = useState<ScreenStatus>({
    active: false,
    note_id: null,
    elapsed_ms: 0,
  });
  // Human label of the current screen capture ("Full screen" / "Window" /
  // "Region"), shown in the recording bar's screen pill.
  const [screenSource, setScreenSource] = useState<string | null>(null);
  const [autoTranscribe, setAutoTranscribe] = useState(true);
  const [templates, setTemplates] = useState<Template[]>([]);
  const [upcoming, setUpcoming] = useState<CalendarEvent[]>([]);
  const [showSettings, setShowSettings] = useState(false);
  const [showFirstRun, setShowFirstRun] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Auto-update (Windows-only). Recording is sacred: while anything records,
  // the banner is unmounted and Settings disables install/restart.
  const recordingActive = recStatus.active || screenStatus.active;
  const updater = useUpdater(info?.os ?? null, recordingActive);

  const openMeetingIdRef = useRef<string | null>(null);
  useEffect(() => {
    openMeetingIdRef.current = openMeeting?.id ?? null;
  }, [openMeeting]);

  const refreshFolders = useCallback(async () => {
    setFolders(await api.listFolders());
  }, []);

  const firstNotesLogged = useRef(false);
  const refreshNotes = useCallback(async () => {
    if (selection.view === "all") {
      const fresh = await api.listRecentNotes(200);
      setNotes(fresh);
      writeNotesCache(fresh);
    } else if (selection.view === "unfiled") {
      setNotes(await api.listNotesInFolder(null));
    } else {
      setNotes(await api.listNotesInFolder(selection.id));
    }
    if (!firstNotesLogged.current) {
      firstNotesLogged.current = true;
      console.info(`[startup] fresh notes list at ${Math.round(performance.now())} ms`);
    }
  }, [selection]);

  // Notes first: this effect stays ahead of the others so the notes query
  // is the first invoke the backend sees on launch.
  useEffect(() => {
    refreshNotes().catch((e) => setError(String(e)));
  }, [refreshNotes]);

  useEffect(() => {
    api
      .appInfo()
      .then(setInfo)
      .catch((e) => setError(String(e)));
    refreshFolders().catch((e) => setError(String(e)));
    api
      .getAsrSettings()
      .then((s) => setAutoTranscribe(s.auto_transcribe))
      .catch(console.error);
    api.listTemplates().then(setTemplates).catch(console.error);
    api
      .getAppSetting("consent.recording_notice_accepted")
      .then((v) => setShowFirstRun(v !== "true"))
      .catch(console.error);
  }, [refreshFolders]);

  // upcoming calendar meetings: on start + every 5 minutes
  useEffect(() => {
    const fetchUpcoming = () => {
      api.upcomingMeetings().then(setUpcoming).catch(console.error);
    };
    fetchUpcoming();
    const t = window.setInterval(fetchUpcoming, 5 * 60_000);
    return () => window.clearInterval(t);
  }, []);

  // poll recording + screen status once a second (indicators, elapsed time)
  useEffect(() => {
    const t = window.setInterval(() => {
      api.recordingStatus().then(setRecStatus).catch(console.error);
      api.screenStatus().then(setScreenStatus).catch(console.error);
    }, 1000);
    return () => window.clearInterval(t);
  }, []);

  // pipeline + model download progress events
  useEffect(() => {
    const unPipeline = listen<PipelineProgress>("pipeline:progress", (e) => {
      const p = e.payload;
      if (p.meeting_id !== openMeetingIdRef.current) return;
      if (p.done) {
        setPipeStage(null);
        setPipeDetail(null);
        setPipelineError(p.error);
        if (!p.error) {
          api.getTranscript(p.meeting_id).then(setTranscript).catch(console.error);
        }
      } else {
        setPipeStage(p.stage);
        setPipeDetail(p.detail);
        setPipelineError(null);
      }
    });
    const unModel = listen<ModelProgress>("model:progress", (e) => setModelProgress(e.payload));
    return () => {
      void unPipeline.then((f) => f());
      void unModel.then((f) => f());
    };
  }, []);

  // debounce search-as-you-type
  useEffect(() => {
    const q = searchQuery.trim();
    if (!q) {
      setSearchHits([]);
      return;
    }
    const t = window.setTimeout(() => {
      api.search(q).then(setSearchHits).catch(console.error);
    }, 200);
    return () => window.clearTimeout(t);
  }, [searchQuery]);

  const openNoteById = useCallback(async (id: string) => {
    const note = await api.getNote(id);
    setOpenNote(note);
    const meeting = await api.getMeetingForNote(id);
    setOpenMeeting(meeting);
    setPipelineError(null);
    setPipeDetail(null);
    if (meeting) {
      setTranscript(await api.getTranscript(meeting.id));
      setPipeStage(await api.pipelineStage(meeting.id));
    } else {
      setTranscript(null);
      setPipeStage(null);
    }
  }, []);

  const newNote = async () => {
    const folderId = selection.view === "folder" ? selection.id : null;
    const note = await api.createNote("Untitled", folderId);
    await refreshNotes();
    setOpenNote(note);
    setOpenMeeting(null);
    setTranscript(null);
    setPipeStage(null);
    setPipeDetail(null);
  };

  const deleteNote = async (id: string) => {
    await api.deleteNote(id);
    if (openNote?.id === id) {
      setOpenNote(null);
      setOpenMeeting(null);
      setTranscript(null);
    }
    await refreshNotes();
  };

  const moveOpenNote = async (folderId: string | null) => {
    if (!openNote) return;
    await api.moveNote(openNote.id, folderId);
    setOpenNote({ ...openNote, folder_id: folderId });
    await refreshNotes();
  };

  // Drag-drop filing: a note dragged from the list onto a Sidebar folder / All notes.
  const moveNoteToFolder = async (noteId: string, folderId: string | null) => {
    await api.moveNote(noteId, folderId);
    if (openNote?.id === noteId) setOpenNote({ ...openNote, folder_id: folderId });
    await refreshNotes();
  };

  const onNoteChanged = (note: Note) => {
    setOpenNote(note);
    void refreshNotes();
  };

  const startRecording = async () => {
    try {
      const status = await api.startRecording(openNote?.id ?? null);
      setRecStatus(status);
      if (!openNote && status.note_id) {
        await refreshNotes();
        await openNoteById(status.note_id);
      } else if (openNote) {
        setOpenMeeting(await api.getMeetingForNote(openNote.id));
      }
    } catch (e) {
      setError(String(e));
    }
  };

  const stopRecording = async () => {
    try {
      const meeting = await api.stopRecording();
      setRecStatus(IDLE_STATUS);
      if (openNote?.id === meeting.note_id) {
        setOpenMeeting(meeting);
      }
      await refreshNotes();
      if (autoTranscribe) {
        await api.transcribeMeeting(meeting.id);
        if (openNote?.id === meeting.note_id) {
          setPipeStage("starting");
          setPipeDetail(null);
          setPipelineError(null);
        }
      }
    } catch (e) {
      setError(String(e));
    }
  };

  const transcribeNow = async () => {
    if (!openMeeting) return;
    setPipelineError(null);
    setPipeStage("starting");
    setPipeDetail(null);
    await api.transcribeMeeting(openMeeting.id);
  };

  const relabel = async (speakerKey: string, label: string) => {
    if (!openMeeting) return;
    setTranscript(await api.relabelSpeaker(openMeeting.id, speakerKey, label));
  };

  const editSegment = async (segmentId: string, text: string) => {
    if (!openMeeting) return;
    try {
      setTranscript(await api.editTranscriptSegment(openMeeting.id, segmentId, text));
    } catch (e) {
      setError(String(e));
    }
  };

  const startScreen = async (target: CaptureTarget) => {
    if (!openNote) return;
    try {
      setScreenStatus(await api.startScreenRecording(openNote.id, target));
      setScreenSource(
        target.kind === "full_screen"
          ? "Full screen"
          : target.kind === "window"
            ? "Window"
            : "Region",
      );
    } catch (e) {
      setError(String(e));
    }
  };

  const stopScreen = async () => {
    try {
      const updated = await api.stopScreenRecording();
      setScreenStatus({ active: false, note_id: null, elapsed_ms: 0 });
      setScreenSource(null);
      if (openNote?.id === updated.id) setOpenNote(updated);
    } catch (e) {
      setError(String(e));
    }
  };

  const startFromEvent = async (ev: CalendarEvent) => {
    try {
      const status = await api.startMeetingFromEvent(ev.title, ev.attendees);
      setRecStatus(status);
      await refreshNotes();
      if (status.note_id) await openNoteById(status.note_id);
    } catch (e) {
      setError(String(e));
    }
  };

  const recordingNoteTitle =
    recStatus.note_id != null
      ? (notes.find((n) => n.id === recStatus.note_id)?.title ??
        (openNote?.id === recStatus.note_id ? openNote.title : null))
      : null;

  return (
    <div className="flex h-screen flex-col bg-shell font-sans text-text">
      <RecordingBar
        status={recStatus}
        noteTitle={recordingNoteTitle}
        screenActive={screenStatus.active}
        screenSource={screenSource}
        onPause={() => void api.pauseRecording().then(setRecStatus).catch(console.error)}
        onResume={() => void api.resumeRecording().then(setRecStatus).catch(console.error)}
        onStop={() => void stopRecording()}
        onOpenNote={() => recStatus.note_id && void openNoteById(recStatus.note_id)}
      />
      <div className="flex min-h-0 flex-1">
        <Sidebar
          folders={folders}
          upcoming={upcoming}
          selection={selection}
          onSelect={setSelection}
          onCreateFolder={(name, parentId) =>
            void api.createFolder(name, parentId).then(refreshFolders)
          }
          onRenameFolder={(id, name) => void api.renameFolder(id, name).then(refreshFolders)}
          onDeleteFolder={(id) =>
            void api.deleteFolder(id).then(async () => {
              if (selection.view === "folder" && selection.id === id) {
                setSelection({ view: "all" });
              }
              await refreshFolders();
              await refreshNotes();
            })
          }
          onStartFromEvent={(ev) => void startFromEvent(ev)}
          onOpenSettings={() => setShowSettings(true)}
          theme={resolved}
          onMoveNote={(id, fid) => void moveNoteToFolder(id, fid)}
        />
        <NoteList
          notes={notes}
          searchQuery={searchQuery}
          searchHits={searchHits}
          selectedNoteId={openNote?.id ?? null}
          onSearchChange={setSearchQuery}
          onOpenNote={(id) => void openNoteById(id)}
          onNewNote={() => void newNote()}
          onDeleteNote={(id) => void deleteNote(id)}
        />
        {openNote ? (
          <Editor
            note={openNote}
            meeting={openMeeting}
            transcript={transcript}
            pipeStage={pipeStage}
            pipeDetail={pipeDetail}
            pipelineError={pipelineError}
            modelProgress={modelProgress}
            recStatus={recStatus}
            screenStatus={screenStatus}
            folders={folders}
            templates={templates}
            dataDir={info?.data_dir ?? null}
            onNoteChanged={onNoteChanged}
            onMoveNote={(folderId) => void moveOpenNote(folderId)}
            onStartRecording={() => void startRecording()}
            onStartScreen={(target) => void startScreen(target)}
            onStopScreen={() => void stopScreen()}
            onTranscribe={() => void transcribeNow()}
            onRelabel={(k, l) => void relabel(k, l)}
            onEditSegment={(id, text) => void editSegment(id, text)}
          />
        ) : (
          <div className="flex flex-1 flex-col items-center justify-center gap-5 bg-surface text-text-3">
            <div className="text-center">
              <img
                src={resolved === "dark" ? logoDark : logoLight}
                alt="Fly on the Wall"
                className="mx-auto block h-auto w-[260px] select-none"
                draggable={false}
              />
              <div className="mt-3 text-sm text-text-2">
                Select a note, create one, or start recording.
              </div>
            </div>
            {!recStatus.active && (
              <button
                onClick={() => void startRecording()}
                className="inline-flex cursor-pointer items-center gap-2.5 rounded-xl bg-primary px-5 py-3 text-sm font-semibold text-on-primary transition-[filter] hover:brightness-105"
              >
                <span className="h-3 w-3 rounded-full bg-on-primary" /> Start a meeting
              </button>
            )}
          </div>
        )}
      </div>
      {error && (
        <div
          className="print:hidden fixed bottom-4 left-4 z-50 flex w-80 items-start gap-2.5 rounded-2xl border border-line bg-error-soft px-4 py-3 text-[13px] leading-relaxed text-error-text"
          style={{ boxShadow: "var(--shadow-lg)" }}
          role="alert"
        >
          <span className="min-w-0 flex-1 break-words">⚠ {error}</span>
          <button
            onClick={() => setError(null)}
            title="Dismiss"
            aria-label="Dismiss error"
            className="flex-none cursor-pointer rounded p-0.5 opacity-70 hover:opacity-100"
          >
            <X size={14} strokeWidth={2} />
          </button>
        </div>
      )}
      {!recordingActive && <UpdateBanner updater={updater} />}
      {showFirstRun && (
        <FirstRunNotice
          onAccept={() => {
            setShowFirstRun(false);
            void api.setAppSetting("consent.recording_notice_accepted", "true");
          }}
        />
      )}
      {showSettings && (
        <SettingsModal
          modelProgress={modelProgress}
          updater={updater}
          recordingActive={recordingActive}
          appVersion={info?.version ?? null}
          onClose={() => {
            setShowSettings(false);
            api
              .getAsrSettings()
              .then((s) => setAutoTranscribe(s.auto_transcribe))
              .catch(console.error);
            api.listTemplates().then(setTemplates).catch(console.error);
            api.upcomingMeetings().then(setUpcoming).catch(console.error);
          }}
        />
      )}
    </div>
  );
}
