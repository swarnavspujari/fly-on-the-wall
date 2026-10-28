import { useCallback, useEffect, useRef, useState } from "react";
import { UNDO_DELETE_MS, useDeferredDelete } from "./deferredDelete";

export interface DeleteItem {
  id: string;
  title: string;
  /** It was open in the editor when deleted (Undo may reopen it). */
  wasOpen: boolean;
}

interface Options {
  /** The delete command (api.deleteNote). */
  remove: (id: string) => Promise<void>;
  /** Re-read the note list. */
  refresh: () => Promise<void>;
  /** The delete went through: close the editor if it shows this note, drop
   *  its held notices. */
  onDeleted: (id: string) => void;
  onError: (message: string) => void;
  delayMs?: number;
}

export interface NoteDeletion {
  /** Notes waiting on the Undo toast, oldest first. */
  pending: DeleteItem[];
  /** Every note the user deleted this session that hasn't come back: lists,
   *  search hits and notices filter these out. */
  hiddenIds: ReadonlySet<string>;
  /** Same as `hiddenIds`, read at call time: for callbacks that may be stale
   *  (a late editor save, an in-flight open). */
  isHidden: (id: string) => boolean;
  request: (item: DeleteItem) => void;
  /** Restore every waiting note; returns them. */
  undo: () => DeleteItem[];
  /** Delete the waiting notes now. */
  flush: () => void;
}

/** Note deletion for App: deferred behind the Undo toast (deferredDelete.ts),
 *  and hidden from the moment it's requested until after the delete AND the
 *  list refresh land, so the row never flashes back. A failed delete
 *  restores the note and reports the error. */
export function useNoteDeletion({
  remove,
  refresh,
  onDeleted,
  onError,
  delayMs = UNDO_DELETE_MS,
}: Options): NoteDeletion {
  const [hiddenIds, setHiddenIds] = useState<ReadonlySet<string>>(() => new Set());
  const hiddenRef = useRef(new Set<string>());
  const setHidden = useCallback((ids: string[], hidden: boolean) => {
    const next = new Set(hiddenRef.current);
    for (const id of ids) {
      if (hidden) next.add(id);
      else next.delete(id);
    }
    hiddenRef.current = next;
    setHiddenIds(next);
  }, []);

  const opts = useRef({ remove, refresh, onDeleted, onError });
  useEffect(() => {
    opts.current = { remove, refresh, onDeleted, onError };
  }, [remove, refresh, onDeleted, onError]);

  const commit = useCallback(
    (item: DeleteItem) => {
      const { remove, refresh, onDeleted, onError } = opts.current;
      remove(item.id)
        .then(() => {
          // Stays hidden: a deleted id never comes back.
          onDeleted(item.id);
          return refresh();
        })
        .catch((e) => {
          setHidden([item.id], false);
          onError(`Couldn’t delete “${item.title}”: ${String(e)}`);
          return refresh();
        })
        .catch(console.error);
    },
    [setHidden],
  );

  const deferred = useDeferredDelete(commit, delayMs);
  const { schedule, undo: takePending, flush } = deferred;

  const request = useCallback(
    (item: DeleteItem) => {
      setHidden([item.id], true);
      schedule(item);
    },
    [setHidden, schedule],
  );

  const undo = useCallback(() => {
    const items = takePending();
    setHidden(
      items.map((i) => i.id),
      false,
    );
    return items;
  }, [takePending, setHidden]);

  const isHidden = useCallback((id: string) => hiddenRef.current.has(id), []);

  return { pending: deferred.pending, hiddenIds, isHidden, request, undo, flush };
}
