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
     fotwMockTapDenied = "1"       → app_info reports macOS + the system-audio
                                     preflight reports a TCC denial (shows the
                                     SystemAudioNotice card)
     fotwMockEncryption = "off" | "on" → disk_encryption_status (default
                                     "unknown": no banner)
     fotwMockAudioDeleted = "1"    → m1's audio reports deleted by retention
     fotwMockOllama = "missing" | "nomodel" → Settings › Ask and Enhance local
                                     model row: runtime not installed, or the
                                     configured model not downloaded
     fotwMockGpuBench = "gpu" | "cpu" | "failed" → a stored GPU speed-test
                                     verdict (Settings "Use the graphics card")
     fotwMockUpdate = "available" | "fail" → the startup update check (5 s
                                     after load) finds v2.4.0; Update now hangs
                                     in "Downloading", or fails
     fotwMockImportFail = "1"      → the first Transcribe of a staged import
                                     can't convert screen-walkthrough.mp4:
                                     that row shows its cause, the others
                                     convert; "Retry failed file" succeeds
     fotwMockRecWarning = "muted" | "volume" | "loopback" | "macos" | "worst" | "lost" | "device" | "disk" | "none"
                                   → with fotwMockRecording, the capture's
                                     warnings are the exact backend sentence
                                     for that case (recording bar chips;
                                     "worst": two warnings + a 1 h timer;
                                     "lost": audio lost on both sides;
                                     "device": the output changed; "disk":
                                     the mic's audio couldn't be saved)
     fotwMockAudioLost = "1"       → Stop emits recording:audio_lost (1.2 s
                                     mic, 0.4 s system): the "Part of the last
                                     recording is missing" notice
     fotwMockNoAttendees = "1"     → Record starts with no attendees, as with
                                     no calendar event (speaker card, typed)
     fotwMockConsentReset = "1"    → no cloud consent recorded (as after the
                                     v2.4 reset): the launch check asks for
                                     Anthropic, the saved provider
     fotwMockConsentSkips = "1"    → the backend skipped m1's cleanup and
                                     action items for missing consent (the
                                     notice with "Review and allow")
     fotwMockRecoveredKeptLocal = "1" → self-heal restored m1 and set it to
                                     Keep local (the review notice)
     fotwMockAskError = keep_local | consent | no_key | auth | ollama_missing |
                        ollama_down | model_missing | offline | timeout |
                        rate_limit | server | other → ask_meeting fails with
                                     the backend's own text for that case
     fotwMockImportPipelineFail = "1" → a staged import's run fails for good
                                     at "Detecting speakers" (after ASR);
                                     pipeline_failure keeps the cause, so the
                                     queue shows it with Retry on reopen

   Settings commands are stateful for the session (ASR save bar, provider
   pick + consent, calendars, templates) and reset on reload.

   QA hook: fotwMockEmit(event, payload) fires a mock backend event at the
   app's listeners (pipeline:progress, model:progress, …) from the console.
   ============================================================ */

import { WHISPER_ENGINE_ID } from "./types";
import {
  customServerHost,
  providerVendor,
  serverChanged,
  serverChangeNeedsKey,
} from "./settingsModel";

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
    // list status (computed by the backend query): Enhanced
    has_transcript: true,
    has_enhanced_doc: true,
    has_recording: true,
    job_status: "done",
  },
  {
    id: "n2",
    title: "Weekly 1:1 with Dana",
    folder_id: "f-team",
    meeting_id: "m2",
    updated_at: ago(120),
    happened_at: ago(180),
    // Ready to enhance
    has_transcript: true,
    has_enhanced_doc: false,
    has_recording: true,
    job_status: "done",
  },
  {
    id: "n3",
    title: "Design weekly — notes",
    folder_id: "f-team",
    meeting_id: null,
    updated_at: ago(1500),
    happened_at: ago(1500),
    // Notes only
    has_transcript: false,
    has_enhanced_doc: false,
    has_recording: false,
    job_status: null,
  },
  {
    id: "n4",
    title: "Q3 planning offsite",
    folder_id: "f-clients",
    meeting_id: "m4",
    updated_at: ago(2900),
    happened_at: ago(2900),
    // Queued (waiting, or requeued after a crash)
    has_transcript: false,
    has_enhanced_doc: false,
    has_recording: true,
    job_status: "queued",
  },
  {
    id: "n5",
    title: "Interview — staff eng",
    folder_id: null,
    meeting_id: "m5",
    updated_at: ago(5800),
    happened_at: ago(5800),
    // Failed
    has_transcript: false,
    has_enhanced_doc: false,
    has_recording: true,
    job_status: "failed" as string | null,
  },
  {
    id: "n6",
    title: "Roadmap brainstorm",
    folder_id: "f-personal",
    meeting_id: null,
    updated_at: ago(8600),
    happened_at: ago(8600),
    // Notes only
    has_transcript: false,
    has_enhanced_doc: false,
    has_recording: false,
    job_status: null,
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
  // An AI line with no source: the Enhanced view flags it with Keep / Remove.
  {
    id: "b5",
    origin: { kind: "ai", source_segment_ids: [] as string[] },
    markdown: "Acme expects to sign the annual contract by the end of the month.",
  },
];

// edit_note_block / keep_note_block change the blocks above for the session,
// like the backend: an edit (or Keep) makes an AI block yours, and an edit to
// empty text removes it. Other notes' blocks (b0 on the notes-only fixtures)
// aren't stored, so those calls just return the note, as before.
function editMockBlock(blockId: string, markdown: string | null): boolean {
  const i = blocks.findIndex((b) => b.id === blockId);
  if (i === -1) return false;
  if (markdown === null || blocks[i].markdown !== markdown) {
    blocks[i] = {
      ...blocks[i],
      origin: { kind: "user" },
      markdown: markdown ?? blocks[i].markdown,
    };
  }
  if (markdown !== null && markdown.trim() === "") blocks.splice(i, 1);
  return true;
}

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
/** True while the mock import's pipeline is queued or running. */
let mockImportRunning = false;
/** A run that gave up keeps its cause, like the backend's parked job row
 *  (read back by pipeline_failure); Retry (transcribe_meeting) clears it. */
const mockPipelineFailures: Record<string, string> = {
  // the seeded "Failed" note (n5): its parked job's cause
  m5: "whisper-cli exited with status 3221225477",
};
const MOCK_PIPELINE_FAILURE =
  "speaker detection failed: failed to launch sherpa-onnx (The filename or extension is too long. (os error 206))";

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
        file_name: "Planning Sync 2026-01-15.mp3",
        size: 18_400_000,
        kind: "audio",
        rel_path: "recordings/imp/Planning Sync 2026-01-15.mp3",
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
      title: "Planning Sync 2026-01-15",
      folder_id: null,
      meeting_id: IMPORT_MEETING_ID,
      updated_at: nowIso(),
      happened_at: nowIso(),
      // staged import: no audio attached until Transcribe
      has_transcript: false,
      has_enhanced_doc: false,
      has_recording: false,
      job_status: null,
    });
  }
  return mockImport;
}

function importTranscribe(order: string[]) {
  if (!mockImport) throw new Error("no staged import for this meeting");
  if (mockImport.started) return mockImport;
  const convertFailure = mockImportConvertFailure(order);
  if (convertFailure) return convertFailure;
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
  mockImportRunning = true;

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
  // engine-labeled details like pipeline.rs transcribe_detail emits: a cloud
  // stretch with one quota-wait notice, then the GPU spillover finishing up
  for (let pct = 5; pct <= 100; pct += 5) {
    const detail =
      pct === 40
        ? "cloud 40%, waiting for cloud quota (~3m)"
        : pct < 60
          ? `cloud ${pct}%`
          : `GPU ${pct}%`;
    at((t += 450), () => pipe("transcribing", detail));
  }
  at((t += 500), () => pipe("diarizing"));
  // fotwMockImportPipelineFail: gives up here, as a sherpa launch failure
  // ends (the backend tries it three times first; the mock skips to the
  // end); the queue stays started
  if (mockFlag("fotwMockImportPipelineFail") === "1") {
    at((t += 1200), () => {
      mockImportRunning = false;
      mockPipelineFailures[IMPORT_MEETING_ID] = MOCK_PIPELINE_FAILURE;
      pipe("error", null, true, MOCK_PIPELINE_FAILURE);
    });
    return mockImport;
  }
  at((t += 1200), () => pipe("aligning"));
  at((t += 600), () => pipe("saving"));
  at((t += 500), () => pipe("polishing"));
  at((t += 1200), () => {
    mockImportDone = true;
    mockImportRunning = false;
    pipe("done", null, true);
  });
  return mockImport;
}

/* ---- after Transcribe: Retry and Cancel (mirror cancel_transcription) ----
   Retry re-enqueues the joined recording: the failure clears and the run
   picks up after ASR (checkpointed), then finishes. Cancel with nothing
   queued or running drops the queue to idle and emits the terminal
   "cancelled" itself; a running mock run ignores it (no unwinding here). */
function mockImportRetry() {
  delete mockPipelineFailures[IMPORT_MEETING_ID];
  mockImportRunning = true;
  const pipe = (stage: string, done = false) =>
    mockEmit("pipeline:progress", {
      meeting_id: IMPORT_MEETING_ID,
      stage,
      detail: null,
      done,
      error: null,
    });
  let t = 0;
  const at = (ms: number, fn: () => void) => window.setTimeout(fn, ms);
  at((t += 100), () => pipe("waiting"));
  for (const stage of ["starting", "diarizing", "aligning", "saving"]) {
    at((t += 500), () => pipe(stage));
  }
  at((t += 500), () => {
    mockImportDone = true;
    mockImportRunning = false;
    pipe("done", true);
  });
}

function mockCancelTranscription(meetingId: string) {
  if (meetingId === IMPORT_MEETING_ID && mockImportRunning) return null;
  if (meetingId === IMPORT_MEETING_ID && mockImport) mockImport = { ...mockImport, started: false };
  // like the backend: a parked failure goes with the run it belongs to
  delete mockPipelineFailures[meetingId];
  window.setTimeout(
    () =>
      mockEmit("pipeline:progress", {
        meeting_id: meetingId,
        stage: "cancelled",
        detail: null,
        done: true,
        error: null,
      }),
    0,
  );
  return null;
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

let templates = [
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
// Section hints shaped like the built-ins' (crates/fly-storage/src/templates.rs),
// so the Notes view's ghost headings have something to show.
const MOCK_TEMPLATE_HINTS: Record<string, string> = {
  "tpl-general": "## Summary\n## Key points\n## Decisions\n## Action items (owner — due)",
  "tpl-1on1": "## Topics discussed\n## Feedback\n## Commitments\n## Follow-ups for next time",
  "tpl-sales": "## Company & contact\n## Pain points\n## Budget & timeline\n## Next steps",
  "tpl-standup": "## Per person\n### <name>\n- Did\n- Next\n- Blockers\n## Cross-team blockers",
  "tpl-interview":
    "## Candidate background\n## Strong signals\n## Concerns\n## Recommended follow-ups",
};
templates = templates.map((t) => ({
  ...t,
  structure_hint: t.structure_hint || MOCK_TEMPLATE_HINTS[t.id] || "",
}));

// fotwMockGpuBench = "gpu" | "cpu" | "failed" seeds a stored speed-test verdict.
function mockGpuBench() {
  const v = typeof localStorage === "undefined" ? null : localStorage.getItem("fotwMockGpuBench");
  if (v === "gpu") return { verdict: "gpu", reason: "", gpu_secs: 11, cpu_secs: 38, model_id: "x" };
  if (v === "cpu") return { verdict: "cpu", reason: "", gpu_secs: 41, cpu_secs: 30, model_id: "x" };
  if (v === "failed")
    return { verdict: "cpu", reason: "failed", gpu_secs: null, cpu_secs: null, model_id: "x" };
  return null;
}

// An in-app engine install (download_model "whisper-bin") flips readiness.
let mockEngineInstalled = false;

// Stateful for the session: set_asr_settings (below) mirrors the backend.
const asrSettings = {
  tier: "balanced",
  model_id: null as string | null,
  use_groq: false,
  max_quality: false,
  has_groq_key: false,
  auto_transcribe: true,
  use_gpu: true,
  gpu_available: true,
  gpu_bench: mockGpuBench() as ReturnType<typeof mockGpuBench>,
  hw: {
    ram_gb: 32,
    cpu_cores: 12,
    gpu_name: "Apple M3 Pro",
    vram_mb: 18000,
    recommended_tier: "best",
  },
  // Ids match the real model catalog (fly-asr / models.rs): ggml-* weights
  // feed the Model select; the rest are tools listed under "Models on this
  // computer".
  models: [
    { id: "ggml-small-q5_1", display: "Whisper small (Q5)", bytes: 190_000_000, installed: false },
    {
      id: "ggml-large-v3-turbo-q5_0",
      display: "Whisper large-v3-turbo (Q5)",
      bytes: 574_000_000,
      installed: true,
    },
    {
      id: "ggml-large-v3-q5_0",
      display: "Whisper large-v3 (Q5)",
      bytes: 1_080_000_000,
      installed: false,
    },
    { id: WHISPER_ENGINE_ID, display: "whisper.cpp CLI", bytes: 4_200_000, installed: true },
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
      mockEngineInstalled ||
      typeof localStorage === "undefined" ||
      localStorage.getItem("fotwMockEngineMissing") !== "1"
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
      default_model: "claude-sonnet-5-5",
      is_local: false,
      has_key: true,
      model: "claude-sonnet-5-5",
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
      id: "nim",
      default_model: "meta/llama-3.1-70b-instruct",
      is_local: false,
      has_key: false,
      model: null as string | null,
      base_url: null as string | null,
    },
  ] as {
    id: string;
    default_model: string;
    is_local: boolean;
    has_key: boolean;
    model: string | null;
    base_url: string | null;
  }[],
};

const calendarStatus = {
  google_client_id: "8342••••.apps.googleusercontent.com",
  google_has_secret: true,
  google_connected: true,
  google_configured: true,
  ms_client_id: "",
  ms_connected: false,
  ms_configured: true,
};
// Home QA (v2.4 PR 1.4). fotwMockCalendar = "none" → no calendar connected
// (Up next's connect prompt; Connect flips it on); "expired" → Google needs
// a reconnect (no events from it, like the backend); "empty" → connected,
// but no video meetings left today. fotwMockNoNotes = "1" → no notes (Home's
// empty state).

let mockCalendars = [
  { provider: "google", id: "primary", name: "you@example.com", primary: true, enabled: true },
  { provider: "google", id: "team-cal", name: "Team events", primary: false, enabled: true },
];

/** set_asr_settings, mirroring asr_commands.rs: GPU off→on clears the
 *  stored speed test; an empty key deletes it, null keeps it. */
function setAsrSettings(u: {
  tier: string;
  model_id: string | null;
  use_groq: boolean;
  max_quality: boolean;
  auto_transcribe: boolean;
  use_gpu: boolean;
  groq_key: string | null;
}) {
  asrSettings.tier = u.tier;
  asrSettings.model_id = u.model_id || null;
  asrSettings.use_groq = u.use_groq;
  asrSettings.max_quality = u.max_quality;
  asrSettings.auto_transcribe = u.auto_transcribe;
  if (u.use_gpu && !asrSettings.use_gpu) asrSettings.gpu_bench = null;
  asrSettings.use_gpu = u.use_gpu;
  if (u.groq_key != null) asrSettings.has_groq_key = u.groq_key !== "";
  return null;
}

function upcoming() {
  const homeCal = mockFlag("fotwMockCalendar");
  if (!calendarStatus.google_connected && !calendarStatus.ms_connected) {
    return { needs_reconnect: [], events: [] };
  }
  if (homeCal === "expired") return { needs_reconnect: ["google"], events: [] };
  if (homeCal === "empty") return { needs_reconnect: [], events: [] };
  return {
    // Flip to ["google"] to preview the sidebar's Reconnect row.
    needs_reconnect: [],
    events: [
      {
        id: "e1",
        provider: "google",
        title: "Acme — renewal review",
        start: ago(5),
        end: new Date(Date.now() + 25 * 60_000).toISOString(),
        attendees: [
          { email: "you@example.com", name: "You", is_self: true, declined: false },
          { email: "dana@acme.com", name: "Dana Osei", is_self: false, declined: false },
          { email: "marc@acme.com", name: "Marc Reyes", is_self: false, declined: false },
          { email: "sam@acme.com", name: "Sam Declined", is_self: false, declined: true },
        ],
        join_url: "https://meet.google.com/abc",
      },
      {
        id: "e2",
        provider: "msgraph",
        title: "Design weekly",
        start: new Date(Date.now() + 150 * 60_000).toISOString(),
        end: new Date(Date.now() + 180 * 60_000).toISOString(),
        attendees: [{ email: "priya@acme.com", name: "Priya N.", is_self: false, declined: false }],
        join_url: null,
      },
    ],
  };
}

function recordingStatus() {
  const qa = mockRecordingQa();
  if (qa) return qa;
  if (typeof localStorage !== "undefined" && localStorage.getItem("fotwMockRecording") === "1") {
    return {
      active: true,
      state: "recording",
      elapsed_ms: 154_000,
      meeting_id: "m1",
      note_id: "n1",
      warnings:
        // + fotwMockTapDenied → the macOS tap-silence warning, which carries
        // the RecordingBar's inline "Open Settings" action.
        localStorage.getItem("fotwMockTapDenied") === "1"
          ? [
              "System audio is playing but its capture is recording only silence — grant it in System Settings → Privacy & Security → Screen & System Audio Recording.",
            ]
          : [
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

// Same shape as commands.rs mcp_config (server key + sidecar file name).
const mcpSnippet = `{
  "mcpServers": {
    "flyonthewall": {
      "command": "C:\\\\Program Files\\\\Fly on the Wall\\\\flyonthewall-mcp.exe",
      "args": []
    }
  }
}`;

/* ---- privacy controls ---- */
const mockFlag = (k: string) =>
  typeof localStorage !== "undefined" ? localStorage.getItem(k) : null;
// get/set_app_setting store (consent.* / capture.* / ui.* keys). The mock
// user already accepted Anthropic (the configured provider), so picking
// OpenAI or NVIDIA NIM asks for cloud consent (request_cloud_consent).
// fotwMockConsentReset starts with none, as the v2.4 consent reset leaves it.
const initialUiSettings = (): Record<string, string> =>
  mockFlag("fotwMockConsentReset") === "1" ? {} : { "consent.cloud_llm.anthropic": "true" };
const mockUiSettings: Record<string, string> = initialUiSettings();

/** Tests: put the get/set_app_setting store back to its starting state, so a
 *  consent one test gives can't leak into the next. */
export function resetMockAppSettings() {
  for (const k of Object.keys(mockUiSettings)) delete mockUiSettings[k];
  Object.assign(mockUiSettings, initialUiSettings());
}
const mockLocalFolders = new Set<string>();
const mockPrivacy: Record<
  string,
  { keep_local: boolean; participants_notified: boolean; audio_deleted_at: string | null }
> = {};
let mockRetentionDays: number | null = null;
let mockBackfillPending = 3;
function meetingPrivacy(meetingId: string) {
  const p = (mockPrivacy[meetingId] ??= {
    keep_local: false,
    participants_notified: false,
    audio_deleted_at:
      meetingId === "m1" && mockFlag("fotwMockAudioDeleted") === "1" ? ago(600) : null,
  });
  const folder = noteMeta.find((n) => n.meeting_id === meetingId)?.folder_id ?? null;
  return {
    meeting_id: meetingId,
    ...p,
    keep_local_from_folder: folder != null && mockLocalFolders.has(folder),
  };
}

/* ---- managed Ollama ---- */
// fotwMockOllama = "missing" | "nomodel" previews the setup states; install,
// pull and delete then change this state for the rest of the session.
let mockOllama: { installed: boolean; models: { name: string; size: number }[] } | null = null;
function ollamaState() {
  if (!mockOllama) {
    const o = mockFlag("fotwMockOllama");
    mockOllama = {
      installed: o !== "missing",
      models:
        o === "missing"
          ? []
          : o === "nomodel"
            ? [{ name: "qwen3.5:4b", size: 3_650_000_000 }]
            : [
                { name: "llama3.1:latest", size: 4_920_000_000 },
                { name: "qwen3.5:4b", size: 3_650_000_000 },
              ],
    };
  }
  return mockOllama;
}

/* ---- Ask ---- */
// The backend's own failure text (llm_commands.rs, ollama.rs, fly-llm), so
// the Ask sheet's human-readable errors can be checked here. Without a
// forced error the mock mirrors build_provider: a cloud provider is refused
// for a kept-local note, before consent, or without a key.
const MOCK_ASK_ERRORS: Record<string, string> = {
  keep_local:
    'this note is set to "Keep local", so it isn\'t sent to anthropic. Switch the AI provider to Ollama (runs on this device) in Settings, or turn off Keep local for this note',
  consent:
    "OpenAI isn't allowed to receive your meeting content yet, so nothing was sent. To allow it, open Settings › Ask and Enhance and click Test. To keep everything on this computer, switch Runs on to This computer (Ollama).",
  no_key: "No API key for OpenAI yet. Add one in Settings › Ask and Enhance.",
  auth: "authentication failed — check the API key",
  ollama_missing:
    "Ollama isn't installed — use the Install button in Settings → AI provider, or install it yourself from ollama.com",
  ollama_down:
    "Ollama isn't reachable at http://localhost:11434. Start the Ollama app (or install it from ollama.com), or switch provider in Settings.",
  model_missing:
    'provider returned an error: 404 Not Found: {"error":"model \\"llama3.1\\" not found, try pulling it first"}',
  offline: "Can't reach anthropic — check your internet connection.",
  timeout: "anthropic timed out. The model may still be loading — try again in a moment.",
  rate_limit:
    'provider returned an error: 429 Too Many Requests: {"type":"error","error":{"type":"rate_limit_error"}}',
  server: 'provider returned an error: 500 Internal Server Error: {"type":"error"}',
  other: "invalid request: the request couldn't be sent",
};
const MOCK_VENDORS: Record<string, string> = {
  anthropic: "Anthropic",
  openai: "OpenAI",
  nim: "NVIDIA",
};
const MOCK_ASK_ANSWER =
  "Here's what stands out:\n\n• Acme is happy with pilot accuracy — PMs dropped manual notes.\n• The blocker is renewal pricing + data-residency wording for legal.\n• Next: you send the revised SOW today; legal review booked Monday.";
function mockAsk(args: Record<string, unknown>): Promise<string> {
  const forced = mockFlag("fotwMockAskError");
  const provider = llmSettings.provider;
  const info = llmSettings.providers.find((p) => p.id === provider);
  const meetingId = noteMeta.find((n) => n.id === args.noteId)?.meeting_id ?? null;
  const privacy = meetingId ? meetingPrivacy(meetingId) : null;
  const keptLocal = !!privacy && (privacy.keep_local || privacy.keep_local_from_folder);
  let error = forced ? (MOCK_ASK_ERRORS[forced] ?? MOCK_ASK_ERRORS.other) : null;
  if (!error && info && !info.is_local) {
    if (keptLocal) error = MOCK_ASK_ERRORS.keep_local.replace("anthropic", provider);
    else if (mockConsentRefusal()) error = mockConsentRefusal();
    else if (!info.has_key)
      error = MOCK_ASK_ERRORS.no_key.replace("OpenAI", MOCK_VENDORS[provider] ?? provider);
  }
  return new Promise((resolve, reject) =>
    setTimeout(() => (error ? reject(error) : resolve(MOCK_ASK_ANSWER)), 700),
  );
}

/** build_provider's missing-consent refusal for the saved provider, or null
 *  (local, or allowed). Enhance, Test and backfill check it, like Ask. */
function mockConsentRefusal(): string | null {
  const provider = llmSettings.provider;
  const info = llmSettings.providers.find((p) => p.id === provider);
  if (!info || info.is_local || mockUiSettings[`consent.cloud_llm.${provider}`] === "true") {
    return null;
  }
  return MOCK_ASK_ERRORS.consent.replace("OpenAI", MOCK_VENDORS[provider] ?? provider);
}

/* ---- cloud consent ---- */
// The backend's own dialog words (llm_commands.rs cloud_consent_prompt).
function mockConsentPrompt(provider: string, atLaunch: boolean, host: string | null = null) {
  const vendor = MOCK_VENDORS[provider] ?? provider;
  const who = host ? `the server at ${host} (your custom ${vendor} endpoint)` : vendor;
  const disclosure =
    `Your notes and full meeting transcripts will be sent to ${who} for processing ` +
    `whenever you run Enhance or Ask, and automatically for transcript cleanup and ` +
    `action items after each transcription. Their data-retention terms apply (see their ` +
    `privacy / data-usage policy).\n\n` +
    `Recordings themselves stay on this computer, and so do notes you keep local. ` +
    `Choose This computer (Ollama) to keep everything local.`;
  return {
    title: `Use ${vendor} for Ask and Enhance?`,
    message: atLaunch
      ? `Your AI provider is set to ${vendor}, and nothing is sent there until you allow it. ` +
        `Until then, Enhance, Ask, transcript cleanup and action items won't run.\n\n${disclosure}`
      : disclosure,
  };
}

/** request_cloud_consent: true without asking for a local provider or one
 *  already allowed; an unknown id is refused; otherwise ask, and record only
 *  a yes. The app asks in a native dialog the page can't fake; the stand-in
 *  here is the browser's own confirm (this mock only runs in a plain
 *  browser, never under Tauri, where confirm is synchronous and real). */
function mockRequestCloudConsent(provider: string, atLaunch: boolean): boolean {
  const info = llmSettings.providers.find((p) => p.id === provider);
  if (!info) throw `unknown provider '${provider}'`;
  const key = `consent.cloud_llm.${provider}`;
  if (info.is_local || mockUiSettings[key] === "true") return true;
  const { title, message } = mockConsentPrompt(
    provider,
    atLaunch,
    customServerHost(provider, info.base_url),
  );
  // eslint-disable-next-line no-restricted-properties -- see above: browser-only stand-in
  if (!window.confirm(`${title}\n\n${message}`)) return false;
  mockUiSettings[key] = "true";
  return true;
}

/** build_provider's missing-key refusal for the saved provider, or null. */
function mockMissingKey(): string | null {
  const info = llmSettings.providers.find((p) => p.id === llmSettings.provider);
  if (!info || info.is_local || info.has_key) return null;
  return MOCK_ASK_ERRORS.no_key.replace("OpenAI", MOCK_VENDORS[info.id] ?? info.id);
}

/* ---- consent skips ---- */
// Meetings whose cleanup / action items the backend skipped for missing
// consent or a missing API key. fotwMockConsentSkips = "1" starts with m1
// skipped, so the notice shows; retry_consent_skipped replays the
// "polishing" → done events, and a meeting stays skipped while the saved
// cloud provider still has no key (as in the backend).
let mockConsentSkips: string[] = mockFlag("fotwMockConsentSkips") === "1" ? ["m1"] : [];

/* ---- recovered meetings kept local ---- */
// fotwMockRecoveredKeptLocal = "1": self-heal restored n1's meeting and set it
// to Keep local because its privacy setting couldn't be confirmed, so the
// review notice shows (privacy.rs recovered_kept_local).
let mockRecoveredKeptLocal: { meeting_id: string; note_id: string; title: string }[] =
  mockFlag("fotwMockRecoveredKeptLocal") === "1"
    ? [{ meeting_id: "m1", note_id: "n1", title: noteMeta[0].title }]
    : [];
function mockRetryConsentSkipped(): number {
  const ids = mockConsentSkips;
  mockConsentSkips = [];
  ids.forEach((id, i) => {
    setTimeout(() => {
      const base = { meeting_id: id, detail: null, error: null };
      mockEmit("pipeline:progress", { ...base, stage: "polishing", done: false });
      setTimeout(() => {
        if (mockMissingKey() && !mockConsentSkips.includes(id)) mockConsentSkips.push(id);
        mockEmit("pipeline:progress", { ...base, stage: null, done: true });
        mockEmit("llm:consent-skips", mockConsentSkips);
      }, 900);
    }, i * 1200);
  });
  return ids.length;
}

/* ---- command router ---- */
function handle(cmd: string, args: Record<string, unknown> = {}): unknown {
  // Stateful delete for the session, so Home's Undo-then-expire is visible
  // (the note stays gone until reload).
  if (cmd === "delete_note") {
    const i = noteMeta.findIndex((n) => n.id === args.id);
    if (i >= 0) noteMeta.splice(i, 1);
    return null;
  }
  // Block edits on the stored meeting blocks change them for the session (a
  // fresh copy back, like an IPC payload); anything else falls through to the
  // plain note reply in the switch below.
  if (cmd === "edit_note_block" || cmd === "keep_note_block") {
    const markdown = cmd === "keep_note_block" ? null : String(args.markdown ?? "");
    if (editMockBlock(String(args.blockId), markdown)) {
      return JSON.parse(JSON.stringify(note(String(args.noteId ?? "n1"))));
    }
    if (cmd === "keep_note_block") return note(String(args.noteId ?? "n1"));
  }
  switch (cmd) {
    case "ping":
      return "pong";
    case "app_info":
      return {
        version: "2.4.0",
        data_dir: "C:\\Users\\you\\AppData\\Roaming\\FlyOnTheWall",
        // fotwMockTapDenied demos the macOS-only SystemAudioNotice, which is
        // gated on app_info.os — so the flag flips the reported OS too.
        os:
          typeof localStorage !== "undefined" && localStorage.getItem("fotwMockTapDenied") === "1"
            ? "macos"
            : "windows",
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
      return null;
    case "cancel_transcription":
      return mockCancelTranscription(String(args.meetingId));
    case "pipeline_failure":
      return mockPipelineFailures[String(args.meetingId)] ?? null;
    case "transcribe_meeting":
      // re-enqueued: like the backend's job row, the parked failure is gone
      delete mockPipelineFailures[String(args.meetingId)];
      if (args.meetingId === IMPORT_MEETING_ID && mockImport?.started && !mockImportRunning) {
        mockImportRetry();
      }
      return null;
    case "list_recent_notes":
      if (mockFlag("fotwMockNoNotes") === "1") return [];
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
      return note(String(args.id ?? args.noteId ?? "n1"));
    case "enhance_note": {
      const refused = mockConsentRefusal() ?? mockMissingKey();
      if (refused) throw refused;
      return note(String(args.id ?? args.noteId ?? "n1"));
    }
    case "get_meeting_for_note":
      return meeting(String(args.noteId));
    case "get_meetings_for_note": {
      const m = meeting(String(args.noteId));
      return m ? [m] : [];
    }
    case "live_status":
      return null;
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
    case "preflight_system_audio":
      // fotwMockTapDenied = "1" → macOS stale-TCC-grant notice shows
      return typeof localStorage !== "undefined" &&
        localStorage.getItem("fotwMockTapDenied") === "1"
        ? { verdict: "silent_while_playing" }
        : { verdict: "inconclusive" };
    case "open_privacy_settings":
      return null;
    case "start_recording":
    case "start_meeting_from_event": {
      // Mirror the backend's seeding: the signed-in user and decliners are
      // skipped, names fall back to the address, list stays unconfirmed.
      const seed = (cmd === "start_recording" ? args.seed : args) as {
        attendees?: { email: string; name?: string; is_self: boolean; declined: boolean }[];
      } | null;
      if (seed?.attendees) {
        mockAttendees = seed.attendees
          .filter((a) => !a.is_self && !a.declined)
          .map((a) => ({ name: a.name?.trim() || a.email, email: a.email }));
        mockAttendeesConfirmed = false;
      }
      // fotwMockNoAttendees: no calendar event, so the backend starts the
      // meeting with no attendees (unconfirmed)
      if (mockFlag("fotwMockNoAttendees") === "1") {
        mockAttendees = [];
        mockAttendeesConfirmed = false;
      }
      mockRecPaused = false;
      if (typeof localStorage !== "undefined") localStorage.setItem("fotwMockRecording", "1");
      return recordingStatus();
    }
    case "recording_status":
      return recordingStatus();
    case "screen_status":
      return typeof localStorage !== "undefined" &&
        localStorage.getItem("fotwMockRecording") === "1"
        ? { active: true, note_id: "n1", elapsed_ms: 132_000 }
        : { active: false, note_id: null, elapsed_ms: 0 };
    case "get_asr_settings":
      // a copy, like a fresh IPC payload (the UI must not see later mutation)
      return JSON.parse(JSON.stringify(asrSettings));
    case "set_asr_settings":
      return setAsrSettings(args.update as Parameters<typeof setAsrSettings>[0]);
    case "get_llm_settings":
      return JSON.parse(JSON.stringify(llmSettings));
    // Stateful so the Settings save/guided-swap flow is testable in the mock.
    case "set_llm_settings": {
      const u = args.update as {
        provider: string;
        model: string | null;
        base_url: string | null;
        api_key: string | null;
      };
      const info = llmSettings.providers.find((p) => p.id === u.provider);
      // Same rule as llm_commands.rs apply_llm_settings: a saved key never
      // follows the Base URL to another server unless it's entered again.
      if (
        info &&
        u.api_key == null &&
        serverChangeNeedsKey(u.provider, info.has_key, info.base_url, u.base_url ?? "")
      ) {
        throw `To use a different server for ${providerVendor(u.provider)}, enter the API key again. Nothing was changed.`;
      }
      llmSettings.provider = u.provider;
      if (info) {
        if (serverChanged(u.provider, info.base_url, u.base_url ?? "")) {
          // the old key and consent never carry over to the new server
          info.has_key = false;
          delete mockUiSettings[`consent.cloud_llm.${u.provider}`];
        }
        info.model = u.model;
        info.base_url = u.base_url;
        if (u.api_key != null) info.has_key = u.api_key !== "";
      }
      return null;
    }
    case "get_calendar_settings":
      return { ...calendarStatus };
    case "set_calendar_settings": {
      const u = args.update as {
        google_client_id: string;
        google_client_secret: string | null;
        ms_client_id: string;
      };
      calendarStatus.google_client_id = u.google_client_id;
      calendarStatus.ms_client_id = u.ms_client_id;
      if (u.google_client_secret) calendarStatus.google_has_secret = true;
      return null;
    }
    case "connect_calendar":
      if (args.provider === "google") calendarStatus.google_connected = true;
      if (args.provider === "msgraph") {
        calendarStatus.ms_connected = true;
        mockCalendars = [
          ...mockCalendars.filter((c) => c.provider !== "msgraph"),
          { provider: "msgraph", id: "cal", name: "Calendar", primary: true, enabled: true },
        ];
      }
      return null;
    case "disconnect_calendar":
      if (args.provider === "google") calendarStatus.google_connected = false;
      if (args.provider === "msgraph") calendarStatus.ms_connected = false;
      mockCalendars = mockCalendars.filter((c) => c.provider !== args.provider);
      return null;
    case "list_calendars":
      return mockCalendars.map((c) => ({ ...c }));
    case "set_calendar_enabled":
      mockCalendars = mockCalendars.map((c) =>
        c.provider === args.provider && c.id === args.calendarId
          ? { ...c, enabled: !!args.enabled }
          : c,
      );
      return null;
    case "upcoming_meetings":
      return upcoming();
    case "list_templates":
      return templates.map((t) => ({ ...t }));
    case "save_template": {
      const t = args.template as (typeof templates)[number];
      if (t.id) templates = templates.map((x) => (x.id === t.id ? { ...t } : x));
      else templates = [...templates, { ...t, id: `tpl-${Date.now()}`, built_in: false }];
      return null;
    }
    case "delete_template":
      templates = templates.filter((t) => t.id !== args.id);
      return null;
    case "list_mic_devices":
      return [
        { id: "d1", name: "MacBook Pro Microphone", is_default: true },
        { id: "d2", name: "AirPods Pro", is_default: false },
      ];
    case "mcp_config":
      return mcpSnippet;
    case "ask_meeting":
      return mockAsk(args);
    case "test_llm_connection": {
      const refused = mockConsentRefusal();
      if (refused) throw refused;
      return `Connected to ${llmSettings.provider}. It answered in 0.8 s.`;
    }
    case "recovered_kept_local":
      return mockRecoveredKeptLocal.map((m) => ({ ...m }));
    case "dismiss_recovered_kept_local": {
      const id = args.meetingId as string | null;
      mockRecoveredKeptLocal = id ? mockRecoveredKeptLocal.filter((m) => m.meeting_id !== id) : [];
      return mockRecoveredKeptLocal.map((m) => ({ ...m }));
    }
    case "consent_skipped_meetings":
      return [...mockConsentSkips];
    case "retry_consent_skipped":
      return mockRetryConsentSkipped();
    case "request_cloud_consent":
      return mockRequestCloudConsent(String(args.provider), args.atLaunch === true);
    case "ollama_status": {
      const o = ollamaState();
      return {
        installed: o.installed,
        can_install: true,
        running: o.installed,
        managed: true,
        base_url: "http://localhost:11434",
        models: o.models.map((m) => ({ ...m })),
      };
    }
    case "ollama_pull": {
      const o = ollamaState();
      const name = String(args.model);
      if (!o.models.some((m) => m.name.split(":")[0] === name.split(":")[0]))
        o.models.push({ name: name.includes(":") ? name : `${name}:latest`, size: 4_700_000_000 });
      return null;
    }
    case "ollama_delete": {
      const o = ollamaState();
      const name = String(args.model);
      o.models = o.models.filter((m) => m.name !== name && m.name.split(":")[0] !== name);
      return null;
    }
    case "get_app_setting":
      if (args.key === "consent.recording_notice_accepted") {
        return typeof localStorage !== "undefined" &&
          localStorage.getItem("fotwMockFirstRun") === "1"
          ? null
          : "true";
      }
      return mockUiSettings[String(args.key)] ?? null;
    case "set_app_setting":
      // Cloud consent is backend-written (commands.rs
      // is_webview_writable_setting_key): only request_cloud_consent sets it.
      if (String(args.key).toLowerCase().startsWith("consent.cloud_llm.")) {
        throw `setting '${String(args.key)}' can't be changed from the app UI`;
      }
      mockUiSettings[String(args.key)] = String(args.value);
      return null;
    case "download_model": {
      const m = asrSettings.models.find((x) => x.id === args.id);
      if (m) m.installed = true;
      if (args.id === WHISPER_ENGINE_ID) mockEngineInstalled = true;
      if (args.id === "ollama-bin") ollamaState().installed = true;
      return "";
    }
    case "reveal_logs_dir":
    case "reveal_data_dir":
      return null;
    case "list_capture_windows":
      return [
        "Weekly sync - Zoom",
        "IDB VDI IT - VMware Horizon Client",
        "budget notes.xlsx - Excel",
      ];
    case "export_note":
      return "C:\\Users\\you\\Desktop\\note.md";
    case "export_transcript_vtt":
      return "C:\\Users\\you\\Desktop\\transcript.vtt";
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
    case "pending_backfill_count":
      return mockBackfillPending;
    case "backfill_meeting_items": {
      const processed = mockBackfillPending;
      const refused = mockConsentRefusal();
      if (refused) return { processed, extracted: 0, failed: processed, first_error: refused };
      mockBackfillPending = 0;
      return { processed, extracted: processed * 4, failed: 0, first_error: null };
    }
    case "get_meeting_privacy":
      return meetingPrivacy(String(args.meetingId));
    case "set_note_keep_local": {
      for (const n of noteMeta.filter((m) => m.id === args.noteId && m.meeting_id)) {
        meetingPrivacy(n.meeting_id as string);
        mockPrivacy[n.meeting_id as string].keep_local = !!args.keepLocal;
      }
      return null;
    }
    case "set_participants_notified":
      meetingPrivacy(String(args.meetingId));
      mockPrivacy[String(args.meetingId)].participants_notified = !!args.notified;
      return null;
    case "set_folder_keep_local":
      if (args.keepLocal) mockLocalFolders.add(String(args.folderId));
      else mockLocalFolders.delete(String(args.folderId));
      return null;
    case "list_local_only_folders":
      return [...mockLocalFolders];
    case "copy_recording_notice":
      void navigator.clipboard?.writeText(String(args.text ?? "")).catch(() => {});
      return null;
    case "get_privacy_settings":
      return {
        audio_retention_days: mockRetentionDays,
        retention_choices: [7, 30, 90, 365],
        retention_held: mockFlag("fotwMockRetentionHeld") === "1" ? 6 : null,
        last_wipe: null,
      };
    case "preview_audio_retention":
      return Number(args.days) <= 30 ? 4 : 0;
    case "set_audio_retention":
      mockRetentionDays = (args.days as number | null) ?? null;
      return {
        meetings: mockRetentionDays && mockRetentionDays <= 30 ? 4 : 0,
        files_removed: mockRetentionDays && mockRetentionDays <= 30 ? 9 : 0,
        bytes_removed: mockRetentionDays && mockRetentionDays <= 30 ? 812_000_000 : 0,
        meetings_failed: 0,
        held: null,
      };
    case "dismiss_wipe_report":
      return null;
    case "delete_all_data":
      if (args.confirmation !== "delete everything") throw new Error("confirmation mismatch");
      return {
        removed: 42,
        failed: [],
        secrets_failed: [],
        restart_required: true,
        db_erased: true,
        // fotwMockWipeDbBusy = "1" → an MCP client holds the database open
        db_checkpointed: mockFlag("fotwMockWipeDbBusy") !== "1",
        notes: [],
      };
    case "disk_encryption_status": {
      const s = mockFlag("fotwMockEncryption");
      return {
        state: s === "on" || s === "off" ? s : "unknown",
        method: "BitLocker",
        volume: "C:",
      };
    }
    // Updater plugin (notice stack QA). fotwMockUpdate = "available" → the
    // startup check (5 s after launch) finds v2.4.0 and Update now hangs in
    // "Downloading"; "fail" → the download fails ("Update didn't install").
    // Unset → no update (the plugin's null).
    case "plugin:updater|check": {
      const u = mockFlag("fotwMockUpdate");
      return u === "available" || u === "fail"
        ? { rid: 1, currentVersion: "2.3.0", version: "2.4.0", date: null, body: "", rawJson: {} }
        : null;
    }
    case "plugin:updater|download":
      return mockFlag("fotwMockUpdate") === "fail"
        ? Promise.reject(new Error("download failed: connection reset by peer"))
        : new Promise(() => {});
    case "pause_recording":
      mockRecPaused = true;
      return recordingStatus();
    case "resume_recording":
      mockRecPaused = false;
      return recordingStatus();
    case "stop_recording":
      // the capture ends; the meeting comes back like the backend's
      mockRecPaused = false;
      if (typeof localStorage !== "undefined") localStorage.removeItem("fotwMockRecording");
      // fotwMockAudioLost=1: the capture had gaps (recording.rs
      // record_capture_health emits this right before returning)
      if (mockFlag("fotwMockAudioLost") === "1") {
        mockEmit("recording:audio_lost", {
          meeting_id: "m1",
          note_id: "n1",
          mic_lost_ms: 1200,
          system_lost_ms: 400,
        });
      }
      return meeting("n1");
    default:
      return null;
  }
}

/* ---- import: one file fails to convert (fotwMockImportFail) ----
   Mirrors import_transcribe's per-file attribution: every file in the order
   is converted, the bad one emits import:progress "failed" with its cause,
   and the call resolves with the queue back at idle (started: false) so the
   row shows its cause and the queue offers Retry. Fails once per page load. */
let mockImportFailedOnce = false;
const MOCK_CONVERT_CAUSE = "This file looks damaged or incomplete, so its audio can't be read.";
function mockImportConvertFailure(order: string[]) {
  if (!mockImport || mockImportFailedOnce || mockFlag("fotwMockImportFail") !== "1") return null;
  if (!order.includes("if-2")) return null;
  mockImportFailedOnce = true;
  const current = mockImport;
  const files = order
    .map((id) => current.files.find((f) => f.id === id))
    .filter((f): f is MockImportFile => f != null && f.error == null)
    .map((f) => ({ ...f, convert_error: f.id === "if-2" ? MOCK_CONVERT_CAUSE : null }));
  return new Promise((resolve) => {
    let t = 200;
    for (const f of files) {
      const id = f.id;
      window.setTimeout(
        () =>
          mockEmit("import:progress", {
            meeting_id: IMPORT_MEETING_ID,
            file_id: id,
            stage: "converting",
          }),
        t,
      );
      t += 900;
      window.setTimeout(
        () =>
          mockEmit("import:progress", {
            meeting_id: IMPORT_MEETING_ID,
            file_id: id,
            stage: id === "if-2" ? "failed" : "converted",
            error: id === "if-2" ? MOCK_CONVERT_CAUSE : null,
          }),
        t,
      );
    }
    window.setTimeout(() => {
      mockImport = { ...current, files, boundaries_ms: [], started: false };
      resolve(mockImport);
    }, t + 400);
  });
}

/* ---- recording bar QA ----
   fotwMockRecWarning swaps the active capture's warnings for the backend's
   exact sentences (fly-audio cpal_backend.rs / tap_messages.rs), so each
   warning chip can be checked; pause_recording / resume_recording round-trip
   through mockRecPaused. Returns null to keep the original fixture. */
let mockRecPaused = false;
const MOCK_REC_WARNINGS: Record<string, string[]> = {
  none: [],
  muted: [
    "System output is muted — the other participants' audio will record as silence. Unmute your speakers.",
  ],
  volume: [
    "System volume is at 0% — the other participants' audio will record as silence. Raise your system volume.",
  ],
  loopback: [
    "System audio capture couldn't start (no default output device) — only your microphone is being recorded, so the other participants won't be in this recording.",
  ],
  worst: [
    "System audio capture couldn't start (no default output device) — only your microphone is being recorded, so the other participants won't be in this recording.",
    "System volume is at 0% — the other participants' audio will record as silence. Raise your system volume.",
  ],
  macos: [
    "System audio is playing but its capture is recording only silence — macOS is blocking system-audio capture for this build, so the other participants may be missing from this recording. Open System Settings → Privacy & Security → Screen & System Audio Recording and allow Fly on the Wall. If it already shows as allowed, the grant belongs to a previously installed build: remove Fly on the Wall from that list with the − button and re-add it (or run `tccutil reset ScreenCapture com.flyonthewall.app` in Terminal), then relaunch.",
  ],
  // device_lost_warning / write_failed_warning (crates/fly-audio/src/cpal_backend.rs)
  device: [
    "The sound output changed or disconnected — the other participants may be missing from the rest of this recording. Stop and record again to follow the current output.",
  ],
  disk: [
    "Part of your microphone audio couldn't be saved — the disk may be full or unavailable. Free up space; the recording keeps trying.",
  ],
  // lost_audio_warning (crates/fly-audio/src/cpal_backend.rs)
  lost: [
    "Your microphone has gaps: 1.2 s was lost because the computer was too busy (most recently at 2:31). The rest is still recording — closing heavy apps can help.",
    "The other participants' audio has gaps: 0.4 s was lost because the computer was too busy (most recently at 2:31). The rest is still recording — closing heavy apps can help.",
  ],
};
function mockRecordingQa() {
  if (mockFlag("fotwMockRecording") !== "1") return null;
  const which = mockFlag("fotwMockRecWarning");
  if (which == null && !mockRecPaused) return null;
  return {
    active: true,
    state: mockRecPaused ? "paused" : "recording",
    elapsed_ms: which === "worst" ? 3_723_000 : 154_000,
    meeting_id: "m1",
    note_id: "n1",
    warnings: which != null ? (MOCK_REC_WARNINGS[which] ?? []) : (MOCK_REC_WARNINGS.muted ?? []),
  };
}

function installDevMock() {
  // fotwMockCalendar = "none": start with no calendar connected (see above).
  if (mockFlag("fotwMockCalendar") === "none") calendarStatus.google_connected = false;
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
