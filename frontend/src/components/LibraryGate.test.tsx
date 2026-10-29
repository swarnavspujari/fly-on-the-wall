// @vitest-environment jsdom
// The startup gate in front of <App/>: the app mounts only once the backend
// says the library is open, and an older backend without the command never
// blocks it.
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { LibraryStatus } from "../types";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const libraryStatus = vi.fn<() => Promise<LibraryStatus>>();
let handlers: ((e: { payload: LibraryStatus }) => void)[] = [];
vi.mock("../api", () => ({ api: { libraryStatus: () => libraryStatus() } }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, h: (e: { payload: LibraryStatus }) => void) => {
    handlers.push(h);
    return () => {
      handlers = handlers.filter((x) => x !== h);
    };
  }),
}));

const { default: LibraryGate, LIBRARY_POLL_MS } = await import("./LibraryGate");

let root: Root | null = null;
let host: HTMLElement | null = null;

beforeEach(() => {
  vi.useFakeTimers();
  libraryStatus.mockReset();
  handlers = [];
});

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.useRealTimers();
});

async function settle(ms = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function render() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(
      <LibraryGate>
        <div data-testid="app">the app</div>
      </LibraryGate>,
    );
  });
  await settle();
}

async function emit(payload: LibraryStatus) {
  await act(async () => {
    for (const h of handlers) h({ payload });
  });
}

const appShown = () => host!.querySelector('[data-testid="app"]') != null;
const heading = () => host!.querySelector("h1")?.textContent ?? null;
const percent = () => host!.querySelector('[role="progressbar"]')?.getAttribute("aria-valuenow");

describe("LibraryGate", () => {
  it("mounts the app at once when the library is ready", async () => {
    libraryStatus.mockResolvedValue({ phase: "ready", done: 0, total: 0 });
    await render();
    expect(appShown()).toBe(true);
    expect(heading()).toBeNull();
  });

  it("shows the upgrade screen, follows the event, then mounts the app", async () => {
    libraryStatus.mockResolvedValue({ phase: "upgrading", done: 2, total: 9 });
    await render();
    expect(appShown()).toBe(false);
    expect(heading()).toBe("Upgrading your library");
    expect(host!.textContent).toContain("your notes are safe");
    expect(percent()).toBe("22");

    await emit({ phase: "upgrading", done: 6, total: 9 });
    expect(percent()).toBe("66");
    expect(appShown()).toBe(false);

    await emit({ phase: "ready", done: 0, total: 0 });
    expect(appShown()).toBe(true);
    expect(heading()).toBeNull();
  });

  it("listens before it asks, so a finish in between isn't missed", async () => {
    // The upgrade ends while the status call is in flight: the event fires
    // first, then the (stale) answer arrives.
    let answer: (s: LibraryStatus) => void = () => {};
    libraryStatus.mockReturnValue(new Promise((r) => (answer = r)));
    await render();
    expect(handlers.length).toBe(1);
    await emit({ phase: "ready", done: 0, total: 0 });
    await act(async () => answer({ phase: "upgrading", done: 8, total: 9 }));
    expect(appShown()).toBe(true);
  });

  it("polls while upgrading in case an event is missed", async () => {
    libraryStatus.mockResolvedValueOnce({ phase: "upgrading", done: 1, total: 9 });
    libraryStatus.mockResolvedValue({ phase: "ready", done: 0, total: 0 });
    await render();
    expect(appShown()).toBe(false);
    await settle(LIBRARY_POLL_MS);
    expect(appShown()).toBe(true);
  });

  it("never moves the bar back on a late answer", async () => {
    libraryStatus.mockResolvedValueOnce({ phase: "upgrading", done: 1, total: 9 });
    libraryStatus.mockResolvedValue({ phase: "upgrading", done: 3, total: 9 });
    await render();
    await emit({ phase: "upgrading", done: 7, total: 9 });
    await settle(LIBRARY_POLL_MS);
    expect(percent()).toBe("77");
  });

  it("mounts the app when the command isn't there (older backend, dev)", async () => {
    libraryStatus.mockRejectedValue(new Error("Command library_status not found"));
    await render();
    expect(appShown()).toBe(true);
  });

  it("says so calmly when the upgrade fails", async () => {
    libraryStatus.mockResolvedValue({ phase: "upgrading", done: 0, total: 0 });
    await render();
    expect(percent()).toBe("0");
    await emit({ phase: "failed", done: 0, total: 0 });
    expect(heading()).toBe("Your library couldn't be upgraded");
    expect(host!.textContent).toContain("Your notes and recordings are safe");
    expect(appShown()).toBe(false);
  });
});
