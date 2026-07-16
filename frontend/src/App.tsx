import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { PanelLeft, X } from "lucide-react";
import { api } from "./api";
import { WHISPER_ENGINE_ID } from "./types";
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

/** Window width (px) under which the sidebars collapse into a menu button. */
const NARROW_BREAKPOINT = 880;

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
  // The LLM-polished variant (same segment ids/speakers/timestamps as the raw
  // transcript, cleaner text). Null until the cleanup pass has run.
  const [cleanedTranscript, setCleanedTranscript] = useState<Transcript | null>(null);
  const [pipeStage, setPipeStage] = useState<string | null>(null);
  const [pipeDetail, setPipeDetail] = useState<string | null>(null);
  const [pipelineError, setPipelineError] = useState<string | null>(null);
  const [modelProgress, setModelProgress] = useState<ModelProgress | null>(null);
  // Transcription-engine (whisper-cli) readiness — distinct from model
  // weights. `null` until the first settings fetch resolves.
  const [engine, setEngine] = useState<{ installed: boolean; managed: boolean } | null>(null);
  const [installingEngine, setInstallingEngine] = useState(false);
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
  // When Settings is opened from the transcribe error, deep-link to the engine
  // row; null for a normal open.
  const [settingsFocus, setSettingsFocus] = useState<"engine" | "groq" | null>(null);
  const [showFirstRun, setShowFirstRun] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Notepad mode: below this width both sidebars fold into a menu button and
  // only the note itself stays visible, so the app works docked in a corner.
  const [narrow, setNarrow] = useState(
    () => typeof window !== "undefined" && window.innerWidth < NARROW_BREAKPOINT,
  );
  const [drawerOpen, setDrawerOpen] = useState(false);

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

  // Re-read whisper-cli readiness (managed install or on PATH). Called after an
  // in-app install and whenever Settings closes, so a manual `brew install`
  // outside the app is reflected too. ONE fetch-and-set for every AsrSettings
  // field the app mirrors — a new field added here updates every caller.
  const refreshAsrState = useCallback(() => {
    api
      .getAsrSettings()
      .then((s) => {
        setAutoTranscribe(s.auto_transcribe);
        setEngine({ installed: s.engine_installed, managed: s.engine_managed });
      })
      .catch(console.error);
  }, []);

  const firstNotesLogged = useRef(false);
  // A slow fetch for a previous selection must not overwrite a newer one.
  const notesSeq = useRef(0);
  const refreshNotes = useCallback(async () => {
    const seq = ++notesSeq.current;
    const fresh = await (selection.view === "all"
      ? api.listRecentNotes(200)
      : api.listNotesInFolder(selection.view === "unfiled" ? null : selection.id));
    if (notesSeq.current !== seq) return;
    setNotes(fresh);
    if (selection.view === "all") writeNotesCache(fresh);
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
    refreshAsrState();
    api.listTemplates().then(setTemplates).catch(console.error);
    api
      .getAppSetting("consent.recording_notice_accepted")
      .then((v) => setShowFirstRun(v !== "true"))
      .catch(console.error);
  }, [refreshFolders, refreshAsrState]);

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

  // Collapse the sidebars into a menu button when the window gets narrow
  // (someone using the app as a small notepad on a corner of the screen).
  useEffect(() => {
    // Plain resize listener (not matchMedia): some webviews don't dispatch
    // media-query change events on window resize.
    const apply = () => {
      const isNarrow = window.innerWidth < NARROW_BREAKPOINT;
      setNarrow(isNarrow);
      if (!isNarrow) setDrawerOpen(false);
    };
    apply();
    window.addEventListener("resize", apply);
    return () => window.removeEventListener("resize", apply);
  }, []);

  // Ctrl+K (Cmd+K on macOS) opens Settings from anywhere.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setShowSettings(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
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
          void (async () => {
            const [raw, cleaned] = await Promise.all([
              api.getTranscript(p.meeting_id),
              api.getCleanedTranscript(p.meeting_id).catch(() => null),
            ]);
            // The user may have switched notes while these fetched.
            if (openMeetingIdRef.current !== p.meeting_id) return;
            setTranscript(raw);
            setCleanedTranscript(cleaned);
          })().catch(console.error);
        }
      } else {
        setPipeStage(p.stage);
        setPipeDetail(p.detail);
        setPipelineError(null);
        // By the polish stage the new raw transcript is already saved — fetch
        // it now so the user reads it while the AI cleanup runs.
        if (p.stage === "polishing") {
          api
            .getTranscript(p.meeting_id)
            .then((t) => {
              if (openMeetingIdRef.current === p.meeting_id) {
                setTranscript(t);
                setCleanedTranscript(null);
              }
            })
            .catch(console.error);
        }
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

  // Monotonic token: rapid note switches fire overlapping fetch chains, and
  // only the newest one may write state (otherwise note B can end up showing
  // note A's meeting/transcript).
  const openSeq = useRef(0);
  const openNoteById = useCallback(async (id: string) => {
    const seq = ++openSeq.current;
    const fresh = () => openSeq.current === seq;
    const note = await api.getNote(id);
    if (!fresh()) return;
    setOpenNote(note);
    const meeting = await api.getMeetingForNote(id);
    if (!fresh()) return;
    setOpenMeeting(meeting);
    setPipelineError(null);
    setPipeDetail(null);
    if (meeting) {
      const [raw, cleaned, stage] = await Promise.all([
        api.getTranscript(meeting.id),
        api.getCleanedTranscript(meeting.id).catch(() => null),
        api.pipelineStage(meeting.id),
      ]);
      if (!fresh()) return;
      setTranscript(raw);
      setCleanedTranscript(cleaned);
      setPipeStage(stage);
    } else {
      setTranscript(null);
      setCleanedTranscript(null);
      setPipeStage(null);
    }
  }, []);

  const newNote = async () => {
    const folderId = selection.view === "folder" ? selection.id : null;
    const note = await api.createNote("Untitled", folderId);
    openSeq.current++; // invalidate any in-flight openNoteById
    await refreshNotes();
    setOpenNote(note);
    setOpenMeeting(null);
    setTranscript(null);
    setCleanedTranscript(null);
    setPipeStage(null);
    setPipeDetail(null);
  };

  const deleteNote = async (id: string) => {
    await api.deleteNote(id);
    if (openNote?.id === id) {
      openSeq.current++;
      setOpenNote(null);
      setOpenMeeting(null);
      setTranscript(null);
      setCleanedTranscript(null);
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

  // One-click engine install from the transcribe error / Settings. Progress
  // arrives on the shared `model:progress` stream (id "whisper-bin").
  const installEngine = useCallback(async () => {
    setInstallingEngine(true);
    setPipelineError(null);
    try {
      await api.downloadModel(WHISPER_ENGINE_ID);
      refreshAsrState();
    } catch (e) {
      // Tag the failure so the notice selector keeps the actionable engine
      // notice even for non-download errors (e.g. a failed extraction).
      setPipelineError(`engine install failed: ${String(e)}`);
    } finally {
      setInstallingEngine(false);
      // Only clear the engine's own progress — `model:progress` is a shared
      // stream, and a concurrent pipeline download must keep its bar.
      setModelProgress((p) => (p && p.id === WHISPER_ENGINE_ID ? null : p));
    }
  }, [refreshAsrState]);

  const transcribeNow = async () => {
    if (!openMeeting) return;
    setPipelineError(null);
    setPipeStage("starting");
    setPipeDetail(null);
    await api.transcribeMeeting(openMeeting.id);
  };

  // Edits and renames apply to both variants backend-side; refresh the
  // cleaned copy so whichever view is showing reflects the change.
  const refreshCleaned = async (meetingId: string) => {
    const cleaned = await api.getCleanedTranscript(meetingId).catch(() => null);
    if (openMeetingIdRef.current === meetingId) setCleanedTranscript(cleaned);
  };

  // Attendee edits (editor Save / "Someone else…") return the updated meeting.
  const meetingChanged = (m: Meeting) => {
    if (openMeetingIdRef.current === m.id) setOpenMeeting(m);
  };

  // Undo of a re-diarize hands back the restored raw transcript; the cleaned
  // variant was restored backend-side too, so refetch it.
  const transcriptRestored = (t: Transcript) => {
    if (openMeetingIdRef.current !== t.meeting_id) return;
    setTranscript(t);
    void refreshCleaned(t.meeting_id);
  };

  const relabel = async (speakerKey: string, label: string) => {
    if (!openMeeting) return;
    const meetingId = openMeeting.id;
    const updated = await api.relabelSpeaker(meetingId, speakerKey, label);
    if (openMeetingIdRef.current === meetingId) setTranscript(updated);
    await refreshCleaned(meetingId);
  };

  const editSegment = async (segmentId: string, text: string) => {
    if (!openMeeting) return;
    const meetingId = openMeeting.id;
    try {
      const updated = await api.editTranscriptSegment(meetingId, segmentId, text);
      if (openMeetingIdRef.current === meetingId) setTranscript(updated);
      await refreshCleaned(meetingId);
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

  // Shared by the docked layout and the narrow-mode drawer.
  const sidebarEl = (
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
      onOpenSettings={() => {
        setDrawerOpen(false);
        setShowSettings(true);
      }}
      theme={resolved}
      onMoveNote={(id, fid) => void moveNoteToFolder(id, fid)}
    />
  );
  const noteListEl = (
    <NoteList
      notes={notes}
      searchQuery={searchQuery}
      searchHits={searchHits}
      selectedNoteId={openNote?.id ?? null}
      onSearchChange={setSearchQuery}
      onOpenNote={(id) => {
        setDrawerOpen(false);
        void openNoteById(id);
      }}
      onNewNote={() => {
        setDrawerOpen(false);
        void newNote();
      }}
      onDeleteNote={(id) => void deleteNote(id)}
    />
  );

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
      {narrow && (
        <div className="print:hidden flex items-center gap-2 border-b border-line bg-shell px-2 py-1.5">
          <button
            onClick={() => setDrawerOpen(true)}
            title="Show folders & notes"
            aria-label="Show folders & notes"
            className="inline-flex cursor-pointer items-center gap-1.5 rounded-lg border border-line bg-surface px-2.5 py-1.5 text-[12.5px] font-semibold text-text-2 hover:bg-surface-3 hover:text-text"
          >
            <PanelLeft size={15} strokeWidth={1.75} />
            Menu
          </button>
          <span className="min-w-0 truncate text-[13px] font-semibold text-text-2">
            {openNote?.title ?? "Fly on the Wall"}
          </span>
        </div>
      )}
      <div className="flex min-h-0 flex-1">
        {!narrow && sidebarEl}
        {!narrow && noteListEl}
        {openNote ? (
          <Editor
            note={openNote}
            meeting={openMeeting}
            transcript={transcript}
            cleanedTranscript={cleanedTranscript}
            pipeStage={pipeStage}
            pipeDetail={pipeDetail}
            pipelineError={pipelineError}
            modelProgress={modelProgress}
            engine={engine}
            engineInstalling={installingEngine}
            recStatus={recStatus}
            screenStatus={screenStatus}
            folders={folders}
            templates={templates}
            dataDir={info?.data_dir ?? null}
            onNoteChanged={onNoteChanged}
            onMeetingChanged={meetingChanged}
            onTranscriptRestored={transcriptRestored}
            onMoveNote={(folderId) => void moveOpenNote(folderId)}
            onStartRecording={() => void startRecording()}
            onStartScreen={(target) => void startScreen(target)}
            onStopScreen={() => void stopScreen()}
            onTranscribe={() => void transcribeNow()}
            onInstallEngine={() => void installEngine()}
            onOpenSettings={(focus) => {
              setSettingsFocus(focus);
              setShowSettings(true);
            }}
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
      {narrow && drawerOpen && (
        <div className="print:hidden fixed inset-0 z-40">
          <div
            className="absolute inset-0"
            style={{ background: "var(--overlay)" }}
            onClick={() => setDrawerOpen(false)}
            aria-hidden="true"
          />
          <div
            className="absolute left-0 top-0 flex h-full max-w-[92vw] overflow-x-auto border-r border-line bg-shell"
            style={{ boxShadow: "var(--shadow-lg)" }}
            role="dialog"
            aria-label="Folders and notes"
          >
            {sidebarEl}
            {noteListEl}
          </div>
        </div>
      )}
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
          initialFocus={settingsFocus}
          onClose={() => {
            setShowSettings(false);
            setSettingsFocus(null);
            refreshAsrState();
            api.listTemplates().then(setTemplates).catch(console.error);
            api.upcomingMeetings().then(setUpcoming).catch(console.error);
          }}
        />
      )}
    </div>
  );
}
