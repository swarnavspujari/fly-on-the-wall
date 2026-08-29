import { Btn } from "./ui";

interface Props {
  /** Open System Settings → Privacy & Security → Screen & System Audio Recording. */
  onOpenSettings: () => void;
  onDismiss: () => void;
}

/** Floating launch-time warning (same shape as UpdateBanner, but higher
 *  priority): the macOS system-audio preflight found the tap recording only
 *  silence while audio was playing — TCC is denying this build. The common
 *  cause is a STALE grant: the Settings toggle already shows the app as
 *  allowed, but it belongs to a previously installed (differently signed)
 *  build, so the fix is remove + re-add, not just "grant it". */
export default function SystemAudioNotice({ onOpenSettings, onDismiss }: Props) {
  return (
    <div
      className="fixed bottom-12 right-4 z-50 w-96 rounded-2xl border border-line bg-surface p-4 shadow-warm"
      role="alert"
    >
      <p className="font-display text-[15px] font-bold tracking-tight text-ink">
        System audio can’t be captured
      </p>
      <p className="mt-1 text-[13px] leading-relaxed text-ink-2">
        macOS is blocking system-audio recording for this build — recordings would capture only your
        microphone and miss the other participants. Allow Fly on the Wall under{" "}
        <b>Screen &amp; System Audio Recording</b>. If it <em>already shows as allowed</em>, that
        grant belongs to a previous install: remove Fly on the Wall from the list with the − button
        and add it back (or run{" "}
        <code className="font-mono text-[12px]">
          tccutil reset ScreenCapture com.flyonthewall.app
        </code>{" "}
        in Terminal), then relaunch.
      </p>
      <div className="mt-3 flex justify-end gap-2">
        <Btn variant="ghost" size="sm" onClick={onDismiss}>
          Dismiss
        </Btn>
        <Btn variant="primary" size="sm" onClick={onOpenSettings}>
          Open System Settings
        </Btn>
      </div>
    </div>
  );
}
