import { useMemo, useState } from "react";
import type { Folder } from "../types";

export type Selection = { view: "all" } | { view: "unfiled" } | { view: "folder"; id: string };

interface Props {
  folders: Folder[];
  selection: Selection;
  onSelect: (sel: Selection) => void;
  onCreateFolder: (name: string, parentId: string | null) => void;
  onRenameFolder: (id: string, name: string) => void;
  onDeleteFolder: (id: string) => void;
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

export default function Sidebar({
  folders,
  selection,
  onSelect,
  onCreateFolder,
  onRenameFolder,
  onDeleteFolder,
}: Props) {
  const tree = useMemo(() => buildTree(folders), [folders]);
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
          className={`group flex cursor-pointer items-center gap-1 rounded px-2 py-1 text-sm ${
            selected ? "bg-indigo-600/30 text-indigo-200" : "text-zinc-300 hover:bg-zinc-800"
          }`}
          style={{ paddingLeft: `${8 + depth * 14}px` }}
          onClick={() => onSelect({ view: "folder", id: folder.id })}
        >
          <span className="text-zinc-500">▸</span>
          {renaming?.id === folder.id ? (
            <input
              autoFocus
              className="w-full rounded bg-zinc-800 px-1 text-sm text-zinc-100 outline-none"
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
            className="hidden text-zinc-500 hover:text-zinc-200 group-hover:inline"
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
            className="hidden text-zinc-500 hover:text-red-400 group-hover:inline"
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
            className="mt-0.5 w-[85%] rounded bg-zinc-800 px-2 py-0.5 text-sm text-zinc-100 outline-none"
            style={{ marginLeft: `${22 + depth * 14}px` }}
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
    <div className="flex h-full w-60 flex-col border-r border-zinc-800 bg-zinc-950">
      <div className="px-4 pb-2 pt-4 text-lg font-semibold tracking-tight text-zinc-100">Looma</div>
      <nav className="flex-1 overflow-y-auto px-2 pb-2">
        <div
          className={`cursor-pointer rounded px-2 py-1 text-sm ${
            selection.view === "all"
              ? "bg-indigo-600/30 text-indigo-200"
              : "text-zinc-300 hover:bg-zinc-800"
          }`}
          onClick={() => onSelect({ view: "all" })}
        >
          All notes
        </div>
        <div
          className={`cursor-pointer rounded px-2 py-1 text-sm ${
            selection.view === "unfiled"
              ? "bg-indigo-600/30 text-indigo-200"
              : "text-zinc-300 hover:bg-zinc-800"
          }`}
          onClick={() => onSelect({ view: "unfiled" })}
        >
          Unfiled
        </div>
        <div className="mt-3 flex items-center justify-between px-2 text-xs uppercase tracking-wide text-zinc-500">
          Folders
          <button
            title="New folder"
            className="text-zinc-500 hover:text-zinc-200"
            onClick={() => {
              setNewFolderParent(null);
              setNewFolderName("");
            }}
          >
            +
          </button>
        </div>
        {newFolderParent === null && (
          <input
            autoFocus
            placeholder="Folder name"
            className="mx-2 mt-1 w-[85%] rounded bg-zinc-800 px-2 py-0.5 text-sm text-zinc-100 outline-none"
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
    </div>
  );
}
