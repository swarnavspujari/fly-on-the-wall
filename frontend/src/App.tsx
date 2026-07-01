import { useCallback, useEffect, useState } from "react";
import { api } from "./api";
import type { AppInfo, Folder, Note, NoteSummary, SearchHit } from "./types";
import Sidebar, { type Selection } from "./components/Sidebar";
import NoteList from "./components/NoteList";
import Editor from "./components/Editor";

export default function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [folders, setFolders] = useState<Folder[]>([]);
  const [selection, setSelection] = useState<Selection>({ view: "all" });
  const [notes, setNotes] = useState<NoteSummary[]>([]);
  const [openNote, setOpenNote] = useState<Note | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchHits, setSearchHits] = useState<SearchHit[]>([]);
  const [error, setError] = useState<string | null>(null);

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
  }, [refreshFolders]);

  useEffect(() => {
    refreshNotes().catch((e) => setError(String(e)));
  }, [refreshNotes]);

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

  const openNoteById = async (id: string) => {
    setOpenNote(await api.getNote(id));
  };

  const newNote = async () => {
    const folderId = selection.view === "folder" ? selection.id : null;
    const note = await api.createNote("Untitled", folderId);
    await refreshNotes();
    setOpenNote(note);
  };

  const deleteNote = async (id: string) => {
    await api.deleteNote(id);
    if (openNote?.id === id) setOpenNote(null);
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

  return (
    <div className="flex h-screen flex-col bg-zinc-950 text-zinc-100">
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
            folders={folders}
            onNoteChanged={onNoteChanged}
            onMoveNote={(folderId) => void moveOpenNote(folderId)}
          />
        ) : (
          <div className="flex flex-1 items-center justify-center bg-zinc-900 text-zinc-600">
            <div className="text-center">
              <div className="text-4xl font-semibold tracking-tight text-zinc-700">Looma</div>
              <div className="mt-2 text-sm">Select a note or create one to get started.</div>
            </div>
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
    </div>
  );
}
