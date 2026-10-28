// Pure logic behind the Settings overlay: section search, scroll-spy, the
// transcript-settings draft (saved all-or-nothing by set_asr_settings) and
// the "Where your data goes" tiles. Kept free of React so it is unit-tested.

import type { AsrSettings, AsrSettingsUpdate, LlmSettings, ModelStatus } from "./types";

export type SectionId =
  "recording" | "transcripts" | "ai" | "calendars" | "files" | "apps" | "general";

export interface SettingsSection {
  id: SectionId;
  title: string;
  /** Extra words search matches on (row titles, synonyms, product names). */
  keywords: string;
}

/** The seven sections, in nav order (spec §3.8 / Appendix A). */
export const SETTINGS_SECTIONS: SettingsSection[] = [
  {
    id: "recording",
    title: "Recording",
    keywords:
      "microphone mic input audio device transcribe when i stop automatically auto remind me tell people participants recording notice consent",
  },
  {
    id: "transcripts",
    title: "Transcripts",
    keywords:
      "quality fast balanced best made on groq cloud key model whisper maximum gpu graphics card vulkan speed test engine models download disk hardware computer ram cpu cores vram api key keychain fallback",
  },
  {
    id: "ai",
    title: "Ask and Enhance",
    keywords:
      "ai assistant runs on provider ollama local openai anthropic claude sonnet haiku nvidia nim model api key test endpoint base url keychain note templates prompts cleanup action items insert",
  },
  {
    id: "calendars",
    title: "Calendars",
    keywords:
      "google outlook microsoft 365 connect disconnect sign in up next oauth client id secret keychain",
  },
  {
    id: "files",
    title: "Your files and privacy",
    keywords:
      "folder data location storage search index backfill disk encryption bitlocker filevault delete audio after retention delete all my data wipe erase privacy keep local",
  },
  {
    id: "apps",
    title: "Other AI apps",
    keywords: "mcp claude desktop claude code cursor vs code connect integrations config snippet",
  },
  {
    id: "general",
    title: "General",
    keywords:
      "appearance theme dark light system updates version check download restart diagnostics logs troubleshooting",
  },
];

/** Sections whose title or keywords contain every word of the query. */
export function matchSections(
  query: string,
  sections: SettingsSection[] = SETTINGS_SECTIONS,
): SettingsSection[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return sections;
  return sections.filter((s) => {
    const hay = `${s.title} ${s.keywords}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

/** Scroll-spy: the last section whose top has scrolled to within
 *  `threshold` px of the viewport top; the first section before that.
 *  At the very bottom the last section wins, since a short final section
 *  can never reach the top. */
export function activeSectionAt<T extends string>(
  tops: { id: T; top: number }[],
  scrollTop: number,
  threshold = 80,
  atBottom = false,
): T | null {
  if (atBottom && tops.length > 0) return tops[tops.length - 1].id;
  let current: T | null = null;
  for (const s of tops) if (s.top - scrollTop <= threshold) current = s.id;
  return current ?? tops[0]?.id ?? null;
}

/* ------------------------------------------------------------- transcripts */

export type Quality = "light" | "balanced" | "best";
/** "groq" = the Cloud tier; "hybrid" = Groq first, this computer fills in. */
export type MadeOn = "local" | "groq" | "hybrid";

export interface AsrDraft {
  quality: Quality;
  madeOn: MadeOn;
  modelId: string;
  maxQuality: boolean;
  useGpu: boolean;
}

const QUALITIES: Quality[] = ["light", "balanced", "best"];
const isQuality = (t: string): t is Quality => (QUALITIES as string[]).includes(t);

/** The editable transcript settings, derived from what's saved. */
export function asrDraftFrom(s: AsrSettings): AsrDraft {
  const quality: Quality = isQuality(s.tier)
    ? s.tier
    : isQuality(s.hw.recommended_tier)
      ? s.hw.recommended_tier
      : "balanced";
  const madeOn: MadeOn = s.tier === "cloud" ? "groq" : s.use_groq ? "hybrid" : "local";
  return {
    quality,
    madeOn,
    modelId: s.model_id ?? "",
    maxQuality: s.max_quality,
    useGpu: s.use_gpu,
  };
}

/** The set_asr_settings payload for a draft. `groqKey` is sent only when
 *  typed: an empty string would delete the saved key. */
export function asrUpdateFrom(
  draft: AsrDraft,
  saved: AsrSettings,
  autoTranscribe: boolean,
  groqKey: string,
): AsrSettingsUpdate {
  return {
    tier: draft.madeOn === "groq" ? "cloud" : draft.quality,
    model_id: draft.modelId || null,
    // Keep a saved Cloud tier's own use_groq flag untouched.
    use_groq:
      draft.madeOn === "hybrid" ||
      (draft.madeOn === "groq" && saved.tier === "cloud" && saved.use_groq),
    max_quality: draft.maxQuality,
    auto_transcribe: autoTranscribe,
    use_gpu: draft.useGpu,
    groq_key: groqKey ? groqKey : null,
  };
}

/** True when the draft (or a typed Groq key) differs from what's saved. */
export function isAsrDirty(draft: AsrDraft, saved: AsrSettings | null, groqKey: string): boolean {
  if (!saved) return false;
  if (groqKey) return true;
  const base = asrDraftFrom(saved);
  return (
    draft.madeOn !== base.madeOn ||
    (draft.madeOn !== "groq" && draft.quality !== base.quality) ||
    draft.modelId !== base.modelId ||
    draft.maxQuality !== base.maxQuality ||
    draft.useGpu !== base.useGpu
  );
}

/** Mirrors hw::default_model_for_tier (src-tauri/src/hw.rs): the whisper
 *  model a tier uses when no specific model is chosen. */
export function defaultModelForTier(tier: string, maxQuality: boolean): string {
  if (maxQuality) return "ggml-large-v3-q5_0";
  return tier === "light" ? "ggml-small-q5_1" : "ggml-large-v3-turbo-q5_0";
}

/** The model the local engine will load for this draft. */
export function effectiveModel(
  draft: AsrDraft,
  models: ModelStatus[],
): { id: string; model: ModelStatus | null } {
  const tier = draft.madeOn === "groq" ? "cloud" : draft.quality;
  const id = draft.modelId || defaultModelForTier(tier, draft.maxQuality);
  return { id, model: models.find((m) => m.id === id) ?? null };
}

/** One honest helper line per quality (no WER claims, spec §3.8.5). */
export function qualityHelper(q: Quality, recommended: string): string {
  const base: Record<Quality, string> = {
    light: "Ready soonest. Best for clear audio.",
    balanced: "Accurate on most meetings.",
    best: "Same model as Balanced. Maximum quality (More options) uses the largest one.",
  };
  return q === recommended ? `Recommended for this computer. ${base[q]}` : base[q];
}

export function madeOnHelper(m: MadeOn): string {
  switch (m) {
    case "local":
      return "Audio never leaves this computer.";
    case "groq":
      return "Audio is sent to Groq; this computer only steps in if Groq is busy or fails. Notes you keep local stay here.";
    case "hybrid":
      return "Groq while its free quota lasts; this computer fills in at your Quality setting. Notes you keep local stay here.";
  }
}

/* ------------------------------------------------------------- providers */

const PROVIDER_LABELS: Record<string, string> = {
  ollama: "This computer (Ollama)",
  anthropic: "Anthropic (Claude)",
  openai: "OpenAI",
  nim: "NVIDIA NIM",
};

/** Who processes the text for each cloud provider (disclosure copy). */
const PROVIDER_VENDORS: Record<string, string> = {
  anthropic: "Anthropic",
  openai: "OpenAI",
  nim: "NVIDIA",
};

export const providerLabel = (id: string) => PROVIDER_LABELS[id] ?? id;
export const providerVendor = (id: string) => PROVIDER_VENDORS[id] ?? id;

/* ------------------------------------------------------------- data tiles */

export interface DataTile {
  section: SectionId;
  label: string;
  where: string;
  cloud: boolean;
  /** Small print under the location. */
  note: string | null;
}

/** "Where your data goes": from the SAVED settings, never an unsaved draft. */
export function dataTiles(asr: AsrSettings | null, llm: LlmSettings | null): DataTile[] {
  const groq = !!asr && (asr.tier === "cloud" || asr.use_groq);
  const provider = llm?.providers.find((p) => p.id === llm.provider) ?? null;
  const aiCloud = !!provider && !provider.is_local;
  return [
    {
      section: "recording",
      label: "Recordings",
      where: "This computer",
      cloud: false,
      note: null,
    },
    {
      section: "transcripts",
      label: "Transcripts",
      where: groq ? "Groq cloud" : "This computer",
      cloud: groq,
      note: groq ? "Except notes you keep local." : null,
    },
    {
      section: "ai",
      label: "Ask and Enhance",
      where: aiCloud ? `${providerVendor(provider!.id)} cloud` : "This computer",
      cloud: aiCloud,
      note: aiCloud
        ? "Also transcript cleanup and action items, except notes you keep local."
        : "Also transcript cleanup and action items.",
    },
  ];
}

/* ------------------------------------------------------------- other AI apps */

/** The MCP server command from the mcp_config snippet, for the per-app
 *  instructions. Null when the snippet can't be read. */
export function mcpCommand(snippet: string): { command: string; args: string[] } | null {
  try {
    const parsed = JSON.parse(snippet) as {
      mcpServers?: Record<string, { command?: unknown; args?: unknown }>;
    };
    const first = Object.values(parsed.mcpServers ?? {})[0];
    if (!first || typeof first.command !== "string") return null;
    const args = Array.isArray(first.args) ? first.args.map(String) : [];
    return { command: first.command, args };
  } catch {
    return null;
  }
}

/** `claude mcp add` one-liner for Claude Code. */
export function claudeCodeCommand(cmd: { command: string; args: string[] }): string {
  const q = (s: string) => (/[\s"]/.test(s) ? `"${s.replace(/"/g, '\\"')}"` : s);
  return ["claude mcp add flyonthewall --", q(cmd.command), ...cmd.args.map(q)].join(" ");
}
