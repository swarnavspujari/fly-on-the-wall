import { useCallback, useEffect, useRef, useState } from "react";

function clearTimer(t: { current: number | null }) {
  if (t.current != null) window.clearTimeout(t.current);
  t.current = null;
}

/** How long the Undo toast stays before the delete runs. */
export const UNDO_DELETE_MS = 8_000;

export interface DeferredDelete<T> {
  /** The item waiting to be deleted (the toast shows while set). */
  pending: T | null;
  /** Hide it now, delete after the delay. A delete already waiting runs
   *  right away: its toast is being replaced, so it can't be undone. */
  schedule: (item: T) => void;
  /** Cancel the waiting delete; returns the item so the caller can restore it. */
  undo: () => T | null;
  /** Run the waiting delete now (the toast's close button). */
  flush: () => void;
}

/** Undo toast = deferred delete (spec §3.2.5): nothing is deleted until the
 *  toast expires, so Undo needs no soft-delete schema. If the app closes or
 *  the component unmounts while a delete is waiting, the timer is dropped
 *  and the note is NOT deleted: losing a delete is recoverable, deleting
 *  something the user might still have undone is not. */
export function useDeferredDelete<T>(
  commit: (item: T) => void,
  delayMs: number = UNDO_DELETE_MS,
): DeferredDelete<T> {
  const [pending, setPending] = useState<T | null>(null);
  const pendingRef = useRef<T | null>(null);
  const timer = useRef<number | null>(null);
  // Always call the latest commit (it closes over App state).
  const commitRef = useRef(commit);
  useEffect(() => {
    commitRef.current = commit;
  }, [commit]);

  const runPending = useCallback(() => {
    const item = pendingRef.current;
    clearTimer(timer);
    pendingRef.current = null;
    setPending(null);
    if (item != null) commitRef.current(item);
  }, []);

  const schedule = useCallback(
    (item: T) => {
      runPending();
      pendingRef.current = item;
      setPending(item);
      timer.current = window.setTimeout(runPending, delayMs);
    },
    [delayMs, runPending],
  );

  const undo = useCallback(() => {
    const item = pendingRef.current;
    clearTimer(timer);
    pendingRef.current = null;
    setPending(null);
    return item;
  }, []);

  // Unmount (window closing, app reload): drop the timer WITHOUT deleting.
  useEffect(() => {
    const t = timer;
    return () => clearTimer(t);
  }, []);

  return { pending, schedule, undo, flush: runPending };
}
