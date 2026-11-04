// Plain-language copy and state for the local-model picker and the one-time
// offer in Settings › Ask and Enhance. The numbers come from the backend's
// one table (local_models.rs, measured by the Ask bench, docs/ASK-MODEL-BENCH.md);
// this file only says them in words a person uses: no "grounded", no "tier".
import type { LocalModelAdvice, LocalModelMeasured, LocalModelRow, OllamaModel } from "./types";

/** "6.6 GB", "620 MB". */
export function sizeLabel(bytes: number): string {
  return bytes >= 1_000_000_000
    ? `${(bytes / 1_000_000_000).toFixed(1)} GB`
    : `${Math.round(bytes / 1_000_000)} MB`;
}

/** What was measured, as a sentence. Numbers only. */
export function measuredLine(m: LocalModelMeasured): string {
  return (
    `Answers backed by a source: ${m.sourced_pct}%. ` +
    `Declines what it can’t answer: ${m.declines_pct}%. ` +
    `About ${m.seconds} seconds per answer.`
  );
}

/** "15" tenths → "1.5". */
const tenths = (n: number) => (n / 10).toFixed(1).replace(/\.0$/, "");

/** What the work after a meeting costs, when it costs more than llama3.1. */
export function slowerLine(m: LocalModelMeasured): string | null {
  if (!m.background_slower) return null;
  const [lo, hi] = m.background_slower;
  return `Notes and cleanup after a meeting take about ${tenths(lo)}–${tenths(hi)} times as long as with llama3.1.`;
}

/** What the model asks of the computer. */
export function memoryLine(row: LocalModelRow): string {
  return `Uses about ${Math.round(row.memory_gb)} GB of memory while answering. Suits computers with ${row.ram_class_gb} GB or more.`;
}

/** The headline of the one-time offer. */
export function offerTitle(model: string, bytes: number): string {
  return `A better local model for this computer: ${model} · ${sizeLabel(bytes)}`;
}

/** The note when a better model exists but the running Ollama is too old. */
export function heldBackNote(h: NonNullable<LocalModelAdvice["held_back"]>): string {
  const have = h.have_ollama ? ` This computer has ${h.have_ollama}.` : "";
  return `${h.model} needs Ollama ${h.needs_ollama} or newer.${have} Update Ollama to use it.`;
}

/** The pinned default needs a newer Ollama than the one running: say so
 *  next to the Download button instead of letting the pull fail. */
export function blockedNote(advice: LocalModelAdvice | null, effective: string): string | null {
  const b = advice?.default_blocked;
  if (!b || !sameModel(b.model, effective)) return null;
  return heldBackNote(b);
}

/** A pull's error in words a person can act on. Ollama's own refusal for a
 *  model its version doesn't have becomes "update Ollama"; anything else is
 *  shown as it came. */
export function friendlyPullError(
  e: unknown,
  model: string,
  advice: LocalModelAdvice | null,
): string {
  const raw = String(e);
  if (!/newer version of Ollama/i.test(raw)) return raw;
  const row = rowFor(advice, model);
  const needs = row?.needs_newer_ollama;
  const have = advice?.ollama_version;
  return (
    `${model} needs ${needs ? `Ollama ${needs} or newer` : "a newer Ollama"}.` +
    (have ? ` This computer has ${have}.` : "") +
    " Update Ollama (ollama.com), then try again."
  );
}

/** Tag-insensitive match, as the backend: a bare name matches any tag of its
 *  family; two explicit tags must be identical. */
export function sameModel(a: string, b: string): boolean {
  if (a === b) return true;
  const ab = a.split(":")[0];
  const bb = b.split(":")[0];
  return (a === ab || b === bb) && ab === bb;
}

export type ChoiceState = "in_use" | "installed" | "download";

/** Where a model stands on this computer: the one in use, one that is
 *  already downloaded, or one to download first. */
export function choiceState(
  row: LocalModelRow,
  installed: OllamaModel[] | undefined,
  effective: string,
): ChoiceState {
  if (sameModel(row.model, effective)) return "in_use";
  return installed?.some((m) => sameModel(m.name, row.model)) ? "installed" : "download";
}

/** The label on the button that makes `row` the model in use. */
export function choiceAction(row: LocalModelRow, state: ChoiceState, busy: boolean): string {
  if (busy) return "Working…";
  return state === "download" ? `Download and use (${sizeLabel(row.download_bytes)})` : "Use";
}

/** The label on the offer's accept button. */
export function offerAction(
  offer: { model: string; download_bytes: number },
  installed: OllamaModel[] | undefined,
  busy: boolean,
): string {
  if (busy) return "Working…";
  return installed?.some((m) => sameModel(m.name, offer.model))
    ? "Switch"
    : `Download and switch (${sizeLabel(offer.download_bytes)})`;
}

/** The row for a model, if the table has it. */
export function rowFor(advice: LocalModelAdvice | null, model: string): LocalModelRow | null {
  return advice?.models.find((m) => sameModel(m.model, model)) ?? null;
}

/** Whether the offer shows: there is one, Ollama is installed (the offer is
 *  about a model, not the runtime), and the provider is the local one. */
export function showOffer(
  advice: LocalModelAdvice | null,
  ollamaInstalled: boolean,
  provider: string,
): boolean {
  return !!advice?.offer && ollamaInstalled && provider === "ollama";
}
