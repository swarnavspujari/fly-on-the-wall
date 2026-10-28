// @vitest-environment jsdom
import { act, StrictMode, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, LLM_SETTINGS_CHANGED_EVENT } from "../api";
import type { LlmSettings } from "../types";
import { CloudConsentOnLaunch, useCloudConsent } from "./CloudConsent";
import { answerConsent, consentRequest, holdConsentRequests } from "./cloudConsentTesting";

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
  // consent is never written from the webview any more
  vi.spyOn(api, "setAppSetting").mockRejectedValue("can't be changed from the app UI");
  holdConsentRequests((p) => {
    stored[`consent.cloud_llm.${p}`] = "true";
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

async function answer(yes: boolean) {
  await act(async () => answerConsent(yes));
  await flush();
}

async function launch() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  // StrictMode, as in main.tsx: effects run twice, the question shows once.
  act(() =>
    root!.render(
      <StrictMode>
        <CloudConsentOnLaunch />
      </StrictMode>,
    ),
  );
  await flush();
}

describe("cloud consent at launch", () => {
  it("asks the backend once, as the launch check, when the saved cloud provider has no consent", async () => {
    await launch();
    expect(consentRequest()).toEqual({ provider: "anthropic", atLaunch: true });
    expect(api.requestCloudConsent).toHaveBeenCalledTimes(1);

    await answer(false);
    expect(consentRequest()).toBeNull(); // asked once, not again
    expect(api.requestCloudConsent).toHaveBeenCalledTimes(1);
    expect(stored["consent.cloud_llm.anthropic"]).toBeUndefined();
    expect(api.setAppSetting).not.toHaveBeenCalled();
  });

  it("leaves recording a yes to the backend, and tells the app it changed", async () => {
    const changed = vi.fn();
    window.addEventListener(LLM_SETTINGS_CHANGED_EVENT, changed);
    await launch();
    await answer(true);
    expect(stored["consent.cloud_llm.anthropic"]).toBe("true");
    expect(api.setAppSetting).not.toHaveBeenCalled();
    expect(changed).toHaveBeenCalledTimes(1);
    window.removeEventListener(LLM_SETTINGS_CHANGED_EVENT, changed);
  });

  it("stays quiet when the provider is local or already allowed", async () => {
    saved = settings("ollama");
    await launch();
    expect(api.requestCloudConsent).not.toHaveBeenCalled();
    act(() => root!.unmount());
    root = null;

    saved = settings("anthropic");
    stored["consent.cloud_llm.anthropic"] = "true";
    await launch();
    expect(api.requestCloudConsent).not.toHaveBeenCalled();
  });

  it("stays quiet when the settings can't be read", async () => {
    vi.mocked(api.getLlmSettings).mockRejectedValue("database is locked");
    await launch();
    expect(api.requestCloudConsent).not.toHaveBeenCalled();
  });
});

describe("useCloudConsent", () => {
  let ask: ReturnType<typeof useCloudConsent>;
  function Grab() {
    const fn = useCloudConsent();
    useEffect(() => {
      ask = fn;
    }, [fn]);
    return null;
  }
  async function mount() {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    act(() => root!.render(<Grab />));
    await flush();
  }

  it("refuses a provider it doesn't know, without asking", async () => {
    await mount();
    let result: boolean | undefined;
    await act(async () => {
      result = await ask(saved.providers, "someday-cloud");
    });
    expect(result).toBe(false);
    expect(api.requestCloudConsent).not.toHaveBeenCalled();
  });

  it("asks once per provider: a second ask while it's open waits for the same answer", async () => {
    await mount();
    const results: boolean[] = [];
    await act(async () => {
      void ask(saved.providers, "anthropic").then((ok) => results.push(ok));
      void ask(saved.providers, "anthropic", { atLaunch: true }).then((ok) => results.push(ok));
    });
    await flush();
    expect(api.requestCloudConsent).toHaveBeenCalledTimes(1);
    expect(consentRequest()).toEqual({ provider: "anthropic", atLaunch: false });
    await answer(true);
    expect(results).toEqual([true, true]);
    expect(consentRequest()).toBeNull();
  });

  it("counts a failed request as no", async () => {
    vi.mocked(api.requestCloudConsent).mockRejectedValueOnce("dialog failed");
    await mount();
    let result: boolean | undefined;
    await act(async () => {
      result = await ask(saved.providers, "anthropic");
    });
    expect(result).toBe(false);
    expect(stored["consent.cloud_llm.anthropic"]).toBeUndefined();
  });
});
