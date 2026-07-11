import { useEffect, useMemo, useState } from "react";
import type { CalendarEvent, Folder } from "../types";
import { Btn, SectionLabel, speakerColor } from "./ui";

export type Selection = { view: "all" } | { view: "unfiled" } | { view: "folder"; id: string };

interface Props {
  folders: Folder[];
  upcoming: CalendarEvent[];
  selection: Selection;
  onSelect: (sel: Selection) => void;
  onCreateFolder: (name: string, parentId: string | null) => void;
  onRenameFolder: (id: string, name: string) => void;
  onDeleteFolder: (id: string) => void;
  onStartFromEvent: (ev: CalendarEvent) => void;
  onOpenSettings: () => void;
}

function eventTime(iso: string): string {
  return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** Stable rotation index so each folder keeps the same dot color across renders. */
function hashId(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) | 0;
  return Math.abs(h);
}

interface TreeNode {
  folder: Folder;
  children: TreeNode[];
}

function buildTree(folders: Folder[]): TreeNode[] {
  const byParent = new Map<string | null, Folder[]>();
  for (const f of folders) {
    const key = f.parent_id;
    const list = byParent.get(key) ?? [];
    list.push(f);
    byParent.set(key, list);
  }
  const build = (parentId: string | null): TreeNode[] =>
    (byParent.get(parentId) ?? []).map((folder) => ({
      folder,
      children: build(folder.id),
    }));
  return build(null);
}

const ROW =
  "flex cursor-pointer items-center gap-2 rounded-lg px-2.5 py-1.5 text-[13.5px] font-medium text-ink";
const INPUT =
  "rounded-lg border border-line bg-surface px-2 py-1 text-[13px] text-ink outline-none placeholder:text-mute";

export default function Sidebar({
  folders,
  upcoming,
  selection,
  onSelect,
  onCreateFolder,
  onRenameFolder,
  onDeleteFolder,
  onStartFromEvent,
  onOpenSettings,
}: Props) {
  const tree = useMemo(() => buildTree(folders), [folders]);
  // clock for the LIVE badge, refreshed each minute (render must stay pure)
  const [now, setNow] = useState(0);
  useEffect(() => {
    setNow(Date.now());
    const t = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(t);
  }, []);
  const [newFolderParent, setNewFolderParent] = useState<string | null | "none">("none");
  const [newFolderName, setNewFolderName] = useState("");
  const [renaming, setRenaming] = useState<{ id: string; name: string } | null>(null);

  const submitNewFolder = () => {
    const name = newFolderName.trim();
    if (name && newFolderParent !== "none") {
      onCreateFolder(name, newFolderParent);
    }
    setNewFolderParent("none");
    setNewFolderName("");
  };

  const renderNode = (node: TreeNode, depth: number) => {
    const { folder, children } = node;
    const selected = selection.view === "folder" && selection.id === folder.id;
    return (
      <div key={folder.id}>
        <div
          className={`group ${ROW} ${selected ? "bg-peach" : "hover:bg-peach-2"}`}
          style={{ paddingLeft: `${10 + depth * 14}px` }}
          onClick={() => onSelect({ view: "folder", id: folder.id })}
        >
          <span
            className="h-[9px] w-[9px] flex-none rounded-full"
            style={{ background: speakerColor("", hashId(folder.id)) }}
          />
          {renaming?.id === folder.id ? (
            <input
              autoFocus
              className="w-full rounded-md border border-line bg-surface px-1.5 text-[13px] text-ink outline-none"
              value={renaming.name}
              onChange={(e) => setRenaming({ id: folder.id, name: e.target.value })}
              onBlur={() => {
                if (renaming.name.trim()) onRenameFolder(folder.id, renaming.name.trim());
                setRenaming(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
                if (e.key === "Escape") setRenaming(null);
              }}
              onClick={(e) => e.stopPropagation()}
            />
          ) : (
            <span
              className="flex-1 truncate"
              onDoubleClick={(e) => {
                e.stopPropagation();
                setRenaming({ id: folder.id, name: folder.name });
              }}
            >
              {folder.name}
            </span>
          )}
          <button
            title="New subfolder"
            className="hidden cursor-pointer text-mute hover:text-ink group-hover:inline"
            onClick={(e) => {
              e.stopPropagation();
              setNewFolderParent(folder.id);
              setNewFolderName("");
            }}
          >
            +
          </button>
          <button
            title="Delete folder"
            className="hidden cursor-pointer text-mute hover:text-rec group-hover:inline"
            onClick={(e) => {
              e.stopPropagation();
              if (confirm(`Delete folder "${folder.name}"? Notes inside become unfiled.`)) {
                onDeleteFolder(folder.id);
              }
            }}
          >
            ✕
          </button>
        </div>
        {newFolderParent === folder.id && (
          <input
            autoFocus
            placeholder="Subfolder name"
            className={`mt-0.5 w-[85%] ${INPUT}`}
            style={{ marginLeft: `${27 + depth * 14}px` }}
            value={newFolderName}
            onChange={(e) => setNewFolderName(e.target.value)}
            onBlur={submitNewFolder}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
              if (e.key === "Escape") setNewFolderParent("none");
            }}
          />
        )}
        {children.map((c) => renderNode(c, depth + 1))}
      </div>
    );
  };

  return (
    <div className="print:hidden flex h-full w-60 flex-col border-r border-line bg-shell">
      <div className="flex items-center gap-2.5 px-4 pb-2.5 pt-3.5">
        <span className="flex h-[30px] w-[30px] flex-none items-center justify-center rounded-[9px] bg-coral">
          <span className="h-3.5 w-3.5 rounded-full border-[3px] border-white" />
        </span>
        <span className="font-display text-[19px] font-bold tracking-tight text-ink">Fly on the Wall</span>
      </div>
      <nav className="flex-1 overflow-y-auto px-3 pb-3">
        <div
          className={`${ROW} ${selection.view === "all" ? "bg-peach" : "hover:bg-peach-2"}`}
          onClick={() => onSelect({ view: "all" })}
        >
          <span className="h-3 w-3 flex-none rounded bg-mute/55" />
          All notes
        </div>
        <div
          className={`${ROW} ${selection.view === "unfiled" ? "bg-peach" : "hover:bg-peach-2"}`}
          onClick={() => onSelect({ view: "unfiled" })}
        >
          <span className="h-3 w-3 flex-none rounded border-[1.5px] border-mute/70" />
          Unfiled
        </div>
        {upcoming.length > 0 && (
          <>
            <SectionLabel className="mt-4 px-2.5 pb-1.5">Up next</SectionLabel>
            {upcoming.map((ev) => {
              const live =
                now > 0 && new Date(ev.start).getTime() <= now && now <= new Date(ev.end).getTime();
              return (
                <div
                  key={`${ev.provider}-${ev.id}`}
                  className="group mt-0.5 rounded-lg px-2.5 py-1.5 hover:bg-peach-2"
                >
                  <div className="flex items-center gap-1.5">
                    {live && (
                      <span className="flex-none rounded bg-rec px-1 py-px text-[9.5px] font-semibold leading-[14px] text-white">
                        LIVE
                      </span>
                    )}
                    <span className="truncate text-[13px] font-semibold text-ink">{ev.title}</span>
                  </div>
                  <div className="flex items-center justify-between gap-1.5">
                    <span className="truncate text-[11.5px] text-mute">
                      {eventTime(ev.start)}–{eventTime(ev.end)} ·{" "}
                      {ev.provider === "google" ? "Google" : "Outlook"}
                    </span>
                    <span className="hidden flex-none group-hover:inline-flex">
                      <Btn
                        variant="soft"
                        size="xs"
                        title="Start note + recording for this meeting"
                        onClick={() => onStartFromEvent(ev)}
                      >
                        Start
                      </Btn>
                    </span>
                  </div>
                </div>
              );
            })}
          </>
        )}
        <div className="mt-4 flex items-center justify-between pb-1 pl-2.5 pr-1">
          <SectionLabel>Folders</SectionLabel>
          <Btn
            variant="ghost"
            size="xs"
            title="New folder"
            onClick={() => {
              setNewFolderParent(null);
              setNewFolderName("");
            }}
          >
            +
          </Btn>
        </div>
        {newFolderParent === null && (
          <input
            autoFocus
            placeholder="Folder name"
            className={`mx-2.5 mt-1 w-[85%] ${INPUT}`}
            value={newFolderName}
            onChange={(e) => setNewFolderName(e.target.value)}
            onBlur={submitNewFolder}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
              if (e.key === "Escape") setNewFolderParent("none");
            }}
          />
        )}
        <div className="mt-1">{tree.map((n) => renderNode(n, 0))}</div>
      </nav>
      <button
        onClick={onOpenSettings}
        className="flex cursor-pointer items-center gap-2.5 border-t border-line px-4 py-2.5 text-left text-[13px] font-medium text-ink-2 hover:bg-peach-2 hover:text-ink"
      >
        <span className="h-3 w-3 flex-none rounded-full border-2 border-mute/60" />
        Settings
      </button>
    </div>
  );
}
