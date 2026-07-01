// Typed wrappers over the Tauri command surface. Components never call
// invoke() directly — everything crosses this one seam.

import { invoke } from "@tauri-apps/api/core";
import type { AppInfo, Folder, Note, NoteSummary, SearchHit } from "./types";

export const api = {
  ping: () => invoke<string>("ping"),
  appInfo: () => invoke<AppInfo>("app_info"),

  // folders
  listFolders: () => invoke<Folder[]>("list_folders"),
  createFolder: (name: string, parentId: string | null) =>
    invoke<Folder>("create_folder", { name, parentId }),
  renameFolder: (id: string, name: string) => invoke<void>("rename_folder", { id, name }),
  moveFolder: (id: string, parentId: string | null) =>
    invoke<void>("move_folder", { id, parentId }),
  deleteFolder: (id: string) => invoke<void>("delete_folder", { id }),

  // notes
  createNote: (title: string, folderId: string | null) =>
    invoke<Note>("create_note", { title, folderId }),
  getNote: (id: string) => invoke<Note>("get_note", { id }),
  listNotesInFolder: (folderId: string | null) =>
    invoke<NoteSummary[]>("list_notes_in_folder", { folderId }),
  listRecentNotes: (limit: number) => invoke<NoteSummary[]>("list_recent_notes", { limit }),
  updateNoteTitle: (id: string, title: string) => invoke<Note>("update_note_title", { id, title }),
  updateNoteScratchpad: (id: string, scratchpad: string) =>
    invoke<Note>("update_note_scratchpad", { id, scratchpad }),
  moveNote: (id: string, folderId: string | null) => invoke<void>("move_note", { id, folderId }),
  deleteNote: (id: string) => invoke<void>("delete_note", { id }),

  // attachments & files
  attachFile: (noteId: string) => invoke<Note | null>("attach_file", { noteId }),
  removeAttachment: (noteId: string, attachmentId: string) =>
    invoke<Note>("remove_attachment", { noteId, attachmentId }),
  openAttachment: (relPath: string) => invoke<void>("open_attachment", { relPath }),
  revealAttachment: (relPath: string) => invoke<void>("reveal_attachment", { relPath }),
  revealDataDir: () => invoke<void>("reveal_data_dir"),

  // search
  search: (query: string) => invoke<SearchHit[]>("search", { query }),
};
