/* ============================================================
   DEV-ONLY mock backend — NEVER runs in production or the native app.
   ------------------------------------------------------------
   Installed from main.tsx only when `import.meta.env.DEV` AND there is no
   real Tauri runtime (`__TAURI_INTERNALS__` absent). It stubs the Tauri IPC
   with realistic fixtures (mirroring the design's data.js) so the whole UI
   renders — with a clean console — in a plain browser for screenshotting /
   visual QA. It changes nothing about the real data flow; the native app
   always has `__TAURI_INTERNALS__`, so this is skipped there.

   Toggles (set in localStorage, then reload):
     fotwMockRecording = "1"  → recording_status returns an active capture
                                 (pinned RecordingBar + muted-output warning)
     fotwMockFirstRun  = "1"  → consent setting unset (shows FirstRunNotice)
     fotwMockNoTranscript = "1"    → meetings report no transcript yet (the
                                     "Transcribe recording" box + notices)
     fotwMockEngineMissing = "1"   → whisper-cli reports not installed
     fotwMockEngineUnmanaged = "1" → no managed engine artifact (manual UI)

   QA hook: fotwMockEmit(event, payload) fires a mock backend event at the
   app's listeners (pipeline:progress, model:progress, …) from the console.
   ============================================================ */

const nowIso = () => new Date().toISOString();
const ago = (mins: number) => new Date(Date.now() - mins * 60_000).toISOString();

const folders = [
  { id: "f-clients", name: "Clients", parent_id: null, created_at: ago(9000) },
  { id: "f-team", name: "Team", parent_id: null, created_at: ago(8000) },
  { id: "f-personal", name: "Personal", parent_id: null, created_at: ago(7000) },
];

// happened_at mirrors the backend's COALESCE(meeting started_at, note
// created_at): it drives list order and is mutable via the date editor.
const noteMeta = [
  {
    id: "n1",
    title: "Acme — renewal review",
    folder_id: "f-clients",
    meeting_id: "m1",
    updated_at: ago(1),
    happened_at: ago(30),
  },
  {
    id: "n2",
    title: "Weekly 1:1 with Dana",
    folder_id: "f-team",
    meeting_id: "m2",
    updated_at: ago(120),
    happened_at: ago(180),
  },
  {
    id: "n3",
    title: "Design weekly — notes",
    folder_id: "f-team",
    meeting_id: null,
    updated_at: ago(1500),
    happened_at: ago(1500),
  },
  {
    id: "n4",
    title: "Q3 planning offsite",
    folder_id: "f-clients",
    meeting_id: "m4",
    updated_at: ago(2900),
    happened_at: ago(2900),
  },
  {
    id: "n5",
    title: "Interview — staff eng",
    folder_id: null,
    meeting_id: "m5",
    updated_at: ago(5800),
    happened_at: ago(5800),
  },
  {
    id: "n6",
    title: "Roadmap brainstorm",
    folder_id: "f-personal",
    meeting_id: null,
    updated_at: ago(8600),
    happened_at: ago(8600),
  },
];

const byHappenedDesc = (a: { happened_at: string }, b: { happened_at: string }) =>
  b.happened_at.localeCompare(a.happened_at);

const scratchpad =
  "acme renewal — procurement pushback on price\n" +
  "- accuracy = the win, PMs stopped manual notes\n" +
  "- legal needs data-residency wording\n" +
  "- send revised SOW, book legal review mon";

const blocks = [
  {
    id: "b1",
    origin: { kind: "user" },
    markdown: "Renewal risk on Acme — procurement flagged pricing. Dana to loop in legal.",
  },
  {
    id: "b2",
    origin: { kind: "ai", source_segment_ids: ["t1", "t2", "t3"] },
    markdown:
      "## Summary\nAcme is happy with the pilot; transcription accuracy removed manual note-taking for PMs. Main blocker is renewal pricing and data-residency language for legal.",
  },
  {
    id: "b3",
    origin: { kind: "ai", source_segment_ids: ["t4", "t5"] },
    markdown:
      "## Action items\n- **You** — send revised SOW today.\n- **You** — book legal review Monday.\n- **Dana** — confirm data-residency requirements.",
  },
  {
    id: "b4",
    origin: { kind: "user" },
    markdown: "Gut check: annual looks likely if legal signs off this week.",
  },
];

function note(id: string) {
  const m = noteMeta.find((n) => n.id === id) || noteMeta[0];
  return {
    id: m.id,
    title: m.title,
    folder_id: m.folder_id,
    meeting_id: m.meeting_id,
    scratchpad: m.id === "n1" ? scratchpad : "",
    blocks:
      m.id === IMPORT_NOTE_ID
        ? []
        : m.meeting_id
          ? blocks
          : [{ id: "b0", origin: { kind: "user" }, markdown: "Rough notes go here." }],
    attachments:
      m.id === "n1"
        ? [
            {
              id: "a1",
              file_name: "acme-pricing.pdf",
              rel_path: "att/acme-pricing.pdf",
              mime: "application/pdf",
              added_at: ago(30),
            },
            // Screen recording: in the browser this URL 404s, which exercises
            // the thumbnail placeholder and the lightbox's playback fallback.
            {
              id: "a2",
              file_name: "screen-20260713-101530.mp4",
              rel_path: "attachments/n1/screen-20260713-101530.mp4",
              mime: "video/mp4",
              added_at: ago(25),
            },
          ]
        : [],
    created_at: ago(200),
    updated_at: m.updated_at,
  };
}

// Mutable so the attendee editor round-trips in the dev browser. One entry
// is calendar-seeded (name = email, unconfirmed) to exercise design state 3.
let mockAttendees: { name: string; email?: string | null }[] = [
  { name: "Dana Osei", email: "dana.osei@acme.com" },
  { name: "marc.reyes@acme.com", email: "marc.reyes@acme.com" },
];
let mockAttendeesConfirmed = false;
let mockUndo: { taken_at: string; changed_segments: number } | null = null;

function meeting(noteId: string) {
  const m = noteMeta.find((n) => n.id === noteId);
  if (!m || !m.meeting_id) return null;
  if (m.id === IMPORT_NOTE_ID) {
    // staged import: no recording until the (mock) transcription starts
    return {
      id: m.meeting_id,
      title: m.title,
      note_id: m.id,
      attendees: [],
      attendees_confirmed: false,
      started_at: ago(1),
      ended_at: mockImport?.started ? nowIso() : null,
      recording: mockImport?.started
        ? { mic_path: null, system_path: null, mixed_path: "mix.wav", duration_ms: 180_000 }
        : null,
    };
  }
  return {
    id: m.meeting_id,
    title: m.title,
    note_id: m.id,
    attendees: mockAttendees,
    attendees_confirmed: mockAttendeesConfirmed,
    started_at: m.happened_at,
    ended_at: new Date(new Date(m.happened_at).getTime() + 754_000).toISOString(),
    recording: {
      mic_path: "mic.wav",
      system_path: "sys.wav",
      mixed_path: "mix.wav",
      duration_ms: 754_000,
    },
  };
}

/* ---- staged media import (sidebar "Import media as a note") ----
   Stateful mock of import_stage / import_state / import_transcribe, with a
   simulated conversion + pipeline event stream so the queue's per-file
   Waiting → Transcribing % → Done flow is testable in the dev browser. */
const IMPORT_NOTE_ID = "n-imp";
const IMPORT_MEETING_ID = "m-imp";
/** Per-file mock durations (ms) — drive the concat boundaries. */
const IMPORT_DURATIONS: Record<string, number> = { "if-1": 120_000, "if-2": 60_000 };
interface MockImportFile {
  id: string;
  file_name: string;
  size: number;
  kind: "audio" | "video";
  rel_path: string | null;
  error: string | null;
}
let mockImport: {
  note_id: string;
  meeting_id: string;
  files: MockImportFile[];
  boundaries_ms: number[];
  started: boolean;
} | null = null;
let mockImportDone = false;

const mockEmit = (event: string, payload: unknown) =>
  (window as unknown as { fotwMockEmit?: (e: string, p: unknown) => void }).fotwMockEmit?.(
    event,
    payload,
  );

function importStage() {
  mockImportDone = false;
  mockImport = {
    note_id: IMPORT_NOTE_ID,
    meeting_id: IMPORT_MEETING_ID,
    files: [
      {
        id: "if-1",
        file_name: "Fuel Bid Meeting 2026-07-09.mp3",
        size: 18_400_000,
        kind: "audio",
        rel_path: "recordings/imp/Fuel Bid Meeting 2026-07-09.mp3",
        error: null,
      },
      {
        id: "if-2",
        file_name: "screen-walkthrough.mp4",
        size: 52_000_000,
        kind: "video",
        rel_path: "recordings/imp/screen-walkthrough.mp4",
        error: null,
      },
      {
        id: "if-3",
        file_name: "agenda.txt",
        size: 4_000,
        kind: "audio",
        rel_path: null,
        error: "Unsupported file type — audio or video only",
      },
    ],
    boundaries_ms: [],
    started: false,
  };
  if (!noteMeta.some((n) => n.id === IMPORT_NOTE_ID)) {
    noteMeta.unshift({
      id: IMPORT_NOTE_ID,
      title: "Fuel Bid Meeting 2026-07-09",
      folder_id: null,
      meeting_id: IMPORT_MEETING_ID,
      updated_at: nowIso(),
      happened_at: nowIso(),
    });
  }
  return mockImport;
}

function importTranscribe(order: string[]) {
  if (!mockImport) throw new Error("no staged import for this meeting");
  if (mockImport.started) return mockImport;
  const files = order
    .map((id) => mockImport?.files.find((f) => f.id === id))
    .filter((f): f is MockImportFile => f != null && f.error == null);
  const boundaries: number[] = [];
  let cum = 0;
  for (const [i, f] of files.entries()) {
    if (i > 0) cum += 1000; // the backend's inter-file silence gap
    cum += IMPORT_DURATIONS[f.id] ?? 60_000;
    boundaries.push(cum);
  }
  mockImport = { ...mockImport, files, boundaries_ms: boundaries, started: true };

  // simulated backend events: per-file conversion, then the real pipeline shape
  const pipe = (
    stage: string,
    detail: string | null = null,
    done = false,
    error: string | null = null,
  ) => mockEmit("pipeline:progress", { meeting_id: IMPORT_MEETING_ID, stage, detail, done, error });
  let t = 200;
  const at = (ms: number, fn: () => void) => window.setTimeout(fn, ms);
  for (const f of files) {
    at(t, () =>
      mockEmit("import:progress", {
        meeting_id: IMPORT_MEETING_ID,
        file_id: f.id,
        stage: "converting",
      }),
    );
    t += 700;
    at(t, () =>
      mockEmit("import:progress", {
        meeting_id: IMPORT_MEETING_ID,
        file_id: f.id,
        stage: "converted",
      }),
    );
  }
  at((t += 300), () => pipe("waiting"));
  at((t += 500), () => pipe("starting"));
  for (let pct = 5; pct <= 100; pct += 5) {
    at((t += 450), () => pipe("transcribing", `${pct}%`));
  }
  at((t += 500), () => pipe("diarizing"));
  at((t += 1200), () => pipe("aligning"));
  at((t += 600), () => pipe("saving"));
  at((t += 500), () => pipe("polishing"));
  at((t += 1200), () => {
    mockImportDone = true;
    pipe("done", null, true);
  });
  return mockImport;
}

const seg = (id: string, key: string, start: number, text: string) => ({
  id,
  speaker_key: key,
  start_ms: start,
  end_ms: start + 7000,
  text,
  words: [],
});
const transcript = {
  meeting_id: "m1",
  language: "en",
  engine: "whisper-large-v3-turbo",
  speakers: [
    { key: "mic", label: "You" },
    { key: "s1", label: "Dana Osei" },
    { key: "s2", label: "Marc Reyes" },
  ],
  segments: [
    seg(
      "t1",
      "s1",
      4000,
      "Thanks for hopping on. We've been happy with the pilot, but procurement flagged the renewal price.",
    ),
    seg(
      "t2",
      "mic",
      12000,
      "Totally hear you. Let's walk through where the value has landed and see what makes sense for the annual.",
    ),
    seg(
      "t3",
      "s2",
      21000,
      "The transcription accuracy is the big win — our PMs stopped taking manual notes entirely.",
    ),
    seg(
      "t4",
      "s1",
      33000,
      "Right. If we can get legal comfortable with the data-residency language, I think we're there by Friday.",
    ),
    seg(
      "t5",
      "mic",
      44000,
      "I'll send the revised SOW today and we can book a legal review Monday.",
    ),
  ],
};

const templates = [
  { id: "tpl-general", name: "General", system_prompt: "", structure_hint: "", built_in: true },
  { id: "tpl-1on1", name: "1:1", system_prompt: "", structure_hint: "", built_in: true },
  {
    id: "tpl-sales",
    name: "Sales discovery",
    system_prompt: "",
    structure_hint: "",
    built_in: true,
  },
  { id: "tpl-standup", name: "Standup", system_prompt: "", structure_hint: "", built_in: true },
  { id: "tpl-interview", name: "Interview", system_prompt: "", structure_hint: "", built_in: true },
];

const asrSettings = {
  tier: "balanced",
  model_id: "whisper-large-v3-turbo",
  use_groq: false,
  max_quality: false,
  has_groq_key: false,
  auto_transcribe: true,
  use_gpu: true,
  gpu_bench: null,
  hw: {
    ram_gb: 32,
    cpu_cores: 12,
    gpu_name: "Apple M3 Pro",
    vram_mb: 18000,
    recommended_tier: "best",
  },
  models: [
    {
      id: "whisper-turbo",
      display: "whisper large-v3-turbo",
      bytes: 1_600_000_000,
      installed: true,
    },
    { id: "whisper-large", display: "whisper large-v3", bytes: 3_100_000_000, installed: false },
    { id: "diarize", display: "pyannote-community-1", bytes: 400_000_000, installed: true },
    { id: "embed", display: "bge-small-en", bytes: 100_000_000, installed: true },
    // The engine binary is filtered into its own row; the Vulkan GPU build
    // must stay visible in the Models list (regression QA for the filter).
    {
      id: "whisper-bin-vulkan",
      display: "whisper.cpp CLI (Vulkan GPU, v1.9.1)",
      bytes: 23_632_146,
      installed: false,
    },
  ],
  // whisper-cli readiness (drives the Transcription-engine row + the
  // engine-missing notice). Flip via localStorage for dev-mock QA:
  //   fotwMockEngineMissing = "1"  → not installed
  //   fotwMockEngineUnmanaged = "1" → no managed artifact (manual install UI)
  get engine_installed() {
    return (
      typeof localStorage === "undefined" || localStorage.getItem("fotwMockEngineMissing") !== "1"
    );
  },
  get engine_managed() {
    return (
      typeof localStorage === "undefined" || localStorage.getItem("fotwMockEngineUnmanaged") !== "1"
    );
  },
};

const llmSettings = {
  provider: "anthropic",
  providers: [
    {
      id: "ollama",
      default_model: "llama3.1",
      is_local: true,
      has_key: true,
      model: "llama3.1",
      base_url: "http://localhost:11434",
    },
    {
      id: "anthropic",
      default_model: "claude-sonnet",
      is_local: false,
      has_key: true,
      model: "claude-sonnet",
      base_url: null,
    },
    {
      id: "openai",
      default_model: "gpt-4o",
      is_local: false,
      has_key: false,
      model: null,
      base_url: null,
    },
    {
      id: "nvidia-nim",
      default_model: "nemotron",
      is_local: false,
      has_key: false,
      model: null,
      base_url: null,
    },
  ],
};

const calendarStatus = {
  google_client_id: "8342••••.apps.googleusercontent.com",
  google_has_secret: true,
  google_connected: true,
  ms_client_id: "",
  ms_connected: false,
};

function upcoming() {
  return [
    {
      id: "e1",
      provider: "google",
      title: "Acme — renewal review",
      start: ago(5),
      end: new Date(Date.now() + 25 * 60_000).toISOString(),
      attendees: ["Dana Osei", "Marc Reyes"],
      join_url: "https://meet.google.com/abc",
    },
    {
      id: "e2",
      provider: "msgraph",
      title: "Design weekly",
      start: new Date(Date.now() + 150 * 60_000).toISOString(),
      end: new Date(Date.now() + 180 * 60_000).toISOString(),
      attendees: ["Priya N."],
      join_url: null,
    },
  ];
}

function recordingStatus() {
  if (typeof localStorage !== "undefined" && localStorage.getItem("fotwMockRecording") === "1") {
    return {
      active: true,
      state: "recording",
      elapsed_ms: 154_000,
      meeting_id: "m1",
      note_id: "n1",
      warnings: [
        "System output looks muted — the other side may record as silence. Unmute to capture them.",
      ],
    };
  }
  return {
    active: false,
    state: null,
    elapsed_ms: 0,
    meeting_id: null,
    note_id: null,
    warnings: [],
  };
}

const searchHits = (q: string) => [
  {
    kind: "note",
    note_id: "n1",
    title: "Acme — renewal review",
    snippet: `…procurement flagged the [[${q || "renewal"}]] price and legal…`,
    start_ms: null,
  },
  {
    kind: "transcript",
    note_id: "n1",
    title: "Acme — renewal review",
    snippet: `Dana: …get legal comfortable with the [[${q || "data"}]]-residency language…`,
    start_ms: 33000,
  },
];

const mcpSnippet = `{
  "mcpServers": {
    "fly-on-the-wall": {
      "command": "C:\\\\Program Files\\\\Fly on the Wall\\\\fly-mcp.exe",
      "args": []
    }
  }
}`;

/* ---- command router ---- */
function handle(cmd: string, args: Record<string, unknown> = {}): unknown {
  switch (cmd) {
    case "ping":
      return "pong";
    case "app_info":
      return {
        version: "1.4.0",
        data_dir: "C:\\Users\\you\\AppData\\Roaming\\Fly on the Wall",
        os: "windows",
      };
    case "list_folders":
      return folders;
    case "create_folder":
      return {
        id: "f-new",
        name: String(args.name ?? "New folder"),
        parent_id: (args.parentId as string) ?? null,
        created_at: nowIso(),
      };
    case "rename_folder":
    case "move_folder":
    case "delete_folder":
    case "move_note":
    case "delete_note":
      return null;
    case "list_recent_notes":
      return [...noteMeta].sort(byHappenedDesc);
    case "list_notes_in_folder":
      return (
        args.folderId == null
          ? noteMeta.filter((n) => !n.folder_id)
          : noteMeta.filter((n) => n.folder_id === args.folderId)
      ).sort(byHappenedDesc);
    case "get_note":
      return note(String(args.id));
    case "create_note":
      return note("n3");
    case "update_note_title":
    case "update_note_scratchpad":
    case "edit_note_block":
    case "enhance_note":
      return note(String(args.id ?? args.noteId ?? "n1"));
    case "get_meeting_for_note":
      return meeting(String(args.noteId));
    case "get_transcript":
      // staged import: no transcript until the mock pipeline finishes
      if (args.meetingId === IMPORT_MEETING_ID && !mockImportDone) return null;
      // fotwMockNoTranscript = "1" → pre-transcription state (the
      // "Transcribe recording" box, where the pipeline notices render).
      if (
        typeof localStorage !== "undefined" &&
        localStorage.getItem("fotwMockNoTranscript") === "1"
      ) {
        return null;
      }
      return transcript;
    case "get_cleaned_transcript":
      if (
        typeof localStorage !== "undefined" &&
        localStorage.getItem("fotwMockNoTranscript") === "1"
      ) {
        return null;
      }
      // Polished variant: same ids/speakers/timestamps, tidied text.
      return {
        ...transcript,
        segments: transcript.segments.map((s) => ({
          ...s,
          text: s.text.replace(/\b(um|uh),?\s*/gi, "").replace(/^\w/, (c) => c.toUpperCase()),
        })),
      };
    case "copy_note_markdown": {
      // The real command writes the clipboard natively; mirror that here so
      // the dev browser behaves the same (best effort — may need focus).
      const md = `# ${note(String(args.noteId ?? "n1")).title}\n\n${scratchpad}`;
      void navigator.clipboard?.writeText(md).catch(() => {});
      return md;
    }
    case "relabel_speaker": {
      const sp = transcript.speakers.find((s) => s.key === args.speakerKey);
      if (sp) sp.label = String(args.label ?? sp.label);
      else
        transcript.speakers.push({
          key: String(args.speakerKey),
          label: String(args.label ?? ""),
        });
      return transcript;
    }
    case "update_meeting_started_at": {
      const m = noteMeta.find((n) => n.meeting_id === args.meetingId);
      if (m) m.happened_at = String(args.startedAt);
      return meeting(m?.id ?? "n1");
    }
    case "update_meeting_attendees": {
      mockAttendees = (args.attendees as typeof mockAttendees) ?? [];
      mockAttendeesConfirmed = true;
      return meeting("n1");
    }
    case "re_diarize_meeting": {
      mockUndo = { taken_at: nowIso(), changed_segments: 3 };
      return { changed_segments: 3, transcript };
    }
    case "revert_speaker_assignment":
      mockUndo = null;
      return transcript;
    case "speaker_undo_state":
      return mockUndo;
    case "edit_transcript_segment": {
      const s = transcript.segments.find((x) => x.id === args.segmentId);
      if (s) s.text = String(args.text ?? s.text);
      return transcript;
    }
    case "pipeline_stage":
      return null;
    case "search":
      return searchHits(String(args.query ?? ""));
    case "search_semantic":
      // dev-mock has no embedding index; the hybrid pass degrades to the
      // same grouped-FTS shape (one hit per note, best snippet first)
      return searchHits(String(args.query ?? "")).slice(0, 1);
    case "recording_status":
      return recordingStatus();
    case "screen_status":
      return typeof localStorage !== "undefined" &&
        localStorage.getItem("fotwMockRecording") === "1"
        ? { active: true, note_id: "n1", elapsed_ms: 132_000 }
        : { active: false, note_id: null, elapsed_ms: 0 };
    case "get_asr_settings":
      return asrSettings;
    case "get_llm_settings":
      return llmSettings;
    // Stateful so the Settings save/guided-swap flow is testable in the mock.
    case "set_llm_settings": {
      const u = args.update as {
        provider: string;
        model: string | null;
        base_url: string | null;
      };
      llmSettings.provider = u.provider;
      const info = llmSettings.providers.find((p) => p.id === u.provider);
      if (info) {
        info.model = u.model;
        info.base_url = u.base_url;
      }
      return null;
    }
    case "get_calendar_settings":
      return calendarStatus;
    case "list_calendars":
      return [
        {
          provider: "google",
          id: "primary",
          name: "you@example.com",
          primary: true,
          enabled: true,
        },
        { provider: "google", id: "team-cal", name: "Team events", primary: false, enabled: true },
      ];
    case "upcoming_meetings":
      return upcoming();
    case "list_templates":
      return templates;
    case "list_mic_devices":
      return [
        { id: "d1", name: "MacBook Pro Microphone", is_default: true },
        { id: "d2", name: "AirPods Pro", is_default: false },
      ];
    case "mcp_config":
      return mcpSnippet;
    case "ask_meeting":
      return "Here's what stands out:\n\n• Acme is happy with pilot accuracy — PMs dropped manual notes.\n• The blocker is renewal pricing + data-residency wording for legal.\n• Next: you send the revised SOW today; legal review booked Monday.";
    case "test_llm_connection":
      return "ok";
    case "ollama_status":
      return {
        installed: true,
        can_install: true,
        running: true,
        managed: true,
        base_url: "http://localhost:11434",
        models: [
          { name: "llama3.1:latest", size: 4_920_000_000 },
          { name: "qwen3.5:4b", size: 3_650_000_000 },
        ],
      };
    case "ollama_pull":
      return null;
    case "ollama_delete":
      return null;
    case "get_app_setting":
      if (args.key === "consent.recording_notice_accepted") {
        return typeof localStorage !== "undefined" &&
          localStorage.getItem("fotwMockFirstRun") === "1"
          ? null
          : "true";
      }
      return null;
    case "download_model":
      return "";
    case "reveal_logs_dir":
      return null;
    case "list_capture_windows":
      return [
        "Weekly sync - Zoom",
        "IDB VDI IT - VMware Horizon Client",
        "budget notes.xlsx - Excel",
      ];
    case "export_note":
      return "C:\\Users\\you\\Desktop\\note.md";
    case "ensure_video_thumbnail":
      return `${String(args.relPath ?? "")}.jpg`;
    case "import_stage":
      return importStage();
    case "import_state":
      return args.meetingId === IMPORT_MEETING_ID && mockImport && !mockImportDone
        ? mockImport
        : null;
    case "import_transcribe":
      return importTranscribe((args.order as string[]) ?? []);
    default:
      return null;
  }
}

function installDevMock() {
  const callbacks: Record<number, (payload: unknown) => void> = {};
  const listeners: Record<string, number[]> = {};
  let cbId = 1;
  const w = window as unknown as Record<string, unknown>;
  w.__TAURI_INTERNALS__ = {
    transformCallback(cb: (payload: unknown) => void) {
      const id = cbId++;
      callbacks[id] = cb;
      return id;
    },
    unregisterCallback(id: number) {
      delete callbacks[id];
    },
    convertFileSrc(filePath: string) {
      return filePath;
    },
    async invoke(cmd: string, args: Record<string, unknown>) {
      // event plugin: remember which handler listens to which event so QA can
      // fire backend events by hand (see fotwMockEmit below)
      if (cmd === "plugin:event|listen" || cmd === "plugin:event|registerListener") {
        const ev = String((args as { event?: unknown })?.event ?? "");
        const handler = Number((args as { handler?: unknown })?.handler ?? 0);
        if (ev && handler) (listeners[ev] ??= []).push(handler);
        return cbId++;
      }
      if (cmd === "plugin:event|unlisten") return null;
      return handle(cmd, args || {});
    },
  };
  // QA hook — fire a mock backend event from the devtools console, e.g.:
  //   fotwMockEmit("pipeline:progress", { meeting_id: "m1", stage: "transcribing",
  //     detail: "your microphone (42%)", done: false, error: null })
  //   fotwMockEmit("pipeline:progress", { meeting_id: "m1", stage: null, detail: null,
  //     done: true, error: "whisper-cli is not installed — install whisper.cpp …" })
  w.fotwMockEmit = (event: string, payload: unknown) => {
    for (const id of listeners[event] ?? []) callbacks[id]?.({ event, id: 0, payload });
  };
  // The event plugin reads a separate global for listener bookkeeping.
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    registerListener() {},
    unregisterListener() {},
  };
  console.info("[devMock] Tauri IPC stubbed (dev browser only — no native runtime).");
}

// Install synchronously at module load — BEFORE the Tauri api modules bind
// window.__TAURI_INTERNALS__. Dev + plain-browser only; in production the
// `import.meta.env.DEV` guard is statically false and this whole module
// (installer + fixtures) is dead-code-eliminated.
if (import.meta.env.DEV && typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window)) {
  installDevMock();
}
