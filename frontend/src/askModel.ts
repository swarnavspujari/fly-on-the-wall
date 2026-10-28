/* Ask: pure helpers for the composer and answer sheet (AskPanel.tsx), kept
   here so the copy is unit-tested (askModel.test.ts). Ask still goes through
   the one backend path, `ask_meeting` → `build_provider`, which enforces
   cloud consent and Keep local; nothing here is a second LLM path. */

import { isLoopbackUrl } from "./homeModel";
import { providerVendor } from "./settingsModel";

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

/** The one-time disclosure before a cloud provider first gets meeting text:
 *  the same words Settings shows, so consent means the same thing in both. */
export function cloudConsentMessage(vendor: string): string {
  return (
    `Use ${vendor} for Ask and Enhance?\n\n` +
    `Your notes and full meeting transcripts will be sent to ${vendor} for processing ` +
    `whenever you run Enhance or Ask, and automatically for transcript cleanup and ` +
    `action items after each transcription. Their data-retention terms apply (see their ` +
    `privacy / data-usage policy).\n\n` +
    `Recordings themselves stay on this computer, and so do notes you keep local. ` +
    `Choose This computer (Ollama) to keep everything local.`
  );
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
  if (t.includes("confirm that meeting content may be sent")) {
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
