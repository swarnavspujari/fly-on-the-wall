// Pure logic behind the recording bar: sorting the backend's capture warnings
// onto the You / Others health chips, and the "Remind me to tell people"
// setting. Kept free of React and IPC so vitest covers it directly.
//
// There are no live level meters yet (spec §3.3.1): the chips only reflect
// `RecordingStatus.warnings`, so "ok" means "no problems detected".

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
 *  system-audio (others) channel; lost audio names its side — "Your
 *  microphone …" or "The other participants' audio …"
 *  (`lost_audio_warning`, crates/fly-audio/src/cpal_backend.rs). */
export function classifyWarning(text: string): CaptureWarning {
  const t = text.toLowerCase();
  const privacyPane = text.includes(MACOS_PRIVACY_PANE);
  let side: CaptureSide = "others";
  let label = "Check audio";
  if (privacyPane || t.includes("recording only silence")) label = "Blocked by macOS";
  else if (t.includes("was lost")) {
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
  /** "ok": checked, no problems detected (the other participants' audio:
   *  mute, volume, loopback start, macOS silence) · "unmonitored": nothing
   *  checks it yet (your mic, until level meters land) · "warn": a warning
   *  applies · "paused". */
  tone: "ok" | "unmonitored" | "warn" | "paused";
  label: string;
  warnings: CaptureWarning[];
}

/** The two chips in the bar, You first. A side with warnings stays flagged
 *  while paused; the rest go neutral because nothing is being captured. */
export function healthChips(warnings: readonly string[], paused: boolean): HealthChip[] {
  const all = warnings.map(classifyWarning);
  return (["you", "others"] as const).map((side) => {
    const mine = all.filter((w) => w.side === side);
    const name = side === "you" ? "You" : "Others";
    if (mine.length === 0) {
      const quiet = side === "you" ? "unmonitored" : "ok";
      return { side, tone: paused ? "paused" : quiet, label: name, warnings: [] };
    }
    const more = mine.length > 1 ? ` +${mine.length - 1}` : "";
    return { side, tone: "warn", label: `${name} · ${mine[0].label}${more}`, warnings: mine };
  });
}
