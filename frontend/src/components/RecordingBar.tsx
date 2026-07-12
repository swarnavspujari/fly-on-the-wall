import { Square } from "lucide-react";
import type { RecordingStatus } from "../types";
import { Button, RecordingIndicator } from "./ui";

interface Props {
  status: RecordingStatus;
  noteTitle: string | null;
  /** A screen capture is running alongside the audio recording. */
  screenActive?: boolean;
  /** Its source label ("Full screen" / "Window" / "Region"). */
  screenSource?: string | null;
  onPause: () => void;
  onResume: () => void;
  onStop: () => void;
  onOpenNote: () => void;
}

export function fmtElapsed(ms: number): string {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  return h > 0
    ? `${h}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}`
    : `${m}:${String(sec).padStart(2, "0")}`;
}

/** Always-visible recording bar (spec §9): while a capture is live this bar is
 *  pinned above everything on every view, on a violet-soft ground, with the
 *  unmissable "system output muted" warning strip on ink beneath it. */
export default function RecordingBar({
  status,
  noteTitle,
  screenActive = false,
  screenSource,
  onPause,
  onResume,
  onStop,
  onOpenNote,
}: Props) {
  if (!status.active) return null;
  const paused = status.state === "paused";

  return (
    <div className="print:hidden">
      <div className="flex flex-wrap items-center gap-3 border-b border-line bg-primary-soft px-4 py-2">
        <RecordingIndicator
          state={paused ? "paused" : "recording"}
          label={paused ? "Recording paused" : "Recording"}
          elapsedMs={status.elapsed_ms}
        />
        {noteTitle && (
          <button
            className="cursor-pointer truncate text-[13px] text-primary-soft-text underline-offset-2 hover:underline"
            onClick={onOpenNote}
          >
            {noteTitle}
          </button>
        )}
        {screenActive && (
          <span className="inline-flex items-center gap-1.5 rounded-full border border-line bg-surface px-2.5 py-[3px] text-[12px] font-semibold text-text-2">
            <span
              className="h-[7px] w-[7px] flex-none rounded-full bg-rec"
              style={{ animation: "fly-pulse-dot 1.2s ease infinite" }}
            />
            Screen · {screenSource ?? "recording"}
          </span>
        )}
        <div className="ml-auto flex items-center gap-2">
          {paused ? (
            <Button variant="outline" size="sm" onClick={onResume}>
              Resume
            </Button>
          ) : (
            <Button variant="outline" size="sm" onClick={onPause}>
              Pause
            </Button>
          )}
          <Button
            variant="contrast"
            size="sm"
            onClick={onStop}
            startIcon={<Square size={11} strokeWidth={1.75} />}
          >
            Stop
          </Button>
        </div>
      </div>
      {status.warnings.length > 0 && (
        <div className="bg-brand-ink px-4 py-1.5">
          {status.warnings.map((w) => (
            <p
              key={w}
              className="flex items-center gap-2 text-[12.5px] font-medium text-brand-cream"
            >
              <span className="h-2 w-2 flex-none rounded-full bg-warning" />
              {w}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}
