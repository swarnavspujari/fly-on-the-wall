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

export const fmtSize = (b: number): string =>
  b >= 1e6 ? `${(b / 1e6).toFixed(1)} MB` : `${Math.max(1, Math.round(b / 1e3))} KB`;
