/** Pure copy + visibility rules for the app-wide notice stack (App.tsx →
 *  AppNotices). Every notice reads cause → numbered fix → collapsible
 *  details, so each builder returns those parts and the components only lay
 *  them out. Kept free of React so vitest covers the per-OS copy directly. */

import { briefError } from "./pipelineNotice";
import type { UpdatePhase } from "./updater";

/** Stack order (spec §3.7): system audio > pipeline error > update, with
 *  a failure that answers the user's own click above them all (its alert
 *  must not hide behind "+N more" under a standing system-audio notice). */
export const NOTICE_PRIORITY = {
  appError: 4,
  systemAudio: 3,
  pipeline: 2,
  update: 1,
} as const;

export interface NoticeCopy {
  title: string;
  /** One line: what happened and what it means for you. */
  cause: string;
  /** Numbered fix, in order. */
  steps: string[];
  detailsLabel?: string;
  /** Collapsed by default: commands, raw error text. */
  details: string | null;
}

/** The macOS bundle id `tccutil` resets (tauri.conf.json identifier). */
export const MACOS_BUNDLE_ID = "com.flyonthewall.app";

/** System audio is silent, so recordings would only hear your microphone.
 *  macOS: the launch preflight found TCC denying the tap (often a stale
 *  grant from an earlier, differently signed install). Windows/Linux: the
 *  output device is muted or on a hands-free profile. */
export function systemAudioCopy(os: string | null): NoticeCopy {
  if (os === "macos") {
    return {
      title: "Can’t hear the other participants",
      cause:
        "macOS is blocking system audio for Fly on the Wall, so recordings would only capture your microphone.",
      steps: [
        "Open System Settings › Privacy & Security › Screen & System Audio Recording.",
        "Turn on Fly on the Wall. If it’s already on, remove it with − and add it back.",
        "Quit and reopen Fly on the Wall.",
      ],
      detailsLabel: "Still blocked?",
      details:
        "An earlier install can hold the permission. Reset it in Terminal, then reopen the app:\n" +
        `tccutil reset ScreenCapture ${MACOS_BUNDLE_ID}`,
    };
  }
  const system = os === "windows" ? "Windows" : "Your computer";
  return {
    title: "Can’t hear the other participants",
    cause: `${system} isn’t sending any sound to the recording, so it would only capture your microphone.`,
    steps: [
      "Unmute your speakers or headset and raise the volume above 0%.",
      "On a Bluetooth headset, pick its stereo output, not hands-free.",
      "Start recording again.",
    ],
    detailsLabel: "Details",
    details:
      "Fly on the Wall records the sound your computer plays, so a muted or silent output records silence.",
  };
}

/** A command that failed after something the user did (start recording,
 *  import, calendar connect…). The raw text varies, so it goes in the
 *  cause line, briefed; the full text stays under Details when it was cut. */
export function appErrorCopy(error: string): NoticeCopy {
  const brief = briefError(error, 160);
  return {
    title: "Something went wrong",
    cause: brief,
    steps: [],
    detailsLabel: "Details",
    details: brief === error.trim() ? null : error,
  };
}

/** Phases the update notice is shown in. "error" only when the download or
 *  install failed: a failed check (even after an earlier find) belongs to
 *  Settings, which shows it. */
export function updateNoticeVisible(u: {
  phase: UpdatePhase;
  dismissed: boolean;
  errorSource?: "check" | "install" | null;
}): boolean {
  if (u.dismissed) return false;
  if (u.phase === "error") return u.errorSource === "install";
  return (
    u.phase === "available" ||
    u.phase === "downloading" ||
    u.phase === "ready" ||
    u.phase === "installing"
  );
}

export function updateCopy(
  phase: UpdatePhase,
  version: string | null,
  error: string | null,
): { title: string; cause: string; details?: string | null } {
  const v = version ? `Version ${version}` : "The new version";
  switch (phase) {
    case "available":
      return {
        title: "Update available",
        cause: `${v} is available. Fly on the Wall restarts once to apply it.`,
      };
    case "downloading":
      return { title: "Downloading update", cause: `${v}. You can keep working.` };
    case "ready":
      return { title: "Update ready", cause: `${v} installs when you restart.` };
    case "installing":
      return { title: "Installing update", cause: "Fly on the Wall restarts itself in a moment." };
    case "error":
      return {
        title: "Update didn’t install",
        cause: `${v} couldn’t be downloaded or installed. Nothing changed.`,
        details: error,
      };
    default:
      return { title: "Update", cause: "" };
  }
}
