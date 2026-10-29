import { useEffect, useId, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { ArrowLeft, Lock, MoreHorizontal, Trash2, TriangleAlert } from "lucide-react";
import { api, CONNECTORS_SETTLED_EVENT } from "../api";
import {
  ALWAYS_ALLOW_WRITE_WARNING,
  PERMISSIONS,
  PERMISSION_LABELS,
  SAVED_IN_KEYCHAIN,
  UNLABELLED_CHIP,
  activityDetail,
  activityResult,
  allowedPermissions,
  connectorStatus,
  formatTime,
  groupTools,
  groupValue,
  groupValueLabel,
  isProblem,
  loosens,
  needsReview,
  planGroupChange,
  argsTemplate,
  canTry,
  parseArgs,
  prettyArgs,
  relativeTime,
  reviewBanner,
  toolStatusChip,
} from "../connectorsModel";
import type {
  Connector,
  ConnectorCallLogEntry,
  ConnectorPermissionChange,
  ConnectorTestResult,
  ConnectorTool,
} from "../types";
import { useConfirm } from "./confirm";
import {
  COLUMN,
  Callout,
  ConnectorIcon,
  ErrorText,
  MONO,
  MenuButton,
  PRE,
  SMALL,
  SignInWaiting,
  useSignIn,
} from "./connectorParts";
import { Badge, Button, Checkbox, Modal, Segmented, SettingsGroup } from "./ui";

/* A connector's detail page: header with status and the ⋯ menu (Reconnect,
   Edit, Disconnect), then About · Tools · Activity. Loosening a tool's
   permission and approving a changed tool are confirmed by the backend in a
   native dialog; tightening applies at once. Server text is plain text. */

type Tab = "about" | "tools" | "activity";
const TABS: { id: Tab; label: string }[] = [
  { id: "about", label: "About" },
  { id: "tools", label: "Tools" },
  { id: "activity", label: "Activity" },
];

export interface DetailProps {
  connector: Connector;
  badge: "Local" | "Custom" | null;
  /** An error from the step that opened this page (listing tools, sign-in). */
  initialNotice: string | null;
  onBack: () => void;
  onChanged: (c: Connector) => void;
  onRemoved: (c: Connector) => void;
  onEdit: () => void;
  /** Says something in the section's polite live region. */
  announce: (text: string) => void;
}

export default function ConnectorDetail({
  connector: c,
  badge,
  initialNotice,
  onBack,
  onChanged,
  onRemoved,
  onEdit,
  announce,
}: DetailProps) {
  const confirm = useConfirm();
  const [tab, setTab] = useState<Tab>("tools");
  const [notice, setNotice] = useState<string | null>(initialNotice);
  const [info, setInfo] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [pending, setPending] = useState<ReadonlySet<string>>(() => new Set());
  const [review, setReview] = useState<string | null>(null);
  const signIn = useSignIn(onChanged);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const tabsId = useId();
  const status = connectorStatus(c);
  const oauth = c.auth.kind === "oauth";

  // Opening the page moves focus to its heading.
  useEffect(() => {
    headingRef.current?.focus();
  }, []);

  const run = async (key: string, fn: () => Promise<void>) => {
    setBusy(key);
    setNotice(null);
    setInfo(null);
    try {
      await fn();
    } catch (e) {
      setNotice(String(e));
    } finally {
      setBusy(null);
    }
  };

  const sync = () =>
    run("sync", async () => {
      const next = await api.syncConnectorTools(c.id);
      onChanged(next);
      setInfo(`Checked. ${next.tools.length} tool${next.tools.length === 1 ? "" : "s"}.`);
      if (next.reviewCount > c.reviewCount)
        announce(`${next.reviewCount} tool${next.reviewCount === 1 ? "" : "s"} changed`);
    });

  const doSignIn = async () => {
    setNotice(null);
    setInfo(null);
    const s = await signIn.start(c.id);
    if (s.cancelled) return;
    if (s.error) setNotice(s.error);
    else announce(`${c.name} connected`);
  };

  const signOut = () =>
    run("signout", async () => {
      onChanged(await api.signOutConnector(c.id));
    });

  const disconnect = async () => {
    const ok = await confirm({
      title: `Disconnect ${c.name}?`,
      message: "Your sign-in is removed from this computer.",
      confirmLabel: "Disconnect",
      tone: "danger",
    });
    if (!ok) return;
    await run("remove", async () => {
      await api.removeConnector(c.id);
      onRemoved(c);
    });
  };

  /** Tightening shows at once; loosening waits for the native dialog. */
  const setPermissions = async (changes: ConnectorPermissionChange[]) => {
    if (changes.length === 0) return;
    const byName = new Map(c.tools.map((t) => [t.name, t]));
    const tighten = changes.filter((ch) => {
      const t = byName.get(ch.tool);
      return t && !loosens(t.permission, ch.permission);
    });
    const loosening = tighten.length < changes.length;
    if (tighten.length > 0) {
      const now = new Map(tighten.map((ch) => [ch.tool, ch.permission]));
      onChanged({
        ...c,
        tools: c.tools.map((t) => (now.has(t.name) ? { ...t, permission: now.get(t.name)! } : t)),
      });
    }
    setPending(new Set(changes.map((ch) => ch.tool)));
    setNotice(null);
    try {
      const r = await api.setConnectorToolPermissions(c.id, changes);
      if (r.connector) onChanged(r.connector);
      // Declined in the native dialog: the old value stays, said quietly.
      if (loosening && !r.saved) announce("Kept as it was.");
    } catch (e) {
      onChanged(c);
      setNotice(String(e));
    } finally {
      setPending(new Set());
    }
  };

  const onTabKey = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const i = TABS.findIndex((t) => t.id === tab);
    let next = -1;
    if (e.key === "ArrowRight") next = (i + 1) % TABS.length;
    else if (e.key === "ArrowLeft") next = (i - 1 + TABS.length) % TABS.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = TABS.length - 1;
    if (next < 0) return;
    e.preventDefault();
    setTab(TABS[next].id);
    document.getElementById(`${tabsId}-${TABS[next].id}`)?.focus();
  };

  const reviewList = c.tools.filter(needsReview);

  return (
    <div style={{ ...COLUMN, gap: 10 }} data-connector-detail={c.id}>
      <div>
        <Button
          variant="ghost"
          size="sm"
          startIcon={<ArrowLeft size={14} strokeWidth={1.75} />}
          onClick={onBack}
        >
          Connectors
        </Button>
      </div>
      <SettingsGroup>
        <div className="fly-srow" style={{ ...COLUMN, gap: 12 }}>
          <div className="flex flex-wrap items-start gap-3">
            <ConnectorIcon name={c.name} />
            <div style={{ flex: "1 1 200px", minWidth: 0 }}>
              <div className="flex flex-wrap items-center gap-2">
                <h3
                  ref={headingRef}
                  tabIndex={-1}
                  style={{
                    margin: 0,
                    fontSize: 17,
                    fontWeight: 700,
                    outline: "none",
                    overflowWrap: "anywhere",
                  }}
                >
                  {c.name}
                </h3>
                {badge && <Badge>{badge}</Badge>}
              </div>
              <p
                className={status.problem ? "text-warning-text" : "text-text-2"}
                style={{ ...SMALL, display: "flex", alignItems: "center", gap: 6 }}
              >
                {status.problem && <TriangleAlert size={13} strokeWidth={1.9} aria-hidden="true" />}
                {status.text}
              </p>
            </div>
            <MenuButton
              label={<MoreHorizontal size={16} strokeWidth={1.9} aria-hidden="true" />}
              ariaLabel={`More for ${c.name}`}
              icon={false}
              disabled={busy !== null}
              items={[
                {
                  label: "Reconnect",
                  onSelect: () => void (oauth ? doSignIn() : sync()),
                },
                { label: "Edit", onSelect: onEdit },
                { label: "Disconnect", tone: "danger", onSelect: () => void disconnect() },
              ]}
            />
          </div>

          {signIn.waiting && <SignInWaiting onCancel={signIn.cancel} />}
          {!signIn.waiting && c.status === "needs_sign_in" && (
            <Callout icon={<Lock size={15} strokeWidth={1.75} />}>
              <div className="flex flex-wrap items-center gap-3">
                <span>Sign in to use {c.name}. Sign-in happens in your browser.</span>
                <Button variant="primary" size="xs" onClick={() => void doSignIn()}>
                  Sign in
                </Button>
              </div>
            </Callout>
          )}
          {c.reviewCount > 0 && (
            <Callout tone="warning" icon={<TriangleAlert size={15} strokeWidth={1.75} />}>
              <div className="flex flex-wrap items-center gap-3">
                <span>{reviewBanner(c.reviewCount)}</span>
                <Button
                  variant="outline"
                  size="xs"
                  onClick={() => setReview(reviewList[0]?.name ?? null)}
                >
                  Review
                </Button>
              </div>
            </Callout>
          )}
          {notice && <ErrorText error={notice} />}
          {info && (
            <p role="status" className="text-text-2" style={SMALL}>
              {info}
            </p>
          )}
        </div>

        <div className="fly-srow" style={{ paddingTop: 10, paddingBottom: 10 }}>
          <div
            role="tablist"
            aria-label={`${c.name} sections`}
            className="flex gap-1"
            onKeyDown={onTabKey}
          >
            {TABS.map((t) => (
              <button
                key={t.id}
                id={`${tabsId}-${t.id}`}
                type="button"
                role="tab"
                aria-selected={tab === t.id}
                aria-controls={`${tabsId}-${t.id}-panel`}
                tabIndex={tab === t.id ? 0 : -1}
                className="fly-settings-navitem"
                style={
                  tab === t.id
                    ? { background: "var(--primary-soft)", color: "var(--primary-text)" }
                    : undefined
                }
                onClick={() => setTab(t.id)}
              >
                {t.label}
              </button>
            ))}
          </div>
        </div>
        <div
          className="fly-srow"
          role="tabpanel"
          id={`${tabsId}-${tab}-panel`}
          aria-labelledby={`${tabsId}-${tab}`}
        >
          {tab === "about" && (
            <AboutTab
              c={c}
              busy={busy !== null || signIn.waiting}
              onSignIn={() => void doSignIn()}
              onSignOut={() => void signOut()}
              onSync={() => void sync()}
            />
          )}
          {tab === "tools" && (
            <ToolsTab
              connectorId={c.id}
              c={c}
              pending={pending}
              onSet={(changes) => void setPermissions(changes)}
              onReview={(name) => setReview(name)}
              onSync={() => void sync()}
              busy={busy !== null}
            />
          )}
          {tab === "activity" && <ActivityTab c={c} />}
        </div>
      </SettingsGroup>
      {review !== null && (
        <ReviewModal
          c={c}
          startAt={review}
          onClose={() => setReview(null)}
          onChanged={onChanged}
          announce={announce}
        />
      )}
    </div>
  );
}

/* ------------------------------------------------------------- about */

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex flex-wrap items-start gap-x-4 gap-y-1" style={{ padding: "6px 0" }}>
      <dt className="text-text-3" style={{ ...SMALL, width: 140, flex: "none" }}>
        {label}
      </dt>
      <dd style={{ ...SMALL, margin: 0, flex: "1 1 240px", minWidth: 0 }} className="text-text">
        {children}
      </dd>
    </div>
  );
}

function AboutTab({
  c,
  busy,
  onSignIn,
  onSignOut,
  onSync,
}: {
  c: Connector;
  busy: boolean;
  onSignIn: () => void;
  onSignOut: () => void;
  onSync: () => void;
}) {
  const kv = c.transport.kind === "http" ? c.transport.headers : c.transport.env;
  const [now, setNow] = useState(0);
  useEffect(() => setNow(Date.now()), [c.lastSync]);
  const secretSet = (name: string) => !!c.secrets.values.find((v) => v.name === name)?.set;
  return (
    <dl style={{ margin: 0 }}>
      <Row label="Sign-in">
        {c.auth.kind === "oauth" ? (
          <span className="flex flex-wrap items-center gap-2">
            {c.signedIn ? `Signed in to ${c.destination}` : "Not signed in"}
            {c.signedIn ? (
              <Button variant="outline" size="xs" disabled={busy} onClick={onSignOut}>
                Sign out
              </Button>
            ) : (
              <Button variant="primary" size="xs" disabled={busy} onClick={onSignIn}>
                Sign in
              </Button>
            )}
          </span>
        ) : c.auth.kind === "api_key" ? (
          `API key or token · ${c.secrets.apiKeySet ? SAVED_IN_KEYCHAIN : "Not set"}`
        ) : (
          "No sign-in"
        )}
      </Row>
      {c.transport.kind === "http" ? (
        <Row label="Address">
          <span style={{ ...MONO, overflowWrap: "anywhere" }}>{c.transport.url}</span>
        </Row>
      ) : (
        <Row label="Will run">
          <span style={{ ...MONO, overflowWrap: "anywhere" }}>
            {c.willRun ?? c.transport.command}
          </span>
        </Row>
      )}
      {kv.length > 0 && (
        <Row label={c.transport.kind === "http" ? "Request headers" : "Settings"}>
          <ul style={{ margin: 0, padding: 0, listStyle: "none" }}>
            {kv.map((h) => (
              <li key={h.name} style={{ overflowWrap: "anywhere" }}>
                <span style={MONO}>{h.name}</span>:{" "}
                {h.value !== null ? (
                  <span style={MONO}>{h.value}</span>
                ) : secretSet(h.name) ? (
                  SAVED_IN_KEYCHAIN
                ) : (
                  "Not set"
                )}
              </li>
            ))}
          </ul>
        </Row>
      )}
      <Row label="Approval">
        {c.approved ? (
          <>
            Approved · config <span style={MONO}>{c.configHash}</span>
          </>
        ) : (
          "Not approved — edit it to approve again"
        )}
      </Row>
      <Row label="Tools checked">
        <span className="flex flex-wrap items-center gap-2">
          {c.lastSync === null ? "Not yet" : now ? relativeTime(c.lastSync, now) : "…"}
          <Button variant="outline" size="xs" disabled={busy} onClick={onSync}>
            Check again
          </Button>
        </span>
      </Row>
    </dl>
  );
}

/* ------------------------------------------------------------- tools */

function ToolsTab({
  connectorId,
  c,
  pending,
  busy,
  onSet,
  onReview,
  onSync,
}: {
  connectorId: string;
  c: Connector;
  pending: ReadonlySet<string>;
  busy: boolean;
  onSet: (changes: ConnectorPermissionChange[]) => void;
  onReview: (tool: string) => void;
  onSync: () => void;
}) {
  const { read, write } = groupTools(c.tools);
  if (c.tools.length === 0) {
    return (
      <div style={COLUMN}>
        <p className="text-text-2" style={SMALL}>
          {c.signedIn === false ? "Sign in to see its tools." : "No tools yet."}
        </p>
        {c.signedIn !== false && (
          <div>
            <Button variant="outline" size="sm" disabled={busy} onClick={onSync}>
              Check again
            </Button>
          </div>
        )}
      </div>
    );
  }
  return (
    <div style={{ ...COLUMN, gap: 16 }}>
      <p className="text-text-2" style={SMALL}>
        Choose when the AI may use each tool.
      </p>
      <ToolGroup
        connectorId={connectorId}
        title="Read-only tools"
        tools={read}
        pending={pending}
        onSet={onSet}
        onReview={onReview}
      />
      <ToolGroup
        connectorId={connectorId}
        title="Tools that can change or delete"
        tools={write}
        pending={pending}
        onSet={onSet}
        onReview={onReview}
      />
    </div>
  );
}

function ToolGroup({
  connectorId,
  title,
  tools,
  pending,
  onSet,
  onReview,
}: {
  connectorId: string;
  title: string;
  tools: ConnectorTool[];
  pending: ReadonlySet<string>;
  onSet: (changes: ConnectorPermissionChange[]) => void;
  onReview: (tool: string) => void;
}) {
  const [open, setOpen] = useState(true);
  const headingId = useId();
  const listId = useId();
  if (tools.length === 0) return null;
  const value = groupValue(tools);
  const label = groupValueLabel(value);
  return (
    <section aria-labelledby={headingId} data-group={title}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h4 id={headingId} style={{ margin: 0, fontSize: 13.5, fontWeight: 600 }}>
          <button
            type="button"
            className="cursor-pointer text-text"
            style={{ background: "none", border: 0, padding: 0, font: "inherit" }}
            aria-expanded={open}
            aria-controls={listId}
            onClick={() => setOpen((o) => !o)}
          >
            {open ? "▾" : "▸"} {title} ({tools.length})
          </button>
        </h4>
        <MenuButton
          label={label}
          ariaLabel={`${title}: ${label}`}
          disabled={pending.size > 0}
          items={PERMISSIONS.map((p) => {
            const plan = planGroupChange(tools, p);
            return {
              label: PERMISSION_LABELS[p],
              checked: value === p,
              disabled: plan.length === 0 && value !== p,
              onSelect: () => onSet(plan),
            };
          })}
        />
      </div>
      {open && (
        <div id={listId}>
          {tools.map((t) => (
            <ToolRow
              key={t.name}
              connectorId={connectorId}
              tool={t}
              busy={pending.has(t.name)}
              onSet={onSet}
              onReview={() => onReview(t.name)}
            />
          ))}
        </div>
      )}
    </section>
  );
}

function ToolRow({
  connectorId,
  tool,
  busy,
  onSet,
  onReview,
}: {
  connectorId: string;
  tool: ConnectorTool;
  busy: boolean;
  onSet: (changes: ConnectorPermissionChange[]) => void;
  onReview: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [trying, setTrying] = useState(false);
  const label = tool.title ?? tool.name;
  const allowed = allowedPermissions(tool);
  const chip = toolStatusChip(tool);
  const long = tool.description.length > 160 || tool.description.split("\n").length > 2;
  return (
    <div
      data-tool={tool.name}
      style={{ padding: "12px 0", borderTop: "1px solid var(--line-2)", ...COLUMN }}
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div
          style={{
            flex: "1 1 240px",
            minWidth: 0,
            display: "flex",
            flexDirection: "column",
            gap: 4,
          }}
        >
          <p style={{ margin: 0, fontSize: 13.5, fontWeight: 600, overflowWrap: "anywhere" }}>
            {label}{" "}
            {tool.title && (
              <span className="text-text-3" style={{ ...MONO, fontWeight: 400 }}>
                {tool.name}
              </span>
            )}
          </p>
          {(!tool.labelled || tool.destructive || chip) && (
            <div className="flex flex-wrap gap-1.5">
              {!tool.labelled && <Badge tone="warning">{UNLABELLED_CHIP}</Badge>}
              {tool.destructive && (
                <Badge tone="danger">
                  <Trash2 size={11} strokeWidth={2} aria-hidden="true" /> Destructive
                </Badge>
              )}
              {chip && (
                <Badge
                  tone={
                    tool.status === "unusable"
                      ? "danger"
                      : tool.status === "missing"
                        ? "neutral"
                        : "warning"
                  }
                >
                  {chip}
                </Badge>
              )}
            </div>
          )}
          {tool.description && (
            <div>
              <p
                className="text-text-2"
                style={{
                  ...SMALL,
                  whiteSpace: "pre-wrap",
                  overflowWrap: "anywhere",
                  ...(long && !expanded
                    ? {
                        display: "-webkit-box",
                        WebkitLineClamp: 2,
                        WebkitBoxOrient: "vertical",
                        overflow: "hidden",
                      }
                    : null),
                }}
              >
                {tool.description}
              </p>
              {long && (
                <button
                  type="button"
                  className="cursor-pointer text-primary-text"
                  style={{ ...SMALL, fontSize: 12, background: "none", border: 0, padding: 0 }}
                  aria-expanded={expanded}
                  onClick={() => setExpanded((x) => !x)}
                >
                  {expanded ? "Show less" : "Show more"}
                </button>
              )}
            </div>
          )}
        </div>
        <Segmented
          label={`Permission for ${label}`}
          value={tool.permission}
          disabled={busy}
          onChange={(p) => onSet([{ tool: tool.name, permission: p }])}
          options={PERMISSIONS.map((p) => ({
            value: p,
            label: PERMISSION_LABELS[p],
            disabled: !allowed.includes(p),
            title: allowed.includes(p) ? undefined : "Review it first",
          }))}
        />
      </div>
      {tool.kind === "write" && tool.permission === "always_allow" && (
        <p
          role="note"
          className="text-warning-text"
          style={{ ...SMALL, display: "flex", gap: 6, alignItems: "flex-start" }}
        >
          <TriangleAlert size={14} strokeWidth={1.9} aria-hidden="true" style={{ marginTop: 2 }} />
          {ALWAYS_ALLOW_WRITE_WARNING}
        </p>
      )}
      {(needsReview(tool) || canTry(tool)) && (
        <div className="flex flex-wrap gap-2">
          {needsReview(tool) && (
            <Button variant="outline" size="xs" onClick={onReview}>
              Review
            </Button>
          )}
          {canTry(tool) && (
            <Button
              variant="ghost"
              size="xs"
              aria-expanded={trying}
              aria-label={`Try ${label}`}
              onClick={() => setTrying((t) => !t)}
            >
              {trying ? "Close" : "Try…"}
            </Button>
          )}
        </div>
      )}
      {trying && canTry(tool) && <TryTool connectorId={connectorId} tool={tool} />}
    </div>
  );
}

/** One real call from Settings: details prefilled from the schema, then the
 *  same rules as Ask (an approval card unless the tool is on Always allow).
 *  The result is the server's text, shown as plain text. */
function TryTool({ connectorId, tool }: { connectorId: string; tool: ConnectorTool }) {
  const [text, setText] = useState(() => prettyArgs(argsTemplate(tool.inputSchema)));
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<ConnectorTestResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const parsed = parseArgs(text);
  const argsId = useId();
  const automatic = tool.permission === "always_allow";

  const run = async () => {
    if (!parsed.ok || running) return;
    setRunning(true);
    setResult(null);
    setError(null);
    try {
      setResult(await api.testConnectorTool(connectorId, tool.name, parsed.value));
    } catch (e) {
      setError(String(e));
    } finally {
      setRunning(false);
    }
  };

  return (
    <div
      data-try={tool.name}
      style={{
        ...COLUMN,
        padding: 12,
        border: "1px solid var(--line-2)",
        borderRadius: "var(--radius-lg)",
        background: "var(--surface-2)",
      }}
    >
      <p className="text-text-2" style={SMALL}>
        {automatic
          ? "This makes one real call. The tool is on Always allow, so it runs without asking."
          : "This makes one real call. It goes through the same approval card as Ask: you choose there."}
      </p>
      <label htmlFor={argsId} className="fly-field-label">
        Details (JSON)
      </label>
      <textarea
        id={argsId}
        value={text}
        rows={Math.min(10, Math.max(3, text.split("\n").length))}
        spellCheck={false}
        disabled={running}
        aria-invalid={!parsed.ok || undefined}
        onChange={(e) => setText(e.target.value)}
        style={{
          ...PRE,
          width: "100%",
          resize: "vertical",
          outline: "none",
          whiteSpace: "pre",
          borderColor: parsed.ok ? "var(--line)" : "var(--error)",
        }}
      />
      {!parsed.ok && (
        <p className="text-error-text" style={SMALL}>
          {parsed.error}
        </p>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="primary"
          size="sm"
          disabled={running || !parsed.ok}
          onClick={() => void run()}
        >
          {running ? (automatic ? "Running…" : "Waiting…") : "Run"}
        </Button>
        {running && !automatic && (
          <span role="status" className="text-text-2" style={SMALL}>
            Waiting for your decision on the approval card…
          </span>
        )}
      </div>
      {result && (
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <span
            className={result.ok ? "text-success-text" : "text-error-text"}
            style={{ ...SMALL, fontWeight: 600 }}
          >
            {result.ok ? "Result" : "No result"}
          </span>
          <pre
            data-result={result.ok ? "ok" : "error"}
            style={{ ...PRE, borderColor: result.ok ? "var(--line-2)" : "var(--error)" }}
          >
            {result.text}
          </pre>
        </div>
      )}
      {error && <ErrorText error={error} />}
    </div>
  );
}

/* ------------------------------------------------------------- review */

const FIELD_NAMES: Record<string, string> = {
  description: "Description",
  inputs: "Inputs",
  inputSchema: "Inputs",
  input_schema: "Inputs",
  title: "Name",
};
const fieldName = (f: string) =>
  Object.prototype.hasOwnProperty.call(FIELD_NAMES, f) ? FIELD_NAMES[f] : f;

function ReviewModal({
  c,
  startAt,
  onClose,
  onChanged,
  announce,
}: {
  c: Connector;
  startAt: string;
  onClose: () => void;
  onChanged: (c: Connector) => void;
  announce: (text: string) => void;
}) {
  const list = c.tools.filter(needsReview);
  const [name, setName] = useState(startAt);
  const [showUnchanged, setShowUnchanged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const tool = list.find((t) => t.name === name) ?? list[0] ?? null;

  useEffect(() => {
    if (!tool) onClose();
  }, [tool, onClose]);
  if (!tool) return null;

  const index = list.indexOf(tool);
  const label = tool.title ?? tool.name;
  const changedFields = new Set(tool.diff.map((d) => fieldName(d.field)));
  const unchanged: { label: string; text: string }[] = [];
  if (tool.status === "changed") {
    if (!changedFields.has("Name") && tool.title)
      unchanged.push({ label: "Name", text: tool.title });
    if (!changedFields.has("Description"))
      unchanged.push({ label: "Description", text: tool.description });
    if (!changedFields.has("Inputs"))
      unchanged.push({ label: "Inputs", text: prettyArgs(tool.inputSchema) });
  }

  const approve = async () => {
    if (!tool.reviewHash) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.approveConnectorTool(c.id, tool.name, tool.reviewHash);
      if (r.connector) onChanged(r.connector);
      if (!r.saved) return; // declined: it stays blocked
      announce(`${label} approved`);
      const rest = (r.connector?.tools ?? []).filter(needsReview);
      if (rest.length === 0) onClose();
      else {
        setName(rest[Math.min(index, rest.length - 1)].name);
        setShowUnchanged(false);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open
      onClose={onClose}
      title="Review changes"
      width={620}
      closeOnOverlay={false}
      footer={
        <>
          <Button variant="outline" size="sm" data-autofocus onClick={onClose}>
            Keep blocked
          </Button>
          <Button variant="primary" size="sm" disabled={busy} onClick={() => void approve()}>
            {busy ? "Approving…" : "Approve changes"}
          </Button>
        </>
      }
    >
      <div style={{ ...COLUMN, gap: 12 }} data-review-tool={tool.name}>
        {list.length > 1 && (
          <p className="text-text-3" style={SMALL}>
            {index + 1} of {list.length}
          </p>
        )}
        <h3 style={{ margin: 0, fontSize: 15, fontWeight: 700, overflowWrap: "anywhere" }}>
          {label}{" "}
          {tool.title && (
            <span className="text-text-3" style={{ ...MONO, fontWeight: 400 }}>
              {tool.name}
            </span>
          )}
        </h3>
        <p className="text-text-2" style={SMALL}>
          {tool.status === "new"
            ? `${c.name} added this tool after you approved it.`
            : `${c.name} changed this tool since you approved it.`}
        </p>
        {tool.status === "new" ? (
          <>
            <Field label="Description" text={tool.description} />
            <Field label="Inputs" text={prettyArgs(tool.inputSchema)} />
          </>
        ) : (
          <>
            {tool.diff.map((d, i) => (
              <div key={`${i}-${d.field}`} style={COLUMN}>
                <p style={{ ...SMALL, fontWeight: 600 }}>{fieldName(d.field)}</p>
                <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <span className="text-text-3" style={{ ...SMALL, fontSize: 12 }}>
                    Was:
                  </span>
                  <pre style={PRE} data-diff="before">
                    {d.before}
                  </pre>
                  <span className="text-text-3" style={{ ...SMALL, fontSize: 12 }}>
                    Now:
                  </span>
                  <pre style={{ ...PRE, borderColor: "var(--warning)" }} data-diff="after">
                    {d.after}
                  </pre>
                </div>
              </div>
            ))}
            {unchanged.length > 0 && (
              <div style={COLUMN}>
                <div>
                  <button
                    type="button"
                    className="cursor-pointer text-primary-text"
                    style={{ ...SMALL, background: "none", border: 0, padding: 0 }}
                    aria-expanded={showUnchanged}
                    onClick={() => setShowUnchanged((s) => !s)}
                  >
                    {showUnchanged ? "Hide unchanged" : "Show unchanged"}
                  </button>
                </div>
                {showUnchanged &&
                  unchanged.map((u) => <Field key={u.label} label={u.label} text={u.text} />)}
              </div>
            )}
          </>
        )}
        <p className="text-text-3" style={SMALL}>
          If you keep it blocked, the AI can&apos;t use it. Approving moves it to Needs approval;
          you&apos;ll confirm in a system dialog.
        </p>
        {error && <ErrorText error={error} />}
      </div>
    </Modal>
  );
}

function Field({ label, text }: { label: string; text: string }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <span style={{ ...SMALL, fontWeight: 600 }}>{label}</span>
      <pre style={PRE}>{text}</pre>
    </div>
  );
}

/* ------------------------------------------------------------- activity */

function ActivityTab({ c }: { c: Connector }) {
  const confirm = useConfirm();
  const [log, setLog] = useState<ConnectorCallLogEntry[] | null>(null);
  const [readAt, setReadAt] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [problemsOnly, setProblemsOnly] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | null = null;
    const load = () =>
      api
        .connectorCallLog()
        .then((l) => {
          if (!live) return;
          setLog(l);
          setReadAt(Date.now());
          setError(null);
        })
        .catch((e) => {
          if (live) setError(String(e));
        });
    void load();
    // A call just finished: show it.
    listen(CONNECTORS_SETTLED_EVENT, () => {
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
  }, []);

  const clear = async () => {
    const ok = await confirm({
      title: "Clear connector activity?",
      message:
        "This clears the activity of every connector. Connectors and their tools stay as they are.",
      confirmLabel: "Clear activity",
      tone: "danger",
    });
    if (!ok) return;
    setBusy(true);
    try {
      await api.clearConnectorCallLog();
      setLog([]);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const mine = (log ?? []).filter((e) => e.serverId === c.id).reverse();
  const rows = problemsOnly ? mine.filter(isProblem) : mine;
  const th = {
    textAlign: "left" as const,
    fontWeight: 600,
    padding: "6px 8px",
    borderBottom: "1px solid var(--line)",
  };
  const td = {
    padding: "6px 8px",
    verticalAlign: "top" as const,
    borderBottom: "1px solid var(--line-2)",
  };

  return (
    <div style={{ ...COLUMN, gap: 10 }}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <Checkbox
          label="Problems only"
          checked={problemsOnly}
          onChange={(e) => setProblemsOnly(e.target.checked)}
        />
        {mine.length > 0 && (
          <Button variant="outline" size="xs" disabled={busy} onClick={() => void clear()}>
            Clear activity…
          </Button>
        )}
      </div>
      {error && <ErrorText error={error} />}
      {log === null && !error && (
        <p className="text-text-3" style={SMALL}>
          Loading…
        </p>
      )}
      {log !== null && rows.length === 0 && (
        <p className="text-text-3" style={SMALL}>
          {mine.length === 0 ? "No calls yet." : "No problems."}
        </p>
      )}
      {rows.length > 0 && (
        <div style={{ overflowX: "auto" }}>
          <table
            className="text-text-2"
            style={{ width: "100%", borderCollapse: "collapse", fontSize: 12.5 }}
          >
            <thead className="text-text-3">
              <tr>
                <th style={th}>Time</th>
                <th style={th}>Tool</th>
                <th style={th}>Result</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((e, i) => {
                const detail = activityDetail(e);
                return (
                  <tr key={`${e.at}-${i}`} data-log-row>
                    <td
                      style={{ ...td, whiteSpace: "nowrap" }}
                      title={new Date(e.at * 1000).toLocaleString()}
                    >
                      {formatTime(e.at, readAt)}
                    </td>
                    <td style={{ ...td, ...MONO, overflowWrap: "anywhere" }}>{e.tool}</td>
                    <td style={td}>
                      <span className={isProblem(e) ? "text-warning-text" : "text-text"}>
                        {activityResult(e)}
                      </span>
                      {detail && (
                        <span className="text-text-3" style={{ display: "block", fontSize: 12 }}>
                          {detail}
                        </span>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      <p className="text-text-3" style={{ ...SMALL, fontSize: 12 }}>
        Only tool names and outcomes are kept — never meeting text.
      </p>
    </div>
  );
}
