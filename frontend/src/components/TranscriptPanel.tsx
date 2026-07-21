import { useEffect, useRef, useState } from "react";
import type { Meeting, ModelProgress, Transcript } from "../types";
import { ChevronDown, Plus, RefreshCw } from "lucide-react";
import { fmtElapsed } from "./RecordingBar";
import { Avatar, Button, ProgressBar, SectionLabel, speakerColor } from "./ui";
import { briefError, selectPipelineNotice } from "../pipelineNotice";
import { WHISPER_ENGINE_ID } from "../types";

interface Props {
  meeting: Meeting;
  transcript: Transcript | null;
  /** LLM-polished variant, shown by default when it exists (same segment
   *  ids/speakers/timestamps as the raw transcript — only the text differs). */
  cleaned: Transcript | null;
  /** Current pipeline stage, or null when idle. */
  stage: string | null;
  /** One-line stage detail (channel being transcribed, GPU benchmark result, CPU fallback notice). */
  stageDetail: string | null;
  modelProgress: ModelProgress | null;
  pipelineError: string | null;
  /** whisper-cli engine readiness — null until the first settings fetch. When
   *  a transcribe fails and the engine isn't installed, the error turns into an
   *  actionable install/setup prompt instead of a raw message. */
  engine: { installed: boolean; managed: boolean } | null;
  /** True while an in-app engine install is streaming. */
  engineInstalling: boolean;
  /** Zoom-in: segment ids to highlight + scroll to (AI block sources). */
  highlightIds: string[];
  onTranscribe: () => void;
  /** Install the whisper.cpp engine in-app (only meaningful when engine.managed). */
  onInstallEngine: () => void;
  /** Open Settings deep-linked to the transcription-engine row or the Groq
   *  option (in a state where the user can actually enable it). */
  onOpenSettings: (focus: "engine" | "groq") => void;
  onRelabel: (speakerKey: string, label: string) => void;
  /** "Someone else…" in the speaker dropdown: adds a new attendee AND
   *  assigns them to the speaker in one step. */
  onAssignNewAttendee: (speakerKey: string, name: string) => void;
  /** Persist an edited transcript line (called on blur when the text changed). */
  onEditSegment: (segmentId: string, text: string) => void;
}

/** App-generated labels ("Speaker 3", "Unknown", raw key) — mirrors the
 *  backend's is_generic_label; anything else is a user assignment. */
export function isGenericLabel(key: string, label: string): boolean {
  return label === key || label === "Unknown" || /^Speaker \d+$/.test(label);
}

const STAGE_LABELS: Record<string, string> = {
  waiting: "Waiting to transcribe (recording comes first)…",
  starting: "Starting…",
  "ensuring-models": "Preparing models…",
  benchmarking: "Testing GPU vs CPU speed (one time)…",
  "preparing-audio": "Preparing audio…",
  transcribing: "Transcribing",
  diarizing: "Detecting speakers…",
  aligning: "Assigning words to speakers…",
  saving: "Saving transcript…",
  polishing: "AI cleanup — polishing the transcript…",
};

/** One choice in the speaker dropdown. */
interface SpeakerOption {
  name: string;
  color: string;
}

/** Speaker label as a dropdown of the attendee list (design state 5).
 *  Assigned labels render as name + speaker color; unassigned stay muted in
 *  a quiet box. Choosing a name applies to ALL of that speaker's lines (it's
 *  a label set on the stable key). "Someone else…" adds a new attendee and
 *  assigns them in one step. */
function SpeakerMenu({
  speakerKey,
  label,
  color,
  options,
  onAssign,
  onAssignNew,
}: {
  speakerKey: string;
  label: string;
  color: string;
  options: SpeakerOption[];
  onAssign: (name: string) => void;
  onAssignNew: (name: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [adding, setAdding] = useState(false);
  const [newName, setNewName] = useState("");
  const generic = isGenericLabel(speakerKey, label);

  useEffect(() => {
    if (!open) {
      setAdding(false);
      setNewName("");
    }
  }, [open]);

  const commit = (name: string) => {
    setOpen(false);
    if (name && name !== label) onAssign(name);
  };
  const commitNew = () => {
    const name = newName.trim();
    setOpen(false);
    if (name) onAssignNew(name);
  };

  return (
    <span className="relative inline-flex">
      <button
        onClick={() => setOpen((o) => !o)}
        title="Who is this? Applies to all of this speaker's lines"
        aria-haspopup="menu"
        aria-expanded={open}
        className="inline-flex cursor-pointer items-center gap-1 border-0 text-xs font-semibold"
        style={
          generic
            ? {
                background: "var(--surface-3)",
                color: "var(--text-2)",
                padding: "1px 7px",
                borderRadius: "var(--radius-sm)",
              }
            : {
                background: "transparent",
                color,
                padding: 0,
                borderBottom: "1px dashed transparent",
              }
        }
        onMouseEnter={(e) => {
          if (!generic) e.currentTarget.style.borderBottomColor = "currentcolor";
        }}
        onMouseLeave={(e) => {
          if (!generic) e.currentTarget.style.borderBottomColor = "transparent";
        }}
      >
        {label}
        <ChevronDown size={9} strokeWidth={2} />
      </button>
      {open && (
        <>
          <span className="fixed inset-0 z-10" onClick={() => setOpen(false)} aria-hidden="true" />
          <div
            role="menu"
            className="absolute left-0 z-20 p-1"
            style={{
              top: "calc(100% + 5px)",
              width: 236,
              background: "var(--surface)",
              border: "1px solid var(--line)",
              borderRadius: "var(--radius-lg)",
              boxShadow: "var(--shadow-pop)",
            }}
          >
            <div
              className="px-2.5 pb-1 pt-1.5 text-[10.5px] font-semibold uppercase"
              style={{ letterSpacing: ".09em", color: "var(--text-3)" }}
            >
              Who is this?
            </div>
            {options.map((o) => (
              <button
                key={o.name}
                role="menuitem"
                onClick={() => commit(o.name)}
                className="flex w-full cursor-pointer items-center gap-2 border-0 bg-transparent px-2.5 py-[7px] text-left text-[13px] text-text hover:bg-surface-3"
                style={{
                  borderRadius: "var(--radius-sm)",
                  background: o.name === label ? "var(--surface-3)" : undefined,
                }}
              >
                <span
                  className="h-[9px] w-[9px] flex-none rounded-full"
                  style={{ background: o.color }}
                />
                {o.name}
              </button>
            ))}
            <div className="mx-1.5 mt-1 border-t border-line pb-1 pt-1.5">
              {adding ? (
                <input
                  autoFocus
                  value={newName}
                  placeholder="Name"
                  aria-label="New attendee name"
                  onChange={(e) => setNewName(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      commitNew();
                    }
                    if (e.key === "Escape") setAdding(false);
                  }}
                  onBlur={() => (newName.trim() ? commitNew() : setAdding(false))}
                  className="w-full rounded-md border border-primary bg-surface px-2 py-1 text-[13px] outline-none"
                  style={{ boxSizing: "border-box" }}
                />
              ) : (
                <button
                  role="menuitem"
                  onClick={() => setAdding(true)}
                  className="inline-flex w-full cursor-pointer items-center gap-1 border-0 bg-transparent px-1 py-0.5 text-left text-[12.5px] font-semibold"
                  style={{ color: "var(--primary-text)" }}
                >
                  <Plus size={12} strokeWidth={2.25} /> Someone else…
                </button>
              )}
            </div>
            <div
              className="mx-1.5 border-t px-1 pb-1 pt-1.5 font-mono text-[10px]"
              style={{ borderColor: "var(--line-2)", color: "var(--text-3)" }}
            >
              applies to all {label} lines
            </div>
          </div>
        </>
      )}
    </span>
  );
}

/** Actionable replacement for the raw "whisper-cli is not installed" error:
 *  explains the engine-vs-weights distinction and offers a one-click install
 *  (when the OS can manage it) plus a Settings deep-link. Falls back to manual
 *  guidance where no managed binary exists yet (macOS/Linux pre-hosting). */
function EngineMissingNotice({
  engine,
  installing,
  installPct,
  installError,
  onInstall,
  onOpenSettings,
}: {
  engine: { installed: boolean; managed: boolean };
  installing: boolean;
  installPct: number | null;
  /** A failed in-app install's (briefed) error — shown instead of vanishing. */
  installError: string | null;
  onInstall: () => void;
  onOpenSettings: (focus: "engine" | "groq") => void;
}) {
  return (
    <div
      className="mt-2 rounded-lg border border-line px-3 py-2.5 text-[13px]"
      style={{ background: "var(--error-soft)", color: "var(--error-text)" }}
      role="alert"
    >
      <div className="font-semibold">Transcription engine not installed</div>
      <div className="mt-1" style={{ lineHeight: 1.5 }}>
        Your recording and the downloaded model are ready, but the whisper.cpp engine that runs them
        isn&apos;t installed yet. It runs fully on this machine — nothing is uploaded.
      </div>
      {installError && (
        <div className="mt-1" style={{ lineHeight: 1.5 }}>
          The install didn&apos;t finish: {installError}
        </div>
      )}
      {installing ? (
        <div className="mt-2 flex items-center gap-2">
          {installPct !== null ? (
            <>
              <ProgressBar value={installPct} style={{ width: 140 }} />
              <span style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}>{installPct}%</span>
            </>
          ) : (
            <span style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}>installing…</span>
          )}
        </div>
      ) : (
        <div className="mt-2 flex flex-wrap items-center gap-2">
          {engine.managed && (
            <Button variant="primary" size="sm" onClick={onInstall}>
              Install engine
            </Button>
          )}
          <Button variant="outline" size="sm" onClick={() => onOpenSettings("engine")}>
            Set up in Settings
          </Button>
        </div>
      )}
      {!engine.managed && !installing && (
        <div className="mt-2" style={{ fontSize: 12, lineHeight: 1.5 }}>
          Or install it yourself — macOS:{" "}
          <code style={{ fontFamily: "var(--font-mono)" }}>brew install whisper-cpp</code>. Then
          transcribe again.
        </div>
      )}
      <GroqHint onOpenSettings={onOpenSettings} />
    </div>
  );
}

/** Shared footer: the cloud escape hatch, with its privacy trade-off named.
 *  A real <button> (keyboard-reachable), styled as an inline text link. */
function GroqHint({ onOpenSettings }: { onOpenSettings: (focus: "engine" | "groq") => void }) {
  return (
    <div className="mt-2" style={{ fontSize: 12, lineHeight: 1.5 }}>
      Or use{" "}
      <button
        type="button"
        className="cursor-pointer underline"
        style={{
          background: "none",
          border: "none",
          padding: 0,
          font: "inherit",
          color: "inherit",
        }}
        onClick={() => onOpenSettings("groq")}
      >
        Groq cloud transcription
      </button>{" "}
      (Settings) — works without local models, but audio leaves this machine.
    </div>
  );
}

/** Actionable notice for a failed model download (offline, CDN outage): the
 *  raw error is collapsed to its human part (signed CDN URLs can be hundreds
 *  of characters) and paired with a retry, a Settings deep-link, and the
 *  Groq escape hatch. */
function DownloadFailedNotice({
  error,
  onRetry,
  onOpenSettings,
}: {
  error: string;
  onRetry: () => void;
  onOpenSettings: (focus: "engine" | "groq") => void;
}) {
  return (
    <div
      className="mt-2 rounded-lg border border-line px-3 py-2.5 text-[13px]"
      style={{ background: "var(--error-soft)", color: "var(--error-text)" }}
      role="alert"
    >
      <div className="font-semibold">Model download failed</div>
      <div className="mt-1" style={{ lineHeight: 1.5 }}>
        {briefError(error)}
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <Button variant="primary" size="sm" onClick={onRetry}>
          Try again
        </Button>
        <Button variant="outline" size="sm" onClick={() => onOpenSettings("engine")}>
          Set up in Settings
        </Button>
      </div>
      <GroqHint onOpenSettings={onOpenSettings} />
    </div>
  );
}

export default function TranscriptPanel({
  meeting,
  transcript,
  cleaned,
  stage,
  stageDetail,
  modelProgress,
  pipelineError,
  engine,
  engineInstalling,
  highlightIds,
  onTranscribe,
  onInstallEngine,
  onOpenSettings,
  onRelabel,
  onAssignNewAttendee,
  onEditSegment,
}: Props) {
  const segRefs = useRef<Map<string, HTMLDivElement>>(new Map());
  // Cleaned-by-default when the polish pass has run; "Raw" shows the exact
  // ASR output. Edits apply to both variants (same segment ids).
  const [showRaw, setShowRaw] = useState(false);

  // Zoom-in: scroll the first highlighted source segment into view.
  useEffect(() => {
    if (highlightIds.length === 0) return;
    const el = segRefs.current.get(highlightIds[0]);
    el?.scrollIntoView({ behavior: "smooth", block: "center" });
  }, [highlightIds]);

  const pct =
    modelProgress && modelProgress.stage === "downloading" && modelProgress.total > 0
      ? Math.round((modelProgress.downloaded / modelProgress.total) * 100)
      : null;

  // Which notice a pipeline failure deserves — decided on the error CONTENT
  // by the pure selector (pipelineNotice.ts) so Groq failures and unrelated
  // errors never get mislabeled as an engine problem. The engine notice also
  // needs the engine object itself for the managed/manual split.
  const notice = selectPipelineNotice({
    pipelineError,
    engineInstalling,
    engineInstalled: engine ? engine.installed : null,
  });
  const engineMissing = notice.kind === "engine-missing" && !!engine;
  const downloadFailed = notice.kind === "download-failed";
  const installPct =
    modelProgress && modelProgress.id === WHISPER_ENGINE_ID && modelProgress.total > 0
      ? Math.round((modelProgress.downloaded / modelProgress.total) * 100)
      : null;

  const stageBanner = stage ? (
    <div
      className="print:hidden mb-4 rounded-lg border px-3.5 py-3"
      style={{ background: "var(--primary-soft)", borderColor: "var(--primary-border)" }}
    >
      <div
        className="flex flex-wrap items-center gap-2 text-[13px] font-medium"
        style={{ color: "var(--primary-soft-text)" }}
      >
        <span
          className="h-2 w-2 shrink-0 rounded-full"
          style={{ background: "var(--primary)", animation: "fly-pulse-dot 1.2s ease infinite" }}
        />
        <span>{STAGE_LABELS[stage] ?? stage}</span>
        {stageDetail && (
          <span className="font-normal" style={{ color: "var(--text-3)" }}>
            — {stageDetail}
          </span>
        )}
        {pct !== null && (
          <span className="font-normal" style={{ color: "var(--text-3)" }}>
            downloading {modelProgress?.id} — {pct}%
          </span>
        )}
      </div>
      {pct !== null && (
        <div className="mt-2">
          <ProgressBar value={pct} />
        </div>
      )}
    </div>
  ) : null;

  // During AI cleanup the new raw transcript is already saved and readable —
  // show the banner ABOVE it instead of hiding the transcript. Every earlier
  // stage still replaces the view (there's nothing current to show yet).
  if (stage && !(stage === "polishing" && transcript)) {
    return stageBanner;
  }

  if (!transcript) {
    if (!meeting.recording) return null;
    return (
      <div
        className="mb-4 rounded-xl border border-line px-4 py-3"
        style={{ background: "var(--surface-2)" }}
      >
        <div className="flex flex-wrap items-center gap-3">
          <Button variant="primary" size="sm" onClick={onTranscribe}>
            Transcribe recording
          </Button>
          <span className="text-xs" style={{ color: "var(--text-3)" }}>
            Runs fully on this machine — audio never leaves your computer.
          </span>
        </div>
        {engineMissing ? (
          <EngineMissingNotice
            engine={engine!}
            installing={engineInstalling}
            installPct={installPct}
            installError={notice.installError}
            onInstall={onInstallEngine}
            onOpenSettings={onOpenSettings}
          />
        ) : downloadFailed ? (
          <DownloadFailedNotice
            error={pipelineError!}
            onRetry={onTranscribe}
            onOpenSettings={onOpenSettings}
          />
        ) : (
          pipelineError && (
            <div
              className="mt-2 rounded-lg border border-line px-3 py-2 text-[13px]"
              style={{ background: "var(--error-soft)", color: "var(--error-text)" }}
            >
              {pipelineError}
            </div>
          )
        )}
      </div>
    );
  }

  const shown = !showRaw && cleaned ? cleaned : transcript;
  // Stable per-speaker index (position in the transcript's speaker list).
  const speakerIndex = (key: string) => {
    const idx = shown.speakers.findIndex((s) => s.key === key);
    return Math.max(idx, 0);
  };
  // Dropdown choices: You first, then the attendee list (colors follow the
  // same rotation the avatars use).
  const speakerOptions: SpeakerOption[] = [
    { name: "You", color: "var(--spk-self)" },
    ...meeting.attendees
      .map((a, i) => ({
        name: (a.name.trim() || a.email || "").trim(),
        color: speakerColor(`att_${i}`, i),
      }))
      .filter((o) => o.name),
  ];

  return (
    <div>
      {stage === "polishing" && stageBanner}
      {/* Re-transcription failures get the same actionable notices as a first
          transcription (engine install / download retry / Groq link) — the raw
          error box is only the fallback for errors neither notice covers. */}
      {engineMissing || downloadFailed ? (
        <div className="print:hidden mb-4">
          <div className="text-[12.5px]" style={{ color: "var(--text-3)" }}>
            Re-transcription failed — showing the previous transcript.
          </div>
          {engineMissing ? (
            <EngineMissingNotice
              engine={engine!}
              installing={engineInstalling}
              installPct={installPct}
              installError={notice.installError}
              onInstall={onInstallEngine}
              onOpenSettings={onOpenSettings}
            />
          ) : (
            <DownloadFailedNotice
              error={pipelineError!}
              onRetry={onTranscribe}
              onOpenSettings={onOpenSettings}
            />
          )}
        </div>
      ) : (
        pipelineError && (
          <div
            className="print:hidden mb-4 rounded-lg border border-line px-3 py-2 text-[13px]"
            style={{ background: "var(--error-soft)", color: "var(--error-text)" }}
            role="alert"
          >
            Re-transcription failed — showing the previous transcript. {briefError(pipelineError)}
          </div>
        )
      )}
      <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-baseline gap-2">
          <SectionLabel>Transcript</SectionLabel>
          <span className="text-[11px]" style={{ color: "var(--text-3)" }}>
            · click any line or name to edit
          </span>
        </div>
        <div className="flex items-center gap-2">
          {cleaned && (
            <div
              className="flex items-center gap-[2px] rounded-full border border-line bg-surface p-[2px]"
              title="AI cleanup ran on this transcript — toggle between the polished text and the original transcription (edits apply to both)"
            >
              {(
                [
                  [false, "AI-cleaned"],
                  [true, "Raw"],
                ] as const
              ).map(([raw, label]) => (
                <button
                  key={label}
                  onClick={() => setShowRaw(raw)}
                  className={`cursor-pointer rounded-full border-0 px-2.5 py-1 text-[11.5px] font-semibold ${
                    showRaw === raw ? "bg-primary text-on-primary" : "bg-transparent text-text-2"
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>
          )}
          <Button
            variant="outline"
            size="sm"
            title="Re-process this recording with the current model settings"
            startIcon={<RefreshCw size={13} strokeWidth={1.75} />}
            onClick={() => {
              if (
                confirm(
                  "Re-run transcription for this meeting?\n\nThis reprocesses the recording with your current model settings and replaces the transcript — manual line edits and speaker renames for this meeting will be lost.",
                )
              ) {
                onTranscribe();
              }
            }}
          >
            Re-run transcription
          </Button>
        </div>
      </div>
      {shown.segments.map((seg) => {
        const isSelf = seg.speaker_key === "mic";
        const idx = speakerIndex(seg.speaker_key);
        const color = speakerColor(seg.speaker_key, idx);
        const label =
          shown.speakers.find((s) => s.key === seg.speaker_key)?.label ?? seg.speaker_key;
        const hot = highlightIds.includes(seg.id);
        return (
          <div
            key={seg.id}
            ref={(el) => {
              if (el) segRefs.current.set(seg.id, el);
              else segRefs.current.delete(seg.id);
            }}
            className={`mb-3.5 flex rounded-lg ${isSelf ? "justify-end" : "justify-start"}`}
            style={
              hot
                ? {
                    background: "var(--primary-soft)",
                    outline: "1.5px solid var(--primary)",
                    padding: 4,
                  }
                : undefined
            }
          >
            <div className="min-w-0" style={{ maxWidth: "86%" }}>
              <div className={`mb-1 flex items-center gap-1.5 ${isSelf ? "justify-end" : ""}`}>
                {!isSelf && isGenericLabel(seg.speaker_key, label) ? (
                  // unassigned: muted, dashed avatar (design state 5)
                  <span
                    className="grid h-5 w-5 flex-none place-items-center rounded-full font-mono text-[8.5px] font-semibold"
                    style={{ color: "var(--text-3)", border: "1.5px dashed var(--line-strong)" }}
                  >
                    {`S${speakerIndex(seg.speaker_key)}`}
                  </span>
                ) : (
                  <Avatar name={label} color={color} shape="circle" size="xs" />
                )}
                {isSelf ? (
                  <span className="text-xs font-semibold" style={{ color }}>
                    {label}
                  </span>
                ) : (
                  <SpeakerMenu
                    speakerKey={seg.speaker_key}
                    label={label}
                    color={color}
                    options={speakerOptions}
                    onAssign={(name) => onRelabel(seg.speaker_key, name)}
                    onAssignNew={(name) => onAssignNewAttendee(seg.speaker_key, name)}
                  />
                )}
                <span className="font-mono text-[11px]" style={{ color: "var(--text-3)" }}>
                  {fmtElapsed(seg.start_ms)}
                </span>
              </div>
              <div
                contentEditable
                suppressContentEditableWarning
                spellCheck={false}
                onFocus={(e) => (e.currentTarget.style.borderColor = "var(--primary)")}
                onBlur={(e) => {
                  e.currentTarget.style.borderColor = "var(--line)";
                  const next = (e.currentTarget.textContent ?? "").trim();
                  if (next && next !== seg.text) onEditSegment(seg.id, next);
                }}
                className="rounded-xl border px-3 py-2 text-[14px] leading-[1.55] outline-none"
                style={{
                  borderColor: "var(--line)",
                  color: "var(--text)",
                  background: isSelf ? "var(--primary-soft)" : "var(--surface-2)",
                }}
              >
                {seg.text}
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
