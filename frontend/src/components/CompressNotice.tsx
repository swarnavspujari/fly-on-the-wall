import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { api } from "../api";
import { COMPRESS_EVENT } from "../storageModel";
import type { CompressStatus } from "../types";

export interface CompressNotice {
  /** Bytes the compression freed, when the one-time notice is due. */
  savedBytes: number | null;
  dismiss: () => void;
}

/** The one-time "recordings compressed" notice: due once the first
 *  background pass after an upgrade freed space (audio_store.rs), until the
 *  user dismisses it (stored by the backend, so it never comes back). */
export function useCompressNotice(): CompressNotice {
  const [status, setStatus] = useState<CompressStatus | null>(null);
  const [dismissed, setDismissed] = useState(false);
  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | null = null;
    api
      .storageReport()
      .then((r) => {
        if (live) setStatus(r.compress);
      })
      .catch(() => {});
    listen<CompressStatus>(COMPRESS_EVENT, (e) => setStatus(e.payload))
      .then((un) => {
        if (live) unlisten = un;
        else un();
      })
      .catch(() => {});
    return () => {
      live = false;
      unlisten?.();
    };
  }, []);
  const due = !dismissed && !!status?.notice_pending && status.saved_bytes > 0;
  return {
    savedBytes: due ? status.saved_bytes : null,
    dismiss: () => {
      setDismissed(true);
      void api.dismissCompressNotice().catch(() => {});
    },
  };
}
