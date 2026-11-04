import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { api } from "../api";
import { BACKUP_EVENT, type BackupNoticeKind } from "../backupModel";
import type { BackupStatus } from "../types";

/** The latest BackupStatus: asked once, then kept current by `backup:status`
 *  (sent when the compression job's own backup is skipped or fails). */
export function useBackupStatus(): BackupStatus | null {
  const [status, setStatus] = useState<BackupStatus | null>(null);
  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | null = null;
    api
      .backupStatus()
      .then((s) => {
        if (live) setStatus(s);
      })
      .catch(() => {});
    listen<BackupStatus>(BACKUP_EVENT, (e) => setStatus(e.payload))
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
  return status;
}

export interface BackupNotice {
  /** Why the one-time notice is due, or null. */
  kind: BackupNoticeKind | null;
  dismiss: () => void;
}

/** The one-time "couldn't save a backup" notice: due when a launch's backup
 *  was skipped (not enough disk space) or failed, until the user dismisses it
 *  (stored by the backend, so it never comes back). */
export function useBackupNotice(): BackupNotice {
  const status = useBackupStatus();
  const [dismissed, setDismissed] = useState(false);
  return {
    kind: dismissed ? null : (status?.notice ?? null),
    dismiss: () => {
      setDismissed(true);
      void api.dismissBackupNotice().catch(() => {});
    },
  };
}
