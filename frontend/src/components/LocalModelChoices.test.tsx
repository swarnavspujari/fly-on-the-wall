// @vitest-environment jsdom
// The one-time offer and the model picker, rendered on their own.
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { LocalModelAdvice, LocalModelRow } from "../types";
import { LocalModelChoices, LocalModelOffer } from "./LocalModelChoices";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const row = (over: Partial<LocalModelRow> = {}): LocalModelRow => ({
  model: "qwen3.5:9b",
  download_bytes: 6_600_000_000,
  memory_gb: 6.2,
  ram_class_gb: 16,
  fits: true,
  recommended: true,
  needs_newer_ollama: null,
  measured: { sourced_pct: 83, declines_pct: 94, seconds: 30 },
  ...over,
});

const advice = (over: Partial<LocalModelAdvice> = {}): LocalModelAdvice => ({
  ram_gb: 31.7,
  ollama_version: "0.40.13",
  recommended: "qwen3.5:9b",
  held_back: null,
  default_model: "llama3.1",
  explicit_model: null,
  offer: { model: "qwen3.5:9b", download_bytes: 6_600_000_000 },
  models: [
    row(),
    row({
      model: "llama3.1",
      download_bytes: 4_920_000_000,
      memory_gb: 5.8,
      ram_class_gb: 12,
      recommended: false,
      measured: { sourced_pct: 62, declines_pct: 75, seconds: 25 },
    }),
    row({
      model: "qwen3.5:4b",
      download_bytes: 3_330_000_000,
      memory_gb: 3.6,
      ram_class_gb: 8,
      recommended: false,
      measured: { sourced_pct: 65, declines_pct: 56, seconds: 20 },
    }),
  ],
  measured_on:
    "Measured on one test laptop (4 GB graphics memory, 32 GB RAM) while other programs were running. Your computer will differ.",
  doc_url: "https://example.com/docs/ASK-MODEL-BENCH.md",
  ...over,
});

let root: Root | null = null;
let host: HTMLElement | null = null;

function render(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(node));
  return host;
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = host = null;
});

const button = (el: HTMLElement, text: string) =>
  [...el.querySelectorAll("button")].find((b) => b.textContent?.startsWith(text)) ?? null;

describe("LocalModelOffer", () => {
  it("names the model, its size and what was measured, honestly", () => {
    const el = render(
      <LocalModelOffer
        advice={advice()}
        installed={[]}
        busy={false}
        onAccept={() => {}}
        onDismiss={() => {}}
      />,
    );
    const text = el.textContent!;
    expect(text).toContain("A better local model for this computer: qwen3.5:9b · 6.6 GB");
    expect(text).toContain("Answers backed by a source: 83%");
    expect(text).toContain("Declines what it can’t answer: 94%");
    expect(text).toContain("About 30 seconds per answer");
    // One laptop, other work running: said next to the numbers.
    expect(text).toContain("Measured on one test laptop");
    expect(text).toContain("other programs were running");
    expect(text).toContain("How this was measured");
    // And nothing changes until the click.
    expect(text).toContain("You keep using your current model until you switch.");
    for (const jargon of ["grounded", "tier", "refusal"]) expect(text).not.toContain(jargon);
  });

  it("downloads only on the click, and 'Not now' is its own answer", () => {
    const onAccept = vi.fn();
    const onDismiss = vi.fn();
    const el = render(
      <LocalModelOffer
        advice={advice()}
        installed={[]}
        busy={false}
        onAccept={onAccept}
        onDismiss={onDismiss}
      />,
    );
    expect(onAccept).not.toHaveBeenCalled();
    expect(onDismiss).not.toHaveBeenCalled();
    act(() => button(el, "Download and switch (6.6 GB)")!.click());
    expect(onAccept).toHaveBeenCalledExactlyOnceWith("qwen3.5:9b");
    expect(onDismiss).not.toHaveBeenCalled();
    act(() => button(el, "Not now")!.click());
    expect(onDismiss).toHaveBeenCalledExactlyOnceWith("qwen3.5:9b");
  });

  it("a model already on disk is a switch, not a download", () => {
    const el = render(
      <LocalModelOffer
        advice={advice()}
        installed={[{ name: "qwen3.5:9b", size: 6_600_000_000 }]}
        busy={false}
        onAccept={() => {}}
        onDismiss={() => {}}
      />,
    );
    expect(button(el, "Switch")).not.toBeNull();
    expect(el.textContent).not.toContain("Download and switch");
  });

  it("is disabled while something else is running", () => {
    const el = render(
      <LocalModelOffer
        advice={advice()}
        installed={[]}
        busy
        onAccept={() => {}}
        onDismiss={() => {}}
      />,
    );
    expect(button(el, "Working…")!.disabled).toBe(true);
    expect(button(el, "Not now")!.disabled).toBe(true);
  });

  it("renders nothing without an offer", () => {
    const el = render(
      <LocalModelOffer
        advice={advice({ offer: null })}
        installed={[]}
        busy={false}
        onAccept={() => {}}
        onDismiss={() => {}}
      />,
    );
    expect(el.textContent).toBe("");
  });
});

describe("LocalModelChoices", () => {
  const choices = (over: Partial<Parameters<typeof LocalModelChoices>[0]> = {}) =>
    render(
      <LocalModelChoices
        advice={advice()}
        installed={[
          { name: "llama3.1:latest", size: 4_920_000_000 },
          { name: "qwen3.5:4b", size: 3_330_000_000 },
        ]}
        effective="llama3.1"
        busy={false}
        onUse={() => {}}
        {...over}
      />,
    );
  const rowEl = (el: HTMLElement, model: string) =>
    el.querySelector<HTMLElement>(`[data-model="${model}"]`)!;

  it("shows what was measured next to each model where the user picks", () => {
    const el = choices();
    const nine = rowEl(el, "qwen3.5:9b").textContent!;
    expect(nine).toContain("6.6 GB download");
    expect(nine).toContain("Answers backed by a source: 83%");
    expect(nine).toContain("Declines what it can’t answer: 94%");
    expect(nine).toContain("About 30 seconds per answer");
    expect(nine).toContain("recommended for this computer");
    expect(rowEl(el, "llama3.1").textContent).toContain("Answers backed by a source: 62%");
    expect(rowEl(el, "llama3.1").textContent).not.toContain("recommended for this computer");
    expect(rowEl(el, "qwen3.5:4b").textContent).toContain("Declines what it can’t answer: 56%");
    expect(el.textContent).toContain("Measured on one test laptop");
  });

  it("marks the model in use and offers the others", () => {
    const onUse = vi.fn();
    const el = choices({ onUse });
    const llama = rowEl(el, "llama3.1");
    expect(llama.textContent).toContain("in use");
    expect(button(llama, "Use")).toBeNull();
    // Already downloaded: "Use". Not here: "Download and use" with its size.
    act(() => button(rowEl(el, "qwen3.5:4b"), "Use")!.click());
    expect(onUse).toHaveBeenLastCalledWith("qwen3.5:4b");
    act(() => button(rowEl(el, "qwen3.5:9b"), "Download and use (6.6 GB)")!.click());
    expect(onUse).toHaveBeenLastCalledWith("qwen3.5:9b");
    expect(onUse).toHaveBeenCalledTimes(2);
  });

  it("says when the computer has less memory than a model suits", () => {
    const el = choices({
      advice: advice({
        models: [
          row({ fits: false }),
          row({ model: "llama3.1", ram_class_gb: 12, recommended: false }),
        ],
      }),
    });
    expect(rowEl(el, "qwen3.5:9b").textContent).toContain(
      "This computer has less memory than that.",
    );
    expect(rowEl(el, "llama3.1").textContent).not.toContain("less memory");
  });

  it("an Ollama that is too old rules a model out and says why", () => {
    const el = choices({
      advice: advice({
        ollama_version: "0.16.9",
        held_back: { model: "qwen3.5:9b", needs_ollama: "0.17.5", have_ollama: "0.16.9" },
        models: [
          row({ needs_newer_ollama: "0.17.5" }),
          row({ model: "llama3.1", recommended: false }),
        ],
      }),
      effective: "llama3.1",
    });
    const nine = rowEl(el, "qwen3.5:9b");
    expect(nine.textContent).toContain("Needs Ollama 0.17.5 or newer (this computer has 0.16.9).");
    expect(button(nine, "Download and use")).toBeNull();
    expect(el.textContent).toContain("Update Ollama to use it.");
  });
});
