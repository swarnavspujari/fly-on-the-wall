// @vitest-environment jsdom
// The import queue's per-file failure attribution (spec §3.6): a file that
// couldn't be converted shows its own cause and Retry, and the rest of the
// batch can go ahead without it.
import "../devMock";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ImportFile, ImportStaged } from "../types";
import ImportQueue from "./ImportQueue";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const file = (id: string, over: Partial<ImportFile> = {}): ImportFile => ({
  id,
  file_name: `${id}.mp4`,
  size: 1_000_000,
  kind: "video",
  rel_path: `recordings/imp/${id}.mp4`,
  error: null,
  convert_error: null,
  ...over,
});

const CAUSE = "This file looks damaged or incomplete, so its audio can't be read.";

const staged = (files: ImportFile[], started = false): ImportStaged => ({
  note_id: "n-imp",
  meeting_id: "m-imp",
  files,
  boundaries_ms: [],
  started,
});

let root: Root | null = null;
let host: HTMLElement | null = null;

async function flush() {
  for (let i = 0; i < 5; i++) await act(async () => {});
}

async function mount(
  s: ImportStaged,
  over: { pipelineError?: string | null; onRetry?: () => void } = {},
) {
  const onTranscribe = vi.fn();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <ImportQueue
        staged={s}
        pipeStage={null}
        pipeDetail={null}
        pipelineError={over.pipelineError ?? null}
        onTranscribe={onTranscribe}
        onRetry={over.onRetry ?? (() => {})}
        onCancel={() => {}}
      />,
    ),
  );
  await flush();
  return onTranscribe;
}

const button = (text: string) =>
  [...host!.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent?.trim() === text,
  ) ?? null;

async function click(el: Element | null) {
  if (!el) throw new Error("element not found");
  await act(async () => (el as HTMLElement).click());
  await flush();
}

afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
  host = null;
});

describe("ImportQueue per-file failures", () => {
  it("shows the failed file's own cause and a Retry, and counts only the rest", async () => {
    await mount(staged([file("a"), file("b", { convert_error: CAUSE }), file("c")]));
    expect(host!.textContent).toContain(CAUSE);
    // one Retry for the queue, not one per row that secretly retries them all
    expect(button("Retry failed file")).not.toBeNull();
    expect(button("Retry")).toBeNull();
    expect(button("Transcribe 2 files")).not.toBeNull();
    expect(host!.textContent).toContain("1 file couldn't be converted");
  });

  it("Retry runs the queue again with the failed file in its place", async () => {
    const onTranscribe = await mount(
      staged([file("a"), file("b", { convert_error: CAUSE }), file("c")]),
    );
    await click(button("Retry failed file"));
    expect(onTranscribe).toHaveBeenCalledWith(["a", "b", "c"]);
  });

  it("Transcribe goes ahead without the failed file", async () => {
    const onTranscribe = await mount(
      staged([file("a"), file("b", { convert_error: CAUSE }), file("c")]),
    );
    await click(button("Transcribe 2 files"));
    expect(onTranscribe).toHaveBeenCalledWith(["a", "c"]);
  });

  it("a failed file can be removed from the queue", async () => {
    const onTranscribe = await mount(
      staged([file("a"), file("b", { convert_error: CAUSE }), file("c")]),
    );
    await click(host!.querySelector('button[aria-label="Remove b.mp4"]'));
    expect(host!.textContent).not.toContain(CAUSE);
    expect(button("Retry failed file")).toBeNull();
    await click(button("Transcribe 2 files"));
    expect(onTranscribe).toHaveBeenCalledWith(["a", "c"]);
  });

  it("with every file failed there's nothing to transcribe, but they can be retried", async () => {
    await mount(staged([file("a", { convert_error: CAUSE })]));
    expect(button("Transcribe")?.disabled).toBe(true);
    expect(button("Retry failed file")).not.toBeNull();
    expect(host!.textContent).toContain("The file couldn't be converted");
  });

  it("words the counts naturally", async () => {
    await mount(staged([file("a", { convert_error: CAUSE }), file("b", { convert_error: CAUSE })]));
    expect(host!.textContent).toContain("Neither file could be converted");
    expect(button("Retry failed files")).not.toBeNull();
    if (root) act(() => root!.unmount());
    host?.remove();
    await mount(
      staged([
        file("a", { convert_error: CAUSE }),
        file("b", { convert_error: CAUSE }),
        file("c", { convert_error: CAUSE }),
      ]),
    );
    expect(host!.textContent).toContain("None of the 3 files could be converted");
    if (root) act(() => root!.unmount());
    host?.remove();
    await mount(staged([file("a"), file("b", { convert_error: CAUSE })]));
    expect(host!.textContent).toContain("transcribe the other file without it");
  });

  it("marks a file the moment its conversion fails while the rest carry on", async () => {
    await mount(staged([file("a"), file("b"), file("c")], true));
    const emit = (window as unknown as { fotwMockEmit: (e: string, p: unknown) => void })
      .fotwMockEmit;
    await act(async () => {
      emit("import:progress", { meeting_id: "m-imp", file_id: "b", stage: "failed", error: CAUSE });
      emit("import:progress", { meeting_id: "m-imp", file_id: "c", stage: "converting" });
    });
    await flush();
    expect(host!.textContent).toContain("Couldn't convert");
    expect(host!.textContent).toContain(CAUSE);
    expect(host!.textContent).toContain("Preparing…");
  });

  it("keeps unsupported files out of the queue as before", async () => {
    const onTranscribe = await mount(
      staged([file("a"), file("x", { rel_path: null, error: "Not an audio or video file" })]),
    );
    expect(button("Retry failed file")).toBeNull();
    await click(button("Transcribe"));
    expect(onTranscribe).toHaveBeenCalledWith(["a"]);
  });

  it("after the run gave up: its cause and Retry, no per-file progress, no Cancel", async () => {
    const onRetry = vi.fn();
    await mount(staged([file("a"), file("b")], true), {
      pipelineError: "speaker detection failed: failed to launch sherpa-onnx",
      onRetry,
    });
    expect(host!.textContent).toContain("failed to launch sherpa-onnx");
    expect(host!.textContent).not.toContain("Starting…");
    // the files aren't waiting on anything: the run is over until Retry
    expect(host!.textContent).not.toContain("Waiting");
    expect(button("Cancel")).toBeNull();
    await click(button("Retry"));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });
});
