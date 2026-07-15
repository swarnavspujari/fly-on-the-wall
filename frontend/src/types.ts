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
  /** "windows" | "macos" | "linux" — auto-update is gated on this. */
  os: string;
}

export interface RecordingRef {
  mic_path: string | null;
  system_path: string | null;
  mixed_path: string | null;
  /** Full-quality playback mix; null on recordings made before it existed. */
  playback_path?: string | null;
  duration_ms: number;
}

/** One participant besides the user. Calendar-seeded attendees start with
 * name = email; renaming keeps the email so the next event still matches. */
export interface Attendee {
  name: string;
  email?: string | null;
}

export interface Meeting {
  id: string;
  title: string;
  note_id: string;
  attendees: Attendee[];
  /** True once the user confirmed the list in the attendee editor — only a
   * confirmed count may drive diarization. */
  attendees_confirmed: boolean;
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
  warnings: string[];
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

/** One segment the drop-content guard refused to clean (its raw text was
 * kept). Surfaced so the UI can mark lines that couldn't be safely polished —
 * a flagged segment is never a silent word loss. */
export interface PolishFlag {
  segment_id: string;
  speaker_key: string;
  /** Content length in non-whitespace characters (language-agnostic). */
  raw_chars: number;
  cleaned_chars: number;
  reason: string;
}

/** Result of a `polishTranscript` run. `transcript` is the cleaned variant —
 * same segment ids / speaker keys / timestamps as the raw one (so provenance
 * citations still resolve); only segment text differs. */
export interface PolishResult {
  transcript: Transcript;
  segments_total: number;
  segments_cleaned: number;
  segments_kept_raw: number;
  flags: PolishFlag[];
}

/** Result of a "Re-analyze speakers" run. */
export interface ReDiarizeOutcome {
  changed_segments: number;
  transcript: Transcript;
}

/** Undo availability for the last re-diarize (null = nothing to revert). */
export interface SpeakerUndoState {
  taken_at: string;
  changed_segments: number;
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

export interface GpuBench {
  verdict: string; // "gpu" | "cpu"
  reason: string;
  gpu_secs: number | null;
  cpu_secs: number | null;
  model_id: string;
}

export interface AsrSettings {
  tier: string;
  model_id: string | null;
  use_groq: boolean;
  max_quality: boolean;
  has_groq_key: boolean;
  auto_transcribe: boolean;
  use_gpu: boolean;
  gpu_bench: GpuBench | null;
  hw: HwInfo;
  models: ModelStatus[];
}

export interface AsrSettingsUpdate {
  tier: string;
  model_id: string | null;
  use_groq: boolean;
  max_quality: boolean;
  auto_transcribe: boolean;
  use_gpu: boolean;
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

export interface OllamaModel {
  /** Full name with tag ("llama3.1:latest", "qwen3.5:4b", …). */
  name: string;
  /** On-disk size in bytes. */
  size: number;
}

export interface OllamaStatus {
  /** An Ollama executable is available (managed install or on PATH). */
  installed: boolean;
  /** This OS has a managed download (show the Install button). */
  can_install: boolean;
  running: boolean;
  /** The running server is a child this app spawned. */
  managed: boolean;
  base_url: string;
  /** Installed models (name + size) when the server is running. */
  models: OllamaModel[];
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

export interface CalendarEvent {
  id: string;
  provider: "google" | "msgraph";
  title: string;
  start: string;
  end: string;
  attendees: string[];
  join_url: string | null;
}

export interface CalendarStatus {
  google_client_id: string;
  google_has_secret: boolean;
  google_connected: boolean;
  /** Connectable — usable creds exist (user-supplied OR bundled default). */
  google_configured: boolean;
  ms_client_id: string;
  ms_connected: boolean;
  ms_configured: boolean;
}

/** One of the user's calendars plus whether it feeds "Up next". */
export interface CalendarToggle {
  provider: "google" | "msgraph";
  id: string;
  name: string;
  primary: boolean;
  enabled: boolean;
}

export interface CalendarSettingsUpdate {
  google_client_id: string;
  google_client_secret: string | null;
  ms_client_id: string;
}

export type CaptureTarget =
  | { kind: "full_screen" }
  | { kind: "window"; title: string }
  | { kind: "region"; x: number; y: number; width: number; height: number };

export interface ScreenStatus {
  active: boolean;
  note_id: string | null;
  elapsed_ms: number;
}

export interface ImportResult {
  meeting: Meeting;
  note_id: string;
}

export interface AudioDevice {
  id: string;
  name: string;
  is_default: boolean;
}
