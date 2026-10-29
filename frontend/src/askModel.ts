/* Ask: pure helpers for the composer and answer sheet (AskPanel.tsx), kept
   here so the copy is unit-tested (askModel.test.ts). Ask still goes through
   the one backend path, `ask_meeting` → `build_provider`, which enforces
   cloud consent and Keep local; nothing here is a second LLM path. */

import { isLoopbackUrl } from "./homeModel";
import { providerVendor } from "./settingsModel";
import type { AskFlag, AskRefusal, AskReply, AskScope, AskStage, AskTier } from "./types";

/** The prompts offered before the first question. */
export const QUICK_PROMPTS = [
  "What did I miss?",
  "What decisions were made?",
  "List the action items",
  "Draft a follow-up email",
] as const;

/** Claude models offered for the Anthropic provider; the first is the
 *  default. Same list as Settings › Ask and Enhance (SettingsModal's
 *  ANTHROPIC_MODELS); keep the two in step. */
export const CLAUDE_MODELS = [
  { value: "claude-sonnet-5-5", label: "Claude Sonnet 5.5 (recommended)" },
  { value: "claude-haiku-5-5", label: "Claude Haiku 5.5 (faster, lower cost)" },
];

/** Who a provider is, for sentences: "Ollama" for the local one, else the
 *  company ("Anthropic", "OpenAI", "NVIDIA"). */
export function providerName(id: string): string {
  return id === "ollama" ? "Ollama" : providerVendor(id);
}

export interface AskErrorView {
  /** What went wrong, in a few words. */
  title: string;
  /** What to do about it. */
  fix: string;
  /** The backend's own text, for the collapsible details. */
  raw: string;
  /** A fix the sheet can offer as a button ("consent": ask for consent
   *  again, then retry). */
  action?: "consent";
}

/** Turn an `ask_meeting` failure into words a person can act on. Matches
 *  the backend's own strings (llm_commands.rs build_provider / keep-local /
 *  consent, ollama.rs, fly-llm transport and HTTP errors); anything else
 *  falls back to a plain "couldn't answer" with the details below it.
 *  `providerId` is the provider the question went to (read when it was
 *  asked), not whatever Runs on shows by the time the error is read. */
export function describeAskError(error: unknown, providerId: string): AskErrorView {
  const raw = String(error ?? "").trim() || "Unknown error";
  const t = raw.toLowerCase();
  const who = providerName(providerId);
  const view = (title: string, fix: string, action?: AskErrorView["action"]): AskErrorView =>
    action ? { title, fix, raw, action } : { title, fix, raw };

  if (t.includes("keep local")) {
    return view(
      "This note is kept local",
      `Ask won't send it to ${who}. Pick This computer under Runs on, or turn off Keep local for this note.`,
    );
  }
  if (t.includes("isn't allowed to receive your meeting content")) {
    return view(
      `${who} isn't confirmed yet`,
      `Ask needs your OK before it sends this note to ${who}.`,
      "consent",
    );
  }
  if (t.includes("no api key")) {
    return view(
      `No API key for ${who}`,
      "Add one in Settings › Ask and Enhance, or pick This computer under Runs on.",
    );
  }
  // fly-llm already maps 401/403 to this text; a bare status code elsewhere
  // in an error body isn't proof of a bad key.
  if (t.includes("authentication failed")) {
    return view(`${who} didn't accept the API key`, "Check the key in Settings › Ask and Enhance.");
  }
  if (t.includes("ollama isn't installed")) {
    return view(
      "The local AI isn't installed",
      "Install it in Settings › Ask and Enhance, or pick a cloud provider under Runs on.",
    );
  }
  // Only Ollama models are "downloaded"; a cloud 404 is something else.
  if (
    providerId === "ollama" &&
    /model .*not found|try pulling|pull the model|model .*not installed/.test(t)
  ) {
    return view(
      "The local model isn't downloaded",
      "Download it in Settings › Ask and Enhance, then try again.",
    );
  }
  if (
    t.includes("ollama isn't reachable") ||
    t.includes("isn't answering yet") ||
    t.includes("could not start ollama")
  ) {
    return view(
      "Can't reach the local AI",
      "Ollama isn't running or is still starting. Wait a few seconds and try again.",
    );
  }
  if (
    /context length|context window|maximum context|(prompt|input) is too long|too many tokens/.test(
      t,
    )
  ) {
    return view(
      "This meeting is too long for the model",
      "Try a model with a larger context in Settings › Ask and Enhance.",
    );
  }
  if (t.includes("timed out") || t.includes("timeout")) {
    return view(
      `${who} took too long to answer`,
      "The model may still be loading. Try again in a moment.",
    );
  }
  if (/\b(429|529)\b|rate limit|too many requests|overloaded/.test(t)) {
    return view(`${who} is busy right now`, "Wait a minute, then try again.");
  }
  // Before the connection check: a 5xx body often mentions "connection".
  if (/\b5\d\d\b/.test(t)) {
    return view(`${who} had a problem answering`, "That's on their side. Try again in a minute.");
  }
  if (
    t.includes("can't reach") ||
    t.includes("network error") ||
    t.includes("error sending request") ||
    t.includes("connection")
  ) {
    return view(`Can't reach ${who}`, "Check your internet connection, then try again.");
  }
  if (/no message content|no content|empty (answer|response|reply)/.test(t)) {
    return view("The answer came back empty", "Try again, or try another model.");
  }
  // The Ask harness's own model failures (fly-ask lib.rs).
  if (t.includes("cut off before it finished")) {
    return view("The answer was cut off", "Try again, or ask a narrower question.");
  }
  if (t.includes("used up its budget")) {
    return view(
      "Ask ran out of room before the model answered",
      "Ask a narrower question, or use @ to pick one meeting.",
    );
  }
  if (t.includes("tool calls couldn't be understood")) {
    return view(
      "The model didn't follow Ask's steps",
      "Try again, or pick another model in Settings › Ask and Enhance.",
    );
  }
  if (t.includes("couldn't read your meetings")) {
    return view("Ask couldn't read your meetings", "Try again in a moment.");
  }
  return view("Ask couldn't answer", "Try again. If it keeps failing, the details below may help.");
}

/** Where a provider runs, for Runs on: this computer (a loopback Ollama),
 *  another machine ("Off this computer · host:port", the privacy pill's
 *  words), or a cloud vendor. */
export function runsOnPlace(info: { id: string; is_local: boolean; base_url: string | null }): {
  kind: "local" | "remote" | "cloud";
  label: string;
} {
  if (!info.is_local) return { kind: "cloud", label: providerName(info.id) };
  if (info.base_url && !isLoopbackUrl(info.base_url)) {
    let host = info.base_url;
    try {
      host = new URL(info.base_url).host;
    } catch {
      /* show it as given */
    }
    return { kind: "remote", label: `Off this computer · ${host}` };
  }
  return { kind: "local", label: "This computer" };
}

/** Under Runs on: the provider is global, not just Ask's. */
export const RUNS_ON_HELPER = "Also used for Enhance, transcript cleanup and action items.";

/** After a switch: say everything that moved. */
export const runsOnSwitched = (label: string) =>
  `Ask, Enhance, transcript cleanup and action items now run on ${label}.`;

/* ------------------------------------------------------------------ harness
   Copy for what the Ask harness (fly-ask) returns: the streaming stages, the
   trace, where an answer ran, and why a sentence or a proposal isn't
   trusted. Everything here is built from counts and enum codes, never from
   meeting text, so the trace can't leak what was said. */

/** A scope that searches more than one meeting. */
export const isWideScope = (scope: AskScope) =>
  scope.kind === "all" || scope.kind === "folder" || scope.kind === "person";

/** What Ask is doing while an answer streams. */
export function stageCopy(stage: AskStage | null, scope: AskScope): string {
  switch (stage) {
    case "searching":
      return isWideScope(scope) ? "Searching your meetings…" : "Searching this meeting…";
    case "answering":
      return "Writing the answer…";
    case "reading":
      return "Looking further…";
    case "checking":
      return "Checking the answer against the transcript…";
    case "retrying":
      return "Asking the model again…";
    default:
      return "Getting ready…";
  }
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** Memory tools (fly-context's memory layer): a briefing on a person or a
 *  recurring meeting, open items, what changed. */
const MEMORY_TOOLS = new Set(["get_briefing", "open_items", "whats_changed"]);

/** "tool_search_passages" → "search_passages". */
export const toolName = (code: string) => code.replace(/^tool_/, "");

/** Ask used what it remembers (a memory briefing or a memory tool). */
export function usedMemory(reply: AskReply): boolean {
  if ((reply.run_log?.prefetch?.memory_facts ?? 0) > 0) return true;
  return (reply.trace?.tool_calls ?? []).some((c) => MEMORY_TOOLS.has(toolName(c.tool)));
}

/** The collapsed trace pill: "Searched this meeting · read 6 passages". */
export function traceSummary(reply: AskReply): string {
  const t = reply.trace;
  if (!t) return reply.trace_lines[0] ?? "How Ask answered";
  const parts: string[] = [];
  if (t.scope === "meeting" || t.scope === "note") parts.push("Searched this meeting");
  else if (t.scope) parts.push(`Searched ${plural(t.meetings_in_scope, "meeting")}`);
  if (t.floor_not_found) parts.push("nothing matched");
  else if (t.passages > 0) parts.push(`read ${plural(t.passages, "passage")}`);
  if (t.tool_calls.length > 0) parts.push(`looked further ${plural(t.tool_calls.length, "time")}`);
  if (usedMemory(reply)) parts.push("used memory");
  return parts.length ? parts.join(" · ") : "How Ask answered";
}

/** The tier as a person reads it ("one step" vs "multi-step"). */
export function tierLabel(tier: AskTier | null | undefined): string | null {
  if (tier === "S") return "one step";
  if (tier === "M" || tier === "C") return "multi-step";
  return null;
}

/** What the tier means, for the trace and the pill's tooltip. */
export function tierDetail(tier: AskTier | null | undefined): string | null {
  switch (tier) {
    case "S":
      return "One step: the model answered from what Ask found.";
    case "M":
      return "Multi-step: the model could search again, up to 3 times.";
    case "C":
      return "Multi-step: the model could search again, up to 8 times.";
    default:
      return null;
  }
}

export interface RunsOnView {
  kind: "local" | "remote" | "cloud";
  /** "On this computer", "On another computer", or the vendor ("Anthropic"). */
  label: string;
}

/** Where an answer ran, from the run log's reach (else `on_device`).
 *  `providerAtSend` names the vendor when the log doesn't. */
export function answerRunsOn(reply: AskReply, providerAtSend: string): RunsOnView {
  const reach = reply.run_log?.reach;
  const vendor = () => {
    const code = reply.run_log?.provider?.replace(/^provider_/, "");
    return providerName(code && code !== "other" ? code : providerAtSend);
  };
  if (reach === "reach_on_device") return { kind: "local", label: "On this computer" };
  if (reach === "reach_local_remote") return { kind: "remote", label: "On another computer" };
  if (reach === "reach_cloud") return { kind: "cloud", label: vendor() };
  return reply.on_device
    ? { kind: "local", label: "On this computer" }
    : { kind: "cloud", label: vendor() };
}

/** The expanded trace: one step per line, counts and codes only. `model` is
 *  the saved model when the question was sent (the run log never holds a
 *  model name). */
export function traceDetails(reply: AskReply, runsOn: RunsOnView, model: string | null): string[] {
  const t = reply.trace;
  const lines: string[] = [];
  if (t?.scope === "meeting" || t?.scope === "note") lines.push("Searched this meeting");
  else if (t?.scope) lines.push(`Searched ${plural(t.meetings_in_scope, "meeting")}`);
  const searches = reply.run_log?.prefetch?.searches ?? 0;
  if (searches > 0) {
    const how = reply.run_log?.prefetch?.semantic ? "by keyword and meaning" : "by keyword";
    lines.push(`Ran ${plural(searches, "search", "searches")} ${how}`);
  }
  if (t && (t.passages > 0 || t.items > 0)) {
    lines.push(
      t.items > 0
        ? `Read ${plural(t.passages, "passage")} and ${plural(t.items, "extracted item")}`
        : `Read ${plural(t.passages, "passage")}`,
    );
  }
  const facts = reply.run_log?.prefetch?.memory_facts ?? 0;
  if (facts > 0) lines.push(`Used ${plural(facts, "remembered fact")}`);
  for (const c of t?.tool_calls ?? [])
    lines.push(`${toolName(c.tool)} · ${c.ok ? "ok" : "failed"}`);
  if (t?.floor_not_found) lines.push("Nothing in the meetings matched the question");
  if (t?.retried) lines.push("Asked the model again");
  if (t?.budget_forced) lines.push("Stopped looking to stay within the answer's budget");
  if (t?.context_truncated) lines.push("The model may not have read every excerpt");
  if (model) lines.push(`Model: ${model}`);
  const tier = tierDetail(t?.tier);
  if (tier) lines.push(tier);
  lines.push(
    runsOn.kind === "local"
      ? "Ran on this computer"
      : runsOn.kind === "remote"
        ? "Ran on another computer"
        : `Ran on ${runsOn.label} (cloud)`,
  );
  if (reply.timing && reply.timing.total_ms > 0) {
    const s = (ms: number) => `${(ms / 1000).toFixed(1)} s`;
    const first = reply.timing.first_token_ms;
    lines.push(
      first != null
        ? `Answered in ${s(reply.timing.total_ms)} · first words after ${s(first)}`
        : `Answered in ${s(reply.timing.total_ms)}`,
    );
  }
  return lines;
}

/** Shown (with no count) when an off-device model searched a wide scope
 *  and left Keep-local meetings out. */
export const KEPT_LOCAL_NOT_SEARCHED =
  "Kept-local meetings weren't searched. They never leave this computer.";

/** A "not found" answer: Ask found nothing, so it didn't guess. `widen`:
 *  offer to search every meeting. */
export function notFoundCopy(scope: AskScope): { title: string; body: string; widen: boolean } {
  const wide = isWideScope(scope);
  return {
    title: wide ? "Not found in your meetings" : "Not found in this meeting",
    body: wide
      ? "Ask didn't find anything about this, so it didn't guess. Try other words."
      : "Ask didn't find anything about this here, so it didn't guess. Try other words, or search all meetings.",
    widen: !wide,
  };
}

/** Over an "unverified" answer: no line could be tied to a source. */
export const UNVERIFIED_COPY =
  "Ask couldn't tie this answer to your meetings, so every line is marked. Check it before you use it.";

/** Why one sentence is marked. */
export function flagCopy(flag: AskFlag): string {
  const d = flag.detail?.trim();
  const isnt = (what: string) =>
    d
      ? `The ${what} “${d}” isn't in the lines it cites.`
      : `A ${what} here isn't in the lines it cites.`;
  switch (flag.kind) {
    case "number":
      return isnt("number");
    case "date":
      return isnt("date");
    case "name":
      return isnt("name");
    case "quote":
      return "This quote isn't in the lines it cites.";
    case "invalid_citation":
      return "It pointed to a line Ask didn't read.";
    default:
      return "This states a fact without a source.";
  }
}

const REFUSAL_REASONS: Record<AskRefusal["reason"], string | null> = {
  bad_arguments: "it was incomplete",
  no_sources: "it had no source line",
  source_not_shown: "it pointed to a line Ask didn't read",
  mixed_meetings: "it mixed lines from different meetings",
  outside_scope: "it pointed outside what you asked about",
  unsupported_claim: "the lines it cites don't back it up",
  unknown_owner: "its owner isn't someone in that meeting",
  too_long: "it was too long",
  duplicate: null,
  blocked: "that kind of change is turned off",
  injected_instruction: "it came from something said in a meeting, not from you",
  sealed: "it came after a connector call, and Ask stops suggesting changes then",
  // You didn't ask for a change, so nothing was offered: nothing to say.
  not_requested: null,
};

/** A proposal the harness wouldn't offer, said plainly; `null` = say
 *  nothing (a duplicate of one already shown). */
export function refusalCopy(r: AskRefusal): string | null {
  if (r.tool === null) return "Ask tried a step that doesn't exist, so it was skipped.";
  // (a duplicate maps to null, so no `??` here)
  const reason = r.reason in REFUSAL_REASONS ? REFUSAL_REASONS[r.reason] : "it didn't pass a check";
  if (reason === null) return null;
  return r.tool === "insert_into_note"
    ? `Ask suggested adding to a note but didn't offer it: ${reason}.`
    : `Ask suggested an action item but didn't offer it: ${reason}.`;
}

/** A proposal that can't be approved any more. */
export const proposalExpired = (expiresAt: string, now = Date.now()) => {
  const t = Date.parse(expiresAt);
  return Number.isFinite(t) && t <= now;
};

/** The backend's "Ask was cancelled." (after Stop). */
export const isCancelled = (error: unknown) => /ask was cancell?ed/i.test(String(error ?? ""));
