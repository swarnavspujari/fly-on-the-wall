// @vitest-environment jsdom
import "../devMock";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import { useCompressNotice } from "./CompressNotice";
import { ConfirmProvider } from "./confirm";
import { confirmChoice, confirmTitle } from "./confirmTesting";
import StorageRows from "./StorageRows";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;
const mount = async (node: React.ReactNode) => {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(node));
  for (let i = 0; i < 3; i++) await act(async () => {});
  return host;
};

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  localStorage.clear();
  vi.restoreAllMocks();
});

describe("Settings › Storage rows", () => {
  it("shows recording and video sizes with the job's status", async () => {
    localStorage.setItem("fotwMockCompress", "working");
    const el = await mount(
      <ConfirmProvider>
        <StorageRows />
      </ConfirmProvider>,
    );
    expect(el.textContent).toContain("Recordings");
    expect(el.textContent).toContain("Compressing recordings without losing anything: 43%.");
    expect(el.querySelector('[data-testid="storage-audio"]')!.textContent).toBe("5.9 GB");
    expect(el.textContent).toContain("2 videos, 3.0 GB.");
    expect(el.textContent).toContain("4 recordings, 1.5 GB.");
  });

  it("after a pass, says what failed and why, and what waits for later", async () => {
    localStorage.setItem("fotwMockCompressFailed", "1");
    const el = await mount(
      <ConfirmProvider>
        <StorageRows />
      </ConfirmProvider>,
    );
    expect(el.textContent).toContain(
      "Recordings are stored losslessly as FLAC. 5.2 GB freed. 1 meeting was in use and will be compressed later. 1 meeting couldn’t be compressed and is kept as recorded (not enough free disk space",
    );
  });

  it("renders nothing until the report arrives", async () => {
    vi.spyOn(api, "storageReport").mockReturnValue(new Promise(() => {}));
    const el = await mount(
      <ConfirmProvider>
        <StorageRows />
      </ConfirmProvider>,
    );
    expect(el.textContent).toBe("");
  });
});

describe("Settings › video choices", () => {
  const rows = () =>
    mount(
      <ConfirmProvider>
        <StorageRows />
      </ConfirmProvider>,
    );
  const button = (el: HTMLElement, text: string) =>
    [...el.querySelectorAll("button")].find((b) => b.textContent === text) as HTMLButtonElement;

  it("Audio only asks first; cancelling changes nothing", async () => {
    const set = vi.spyOn(api, "setImportVideoPolicy");
    const el = await rows();
    act(() => button(el, "Audio only").click());
    await act(async () => {});
    expect(confirmTitle()).toBe("Keep only the audio of imported videos?");
    act(() => confirmChoice(false).click());
    await act(async () => {});
    expect(set).not.toHaveBeenCalled();
    act(() => button(el, "Audio only").click());
    await act(async () => {});
    act(() => confirmChoice(true).click());
    for (let i = 0; i < 3; i++) await act(async () => {});
    expect(set).toHaveBeenCalledWith("audio_only");
  });

  it("Keep video needs no confirmation", async () => {
    await api.setImportVideoPolicy("audio_only"); // the mock's saved policy
    const set = vi.spyOn(api, "setImportVideoPolicy");
    const el = await rows();
    act(() => button(el, "Keep video").click());
    for (let i = 0; i < 3; i++) await act(async () => {});
    expect(confirmTitle()).toBeNull();
    expect(set).toHaveBeenCalledWith("keep");
  });

  it("Shrink estimates, asks, then starts the job", async () => {
    const start = vi.spyOn(api, "startVideoJob");
    const est = vi.spyOn(api, "estimateScreenShrink");
    const el = await rows();
    act(() => button(el, "Shrink…").click());
    for (let i = 0; i < 3; i++) await act(async () => {});
    expect(est).toHaveBeenCalledTimes(1);
    expect(confirmTitle()).toBe("Shrink 4 screen recordings?");
    act(() => confirmChoice(true).click());
    for (let i = 0; i < 3; i++) await act(async () => {});
    expect(start).toHaveBeenCalledWith("shrink_screen");
  });

  it("a running job shows its progress and a Stop button", async () => {
    localStorage.setItem("fotwMockVideoJob", "working");
    const cancel = vi.spyOn(api, "cancelVideoJob");
    const el = await rows();
    expect(el.textContent).toContain("Shrinking screen recordings: 2 of 4 recordings.");
    act(() => button(el, "Stop").click());
    expect(cancel).toHaveBeenCalledTimes(1);
  });
});

function NoticeProbe() {
  const n = useCompressNotice();
  return (
    <button data-saved={n.savedBytes ?? "none"} onClick={n.dismiss}>
      x
    </button>
  );
}

describe("one-time compressed notice", () => {
  it("is due only when the backend says so, and dismissing it tells the backend", async () => {
    localStorage.setItem("fotwMockCompressNotice", "1");
    const dismiss = vi.spyOn(api, "dismissCompressNotice");
    const el = await mount(<NoticeProbe />);
    const b = el.querySelector("button")!;
    expect(Number(b.dataset.saved)).toBeGreaterThan(5 * 1024 ** 3);
    act(() => b.click());
    expect(b.dataset.saved).toBe("none");
    expect(dismiss).toHaveBeenCalledTimes(1);
  });

  it("is not due by default", async () => {
    const el = await mount(<NoticeProbe />);
    expect(el.querySelector("button")!.dataset.saved).toBe("none");
  });
});
