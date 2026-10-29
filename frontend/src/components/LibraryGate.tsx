import { useEffect, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import type { LibraryStatus } from "../types";
import { ProgressBar } from "./ui";

/** Backend event carrying a LibraryStatus (src-tauri startup.rs). */
export const LIBRARY_UPGRADE_EVENT = "library:upgrade";
/** Safety net for a missed event while upgrading. */
export const LIBRARY_POLL_MS = 1000;

const READY: LibraryStatus = { phase: "ready", done: 0, total: 0 };

/** Whole percent for the bar (0 until the first report). */
export function upgradePercent(s: LibraryStatus): number {
  return s.total > 0 ? Math.min(100, Math.floor((s.done / s.total) * 100)) : 0;
}

/** Keep the newest state: "ready" is final, and a late poll answer never
 *  moves the bar back. */
function merge(prev: LibraryStatus | null, next: LibraryStatus): LibraryStatus {
  if (prev?.phase === "ready") return prev;
  if (
    prev?.phase === "upgrading" &&
    next.phase === "upgrading" &&
    next.total === prev.total &&
    next.done < prev.done
  ) {
    return prev;
  }
  return next;
}

/** Mounts the app only once the library is open. A first launch after a
 *  schema update can spend a while upgrading the library in the background;
 *  until it's done the backend has no app state, so this shows the upgrade
 *  screen instead. A backend without `library_status` (older build, plain
 *  dev) mounts the app straight away. */
export default function LibraryGate({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<LibraryStatus | null>(null);
  const phase = status?.phase ?? null;

  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | null = null;
    const apply = (s: LibraryStatus) => {
      if (alive) setStatus((prev) => merge(prev, s));
    };
    (async () => {
      // Listen first, then ask: an upgrade that finishes in between still
      // reaches us.
      try {
        unlisten = await listen<LibraryStatus>(LIBRARY_UPGRADE_EVENT, (e) => apply(e.payload));
      } catch {
        // No event bridge: the poll below still gets there.
      }
      if (!alive) {
        unlisten?.();
        return;
      }
      try {
        apply(await api.libraryStatus());
      } catch {
        apply(READY);
      }
    })();
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (phase !== "upgrading") return;
    const t = window.setInterval(() => {
      api.libraryStatus().then(
        (s) => setStatus((prev) => merge(prev, s)),
        () => {},
      );
    }, LIBRARY_POLL_MS);
    return () => window.clearInterval(t);
  }, [phase]);

  if (status === null) return null;
  if (status.phase === "ready") return <>{children}</>;
  return <UpgradeScreen status={status} />;
}

function UpgradeScreen({ status }: { status: LibraryStatus }) {
  const failed = status.phase === "failed";
  const pct = upgradePercent(status);
  return (
    <main className="grid min-h-screen place-items-center bg-bg p-6">
      <div className="w-full max-w-[420px]">
        {/* Live region for the phase only; the bar itself isn't announced
            on every tick. */}
        <div role="status">
          <h1
            id="library-upgrade-title"
            className="font-display text-[22px] font-bold text-text"
            style={{ margin: "0 0 8px" }}
          >
            {failed ? "Your library couldn't be upgraded" : "Upgrading your library"}
          </h1>
          <p className="text-[13.5px] leading-[1.55] text-text-2" style={{ margin: "0 0 20px" }}>
            {failed
              ? "Fly on the Wall will show what happened in a separate message. Your notes and recordings are safe."
              : "Fly on the Wall is rebuilding its search index. This happens once, and your notes are safe."}
          </p>
        </div>
        {!failed && (
          <>
            <ProgressBar value={pct} size="md" aria-labelledby="library-upgrade-title" />
            <p
              className="text-text-3"
              style={{
                margin: "8px 0 0",
                fontFamily: "var(--font-mono)",
                fontSize: 12,
                fontVariantNumeric: "tabular-nums",
              }}
            >
              {pct}%
            </p>
          </>
        )}
      </div>
    </main>
  );
}
