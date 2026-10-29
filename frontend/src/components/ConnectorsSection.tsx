import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { TriangleAlert } from "lucide-react";
import { api, CONNECTORS_CHANGED_EVENT } from "../api";
import { CONNECTORS_BETA_KEY, connectorBadge, connectorStatus } from "../connectorsModel";
import type { Connector, ConnectorDirectoryEntry, ConnectorsOverview } from "../types";
import { BrowseSheet, CustomSheet, ImportSheet, LocalSheet } from "./ConnectorAdd";
import ConnectorDetail from "./ConnectorDetail";
import { ConnectorIcon, ErrorText, MenuButton, SMALL } from "./connectorParts";
import { Badge, Button, SettingsGroup, SettingsRow, Switch } from "./ui";

/* Settings › Connectors. Rules this view keeps:
   - It never grants trust itself. Adding or turning on a connector,
     loosening a tool's permission and approving a changed tool are
     commands the backend confirms in a native dialog; a decline comes back
     as `saved: false` and changes nothing on screen.
   - The only setting it writes is the beta flag (ui.beta.connectors).
   - Server text (names, descriptions, diffs, errors) is plain text only:
     text nodes and <pre>, never markdown or links. */

type Sheet =
  | { kind: "browse" }
  | { kind: "custom"; editId?: string }
  | { kind: "local"; editId?: string }
  | { kind: "import" };

/** The Settings section body (the caller draws the heading). */
export default function ConnectorsSection() {
  const [overview, setOverview] = useState<ConnectorsOverview | null>(null);
  // null until known (the switch waits)
  const [beta, setBeta] = useState<boolean | null>(null);
  const [betaBusy, setBetaBusy] = useState(false);
  const [betaError, setBetaError] = useState<string | null>(null);
  const [state, setState] = useState<"loading" | "ok" | "error">("loading");
  const [directory, setDirectory] = useState<ConnectorDirectoryEntry[]>([]);
  const [view, setView] = useState<{ id: string; notice: string | null } | null>(null);
  const [sheet, setSheet] = useState<Sheet | null>(null);
  const [announcement, setAnnouncement] = useState("");
  const [rowBusy, setRowBusy] = useState<ReadonlySet<string>>(() => new Set());
  const [rowError, setRowError] = useState<{ id: string; error: string } | null>(null);
  const reviewCounts = useRef(new Map<string, number>());
  const focusRow = useRef<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const announce = useCallback((text: string) => setAnnouncement(text), []);

  const load = useCallback(async () => {
    try {
      const o = await api.connectorsOverview();
      setOverview(o);
      setBeta(o.beta);
      setState("ok");
      // "2 tools changed", politely, when a refresh finds new changes.
      for (const c of o.connectors) {
        const before = reviewCounts.current.get(c.id);
        if (before !== undefined && c.reviewCount > before)
          setAnnouncement(
            `${c.name}: ${c.reviewCount} tool${c.reviewCount === 1 ? "" : "s"} changed`,
          );
        reviewCounts.current.set(c.id, c.reviewCount);
      }
    } catch {
      setState("error");
      // The switch still works without the overview.
      const v = await api.getAppSetting(CONNECTORS_BETA_KEY).catch(() => null);
      setBeta(v === "1");
    }
  }, []);

  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | null = null;
    void load();
    // Something changed in the backend (a sync, a sign-in, a tool changing
    // on the server): read everything again.
    listen<{ id: string }>(CONNECTORS_CHANGED_EVENT, () => {
      if (live) void load();
    })
      .then((un) => {
        if (live) unlisten = un;
        else un();
      })
      .catch(() => {});
    return () => {
      live = false;
      unlisten?.();
    };
  }, [load]);

  // The directory names hosted connectors (no Custom badge) and fills Browse.
  useEffect(() => {
    if (!beta || directory.length > 0) return;
    let live = true;
    api
      .connectorsDirectory()
      .then((d) => {
        if (live) setDirectory(d);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [beta, directory.length]);

  // Back from a detail page: focus returns to its row.
  useEffect(() => {
    if (view || !focusRow.current) return;
    listRef.current
      ?.querySelector<HTMLButtonElement>(
        `[data-connector-row="${CSS_ID(focusRow.current)}"] [data-open-row]`,
      )
      ?.focus();
    focusRow.current = null;
  }, [view]);

  const toggleBeta = async (on: boolean) => {
    setBetaBusy(true);
    setBetaError(null);
    try {
      await api.setAppSetting(CONNECTORS_BETA_KEY, on ? "1" : "0");
      setBeta(on);
      if (on) setState("loading");
      await load();
    } catch (e) {
      setBetaError(String(e));
    } finally {
      setBetaBusy(false);
    }
  };

  /** Put a connector the backend returned into the list (or add it). */
  const replace = useCallback((next: Connector) => {
    reviewCounts.current.set(next.id, next.reviewCount);
    setOverview((o) => {
      if (!o) return o;
      const has = o.connectors.some((c) => c.id === next.id);
      return {
        ...o,
        connectors: has
          ? o.connectors.map((c) => (c.id === next.id ? next : c))
          : [...o.connectors, next],
      };
    });
  }, []);

  const setEnabled = async (c: Connector, on: boolean) => {
    setRowBusy((s) => new Set(s).add(c.id));
    setRowError(null);
    try {
      // Off applies at once; on asks in a native dialog (a no keeps it off).
      const r = await api.setConnectorEnabled(c.id, on);
      if (r.connector) replace(r.connector);
      if (r.saved) announce(`${c.name} turned ${on ? "on" : "off"}`);
    } catch (e) {
      setRowError({ id: c.id, error: String(e) });
    } finally {
      setRowBusy((s) => {
        const next = new Set(s);
        next.delete(c.id);
        return next;
      });
    }
  };

  /** Added or edited: show its detail page. */
  const done = (c: Connector, notice: string | null) => {
    setSheet(null);
    replace(c);
    setView({ id: c.id, notice });
    announce(c.status === "connected" ? `${c.name} connected` : `${c.name} added`);
    void load();
  };

  const connectors = overview?.connectors ?? [];
  const current = view ? (connectors.find((c) => c.id === view.id) ?? null) : null;
  const editing =
    sheet && (sheet.kind === "custom" || sheet.kind === "local") && sheet.editId
      ? (connectors.find((c) => c.id === sheet.editId) ?? null)
      : null;
  const addMenu = [
    { label: "Custom (web address)", onSelect: () => setSheet({ kind: "custom" }) },
    { label: "Local (program on this computer)", onSelect: () => setSheet({ kind: "local" }) },
    { label: "Import from Claude Desktop", onSelect: () => setSheet({ kind: "import" }) },
  ];
  const sheetCallbacks = { onClose: () => setSheet(null), onDone: done, onChanged: replace };

  return (
    <>
      <div role="status" aria-live="polite" className="sr-only">
        {announcement}
      </div>
      <SettingsGroup>
        <SettingsRow
          title="Let Ask use connectors (beta)"
          helper={
            <>
              <span style={{ display: "block" }}>
                Let Ask and Enhance check other apps and, if you allow it, make changes there.
              </span>
              <span style={{ display: "block", marginTop: 2 }} className="text-text-3">
                Connectors use MCP, an open standard for linking AI to apps.{" "}
                <a
                  href="https://modelcontextprotocol.io"
                  target="_blank"
                  rel="noreferrer"
                  className="text-primary-text underline"
                >
                  Learn more
                </a>
              </span>
            </>
          }
          control={
            <Switch
              label="Let Ask use connectors (beta)"
              checked={beta === true}
              disabled={beta === null || betaBusy}
              onChange={(on) => void toggleBeta(on)}
            />
          }
        >
          {betaError && <ErrorText error={betaError} />}
        </SettingsRow>
      </SettingsGroup>

      {beta &&
        (current ? (
          <ConnectorDetail
            key={current.id}
            connector={current}
            badge={connectorBadge(current, directory)}
            initialNotice={view?.notice ?? null}
            onBack={() => {
              focusRow.current = current.id;
              setView(null);
            }}
            onChanged={replace}
            onRemoved={(c) => {
              setView(null);
              setOverview((o) =>
                o ? { ...o, connectors: o.connectors.filter((x) => x.id !== c.id) } : o,
              );
              announce(`${c.name} disconnected`);
              void load();
            }}
            onEdit={() =>
              setSheet({ kind: current.kind === "local" ? "local" : "custom", editId: current.id })
            }
            announce={announce}
          />
        ) : (
          <div ref={listRef} style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <div className="flex flex-wrap items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => setSheet({ kind: "browse" })}>
                Browse connectors
              </Button>
              <MenuButton label="Add connector" items={addMenu} />
            </div>
            {state === "loading" && (
              <SettingsGroup aria-busy="true">
                <span className="sr-only">Loading connectors…</span>
                {[0, 1, 2].map((i) => (
                  <div key={i} className="fly-srow" aria-hidden="true">
                    <div
                      style={{
                        height: 34,
                        borderRadius: "var(--radius-md)",
                        background: "var(--surface-3)",
                      }}
                    />
                  </div>
                ))}
              </SettingsGroup>
            )}
            {state === "error" && (
              <SettingsGroup>
                <SettingsRow
                  title="Couldn't load connectors."
                  control={
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        setState("loading");
                        void load();
                      }}
                    >
                      Try again
                    </Button>
                  }
                />
              </SettingsGroup>
            )}
            {state === "ok" && connectors.length === 0 && (
              <SettingsGroup>
                <SettingsRow
                  title="No connectors yet."
                  helper="Connect an app you already use, like Notion or Linear."
                  control={
                    <Button
                      variant="primary"
                      size="sm"
                      onClick={() => setSheet({ kind: "browse" })}
                    >
                      Browse connectors
                    </Button>
                  }
                />
              </SettingsGroup>
            )}
            {state === "ok" && connectors.length > 0 && (
              <SettingsGroup aria-label="Your connectors">
                {connectors.map((c) => {
                  const status = connectorStatus(c);
                  const badge = connectorBadge(c, directory);
                  return (
                    <div key={c.id} className="fly-srow" data-connector-row={c.id}>
                      <div className="flex items-center gap-3">
                        <button
                          type="button"
                          data-open-row
                          className="flex min-w-0 flex-1 cursor-pointer items-center gap-3 rounded-lg text-left focus-visible:shadow-[var(--focus-ring)] focus-visible:outline-none"
                          style={{ background: "none", border: 0, padding: 0, color: "inherit" }}
                          onClick={() => setView({ id: c.id, notice: null })}
                        >
                          <ConnectorIcon name={c.name} />
                          <span
                            style={{
                              display: "flex",
                              flexDirection: "column",
                              minWidth: 0,
                              gap: 1,
                            }}
                          >
                            <span className="flex flex-wrap items-center gap-2">
                              <span
                                style={{ fontSize: 14, fontWeight: 600, overflowWrap: "anywhere" }}
                              >
                                {c.name}
                              </span>
                              {badge && <Badge>{badge}</Badge>}
                            </span>
                            <span
                              className={status.problem ? "text-warning-text" : "text-text-2"}
                              style={{ ...SMALL, display: "flex", alignItems: "center", gap: 5 }}
                            >
                              {status.problem && (
                                <TriangleAlert size={13} strokeWidth={1.9} aria-hidden="true" />
                              )}
                              {status.text}
                            </span>
                          </span>
                        </button>
                        <Switch
                          label={`Use ${c.name}`}
                          checked={c.enabled}
                          disabled={rowBusy.has(c.id) || (!c.approved && !c.enabled)}
                          title={
                            !c.approved && !c.enabled ? "Edit it to approve it again." : undefined
                          }
                          onChange={(on) => void setEnabled(c, on)}
                        />
                      </div>
                      {rowError?.id === c.id && (
                        <div style={{ marginTop: 8 }}>
                          <ErrorText error={rowError.error} />
                        </div>
                      )}
                    </div>
                  );
                })}
              </SettingsGroup>
            )}
          </div>
        ))}

      {sheet?.kind === "browse" && (
        <BrowseSheet
          directory={directory}
          connectors={connectors}
          onCustom={() => setSheet({ kind: "custom" })}
          {...sheetCallbacks}
        />
      )}
      {sheet?.kind === "custom" && <CustomSheet editing={editing} {...sheetCallbacks} />}
      {sheet?.kind === "local" && <LocalSheet editing={editing} {...sheetCallbacks} />}
      {sheet?.kind === "import" && (
        <ImportSheet
          onClose={() => setSheet(null)}
          onDone={(n) => {
            setSheet(null);
            if (n > 0) announce(`${n} connector${n === 1 ? "" : "s"} imported`);
            void load();
          }}
        />
      )}
    </>
  );
}

/** Ids come from the backend; quote them for an attribute selector. */
function CSS_ID(id: string): string {
  return id.replace(/["\\]/g, "\\$&");
}
