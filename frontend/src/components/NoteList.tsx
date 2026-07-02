import type { NoteSummary, SearchHit } from "../types";
import { Btn, speakerColor, speakerInitials } from "./ui";

interface Props {
  notes: NoteSummary[];
  searchQuery: string;
  searchHits: SearchHit[];
  selectedNoteId: string | null;
  onSearchChange: (q: string) => void;
  onOpenNote: (id: string) => void;
  onNewNote: () => void;
  onDeleteNote: (id: string) => void;
  onImport: () => void;
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

/** Stable per-note hash so each note keeps its chip color across renders. */
function hashId(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) | 0;
  return Math.abs(h);
}

/** Render a FTS snippet, highlighting the [[match]] markers. */
function Snippet({ text }: { text: string }) {
  const parts = text.split(/\[\[|\]\]/);
  return (
    <span>
      {parts.map((p, i) =>
        i % 2 === 1 ? (
          <mark key={i} className="rounded-sm bg-peach px-0.5 text-clay">
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
  onImport,
}: Props) {
  const searching = searchQuery.trim().length > 0;

  return (
    <div className="print:hidden flex h-full w-72 flex-col border-r border-line bg-cream">
      <div className="flex flex-col gap-2 p-3">
        <input
          value={searchQuery}
          onChange={(e) => onSearchChange(e.target.value)}
          placeholder="Search notes…"
          className="w-full rounded-[9px] border border-line bg-surface px-3 py-1.5 text-[13px] text-ink outline-none placeholder:text-mute focus:border-coral"
        />
        <div className="flex items-center gap-2">
          <Btn
            variant="outline"
            size="sm"
            onClick={onNewNote}
            title="New note"
            className="flex-1 text-ink"
          >
            <span className="h-2 w-2 rounded-[2px] bg-coral" /> New note
          </Btn>
          <Btn
            variant="ghost"
            size="sm"
            onClick={onImport}
            title="Import an audio/video file and transcribe it"
          >
            Import
          </Btn>
        </div>
      </div>
      <div className="flex-1 overflow-y-auto px-2 pb-2">
        {searching ? (
          searchHits.length === 0 ? (
            <div className="px-3 py-6 text-center text-[13px] text-mute">No matches</div>
          ) : (
            searchHits.map((hit, i) => (
              <div
                key={`${hit.note_id}-${i}`}
                onClick={() => onOpenNote(hit.note_id)}
                className="cursor-pointer rounded-[11px] px-3 py-2 hover:bg-peach-2"
              >
                <div className="flex items-center gap-2">
                  <span className="truncate text-[14px] font-semibold text-ink">{hit.title}</span>
                  {hit.kind === "transcript" && (
                    <span className="flex-none rounded bg-peach px-1 text-[10px] font-semibold uppercase tracking-wide text-clay">
                      transcript
                    </span>
                  )}
                </div>
                <div className="mt-0.5 truncate text-[12px] text-mute">
                  <Snippet text={hit.snippet} />
                </div>
              </div>
            ))
          )
        ) : notes.length === 0 ? (
          <div className="px-3 py-6 text-center text-[13px] text-mute">
            No notes here yet.
            <br />
            Create one with “New note”.
          </div>
        ) : (
          notes.map((n) => (
            <div
              key={n.id}
              onClick={() => onOpenNote(n.id)}
              className={`group flex cursor-pointer items-center gap-2.5 rounded-[11px] px-3 py-2.5 ${
                selectedNoteId === n.id ? "bg-peach" : "hover:bg-peach-2"
              }`}
            >
              <span
                className="flex h-[34px] w-[34px] flex-none items-center justify-center rounded-[10px] text-[12px] font-semibold text-white"
                style={{ background: speakerColor("", hashId(n.id)) }}
              >
                {speakerInitials(n.title)}
              </span>
              <div className="min-w-0 flex-1">
                <div className="flex items-center justify-between gap-2">
                  <span className="truncate text-[14px] font-semibold text-ink">{n.title}</span>
                  <button
                    title="Delete note"
                    className="hidden flex-none cursor-pointer rounded-md px-1.5 py-0.5 text-[11px] text-mute hover:bg-peach-2 hover:text-rec group-hover:inline-flex"
                    onClick={(e) => {
                      e.stopPropagation();
                      if (confirm(`Delete note "${n.title}"?`)) onDeleteNote(n.id);
                    }}
                  >
                    ✕
                  </button>
                </div>
                <div className="text-[12px] text-mute">{relTime(n.updated_at)}</div>
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
