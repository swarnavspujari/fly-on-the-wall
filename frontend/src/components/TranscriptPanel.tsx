import { useEffect, useRef, useState } from "react";
import type { Meeting, ModelProgress, Transcript } from "../types";
import { fmtElapsed } from "./RecordingBar";

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

const SPEAKER_COLORS = [
  "text-sky-300",
  "text-emerald-300",
  "text-amber-300",
  "text-fuchsia-300",
  "text-rose-300",
  "text-lime-300",
];

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

  const speakerColor = (key: string) => {
    if (key === "mic") return "text-indigo-300";
    const idx = transcript?.speakers.findIndex((s) => s.key === key) ?? 0;
    return SPEAKER_COLORS[Math.max(idx, 0) % SPEAKER_COLORS.length];
  };

  if (stage) {
    const pct =
      modelProgress && modelProgress.stage === "downloading" && modelProgress.total > 0
        ? Math.round((modelProgress.downloaded / modelProgress.total) * 100)
        : null;
    return (
      <div className="border-b border-zinc-800 bg-zinc-900/80 px-6 py-3 text-sm text-zinc-300">
        <div className="flex items-center gap-2">
          <span className="inline-block h-3 w-3 animate-spin rounded-full border-2 border-indigo-400 border-t-transparent" />
          <span>{STAGE_LABELS[stage] ?? stage}</span>
          {pct !== null && (
            <span className="text-zinc-500">
              downloading {modelProgress!.id} — {pct}%
            </span>
          )}
        </div>
        {pct !== null && (
          <div className="mt-2 h-1.5 w-full overflow-hidden rounded bg-zinc-800">
            <div className="h-full bg-indigo-500 transition-all" style={{ width: `${pct}%` }} />
          </div>
        )}
      </div>
    );
  }

  if (!transcript) {
    if (!meeting.recording) return null;
    return (
      <div className="flex items-center gap-3 border-b border-zinc-800 bg-zinc-900/80 px-6 py-2 text-sm">
        <button
          onClick={onTranscribe}
          className="rounded-md bg-indigo-600 px-3 py-1 text-xs font-medium text-white hover:bg-indigo-500"
        >
          ✨ Transcribe recording
        </button>
        <span className="text-xs text-zinc-500">
          Runs fully on this machine — audio never leaves your computer.
        </span>
        {pipelineError && <span className="text-xs text-red-400">⚠ {pipelineError}</span>}
      </div>
    );
  }

  return (
    <div className="max-h-72 overflow-y-auto border-b border-zinc-800 bg-zinc-900/60 px-6 py-3">
      <div className="mb-2 flex items-center gap-2 text-xs uppercase tracking-wide text-zinc-500">
        Transcript
        <span className="normal-case text-zinc-600">
          · {transcript.engine}
          {transcript.language ? ` · ${transcript.language}` : ""} · click a name to rename
        </span>
      </div>
      {transcript.segments.map((seg) => (
        <div
          key={seg.id}
          ref={(el) => {
            if (el) segRefs.current.set(seg.id, el);
            else segRefs.current.delete(seg.id);
          }}
          className={`mb-2 flex gap-3 rounded text-sm ${
            highlightIds.includes(seg.id)
              ? "bg-indigo-900/40 outline outline-1 outline-indigo-600/60"
              : ""
          }`}
        >
          <span className="w-12 shrink-0 pt-0.5 text-right text-xs tabular-nums text-zinc-600">
            {fmtElapsed(seg.start_ms)}
          </span>
          <div className="min-w-0">
            {renaming?.key === seg.speaker_key ? (
              <input
                autoFocus
                className="mr-2 w-32 rounded bg-zinc-800 px-1 text-xs text-zinc-100 outline-none"
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
                className={`mr-2 text-xs font-semibold ${speakerColor(seg.speaker_key)} hover:underline`}
                title="Rename speaker"
                onClick={() =>
                  setRenaming({
                    key: seg.speaker_key,
                    label:
                      transcript.speakers.find((s) => s.key === seg.speaker_key)?.label ??
                      seg.speaker_key,
                  })
                }
              >
                {transcript.speakers.find((s) => s.key === seg.speaker_key)?.label ??
                  seg.speaker_key}
              </button>
            )}
            <span className="text-zinc-300">{seg.text}</span>
          </div>
        </div>
      ))}
    </div>
  );
}
