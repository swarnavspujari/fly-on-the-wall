import { useEffect, useMemo, useState } from "react";
import { Inbox, List, Plus, Settings as SettingsIcon, X } from "lucide-react";
import type { CalendarEvent, Folder } from "../types";
import { Badge, Button, SectionLabel } from "./ui";
import logoLight from "../assets/brand/fly-on-the-wall-logo.svg";
import logoDark from "../assets/brand/fly-on-the-wall-logo-dark.svg";

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
  /** Resolved theme — picks the wordmark variant (dark logo on the ink shell). */
  theme?: "light" | "dark";
  /** File a dragged note into a folder (or null for All notes / Unfiled). */
  onMoveNote?: (noteId: string, folderId: string | null) => void;
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

const FOLDER_COLORS = [
  "var(--primary)",
  "var(--spk-teal)",
  "var(--spk-blue)",
  "var(--spk-pink)",
  "var(--spk-amber)",
  "var(--spk-green)",
  "var(--spk-indigo)",
];
const FOLDER_EMOJIS = [
  "📁",
  "📂",
  "🗂️",
  "💼",
  "🧑‍💻",
  "📊",
  "📈",
  "📌",
  "📎",
  "✅",
  "🎯",
  "🚀",
  "🔥",
  "⭐",
  "💡",
  "🧠",
  "📝",
  "🗓️",
  "👥",
  "🤝",
  "🏷️",
  "🔒",
  "🌐",
  "🧩",
  "🎨",
  "🛠️",
  "📣",
  "☕",
];

/** A folder's chosen dot color (falls back to a stable rotation color). */
function folderColor(meta: FolderMeta, id: string): string {
  return meta.color ?? FOLDER_COLORS[hashId(id) % FOLDER_COLORS.length];
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

type FolderMeta = { color?: string; emoji?: string };
const META_KEY = "fotw-folder-meta";

const ROW =
  "group flex cursor-pointer items-center gap-2 rounded-lg px-2.5 py-1.5 text-[13.5px] font-medium text-text";
const INPUT =
  "rounded-lg border border-line bg-surface px-2 py-1 text-[13px] text-text outline-none placeholder:text-text-3";

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
  theme = "light",
  onMoveNote,
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

  // frontend-only folder personalization (color + emoji), persisted locally
  const [meta, setMeta] = useState<Record<string, FolderMeta>>(() => {
    try {
      return JSON.parse(localStorage.getItem(META_KEY) || "{}");
    } catch {
      return {};
    }
  });
  const setFolderMeta = (id: string, patch: FolderMeta) =>
    setMeta((m) => {
      const next = { ...m, [id]: { ...m[id], ...patch } };
      try {
        localStorage.setItem(META_KEY, JSON.stringify(next));
      } catch {
        /* storage may be unavailable */
      }
      return next;
    });
  const [picker, setPicker] = useState<{ id: string; x: number; y: number } | null>(null);

  // drag-drop note filing: which target is currently hovered
  const [dropId, setDropId] = useState<string | null>(null);
  const dropProps = (targetFolder: string | null, key: string) =>
    onMoveNote
      ? {
          onDragOver: (e: React.DragEvent) => {
            e.preventDefault();
            e.dataTransfer.dropEffect = "move";
          },
          onDragEnter: (e: React.DragEvent) => {
            e.preventDefault();
            setDropId(key);
          },
          onDragLeave: (e: React.DragEvent) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node)) {
              setDropId((c) => (c === key ? null : c));
            }
          },
          onDrop: (e: React.DragEvent) => {
            e.preventDefault();
            const id = e.dataTransfer.getData("text/plain");
            if (id) onMoveNote(id, targetFolder);
            setDropId(null);
          },
        }
      : {};
  const dropRing =
    "outline outline-[1.5px] -outline-offset-1 outline-dashed outline-primary bg-primary-soft";

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
    const isDrop = dropId === folder.id;
    const fmeta = meta[folder.id] || {};
    return (
      <div key={folder.id}>
        <div
          className={`${ROW} ${selected ? "bg-primary-soft" : "hover:bg-surface-3"} ${isDrop ? dropRing : ""}`}
          style={{ paddingLeft: `${10 + depth * 14}px` }}
          onClick={() => onSelect({ view: "folder", id: folder.id })}
          {...dropProps(folder.id, folder.id)}
        >
          <button
            title="Folder color & emoji"
            className="grid h-[18px] w-[18px] flex-none cursor-pointer place-items-center"
            onClick={(e) => {
              e.stopPropagation();
              const r = e.currentTarget.getBoundingClientRect();
              setPicker((pk) =>
                pk && pk.id === folder.id ? null : { id: folder.id, x: r.left, y: r.bottom + 6 },
              );
            }}
          >
            {fmeta.emoji ? (
              <span className="text-[14px] leading-none">{fmeta.emoji}</span>
            ) : (
              <span
                className="h-[9px] w-[9px] rounded-full"
                style={{ background: folderColor(fmeta, folder.id) }}
              />
            )}
          </button>
          {renaming?.id === folder.id ? (
            <input
              autoFocus
              className="w-full rounded-md border border-line bg-surface px-1.5 text-[13px] text-text outline-none"
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
            className="hidden flex-none cursor-pointer rounded p-0.5 text-text-3 hover:text-text group-hover:inline-flex"
            onClick={(e) => {
              e.stopPropagation();
              setNewFolderParent(folder.id);
              setNewFolderName("");
            }}
          >
            <Plus size={14} strokeWidth={1.75} />
          </button>
          <button
            title="Delete folder"
            className="hidden flex-none cursor-pointer rounded p-0.5 text-text-3 hover:text-rec group-hover:inline-flex"
            onClick={(e) => {
              e.stopPropagation();
              if (confirm(`Delete folder "${folder.name}"? Notes inside become unfiled.`)) {
                onDeleteFolder(folder.id);
              }
            }}
          >
            <X size={14} strokeWidth={2} />
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
      <div className="flex items-center px-4 pb-2.5 pt-4">
        <img
          src={theme === "dark" ? logoDark : logoLight}
          alt="Fly on the Wall"
          className="block h-auto w-[168px] select-none"
          draggable={false}
        />
      </div>
      <nav className="flex-1 overflow-y-auto px-3 pb-3">
        <div
          className={`${ROW} ${selection.view === "all" ? "bg-primary-soft" : "hover:bg-surface-3"} ${
            dropId === "all" ? dropRing : ""
          }`}
          onClick={() => onSelect({ view: "all" })}
          {...dropProps(null, "all")}
        >
          <List size={15} strokeWidth={1.75} className="flex-none text-text-3" />
          All notes
        </div>
        <div
          className={`${ROW} ${selection.view === "unfiled" ? "bg-primary-soft" : "hover:bg-surface-3"} ${
            dropId === "unfiled" ? dropRing : ""
          }`}
          onClick={() => onSelect({ view: "unfiled" })}
          {...dropProps(null, "unfiled")}
        >
          <Inbox size={15} strokeWidth={1.75} className="flex-none text-text-3" />
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
                  className="group mt-0.5 rounded-lg px-2.5 py-1.5 hover:bg-surface-3"
                >
                  <div className="flex items-center gap-1.5">
                    {live && (
                      <Badge tone="live" size="sm" uppercase>
                        Live
                      </Badge>
                    )}
                    <span className="truncate text-[13px] font-semibold text-text">{ev.title}</span>
                  </div>
                  <div className="flex items-center justify-between gap-1.5">
                    <span className="truncate text-[11.5px] text-text-3">
                      {eventTime(ev.start)}–{eventTime(ev.end)} ·{" "}
                      {ev.provider === "google" ? "Google" : "Outlook"}
                    </span>
                    <span className="hidden flex-none group-hover:inline-flex">
                      <Button
                        variant="soft"
                        size="xs"
                        title="Start note + recording for this meeting"
                        onClick={() => onStartFromEvent(ev)}
                      >
                        Start
                      </Button>
                    </span>
                  </div>
                </div>
              );
            })}
          </>
        )}
        <div className="mt-4 flex items-center justify-between pb-1 pl-2.5 pr-1">
          <SectionLabel>Folders</SectionLabel>
          <Button
            variant="ghost"
            size="xs"
            title="New folder"
            onClick={() => {
              setNewFolderParent(null);
              setNewFolderName("");
            }}
          >
            <Plus size={14} strokeWidth={1.75} />
          </Button>
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
        className="flex cursor-pointer items-center gap-2.5 border-t border-line px-4 py-2.5 text-left text-[13px] font-medium text-text-2 hover:bg-surface-3 hover:text-text"
      >
        <SettingsIcon size={15} strokeWidth={1.75} className="flex-none" />
        Settings
      </button>

      {/* folder color + emoji picker popover (fixed so the sidebar's overflow can't clip it) */}
      {picker && (
        <>
          <div className="fixed inset-0 z-50" onClick={() => setPicker(null)} />
          <div
            className="fixed z-[51] w-[214px] rounded-xl border border-line bg-surface p-2.5 shadow-pop"
            style={{ left: picker.x, top: picker.y, boxShadow: "var(--shadow-pop)" }}
          >
            <div className="mb-1.5 text-[10.5px] font-bold uppercase tracking-[0.06em] text-text-3">
              Color
            </div>
            <div className="mb-2.5 flex flex-wrap gap-1.5">
              {FOLDER_COLORS.map((c) => {
                const on = (meta[picker.id] || {}).color === c;
                return (
                  <button
                    key={c}
                    title="Set color"
                    onClick={() => setFolderMeta(picker.id, { color: c })}
                    className="h-5 w-5 cursor-pointer rounded-full"
                    style={{
                      background: c,
                      border: on ? "2px solid var(--text)" : "2px solid transparent",
                      boxShadow: "0 0 0 1px var(--line)",
                    }}
                  />
                );
              })}
            </div>
            <div className="mb-1.5 flex items-center justify-between">
              <span className="text-[10.5px] font-bold uppercase tracking-[0.06em] text-text-3">
                Emoji
              </span>
              <button
                className="cursor-pointer text-[11px] font-semibold text-primary-text"
                onClick={() => setFolderMeta(picker.id, { emoji: undefined })}
              >
                Clear
              </button>
            </div>
            <div className="grid max-h-[132px] grid-cols-7 gap-0.5 overflow-y-auto">
              {FOLDER_EMOJIS.map((e) => (
                <button
                  key={e}
                  title={`Use ${e}`}
                  onClick={() => setFolderMeta(picker.id, { emoji: e })}
                  className="cursor-pointer rounded-md py-1 text-[16px] leading-none hover:bg-surface-3"
                >
                  {e}
                </button>
              ))}
            </div>
          </div>
        </>
      )}
    </div>
  );
}
