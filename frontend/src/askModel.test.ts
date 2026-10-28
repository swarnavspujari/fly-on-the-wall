import { describe, expect, it } from "vitest";
import {
  CLAUDE_MODELS,
  QUICK_PROMPTS,
  cloudConsentMessage,
  describeAskError,
  providerName,
} from "./askModel";

describe("describeAskError", () => {
  // The backend's own strings (llm_commands.rs, ollama.rs, fly-llm) → title.
  const cases: [string, string, string][] = [
    [
      'this note is set to "Keep local", so it isn\'t sent to anthropic. Switch the AI provider to Ollama (runs on this device) in Settings, or turn off Keep local for this note',
      "anthropic",
      "This note is kept local",
    ],
    [
      "confirm that meeting content may be sent to openai: open Settings → AI provider and select it again",
      "openai",
      "OpenAI isn't confirmed yet",
    ],
    ["no API key stored for nim — add one in Settings", "nim", "No API key for NVIDIA"],
    [
      "authentication failed — check the API key",
      "anthropic",
      "Anthropic didn't accept the API key",
    ],
    [
      "Ollama isn't installed — use the Install button in Settings → AI provider, or install it yourself from ollama.com",
      "ollama",
      "The local AI isn't installed",
    ],
    [
      'provider returned an error: 404 Not Found: {"error":"model \\"llama3.1\\" not found, try pulling it first"}',
      "ollama",
      "The local model isn't downloaded",
    ],
    [
      "Ollama isn't reachable at http://localhost:11434. Start the Ollama app (or install it from ollama.com), or switch provider in Settings.",
      "ollama",
      "Can't reach the local AI",
    ],
    [
      "Ollama was started but isn't answering yet — try again in a few seconds",
      "ollama",
      "Can't reach the local AI",
    ],
    [
      "anthropic timed out. The model may still be loading — try again in a moment.",
      "anthropic",
      "Anthropic took too long to answer",
    ],
    [
      'provider returned an error: 429 Too Many Requests: {"type":"rate_limit_error"}',
      "anthropic",
      "Anthropic is busy right now",
    ],
    ["Can't reach openai — check your internet connection.", "openai", "Can't reach OpenAI"],
    [
      "provider returned an error: 503 Service Unavailable: upstream",
      "openai",
      "OpenAI had a problem answering",
    ],
    [
      "provider returned an error: 400 Bad Request: prompt is too long: 250000 tokens",
      "anthropic",
      "This meeting is too long for the model",
    ],
    [
      "provider returned an error: response had no message content",
      "openai",
      "The answer came back empty",
    ],
    ["invalid request: something odd", "openai", "Ask couldn't answer"],
  ];

  it.each(cases)("%s", (raw, provider, title) => {
    const v = describeAskError(raw, provider);
    expect(v.title).toBe(title);
    expect(v.fix.length).toBeGreaterThan(10);
    expect(v.raw).toBe(raw);
  });

  it("keeps the raw text for the details and never shows an empty one", () => {
    expect(describeAskError("", "ollama").raw).toBe("Unknown error");
    expect(describeAskError(new Error("boom"), "ollama").raw).toBe("Error: boom");
  });
});

describe("Ask copy", () => {
  it("keeps the four quick prompts", () => {
    expect(QUICK_PROMPTS).toEqual([
      "What did I miss?",
      "What decisions were made?",
      "List the action items",
      "Draft a follow-up email",
    ]);
  });

  it("names providers for sentences", () => {
    expect(providerName("ollama")).toBe("Ollama");
    expect(providerName("anthropic")).toBe("Anthropic");
    expect(providerName("nim")).toBe("NVIDIA");
  });

  it("discloses what a cloud provider receives, like Settings does", () => {
    const m = cloudConsentMessage("OpenAI");
    expect(m).toMatch(/^Use OpenAI for Ask and Enhance\?/);
    expect(m).toContain("full meeting transcripts will be sent to OpenAI");
    expect(m).toContain("transcript cleanup and action items");
    expect(m).toContain("notes you keep local");
  });

  it("offers the Claude models Settings offers, recommended first", () => {
    expect(CLAUDE_MODELS.map((m) => m.value)).toEqual(["claude-sonnet-5-5", "claude-haiku-5-5"]);
  });
});
