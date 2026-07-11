import { Btn } from "./ui";
import type { Updater } from "../updater";

interface Props {
  updater: Updater;
}

/** Floating, non-blocking update prompt (bottom-right, under any modal).
 *  App only mounts it while nothing is recording, so it can never interrupt
 *  a meeting; "Later" hides it for the session and Settings takes over. */
export default function UpdateBanner({ updater }: Props) {
  const { phase, version, progress, dismissed } = updater;

  const active =
    phase === "available" || phase === "downloading" || phase === "ready" || phase === "installing";
  if (!active || dismissed) return null;

  return (
    <div className="fixed bottom-12 right-4 z-40 w-80 rounded-2xl border border-line bg-surface p-4 shadow-warm">
      <p className="font-display text-[15px] font-bold tracking-tight text-ink">
        {phase === "ready" ? "Update downloaded" : `Update available: Fly on the Wall v${version}`}
      </p>

      {phase === "available" && (
        <>
          <p className="mt-1 text-[13px] leading-relaxed text-ink-2">
            Downloads in the background — Fly on the Wall restarts once to apply it.
          </p>
          <div className="mt-3 flex justify-end gap-2">
            <Btn variant="ghost" size="sm" onClick={updater.dismiss}>
              Later
            </Btn>
            <Btn variant="primary" size="sm" onClick={updater.downloadAndInstall}>
              Update now
            </Btn>
          </div>
        </>
      )}

      {phase === "downloading" && (
        <div className="mt-2.5 flex items-center gap-2">
          <span className="h-1.5 flex-1 overflow-hidden rounded-full bg-line">
            <span
              className={`block h-full rounded-full bg-coral ${progress == null ? "w-1/3 animate-pulse" : ""}`}
              style={progress == null ? undefined : { width: `${Math.round(progress * 100)}%` }}
            />
          </span>
          <span className="font-mono text-[11px] text-mute">
            {progress == null ? "downloading…" : `${Math.round(progress * 100)}%`}
          </span>
        </div>
      )}

      {phase === "ready" && (
        <>
          <p className="mt-1 text-[13px] leading-relaxed text-ink-2">
            Restart Fly on the Wall to finish updating.
          </p>
          <div className="mt-3 flex justify-end gap-2">
            <Btn variant="ghost" size="sm" onClick={updater.dismiss}>
              Later
            </Btn>
            <Btn variant="primary" size="sm" onClick={updater.restart}>
              Restart now
            </Btn>
          </div>
        </>
      )}

      {phase === "installing" && (
        <p className="mt-1 text-[13px] leading-relaxed text-ink-2">
          Installing — Fly on the Wall will restart itself…
        </p>
      )}
    </div>
  );
}
