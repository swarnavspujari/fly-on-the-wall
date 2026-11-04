import { describe, expect, it } from "vitest";
import {
  blockedNote,
  choiceAction,
  choiceState,
  friendlyPullError,
  heldBackNote,
  measuredLine,
  memoryLine,
  offerAction,
  offerTitle,
  rowFor,
  sameModel,
  showOffer,
  sizeLabel,
  slowerLine,
} from "./localModels";
import type { LocalModelAdvice, LocalModelRow } from "./types";

const row = (over: Partial<LocalModelRow> = {}): LocalModelRow => ({
  model: "qwen3.5:9b",
  download_bytes: 6_600_000_000,
  memory_gb: 6.2,
  ram_class_gb: 16,
  fits: true,
  recommended: true,
  needs_newer_ollama: null,
  measured: { sourced_pct: 83, declines_pct: 94, seconds: 30, background_slower: [15, 25] },
  ...over,
});

const advice = (over: Partial<LocalModelAdvice> = {}): LocalModelAdvice => ({
  ram_gb: 31.7,
  ollama_version: "0.40.13",
  recommended: "qwen3.5:9b",
  held_back: null,
  default_model: "llama3.1",
  default_blocked: null,
  explicit_model: null,
  offer: { model: "qwen3.5:9b", download_bytes: 6_600_000_000 },
  models: [row()],
  measured_on:
    "Measured on one test laptop (4 GB graphics memory, 32 GB RAM) while other programs were running. Your computer will differ.",
  doc_url: "https://example.com/docs/ASK-MODEL-BENCH.md",
  ...over,
});

describe("local model copy", () => {
  it("says the measured numbers in plain words, numbers only", () => {
    const line = measuredLine(row().measured);
    expect(line).toBe(
      "Answers backed by a source: 83%. Declines what it can’t answer: 94%. About 30 seconds per answer.",
    );
    // No jargon from the evals, and no fixture or gold text.
    for (const word of ["grounded", "tier", "refusal", "claim", "floor"]) {
      expect(line).not.toContain(word);
    }
  });

  it("sizes and memory are readable", () => {
    expect(sizeLabel(6_600_000_000)).toBe("6.6 GB");
    expect(sizeLabel(4_920_000_000)).toBe("4.9 GB");
    expect(sizeLabel(620_000_000)).toBe("620 MB");
    expect(memoryLine(row())).toBe(
      "Uses about 6 GB of memory while answering. Suits computers with 16 GB or more.",
    );
    expect(memoryLine(row({ memory_gb: 3.6, ram_class_gb: 8 }))).toContain("about 4 GB");
  });

  it("the offer names the model and what the download costs", () => {
    expect(offerTitle("qwen3.5:9b", 6_600_000_000)).toBe(
      "A better local model for this computer: qwen3.5:9b · 6.6 GB",
    );
  });

  it("an old Ollama is named, with its version when known", () => {
    expect(
      heldBackNote({ model: "qwen3.5:9b", needs_ollama: "0.17.5", have_ollama: "0.16.9" }),
    ).toBe(
      "qwen3.5:9b needs Ollama 0.17.5 or newer. This computer has 0.16.9. Update Ollama to use it.",
    );
    expect(heldBackNote({ model: "qwen3.5:9b", needs_ollama: "0.17.5", have_ollama: "" })).toBe(
      "qwen3.5:9b needs Ollama 0.17.5 or newer. Update Ollama to use it.",
    );
  });
});

describe("what the model costs after a meeting", () => {
  it("says how much longer notes and cleanup take, in times as long", () => {
    expect(slowerLine(row().measured)).toBe(
      "Notes and cleanup after a meeting take about 1.5–2.5 times as long as with llama3.1.",
    );
    // Whole numbers have no trailing ".0".
    expect(slowerLine({ ...row().measured, background_slower: [20, 30] })).toBe(
      "Notes and cleanup after a meeting take about 2–3 times as long as with llama3.1.",
    );
  });

  it("says nothing for a model that is no slower", () => {
    expect(slowerLine({ ...row().measured, background_slower: null })).toBeNull();
  });
});

describe("a download that cannot work", () => {
  const old = advice({
    ollama_version: "0.16.9",
    models: [row({ needs_newer_ollama: "0.17.5" })],
  });
  const ollamasOwn =
    "pull model manifest: 412: The model you are attempting to pull requires a newer version of Ollama.";

  it("Ollama's refusal for a version it doesn't have becomes 'update Ollama'", () => {
    expect(friendlyPullError(ollamasOwn, "qwen3.5:9b", old)).toBe(
      "qwen3.5:9b needs Ollama 0.17.5 or newer. This computer has 0.16.9. Update Ollama (ollama.com), then try again.",
    );
    // Without the table row or the version it still says what to do.
    expect(friendlyPullError(ollamasOwn, "qwen3.5:9b", null)).toBe(
      "qwen3.5:9b needs a newer Ollama. Update Ollama (ollama.com), then try again.",
    );
    expect(friendlyPullError(new Error(ollamasOwn), "qwen3.5:9b", old)).toContain("Update Ollama");
  });

  it("anything else is shown as it came, including the app's own disk message", () => {
    const disk =
      "Not enough free disk space for qwen3.5:9b. It needs about 8.6 GB free (a 6.6 GB download plus 2.0 GB to spare) and this drive has 3.1 GB.";
    expect(friendlyPullError(disk, "qwen3.5:9b", old)).toBe(disk);
    expect(friendlyPullError("the download stopped", "qwen3.5:9b", null)).toBe(
      "the download stopped",
    );
  });

  it("the pinned default that needs a newer Ollama gets a note, no other model does", () => {
    const blocked = advice({
      default_model: "qwen3.5:9b",
      default_blocked: { model: "qwen3.5:9b", needs_ollama: "0.17.5", have_ollama: "0.16.9" },
    });
    expect(blockedNote(blocked, "qwen3.5:9b")).toBe(
      "qwen3.5:9b needs Ollama 0.17.5 or newer. This computer has 0.16.9. Update Ollama to use it.",
    );
    // The model in use is something else (an explicit pick): nothing to warn about.
    expect(blockedNote(blocked, "llama3.1")).toBeNull();
    expect(blockedNote(advice(), "qwen3.5:9b")).toBeNull();
    expect(blockedNote(null, "qwen3.5:9b")).toBeNull();
  });
});

describe("local model state", () => {
  it("matches names the way the backend does", () => {
    expect(sameModel("llama3.1", "llama3.1:latest")).toBe(true);
    expect(sameModel("qwen3.5:9b", "qwen3.5:9b")).toBe(true);
    expect(sameModel("qwen3.5:9b", "qwen3.5:4b")).toBe(false);
    expect(sameModel("qwen3.5", "qwen3.5:9b")).toBe(true);
    expect(sameModel("llama3.1", "qwen3.5:9b")).toBe(false);
  });

  it("a model is in use, already here, or to download", () => {
    const installed = [
      { name: "llama3.1:latest", size: 4_920_000_000 },
      { name: "qwen3.5:4b", size: 3_330_000_000 },
    ];
    const nine = row();
    const four = row({ model: "qwen3.5:4b" });
    const llama = row({ model: "llama3.1" });
    expect(choiceState(nine, installed, "llama3.1")).toBe("download");
    expect(choiceState(four, installed, "llama3.1")).toBe("installed");
    expect(choiceState(llama, installed, "llama3.1")).toBe("in_use");
    // The server isn't answering: nothing is known to be here.
    expect(choiceState(four, undefined, "llama3.1")).toBe("download");
  });

  it("the buttons say what a click costs", () => {
    expect(choiceAction(row(), "download", false)).toBe("Download and use (6.6 GB)");
    expect(choiceAction(row(), "installed", false)).toBe("Use");
    expect(choiceAction(row(), "download", true)).toBe("Working…");
    const offer = { model: "qwen3.5:9b", download_bytes: 6_600_000_000 };
    expect(offerAction(offer, [], false)).toBe("Download and switch (6.6 GB)");
    expect(offerAction(offer, [{ name: "qwen3.5:9b", size: 1 }], false)).toBe("Switch");
    expect(offerAction(offer, [], true)).toBe("Working…");
  });

  it("finds a row by model, tag-insensitively", () => {
    const a = advice({ models: [row(), row({ model: "llama3.1" })] });
    expect(rowFor(a, "llama3.1:latest")?.model).toBe("llama3.1");
    expect(rowFor(a, "mistral")).toBeNull();
    expect(rowFor(null, "llama3.1")).toBeNull();
  });

  it("the offer shows only for a local provider with Ollama installed", () => {
    expect(showOffer(advice(), true, "ollama")).toBe(true);
    expect(showOffer(advice(), false, "ollama")).toBe(false);
    expect(showOffer(advice(), true, "anthropic")).toBe(false);
    expect(showOffer(advice({ offer: null }), true, "ollama")).toBe(false);
    expect(showOffer(null, true, "ollama")).toBe(false);
  });
});
