import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "./api";
import type {
  AppInfo,
  Folder,
  Meeting,
  ModelProgress,
  Note,
  NoteSummary,
  PipelineProgress,
  RecordingStatus,
  SearchHit,
  Template,
  Transcript,
} from "./types";
import Sidebar, { type Selection } from "./components/Sidebar";
import NoteList from "./components/NoteList";
import Editor from "./components/Editor";
import RecordingBar from "./components/RecordingBar";
import SettingsModal from "./components/SettingsModal";

const IDLE_STATUS: RecordingStatus = {
  active: false,
  state: null,
  elapsed_ms: 0,
  meeting_id: null,
  note_id: null,
};

export default function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [folders, setFolders] = useState<Folder[]>([]);
  const [selection, setSelection] = useState<Selection>({ view: "all" });
  const [notes, setNotes] = useState<NoteSummary[]>([]);
  const [openNote, setOpenNote] = useState<Note | null>(null);
  const [openMeeting, setOpenMeeting] = useState<Meeting | null>(null);
  const [transcript, setTranscript] = useState<Transcript | null>(null);
  const [pipeStage, setPipeStage] = useState<string | null>(null);
  const [pipelineError, setPipelineError] = useState<string | null>(null);
  const [modelProgress, setModelProgress] = useState<ModelProgress | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchHits, setSearchHits] = useState<SearchHit[]>([]);
  const [recStatus, setRecStatus] = useState<RecordingStatus>(IDLE_STATUS);
  const [autoTranscribe, setAutoTranscribe] = useState(true);
  const [templates, setTemplates] = useState<Template[]>([]);
  const [showSettings, setShowSettings] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const openMeetingIdRef = useRef<string | null>(null);
  useEffect(() => {
    openMeetingIdRef.current = openMeeting?.id ?? null;
  }, [openMeeting]);

  const refreshFolders = useCallback(async () => {
    setFolders(await api.listFolders());
  }, []);

  const refreshNotes = useCallback(async () => {
    if (selection.view === "all") {
      setNotes(await api.listRecentNotes(200));
    } else if (selection.view === "unfiled") {
      setNotes(await api.listNotesInFolder(null));
    } else {
      setNotes(await api.listNotesInFolder(selection.id));
    }
  }, [selection]);

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
  }, [refreshFolders]);

  useEffect(() => {
    refreshNotes().catch((e) => setError(String(e)));
  }, [refreshNotes]);

  // poll recording status once a second (recording indicator, elapsed time)
  useEffect(() => {
    const t = window.setInterval(() => {
      api.recordingStatus().then(setRecStatus).catch(console.error);
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
        setPipelineError(p.error);
        if (!p.error) {
          api.getTranscript(p.meeting_id).then(setTranscript).catch(console.error);
        }
      } else {
        setPipeStage(p.stage);
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
    await api.transcribeMeeting(openMeeting.id);
  };

  const relabel = async (speakerKey: string, label: string) => {
    if (!openMeeting) return;
    setTranscript(await api.relabelSpeaker(openMeeting.id, speakerKey, label));
  };

  const recordingNoteTitle =
    recStatus.note_id != null
      ? (notes.find((n) => n.id === recStatus.note_id)?.title ??
        (openNote?.id === recStatus.note_id ? openNote.title : null))
      : null;

  return (
    <div className="flex h-screen flex-col bg-zinc-950 text-zinc-100">
      <RecordingBar
        status={recStatus}
        noteTitle={recordingNoteTitle}
        onPause={() => void api.pauseRecording().then(setRecStatus).catch(console.error)}
        onResume={() => void api.resumeRecording().then(setRecStatus).catch(console.error)}
        onStop={() => void stopRecording()}
        onOpenNote={() => recStatus.note_id && void openNoteById(recStatus.note_id)}
      />
      <div className="flex min-h-0 flex-1">
        <Sidebar
          folders={folders}
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
          onOpenSettings={() => setShowSettings(true)}
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
            pipelineError={pipelineError}
            modelProgress={modelProgress}
            recStatus={recStatus}
            folders={folders}
            templates={templates}
            onNoteChanged={onNoteChanged}
            onMoveNote={(folderId) => void moveOpenNote(folderId)}
            onStartRecording={() => void startRecording()}
            onTranscribe={() => void transcribeNow()}
            onRelabel={(k, l) => void relabel(k, l)}
          />
        ) : (
          <div className="flex flex-1 flex-col items-center justify-center gap-4 bg-zinc-900 text-zinc-600">
            <div className="text-center">
              <div className="text-4xl font-semibold tracking-tight text-zinc-700">Looma</div>
              <div className="mt-2 text-sm">Select a note, create one, or start recording.</div>
            </div>
            {!recStatus.active && (
              <button
                onClick={() => void startRecording()}
                className="rounded-md bg-red-600 px-4 py-2 text-sm font-medium text-white hover:bg-red-500"
              >
                ● Record a meeting
              </button>
            )}
          </div>
        )}
      </div>
      <footer className="flex items-center justify-between border-t border-zinc-800 bg-zinc-950 px-4 py-1.5 text-xs text-zinc-600">
        <span>{error ? `⚠ ${error}` : "local-first · offline capable"}</span>
        {info && (
          <button
            className="hover:text-zinc-300"
            title="Reveal data folder in Explorer"
            onClick={() => void api.revealDataDir()}
          >
            v{info.version} · {info.data_dir}
          </button>
        )}
      </footer>
      {showSettings && (
        <SettingsModal
          modelProgress={modelProgress}
          onClose={() => {
            setShowSettings(false);
            api
              .getAsrSettings()
              .then((s) => setAutoTranscribe(s.auto_transcribe))
              .catch(console.error);
            api.listTemplates().then(setTemplates).catch(console.error);
          }}
        />
      )}
    </div>
  );
}
