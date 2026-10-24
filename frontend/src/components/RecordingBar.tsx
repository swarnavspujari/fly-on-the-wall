import { useEffect, useState } from "react";
import { Check, Copy, Megaphone, Square, X } from "lucide-react";
import { api } from "../api";
import { recordingNoticeText } from "../privacy";
import type { RecordingStatus } from "../types";
import { Button, Checkbox, RecordingIndicator } from "./ui";

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
  /** macOS: open the Screen & System Audio Recording privacy pane. Rendered
   *  next to warnings that direct the user there. */
  onOpenPrivacySettings?: () => void;
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

/** State behind the non-blocking "Let participants know" helper: open at the
 *  start of each recording, collapsible, never gating the capture itself. */
function useConsentHelper(meetingId: string | null, noteTitle: string | null) {
  const [open, setOpen] = useState(true);
  const [notified, setNotified] = useState(false);
  const [copied, setCopied] = useState<"idle" | "copied" | "failed">("idle");

  useEffect(() => {
    setOpen(true);
    setNotified(false);
    setCopied("idle");
    if (!meetingId) return;
    let live = true;
    api
      .getMeetingPrivacy(meetingId)
      .then((p) => {
        if (!live) return;
        setNotified(p.participants_notified);
        if (p.participants_notified) setOpen(false);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [meetingId]);

  const copy = () => {
    api
      .copyRecordingNotice(recordingNoticeText(noteTitle))
      .then(() => setCopied("copied"))
      .catch(() => setCopied("failed"))
      .finally(() => window.setTimeout(() => setCopied("idle"), 1800));
  };
  const markNotified = (value: boolean) => {
    if (!meetingId) return;
    setNotified(value);
    api.setParticipantsNotified(meetingId, value).catch(() => setNotified(!value));
    if (value) setOpen(false);
  };
  return { open, setOpen, notified, copied, copy, markNotified };
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
  onOpenPrivacySettings,
}: Props) {
  const consent = useConsentHelper(status.active ? status.meeting_id : null, noteTitle);
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
          {status.meeting_id &&
            !consent.open &&
            (consent.notified ? (
              <button
                className="inline-flex cursor-pointer items-center gap-1 text-[12px] font-medium text-primary-soft-text"
                title="You noted that participants were told about the recording"
                onClick={() => consent.setOpen(true)}
              >
                <Check size={13} strokeWidth={2} /> Participants notified
              </button>
            ) : (
              <Button
                variant="ghost"
                size="sm"
                startIcon={<Megaphone size={13} strokeWidth={1.75} />}
                onClick={() => consent.setOpen(true)}
              >
                Let participants know
              </Button>
            ))}
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
      {status.meeting_id && consent.open && (
        <div
          role="region"
          aria-label="Let participants know"
          className="flex flex-wrap items-center gap-x-3 gap-y-1.5 border-b border-line bg-surface px-4 py-1.5"
        >
          <Megaphone
            size={14}
            strokeWidth={1.75}
            className="flex-none"
            style={{ color: "var(--primary-text)" }}
          />
          <span className="min-w-0 flex-1 text-[12.5px] text-text-2">
            Let participants know they're being recorded. Some places require everyone's consent.
          </span>
          <Button
            variant="soft"
            size="xs"
            startIcon={
              consent.copied === "copied" ? (
                <Check size={13} strokeWidth={2} />
              ) : (
                <Copy size={13} strokeWidth={1.75} />
              )
            }
            title={recordingNoticeText(noteTitle)}
            onClick={consent.copy}
          >
            {consent.copied === "copied"
              ? "Copied — paste it in the meeting chat"
              : consent.copied === "failed"
                ? "Couldn't copy"
                : "Copy recording notice"}
          </Button>
          <Checkbox
            checked={consent.notified}
            onChange={(e) => consent.markNotified(e.target.checked)}
            label={<span className="text-[12.5px]">Participants notified</span>}
          />
          <button
            title="Hide"
            aria-label="Hide the participants notice"
            className="flex-none cursor-pointer rounded p-0.5 text-text-3 hover:text-text"
            onClick={() => consent.setOpen(false)}
          >
            <X size={14} strokeWidth={2} />
          </button>
        </div>
      )}
      {status.warnings.length > 0 && (
        <div className="bg-brand-ink px-4 py-1.5">
          {status.warnings.map((w) => (
            <p
              key={w}
              className="flex items-center gap-2 text-[12.5px] font-medium text-brand-cream"
            >
              <span className="h-2 w-2 flex-none rounded-full bg-warning" />
              <span className="min-w-0 flex-1">{w}</span>
              {onOpenPrivacySettings && w.includes("Screen & System Audio Recording") && (
                <button
                  className="flex-none cursor-pointer rounded-md border border-brand-cream/40 px-2 py-[2px] text-[11.5px] font-semibold text-brand-cream hover:bg-brand-cream/10"
                  onClick={onOpenPrivacySettings}
                >
                  Open Settings
                </button>
              )}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}
