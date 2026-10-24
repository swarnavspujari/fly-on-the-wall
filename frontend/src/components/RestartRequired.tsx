import { useEffect, useRef, useState } from "react";
import { relaunch } from "@tauri-apps/plugin-process";
import { api } from "../api";
import {
  RESTART_FAILED_TEXT,
  RESTART_REQUIRED_EVENT,
  WIPE_CONFIRMATION,
  wipeOutcome,
  type RestartRequiredDetail,
} from "../privacy";
import type { WipeReport } from "../types";
import { Button } from "./ui";

/** Blocking full-window screen shown after "Delete all my data" until the
 *  app restarts: the data folder is half-deleted and a pending wipe marker
 *  expires in 10 minutes, so no further work may happen in this session.
 *  Mounted once at App level (it must outlive the Settings modal) and armed
 *  by RESTART_REQUIRED_EVENT. It has no close button on purpose. */
export default function RestartRequired() {
  const [detail, setDetail] = useState<RestartRequiredDetail | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const autoStarted = useRef(false);
  const dialogRef = useRef<HTMLDivElement>(null);

  const restart = () => {
    setError(null);
    relaunch().catch(() => setError(RESTART_FAILED_TEXT));
  };

  useEffect(() => {
    const on = (e: Event) => setDetail((e as CustomEvent<RestartRequiredDetail>).detail);
    window.addEventListener(RESTART_REQUIRED_EVENT, on);
    return () => window.removeEventListener(RESTART_REQUIRED_EVENT, on);
  }, []);

  const outcome = detail ? wipeOutcome(detail.report) : null;

  // While shown, app-wide shortcuts (new note, record, Escape…) must not
  // reach the app underneath; only focus movement stays usable.
  useEffect(() => {
    if (!detail) return;
    const block = (e: KeyboardEvent) => {
      const inside = (e.target as Node | null) && dialogRef.current?.contains(e.target as Node);
      if (e.key === "Tab" || (inside && (e.key === "Enter" || e.key === " "))) return;
      e.stopPropagation();
      if (!inside) e.preventDefault();
    };
    window.addEventListener("keydown", block, true);
    return () => window.removeEventListener("keydown", block, true);
  }, [detail]);

  // A clean wipe restarts on its own after a beat; anything else waits for
  // the user to read what's left.
  useEffect(() => {
    if (outcome !== "clean" || autoStarted.current) return;
    autoStarted.current = true;
    const t = window.setTimeout(() => {
      relaunch().catch(() => setError(RESTART_FAILED_TEXT));
    }, 1500);
    return () => window.clearTimeout(t);
  }, [outcome]);

  if (!detail || !outcome) return null;
  const r: WipeReport = detail.report;

  const retry = async () => {
    setBusy(true);
    setError(null);
    try {
      const report = await api.deleteAllData(detail.includeModels, WIPE_CONFIRMATION);
      setDetail({ ...detail, report });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="restart-required-title"
      ref={dialogRef}
      className="fixed inset-0 z-[2000] grid place-items-center bg-bg/95 p-6"
    >
      <div className="w-full max-w-[460px] rounded-2xl border border-line bg-surface p-6 shadow-lg">
        <h2
          id="restart-required-title"
          className="mb-2 font-display text-[19px] font-bold text-text"
          style={{ margin: "0 0 8px" }}
        >
          {outcome === "clean" ? "Your data is deleted" : "Restart required"}
        </h2>

        {outcome === "clean" && (
          <p className="text-[13px] leading-[1.55] text-text-2" style={{ margin: 0 }}>
            Restarting Fly on the Wall to finish…
          </p>
        )}

        {outcome === "db-open" && (
          <p className="text-[13px] leading-[1.55] text-text" style={{ margin: 0 }}>
            Recordings, notes and saved keys are deleted, but another program still has the database
            open, so its old contents may remain on disk. Quit Claude Desktop and any other app that
            uses Fly on the Wall's MCP server, then choose Try again.
          </p>
        )}

        {outcome === "leftovers" && (
          <>
            <p className="text-[13px] leading-[1.55] text-text" style={{ margin: 0 }}>
              Most data is deleted. A few items were in use; restarting removes them.
              {r.secrets_failed.length > 0 &&
                ` These keychain entries couldn't be removed: ${r.secrets_failed.join(", ")}.`}
            </p>
            {r.failed.length > 0 && (
              <p
                className="text-text-3"
                style={{ margin: "8px 0 0", fontFamily: "var(--font-mono)", fontSize: 11 }}
              >
                {r.failed.slice(0, 8).join(", ")}
                {r.failed.length > 8 ? "…" : ""}
              </p>
            )}
          </>
        )}

        <p className="text-[12px] leading-[1.5] text-text-3" style={{ margin: "10px 0 0" }}>
          The app can't be used until it restarts.
        </p>

        {error && (
          <p className="text-[12.5px] text-error-text" style={{ margin: "10px 0 0" }}>
            {error}
          </p>
        )}

        <div className="flex gap-2" style={{ marginTop: 16 }}>
          {outcome === "db-open" && (
            <Button variant="record" size="sm" disabled={busy} onClick={() => void retry()}>
              {busy ? "Deleting…" : "Try again"}
            </Button>
          )}
          {outcome !== "clean" && (
            <Button
              variant={outcome === "db-open" ? "outline" : "record"}
              size="sm"
              disabled={busy}
              onClick={restart}
            >
              {outcome === "db-open" ? "Restart anyway" : "Restart now"}
            </Button>
          )}
          {outcome === "clean" && error && (
            <Button variant="record" size="sm" onClick={restart}>
              Try restarting again
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
