import { X } from "lucide-react";
import { Button } from "./ui";

interface Props {
  /** What was deleted, e.g. the note title. */
  title: string;
  onUndo: () => void;
  /** Close the toast now: the delete runs right away. */
  onDismiss: () => void;
}

/** "Deleted … · Undo" after a note delete (bottom-left, clear of the notice
 *  stack in the bottom-right). The delete itself waits for the toast to
 *  expire (deferredDelete.ts), so Undo restores the note untouched. */
export default function UndoToast({ title, onUndo, onDismiss }: Props) {
  return (
    <div
      role="status"
      className="print:hidden fixed bottom-4 left-4 z-50 flex max-w-[min(420px,calc(100vw-32px))] items-center gap-3 rounded-xl border border-line bg-surface py-2 pl-3.5 pr-2 text-[13px] text-text"
      style={{ boxShadow: "var(--shadow-lg)" }}
    >
      <span className="min-w-0 flex-1 truncate">
        Deleted <b className="font-semibold">“{title}”</b>
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
