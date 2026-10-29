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
  /** When the note's meeting happened (started_at), else the note's
   * created_at — list order and the date shown in rows. */
  happened_at: string;
  /* List status (read-only, computed by the query). Optional: summaries
   * cached in localStorage before these existed lack them. */
  /** The note's meeting has a transcript. */
  has_transcript?: boolean;
  /** An enhanced doc exists (any blocks, including AI blocks the user kept). */
  has_enhanced_doc?: boolean;
  /** The meeting's audio is on disk (not removed by retention). */
  has_recording?: boolean;
  /** Transcription job: "queued" | "running" | "done" | "failed"; null = none. */
  job_status?: string | null;
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
  /** True when running from a Linux AppImage — the only Linux install the updater can replace. */
  appimage: boolean;
}

export interface RecordingRef {
  /** WAV, or lossless FLAC once the recording is compressed. */
  mic_path: string | null;
  system_path: string | null;
  /** 16 kHz ASR mixdown. null on a compressed recording (it regenerates). */
  mixed_path: string | null;
  /** Full-quality playback mix; null on recordings made before it existed
   *  and on compressed recordings (ask prepare_playback for the file). */
  playback_path?: string | null;
  duration_ms: number;
}

/** prepare_playback: the file the player streams (absolute path). */
export interface PlaybackSource {
  /** null until a compressed recording's mix is rebuilt (needs_build). */
  path: string | null;
  /** Call prepare_playback again with build=true (on Play). */
  needs_build: boolean;
}

/** The background "Compress recordings" job (storage:compress event). */
export interface CompressStatus {
  state: "waiting" | "working" | "paused" | "done" | "stopped" | "";
  paused_for: "recording" | "transcribing" | "importing" | null;
  meetings_total: number;
  meetings_done: number;
  /** Of the finished ones, this pass: left for a later pass (in use, just
   * played), and failed (originals kept; the latest reason is last_error). */
  meetings_skipped: number;
  meetings_failed: number;
  bytes_total: number;
  bytes_done: number;
  /** Freed so far, across every pass. */
  saved_bytes: number;
  /** WAVs left as they are (not in the layout capture writes). */
  kept_files: number;
  /** The one-time "recordings compressed" notice is due. */
  notice_pending: boolean;
  last_error: string | null;
}

/** Settings › Storage: media in the data folder, by kind. */
export interface StorageReport {
  audio_bytes: number;
  /** The part of audio_bytes still stored as uncompressed WAV. */
  wav_bytes: number;
  imported_video_bytes: number;
  imported_video_files: number;
  screen_recording_bytes: number;
  screen_recording_files: number;
  other_attachment_bytes: number;
  compress: CompressStatus;
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

/** macOS system-audio consent probe (`preflight_system_audio`). The verdict
 *  strings mirror fly-audio's `SystemAudioPreflight` serde tags. */
export interface SystemAudioPreflight {
  verdict: "ok" | "silent_while_playing" | "inconclusive" | "unsupported" | "error";
  detail?: string;
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

/** Managed-artifact id of the whisper.cpp engine — the ONE frontend copy,
 *  mirroring models::WHISPER_ENGINE_ID in the backend. */
export const WHISPER_ENGINE_ID = "whisper-bin";

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
  /** GPU transcription is possible on this machine at all — false on Intel
   *  Macs, where the toggle renders as unavailable. */
  gpu_available: boolean;
  gpu_bench: GpuBench | null;
  hw: HwInfo;
  models: ModelStatus[];
  /** whisper-cli engine is resolvable now (managed install or on PATH).
   *  Downloaded model weights can't transcribe without it. */
  engine_installed: boolean;
  /** This OS can install the engine in-app (a managed artifact exists);
   *  when false the UI shows manual install guidance instead of a button. */
  engine_managed: boolean;
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

/** A meeting self-heal restored and set to Keep local because its privacy
 *  setting couldn't be confirmed (privacy.rs RecoveredMeeting). */
export interface RecoveredMeeting {
  meeting_id: string;
  note_id: string;
  title: string;
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

export interface CalendarAttendee {
  email: string;
  name?: string;
  /** The signed-in user — never listed as a separate attendee. */
  is_self: boolean;
  declined: boolean;
}

export interface CalendarEvent {
  id: string;
  provider: "google" | "msgraph";
  title: string;
  start: string;
  end: string;
  attendees: CalendarAttendee[];
  join_url: string | null;
}

/** Calendar context handed to `start_recording` so a plain Record press
 *  still names the note after the meeting and prefills its attendees. */
export interface MeetingSeed {
  title: string | null;
  attendees: CalendarAttendee[];
}

/** Upcoming events + which connected providers need an interactive reconnect. */
export interface UpcomingMeetings {
  events: CalendarEvent[];
  needs_reconnect: ("google" | "msgraph")[];
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

export interface ImportFile {
  id: string;
  file_name: string;
  size: number;
  kind: "audio" | "video";
  /** Source copy inside the data dir (null for unsupported files). */
  rel_path: string | null;
  error: string | null;
  /** Why this file's audio couldn't be converted on the last Transcribe
   *  (the file stays staged, so Retry can try it again). */
  convert_error?: string | null;
}

/** A note created from imported media that hasn't been transcribed yet. */
export interface ImportStaged {
  note_id: string;
  meeting_id: string;
  files: ImportFile[];
  /** Set once transcription starts: cumulative end (ms) of each file on the
   * concatenated timeline, parallel to `files` — maps the pipeline's global
   * transcription % onto per-file progress. */
  boundaries_ms: number[];
  started: boolean;
}

/** Per-file conversion progress emitted while `import_transcribe` normalizes
 * the queue (ASR % rides the normal pipeline:progress stream afterwards). */
export interface ImportProgress {
  meeting_id: string;
  file_id: string;
  stage: "converting" | "converted" | "failed";
  /** With "failed": this file's cause (the rest of the batch carries on). */
  error?: string | null;
}

export interface AudioDevice {
  id: string;
  name: string;
  is_default: boolean;
}

/* ---- privacy controls (privacy.rs) ---- */

/** Per-meeting privacy state (`get_meeting_privacy`). */
export interface MeetingPrivacy {
  meeting_id: string;
  /** The note's own "Keep local" flag. */
  keep_local: boolean;
  /** A folder (this note's or a parent) forces keep-local. */
  keep_local_from_folder: boolean;
  participants_notified: boolean;
  /** Set when the retention setting deleted this meeting's audio. */
  audio_deleted_at: string | null;
}

export interface WipeReport {
  removed: number;
  /** Data-folder paths that couldn't be deleted (locked / permissions). */
  failed: string[];
  secrets_failed: string[];
  restart_required: boolean;
  /** Every table emptied in place. */
  db_erased: boolean;
  /** Erased pages reached the file (false = another program, e.g. an MCP
   *  client, holds the database open and old content may remain). */
  db_checkpointed: boolean;
  /** Notes for the user (stale request discarded, DB held open, …). */
  notes: string[];
}

export interface PrivacySettings {
  /** null = keep audio forever (default). */
  audio_retention_days: number | null;
  retention_choices: number[];
  /** Meetings a background sweep held back (too much audio at once). */
  retention_held: number | null;
  /** Leftovers of the last "Delete all my data" restart, if any. */
  last_wipe: WipeReport | null;
}

export interface SweepReport {
  meetings: number;
  files_removed: number;
  bytes_removed: number;
  meetings_failed: number;
  /** Why a background sweep held back, if it did. */
  held: string | null;
}

/** `library_status` and the `library:upgrade` event (src-tauri startup.rs).
 *  `done` / `total` are progress units — a fraction, not meetings; both are 0
 *  until the upgrade's first report. */
export interface LibraryStatus {
  phase: "ready" | "upgrading" | "failed";
  done: number;
  total: number;
}

export interface DiskEncryption {
  state: "on" | "off" | "unknown";
  method: "BitLocker" | "FileVault" | null;
  volume: string | null;
}
