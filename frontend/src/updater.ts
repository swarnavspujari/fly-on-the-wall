import { useCallback, useEffect, useRef, useState } from "react";
import { check as pluginCheck, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

/* Auto-update lifecycle. Windows-only for now: latest.json on GitHub Releases
   carries a windows-x86_64 entry, so mac/linux builds never even check.

   Hard rule: an update must NEVER interrupt an active recording. The app never
   restarts itself — install + relaunch only ever run from an explicit user
   click, and App hides the banner (and Settings disables the buttons) while a
   recording is running. If a recording starts mid-download, the downloaded
   update parks in "ready" until the user restarts. */

export type UpdatePhase =
  | "idle" // no check yet (or a silent startup check failed)
  | "checking"
  | "upToDate"
  | "available" // update found, waiting for the user to opt in
  | "downloading"
  | "ready" // downloaded; waiting for an explicit restart
  | "installing"
  | "error";

export interface Updater {
  phase: UpdatePhase;
  /** Version of the available update (from "available" onward). */
  version: string | null;
  /** 0..1 while downloading; null when the server sent no content length. */
  progress: number | null;
  error: string | null;
  /** False on macOS/Linux — new versions ship via GitHub Releases only. */
  supported: boolean;
  /** True once the banner's "Later" was clicked; Settings keeps working. */
  dismissed: boolean;
  check: () => void;
  downloadAndInstall: () => void;
  restart: () => void;
  dismiss: () => void;
}

/** Startup check waits for first-run IO (models, DB migrations) to settle. */
const AUTO_CHECK_DELAY_MS = 5_000;

export function useUpdater(os: string | null, recordingActive: boolean): Updater {
  const supported = os === "windows";

  const [phase, setPhase] = useState<UpdatePhase>("idle");
  const [version, setVersion] = useState<string | null>(null);
  const [progress, setProgress] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState(false);

  const updateRef = useRef<Update | null>(null);
  const autoChecked = useRef(false);

  // Callbacks consult recording state at completion time, not click time.
  const recordingRef = useRef(recordingActive);
  useEffect(() => {
    recordingRef.current = recordingActive;
  }, [recordingActive]);

  const doCheck = useCallback(async (silent: boolean) => {
    setPhase("checking");
    setError(null);
    try {
      const update = await pluginCheck({ timeout: 30_000 });
      if (update) {
        updateRef.current = update;
        setVersion(update.version);
        setPhase("available");
      } else {
        setPhase(silent ? "idle" : "upToDate");
      }
    } catch (e) {
      if (silent) {
        // Offline or no release published yet — not worth bothering anyone.
        console.warn("update check failed:", e);
        setPhase("idle");
      } else {
        setError(String(e));
        setPhase("error");
      }
    }
  }, []);

  // One silent check shortly after startup (Windows only).
  useEffect(() => {
    if (!supported || autoChecked.current) return;
    autoChecked.current = true;
    const t = window.setTimeout(() => void doCheck(true), AUTO_CHECK_DELAY_MS);
    return () => window.clearTimeout(t);
  }, [supported, doCheck]);

  const install = useCallback(async () => {
    const update = updateRef.current;
    if (!update) return;
    setPhase("installing");
    try {
      // On Windows the installer kills the app and relaunches it; relaunch()
      // is the documented fallback for platforms where it doesn't.
      await update.install();
      await relaunch();
    } catch (e) {
      setError(String(e));
      setPhase("error");
    }
  }, []);

  const downloadAndInstall = useCallback(() => {
    const update = updateRef.current;
    if (!update) return;
    setPhase("downloading");
    setProgress(null);
    let total = 0;
    let received = 0;
    let lastPct = -1;
    void update
      .download((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          received += event.data.chunkLength;
          if (total > 0) {
            const pct = Math.floor((received / total) * 100);
            if (pct !== lastPct) {
              lastPct = pct;
              setProgress(received / total);
            }
          }
        }
      })
      .then(() => {
        // A recording may have started while we downloaded — park the update.
        if (recordingRef.current) {
          setPhase("ready");
        } else {
          return install();
        }
      })
      .catch((e: unknown) => {
        setError(String(e));
        setPhase("error");
      });
  }, [install]);

  const restart = useCallback(() => {
    if (recordingRef.current) return; // buttons are disabled, but be safe
    void install();
  }, [install]);

  const checkNow = useCallback(() => void doCheck(false), [doCheck]);
  const dismiss = useCallback(() => setDismissed(true), []);

  return {
    phase,
    version,
    progress,
    error,
    supported,
    dismissed,
    check: checkNow,
    downloadAndInstall,
    restart,
    dismiss,
  };
}
