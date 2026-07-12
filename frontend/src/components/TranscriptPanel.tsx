import { useEffect, useRef, useState } from "react";
import type { Meeting, ModelProgress, Transcript } from "../types";
import { Pencil, RefreshCw } from "lucide-react";
import { fmtElapsed } from "./RecordingBar";
import { Avatar, Button, ProgressBar, SectionLabel, speakerColor } from "./ui";

interface Props {
  meeting: Meeting;
  transcript: Transcript | null;
  /** Current pipeline stage, or null when idle. */
  stage: string | null;
  /** One-line stage detail (channel being transcribed, GPU benchmark result, CPU fallback notice). */
  stageDetail: string | null;
  modelProgress: ModelProgress | null;
  pipelineError: string | null;
  /** Zoom-in: segment ids to highlight + scroll to (AI block sources). */
  highlightIds: string[];
  onTranscribe: () => void;
  onRelabel: (speakerKey: string, label: string) => void;
  /** Persist an edited transcript line (called on blur when the text changed). */
  onEditSegment: (segmentId: string, text: string) => void;
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
};

/** Editable speaker name: quiet dashed underline on hover, click renames it
 *  everywhere (commit calls the existing onRelabel handler). */
function SpeakerName({
  label,
  color,
  onRename,
}: {
  label: string;
  color: string;
  onRename: (name: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [val, setVal] = useState(label);
  const [hov, setHov] = useState(false);
  useEffect(() => setVal(label), [label]);

  if (editing) {
    return (
      <input
        autoFocus
        value={val}
        onChange={(e) => setVal(e.target.value)}
        onBlur={() => {
          setEditing(false);
          if (val.trim()) onRename(val.trim());
          else setVal(label);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            e.currentTarget.blur();
          }
          if (e.key === "Escape") {
            setVal(label);
            setEditing(false);
          }
        }}
        className="rounded-md border bg-surface px-1.5 py-px text-xs font-semibold outline-none"
        style={{ color, borderColor: "var(--primary)", width: Math.max(56, val.length * 7.5) }}
      />
    );
  }
  return (
    <span
      role="button"
      title="Click to rename"
      onClick={() => setEditing(true)}
      onMouseEnter={() => setHov(true)}
      onMouseLeave={() => setHov(false)}
      className="inline-flex cursor-text items-center gap-1 text-xs font-semibold"
      style={{ color, borderBottom: `1px dashed ${hov ? "currentColor" : "transparent"}` }}
    >
      {label}
      {hov && <Pencil size={11} strokeWidth={1.75} style={{ opacity: 0.65 }} />}
    </span>
  );
}

export default function TranscriptPanel({
  meeting,
  transcript,
  stage,
  stageDetail,
  modelProgress,
  pipelineError,
  highlightIds,
  onTranscribe,
  onRelabel,
  onEditSegment,
}: Props) {
  const segRefs = useRef<Map<string, HTMLDivElement>>(new Map());

  // Zoom-in: scroll the first highlighted source segment into view.
  useEffect(() => {
    if (highlightIds.length === 0) return;
    const el = segRefs.current.get(highlightIds[0]);
    el?.scrollIntoView({ behavior: "smooth", block: "center" });
  }, [highlightIds]);

  // Stable per-speaker index (position in the transcript's speaker list).
  const speakerIndex = (key: string) => {
    const idx = transcript?.speakers.findIndex((s) => s.key === key) ?? 0;
    return Math.max(idx, 0);
  };

  if (stage) {
    const pct =
      modelProgress && modelProgress.stage === "downloading" && modelProgress.total > 0
        ? Math.round((modelProgress.downloaded / modelProgress.total) * 100)
        : null;
    return (
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
    );
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
        {pipelineError && (
          <div
            className="mt-2 rounded-lg border border-line px-3 py-2 text-[13px]"
            style={{ background: "var(--error-soft)", color: "var(--error-text)" }}
          >
            {pipelineError}
          </div>
        )}
      </div>
    );
  }

  return (
    <div>
      <div className="mb-4 flex items-center justify-between gap-2">
        <div className="flex items-baseline gap-2">
          <SectionLabel>Transcript</SectionLabel>
          <span className="text-[11px]" style={{ color: "var(--text-3)" }}>
            · click any line or name to edit
          </span>
        </div>
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
      {transcript.segments.map((seg) => {
        const isSelf = seg.speaker_key === "mic";
        const idx = speakerIndex(seg.speaker_key);
        const color = speakerColor(seg.speaker_key, idx);
        const label =
          transcript.speakers.find((s) => s.key === seg.speaker_key)?.label ?? seg.speaker_key;
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
                <Avatar name={label} color={color} shape="circle" size="xs" />
                <SpeakerName
                  label={label}
                  color={color}
                  onRename={(name) => onRelabel(seg.speaker_key, name)}
                />
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
