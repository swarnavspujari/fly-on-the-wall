import { describe, expect, it } from "vitest";
import { compressLine, compressNoticeCopy, compressPercent, videoLine } from "./storageModel";
import type { CompressStatus, StorageReport } from "./types";

const GB = 1024 ** 3;
const status = (p: Partial<CompressStatus> = {}): CompressStatus => ({
  state: "done",
  paused_for: null,
  meetings_total: 4,
  meetings_done: 4,
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
    expect(compressLine(status({ state: "paused", paused_for: "importing" }))).toMatch(
      /while files are imported/,
    );
  });

  it("reports what was freed when done, and files left as recorded", () => {
    expect(compressLine(status({ saved_bytes: 23.1 * GB }))).toBe(
      "Every recording is stored losslessly as FLAC. 23 GB freed.",
    );
    expect(compressLine(status())).toBe("Every recording is stored losslessly as FLAC.");
    expect(compressLine(status({ kept_files: 1 }))).toMatch(
      /1 file was left as recorded \(not in the usual format\)/,
    );
    expect(compressLine(status({ kept_files: 3 }))).toMatch(/3 files were left/);
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
    imported_video_bytes: 14 * GB,
    imported_video_files: 6,
    screen_recording_bytes: 5.6 * GB,
    screen_recording_files: 1,
    other_attachment_bytes: 0,
    compress: status(),
    ...p,
  });
  it("lists both kinds with sizes and says they're left alone", () => {
    expect(videoLine(report())).toBe(
      "6 imported meeting videos (14 GB), 1 screen recording (5.6 GB). Kept as they are: lossless compression doesn’t shrink video.",
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
    const c = compressNoticeCopy(23.1 * GB);
    expect(c.title).toBe("Recordings compressed");
    expect(c.cause).toMatch(/freeing 23 GB/);
    expect(c.cause).toMatch(/Every sample is kept/);
  });
});
