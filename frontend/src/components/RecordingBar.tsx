import { useCallback, useEffect, useRef, useState } from "react";
import { Check, Copy, FileText, Megaphone, Square, X } from "lucide-react";
import { api } from "../api";
import { recordingNoticeText } from "../privacy";
import { healthChips, remindNoticeOn, REMIND_NOTICE_KEY, type HealthChip } from "../recordingBar";
import type { RecordingStatus } from "../types";
import {
  Button,
  Checkbox,
  EscapeLayerScope,
  RecordingIndicator,
  useDialogFocus,
  useEscapeLayer,
} from "./ui";

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

/** The recording notice in the bar: one-click copy, the per-meeting
 *  "participants notified" state, and the start-of-recording reminder
 *  ("Remind me to tell people", on unless turned off in Settings). None of
 *  it ever gates the capture itself. */
function useRecordingNotice(meetingId: string | null, noteTitle: string | null) {
  const [reminder, setReminder] = useState(false);
  const [notified, setNotified] = useState(false);
  const [copied, setCopied] = useState<"idle" | "copied" | "failed">("idle");
  const resetCopied = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(resetCopied.current), []);

  useEffect(() => {
    setReminder(false);
    setNotified(false);
    setCopied("idle");
    if (!meetingId) return;
    let live = true;
    // An unreadable setting counts as on, an unreadable privacy row as "not
    // yet notified": when in doubt, remind.
    void Promise.all([
      api.getAppSetting(REMIND_NOTICE_KEY).catch(() => null),
      api
        .getMeetingPrivacy(meetingId)
        .then((p) => p.participants_notified)
        .catch(() => false),
    ]).then(([remind, wasNotified]) => {
      if (!live) return;
      setNotified(wasNotified);
      setReminder(remindNoticeOn(remind) && !wasNotified);
    });
    return () => {
      live = false;
    };
  }, [meetingId]);

  const copy = () => {
    api
      .copyRecordingNotice(recordingNoticeText(noteTitle))
      .then(() => setCopied("copied"))
      .catch(() => setCopied("failed"))
      .finally(() => {
        window.clearTimeout(resetCopied.current);
        resetCopied.current = window.setTimeout(() => setCopied("idle"), 1800);
      });
  };
  const markNotified = (value: boolean) => {
    if (!meetingId) return;
    setNotified(value);
    api.setParticipantsNotified(meetingId, value).catch(() => setNotified(!value));
    if (value) setReminder(false);
  };
  return {
    reminder,
    closeReminder: () => setReminder(false),
    notified,
    copied,
    copy,
    markNotified,
  };
}

/** Whether stopping will start the transcript ("Transcribe when I stop"),
 *  so the stop button says what happens. Read when each recording starts and
 *  again as you reach for the button (hover or focus), so a change made in
 *  Settings mid-recording shows before you click. */
function useTranscribesOnStop(meetingId: string | null) {
  const [auto, setAuto] = useState(true);
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  const refresh = useCallback(() => {
    api
      .getAsrSettings()
      .then((s) => {
        if (live.current) setAuto(s.auto_transcribe);
      })
      .catch(() => {});
  }, []);
  useEffect(() => {
    if (meetingId) refresh();
  }, [meetingId, refresh]);
  return { auto, refresh };
}

/** One side of the call (You = your mic, Others = system audio). With no
 *  warning it's a quiet status chip; with one it turns ink and opens the
 *  full cause and fix. There are no live meters yet (spec §3.3.1): nothing
 *  checks the mic, so "You" never shows a green "fine". */
function HealthChipView({
  chip,
  onOpenPrivacySettings,
}: {
  chip: HealthChip;
  onOpenPrivacySettings?: () => void;
}) {
  const warn = chip.tone === "warn";
  const [open, setOpen] = useState(false);
  const popRef = useRef<HTMLDivElement>(null);
  // the same element in both states, so focus has somewhere to go back to
  const wrapRef = useRef<HTMLSpanElement>(null);
  const depth = useEscapeLayer(() => setOpen(false), open && warn);
  useDialogFocus(popRef, open && warn);
  // A warning that clears (you unmuted) takes its popover with it; if focus
  // was in there, it comes back to the chip instead of the page.
  useEffect(() => {
    if (warn || !open) return;
    setOpen(false);
    const at = document.activeElement;
    if (!at || at === document.body) wrapRef.current?.focus();
  }, [warn, open]);

  const name = chip.side === "you" ? "You" : "Others";
  if (!warn) {
    return (
      <span ref={wrapRef} tabIndex={-1} className="inline-flex flex-none outline-none">
        <span
          className="inline-flex items-center gap-1.5 rounded-full border border-line bg-surface px-2.5 py-[3px] text-[12px] font-semibold text-text-2"
          title={
            chip.tone === "paused"
              ? "Paused"
              : chip.tone === "unmonitored"
                ? "Your microphone: not monitored yet"
                : "The other participants' audio: no problems detected"
          }
        >
          <span
            className="h-[7px] w-[7px] flex-none rounded-full"
            style={{ background: chip.tone === "ok" ? "var(--success)" : "var(--text-3)" }}
          />
          {chip.label}
        </span>
      </span>
    );
  }

  const detail = chip.label.slice(name.length);
  return (
    <span ref={wrapRef} tabIndex={-1} className="relative inline-flex flex-none outline-none">
      <button
        type="button"
        aria-label={chip.label}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="inline-flex cursor-pointer items-center gap-1.5 rounded-full border border-warning bg-brand-ink px-2.5 py-[3px] text-[12px] font-semibold text-brand-cream hover:opacity-90"
      >
        <span className="h-[7px] w-[7px] flex-none rounded-full bg-warning" />
        {name}
        <span className="hidden lg:inline">{detail}</span>
      </button>
      {open && (
        <>
          <span className="fixed inset-0 z-40" onClick={() => setOpen(false)} aria-hidden="true" />
          <div
            ref={popRef}
            role="dialog"
            aria-label={
              chip.side === "you"
                ? "Your microphone may be missing from this recording"
                : "The other participants may be missing from this recording"
            }
            className="absolute left-0 z-50 w-[360px] max-w-[calc(100vw-32px)] rounded-2xl border border-line bg-surface p-3.5 text-text"
            style={{ top: "calc(100% + 8px)", boxShadow: "var(--shadow-pop)" }}
          >
            <EscapeLayerScope depth={depth}>
              <div className="flex items-start gap-2">
                <span className="mt-[5px] h-2 w-2 flex-none rounded-full bg-warning" />
                <p className="m-0 min-w-0 flex-1 text-[13px] font-semibold">
                  {chip.side === "you"
                    ? "Your microphone may be missing from this recording"
                    : "The other participants may be missing from this recording"}
                </p>
                <button
                  type="button"
                  data-dialog-close
                  aria-label="Close"
                  onClick={() => setOpen(false)}
                  className="flex-none cursor-pointer rounded p-0.5 text-text-3 hover:text-text"
                >
                  <X size={14} strokeWidth={2} />
                </button>
              </div>
              {chip.warnings.map((w) => (
                <div key={w.text} className="mt-2 pl-4">
                  <p className="m-0 text-[12.5px] leading-[1.5] text-text-2">{w.text}</p>
                  {w.privacyPane && onOpenPrivacySettings && (
                    <Button
                      variant="outline"
                      size="xs"
                      className="mt-2"
                      onClick={onOpenPrivacySettings}
                    >
                      Open Settings
                    </Button>
                  )}
                </div>
              ))}
            </EscapeLayerScope>
          </div>
        </>
      )}
    </span>
  );
}

/** Start-of-recording reminder under the notice controls. Never takes focus
 *  (you may already be typing notes); ticking "Participants notified", the X,
 *  or Esc while focus is inside it closes it. It doesn't join the global
 *  Esc stack: an Esc meant for the editor or a rename must not close it. */
function NoticeReminder({
  copied,
  onCopy,
  onClose,
}: {
  copied: "idle" | "copied" | "failed";
  onCopy: () => void;
  onClose: () => void;
}) {
  return (
    <div
      role="region"
      aria-label="Reminder: let participants know"
      onKeyDown={(e) => {
        if (e.key !== "Escape" || e.defaultPrevented) return;
        e.preventDefault();
        onClose();
      }}
      className="absolute right-0 z-40 w-[330px] max-w-[calc(100vw-32px)] rounded-2xl border border-line bg-surface p-3.5 text-text"
      style={{ top: "calc(100% + 8px)", boxShadow: "var(--shadow-pop)" }}
    >
      <div className="flex items-start gap-2.5">
        <Megaphone
          size={15}
          strokeWidth={1.75}
          className="mt-px flex-none"
          style={{ color: "var(--primary-text)" }}
        />
        <div className="min-w-0 flex-1">
          <p className="m-0 text-[13px] font-semibold">
            Let participants know they&apos;re being recorded
          </p>
          <p className="m-0 mt-0.5 text-[12.5px] leading-[1.5] text-text-2">
            Some places require everyone&apos;s consent. Paste the notice in the meeting chat, then
            tick Participants notified.
          </p>
          <Button
            variant="soft"
            size="xs"
            className="mt-2.5"
            startIcon={
              copied === "copied" ? (
                <Check size={13} strokeWidth={2} />
              ) : (
                <Copy size={13} strokeWidth={1.75} />
              )
            }
            onClick={onCopy}
          >
            {copied === "copied"
              ? "Copied — paste it in the meeting chat"
              : copied === "failed"
                ? "Couldn't copy"
                : "Copy recording notice"}
          </Button>
        </div>
        <button
          type="button"
          title="Hide"
          aria-label="Hide the reminder"
          onClick={onClose}
          className="flex-none cursor-pointer rounded p-0.5 text-text-3 hover:text-text"
        >
          <X size={14} strokeWidth={2} />
        </button>
      </div>
    </div>
  );
}

/** Always-visible recording bar (spec §3.3): while a capture is live this one
 *  fixed-height row is pinned above every view, on a violet-soft ground.
 *  Capture warnings live in the Others / You chips (click for the full cause
 *  and fix), so the editor below never moves. */
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
  const meetingId = status.active ? status.meeting_id : null;
  const notice = useRecordingNotice(meetingId, noteTitle);
  const stopping = useTranscribesOnStop(meetingId);
  const noticeRef = useRef<HTMLDivElement>(null);
  if (!status.active) return null;
  // Esc or Hide removes the reminder and the button you were on; focus moves
  // to the notice button it hangs from instead of dropping to the page.
  const closeReminder = () => {
    noticeRef.current?.querySelector<HTMLElement>("[data-notice-copy]")?.focus();
    notice.closeReminder();
  };
  const paused = status.state === "paused";
  const chips = healthChips(status.warnings, paused);
  const warned = chips.filter((c) => c.tone === "warn");
  const copyLabel =
    notice.copied === "copied"
      ? "Copied"
      : notice.copied === "failed"
        ? "Couldn't copy"
        : "Copy recording notice";

  return (
    <div className="print:hidden">
      <div
        data-recording-bar-row
        className="flex h-[46px] flex-nowrap items-center gap-3 border-b border-line bg-primary-soft px-4"
      >
        {/* Overflow valve: ≥ lg the note title truncates first; below lg (title
            is an icon) the indicator's label truncates. The timer never does. */}
        <RecordingIndicator
          className="min-w-0 flex-none max-lg:shrink [&_.fly-rec-label]:min-w-0 [&_.fly-rec-label]:truncate"
          state={paused ? "paused" : "recording"}
          label={paused ? "Recording paused" : "Recording"}
          elapsedMs={status.elapsed_ms}
        />
        {noteTitle && (
          <button
            className="flex-none cursor-pointer rounded p-0.5 text-primary-soft-text hover:bg-primary-soft lg:hidden"
            onClick={onOpenNote}
            title={noteTitle}
            aria-label="Open the recording's note"
          >
            <FileText size={14} strokeWidth={1.75} />
          </button>
        )}
        {noteTitle && (
          <button
            className="hidden min-w-0 cursor-pointer truncate text-[13px] text-primary-soft-text underline-offset-2 hover:underline lg:block"
            onClick={onOpenNote}
            title={noteTitle}
          >
            {noteTitle}
          </button>
        )}
        <div className="flex flex-none items-center gap-1.5">
          {chips.map((c) => (
            <HealthChipView key={c.side} chip={c} onOpenPrivacySettings={onOpenPrivacySettings} />
          ))}
        </div>
        {/* announce a new capture problem; the chip itself is the visible cue */}
        <span className="sr-only" role="status" aria-live="polite">
          {warned.map((c) => c.label).join(". ")}
        </span>
        {screenActive && (
          <span className="inline-flex flex-none items-center gap-1.5 rounded-full border border-line bg-surface px-2.5 py-[3px] text-[12px] font-semibold text-text-2">
            <span
              className="h-[7px] w-[7px] flex-none rounded-full bg-rec"
              style={{ animation: "fly-pulse-dot 1.2s ease infinite" }}
            />
            Screen<span className="hidden lg:inline"> · {screenSource ?? "recording"}</span>
          </span>
        )}
        <div className="ml-auto flex flex-none items-center gap-2">
          {status.meeting_id && (
            <div ref={noticeRef} className="relative flex items-center gap-2">
              <Button
                data-notice-copy
                variant="ghost"
                size="sm"
                aria-label={copyLabel}
                title={recordingNoticeText(noteTitle)}
                startIcon={
                  notice.copied === "copied" ? (
                    <Check size={13} strokeWidth={2} />
                  ) : (
                    <Megaphone size={13} strokeWidth={1.75} />
                  )
                }
                onClick={notice.copy}
              >
                <span className="hidden xl:inline">{copyLabel}</span>
              </Button>
              <Checkbox
                aria-label="Participants notified"
                title="You told everyone about the recording"
                checked={notice.notified}
                onChange={(e) => notice.markNotified(e.target.checked)}
                label={
                  <span className="text-[12.5px] font-medium text-primary-soft-text">
                    <span className="hidden xl:inline">Participants notified</span>
                    <span className="hidden sm:inline xl:hidden">Notified</span>
                  </span>
                }
              />
              {notice.reminder && (
                <NoticeReminder
                  copied={notice.copied}
                  onCopy={notice.copy}
                  onClose={closeReminder}
                />
              )}
            </div>
          )}
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
            onMouseEnter={stopping.refresh}
            onFocus={stopping.refresh}
            title={stopping.auto ? "Stop recording and start the transcript" : "Stop recording"}
            startIcon={<Square size={11} strokeWidth={1.75} />}
          >
            {stopping.auto ? "Stop and transcribe" : "Stop"}
          </Button>
        </div>
      </div>
    </div>
  );
}
