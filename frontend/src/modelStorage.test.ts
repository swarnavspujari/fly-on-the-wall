import { describe, expect, it } from "vitest";
import {
  formatBytes,
  freeUpPlan,
  freeUpSummary,
  outcomeMessage,
  rowDetail,
  rowStatus,
  shortName,
} from "./modelStorage";
import type { ArtifactUsage, DeleteOutcome, ModelStorageReport } from "./types";

const row = (over: Partial<ArtifactUsage> & { id: string }): ArtifactUsage => ({
  display: over.id,
  installed: true,
  size_on_disk: 1_000_000,
  download_bytes: 1_000_000,
  in_use: null,
  note: "Not used by your current settings. It downloads again if you need it.",
  can_delete: true,
  blocked: null,
  ...over,
});

const report = (over: Partial<ModelStorageReport> = {}): ModelStorageReport => ({
  artifacts: [],
  leftovers: [],
  unrecognised: [],
  reclaimable_bytes: 0,
  busy: null,
  ...over,
});

describe("formatBytes", () => {
  it("uses the download sizes' decimal units", () => {
    expect(formatBytes(539_212_467)).toBe("539 MB");
    expect(formatBytes(1_081_140_203)).toBe("1.1 GB");
    expect(formatBytes(2_048)).toBe("2 KB");
    expect(formatBytes(7)).toBe("1 KB");
    expect(formatBytes(0)).toBe("0 KB");
  });
});

describe("shortName", () => {
  it("drops the catalog tail unless two rows would read the same", () => {
    const medium = row({ id: "m", display: "Whisper medium (Q5, ~540 MB)" });
    const turbo = row({
      id: "t",
      display: "Whisper large-v3-turbo (Q5, ~574 MB) — Balanced/Best default",
    });
    const cpu = row({ id: "c", display: "whisper.cpp CLI (CPU, v1.9.5)" });
    const gpu = row({ id: "g", display: "whisper.cpp CLI (Vulkan GPU, v1.9.5)" });
    const all = [medium, turbo, cpu, gpu];
    expect(shortName(medium, all)).toBe("Whisper medium");
    expect(shortName(turbo, all)).toBe("Whisper large-v3-turbo");
    expect(shortName(gpu, all)).toBe("whisper.cpp CLI (Vulkan GPU, v1.9.5)");
    expect(shortName(cpu, all)).toBe("whisper.cpp CLI (CPU, v1.9.5)");
  });
});

describe("row status and detail", () => {
  it("reads in use, unused and not installed", () => {
    const inUse = row({
      id: "a",
      in_use: "Used for Balanced transcription.",
      note: null,
      can_delete: false,
      blocked: "In use, so it can't be deleted. Used for Balanced transcription.",
      size_on_disk: 574_041_195,
    });
    expect(rowStatus(inUse)).toBe("in_use");
    expect(rowDetail(inUse)).toBe("574 MB on disk · Used for Balanced transcription.");

    const unused = row({ id: "b", size_on_disk: 539_212_467 });
    expect(rowStatus(unused)).toBe("unused");
    expect(rowDetail(unused)).toContain("539 MB on disk · Not used by your current settings.");

    const absent = row({
      id: "c",
      installed: false,
      size_on_disk: 0,
      download_bytes: 1_081_140_203,
      note: null,
      can_delete: false,
    });
    expect(rowStatus(absent)).toBe("not_installed");
    expect(rowDetail(absent)).toBe("1.1 GB download");
  });
});

describe("freeUpPlan", () => {
  it("takes only deletable models plus every leftover, never unrecognised files", () => {
    const r = report({
      artifacts: [
        row({ id: "medium", display: "Whisper medium (Q5, ~540 MB)", size_on_disk: 500 }),
        row({ id: "turbo", in_use: "Used for Balanced transcription.", can_delete: false }),
        row({ id: "large", installed: false, size_on_disk: 0, can_delete: false }),
      ],
      leftovers: [
        { path: "x.download.1.0", bytes: 70, kind: "unfinished_download", label: "Unfinished" },
      ],
      unrecognised: [
        { path: "models/asr/notes.txt", bytes: 9, kind: "not_recognised", label: "?" },
      ],
    });
    const plan = freeUpPlan(r);
    expect(plan.ids).toEqual(["medium"]);
    expect(plan.paths).toEqual(["x.download.1.0"]);
    expect(plan.items).toEqual([
      { label: "Whisper medium", bytes: 500 },
      { label: "Unfinished", bytes: 70 },
    ]);
    expect(plan.bytes).toBe(570);
    expect(freeUpSummary(plan)).toBe(
      "Removes 1 unused model and 1 leftover. Anything you need later downloads again.",
    );
  });

  it("says so when there's nothing to free (everything in use, or busy)", () => {
    const busy = report({
      artifacts: [row({ id: "medium", can_delete: false, blocked: "Recording now." })],
      busy: "Recording now.",
    });
    const plan = freeUpPlan(busy);
    expect(plan.items).toEqual([]);
    expect(freeUpSummary(plan)).toBe("Nothing to free up. Everything here is in use.");
  });
});

describe("outcomeMessage", () => {
  const base: DeleteOutcome = {
    freed_bytes: 0,
    removed: [],
    skipped: [],
    failed: [],
    report: report(),
  };
  it("reports freed space, kept items and failures", () => {
    expect(outcomeMessage({ ...base, freed_bytes: 540_000_000, removed: ["m"] })).toEqual({
      text: "Freed 540 MB.",
      tone: "default",
    });
    expect(outcomeMessage({ ...base, skipped: ["Whisper medium: in use"] }).text).toBe(
      "Nothing was deleted. Kept: Whisper medium: in use.",
    );
    const failed = outcomeMessage({
      ...base,
      freed_bytes: 1_000_000,
      removed: ["a"],
      failed: ["ffmpeg: access denied"],
    });
    expect(failed.tone).toBe("error");
    expect(failed.text).toBe("Freed 1 MB. Couldn't delete: ffmpeg: access denied.");
  });
});
