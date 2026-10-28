import { Fragment, useEffect, useState } from "react";
import { groupNotesByDate, noteStatus, rowDateLabel, type LiveNoteState } from "../homeModel";
import type { NoteSummary, SearchHit } from "../types";
import { Avatar, Badge, Button, SectionLabel } from "./ui";
import { Mic, Plus, Search, Trash2 } from "lucide-react";

interface Props {
  notes: NoteSummary[];
  searchQuery: string;
  searchHits: SearchHit[];
  selectedNoteId: string | null;
  onSearchChange: (q: string) => void;
  onOpenNote: (id: string) => void;
  onNewNote: () => void;
  /** Deletes after an Undo window (App's deferred delete): no confirm. */
  onDeleteNote: (id: string) => void;
  /** Recording / transcribing / failed state for the status chips. */
  live: LiveNoteState;
}

/** Render a FTS snippet, highlighting the [[match]] markers. */
function Snippet({ text }: { text: string }) {
  const parts = text.split(/\[\[|\]\]/);
  return (
    <span>
      {parts.map((p, i) =>
        i % 2 === 1 ? (
          <mark
            key={i}
            className="rounded-[var(--radius-xs)] bg-highlight px-0.5 text-on-highlight"
          >
            {p}
          </mark>
        ) : (
          <span key={i}>{p}</span>
        ),
      )}
    </span>
  );
}

/** Shared drag source: dragging a note carries its id so a folder/scope can re-file it. */
function noteDragProps(id: string) {
  return {
    draggable: true,
    onDragStart: (e: React.DragEvent) => {
      e.dataTransfer.setData("text/plain", id);
      e.dataTransfer.effectAllowed = "move";
    },
    title: "Drag onto a folder to file it",
  };
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
  live,
}: Props) {
  // Clock for the date groups (Today rolls over at midnight), read once at
  // mount so rows paint on the first frame, then refreshed each minute.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(t);
  }, []);
  const searching = searchQuery.trim().length > 0;
  const scopeLabel = searching ? "Results" : "Notes";
  const visibleCount = searching ? searchHits.length : notes.length;

  return (
    <div className="print:hidden flex h-full w-72 flex-col border-r border-line bg-bg">
      {/* Search + actions */}
      <div className="flex flex-col gap-2 p-3">
        <div className="relative flex items-center">
          <span className="pointer-events-none absolute left-[11px] flex text-text-3">
            <Search size={15} strokeWidth={1.75} />
          </span>
          <input
            value={searchQuery}
            onChange={(e) => onSearchChange(e.target.value)}
            placeholder="Search notes & transcripts…"
            className="w-full rounded-[var(--radius-md)] border border-line bg-surface py-2 pr-3 pl-8 font-sans text-[13px] text-text outline-none placeholder:text-text-3 focus:border-primary"
          />
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={onNewNote}
          title="New note"
          startIcon={<Plus size={14} strokeWidth={1.75} />}
          className="w-full"
        >
          New note
        </Button>
      </div>

      {/* Scope + count */}
      <div className="flex items-center justify-between px-3.5 pb-1.5">
        <SectionLabel>{scopeLabel}</SectionLabel>
        <span className="text-[11px] text-text-3">{visibleCount}</span>
      </div>

      {/* Rows */}
      <div className="flex-1 overflow-y-auto px-2 pb-2">
        {searching ? (
          searchHits.length === 0 ? (
            <div className="px-3 py-6 text-center text-[13px] text-text-3">No matches</div>
          ) : (
            searchHits.map((hit, i) => {
              const active = selectedNoteId === hit.note_id;
              return (
                <div
                  key={`${hit.note_id}-${i}`}
                  onClick={() => onOpenNote(hit.note_id)}
                  className={`flex cursor-pointer items-start gap-2.5 rounded-[var(--radius-lg)] px-2.5 py-2 ${
                    active ? "bg-primary-soft" : "hover:bg-surface-3"
                  }`}
                >
                  <Avatar shape="square" colorKey={hit.note_id} size="md" name={hit.title} />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="min-w-0 truncate text-[14px] font-semibold text-text">
                        {hit.title}
                      </span>
                      {hit.kind === "transcript" && (
                        <Badge tone="primary" size="sm" uppercase className="flex-none">
                          transcript
                        </Badge>
                      )}
                    </div>
                    <div className="mt-0.5 truncate text-[12px] text-text-3">
                      <Snippet text={hit.snippet} />
                    </div>
                  </div>
                </div>
              );
            })
          )
        ) : notes.length === 0 ? (
          <div className="px-3 py-6 text-center text-[13px] text-text-3">
            No notes here yet.
            <br />
            Create one with “New note”.
          </div>
        ) : (
          groupNotesByDate(notes, new Date(now)).map((g) => (
            <Fragment key={g.label}>
              <SectionLabel as="div" className="px-2.5 pb-1 pt-2.5" role="heading" aria-level={3}>
                {g.label}
              </SectionLabel>
              {g.notes.map((n) => renderRow(n, new Date(now)))}
            </Fragment>
          ))
        )}
      </div>
    </div>
  );

  function renderRow(n: NoteSummary, nowDate: Date) {
    const active = selectedNoteId === n.id;
    const status = noteStatus(n, live);
    return (
      <div
        key={n.id}
        {...noteDragProps(n.id)}
        onClick={() => onOpenNote(n.id)}
        className={`group flex cursor-pointer items-center gap-2.5 rounded-[var(--radius-lg)] px-2.5 py-[9px] ${
          active ? "bg-primary-soft" : "hover:bg-surface-3"
        }`}
      >
        <Avatar shape="square" colorKey={n.id} size="md" name={n.title} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="flex min-w-0 flex-1 items-center gap-1.5">
              <span className="min-w-0 truncate text-[14px] font-semibold text-text">
                {n.title}
              </span>
              {n.meeting_id && (
                <Mic size={12} strokeWidth={1.75} className="flex-none text-text-3" />
              )}
            </span>
            {/* Invisible until hover or keyboard focus, but always in the tab
                order. Not offered on the note that's recording (deleting it
                would purge the files being written). */}
            {live.recordingNoteId !== n.id && (
              <button
                type="button"
                title="Delete note"
                aria-label={`Delete note “${n.title}”`}
                className="inline-flex flex-none cursor-pointer rounded-[var(--radius-sm)] p-1 text-text-3 opacity-0 hover:bg-surface-3 hover:text-rec focus-visible:opacity-100 group-hover:opacity-100"
                onClick={(e) => {
                  e.stopPropagation();
                  onDeleteNote(n.id);
                }}
              >
                <Trash2 size={13} strokeWidth={1.75} />
              </button>
            )}
          </div>
          <div className="mt-0.5 flex items-center justify-between gap-2">
            {/* happened_at fallback: pre-feature summaries in the localStorage cache */}
            <span className="min-w-0 truncate text-[12px] text-text-3">
              {rowDateLabel(n.happened_at ?? n.updated_at, nowDate)}
            </span>
            {status && (
              <Badge tone={status.tone} size="sm" className="flex-none">
                {status.label}
              </Badge>
            )}
          </div>
        </div>
      </div>
    );
  }
}
