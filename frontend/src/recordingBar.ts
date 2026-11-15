// Pure logic behind the recording bar: sorting the backend's capture warnings
// onto the You / Others health chips, and the "Remind me to tell people"
// setting. Kept free of React and IPC so vitest covers it directly.
//
// The chips reflect `RecordingStatus.warnings` and, when the backend reports
// it, whether each side was heard in the last ~1.5 s (`mic_active` /
// `system_active`). Without that reading a chip falls back to "no problems
// detected" (Others) or "not monitored" (You).

/** Settings key for "Remind me to tell people" (webview-settable `ui.` prefix). */
export const REMIND_NOTICE_KEY = "ui.recording.remind_notice";

/** "Remind me" defaults on (spec §3.3.4): only an explicit "false" turns it off. */
export function remindNoticeOn(raw: string | null | undefined): boolean {
  return raw !== "false";
}

/** The macOS privacy pane named in the tap-silence warning (tap_messages.rs);
 *  a warning that names it gets the "Open Settings" action. */
export const MACOS_PRIVACY_PANE = "Screen & System Audio Recording";

export type CaptureSide = "you" | "others";

export interface CaptureWarning {
  /** Which side of the call it affects: your mic, or the system audio. */
  side: CaptureSide;
  /** Short chip label. */
  label: string;
  /** The backend's full sentence: cause and fix. */
  text: string;
  /** Offer the macOS "Open Settings" action next to it. */
  privacyPane: boolean;
}

/** Map one backend warning sentence onto a chip. Most are about the
 *  system-audio (others) channel; lost audio, a device change and a disk
 *  that won't take the audio name their side with "microphone" or not
 *  (`lost_audio_warning`, `device_lost_warning`, `write_failed_warning`,
 *  crates/fly-audio/src/cpal_backend.rs — each pinned there by a test). */
export function classifyWarning(text: string): CaptureWarning {
  const t = text.toLowerCase();
  const privacyPane = text.includes(MACOS_PRIVACY_PANE);
  let side: CaptureSide = "others";
  let label = "Check audio";
  if (privacyPane || t.includes("recording only silence")) label = "Blocked by macOS";
  else if (t.includes("changed or disconnected")) {
    label = "Device changed";
    if (t.includes("microphone")) side = "you";
  } else if (t.includes("couldn't be saved")) {
    label = "Not saving";
    if (t.includes("microphone")) side = "you";
  } else if (t.includes("was lost")) {
    label = "Audio lost";
    if (t.includes("microphone")) side = "you";
  } else if (t.includes("couldn't start")) label = "Not recorded";
  else if (t.includes("volume is at 0%")) label = "Volume at 0%";
  else if (t.includes("muted")) label = "Output muted";
  else if (t.includes("microphone") && !t.includes("only your microphone")) {
    side = "you";
    label = "Check your mic";
  }
  return { side, label, text, privacyPane };
}

export interface HealthChip {
  side: CaptureSide;
  /** "ok": that side was heard in the last ~1.5 s (or, with no reading, the
   *  other participants' audio with no problems detected) · "quiet": it was
   *  not · "unmonitored": no reading and nothing else checks it (your mic) ·
   *  "warn": a warning applies · "paused". */
  tone: "ok" | "quiet" | "unmonitored" | "warn" | "paused";
  label: string;
  warnings: CaptureWarning[];
  /** The tone comes from a level reading, not from the absence of warnings. */
  measured: boolean;
}

/** Was each side heard in the last ~1.5 s (`RecordingStatus.mic_active` /
 *  `system_active`)? `null` / absent: not measured. */
export interface SideActivity {
  you?: boolean | null;
  others?: boolean | null;
}

/** The two chips in the bar, You first, one rule for both: a warning wins;
 *  otherwise heard recently is "ok" (green), known silent is "quiet" (grey),
 *  and with no reading the chip is what it was before levels existed. A side
 *  with warnings stays flagged while paused; the rest go neutral because
 *  nothing is being captured. */
export function healthChips(
  warnings: readonly string[],
  paused: boolean,
  activity: SideActivity = {},
): HealthChip[] {
  const all = warnings.map(classifyWarning);
  return (["you", "others"] as const).map((side) => {
    const mine = all.filter((w) => w.side === side);
    const name = side === "you" ? "You" : "Others";
    if (mine.length === 0) {
      const heard = activity[side];
      const tone: HealthChip["tone"] = paused
        ? "paused"
        : heard === true
          ? "ok"
          : heard === false
            ? "quiet"
            : side === "you"
              ? "unmonitored"
              : "ok";
      return { side, tone, label: name, warnings: [], measured: typeof heard === "boolean" };
    }
    const more = mine.length > 1 ? ` +${mine.length - 1}` : "";
    return {
      side,
      tone: "warn",
      label: `${name} · ${mine[0].label}${more}`,
      warnings: mine,
      measured: typeof activity[side] === "boolean",
    };
  });
}

/** The warning popover's heading. Lost audio (gaps, the rest recorded) reads
 *  milder than a side that may be missing from the recording entirely. */
export function chipHeading(chip: Pick<HealthChip, "side" | "warnings">): string {
  const onlyGaps = chip.warnings.length > 0 && chip.warnings.every((w) => w.label === "Audio lost");
  if (chip.side === "you") {
    return onlyGaps
      ? "Part of your microphone audio was lost"
      : "Your microphone may be missing from this recording";
  }
  return onlyGaps
    ? "Part of the other participants’ audio was lost"
    : "The other participants may be missing from this recording";
}
