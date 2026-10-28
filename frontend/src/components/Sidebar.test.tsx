// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { Folder } from "../types";
import { ConfirmProvider } from "./confirm";
import { confirmChoice, confirmDialog, confirmTitle } from "./confirmTesting";
import Sidebar from "./Sidebar";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const folders: Folder[] = [
  { id: "f1", name: "Projects", parent_id: null, created_at: "2026-10-01T00:00:00Z" },
];

let root: Root | null = null;
let host: HTMLElement | null = null;

beforeEach(() => {
  vi.spyOn(api, "listLocalOnlyFolders").mockResolvedValue([]);
  vi.spyOn(api, "setFolderKeepLocal").mockResolvedValue(undefined);
});

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.restoreAllMocks();
});

const flush = async () => {
  for (let i = 0; i < 3; i++) await act(async () => {});
};

async function render(onDeleteFolder: (id: string) => void = () => {}) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <ConfirmProvider>
        <Sidebar
          folders={folders}
          upcoming={[]}
          selection={{ view: "all" }}
          onSelect={() => {}}
          onCreateFolder={() => {}}
          onRenameFolder={() => {}}
          onDeleteFolder={onDeleteFolder}
          onStartFromEvent={() => {}}
          onImportMedia={() => {}}
          onOpenSettings={() => {}}
        />
      </ConfirmProvider>,
    ),
  );
  await flush();
}

const byTitle = (title: string) => host!.querySelector<HTMLElement>(`button[title="${title}"]`)!;
const keepLocal = () => host!.querySelector<HTMLElement>('button[aria-label="Keep folder local"]');

describe("Sidebar › Delete folder", () => {
  it("asks first, and Cancel keeps the folder", async () => {
    const onDelete = vi.fn();
    await render(onDelete);
    await act(async () => byTitle("Delete folder").click());
    expect(confirmTitle()).toBe("Delete folder “Projects”?");
    expect(confirmDialog()!.dataset.tone).toBe("danger");
    expect(onDelete).not.toHaveBeenCalled(); // nothing happens while it asks
    await act(async () => confirmChoice(false).click());
    await flush();
    expect(onDelete).not.toHaveBeenCalled();
  });

  it("deletes once you confirm", async () => {
    const onDelete = vi.fn();
    await render(onDelete);
    await act(async () => byTitle("Delete folder").click());
    await act(async () => confirmChoice(true).click());
    await flush();
    expect(onDelete).toHaveBeenCalledWith("f1");
  });
});

describe("Sidebar › Keep folder local", () => {
  it("asks first, and Cancel changes nothing", async () => {
    await render();
    await act(async () => keepLocal()!.click());
    expect(confirmTitle()).toBe("Keep everything in “Projects” local?");
    expect(api.setFolderKeepLocal).not.toHaveBeenCalled();
    await act(async () => confirmChoice(false).click());
    await flush();
    expect(api.setFolderKeepLocal).not.toHaveBeenCalled();
    expect(keepLocal()).not.toBeNull(); // still not kept local
  });

  it("keeps it local once you confirm", async () => {
    await render();
    await act(async () => keepLocal()!.click());
    await act(async () => confirmChoice(true).click());
    await flush();
    expect(api.setFolderKeepLocal).toHaveBeenCalledWith("f1", true);
    expect(host!.querySelector('button[aria-label="Folder kept local"]')).not.toBeNull();
  });

  it("turning it off needs no question", async () => {
    vi.mocked(api.listLocalOnlyFolders).mockResolvedValue(["f1"]);
    await render();
    await act(async () =>
      host!.querySelector<HTMLElement>('button[aria-label="Folder kept local"]')!.click(),
    );
    await flush();
    expect(confirmDialog()).toBeNull();
    expect(api.setFolderKeepLocal).toHaveBeenCalledWith("f1", false);
  });
});
