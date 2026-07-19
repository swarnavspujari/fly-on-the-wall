import { describe, expect, it } from "vitest";
import { fmtSize, mapProgress, parseTranscribingDetail, rollingStatus } from "./importProgress";
import type { ImportFile } from "./types";

const file = (id: string): ImportFile => ({
  id,
  file_name: `${id}.mp3`,
  size: 1000,
  kind: "audio",
  rel_path: `recordings/x/${id}.mp3`,
  error: null,
});

// 2s + gap + 1s + gap + 3s — the boundaries the backend concat produces
const FILES = [file("a"), file("b"), file("c")];
const BOUNDS = [2000, 4000, 8000];

describe("mapProgress", () => {
  it("attributes the global % to the file whose timeline window contains it", () => {
    // 2000/8000 = 25%, 4000/8000 = 50%
    const at10 = mapProgress(FILES, BOUNDS, 10);
    expect(at10.statuses).toEqual({ a: "transcribing", b: "waiting", c: "waiting" });
    expect(at10.activePct).toBe(40); // 10% of 0–25% window

    const at30 = mapProgress(FILES, BOUNDS, 30);
    expect(at30.statuses).toEqual({ a: "done", b: "transcribing", c: "waiting" });

    const at75 = mapProgress(FILES, BOUNDS, 75);
    expect(at75.statuses).toEqual({ a: "done", b: "done", c: "transcribing" });
    expect(at75.activePct).toBe(50); // halfway through the 50–100% window
  });

  it("marks everything done at 100 and clamps the active % below 100", () => {
    expect(mapProgress(FILES, BOUNDS, 100).statuses).toEqual({ a: "done", b: "done", c: "done" });
    const nearEnd = mapProgress(FILES, BOUNDS, 49); // just under b's 50% boundary
    expect(nearEnd.statuses.b).toBe("transcribing");
    expect(nearEnd.activePct).toBeLessThan(100);
  });

  it("falls back to indeterminate when boundaries are missing or mismatched", () => {
    const noBounds = mapProgress(FILES, [], 40);
    expect(noBounds.activePct).toBeNull();
    expect(Object.values(noBounds.statuses)).toEqual([
      "transcribing",
      "transcribing",
      "transcribing",
    ]);
    expect(mapProgress(FILES, [2000, 4000], 40).activePct).toBeNull();
  });

  it("single file maps the global % straight through", () => {
    const one = mapProgress([file("a")], [5000], 42);
    expect(one.statuses).toEqual({ a: "transcribing" });
    expect(one.activePct).toBe(42);
  });
});

describe("parseTranscribingDetail", () => {
  it("parses the engine-labeled percent details the pipeline emits", () => {
    expect(parseTranscribingDetail("cloud 42%")).toEqual({
      pct: 42,
      engine: "cloud",
      quotaWaitMin: null,
    });
    expect(parseTranscribingDetail("GPU 7%")).toEqual({ pct: 7, engine: "GPU", quotaWaitMin: null });
    expect(parseTranscribingDetail("CPU 99%")).toEqual({
      pct: 99,
      engine: "CPU",
      quotaWaitMin: null,
    });
  });

  it("parses quota-wait details with the ETA", () => {
    expect(parseTranscribingDetail("cloud 40%, waiting for cloud quota (~3m)")).toEqual({
      pct: 40,
      engine: "cloud",
      quotaWaitMin: 3,
    });
  });

  it("handles channel-wrapped, engine-less, and missing details", () => {
    expect(parseTranscribingDetail("your microphone (cloud 42%)")).toEqual({
      pct: 42,
      engine: "cloud",
      quotaWaitMin: null,
    });
    expect(parseTranscribingDetail("42%")).toEqual({ pct: 42, engine: null, quotaWaitMin: null });
    expect(parseTranscribingDetail(null)).toEqual({ pct: null, engine: null, quotaWaitMin: null });
    expect(parseTranscribingDetail("your microphone")).toEqual({
      pct: null,
      engine: null,
      quotaWaitMin: null,
    });
  });
});

describe("rollingStatus", () => {
  it("shows current file, engine, and file-local percent", () => {
    // global 30% → file b (25–50% window), 20% through it
    expect(rollingStatus(FILES, BOUNDS, "cloud 30%")).toBe("2 of 3 · Cloud · 20%");
    expect(rollingStatus(FILES, BOUNDS, "GPU 30%")).toBe("2 of 3 · GPU · 20%");
    expect(rollingStatus(FILES, BOUNDS, "30%")).toBe("2 of 3 · 20%");
  });

  it("says so while the cloud pacer waits, instead of a fake spinner", () => {
    expect(rollingStatus(FILES, BOUNDS, "cloud 30%, waiting for cloud quota (~3m)")).toBe(
      "2 of 3 · Waiting for cloud quota (~3m)",
    );
  });

  it("returns null without a percent (caller falls back to the stage label)", () => {
    expect(rollingStatus(FILES, BOUNDS, null)).toBeNull();
    expect(rollingStatus(FILES, BOUNDS, "your microphone")).toBeNull();
  });
});

describe("fmtSize", () => {
  it("formats MB and KB like the design", () => {
    expect(fmtSize(12_400_000)).toBe("12.4 MB");
    expect(fmtSize(340_000)).toBe("340 KB");
    expect(fmtSize(200)).toBe("1 KB");
  });
});
