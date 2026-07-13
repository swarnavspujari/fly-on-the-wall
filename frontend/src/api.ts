// Typed wrappers over the Tauri command surface. Components never call
// invoke() directly — everything crosses this one seam.

import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  AskMessage,
  AudioDevice,
  CalendarEvent,
  CalendarSettingsUpdate,
  CalendarStatus,
  CalendarToggle,
  CaptureTarget,
  ImportResult,
  ScreenStatus,
  AsrSettings,
  AsrSettingsUpdate,
  Folder,
  LlmSettings,
  LlmSettingsUpdate,
  Meeting,
  Note,
  NoteSummary,
  OllamaStatus,
  PolishResult,
  RecordingStatus,
  SearchHit,
  Template,
  Transcript,
} from "./types";

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
  exportNote: (noteId: string) => invoke<string | null>("export_note", { noteId }),
  // Copies the note to the clipboard as plain markdown (native-side write;
  // returns the markdown that was copied).
  copyNoteMarkdown: (noteId: string) => invoke<string>("copy_note_markdown", { noteId }),
  removeAttachment: (noteId: string, attachmentId: string) =>
    invoke<Note>("remove_attachment", { noteId, attachmentId }),
  openAttachment: (relPath: string) => invoke<void>("open_attachment", { relPath }),
  // Lazily generates (native side, ffmpeg sidecar) and caches a poster .jpg
  // next to a video attachment; returns the poster's rel path.
  ensureVideoThumbnail: (relPath: string) => invoke<string>("ensure_video_thumbnail", { relPath }),
  revealAttachment: (relPath: string) => invoke<void>("reveal_attachment", { relPath }),
  revealDataDir: () => invoke<void>("reveal_data_dir"),
  mcpConfig: () => invoke<string>("mcp_config"),
  getAppSetting: (key: string) => invoke<string | null>("get_app_setting", { key }),
  setAppSetting: (key: string, value: string) => invoke<void>("set_app_setting", { key, value }),

  // search
  search: (query: string) => invoke<SearchHit[]>("search", { query }),

  // recording
  recordingStatus: () => invoke<RecordingStatus>("recording_status"),
  startRecording: (noteId: string | null) => invoke<RecordingStatus>("start_recording", { noteId }),
  pauseRecording: () => invoke<RecordingStatus>("pause_recording"),
  resumeRecording: () => invoke<RecordingStatus>("resume_recording"),
  stopRecording: () => invoke<Meeting>("stop_recording"),
  getMeetingForNote: (noteId: string) => invoke<Meeting | null>("get_meeting_for_note", { noteId }),

  // transcription
  transcribeMeeting: (meetingId: string) => invoke<void>("transcribe_meeting", { meetingId }),
  getTranscript: (meetingId: string) => invoke<Transcript | null>("get_transcript", { meetingId }),
  // The LLM-polished transcript variant (null until polishTranscript runs).
  getCleanedTranscript: (meetingId: string) =>
    invoke<Transcript | null>("get_cleaned_transcript", { meetingId }),
  // Re-runnable cleanup pass: produces + stores the polished variant alongside
  // the raw transcript (never overwrites raw). Returns per-run stats + flags.
  polishTranscript: (meetingId: string) => invoke<PolishResult>("polish_transcript", { meetingId }),
  relabelSpeaker: (meetingId: string, speakerKey: string, label: string) =>
    invoke<Transcript>("relabel_speaker", { meetingId, speakerKey, label }),
  editTranscriptSegment: (meetingId: string, segmentId: string, text: string) =>
    invoke<Transcript>("edit_transcript_segment", { meetingId, segmentId, text }),
  pipelineStage: (meetingId: string) => invoke<string | null>("pipeline_stage", { meetingId }),

  // ASR settings & models
  getAsrSettings: () => invoke<AsrSettings>("get_asr_settings"),
  setAsrSettings: (update: AsrSettingsUpdate) => invoke<void>("set_asr_settings", { update }),
  downloadModel: (id: string) => invoke<string>("download_model", { id }),

  // enhance / ask / templates / LLM settings
  enhanceNote: (noteId: string, templateId: string) =>
    invoke<Note>("enhance_note", { noteId, templateId }),
  editNoteBlock: (noteId: string, blockId: string, markdown: string) =>
    invoke<Note>("edit_note_block", { noteId, blockId, markdown }),
  askMeeting: (noteId: string, history: AskMessage[]) =>
    invoke<string>("ask_meeting", { noteId, history }),
  listTemplates: () => invoke<Template[]>("list_templates"),
  saveTemplate: (template: Template) => invoke<void>("save_template", { template }),
  deleteTemplate: (id: string) => invoke<void>("delete_template", { id }),
  getLlmSettings: () => invoke<LlmSettings>("get_llm_settings"),
  setLlmSettings: (update: LlmSettingsUpdate) => invoke<void>("set_llm_settings", { update }),
  testLlmConnection: () => invoke<string>("test_llm_connection"),
  // structured item extraction (decisions / action items / figures …)
  extractMeetingItems: (meetingId: string) =>
    invoke<number>("extract_meeting_items", { meetingId }),
  backfillMeetingItems: () =>
    invoke<{ processed: number; extracted: number; failed: number }>("backfill_meeting_items"),

  // managed Ollama (local AI provider)
  ollamaStatus: () => invoke<OllamaStatus>("ollama_status"),
  // Pulls a model into the local server; progress arrives as model:progress
  // events with id "ollama:<model>".
  ollamaPull: (model: string) => invoke<void>("ollama_pull", { model }),

  // calendars
  getCalendarSettings: () => invoke<CalendarStatus>("get_calendar_settings"),
  setCalendarSettings: (update: CalendarSettingsUpdate) =>
    invoke<void>("set_calendar_settings", { update }),
  connectCalendar: (provider: string) => invoke<void>("connect_calendar", { provider }),
  disconnectCalendar: (provider: string) => invoke<void>("disconnect_calendar", { provider }),
  listCalendars: () => invoke<CalendarToggle[]>("list_calendars"),
  setCalendarEnabled: (provider: string, calendarId: string, enabled: boolean) =>
    invoke<void>("set_calendar_enabled", { provider, calendarId, enabled }),
  upcomingMeetings: () => invoke<CalendarEvent[]>("upcoming_meetings"),
  startMeetingFromEvent: (title: string, attendees: string[]) =>
    invoke<RecordingStatus>("start_meeting_from_event", { title, attendees }),

  listMicDevices: () => invoke<AudioDevice[]>("list_mic_devices"),

  // screen recording & import
  screenStatus: () => invoke<ScreenStatus>("screen_status"),
  startScreenRecording: (noteId: string, target: CaptureTarget) =>
    invoke<ScreenStatus>("start_screen_recording", { noteId, target }),
  stopScreenRecording: () => invoke<Note>("stop_screen_recording"),
  importMedia: () => invoke<ImportResult | null>("import_media"),
};
