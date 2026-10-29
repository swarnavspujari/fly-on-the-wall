// Pure copy for recording storage v2 (audio_store.rs): the Settings ›
// Storage rows and the one-time "recordings compressed" notice. Kept free of
// React so vitest covers every job state.

import { formatBytes } from "./privacy";
import type { CompressStatus, ShrinkEstimate, StorageReport, VideoJobStatus } from "./types";

/** Event the backend emits with a fresh CompressStatus. */
export const COMPRESS_EVENT = "storage:compress";
/** Event the backend emits with a fresh VideoJobStatus. */
export const VIDEO_EVENT = "storage:videos";

const PAUSED_FOR: Record<string, string> = {
  recording: "you record",
  transcribing: "a transcript is being made",
  ai: "the AI is working",
};

/** Progress through this pass, 0–99 while working (never claims 100%). */
export function compressPercent(c: CompressStatus): number {
  if (c.bytes_total <= 0) return 0;
  return Math.min(99, Math.floor((100 * c.bytes_done) / c.bytes_total));
}

/** The Recordings row's helper: what the job is doing and what it freed. */
export function compressLine(c: CompressStatus): string {
  const saved = c.saved_bytes > 0 ? formatBytes(c.saved_bytes) : null;
  const soFar = saved ? ` ${saved} freed so far.` : "";
  switch (c.state) {
    case "working":
      return `Compressing recordings without losing anything: ${compressPercent(c)}%.${soFar}`;
    case "paused":
      return `Compression is paused while ${PAUSED_FOR[c.paused_for ?? ""] ?? "the app is busy"}.${soFar}`;
    case "stopped":
      return saved ? `Compression stopped. ${saved} freed.` : "Compression stopped.";
    case "waiting":
    case "":
      return saved
        ? `Stored losslessly as FLAC. ${saved} freed.`
        : "Recordings are compressed losslessly in the background, a few minutes after launch.";
    default: {
      const kept =
        c.kept_files > 0
          ? ` ${c.kept_files} file${c.kept_files === 1 ? " was" : "s were"} left as recorded (not in the usual format).`
          : "";
      const skipped = c.meetings_skipped;
      const failed = c.meetings_failed;
      const later =
        skipped > 0
          ? ` ${skipped} meeting${skipped === 1 ? " was" : "s were"} in use and will be compressed later.`
          : "";
      const why = c.last_error ? ` (${c.last_error})` : "";
      const notDone =
        failed > 0
          ? ` ${failed} meeting${failed === 1 ? "" : "s"} couldn’t be compressed and ${failed === 1 ? "is" : "are"} kept as recorded${why}.`
          : "";
      const head =
        skipped + failed + c.kept_files === 0
          ? "Every recording is stored losslessly as FLAC."
          : "Recordings are stored losslessly as FLAC.";
      return `${head}${saved ? ` ${saved} freed.` : ""}${later}${notDone}${kept}`;
    }
  }
}

const VIDEO_PAUSED_FOR: Record<string, string> = PAUSED_FOR;

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** The job line shown in the row its kind belongs to, or null when idle. */
export function videoJobLine(v: VideoJobStatus): string | null {
  if (!v.kind || v.state === "idle") return null;
  const what = v.kind === "audio_only" ? "Keeping audio only" : "Shrinking screen recordings";
  const unit = v.kind === "audio_only" ? "video" : "recording";
  const freed = v.bytes_saved > 0 ? ` ${formatBytes(v.bytes_saved)} freed so far.` : "";
  switch (v.state) {
    case "working":
      return `${what}: ${Math.min(v.files_done + 1, v.files_total)} of ${plural(v.files_total, unit)}.${freed}`;
    case "paused":
      return `${what} is paused while ${VIDEO_PAUSED_FOR[v.paused_for ?? ""] ?? "the app is busy"}.${freed}`;
    case "stopped":
      return `Stopped. ${formatBytes(v.bytes_saved)} freed.`;
    default: {
      const left =
        v.files_skipped > 0
          ? ` ${plural(v.files_skipped, unit)} left as ${v.files_skipped === 1 ? "it was" : "they were"}.`
          : "";
      return `Done: ${formatBytes(v.bytes_saved)} freed.${left}`;
    }
  }
}

/** Imported videos row helper. */
export function importedVideosLine(r: StorageReport): string {
  if (r.imported_video_files === 0)
    return r.videos.import_policy === "audio_only"
      ? "Videos you import keep only their audio once they're transcribed."
      : "No imported videos.";
  const size = `${plural(r.imported_video_files, "video")}, ${formatBytes(r.imported_video_bytes)}.`;
  return r.videos.import_policy === "audio_only"
    ? `${size} Each one keeps only its audio, exactly as it is.`
    : `${size} Audio only removes the picture and keeps the audio exactly as it is.`;
}

/** Screen recordings row helper. */
export function screenLine(r: StorageReport): string {
  if (r.screen_recording_files === 0) return "No screen recordings.";
  return `${plural(r.screen_recording_files, "recording")}, ${formatBytes(r.screen_recording_bytes)}. Shrinking re-encodes them to look the same.`;
}

export function audioOnlyConfirm(r: StorageReport): { title: string; message: string } {
  const n = r.imported_video_files;
  return {
    title: "Keep only the audio of imported videos?",
    message:
      (n > 0
        ? `The picture of ${plural(n, "imported video")} (${formatBytes(r.imported_video_bytes)}) is removed from Fly on the Wall, and any subtitles in them. `
        : "The picture (and any subtitles) is removed. ") +
      "The audio is copied out first, exactly as it is, and checked before anything is deleted; transcripts and notes don't change. " +
      "Videos you import later get the same treatment once they're transcribed.\n\n" +
      "The files you imported from aren't touched. If you've deleted them, this removes the last copy of the picture.",
  };
}

export function shrinkConfirm(e: ShrinkEstimate): { title: string; message: string } {
  return {
    title: `Shrink ${plural(e.files, "screen recording")}?`,
    message:
      `About ${formatBytes(e.bytes)} → ${formatBytes(e.bytes_after)} (estimated from a 30-second sample). ` +
      "They're re-encoded to look the same, but not bit for bit.\n\n" +
      "Each one is checked before its original is replaced: it plays from start to end, it's the same length, " +
      "the audio is unchanged, and the picture matches the original (similarity at least 98%). " +
      "It runs in the background and pauses while you record.",
  };
}

/** The one-time notice after the first pass freed space. */
export function compressNoticeCopy(savedBytes: number): { title: string; cause: string } {
  return {
    title: "Recordings compressed",
    cause:
      `Your recordings are now stored as lossless FLAC, freeing ${formatBytes(savedBytes)}. ` +
      "Every sample is kept: playback and transcripts are exactly as before.",
  };
}
