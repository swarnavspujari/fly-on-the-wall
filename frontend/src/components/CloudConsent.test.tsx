// @vitest-environment jsdom
import { act, StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { LlmSettings } from "../types";
import { CloudConsentOnLaunch } from "./CloudConsent";
import { ConfirmProvider } from "./confirm";
import { confirmChoice, confirmDialog, confirmTitle } from "./confirmTesting";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const settings = (provider: string): LlmSettings => ({
  provider,
  providers: [
    {
      id: "ollama",
      default_model: "llama3.1",
      is_local: true,
      has_key: true,
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

let saved: LlmSettings;
let stored: Record<string, string>;
let root: Root | null = null;
let host: HTMLElement | null = null;

beforeEach(() => {
  saved = settings("anthropic");
  stored = {};
  vi.spyOn(api, "getLlmSettings").mockImplementation(async () => structuredClone(saved));
  vi.spyOn(api, "getAppSetting").mockImplementation(async (k) => stored[k] ?? null);
  vi.spyOn(api, "setAppSetting").mockImplementation(async (k, v) => {
    stored[k] = v;
  });
});

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.restoreAllMocks();
});

const flush = async () => {
  for (let i = 0; i < 5; i++) await act(async () => {});
};

async function launch() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  // StrictMode, as in main.tsx: effects run twice, the question shows once.
  act(() =>
    root!.render(
      <StrictMode>
        <ConfirmProvider>
          <CloudConsentOnLaunch />
        </ConfirmProvider>
      </StrictMode>,
    ),
  );
  await flush();
}

describe("cloud consent at launch", () => {
  it("asks once when the saved cloud provider has no consent; Not now saves nothing", async () => {
    await launch();
    expect(confirmTitle()).toBe("Use Anthropic for Ask and Enhance?");
    expect(document.querySelectorAll('[data-testid="confirm-dialog"]')).toHaveLength(1);
    const text = confirmDialog()!.textContent!;
    expect(text).toContain("Your AI provider is set to Anthropic");
    expect(text).toContain("full meeting transcripts will be sent to Anthropic");
    expect(api.setAppSetting).not.toHaveBeenCalled();

    expect(confirmChoice(false).textContent).toBe("Not now");
    await act(async () => confirmChoice(false).click());
    await flush();
    expect(confirmDialog()).toBeNull(); // asked once, not again
    expect(api.setAppSetting).not.toHaveBeenCalled();
    expect(stored["consent.cloud_llm.anthropic"]).toBeUndefined();
  });

  it("records consent only on an explicit yes", async () => {
    await launch();
    expect(confirmChoice(true).textContent).toBe("Use Anthropic");
    await act(async () => confirmChoice(true).click());
    await flush();
    expect(api.setAppSetting).toHaveBeenCalledTimes(1);
    expect(stored["consent.cloud_llm.anthropic"]).toBe("true");
  });

  it("stays quiet when the provider is local or already allowed", async () => {
    saved = settings("ollama");
    await launch();
    expect(confirmDialog()).toBeNull();
    act(() => root!.unmount());
    root = null;

    saved = settings("anthropic");
    stored["consent.cloud_llm.anthropic"] = "true";
    await launch();
    expect(confirmDialog()).toBeNull();
    expect(api.setAppSetting).not.toHaveBeenCalled();
  });

  it("stays quiet when the settings can't be read", async () => {
    vi.mocked(api.getLlmSettings).mockRejectedValue("database is locked");
    await launch();
    expect(confirmDialog()).toBeNull();
    expect(api.setAppSetting).not.toHaveBeenCalled();
  });
});
