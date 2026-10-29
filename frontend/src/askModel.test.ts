import { describe, expect, it } from "vitest";
import {
  CLAUDE_MODELS,
  QUICK_PROMPTS,
  RUNS_ON_HELPER,
  describeAskError,
  providerName,
  runsOnPlace,
  runsOnSwitched,
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
      "OpenAI isn't allowed to receive your meeting content yet, so nothing was sent. To allow it, open Settings › Ask and Enhance and click Test. To keep everything on this computer, switch Runs on to This computer (Ollama).",
      "openai",
      "OpenAI isn't confirmed yet",
    ],
    [
      "No API key for NVIDIA yet. Add one in Settings › Ask and Enhance.",
      "nim",
      "No API key for NVIDIA",
    ],
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

  it("offers a consent button for 'isn't confirmed', and only there", () => {
    const consent = describeAskError(
      "OpenAI isn't allowed to receive your meeting content yet, so nothing was sent. To allow it, open Settings › Ask and Enhance and click Test. To keep everything on this computer, switch Runs on to This computer (Ollama).",
      "openai",
    );
    expect(consent.action).toBe("consent");
    expect(consent.fix).toBe("Ask needs your OK before it sends this note to OpenAI.");
    expect(describeAskError("anthropic timed out.", "anthropic").action).toBeUndefined();
  });

  it("only calls a missing model 'not downloaded' for the local AI", () => {
    const cloud404 =
      'provider returned an error: 404 Not Found: {"error":{"message":"The model `gpt-9` does not exist","code":"model_not_found"}}';
    expect(describeAskError(cloud404, "openai").title).not.toContain("downloaded");
  });

  it("puts 5xx before connection wording, and doesn't guess auth from a bare 401", () => {
    expect(
      describeAskError(
        "provider returned an error: 502 Bad Gateway: upstream connection reset",
        "openai",
      ).title,
    ).toBe("OpenAI had a problem answering");
    expect(
      describeAskError("provider returned an error: tokens used 401 of 4096", "openai").title,
    ).not.toContain("API key");
  });
});

describe("Runs on helpers", () => {
  // The loopback rule itself is Home's (homeModel.test.ts).
  it("says where a provider runs, in the privacy pill's words", () => {
    expect(runsOnPlace({ id: "ollama", is_local: true, base_url: null })).toEqual({
      kind: "local",
      label: "This computer",
    });
    expect(
      runsOnPlace({ id: "ollama", is_local: true, base_url: "http://localhost:11434" }).kind,
    ).toBe("local");
    expect(
      runsOnPlace({ id: "ollama", is_local: true, base_url: "http://192.168.1.20:11434" }),
    ).toEqual({ kind: "remote", label: "Off this computer · 192.168.1.20:11434" });
    expect(
      runsOnPlace({ id: "ollama", is_local: true, base_url: "http://127.example.com:11434" }).kind,
    ).toBe("remote");
    expect(runsOnPlace({ id: "anthropic", is_local: false, base_url: null })).toEqual({
      kind: "cloud",
      label: "Anthropic",
    });
  });

  it("discloses that the provider isn't just Ask's", () => {
    expect(RUNS_ON_HELPER).toBe("Also used for Enhance, transcript cleanup and action items.");
    expect(runsOnSwitched("OpenAI")).toBe(
      "Ask, Enhance, transcript cleanup and action items now run on OpenAI.",
    );
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

  it("offers the Claude models Settings offers, recommended first", () => {
    expect(CLAUDE_MODELS.map((m) => m.value)).toEqual(["claude-sonnet-5-5", "claude-haiku-5-5"]);
  });
});
