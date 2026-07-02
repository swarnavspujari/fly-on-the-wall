import type { RecordingStatus } from "../types";
import { Btn } from "./ui";

interface Props {
  status: RecordingStatus;
  noteTitle: string | null;
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

const WAVE_DELAYS = ["0s", ".15s", ".3s", ".45s", ".6s"];

/** Always-visible consent/recording indicator (spec §9): while a capture is
 *  live this bar is pinned above everything, on every view. */
export default function RecordingBar({
  status,
  noteTitle,
  onPause,
  onResume,
  onStop,
  onOpenNote,
}: Props) {
  if (!status.active) return null;
  const paused = status.state === "paused";

  return (
    <div className="print:hidden flex flex-wrap items-center gap-3 border-b border-line bg-peach px-4 py-2 text-sm">
      <span
        className={`h-2.5 w-2.5 flex-none rounded-full ${paused ? "bg-spk-amber" : "bg-rec"}`}
        style={paused ? undefined : { animation: "pulse-dot 1.2s ease infinite" }}
      />
      <span className="text-[13px] font-semibold text-ink">
        {paused ? "Recording paused" : "Recording"}
      </span>
      <span className="font-mono text-[13px] tabular-nums text-ink">
        {fmtElapsed(status.elapsed_ms)}
      </span>
      {!paused && (
        <span className="flex h-4 flex-none items-end gap-[3px]">
          {WAVE_DELAYS.map((delay) => (
            <span
              key={delay}
              className="h-full w-[3px] rounded-full bg-coral"
              style={{
                animation: `wave .9s ease-in-out ${delay} infinite`,
                transform: "scaleY(.32)",
                transformOrigin: "bottom",
              }}
            />
          ))}
        </span>
      )}
      {noteTitle && (
        <button
          className="cursor-pointer truncate text-clay underline-offset-2 hover:underline"
          onClick={onOpenNote}
        >
          {noteTitle}
        </button>
      )}
      <div className="ml-auto flex items-center gap-2">
        {paused ? (
          <Btn variant="outline" size="sm" onClick={onResume}>
            Resume
          </Btn>
        ) : (
          <Btn variant="outline" size="sm" onClick={onPause}>
            Pause
          </Btn>
        )}
        <Btn variant="dark" size="sm" onClick={onStop}>
          Stop
        </Btn>
      </div>
      {status.warnings.length > 0 && (
        <div className="-mx-4 -mb-2 w-[calc(100%+2rem)] bg-ink px-4 py-1.5">
          {status.warnings.map((w) => (
            <p key={w} className="text-[12.5px] font-medium text-white">
              <span className="mr-1.5 inline-block h-2 w-2 rounded-full bg-spk-amber align-baseline" />
              {w}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}
