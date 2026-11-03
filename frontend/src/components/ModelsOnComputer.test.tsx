// @vitest-environment jsdom
// Settings › Models on this computer against the dev mock backend (the
// same stub port-1420 QA uses); the mock mirrors model_cleanup.rs.
import { resetMockModelStorage } from "../devMock";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { AsrSettings } from "../types";
import { ConfirmProvider } from "./confirm";
import { confirmChoice, confirmDialog, confirmTitle } from "./confirmTesting";
import ModelsOnComputer from "./ModelsOnComputer";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;
let onDeleted = vi.fn();

async function flush() {
  for (let i = 0; i < 5; i++) await act(async () => {});
}

async function render(settings?: AsrSettings) {
  const s = settings ?? (await api.getAsrSettings());
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  onDeleted = vi.fn();
  act(() =>
    root!.render(
      <ConfirmProvider>
        <ModelsOnComputer
          settings={s}
          downloading={null}
          downloadErrors={{}}
          modelProgress={null}
          onDownload={() => {}}
          onDeleted={onDeleted}
        />
      </ConfirmProvider>,
    ),
  );
  await flush();
}

const $ = <T extends Element = HTMLElement>(sel: string) => document.querySelector<T>(sel);
const rowOf = (id: string) => $(`[data-testid="model-row-${id}"]`)!;
const deleteIn = (el: Element) =>
  [...el.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent === "Delete")!;
const freeUpButton = () =>
  [...$('[data-testid="free-up"]')!.querySelectorAll<HTMLButtonElement>("button")][0];

async function click(el: Element) {
  await act(async () => {
    (el as HTMLElement).click();
  });
  await flush();
}

async function answer(yes: boolean) {
  await click(confirmChoice(yes));
}

beforeEach(() => {
  resetMockModelStorage();
  localStorage.removeItem("fotwMockRecording");
});

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  localStorage.removeItem("fotwMockRecording");
});

describe("ModelsOnComputer", () => {
  it("shows size on disk and what each model is used for", async () => {
    await render();
    const turbo = rowOf("ggml-large-v3-turbo-q5_0");
    expect(turbo.textContent).toContain("574 MB on disk");
    expect(turbo.textContent).toContain("Used for Balanced transcription.");
    expect(turbo.textContent).toContain("in use");
    const small = rowOf("ggml-small-q5_1");
    expect(small.textContent).toContain("the live transcript while recording");
    const medium = rowOf("ggml-medium-q5_0");
    expect(medium.textContent).toContain("not in use");
    expect(medium.textContent).toContain("Not used by your current settings");
    // not downloaded: download size and Install, no Delete
    const large = rowOf("ggml-large-v3-q5_0");
    expect(large.textContent).toContain("1.1 GB download");
    expect(large.textContent).toContain("Install");
    expect(deleteIn(large)).toBeUndefined();
    // the engine row keeps its own wording and gets its size
    const engine = $('[data-testid="models-on-computer"]')!.firstElementChild!;
    expect(engine.textContent).toContain("Transcription engine (whisper.cpp)");
    expect(engine.textContent).toContain("21 MB on disk");
  });

  it("disables Delete on models in use and says why", async () => {
    await render();
    for (const id of ["ggml-large-v3-turbo-q5_0", "pyannote-seg", "whisper-bin-vulkan"]) {
      const btn = deleteIn(rowOf(id));
      expect(btn.disabled, id).toBe(true);
      expect(btn.title, id).toMatch(/^In use, so it can't be deleted\. Used for /);
      const why = document.getElementById(btn.getAttribute("aria-describedby")!);
      expect(why?.textContent, id).toContain("Used for");
    }
    expect(deleteIn(rowOf("ggml-medium-q5_0")).disabled).toBe(false);
    expect(deleteIn(rowOf("ffmpeg")).disabled).toBe(false);
  });

  it("asks before deleting one model; Cancel keeps it", async () => {
    const del = vi.spyOn(api, "deleteModelFiles");
    await render();
    await click(deleteIn(rowOf("ggml-medium-q5_0")));
    expect(confirmTitle()).toBe("Delete Whisper medium?");
    expect(confirmDialog()!.textContent).toContain(
      "It's removed from this computer. You can download it again later.",
    );
    expect(confirmDialog()!.dataset.tone).toBe("danger");
    await answer(false);
    expect(del).not.toHaveBeenCalled();

    await click(deleteIn(rowOf("ggml-medium-q5_0")));
    await answer(true);
    expect(del).toHaveBeenCalledWith(["ggml-medium-q5_0"], []);
    expect(onDeleted).toHaveBeenCalled();
    expect($('[role="status"]')!.textContent).toBe("Freed 539 MB.");
    // the row is back to a download (re-download works on demand)
    const medium = rowOf("ggml-medium-q5_0");
    expect(medium.textContent).toContain("Install");
    del.mockRestore();
  });

  it("Free up lists what goes, then removes unused models and leftovers only", async () => {
    const del = vi.spyOn(api, "deleteModelFiles");
    await render();
    const before = await api.modelStorageReport();
    // leftovers and not-recognised files are listed with their sizes
    expect(document.querySelectorAll('[data-testid="leftover-row"]').length).toBe(3);
    const kept = document.querySelectorAll('[data-testid="unrecognised-row"]');
    expect(kept.length).toBe(1);
    expect(kept[0].textContent).toContain("models/asr/my-notes.txt");
    expect(kept[0].textContent).toContain("left alone");

    const summary = $('[data-testid="free-up"]')!.textContent!;
    expect(summary).toContain("Removes 2 unused models and 3 leftovers.");
    const btn = freeUpButton();
    expect(btn.textContent).toBe("Free up 1.1 GB");
    expect(before.reclaimable_bytes).toBe(539_212_467 + 214_958_080 + 391_118_848);

    await click(btn);
    expect(confirmTitle()).toBe("Free up 1.1 GB?");
    const items = [...document.querySelectorAll('[data-testid="free-up-list"] li')].map(
      (li) => li.textContent,
    );
    expect(items).toEqual([
      "Whisper medium · 539 MB",
      "ffmpeg · 215 MB",
      "Unfinished download of Whisper large-v3 · 312 MB",
      "Older version of whisper.cpp CLI · 21 MB",
      "Speech model this version no longer uses · 58 MB",
    ]);
    expect(confirmDialog()!.textContent).toContain(
      "Files the app doesn't recognise are left alone.",
    );
    await answer(true);
    expect(del).toHaveBeenCalledWith(
      ["ggml-medium-q5_0", "ffmpeg"],
      [
        "ggml-large-v3-q5_0.download.4242.0",
        "bin/whisper/v1.9.1",
        "models/asr/ggml-base.en-q5_1.bin",
      ],
    );
    expect($('[role="status"]')!.textContent).toBe("Freed 1.1 GB.");
    expect($('[data-testid="free-up"]')!.textContent).toContain(
      "Nothing to free up. Everything here is in use.",
    );
    expect(freeUpButton().disabled).toBe(true);
    // the unrecognised file is still listed
    expect(document.querySelectorAll('[data-testid="unrecognised-row"]').length).toBe(1);
    expect(document.querySelectorAll('[data-testid="leftover-row"]').length).toBe(0);
    del.mockRestore();
  });

  it("while recording, nothing can be deleted and it says why", async () => {
    localStorage.setItem("fotwMockRecording", "1");
    await render();
    const free = $('[data-testid="free-up"]')!;
    expect(free.textContent).toContain("Recording now. You can delete models after it ends.");
    expect(freeUpButton().disabled).toBe(true);
    const btn = deleteIn(rowOf("ggml-medium-q5_0"));
    expect(btn.disabled).toBe(true);
    expect(btn.title).toBe("Recording now. You can delete models after it ends.");
    // ffmpeg is in use for the screen recording
    expect(rowOf("ffmpeg").textContent).toContain("the screen recording in progress");
  });

  it("a model in a linked folder says so, has no size and no working Delete", async () => {
    const real = await api.modelStorageReport();
    const linkedNote = "Linked folder, not managed by the app.";
    const report = vi.spyOn(api, "modelStorageReport").mockResolvedValue({
      ...real,
      artifacts: real.artifacts.map((a) =>
        a.id === "ggml-medium-q5_0"
          ? {
              ...a,
              linked: true,
              size_on_disk: 0,
              note: linkedNote,
              can_delete: false,
              blocked: linkedNote,
            }
          : a,
      ),
      reclaimable_bytes: real.reclaimable_bytes - 539_212_467,
    });
    await render();
    const medium = rowOf("ggml-medium-q5_0");
    expect(medium.textContent).toContain("linked");
    expect(medium.textContent).toContain(linkedNote);
    expect(medium.textContent).not.toContain("on disk");
    const btn = deleteIn(medium);
    expect(btn.disabled).toBe(true);
    expect(btn.title).toBe(linkedNote);
    // not part of Free up
    expect(freeUpButton().textContent).toBe("Free up 606 MB");
    report.mockRestore();
  });

  it("a backend refusal is shown, and the list refreshes", async () => {
    const del = vi
      .spyOn(api, "deleteModelFiles")
      .mockRejectedValueOnce("Transcribing now. You can delete models when it finishes.");
    await render();
    await click(deleteIn(rowOf("ggml-medium-q5_0")));
    await answer(true);
    expect($('[role="status"]')!.textContent).toBe(
      "Transcribing now. You can delete models when it finishes.",
    );
    expect(rowOf("ggml-medium-q5_0").textContent).toContain("not in use");
    del.mockRestore();
  });
});
