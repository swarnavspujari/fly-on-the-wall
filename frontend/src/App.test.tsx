// @vitest-environment jsdom
// App-level tests for note deletion and the list's live chips, driving the
// real App against the dev mock backend (devMock.ts, the same stub used for
// port-1420 QA). Each test boots a fresh App + mock (the mock is stateful).
import type { Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
// Booting the whole App (first transform included) can exceed the 5 s default
// when the suite runs in parallel.
vi.setConfig({ testTimeout: 30_000 });

type Args = Record<string, unknown>;
type Override = (cmd: string, args: Args) => Promise<unknown> | undefined;
interface MockWindow {
  __TAURI_INTERNALS__?: { invoke: (cmd: string, args: Args) => Promise<unknown> };
  fotwMockEmit?: (event: string, payload: unknown) => void;
}

// React is imported fresh per boot (after vi.resetModules), so the test's
// act/createRoot and the App share one React instance.
let act: typeof import("react").act;
let root: Root | null = null;
let host: HTMLElement | null = null;
let calls: { cmd: string; args: Args }[] = [];
let override: Override | null = null;
let realInvoke: (cmd: string, args: Args) => Promise<unknown>;

const w = () => window as unknown as MockWindow;

/** Advance fake time and flush the promises it releases. */
async function settle(ms = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function boot(flags: Record<string, string> = {}) {
  vi.useFakeTimers();
  localStorage.clear();
  for (const [k, v] of Object.entries(flags)) localStorage.setItem(k, v);
  vi.resetModules();
  ({ act } = await import("react"));
  const { createRoot } = await import("react-dom/client");
  delete w().__TAURI_INTERNALS__;
  await import("./devMock");
  const internals = w().__TAURI_INTERNALS__!;
  const real = internals.invoke.bind(internals);
  realInvoke = real;
  calls = [];
  override = null;
  internals.invoke = (cmd, args) => {
    calls.push({ cmd, args });
    return override?.(cmd, args ?? {}) ?? real(cmd, args);
  };
  const { default: App } = await import("./App");
  const { ConfirmProvider } = await import("./components/confirm");
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(
      <ConfirmProvider>
        <App />
      </ConfirmProvider>,
    );
  });
  await settle(50);
}

afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.useRealTimers();
});

const listTitles = () =>
  Array.from(host!.querySelectorAll(".w-72 [draggable] .font-semibold.text-text")).map(
    (e) => e.textContent,
  );
const rowFor = (title: string) =>
  Array.from(host!.querySelectorAll<HTMLElement>(".w-72 [draggable]")).find((r) =>
    r.textContent?.includes(title),
  );
const chipFor = (title: string) => rowFor(title)?.querySelector(".fly-badge")?.textContent ?? null;
const trashFor = (title: string) =>
  host!.querySelector<HTMLElement>(`[aria-label="Delete note “${title}”"]`);
const toastText = () => host!.querySelector('[role="status"].fixed')?.textContent ?? null;
const homeShown = () => host!.querySelector('[aria-label="Up next"]') != null;
const deleteCalls = () => calls.filter((c) => c.cmd === "delete_note").map((c) => c.args.id);
const button = (label: string) =>
  Array.from(host!.querySelectorAll("button")).find((b) => b.textContent?.trim() === label);

async function click(el: Element | null | undefined) {
  if (!el) throw new Error("element not found");
  await act(async () => {
    (el as HTMLElement).click();
  });
  await settle(0);
}

describe("App: deleting a note", () => {
  it("a late editor save can't bring a deleted note back (1)", async () => {
    await boot();
    await click(rowFor("Design weekly — notes"));
    await settle(100);
    expect(homeShown()).toBe(false);

    // Type, then delete within the editor's debounce window.
    const editable = host!.querySelector<HTMLElement>('[contenteditable="true"]');
    expect(editable).not.toBeNull();
    await act(async () => {
      editable!.innerHTML += "<p>one more thought</p>";
      editable!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(trashFor("Design weekly — notes"));
    expect(homeShown()).toBe(true);

    // The debounced saves land after the delete…
    await settle(1500);
    expect(calls.some((c) => c.cmd === "update_note_scratchpad")).toBe(true);
    // …and the note stays gone: no editor, not in the list.
    expect(homeShown()).toBe(true);
    expect(listTitles()).not.toContain("Design weekly — notes");
    expect(toastText()).toContain("Deleted “Design weekly — notes”");

    await settle(8000);
    expect(deleteCalls()).toEqual(["n3"]);
    expect(homeShown()).toBe(true);
    expect(listTitles()).not.toContain("Design weekly — notes");
  });

  it("the row never flashes back while the delete commits (2)", async () => {
    await boot();
    let release: (() => void) | null = null;
    override = (cmd) => {
      // hold the list refresh that follows the delete
      if (cmd === "list_recent_notes" && deleteCalls().length > 0 && !release) {
        return new Promise<void>((r) => (release = r)).then(() =>
          realInvoke("list_recent_notes", { limit: 200 }),
        );
      }
      return undefined;
    };
    await click(trashFor("Roadmap brainstorm"));
    expect(listTitles()).not.toContain("Roadmap brainstorm");
    await settle(8000);
    expect(deleteCalls()).toEqual(["n6"]);
    // delete done, refresh still in flight: still hidden
    expect(release).not.toBeNull();
    expect(listTitles()).not.toContain("Roadmap brainstorm");
    override = null;
    await act(async () => release!());
    await settle(0);
    expect(listTitles()).not.toContain("Roadmap brainstorm");
    expect(toastText()).toBeNull();
  });

  it("a deleted note's search hits stay gone after the delete commits (2)", async () => {
    await boot();
    await click(trashFor("Acme — renewal review"));
    const search = Array.from(host!.querySelectorAll("input")).find((i) =>
      i.placeholder.startsWith("Search notes"),
    )!;
    expect(search).toBeTruthy();
    await act(async () => {
      const set = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(search), "value")!.set!;
      set.call(search, "pricing");
      search.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await settle(600);
    // the mock's search only ever returns n1's hits
    expect(calls.some((c) => c.cmd === "search")).toBe(true);
    expect(host!.textContent).toContain("No matches");
    await settle(8000);
    expect(deleteCalls()).toEqual(["n1"]);
    expect(host!.textContent).toContain("No matches");
  });

  it("a failed delete restores the note and says why (2)", async () => {
    await boot();
    override = (cmd) => (cmd === "delete_note" ? Promise.reject("disk is read-only") : undefined);
    await click(trashFor("Roadmap brainstorm"));
    expect(listTitles()).not.toContain("Roadmap brainstorm");
    await settle(8100);
    expect(listTitles()).toContain("Roadmap brainstorm");
    expect(host!.textContent).toContain("Couldn’t delete “Roadmap brainstorm”: disk is read-only");
  });

  it("two deletes share one toast; neither runs early; Undo restores both", async () => {
    await boot();
    await click(trashFor("Roadmap brainstorm"));
    await settle(5000);
    await click(trashFor("Design weekly — notes"));
    expect(toastText()).toContain("Deleted 2 notes");
    // the first one's own 8 s pass: nothing deleted yet
    await settle(5000);
    expect(deleteCalls()).toEqual([]);
    await click(button("Undo"));
    await settle(10_000);
    expect(deleteCalls()).toEqual([]);
    expect(listTitles()).toEqual(
      expect.arrayContaining(["Roadmap brainstorm", "Design weekly — notes"]),
    );
    expect(toastText()).toBeNull();
  });

  it("Undo reopens the deleted note only if no other note was opened since", async () => {
    await boot();
    await click(rowFor("Design weekly — notes"));
    await settle(100);
    await click(trashFor("Design weekly — notes"));
    await click(rowFor("Weekly 1:1 with Dana"));
    await settle(100);
    const before = calls.length;
    await click(button("Undo"));
    await settle(100);
    const reopened = calls.slice(before).some((c) => c.cmd === "get_note" && c.args.id === "n3");
    expect(reopened).toBe(false);
    expect(listTitles()).toContain("Design weekly — notes");
  });

  it("the note that's recording has no trash (3)", async () => {
    await boot({ fotwMockRecording: "1" });
    await settle(1100); // first recording_status poll
    expect(trashFor("Acme — renewal review")).toBeNull();
    expect(trashFor("Roadmap brainstorm")).not.toBeNull();
  });
});

describe("App: live chips", () => {
  it("a queued ('waiting') job shows Queued, not Transcribing (4)", async () => {
    await boot();
    expect(chipFor("Weekly 1:1 with Dana")).toBe("Ready to enhance");
    await act(async () => {
      w().fotwMockEmit!("pipeline:progress", {
        meeting_id: "m2",
        stage: "waiting",
        detail: null,
        done: false,
        error: null,
      });
    });
    expect(chipFor("Weekly 1:1 with Dana")).toBe("Queued");
    await act(async () => {
      w().fotwMockEmit!("pipeline:progress", {
        meeting_id: "m2",
        stage: "transcribing",
        detail: "10%",
        done: false,
        error: null,
      });
    });
    expect(chipFor("Weekly 1:1 with Dana")).toBe("Transcribing");
  });
});

describe("App: a finished import call", () => {
  const queueShown = () => host!.textContent?.includes("screen-walkthrough.mp4") ?? false;
  const CAUSE = "This file looks damaged or incomplete, so its audio can't be read.";

  /** Stage an import (it opens on its queue) and hold import_transcribe open. */
  async function stageAndHoldTranscribe() {
    let finish!: (outcome: "resolves" | "fails") => void;
    override = (cmd, args) => {
      if (cmd !== "import_transcribe") return undefined;
      return new Promise((resolve, reject) => {
        finish = (outcome) => {
          if (outcome === "fails") return reject(new Error("ffmpeg went away"));
          // the backend's paused queue: one file couldn't be converted
          void realInvoke("import_state", { meetingId: args.meetingId }).then((q) => {
            const staged = q as { files: { id: string }[] };
            resolve({
              ...staged,
              files: staged.files.map((f) => ({
                ...f,
                convert_error: f.id === "if-2" ? CAUSE : null,
              })),
            });
          });
        };
      });
    };
    await click(button("Import media as a note"));
    await settle(100);
    expect(queueShown()).toBe(true);
    await click(button("Transcribe 2 files"));
    return (outcome: "resolves" | "fails") =>
      act(async () => {
        finish(outcome);
        await vi.advanceTimersByTimeAsync(100);
      });
  }

  it("updates the queue of the import note that's still open", async () => {
    await boot();
    const finish = await stageAndHoldTranscribe();
    await finish("resolves");
    expect(queueShown()).toBe(true);
    expect(host!.textContent).toContain(CAUSE);
  });

  it.each(["resolves", "fails"] as const)(
    "never puts its queue on another note opened meanwhile (call %s)",
    async (outcome) => {
      await boot();
      const finish = await stageAndHoldTranscribe();
      await click(rowFor("Design weekly — notes"));
      await settle(100);
      expect(queueShown()).toBe(false);

      await finish(outcome);
      await settle(100);
      expect(queueShown()).toBe(false);
      expect(host!.textContent).not.toContain(CAUSE);
    },
  );
});

describe("App: an import whose run failed after Transcribe", () => {
  const CAUSE = "failed to launch sherpa-onnx";
  const text = () => host!.textContent ?? "";

  /** Stage the mock import and transcribe it; the run gives up at speaker
   *  detection (fotwMockImportPipelineFail), after ASR. */
  async function importAndFail() {
    await click(button("Import media as a note"));
    await settle(100);
    await click(button("Transcribe 2 files"));
    await settle(16_000);
  }

  /** Open another note, then the import again: what a restart also does. */
  async function reopenImport() {
    await click(rowFor("Design weekly — notes"));
    await settle(100);
    await click(rowFor("Planning Sync 2026-01-15"));
    await settle(100);
  }

  it("shows the cause and Retry when you come back to the note", async () => {
    await boot({ fotwMockImportPipelineFail: "1" });
    await importAndFail();
    expect(text()).toContain(CAUSE);

    await reopenImport();
    expect(text()).toContain(CAUSE);
    expect(text()).not.toContain("Starting…");
    expect(button("Retry")).toBeDefined();
    expect(button("Cancel")).toBeUndefined();

    // Retry re-enqueues the joined recording, and the run finishes
    await click(button("Retry"));
    expect(calls.some((c) => c.cmd === "transcribe_meeting" && c.args.meetingId === "m-imp")).toBe(
      true,
    );
    await settle(5_000);
    expect(text()).not.toContain(CAUSE);
    expect(text()).not.toContain("Imported media");
  });

  it("Cancel with nothing running drops a stuck queue to idle at once", async () => {
    await boot({ fotwMockImportPipelineFail: "1" });
    await importAndFail();
    // a queue left started with no job and no recorded failure (an enqueue
    // that never happened): it can only say "Starting…" and offer Cancel
    override = (cmd) => (cmd === "pipeline_failure" ? Promise.resolve(null) : undefined);
    await reopenImport();
    expect(text()).toContain("Starting…");

    await click(button("Cancel"));
    await settle(50);
    expect(text()).not.toContain("Starting…");
    expect(button("Transcribe 2 files")).toBeDefined();
  });
});

describe("App: a recording whose transcription failed", () => {
  it("says why when you open the note later, not just 'Transcribe recording'", async () => {
    const WHY = "whisper-cli exited with status 3221225477";
    const URL = "https://example.invalid/log?sig=0123456789abcdef";
    await boot({ fotwMockNoTranscript: "1" });
    // the run gave up while another note was open (or before a restart)
    override = (cmd, args) =>
      cmd === "pipeline_failure"
        ? Promise.resolve(args.meetingId === "m1" ? `${WHY} (log: ${URL})` : null)
        : undefined;
    await click(rowFor("Acme — renewal review"));
    await settle(100);
    const tab = Array.from(host!.querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Transcript"),
    );
    await click(tab);
    expect(button("Transcribe recording")).toBeDefined();
    expect(host!.textContent).toContain(WHY);
    // briefed like every other failure: no signed URLs
    expect(host!.textContent).not.toContain(URL);
  });
});

describe("App: a failure stays with its own meeting", () => {
  it("never carries over to a new note or the recording made in it", async () => {
    const WHY = "whisper-cli exited with status 3221225477";
    await boot({ fotwMockNoTranscript: "1" });
    let recorded = false;
    override = (cmd, args) => {
      if (cmd === "pipeline_failure") return Promise.resolve(args.meetingId === "m1" ? WHY : null);
      if (cmd === "start_recording") recorded = true;
      // the new note (the mock's create_note is n3) gets its own fresh meeting
      if (cmd === "get_meetings_for_note" && args.noteId === "n3" && recorded) {
        return Promise.resolve([
          {
            id: "m-new",
            title: "Design weekly — notes",
            note_id: "n3",
            attendees: [],
            attendees_confirmed: false,
            started_at: new Date().toISOString(),
            ended_at: null,
            recording: { mic_path: null, system_path: null, mixed_path: "mix.wav", duration_ms: 1 },
          },
        ]);
      }
      return undefined;
    };
    const transcriptTab = () =>
      Array.from(host!.querySelectorAll("button")).find((b) =>
        b.textContent?.includes("Transcript"),
      );
    await click(rowFor("Acme — renewal review"));
    await settle(100);
    await click(transcriptTab());
    expect(host!.textContent).toContain(WHY);

    await click(host!.querySelector('button[title="New note"]'));
    await settle(100);
    await click(button("Record"));
    await settle(100);
    await click(transcriptTab());
    // the new meeting's panel renders, without the other meeting's failure
    expect(button("Transcribe recording")).toBeDefined();
    expect(host!.textContent).not.toContain(WHY);
  });
});

describe("App: opening a note while its run reports", () => {
  it("a live failure isn't overwritten by the open's older read", async () => {
    const LIVE = "speaker detection failed: the diarizer crashed";
    await boot({ fotwMockNoTranscript: "1" });
    // hold the open's stage and failure reads until after the live event
    const held: (() => void)[] = [];
    override = (cmd, args) =>
      (cmd === "pipeline_failure" || cmd === "pipeline_stage") && args.meetingId === "m1"
        ? new Promise((resolve) => held.push(() => resolve(null)))
        : undefined;
    await click(rowFor("Acme — renewal review"));
    await act(async () => {
      w().fotwMockEmit!("pipeline:progress", {
        meeting_id: "m1",
        stage: "error",
        detail: null,
        done: true,
        error: LIVE,
      });
    });
    await act(async () => {
      for (const release of held) release();
    });
    await settle(100);
    const tab = Array.from(host!.querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Transcript"),
    );
    await click(tab);
    expect(host!.textContent).toContain(LIVE);
  });
});
