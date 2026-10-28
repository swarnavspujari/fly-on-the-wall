import { TriangleAlert } from "lucide-react";
import { systemAudioCopy } from "../notices";
import { Button, Notice } from "./ui";

interface Props {
  /** AppInfo.os — picks the cause and fix (see `systemAudioCopy`). */
  os: string | null;
  /** macOS: open System Settings → Privacy & Security → Screen & System
   *  Audio Recording. Omitted where there's no pane to open. */
  onOpenSettings?: () => void;
  onDismiss: () => void;
}

/** System audio records as silence, so a recording would miss the other
 *  participants. Today only macOS raises it at launch: the post-consent
 *  preflight found the tap denied, usually a STALE grant from an earlier,
 *  differently signed install, so the fix is remove + re-add (or tccutil),
 *  not just "allow it". Rendered inside the app NoticeStack (top priority). */
export default function SystemAudioNotice({ os, onOpenSettings, onDismiss }: Props) {
  const copy = systemAudioCopy(os);
  return (
    <Notice
      role="alert"
      tone="warning"
      icon={<TriangleAlert size={18} strokeWidth={1.75} />}
      title={copy.title}
      steps={copy.steps}
      details={copy.details}
      detailsLabel={copy.detailsLabel}
      actions={
        <>
          {onOpenSettings && (
            <Button variant="primary" size="sm" onClick={onOpenSettings}>
              Open System Settings
            </Button>
          )}
          <Button variant="ghost" size="sm" onClick={onDismiss}>
            Dismiss
          </Button>
        </>
      }
    >
      {copy.cause}
    </Notice>
  );
}
