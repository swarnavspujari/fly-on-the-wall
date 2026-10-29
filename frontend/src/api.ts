// Typed wrappers over the Tauri command surface. Components never call
// invoke() directly — everything crosses this one seam.

import { invoke } from "@tauri-apps/api/core";

/** Window event after the saved AI provider or a cloud consent changes, so
 *  views that name the provider (the skipped-cleanup notice) re-read it. */
export const LLM_SETTINGS_CHANGED_EVENT = "fotw:llm-settings-changed";
const llmSettingsChanged = (): void => {
  window.dispatchEvent(new Event(LLM_SETTINGS_CHANGED_EVENT));
};
import type {
  CalendarAttendee,
  MeetingSeed,
  AppInfo,
  AskMessage,
  Attendee,
  AudioDevice,
  CalendarSettingsUpdate,
  CalendarStatus,
  CalendarToggle,
  CaptureTarget,
  DiskEncryption,
  ImportStaged,
  ScreenStatus,
  AsrSettings,
  AsrSettingsUpdate,
  Folder,
  LlmSettings,
  LlmSettingsUpdate,
  RecoveredMeeting,
  Meeting,
  MeetingPrivacy,
  Note,
  NoteSummary,
  OllamaStatus,
  PolishResult,
  PlaybackSource,
  ShrinkEstimate,
  PrivacySettings,
  RecordingStatus,
  ReDiarizeOutcome,
  SearchHit,
  SpeakerUndoState,
  StorageReport,
  SweepReport,
  VideoJobKind,
  VideoJobStatus,
  SystemAudioPreflight,
  Template,
  Transcript,
  UpcomingMeetings,
  WipeReport,
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
  exportTranscriptVtt: (meetingId: string, cleaned: boolean) =>
    invoke<string | null>("export_transcript_vtt", { meetingId, cleaned }),
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
  revealLogsDir: () => invoke<void>("reveal_logs_dir"),
  listCaptureWindows: () => invoke<string[]>("list_capture_windows"),
  mcpConfig: () => invoke<string>("mcp_config"),
  getAppSetting: (key: string) => invoke<string | null>("get_app_setting", { key }),
  setAppSetting: (key: string, value: string) => invoke<void>("set_app_setting", { key, value }),

  // search
  search: (query: string) => invoke<SearchHit[]>("search", { query }),
  // hybrid (FTS + semantic) pass; degrades to grouped FTS when the local
  // embedding model isn't available
  searchSemantic: (query: string) => invoke<SearchHit[]>("search_semantic", { query }),

  // recording
  recordingStatus: () => invoke<RecordingStatus>("recording_status"),
  startRecording: (noteId: string | null, seed: MeetingSeed | null = null) =>
    invoke<RecordingStatus>("start_recording", { noteId, seed }),
  pauseRecording: () => invoke<RecordingStatus>("pause_recording"),
  resumeRecording: () => invoke<RecordingStatus>("resume_recording"),
  stopRecording: () => invoke<Meeting>("stop_recording"),
  getMeetingForNote: (noteId: string) => invoke<Meeting | null>("get_meeting_for_note", { noteId }),
  /** ALL meetings attached to a note, newest first (re-recording into a note
   *  adds a meeting; the earlier ones stay reachable through this). */
  getMeetingsForNote: (noteId: string) => invoke<Meeting[]>("get_meetings_for_note", { noteId }),
  /** Latest live-caption status for a meeting (catch-up for the live pane —
   *  a status emitted before the pane mounted would otherwise be lost). */
  liveStatus: (meetingId: string) =>
    invoke<{ meeting_id: string; state: "ready" | "unavailable"; detail: string } | null>(
      "live_status",
      { meetingId },
    ),
  /** The file the player streams. `build` rebuilds a compressed recording's
   *  playback mix (on Play); without it, opening a note never does work. */
  preparePlayback: (meetingId: string, build: boolean) =>
    invoke<PlaybackSource | null>("prepare_playback", { meetingId, build }),
  /** Settings › Storage: sizes by kind plus the compression job's status. */
  storageReport: () => invoke<StorageReport>("storage_report"),
  /** The one-time "recordings compressed" notice was seen. */
  dismissCompressNotice: () => invoke<void>("dismiss_compress_notice"),
  /** Opt-in video storage: start a job (the UI confirms first). */
  startVideoJob: (kind: VideoJobKind) => invoke<VideoJobStatus>("start_video_job", { kind }),
  cancelVideoJob: () => invoke<void>("cancel_video_job"),
  /** "audio_only" also starts the job for the videos already imported. */
  setImportVideoPolicy: (policy: "keep" | "audio_only") =>
    invoke<VideoJobStatus>("set_import_video_policy", { policy }),
  /** What shrinking the screen recordings would save (30 s sample). */
  estimateScreenShrink: () => invoke<ShrinkEstimate | null>("estimate_screen_shrink"),
  // Set the meeting's start date/time (RFC 3339 UTC). Length is preserved
  // (ended_at shifts); the meeting folder + manifest re-mirror the date.
  updateMeetingStartedAt: (meetingId: string, startedAt: string) =>
    invoke<Meeting>("update_meeting_started_at", { meetingId, startedAt }),

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
  // Why the meeting's transcription last gave up (null = it didn't): the
  // terminal error event is gone once you reopen the note or restart.
  pipelineFailure: (meetingId: string) => invoke<string | null>("pipeline_failure", { meetingId }),

  // attendees & attendee-informed diarization
  // Replaces the list and marks it user-confirmed. Never re-transcribes.
  updateMeetingAttendees: (meetingId: string, attendees: Attendee[]) =>
    invoke<Meeting>("update_meeting_attendees", { meetingId, attendees }),
  // Re-runs ONLY diarize → align → save (+ background re-extraction) on the
  // existing audio/transcript; snapshots the prior assignment for undo.
  reDiarizeMeeting: (meetingId: string) =>
    invoke<ReDiarizeOutcome>("re_diarize_meeting", { meetingId }),
  revertSpeakerAssignment: (meetingId: string) =>
    invoke<Transcript>("revert_speaker_assignment", { meetingId }),
  speakerUndoState: (meetingId: string) =>
    invoke<SpeakerUndoState | null>("speaker_undo_state", { meetingId }),

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
  setLlmSettings: (update: LlmSettingsUpdate) =>
    invoke<void>("set_llm_settings", { update }).then(llmSettingsChanged),
  testLlmConnection: () => invoke<string>("test_llm_connection"),
  // Meetings whose cleanup / action items were skipped for missing cloud
  // consent this session (updates arrive as "llm:consent-skips"), and the
  // re-run once the provider is allowed (background; returns how many).
  consentSkippedMeetings: () => invoke<string[]>("consent_skipped_meetings"),
  retryConsentSkipped: () => invoke<number>("retry_consent_skipped"),
  // Meetings self-heal restored this launch with Keep local set because their
  // privacy setting couldn't be confirmed (updates: "privacy:recovered-kept-local").
  recoveredKeptLocal: () => invoke<RecoveredMeeting[]>("recovered_kept_local"),
  /** One meeting off the list (null: all); returns what's left. */
  dismissRecoveredKeptLocal: (meetingId: string | null) =>
    invoke<RecoveredMeeting[]>("dismiss_recovered_kept_local", { meetingId }),
  // Ask in a native dialog (the backend owns its words) whether `provider`
  // may receive meeting content, and record a yes. True when it may: local,
  // allowed before, or allowed now. The only way consent gets recorded;
  // `consent.cloud_llm.*` can be read but not set from here.
  requestCloudConsent: (provider: string, atLaunch = false) =>
    invoke<boolean>("request_cloud_consent", { provider, atLaunch }),
  // structured item extraction (decisions / action items / figures …)
  extractMeetingItems: (meetingId: string) =>
    invoke<number>("extract_meeting_items", { meetingId }),
  backfillMeetingItems: () =>
    invoke<{ processed: number; extracted: number; failed: number; first_error: string | null }>(
      "backfill_meeting_items",
    ),
  pendingBackfillCount: () => invoke<number>("pending_backfill_count"),

  // managed Ollama (local AI provider)
  ollamaStatus: () => invoke<OllamaStatus>("ollama_status"),
  // Pulls a model into the local server; progress arrives as model:progress
  // events with id "ollama:<model>".
  ollamaPull: (model: string) => invoke<void>("ollama_pull", { model }),
  // Deletes a local model; the backend refuses to delete the model the app
  // is currently configured to use.
  ollamaDelete: (model: string) => invoke<void>("ollama_delete", { model }),

  // calendars
  getCalendarSettings: () => invoke<CalendarStatus>("get_calendar_settings"),
  setCalendarSettings: (update: CalendarSettingsUpdate) =>
    invoke<void>("set_calendar_settings", { update }),
  connectCalendar: (provider: string) => invoke<void>("connect_calendar", { provider }),
  disconnectCalendar: (provider: string) => invoke<void>("disconnect_calendar", { provider }),
  listCalendars: () => invoke<CalendarToggle[]>("list_calendars"),
  setCalendarEnabled: (provider: string, calendarId: string, enabled: boolean) =>
    invoke<void>("set_calendar_enabled", { provider, calendarId, enabled }),
  upcomingMeetings: () => invoke<UpcomingMeetings>("upcoming_meetings"),
  startMeetingFromEvent: (title: string, attendees: CalendarAttendee[]) =>
    invoke<RecordingStatus>("start_meeting_from_event", { title, attendees }),

  listMicDevices: () => invoke<AudioDevice[]>("list_mic_devices"),
  /** macOS: short throwaway tap probing whether system-audio capture is
   *  actually permitted (a stale TCC grant shows as allowed but records
   *  silence). Cheap constant elsewhere. */
  preflightSystemAudio: () => invoke<SystemAudioPreflight>("preflight_system_audio"),
  /** macOS: open System Settings → Privacy → Screen & System Audio Recording. */
  openPrivacySettings: () => invoke<void>("open_privacy_settings"),

  // screen recording & import
  screenStatus: () => invoke<ScreenStatus>("screen_status"),
  startScreenRecording: (noteId: string, target: CaptureTarget) =>
    invoke<ScreenStatus>("start_screen_recording", { noteId, target }),
  stopScreenRecording: () => invoke<Note>("stop_screen_recording"),
  // Multi-select picker → ONE staged (untranscribed) note. Null on cancel.
  importStage: () => invoke<ImportStaged | null>("import_stage"),
  // The staged queue for a meeting (null once its transcript exists).
  importState: (meetingId: string) => invoke<ImportStaged | null>("import_state", { meetingId }),
  // Normalize + join the staged files in `order`, then queue the pipeline.
  importTranscribe: (meetingId: string, order: string[]) =>
    invoke<ImportStaged>("import_transcribe", { meetingId, order }),
  // Stop a queued/running transcription; finished batches stay checkpointed.
  cancelTranscription: (meetingId: string) => invoke<void>("cancel_transcription", { meetingId }),

  // privacy controls
  getMeetingPrivacy: (meetingId: string) =>
    invoke<MeetingPrivacy>("get_meeting_privacy", { meetingId }),
  /** "Keep local" for every meeting of a note: the backend then refuses cloud
   *  AI providers and Groq transcription for it. */
  setNoteKeepLocal: (noteId: string, keepLocal: boolean) =>
    invoke<void>("set_note_keep_local", { noteId, keepLocal }),
  setFolderKeepLocal: (folderId: string, keepLocal: boolean) =>
    invoke<void>("set_folder_keep_local", { folderId, keepLocal }),
  listLocalOnlyFolders: () => invoke<string[]>("list_local_only_folders"),
  setParticipantsNotified: (meetingId: string, notified: boolean) =>
    invoke<void>("set_participants_notified", { meetingId, notified }),
  copyRecordingNotice: (text: string) => invoke<void>("copy_recording_notice", { text }),
  getPrivacySettings: () => invoke<PrivacySettings>("get_privacy_settings"),
  /** How many meetings would lose their audio right now under `days`. */
  previewAudioRetention: (days: number) => invoke<number>("preview_audio_retention", { days }),
  /** Set retention (null = never delete) and sweep immediately. */
  setAudioRetention: (days: number | null) => invoke<SweepReport>("set_audio_retention", { days }),
  dismissWipeReport: () => invoke<void>("dismiss_wipe_report"),
  deleteAllData: (includeModels: boolean, confirmation: string) =>
    invoke<WipeReport>("delete_all_data", { includeModels, confirmation }),
  diskEncryptionStatus: () => invoke<DiskEncryption>("disk_encryption_status"),

  /** "Keep" an enhanced block as your own text, unchanged (an AI line with no
   *  source becomes yours). To remove one, edit it to empty text. */
  keepNoteBlock: (noteId: string, blockId: string) =>
    invoke<Note>("keep_note_block", { noteId, blockId }),
};
