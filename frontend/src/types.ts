// Mirrors of the Rust domain types crossing the IPC boundary.

export interface Folder {
  id: string;
  name: string;
  parent_id: string | null;
  created_at: string;
}

export type BlockOrigin = { kind: "user" } | { kind: "ai"; source_segment_ids: string[] };

export interface NoteBlock {
  id: string;
  origin: BlockOrigin;
  markdown: string;
}

export interface Attachment {
  id: string;
  file_name: string;
  rel_path: string;
  mime: string | null;
  added_at: string;
}

export interface Note {
  id: string;
  title: string;
  folder_id: string | null;
  meeting_id: string | null;
  scratchpad: string;
  blocks: NoteBlock[];
  attachments: Attachment[];
  created_at: string;
  updated_at: string;
}

export interface NoteSummary {
  id: string;
  title: string;
  folder_id: string | null;
  meeting_id: string | null;
  updated_at: string;
}

export interface SearchHit {
  kind: "note" | "transcript";
  note_id: string;
  title: string;
  snippet: string;
  start_ms: number | null;
}

export interface AppInfo {
  version: string;
  data_dir: string;
}
