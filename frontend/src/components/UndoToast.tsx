import { X } from "lucide-react";
import { Button } from "./ui";

interface Props {
  /** Titles of the notes waiting to be deleted, oldest first. */
  titles: string[];
  /** Restore every waiting note. */
  onUndo: () => void;
  /** Close the toast now: the deletes run right away. */
  onDismiss: () => void;
}

/** "Deleted … · Undo" after note deletes (bottom-left, clear of the notice
 *  stack in the bottom-right). The deletes wait for the toast to expire
 *  (noteDeletion.ts), so Undo restores the notes untouched; several deletes
 *  share one toast and one Undo. */
export default function UndoToast({ titles, onUndo, onDismiss }: Props) {
  const several = titles.length > 1;
  return (
    <div
      role="status"
      className="print:hidden fixed bottom-4 left-4 z-50 flex max-w-[min(420px,calc(100vw-32px))] items-center gap-3 rounded-xl border border-line bg-surface py-2 pl-3.5 pr-2 text-[13px] text-text"
      style={{ boxShadow: "var(--shadow-lg)" }}
    >
      <span className="min-w-0 flex-1 truncate">
        {several ? (
          <>
            Deleted <b className="font-semibold">{titles.length} notes</b>
          </>
        ) : (
          <>
            Deleted <b className="font-semibold">“{titles[0]}”</b>
          </>
        )}
      </span>
      <Button variant="soft" size="sm" onClick={onUndo}>
        Undo
      </Button>
      <button
        type="button"
        onClick={onDismiss}
        aria-label="Close and delete now"
        title="Close and delete now"
        className="flex-none cursor-pointer rounded-[var(--radius-sm)] p-1 text-text-3 hover:bg-surface-3 hover:text-text"
      >
        <X size={14} strokeWidth={2} />
      </button>
    </div>
  );
}
