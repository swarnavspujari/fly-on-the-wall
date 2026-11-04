// Pure copy for library backups (src-tauri backups.rs, fly-storage backup.rs):
// the Settings row and the one-time "couldn't save a backup" notice. Kept
// free of React so vitest covers every case.

import type { BackupStatus } from "./types";

/** Event the backend emits with a fresh BackupStatus. */
export const BACKUP_EVENT = "backup:status";

/** Backups kept (fly-storage `KEEP_BACKUPS`). */
export const BACKUPS_KEPT = 3;

export type BackupNoticeKind = NonNullable<BackupStatus["notice"]>;

/** The notice shown when a launch's backup was skipped or failed. The
 *  library was upgraded anyway, so it says what is and isn't affected. */
export function backupNoticeCopy(kind: BackupNoticeKind): {
  title: string;
  cause: string;
  steps: string[];
} {
  if (kind === "space") {
    return {
      title: "No backup of your library was saved",
      cause:
        "Fly on the Wall couldn’t save a backup of your library before upgrading: not enough disk space. Your notes and recordings were upgraded without a copy of the old library.",
      steps: ["Free up some space on this drive. The next app update saves a backup again."],
    };
  }
  return {
    title: "No backup of your library was saved",
    cause:
      "Fly on the Wall couldn’t save a backup of your library before upgrading. Your notes and recordings were upgraded without a copy of the old library.",
    steps: [
      "If you want your own copy, close the app and copy flyonthewall.db from the data folder.",
    ],
  };
}

/** The Settings row's helper. */
export function backupRowHelper(count: number): string {
  const kept = `Recordings aren’t copied: they’re stored losslessly and checked.`;
  if (count === 0) {
    return `Before an update changes your library, Fly on the Wall saves a copy of it here. ${kept}`;
  }
  return `Copies of your library from before updates. The newest ${BACKUPS_KEPT} are kept, so a note you delete later stays in older copies until they’re replaced. ${kept}`;
}
