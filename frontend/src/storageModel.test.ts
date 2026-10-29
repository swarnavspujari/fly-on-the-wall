import { describe, expect, it } from "vitest";
import { compressLine, compressNoticeCopy, compressPercent, videoLine } from "./storageModel";
import type { CompressStatus, StorageReport } from "./types";

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

describe("videoLine", () => {
  const report = (p: Partial<StorageReport> = {}): StorageReport => ({
    audio_bytes: 0,
    wav_bytes: 0,
    imported_video_bytes: 3 * GB,
    imported_video_files: 2,
    screen_recording_bytes: 1.5 * GB,
    screen_recording_files: 1,
    other_attachment_bytes: 0,
    compress: status(),
    ...p,
  });
  it("lists both kinds with sizes and says they're left alone", () => {
    expect(videoLine(report())).toBe(
      "2 imported meeting videos (3.0 GB), 1 screen recording (1.5 GB). Kept as they are: lossless compression doesn’t shrink video.",
    );
  });
  it("no videos", () => {
    expect(videoLine(report({ imported_video_files: 0, screen_recording_files: 0 }))).toBe(
      "No videos.",
    );
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
