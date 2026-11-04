// Pure copy for library backups (src-tauri backups.rs, fly-storage backup.rs):
// the Settings row, the startup step's hint and the one-time "couldn't save a
// backup" notice. Kept free of React so vitest covers every case.

import { formatBytes } from "./privacy";
import type { BackupStatus } from "./types";

/** Event the backend emits with a fresh BackupStatus. */
export const BACKUP_EVENT = "backup:status";

/** Newest backups kept (fly-storage `KEEP_BACKUPS`). */
export const BACKUPS_KEPT = 3;

export type BackupNoticeKind = NonNullable<BackupStatus["notice"]>;

const NOTICE_TITLE = "No backup of your library was saved";

/** The notice shown when a backup was skipped or failed. Before an upgrade the
 *  library was upgraded anyway, so that copy says what is and isn't affected;
 *  before the recordings are compressed nothing of the library changed. */
export function backupNoticeCopy(kind: BackupNoticeKind): {
  title: string;
  cause: string;
  steps: string[];
} {
  switch (kind) {
    case "space":
      return {
        title: NOTICE_TITLE,
        cause:
          "Fly on the Wall couldn’t save a backup of your library before upgrading: not enough disk space. Your notes and recordings were upgraded without a copy of the old library.",
        steps: ["Free up some space on this drive. The next app update saves a backup again."],
      };
    case "failed":
      return {
        title: NOTICE_TITLE,
        cause:
          "Fly on the Wall couldn’t save a backup of your library before upgrading. Your notes and recordings were upgraded without a copy of the old library.",
        steps: [
          "If you want your own copy, close the app and copy flyonthewall.db from the data folder.",
        ],
      };
    case "compression_space":
      return {
        title: NOTICE_TITLE,
        cause:
          "Fly on the Wall couldn’t save a backup of your library before storing your recordings more compactly: not enough disk space. The recordings are checked sample for sample as they’re converted, but your notes have no safety copy.",
        steps: [
          "Free up some space on this drive. Fly on the Wall tries again the next time it starts.",
        ],
      };
    case "compression_failed":
      return {
        title: NOTICE_TITLE,
        cause:
          "Fly on the Wall couldn’t save a backup of your library before storing your recordings more compactly. The recordings are checked sample for sample as they’re converted, but your notes have no safety copy.",
        steps: [
          "If you want your own copy, close the app and copy flyonthewall.db from the data folder.",
        ],
      };
  }
}

/** The Settings row's helper. */
export function backupRowHelper(count: number): string {
  const kept = `Recordings aren’t copied: they’re stored losslessly and checked.`;
  if (count === 0) {
    return `Before an update changes your library, Fly on the Wall saves a copy of it here. ${kept}`;
  }
  return `Copies of your library from before updates. The newest ${BACKUPS_KEPT} are kept, plus the last copy from before each recent change to how the library is stored, so a note you delete later stays in older copies until they’re replaced. ${kept}`;
}

/** Seconds before the startup step says how big the copy is and how long it
 *  has taken; a small library is done before that. */
export const BACKUP_HINT_AFTER_S = 3;

/** The startup backup step has no percentage (the copy reports none), so a big
 *  library would look stuck. After a few seconds, say what is being copied and
 *  for how long, so the wait reads as work. `bytes` is the library's size
 *  (0 when unknown). */
export function backupStepHint(bytes: number, seconds: number): string | null {
  if (seconds < BACKUP_HINT_AFTER_S) return null;
  const size = bytes > 0 ? `Copying ${formatBytes(bytes)}. ` : "";
  const m = Math.floor(seconds / 60);
  const elapsed = m > 0 ? `${m} min ${seconds % 60} s` : `${seconds} s`;
  return `${size}${elapsed} so far.`;
}
