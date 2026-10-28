import { describe, expect, it } from "vitest";
import {
  SETTINGS_SECTIONS,
  activeSectionAt,
  asrDraftFrom,
  asrUpdateFrom,
  claudeCodeCommand,
  dataTiles,
  effectiveModel,
  isAsrDirty,
  matchSections,
  mcpCommand,
  qualityHelper,
} from "./settingsModel";
import type { AsrSettings, LlmSettings } from "./types";

const asr = (p: Partial<AsrSettings> = {}): AsrSettings => ({
  tier: "balanced",
  model_id: null,
  use_groq: false,
  max_quality: false,
  has_groq_key: false,
  auto_transcribe: true,
  use_gpu: true,
  gpu_available: true,
  gpu_bench: null,
  hw: { ram_gb: 16, cpu_cores: 8, gpu_name: null, vram_mb: null, recommended_tier: "balanced" },
  models: [
    { id: "ggml-small-q5_1", display: "small", bytes: 1, installed: true },
    { id: "ggml-large-v3-turbo-q5_0", display: "large-v3-turbo", bytes: 2, installed: true },
    { id: "ggml-large-v3-q5_0", display: "large-v3", bytes: 3, installed: false },
  ],
  engine_installed: true,
  engine_managed: true,
  ...p,
});

const llm = (provider: string): LlmSettings => ({
  provider,
  providers: [
    {
      id: "ollama",
      default_model: "llama3.1",
      is_local: true,
      has_key: false,
      model: null,
      base_url: null,
    },
    {
      id: "anthropic",
      default_model: "claude-sonnet-5-5",
      is_local: false,
      has_key: true,
      model: null,
      base_url: null,
    },
  ],
});

describe("section search", () => {
  it("lists all seven sections in order for an empty query", () => {
    expect(matchSections("  ").map((s) => s.id)).toEqual([
      "recording",
      "transcripts",
      "ai",
      "calendars",
      "files",
      "apps",
      "general",
    ]);
    expect(SETTINGS_SECTIONS).toHaveLength(7);
  });

  it("matches titles and keywords, case-insensitively", () => {
    expect(matchSections("mcp").map((s) => s.id)).toEqual(["apps"]);
    expect(matchSections("GROQ").map((s) => s.id)).toEqual(["transcripts"]);
    expect(matchSections("dark").map((s) => s.id)).toEqual(["general"]);
    expect(matchSections("templates").map((s) => s.id)).toEqual(["ai"]);
    expect(matchSections("retention").map((s) => s.id)).toEqual(["files"]);
  });

  it("needs every word to match", () => {
    expect(matchSections("google client").map((s) => s.id)).toEqual(["calendars"]);
    expect(matchSections("google banana")).toEqual([]);
  });
});

describe("scroll-spy", () => {
  const tops = [
    { id: "a", top: 0 },
    { id: "b", top: 400 },
    { id: "c", top: 900 },
  ];
  it("picks the last section near the top", () => {
    expect(activeSectionAt(tops, 0)).toBe("a");
    expect(activeSectionAt(tops, 330)).toBe("b");
    expect(activeSectionAt(tops, 2000)).toBe("c");
  });
  it("picks the last section once scrolled to the bottom", () => {
    expect(activeSectionAt(tops, 700, 80, true)).toBe("c");
    expect(activeSectionAt(tops, 700, 80, false)).toBe("b");
  });
  it("falls back to the first section", () => {
    expect(activeSectionAt([{ id: "a", top: 300 }], 0)).toBe("a");
    expect(activeSectionAt([], 0)).toBeNull();
  });
});

describe("transcript draft", () => {
  it("maps tiers onto Quality + Made on", () => {
    expect(asrDraftFrom(asr()).madeOn).toBe("local");
    expect(asrDraftFrom(asr({ use_groq: true, tier: "light" }))).toMatchObject({
      madeOn: "hybrid",
      quality: "light",
    });
    // the Cloud tier keeps a sensible local quality for when you switch back
    expect(
      asrDraftFrom(
        asr({
          tier: "cloud",
          hw: { ...asr().hw, recommended_tier: "best" },
        }),
      ),
    ).toMatchObject({ madeOn: "groq", quality: "best" });
  });

  it("round-trips every saved combination unchanged", () => {
    const combos: Partial<AsrSettings>[] = [
      { tier: "light" },
      { tier: "best", max_quality: true, model_id: "ggml-small-q5_1" },
      { tier: "balanced", use_groq: true },
      { tier: "cloud" },
      { tier: "cloud", use_groq: true },
      { tier: "balanced", use_gpu: false, auto_transcribe: false },
    ];
    for (const c of combos) {
      const saved = asr(c);
      const draft = asrDraftFrom(saved);
      expect(isAsrDirty(draft, saved, "")).toBe(false);
      const u = asrUpdateFrom(draft, saved, saved.auto_transcribe, "");
      expect(u).toEqual({
        tier: saved.tier,
        model_id: saved.model_id,
        use_groq: saved.use_groq,
        max_quality: saved.max_quality,
        auto_transcribe: saved.auto_transcribe,
        use_gpu: saved.use_gpu,
        groq_key: null,
      });
    }
  });

  it("builds the payload for each Made on choice", () => {
    const saved = asr();
    const d = asrDraftFrom(saved);
    expect(asrUpdateFrom({ ...d, madeOn: "groq" }, saved, true, "gsk_1")).toMatchObject({
      tier: "cloud",
      use_groq: false,
      groq_key: "gsk_1",
    });
    expect(
      asrUpdateFrom({ ...d, madeOn: "hybrid", quality: "light" }, saved, true, ""),
    ).toMatchObject({ tier: "light", use_groq: true, groq_key: null });
  });

  it("never sends an empty Groq key (that would delete the saved one)", () => {
    const saved = asr({ has_groq_key: true });
    expect(asrUpdateFrom(asrDraftFrom(saved), saved, true, "").groq_key).toBeNull();
  });

  it("flags edits, including a typed key", () => {
    const saved = asr();
    const d = asrDraftFrom(saved);
    expect(isAsrDirty({ ...d, quality: "best" }, saved, "")).toBe(true);
    expect(isAsrDirty({ ...d, useGpu: false }, saved, "")).toBe(true);
    expect(isAsrDirty({ ...d, modelId: "ggml-small-q5_1" }, saved, "")).toBe(true);
    expect(isAsrDirty(d, saved, "gsk_")).toBe(true);
    expect(isAsrDirty(d, null, "gsk_")).toBe(false);
  });

  it("resolves the model the engine will use", () => {
    const s = asr();
    const d = asrDraftFrom(s);
    expect(effectiveModel(d, s.models).id).toBe("ggml-large-v3-turbo-q5_0");
    expect(effectiveModel({ ...d, quality: "light" }, s.models).id).toBe("ggml-small-q5_1");
    expect(effectiveModel({ ...d, maxQuality: true }, s.models).model?.display).toBe("large-v3");
    expect(
      effectiveModel({ ...d, modelId: "ggml-small-q5_1", maxQuality: true }, s.models).id,
    ).toBe("ggml-small-q5_1");
  });

  it("only calls the recommended tier recommended", () => {
    expect(qualityHelper("balanced", "balanced")).toMatch(/^Recommended for this computer/);
    expect(qualityHelper("best", "balanced")).not.toMatch(/Recommended/);
    expect(qualityHelper("best", "balanced")).not.toMatch(/WER|Most accurate/);
  });
});

describe("Where your data goes", () => {
  it("is all local by default", () => {
    const tiles = dataTiles(asr(), llm("ollama"));
    expect(tiles.map((t) => [t.label, t.where, t.cloud])).toEqual([
      ["Recordings", "This computer", false],
      ["Transcripts", "This computer", false],
      ["Ask and Enhance", "This computer", false],
    ]);
  });

  it("names the cloud and covers cleanup, action items and keep-local", () => {
    const tiles = dataTiles(asr({ use_groq: true }), llm("anthropic"));
    expect(tiles[1]).toMatchObject({ where: "Groq cloud", cloud: true });
    expect(tiles[1].note).toMatch(/except notes you keep local/i);
    expect(tiles[2]).toMatchObject({ where: "Anthropic cloud", cloud: true });
    expect(tiles[2].note).toMatch(/transcript cleanup/);
    expect(tiles[2].note).toMatch(/action items/);
    expect(tiles[2].note).toMatch(/except notes you keep local/);
    // the Ask tile mentions cleanup + items even when local
    expect(dataTiles(asr(), llm("ollama"))[2].note).toMatch(/cleanup and action items/);
  });

  it("treats the Cloud tier as Groq", () => {
    expect(dataTiles(asr({ tier: "cloud" }), null)[1].where).toBe("Groq cloud");
  });
});

describe("MCP instructions", () => {
  const snippet = JSON.stringify({
    mcpServers: {
      flyonthewall: {
        command: "C:\\Program Files\\Fly on the Wall\\flyonthewall-mcp.exe",
        args: [],
      },
    },
  });
  it("reads the server command from the snippet", () => {
    expect(mcpCommand(snippet)).toEqual({
      command: "C:\\Program Files\\Fly on the Wall\\flyonthewall-mcp.exe",
      args: [],
    });
    expect(mcpCommand("not json")).toBeNull();
    expect(mcpCommand("{}")).toBeNull();
  });
  it("quotes paths with spaces for Claude Code", () => {
    expect(claudeCodeCommand(mcpCommand(snippet)!)).toBe(
      'claude mcp add flyonthewall -- "C:\\Program Files\\Fly on the Wall\\flyonthewall-mcp.exe"',
    );
    expect(claudeCodeCommand({ command: "/usr/bin/fly-mcp", args: ["--x"] })).toBe(
      "claude mcp add flyonthewall -- /usr/bin/fly-mcp --x",
    );
  });
});
