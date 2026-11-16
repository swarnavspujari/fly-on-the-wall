import { useCallback, useEffect, useRef, useState } from "react";
import { check as pluginCheck, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { Channel, invoke } from "@tauri-apps/api/core";

/* Auto-update lifecycle. latest.json on GitHub Releases carries entries for
   windows-x86_64, darwin-aarch64, darwin-x86_64 and linux-x86_64. The one
   install the updater cannot replace is a Linux deb (apt owns it), so that's
   the only case `supported` is false.

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
  /** What failed when phase is "error": a check, or a download/install.
   *  Optional so hand-made Updater objects (tests) needn't set it. */
  errorSource?: "check" | "install" | null;
  /** False only for Linux deb installs — those update via apt/GitHub Releases. */
  supported: boolean;
  /** True once the banner's "Later" was clicked; Settings keeps working. */
  dismissed: boolean;
  check: () => void;
  downloadAndInstall: () => void;
  restart: () => void;
  dismiss: () => void;
}

/** Download progress from the Windows download command; same shape as the
 *  plugin's own channel events so one handler serves both paths. */
type DownloadEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | { event: "Progress"; data: { chunkLength: number } }
  | { event: "Finished" };

/** Startup check waits for first-run IO (models, DB migrations) to settle. */
const AUTO_CHECK_DELAY_MS = 5_000;

export function useUpdater(
  os: string | null,
  recordingActive: boolean,
  appImage: boolean = false,
): Updater {
  const supported = os === "windows" || os === "macos" || (os === "linux" && appImage);

  const [phase, setPhase] = useState<UpdatePhase>("idle");
  const [version, setVersion] = useState<string | null>(null);
  const [progress, setProgress] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [errorSource, setErrorSource] = useState<"check" | "install" | null>(null);
  const [dismissed, setDismissed] = useState(false);

  const updateRef = useRef<Update | null>(null);
  // Set when an install failed: the Update we hold may be stale (its backend
  // resource can be gone), so the next attempt checks again first.
  const needsRecheck = useRef(false);
  // Windows installs through the app's own commands (see windows_update.rs):
  // the plugin's JS install hides the window and drops the single-instance
  // lock before it launches the installer, so a launch blocked by antivirus
  // would leave a hidden second-copy-able app.
  const viaApp = os === "windows";
  const autoChecked = useRef(false);

  // Callbacks consult recording state at completion time, not click time.
  const recordingRef = useRef(recordingActive);
  useEffect(() => {
    recordingRef.current = recordingActive;
  }, [recordingActive]);

  const doCheck = useCallback(async (silent: boolean) => {
    setPhase("checking");
    setError(null);
    setErrorSource(null);
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
        setErrorSource("check");
        setPhase("error");
      }
    }
  }, []);

  // One silent check shortly after startup (skipped on unsupported installs).
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
      // On Windows the installer kills the app and relaunches it; on macOS
      // (.app swap) and Linux (AppImage swap) relaunch() does the restart.
      if (viaApp) {
        await invoke("windows_update_install");
      } else {
        await update.install();
      }
      await relaunch();
    } catch (e) {
      needsRecheck.current = true;
      setError(String(e));
      setErrorSource("install");
      setPhase("error");
    }
  }, [viaApp]);

  const downloadAndInstall = useCallback(() => {
    const run = async () => {
      if (needsRecheck.current) {
        // "Try again" after a failed install: look for the update afresh.
        needsRecheck.current = false;
        setPhase("checking");
        const fresh = await pluginCheck({ timeout: 30_000 });
        if (!fresh) {
          updateRef.current = null;
          setPhase("upToDate");
          return;
        }
        updateRef.current = fresh;
        setVersion(fresh.version);
      }
      const update = updateRef.current;
      if (!update) return;
      setPhase("downloading");
      setProgress(null);
      setError(null);
      setErrorSource(null);
      let total = 0;
      let received = 0;
      let lastPct = -1;
      const onEvent = (event: DownloadEvent) => {
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
      };
      if (viaApp) {
        const channel = new Channel<DownloadEvent>();
        channel.onmessage = onEvent;
        await invoke("windows_update_download", { onEvent: channel });
      } else {
        await update.download(onEvent);
      }
      // A recording may have started while we downloaded — park the update.
      if (recordingRef.current) {
        setPhase("ready");
      } else {
        await install();
      }
    };
    void run().catch((e: unknown) => {
      needsRecheck.current = true;
      setError(String(e));
      setErrorSource("install");
      setPhase("error");
    });
  }, [install, viaApp]);

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
    errorSource,
    supported,
    dismissed,
    check: checkNow,
    downloadAndInstall,
    restart,
    dismiss,
  };
}
