import { RefreshCw } from "lucide-react";
import { updateCopy, updateNoticeVisible } from "../notices";
import type { Updater } from "../updater";
import { Button, Notice, ProgressBar } from "./ui";

interface Props {
  updater: Updater;
}

/** Non-blocking update prompt, rendered inside the app NoticeStack (lowest
 *  priority). App only mounts it while nothing is recording, so it can never
 *  interrupt a meeting; "Later" hides it for the session and Settings takes
 *  over. A failed download/install stays visible with "Try again" instead of
 *  silently vanishing. */
export default function UpdateBanner({ updater }: Props) {
  const { phase, version, progress, error } = updater;
  if (!updateNoticeVisible(updater)) return null;
  const copy = updateCopy(phase, version, error);
  const pct = progress == null ? null : Math.round(progress * 100);

  const later = (
    <Button variant="ghost" size="sm" onClick={updater.dismiss}>
      Later
    </Button>
  );
  const actions =
    phase === "available" ? (
      <>
        <Button variant="primary" size="sm" onClick={updater.downloadAndInstall}>
          Update now
        </Button>
        {later}
      </>
    ) : phase === "ready" ? (
      <>
        <Button variant="primary" size="sm" onClick={updater.restart}>
          Restart now
        </Button>
        {later}
      </>
    ) : phase === "error" ? (
      <>
        <Button variant="outline" size="sm" onClick={updater.downloadAndInstall}>
          Try again
        </Button>
        {later}
      </>
    ) : undefined;

  return (
    <Notice
      tone={phase === "error" ? "warning" : "primary"}
      icon={<RefreshCw size={18} strokeWidth={1.75} />}
      title={copy.title}
      details={copy.details ?? undefined}
      actions={actions}
    >
      {copy.cause}
      {phase === "downloading" && (
        <div className="mt-2 flex items-center gap-2">
          <ProgressBar value={pct} className="flex-1" aria-label="Update download" />
          {/* The bar carries aria-valuenow; the changing text would make the
              status region re-announce every percent. */}
          <span className="font-mono text-[11px] text-text-3" aria-hidden="true">
            {pct == null ? "Downloading…" : `${pct}%`}
          </span>
        </div>
      )}
    </Notice>
  );
}
