// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The hook talks to three modules; mock all of them so the test drives the
// real state machine without a Tauri runtime.
const pluginCheck = vi.fn();
vi.mock("@tauri-apps/plugin-updater", () => ({
  check: (...a: unknown[]) => pluginCheck(...a),
}));
const relaunch = vi.fn(async () => {});
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: () => relaunch() }));
const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class {
    onmessage: (m: unknown) => void = () => {};
  },
}));

import { useUpdater, type Updater } from "./updater";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// Minimal renderHook (no @testing-library dependency in this repo).
function renderUpdater(os: string, recording = false) {
  const result: { current: Updater } = { current: undefined as unknown as Updater };
  const props = { os, recording };
  function Probe() {
    result.current = useUpdater(props.os, props.recording);
    return null;
  }
  const host = document.createElement("div");
  const root: Root = createRoot(host);
  act(() => root.render(createElement(Probe)));
  const rerender = (recordingNow: boolean) => {
    props.recording = recordingNow;
    act(() => root.render(createElement(Probe)));
  };
  return { result, rerender, unmount: () => act(() => root.unmount()) };
}

function fakeUpdate(over: Record<string, unknown> = {}) {
  return {
    version: "9.9.9",
    download: vi.fn(async () => {}),
    install: vi.fn(async () => {}),
    ...over,
  };
}

/** Run an async hook action to completion inside act(). */
async function run(fn: () => void) {
  await act(async () => {
    fn();
    await new Promise((r) => setTimeout(r, 0));
  });
}

let hook: ReturnType<typeof renderUpdater> | null = null;
beforeEach(() => {
  pluginCheck.mockReset();
  invoke.mockReset();
  relaunch.mockClear();
  vi.spyOn(console, "warn").mockImplementation(() => {});
});
afterEach(() => {
  hook?.unmount();
  hook = null;
  vi.restoreAllMocks();
});

describe("useUpdater: install failures", () => {
  it("an install that rejects ends in phase error with errorSource install", async () => {
    const update = fakeUpdate({ install: vi.fn(async () => Promise.reject(new Error("blocked"))) });
    pluginCheck.mockResolvedValue(update);
    hook = renderUpdater("macos");
    await run(() => hook!.result.current.check());
    expect(hook.result.current.phase).toBe("available");
    await run(() => hook!.result.current.downloadAndInstall());
    expect(hook.result.current.phase).toBe("error");
    expect(hook.result.current.errorSource).toBe("install");
    expect(hook.result.current.error).toContain("blocked");
    expect(relaunch).not.toHaveBeenCalled();
  });

  it("Try again checks for the update again instead of reusing the stale one", async () => {
    const stale = fakeUpdate({ install: vi.fn(async () => Promise.reject(new Error("blocked"))) });
    const fresh = fakeUpdate();
    pluginCheck.mockResolvedValueOnce(stale).mockResolvedValueOnce(fresh);
    hook = renderUpdater("macos");
    await run(() => hook!.result.current.check());
    await run(() => hook!.result.current.downloadAndInstall());
    expect(hook.result.current.phase).toBe("error");
    expect(pluginCheck).toHaveBeenCalledTimes(1);

    await run(() => hook!.result.current.downloadAndInstall()); // "Try again"
    expect(pluginCheck).toHaveBeenCalledTimes(2);
    expect(stale.download).toHaveBeenCalledTimes(1); // not reused
    expect(fresh.download).toHaveBeenCalledTimes(1);
    expect(relaunch).toHaveBeenCalledTimes(1);
  });

  it("Try again with nothing newer reports up to date instead of failing again", async () => {
    const stale = fakeUpdate({ install: vi.fn(async () => Promise.reject(new Error("blocked"))) });
    pluginCheck.mockResolvedValueOnce(stale).mockResolvedValueOnce(null);
    hook = renderUpdater("macos");
    await run(() => hook!.result.current.check());
    await run(() => hook!.result.current.downloadAndInstall());
    await run(() => hook!.result.current.downloadAndInstall());
    expect(hook.result.current.phase).toBe("upToDate");
  });
});

describe("useUpdater: platform routing", () => {
  it("Windows installs through the app command, never the plugin's JS install", async () => {
    const update = fakeUpdate();
    pluginCheck.mockResolvedValue(update);
    invoke.mockResolvedValue(undefined);
    hook = renderUpdater("windows");
    await run(() => hook!.result.current.check());
    await run(() => hook!.result.current.downloadAndInstall());
    expect(invoke.mock.calls.map((c) => c[0])).toEqual([
      "windows_update_download",
      "windows_update_install",
    ]);
    expect(update.download).not.toHaveBeenCalled();
    expect(update.install).not.toHaveBeenCalled();
  });

  it("macOS and Linux use the JS plugin path and never the Windows commands", async () => {
    for (const os of ["macos", "linux"]) {
      const update = fakeUpdate();
      pluginCheck.mockResolvedValue(update);
      const h = renderUpdater(os);
      await run(() => h.result.current.check()); // Linux without AppImage is unsupported
      if (h.result.current.phase === "available") {
        await run(() => h.result.current.downloadAndInstall());
        expect(update.install).toHaveBeenCalled();
      }
      h.unmount();
    }
    expect(invoke).not.toHaveBeenCalled();
  });

  it("Windows: a failed installer launch shows the error and Try again recovers", async () => {
    pluginCheck.mockResolvedValue(fakeUpdate());
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "windows_update_install") throw new Error("installer blocked");
    });
    hook = renderUpdater("windows");
    await run(() => hook!.result.current.check());
    await run(() => hook!.result.current.downloadAndInstall());
    expect(hook.result.current.phase).toBe("error");
    expect(hook.result.current.errorSource).toBe("install");

    invoke.mockReset();
    invoke.mockResolvedValue(undefined);
    await run(() => hook!.result.current.downloadAndInstall());
    expect(pluginCheck).toHaveBeenCalledTimes(2);
    expect(invoke.mock.calls.map((c) => c[0])).toEqual([
      "windows_update_download",
      "windows_update_install",
    ]);
  });

  it("Windows: a recording that starts mid-download parks the update in ready", async () => {
    pluginCheck.mockResolvedValue(fakeUpdate());
    let finishDownload: () => void = () => {};
    invoke.mockImplementation((cmd: string) =>
      cmd === "windows_update_download"
        ? new Promise<void>((r) => (finishDownload = r))
        : Promise.resolve(),
    );
    hook = renderUpdater("windows");
    await run(() => hook!.result.current.check());
    await run(() => hook!.result.current.downloadAndInstall());
    expect(hook.result.current.phase).toBe("downloading");
    hook.rerender(true);
    await run(() => finishDownload());
    expect(hook.result.current.phase).toBe("ready");
    expect(invoke.mock.calls.map((c) => c[0])).toEqual(["windows_update_download"]);
  });
});
