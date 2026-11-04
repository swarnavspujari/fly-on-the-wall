import { api } from "../api";
import { backupRowHelper } from "../backupModel";
import { useBackupStatus } from "./BackupNotice";
import { Button, SettingsRow } from "./ui";

/** Settings › Your files and privacy: "Backups: 3 · Open folder". The copies
 *  of the library the app saves before an update changes it. Read-only — no
 *  restore button; the steps are in the docs. */
export default function BackupRow() {
  const status = useBackupStatus();
  if (!status) return null;
  return (
    <SettingsRow
      title="Backups"
      helper={backupRowHelper(status.count)}
      control={
        <>
          <span className="text-text-2" data-testid="backup-count">
            {status.count}
          </span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void api.revealBackupsDir().catch(console.error)}
          >
            Open folder
          </Button>
        </>
      }
    />
  );
}
