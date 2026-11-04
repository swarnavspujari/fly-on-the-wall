// @vitest-environment jsdom
import "../devMock";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import BackupRow from "./BackupRow";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;
const mount = async () => {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<BackupRow />));
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

describe("Settings › Backups row", () => {
  it("shows the count and opens the folder", async () => {
    localStorage.setItem("fotwMockBackups", "3");
    const reveal = vi.spyOn(api, "revealBackupsDir");
    const el = await mount();
    expect(el.textContent).toContain("Backups");
    expect(el.querySelector('[data-testid="backup-count"]')!.textContent).toBe("3");
    expect(el.textContent).toContain("The newest 3 are kept");
    const open = [...el.querySelectorAll("button")].find((b) => b.textContent === "Open folder")!;
    act(() => open.click());
    expect(reveal).toHaveBeenCalledTimes(1);
  });

  it("with none yet, says when one is made", async () => {
    localStorage.setItem("fotwMockBackups", "0");
    const el = await mount();
    expect(el.querySelector('[data-testid="backup-count"]')!.textContent).toBe("0");
    expect(el.textContent).toContain("saves a copy of it here");
  });

  it("renders nothing until the status arrives (or if it never does)", async () => {
    vi.spyOn(api, "backupStatus").mockReturnValue(new Promise(() => {}));
    const el = await mount();
    expect(el.textContent).toBe("");
  });
});
