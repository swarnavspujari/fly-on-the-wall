// Pure copy for recording storage v2 (audio_store.rs): the Settings ›
// Storage rows and the one-time "recordings compressed" notice. Kept free of
// React so vitest covers every job state.

import { formatBytes } from "./privacy";
import type { CompressStatus, StorageReport } from "./types";

/** Event the backend emits with a fresh CompressStatus. */
export const COMPRESS_EVENT = "storage:compress";

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

/** The Videos row: a size report only; nothing compresses videos. */
export function videoLine(r: StorageReport): string {
  const parts: string[] = [];
  if (r.imported_video_files > 0)
    parts.push(
      `${r.imported_video_files} imported meeting video${r.imported_video_files === 1 ? "" : "s"} (${formatBytes(r.imported_video_bytes)})`,
    );
  if (r.screen_recording_files > 0)
    parts.push(
      `${r.screen_recording_files} screen recording${r.screen_recording_files === 1 ? "" : "s"} (${formatBytes(r.screen_recording_bytes)})`,
    );
  if (parts.length === 0) return "No videos.";
  return `${parts.join(", ")}. Kept as they are: lossless compression doesn’t shrink video.`;
}

export function videoBytes(r: StorageReport): number {
  return r.imported_video_bytes + r.screen_recording_bytes;
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
