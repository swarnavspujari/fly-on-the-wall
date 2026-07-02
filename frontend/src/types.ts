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

export interface RecordingRef {
  mic_path: string | null;
  system_path: string | null;
  mixed_path: string | null;
  duration_ms: number;
}

export interface Meeting {
  id: string;
  title: string;
  note_id: string;
  attendees: string[];
  started_at: string;
  ended_at: string | null;
  recording: RecordingRef | null;
}

export interface RecordingStatus {
  active: boolean;
  state: "recording" | "paused" | "stopped" | null;
  elapsed_ms: number;
  meeting_id: string | null;
  note_id: string | null;
}

export interface Word {
  text: string;
  start_ms: number;
  end_ms: number;
}

export interface TranscriptSegment {
  id: string;
  speaker_key: string;
  start_ms: number;
  end_ms: number;
  text: string;
  words: Word[];
}

export interface Speaker {
  key: string;
  label: string;
}

export interface Transcript {
  meeting_id: string;
  language: string | null;
  engine: string;
  segments: TranscriptSegment[];
  speakers: Speaker[];
}

export interface PipelineProgress {
  meeting_id: string;
  stage: string;
  detail: string | null;
  done: boolean;
  error: string | null;
}

export interface ModelProgress {
  id: string;
  downloaded: number;
  total: number;
  stage: "downloading" | "verifying" | "extracting" | "done" | "error";
  error: string | null;
}

export interface HwInfo {
  ram_gb: number;
  cpu_cores: number;
  gpu_name: string | null;
  vram_mb: number | null;
  recommended_tier: string;
}

export interface ModelStatus {
  id: string;
  display: string;
  bytes: number;
  installed: boolean;
}

export interface AsrSettings {
  tier: string;
  model_id: string | null;
  use_groq: boolean;
  max_quality: boolean;
  has_groq_key: boolean;
  auto_transcribe: boolean;
  hw: HwInfo;
  models: ModelStatus[];
}

export interface AsrSettingsUpdate {
  tier: string;
  model_id: string | null;
  use_groq: boolean;
  max_quality: boolean;
  auto_transcribe: boolean;
  groq_key: string | null;
}

export interface Template {
  id: string;
  name: string;
  system_prompt: string;
  structure_hint: string;
  built_in: boolean;
}

export interface LlmProviderInfo {
  id: string;
  default_model: string;
  is_local: boolean;
  has_key: boolean;
  model: string | null;
  base_url: string | null;
}

export interface LlmSettings {
  provider: string;
  providers: LlmProviderInfo[];
}

export interface LlmSettingsUpdate {
  provider: string;
  model: string | null;
  base_url: string | null;
  api_key: string | null;
}

export interface AskMessage {
  role: "user" | "assistant";
  content: string;
}
