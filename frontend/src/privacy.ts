// Pure helpers behind the privacy controls (Settings → Privacy, the editor's
// Keep-local chip, the recording-notice helper). Kept free of React and IPC
// so vitest covers them directly.

import type { DiskEncryption, MeetingPrivacy, WipeReport } from "./types";

/** Settings key remembering that the disk-encryption banner was dismissed. */
export const ENCRYPTION_BANNER_DISMISSED_KEY = "ui.privacy.disk_encryption_banner_dismissed";

/** Text the user types to arm "Delete all my data" (the backend checks it too). */
export const WIPE_CONFIRMATION = "delete everything";

/** Window event fired after a keep-local change so open views re-read it. */
export const PRIVACY_CHANGED_EVENT = "fotw:privacy-changed";

/** Chat-pasteable notice for the other participants. */
export function recordingNoticeText(meetingTitle?: string | null): string {
  const title = meetingTitle?.trim();
  const what = title ? `this meeting ("${title}")` : "this meeting";
  return (
    `Heads up: I'm recording ${what} to take notes. ` +
    `The recording and transcript stay on my computer. ` +
    `Let me know if you'd rather I didn't record.`
  );
}

export interface RetentionOption {
  value: string;
  label: string;
}

/** Select options for the retention setting; "" means never delete. */
export function retentionOptions(choices: number[]): RetentionOption[] {
  return [
    { value: "", label: "Never (keep audio)" },
    ...[...choices]
      .sort((a, b) => a - b)
      .map((d) => ({ value: String(d), label: d === 365 ? "After 1 year" : `After ${d} days` })),
  ];
}

/** Parse the select's value back into the command's argument. */
export function parseRetention(value: string, choices: number[]): number | null {
  if (value === "") return null;
  const n = Number(value);
  return choices.includes(n) ? n : null;
}

/** Warning shown before a retention change that deletes audio right away. */
export function retentionWarning(days: number, affected: number): string | null {
  if (affected <= 0) return null;
  const what = affected === 1 ? "1 meeting" : `${affected} meetings`;
  return (
    `Audio from ${what} older than ${days === 365 ? "1 year" : `${days} days`} ` +
    `will be deleted now. Transcripts and notes are kept.`
  );
}

/** Only a clear "off" shows the banner; "unknown" never nags. */
export function showEncryptionBanner(
  status: DiskEncryption | null,
  dismissed: string | null,
): boolean {
  return status?.state === "off" && dismissed !== "true";
}

export function encryptionBannerText(status: DiskEncryption): string {
  const method = status.method ?? "disk encryption";
  const where = status.volume ? ` for the ${status.volume} drive` : "";
  return (
    `${method} looks turned off${where}. Your recordings, transcripts and notes are ` +
    `stored as files on this computer, so they're only as protected as the disk: anyone ` +
    `with the device could read them. Turning on ${method} protects them if the device ` +
    `is lost or stolen.`
  );
}

/** The armed state of the two-step delete: both confirmations given. */
export function canConfirmWipe(acknowledged: boolean, typed: string): boolean {
  return acknowledged && typed.trim().toLowerCase() === WIPE_CONFIRMATION;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = bytes / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 10 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** Effective keep-local (own flag or a folder's). */
export function isKeptLocal(p: MeetingPrivacy | null): boolean {
  return !!p && (p.keep_local || p.keep_local_from_folder);
}

/** Tooltip for the editor's Keep-local chip. */
export function keepLocalTitle(p: MeetingPrivacy | null): string {
  if (p?.keep_local_from_folder) {
    return "Kept local by its folder: AI features use only Ollama on this device, and transcription never uses Groq. Change it on the folder in the sidebar.";
  }
  if (p?.keep_local) {
    return "Keep local is on: AI features use only Ollama on this device, and transcription never uses Groq. Click to allow cloud processing again.";
  }
  return "Keep local: never send this note's audio, transcript or notes to a cloud service (cloud AI providers or Groq).";
}

/** Window event carrying a finished "Delete all my data" to the App-level
 *  RestartRequired screen (it must outlive the Settings modal). */
export const RESTART_REQUIRED_EVENT = "fotw:restart-required";

export interface RestartRequiredDetail {
  report: WipeReport;
  includeModels: boolean;
}

/** How the in-app half of the wipe went, which decides what the
 *  restart-required screen offers:
 *  - "clean": everything erased → restart on its own;
 *  - "db-open": the database is held open elsewhere (MCP client) → its old
 *    contents may survive; ask the user to quit those apps and try again;
 *  - "leftovers": some files/keychain entries are stuck → list them, restart
 *    finishes the files. */
export function wipeOutcome(r: WipeReport): "clean" | "db-open" | "leftovers" {
  if (!r.db_erased || !r.db_checkpointed) return "db-open";
  if (r.failed.length > 0 || r.secrets_failed.length > 0) return "leftovers";
  return "clean";
}

export const RESTART_FAILED_TEXT =
  "Fly on the Wall couldn't restart itself. Quit it (and make sure it's closed) and open it again to finish deleting.";
