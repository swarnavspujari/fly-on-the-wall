/** Pure progress-mapping logic for the import queue, kept free of React so
 *  it can be unit-tested directly (same pattern as pipelineNotice.ts). */

import type { ImportFile } from "./types";

export type ImportFileStatus = "waiting" | "preparing" | "transcribing" | "done";

export interface MappedProgress {
  statuses: Record<string, ImportFileStatus>;
  /** File-local % for the one active file (null when it can't be derived). */
  activePct: number | null;
}

/** Global ASR % → per-file status + per-file %, using each file's share of
 *  the joined timeline (`boundariesMs` = cumulative end of each file, ms).
 *  ASR % is speech-time, so the mapping is proportional rather than exact —
 *  but it is real pipeline progress, not a timer. */
export function mapProgress(
  files: ImportFile[],
  boundariesMs: number[],
  pct: number,
): MappedProgress {
  const statuses: Record<string, ImportFileStatus> = {};
  const total = boundariesMs[boundariesMs.length - 1] ?? 0;
  if (total <= 0 || files.length !== boundariesMs.length) {
    // no usable boundaries (e.g. a stale manifest) — show honest indeterminate
    for (const f of files) statuses[f.id] = "transcribing";
    return { statuses, activePct: null };
  }
  let activePct: number | null = null;
  let prevEnd = 0;
  let activeSeen = false;
  for (let i = 0; i < files.length; i++) {
    const endFrac = (boundariesMs[i] / total) * 100;
    if (pct >= endFrac - 0.5) {
      statuses[files[i].id] = "done";
    } else if (!activeSeen) {
      activeSeen = true;
      statuses[files[i].id] = "transcribing";
      const startFrac = (prevEnd / total) * 100;
      const share = endFrac - startFrac;
      activePct =
        share > 0 ? Math.min(99, Math.max(0, Math.round(((pct - startFrac) / share) * 100))) : 0;
    } else {
      statuses[files[i].id] = "waiting";
    }
    prevEnd = boundariesMs[i];
  }
  return { statuses, activePct };
}

/** What the pipeline's "transcribing" detail said, structured. Details look
 *  like "cloud 42%", "GPU 42%", "your microphone (CPU 42%)", or
 *  "cloud 40%, waiting for cloud quota (~3m)" (pipeline.rs
 *  transcribe_detail). */
export interface AsrDetail {
  pct: number | null;
  /** Which engine tier is decoding, as the pipeline spells it. */
  engine: "cloud" | "GPU" | "CPU" | null;
  /** Minutes the cloud pacer said it will wait, when it is waiting. */
  quotaWaitMin: number | null;
}

export function parseTranscribingDetail(detail: string | null): AsrDetail {
  if (!detail) return { pct: null, engine: null, quotaWaitMin: null };
  const pct = /(\d+)%/.exec(detail)?.[1];
  const engine = /\b(cloud|GPU|CPU)\b/.exec(detail)?.[1] as AsrDetail["engine"] | undefined;
  const wait = /waiting for cloud quota[^0-9]*(\d+)m/.exec(detail)?.[1];
  return {
    pct: pct != null ? Number(pct) : null,
    engine: engine ?? null,
    quotaWaitMin: wait != null ? Number(wait) : null,
  };
}

/** Compact one-line status for the import queue header while ASR runs:
 *  "3 of 11 · Cloud · 42%" (file-local %), or the pacer's honest
 *  "3 of 11 · Waiting for cloud quota (~3m)" instead of a fake spinner.
 *  Null when the detail carries no percent — callers keep the stage label. */
export function rollingStatus(
  files: ImportFile[],
  boundariesMs: number[],
  detail: string | null,
): string | null {
  const d = parseTranscribingDetail(detail);
  if (d.pct == null) return null;
  const { statuses, activePct } = mapProgress(files, boundariesMs, d.pct);
  const idx = files.findIndex((f) => statuses[f.id] === "transcribing");
  const pos = `${(idx >= 0 ? idx : files.length - 1) + 1} of ${files.length}`;
  if (d.quotaWaitMin != null) return `${pos} · Waiting for cloud quota (~${d.quotaWaitMin}m)`;
  const engine = d.engine === "cloud" ? "Cloud" : d.engine;
  const pct = activePct ?? d.pct;
  return engine != null ? `${pos} · ${engine} · ${pct}%` : `${pos} · ${pct}%`;
}

export const fmtSize = (b: number): string =>
  b >= 1e6 ? `${(b / 1e6).toFixed(1)} MB` : `${Math.max(1, Math.round(b / 1e3))} KB`;
