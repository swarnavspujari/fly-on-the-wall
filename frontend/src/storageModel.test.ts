import { describe, expect, it } from "vitest";
import {
  audioOnlyConfirm,
  compressLine,
  compressNoticeCopy,
  compressPercent,
  importedVideosLine,
  screenLine,
  shrinkConfirm,
  videoJobLine,
} from "./storageModel";
import type { CompressStatus, StorageReport, VideoJobStatus } from "./types";

const GB = 1024 ** 3;
const status = (p: Partial<CompressStatus> = {}): CompressStatus => ({
  state: "done",
  paused_for: null,
  meetings_total: 4,
  meetings_done: 4,
  meetings_skipped: 0,
  meetings_failed: 0,
  bytes_total: 10 * GB,
  bytes_done: 10 * GB,
  saved_bytes: 0,
  kept_files: 0,
  notice_pending: false,
  last_error: null,
  ...p,
});

describe("compressLine", () => {
  it("shows progress while working, never 100%", () => {
    expect(compressLine(status({ state: "working", bytes_done: 2.5 * GB }))).toBe(
      "Compressing recordings without losing anything: 25%.",
    );
    expect(compressPercent(status({ state: "working", bytes_done: 10 * GB }))).toBe(99);
    expect(compressPercent(status({ bytes_total: 0 }))).toBe(0);
    expect(
      compressLine(status({ state: "working", bytes_done: 5 * GB, saved_bytes: 4.2 * GB })),
    ).toBe("Compressing recordings without losing anything: 50%. 4.2 GB freed so far.");
  });

  it("says why it's paused", () => {
    expect(compressLine(status({ state: "paused", paused_for: "recording" }))).toBe(
      "Compression is paused while you record.",
    );
    expect(compressLine(status({ state: "paused", paused_for: "transcribing" }))).toMatch(
      /while a transcript is being made/,
    );
    expect(compressLine(status({ state: "paused", paused_for: "ai" }))).toBe(
      "Compression is paused while the AI is working.",
    );
  });

  it("reports what was freed when done, and files left as recorded", () => {
    expect(compressLine(status({ saved_bytes: 12.4 * GB }))).toBe(
      "Every recording is stored losslessly as FLAC. 12 GB freed.",
    );
    expect(compressLine(status())).toBe("Every recording is stored losslessly as FLAC.");
    expect(compressLine(status({ kept_files: 1 }))).toMatch(
      /1 file was left as recorded \(not in the usual format\)/,
    );
    expect(compressLine(status({ kept_files: 3 }))).toMatch(/3 files were left/);
    expect(compressLine(status({ kept_files: 3 }))).toMatch(/^Recordings are stored/);
  });

  it("doesn't claim every recording when some failed or wait for later", () => {
    expect(
      compressLine(
        status({
          saved_bytes: GB,
          meetings_failed: 2,
          meetings_skipped: 1,
          last_error: "not enough free disk space",
        }),
      ),
    ).toBe(
      "Recordings are stored losslessly as FLAC. 1.0 GB freed. 1 meeting was in use and will be compressed later. 2 meetings couldn’t be compressed and are kept as recorded (not enough free disk space).",
    );
    expect(compressLine(status({ meetings_failed: 1 }))).toBe(
      "Recordings are stored losslessly as FLAC. 1 meeting couldn’t be compressed and is kept as recorded.",
    );
    expect(compressLine(status({ meetings_skipped: 3 }))).toMatch(
      /3 meetings were in use and will be compressed later\.$/,
    );
  });

  it("before the first pass", () => {
    expect(compressLine(status({ state: "waiting" }))).toMatch(/in the background/);
    expect(compressLine(status({ state: "waiting", saved_bytes: GB }))).toBe(
      "Stored losslessly as FLAC. 1.0 GB freed.",
    );
  });
});

const videos = (p: Partial<VideoJobStatus> = {}): VideoJobStatus => ({
  kind: null,
  state: "idle",
  paused_for: null,
  files_total: 0,
  files_done: 0,
  files_skipped: 0,
  bytes_saved: 0,
  last_error: null,
  import_policy: "keep",
  ...p,
});
const report = (p: Partial<StorageReport> = {}): StorageReport => ({
  audio_bytes: 0,
  wav_bytes: 0,
  imported_video_bytes: 3 * GB,
  imported_video_files: 2,
  screen_recording_bytes: 1.5 * GB,
  screen_recording_files: 1,
  other_attachment_bytes: 0,
  compress: status(),
  videos: videos(),
  ...p,
});

describe("video rows", () => {
  it("imported videos: sizes and what each choice does", () => {
    expect(importedVideosLine(report())).toBe(
      "2 videos, 3.0 GB. Audio only removes the picture and keeps the audio exactly as it is.",
    );
    expect(importedVideosLine(report({ videos: videos({ import_policy: "audio_only" }) }))).toMatch(
      /keeps only its audio, exactly as it is/,
    );
    expect(importedVideosLine(report({ imported_video_files: 0 }))).toBe("No imported videos.");
    expect(
      importedVideosLine(
        report({ imported_video_files: 0, videos: videos({ import_policy: "audio_only" }) }),
      ),
    ).toMatch(/Videos you import keep only their audio/);
  });

  it("screen recordings", () => {
    expect(screenLine(report())).toBe(
      "1 recording, 1.5 GB. Shrinking re-encodes them to look the same.",
    );
    expect(screenLine(report({ screen_recording_files: 0 }))).toBe("No screen recordings.");
  });

  it("the job line per state", () => {
    expect(videoJobLine(videos())).toBeNull();
    const run = videos({ kind: "shrink_screen", files_total: 4, files_done: 1 });
    expect(videoJobLine({ ...run, state: "working" })).toBe(
      "Shrinking screen recordings: 2 of 4 recordings.",
    );
    expect(videoJobLine({ ...run, state: "working", bytes_saved: 2 * GB })).toMatch(
      /2\.0 GB freed so far/,
    );
    expect(videoJobLine({ ...run, state: "paused", paused_for: "recording" })).toBe(
      "Shrinking screen recordings is paused while you record.",
    );
    expect(videoJobLine({ ...run, state: "done", bytes_saved: 3 * GB, files_skipped: 1 })).toBe(
      "Done: 3.0 GB freed. 1 recording left as it was.",
    );
    expect(videoJobLine({ ...run, state: "stopped", bytes_saved: GB })).toBe(
      "Stopped. 1.0 GB freed.",
    );
    expect(
      videoJobLine(videos({ kind: "audio_only", state: "working", files_total: 1, files_done: 0 })),
    ).toBe("Keeping audio only: 1 of 1 video.");
  });

  it("the confirmations say what happens and what doesn't", () => {
    const a = audioOnlyConfirm(report());
    expect(a.message).toMatch(/picture of 2 imported videos \(3\.0 GB\) is removed/);
    expect(a.message).toMatch(/exactly as it is, and checked before anything is deleted/);
    expect(a.message).toMatch(/files you imported from aren't touched/);
    const s = shrinkConfirm({ files: 4, bytes: 1.5 * GB, bytes_after: 0.5 * GB });
    expect(s.title).toBe("Shrink 4 screen recordings?");
    expect(s.message).toMatch(/About 1\.5 GB → 512 MB/);
    expect(s.message).toMatch(/not bit for bit/);
    expect(s.message).toMatch(/at least 98%/);
  });
});

describe("compressNoticeCopy", () => {
  it("names the space freed and promises nothing changed", () => {
    const c = compressNoticeCopy(12.4 * GB);
    expect(c.title).toBe("Recordings compressed");
    expect(c.cause).toMatch(/freeing 12 GB/);
    expect(c.cause).toMatch(/Every sample is kept/);
  });
});
