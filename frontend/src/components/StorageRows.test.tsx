// @vitest-environment jsdom
import "../devMock";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import { useCompressNotice } from "./CompressNotice";
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
    const el = await mount(<StorageRows />);
    expect(el.textContent).toContain("Recordings");
    expect(el.textContent).toContain("Compressing recordings without losing anything: 43%.");
    expect(el.querySelector('[data-testid="storage-audio"]')!.textContent).toBe("5.9 GB");
    expect(el.querySelector('[data-testid="storage-video"]')!.textContent).toBe("4.5 GB");
    expect(el.textContent).toContain("2 imported meeting videos (3.0 GB), 4 screen recordings");
  });

  it("after a pass, says what failed and why, and what waits for later", async () => {
    localStorage.setItem("fotwMockCompressFailed", "1");
    const el = await mount(<StorageRows />);
    expect(el.textContent).toContain(
      "Recordings are stored losslessly as FLAC. 5.2 GB freed. 1 meeting was in use and will be compressed later. 1 meeting couldn’t be compressed and is kept as recorded (not enough free disk space",
    );
  });

  it("renders nothing until the report arrives", async () => {
    vi.spyOn(api, "storageReport").mockReturnValue(new Promise(() => {}));
    const el = await mount(<StorageRows />);
    expect(el.textContent).toBe("");
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
