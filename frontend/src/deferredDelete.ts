import { useCallback, useEffect, useRef, useState } from "react";

function clearTimer(t: { current: number | null }) {
  if (t.current != null) window.clearTimeout(t.current);
  t.current = null;
}

/** How long the Undo toast stays before the delete runs. */
export const UNDO_DELETE_MS = 8_000;

export interface DeferredDelete<T> {
  /** Items waiting to be deleted, oldest first (the toast shows while any). */
  pending: T[];
  /** Hide it now; delete when the toast expires. A delete while others wait
   *  joins them behind the same toast and restarts its timer, so nothing is
   *  deleted before the user had the full Undo window for it. */
  schedule: (item: T) => void;
  /** Cancel every waiting delete; returns the items so the caller can
   *  restore them. */
  undo: () => T[];
  /** Run the waiting deletes now (the toast's close button). */
  flush: () => void;
}

/** Undo toast = deferred delete (spec §3.2.5): nothing is deleted until the
 *  toast expires or is closed, so Undo needs no soft-delete schema. If the
 *  app closes or the component unmounts while deletes wait, the timer is
 *  dropped and the notes are NOT deleted: losing a delete is recoverable,
 *  deleting something the user might still have undone is not. */
export function useDeferredDelete<T>(
  commit: (item: T) => void,
  delayMs: number = UNDO_DELETE_MS,
): DeferredDelete<T> {
  const [pending, setPending] = useState<T[]>([]);
  const pendingRef = useRef<T[]>([]);
  const timer = useRef<number | null>(null);
  // Always call the latest commit (it closes over App state).
  const commitRef = useRef(commit);
  useEffect(() => {
    commitRef.current = commit;
  }, [commit]);

  const take = useCallback(() => {
    const items = pendingRef.current;
    clearTimer(timer);
    pendingRef.current = [];
    setPending([]);
    return items;
  }, []);

  const runPending = useCallback(() => {
    for (const item of take()) commitRef.current(item);
  }, [take]);

  const schedule = useCallback(
    (item: T) => {
      clearTimer(timer);
      pendingRef.current = [...pendingRef.current, item];
      setPending(pendingRef.current);
      timer.current = window.setTimeout(runPending, delayMs);
    },
    [delayMs, runPending],
  );

  // Unmount (window closing, app reload): drop the timer WITHOUT deleting.
  useEffect(() => {
    const t = timer;
    return () => clearTimer(t);
  }, []);

  return { pending, schedule, undo: take, flush: runPending };
}
