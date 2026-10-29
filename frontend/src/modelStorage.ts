/* Settings › Models on this computer: the pure half of "Free up space"
   (ModelsOnComputer.tsx renders it). The backend (model_cleanup.rs) decides
   what's in use and re-checks every delete; this only shapes its report. */

import type { ArtifactUsage, DeleteOutcome, ModelStorageReport } from "./types";

/** "540 MB", "1.6 GB", "12 KB" (decimal units, like the download sizes). */
export function formatBytes(bytes: number): string {
  if (bytes >= 1_000_000_000) return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
  if (bytes >= 1_000_000) return `${Math.round(bytes / 1_000_000)} MB`;
  if (bytes > 0) return `${Math.max(1, Math.round(bytes / 1_000))} KB`;
  return "0 KB";
}

/** The name a question uses: the catalog name without its "(Q5, ~540 MB) —
 *  …" tail ("Whisper medium"), unless another row would then read the same
 *  (the CPU and Vulkan whisper.cpp builds keep their full names). */
export function shortName(a: ArtifactUsage, all: readonly ArtifactUsage[]): string {
  const short = (d: string) => d.split(" — ")[0].split(" (")[0].trim();
  const s = short(a.display);
  const clash = all.some((o) => o.id !== a.id && short(o.display) === s);
  return clash || !s ? a.display : s;
}

/** How a row reads: in use, installed but unused, or not on this computer. */
export type RowStatus = "in_use" | "unused" | "not_installed";

export function rowStatus(a: ArtifactUsage): RowStatus {
  if (!a.installed) return "not_installed";
  return a.in_use ? "in_use" : "unused";
}

/** The row's second line: size on disk and what it's for. */
export function rowDetail(a: ArtifactUsage): string {
  if (!a.installed) return `${formatBytes(a.download_bytes)} download`;
  const why = a.in_use ?? a.note;
  return why
    ? `${formatBytes(a.size_on_disk)} on disk · ${why}`
    : `${formatBytes(a.size_on_disk)} on disk`;
}

export interface FreeUpItem {
  label: string;
  bytes: number;
}

/** Everything one "Free up" click removes: unused models and leftovers. */
export interface FreeUpPlan {
  ids: string[];
  paths: string[];
  items: FreeUpItem[];
  bytes: number;
  models: number;
  leftovers: number;
}

export function freeUpPlan(report: ModelStorageReport): FreeUpPlan {
  const models = report.artifacts.filter((a) => a.can_delete);
  const items: FreeUpItem[] = [
    ...models.map((a) => ({ label: shortName(a, report.artifacts), bytes: a.size_on_disk })),
    ...report.leftovers.map((e) => ({ label: e.label, bytes: e.bytes })),
  ];
  return {
    ids: models.map((a) => a.id),
    paths: report.leftovers.map((e) => e.path),
    items,
    bytes: items.reduce((sum, i) => sum + i.bytes, 0),
    models: models.length,
    leftovers: report.leftovers.length,
  };
}

const count = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** "Free up removes 2 unused models and 1 leftover." / the nothing-to-do line. */
export function freeUpSummary(plan: FreeUpPlan): string {
  if (plan.items.length === 0) return "Nothing to free up. Everything here is in use.";
  const parts = [
    plan.models > 0 && count(plan.models, "unused model", "unused models"),
    plan.leftovers > 0 && count(plan.leftovers, "leftover", "leftovers"),
  ].filter(Boolean);
  return `Removes ${parts.join(" and ")}. Anything you need later downloads again.`;
}

/** What to say after a delete: freed space, plus anything kept or failed. */
export function outcomeMessage(o: DeleteOutcome): { text: string; tone: "default" | "error" } {
  const parts: string[] = [];
  if (o.removed.length > 0) parts.push(`Freed ${formatBytes(o.freed_bytes)}.`);
  else parts.push("Nothing was deleted.");
  if (o.skipped.length > 0) parts.push(`Kept: ${o.skipped.join("; ")}.`);
  if (o.failed.length > 0) parts.push(`Couldn't delete: ${o.failed.join("; ")}.`);
  return { text: parts.join(" "), tone: o.failed.length > 0 ? "error" : "default" };
}
