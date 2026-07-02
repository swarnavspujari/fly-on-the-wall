import { useEffect, useRef, useState } from "react";
import type { Meeting, ModelProgress, Transcript } from "../types";
import { fmtElapsed } from "./RecordingBar";
import { Btn, SectionLabel, speakerColor, speakerInitials } from "./ui";

interface Props {
  meeting: Meeting;
  transcript: Transcript | null;
  /** Current pipeline stage, or null when idle. */
  stage: string | null;
  modelProgress: ModelProgress | null;
  pipelineError: string | null;
  /** Zoom-in: segment ids to highlight + scroll to (AI block sources). */
  highlightIds: string[];
  onTranscribe: () => void;
  onRelabel: (speakerKey: string, label: string) => void;
}

const STAGE_LABELS: Record<string, string> = {
  starting: "Starting…",
  "ensuring-models": "Preparing models…",
  "preparing-audio": "Preparing audio…",
  transcribing: "Transcribing",
  diarizing: "Detecting speakers…",
  aligning: "Assigning words to speakers…",
  saving: "Saving transcript…",
};

export default function TranscriptPanel({
  meeting,
  transcript,
  stage,
  modelProgress,
  pipelineError,
  highlightIds,
  onTranscribe,
  onRelabel,
}: Props) {
  const [renaming, setRenaming] = useState<{ key: string; label: string } | null>(null);
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
      <div className="print:hidden border-b border-line bg-cream px-6 py-3">
        <div className="flex items-center gap-2.5 text-[13px] font-medium text-clay">
          <span
            className="h-2 w-2 shrink-0 rounded-full bg-coral"
            style={{ animation: "pulse-dot 1s ease infinite" }}
          />
          <span>{STAGE_LABELS[stage] ?? stage}</span>
          {pct !== null && (
            <span className="font-normal text-mute">
              downloading {modelProgress!.id} — {pct}%
            </span>
          )}
        </div>
        {pct !== null && (
          <div className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-line">
            <div
              className="h-full rounded-full bg-coral transition-all"
              style={{ width: `${pct}%` }}
            />
          </div>
        )}
      </div>
    );
  }

  if (!transcript) {
    if (!meeting.recording) return null;
    return (
      <div className="border-b border-line bg-cream px-6 py-2.5">
        <div className="flex items-center gap-3">
          <Btn variant="primary" size="sm" onClick={onTranscribe}>
            Transcribe recording
          </Btn>
          <span className="text-xs text-mute">
            Runs fully on this machine — audio never leaves your computer.
          </span>
        </div>
        {pipelineError && (
          <div className="mt-2 rounded-[12px] border border-line bg-peach-2 p-3 text-[13px] text-clay">
            ⚠ {pipelineError}
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="max-h-72 overflow-y-auto border-b border-line bg-cream px-6 py-3">
      <div className="mb-3 flex items-baseline gap-2">
        <SectionLabel>Transcript</SectionLabel>
        <span className="text-[11px] text-mute">
          · {transcript.engine}
          {transcript.language ? ` · ${transcript.language}` : ""} · click a name to rename
        </span>
      </div>
      {transcript.segments.map((seg) => {
        const isSelf = seg.speaker_key === "mic";
        const color = speakerColor(seg.speaker_key, speakerIndex(seg.speaker_key));
        const label =
          transcript.speakers.find((s) => s.key === seg.speaker_key)?.label ?? seg.speaker_key;
        return (
          <div
            key={seg.id}
            ref={(el) => {
              if (el) segRefs.current.set(seg.id, el);
              else segRefs.current.delete(seg.id);
            }}
            className={`mb-3 flex rounded-xl ${isSelf ? "justify-end" : ""} ${
              highlightIds.includes(seg.id) ? "bg-peach outline outline-1 outline-coral/60" : ""
            }`}
          >
            <div className="min-w-0 max-w-[86%]">
              <div className={`mb-1 flex items-center gap-1.5 ${isSelf ? "justify-end" : ""}`}>
                <span
                  className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full text-[9px] font-semibold text-white"
                  style={{ background: color }}
                >
                  {speakerInitials(label)}
                </span>
                {renaming?.key === seg.speaker_key ? (
                  <input
                    autoFocus
                    className="w-32 rounded border border-line bg-surface px-1 text-xs text-ink outline-none focus:border-coral"
                    value={renaming.label}
                    onChange={(e) => setRenaming({ key: seg.speaker_key, label: e.target.value })}
                    onBlur={() => {
                      if (renaming.label.trim()) onRelabel(seg.speaker_key, renaming.label.trim());
                      setRenaming(null);
                    }}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") e.currentTarget.blur();
                      if (e.key === "Escape") setRenaming(null);
                    }}
                  />
                ) : (
                  <button
                    className="cursor-pointer text-xs font-semibold hover:underline"
                    style={{ color }}
                    title="Rename speaker"
                    onClick={() => setRenaming({ key: seg.speaker_key, label })}
                  >
                    {label}
                  </button>
                )}
                <span className="font-mono text-[11px] text-mute">{fmtElapsed(seg.start_ms)}</span>
              </div>
              <div
                className={`rounded-xl border border-line px-3 py-2 text-[14px] leading-[1.55] text-ink ${
                  isSelf ? "bg-peach" : "bg-surface"
                }`}
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
