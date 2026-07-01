import type { NoteSummary, SearchHit } from "../types";

interface Props {
  notes: NoteSummary[];
  searchQuery: string;
  searchHits: SearchHit[];
  selectedNoteId: string | null;
  onSearchChange: (q: string) => void;
  onOpenNote: (id: string) => void;
  onNewNote: () => void;
  onDeleteNote: (id: string) => void;
}

function relTime(iso: string): string {
  const then = new Date(iso).getTime();
  const mins = Math.floor((Date.now() - then) / 60_000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d ago`;
  return new Date(iso).toLocaleDateString();
}

/** Render a FTS snippet, highlighting the [[match]] markers. */
function Snippet({ text }: { text: string }) {
  const parts = text.split(/\[\[|\]\]/);
  return (
    <span>
      {parts.map((p, i) =>
        i % 2 === 1 ? (
          <mark key={i} className="rounded bg-amber-400/30 px-0.5 text-amber-200">
            {p}
          </mark>
        ) : (
          <span key={i}>{p}</span>
        ),
      )}
    </span>
  );
}

export default function NoteList({
  notes,
  searchQuery,
  searchHits,
  selectedNoteId,
  onSearchChange,
  onOpenNote,
  onNewNote,
  onDeleteNote,
}: Props) {
  const searching = searchQuery.trim().length > 0;

  return (
    <div className="flex h-full w-72 flex-col border-r border-zinc-800 bg-zinc-900/60">
      <div className="flex items-center gap-2 p-3">
        <input
          value={searchQuery}
          onChange={(e) => onSearchChange(e.target.value)}
          placeholder="Search notes…"
          className="w-full rounded-md border border-zinc-800 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-200 outline-none placeholder:text-zinc-600 focus:border-indigo-500"
        />
        <button
          onClick={onNewNote}
          title="New note"
          className="rounded-md bg-indigo-600 px-2.5 py-1.5 text-sm font-medium text-white hover:bg-indigo-500"
        >
          +
        </button>
      </div>
      <div className="flex-1 overflow-y-auto px-2 pb-2">
        {searching ? (
          searchHits.length === 0 ? (
            <div className="px-3 py-6 text-center text-sm text-zinc-500">No matches</div>
          ) : (
            searchHits.map((hit, i) => (
              <div
                key={`${hit.note_id}-${i}`}
                onClick={() => onOpenNote(hit.note_id)}
                className="cursor-pointer rounded-md px-3 py-2 hover:bg-zinc-800"
              >
                <div className="flex items-center gap-2">
                  <span className="truncate text-sm font-medium text-zinc-200">{hit.title}</span>
                  {hit.kind === "transcript" && (
                    <span className="rounded bg-zinc-700 px-1 text-[10px] uppercase text-zinc-300">
                      transcript
                    </span>
                  )}
                </div>
                <div className="mt-0.5 truncate text-xs text-zinc-500">
                  <Snippet text={hit.snippet} />
                </div>
              </div>
            ))
          )
        ) : notes.length === 0 ? (
          <div className="px-3 py-6 text-center text-sm text-zinc-500">
            No notes here yet.
            <br />
            Create one with “+”.
          </div>
        ) : (
          notes.map((n) => (
            <div
              key={n.id}
              onClick={() => onOpenNote(n.id)}
              className={`group cursor-pointer rounded-md px-3 py-2 ${
                selectedNoteId === n.id ? "bg-zinc-800" : "hover:bg-zinc-800/60"
              }`}
            >
              <div className="flex items-center justify-between gap-2">
                <span className="truncate text-sm font-medium text-zinc-200">{n.title}</span>
                <button
                  title="Delete note"
                  className="hidden text-xs text-zinc-500 hover:text-red-400 group-hover:inline"
                  onClick={(e) => {
                    e.stopPropagation();
                    if (confirm(`Delete note "${n.title}"?`)) onDeleteNote(n.id);
                  }}
                >
                  ✕
                </button>
              </div>
              <div className="text-xs text-zinc-500">{relTime(n.updated_at)}</div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
