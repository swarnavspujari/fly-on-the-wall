import { useCallback, useEffect, useId, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { Plus, RefreshCw } from "lucide-react";
import { api, CONNECTORS_CHANGED_EVENT, CONNECTORS_SETTLED_EVENT } from "../api";
import {
  CONNECTORS_BETA_KEY,
  SYSTEM_DIALOG_NOTE,
  argsTemplate,
  draftValue,
  effectiveKind,
  emptyAddDraft,
  formatChars,
  formatDuration,
  grantsTrust,
  kindLabel,
  newestFirst,
  outcomeLabel,
  parseArgs,
  prettyArgs,
  relativeTime,
  toolPermissionChanges,
  toolStatusLabel,
  validateAddDraft,
  type AddDraft,
  type AddErrors,
  type ToolDraft,
} from "../connectorsModel";
import type {
  Connector,
  ConnectorCallLogEntry,
  ConnectorTestResult,
  ConnectorTool,
  ConnectorsOverview,
} from "../types";
import { useConfirm } from "./confirm";
import {
  Badge,
  Button,
  Checkbox,
  Input,
  Segmented,
  SettingsGroup,
  SettingsRow,
  Switch,
} from "./ui";

/* Settings › Connectors (beta). Rules this view keeps:
   - It never grants trust itself. Adding a server, turning a tool on,
     marking one read-only and accepting a changed tool are commands the
     backend confirms in a native dialog; a decline there returns
     { saved: false } and shows as a quiet "Not saved.".
   - The only setting it writes is the beta flag (ui.beta.connectors).
   - Server text (tool names, titles, descriptions, diffs, results) is shown
     as plain text only: text nodes and <pre>, never markdown or links. */

const SMALL: CSSProperties = { margin: 0, fontSize: 12, lineHeight: 1.5 };
const MONO: CSSProperties = { fontFamily: "var(--font-mono)", fontSize: 11.5 };
const PRE: CSSProperties = {
  ...MONO,
  margin: 0,
  padding: "8px 10px",
  lineHeight: 1.5,
  whiteSpace: "pre-wrap",
  overflowWrap: "anywhere",
  maxHeight: 220,
  overflow: "auto",
  color: "var(--text)",
  background: "var(--surface-2)",
  border: "1px solid var(--line-2)",
  borderRadius: "var(--radius-md)",
};
const TEXTAREA: CSSProperties = {
  ...MONO,
  width: "100%",
  lineHeight: 1.5,
  color: "var(--text)",
  background: "var(--surface-2)",
  border: "1px solid var(--line)",
  borderRadius: "var(--radius-md)",
  padding: "8px 10px",
  outline: "none",
  resize: "vertical",
  whiteSpace: "pre",
};
const COLUMN: CSSProperties = { display: "flex", flexDirection: "column", gap: 8 };
const BUTTONS = "flex flex-wrap items-center gap-2";

const INTRO =
  "Connectors let Ask look things up in other services, like a CRM or your docs. " +
  "Every call is shown to you first, with its exact arguments, and runs only if you say so. " +
  "Notes you keep local never use connectors.";

type Msg = { text: string; tone: "muted" | "error" | "success" } | null;

function Message({ msg }: { msg: Msg }) {
  if (!msg) return null;
  const cls =
    msg.tone === "error"
      ? "text-error-text"
      : msg.tone === "success"
        ? "text-success-text"
        : "text-text-2";
  return (
    <p
      role={msg.tone === "error" ? "alert" : "status"}
      className={cls}
      style={{ ...SMALL, overflowWrap: "anywhere" }}
    >
      {msg.text}
    </p>
  );
}

/** The Settings section body (the caller draws the heading). */
export default function ConnectorsSection() {
  const [overview, setOverview] = useState<ConnectorsOverview | null>(null);
  // null until known (the switch waits)
  const [beta, setBeta] = useState<boolean | null>(null);
  const [betaBusy, setBetaBusy] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [addNotice, setAddNotice] = useState<Msg>(null);

  const load = useCallback(async () => {
    try {
      const o = await api.connectorsOverview();
      setOverview(o);
      setBeta(o.beta);
      setLoadError(null);
    } catch (e) {
      setLoadError(String(e));
      // The switch still works without the overview.
      const v = await api.getAppSetting(CONNECTORS_BETA_KEY).catch(() => null);
      setBeta(v === "1");
    }
  }, []);

  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | null = null;
    void load();
    // Something changed in the backend (a sync, a sign-in finishing, a tool
    // changing on the server): read everything again.
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

  const toggleBeta = async (on: boolean) => {
    setBetaBusy(true);
    setLoadError(null);
    try {
      await api.setAppSetting(CONNECTORS_BETA_KEY, on ? "1" : "0");
      setBeta(on);
      await load();
    } catch (e) {
      setLoadError(String(e));
    } finally {
      setBetaBusy(false);
    }
  };

  const replace = (next: Connector) =>
    setOverview((o) =>
      o ? { ...o, connectors: o.connectors.map((c) => (c.id === next.id ? next : c)) } : o,
    );

  const added = async (c: Connector) => {
    await load();
    // List its tools right away; they arrive off.
    try {
      const synced = await api.syncConnectorTools(c.id);
      replace(synced);
      setAddNotice({
        text: `Added ${c.name}. Its tools start off: turn on the ones Ask may use.`,
        tone: "success",
      });
    } catch (e) {
      setAddNotice({
        text: `Added ${c.name}, but its tools couldn't be listed: ${String(e)}. Try Refresh tools.`,
        tone: "error",
      });
    }
  };

  const connectors = overview?.connectors ?? [];

  return (
    <>
      <SettingsGroup>
        <SettingsRow
          title="Let Ask use connectors (beta)"
          helper={INTRO}
          control={
            <Switch
              label="Let Ask use connectors (beta)"
              checked={beta === true}
              disabled={beta === null || betaBusy}
              onChange={(on) => void toggleBeta(on)}
            />
          }
        >
          {loadError && <Message msg={{ text: loadError, tone: "error" }} />}
        </SettingsRow>
      </SettingsGroup>

      {beta && (
        <>
          {connectors.map((c) => (
            <ConnectorCard
              key={c.id}
              connector={c}
              onChange={replace}
              onRemoved={() => void load()}
            />
          ))}
          <AddConnector
            empty={connectors.length === 0}
            notice={addNotice}
            onOpen={() => setAddNotice(null)}
            onAdded={added}
          />
          <CallLog />
        </>
      )}
    </>
  );
}

/* ------------------------------------------------------------- one connector */

function ConnectorCard({
  connector: c,
  onChange,
  onRemoved,
}: {
  connector: Connector;
  onChange: (c: Connector) => void;
  onRemoved: () => void;
}) {
  const confirm = useConfirm();
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<Msg>(null);
  const [draft, setDraft] = useState<ToolDraft>(() => new Map());
  const [saveMsg, setSaveMsg] = useState<Msg>(null);

  /** Runs one action at a time; a failure shows next to it. */
  const run = async (key: string, fn: () => Promise<void>, show = setMsg) => {
    setBusy(key);
    show(null);
    try {
      await fn();
    } catch (e) {
      show({ text: String(e), tone: "error" });
    } finally {
      setBusy(null);
    }
  };

  const sync = () =>
    run("sync", async () => {
      const next = await api.syncConnectorTools(c.id);
      onChange(next);
      const fresh = next.tools.filter((t) => !c.tools.some((o) => o.name === t.name)).length;
      setMsg({
        text:
          fresh > 0
            ? `Found ${fresh} new tool${fresh === 1 ? "" : "s"}. New tools start off.`
            : "Tools are up to date.",
        tone: "muted",
      });
    });

  const remove = async () => {
    const ok = await confirm({
      title: `Remove “${c.name}”?`,
      message:
        "Ask stops using it, and its sign-in and saved variables are deleted from your system keychain too.",
      confirmLabel: "Remove connector",
      tone: "danger",
    });
    if (!ok) return;
    await run("remove", async () => {
      await api.removeConnector(c.id);
      onRemoved();
    });
  };

  const signIn = () =>
    run("signin", async () => {
      const next = await api.signInConnector(c.id);
      onChange(next);
      setMsg(next.signedIn ? { text: "Signed in.", tone: "success" } : null);
    });

  const signOut = () =>
    run("signout", async () => {
      onChange(await api.signOutConnector(c.id));
    });

  /* tools: a local draft, saved together */
  const changes = toolPermissionChanges(c.tools, draft);
  const trust = grantsTrust(c.tools, changes);
  const setTool = (t: ConnectorTool, patch: Partial<{ enabled: boolean; readOnly: boolean }>) => {
    setSaveMsg(null);
    setDraft((d) => {
      const next = new Map(d);
      next.set(t.name, { ...draftValue(t, d), ...patch });
      return next;
    });
  };
  const discard = () => {
    setDraft(new Map());
    setSaveMsg(null);
  };
  const save = () =>
    run(
      "save",
      async () => {
        const r = await api.saveConnectorTools(c.id, changes);
        if (r.saved) {
          if (r.connector) onChange(r.connector);
          setDraft(new Map());
          setSaveMsg({ text: "Saved.", tone: "success" });
        } else {
          // declined in the native dialog: keep the draft, change nothing
          setSaveMsg({ text: "Not saved.", tone: "muted" });
        }
      },
      setSaveMsg,
    );
  const forgetDraft = (name: string) =>
    setDraft((d) => {
      if (!d.has(name)) return d;
      const next = new Map(d);
      next.delete(name);
      return next;
    });

  const auth = c.auth.kind === "oauth" ? c.auth : null;

  return (
    <SettingsGroup aria-label={`Connector ${c.name}`} data-connector-id={c.id}>
      <SettingsRow
        title={<span style={{ overflowWrap: "anywhere" }}>{c.name}</span>}
        badge={
          c.approved ? (
            <Badge tone="success">
              Approved · config <span style={MONO}>{c.configHash}</span>
            </Badge>
          ) : (
            <Badge tone="warning">Not approved</Badge>
          )
        }
        helper={
          <>
            <span style={{ ...MONO, overflowWrap: "anywhere" }}>{c.destination}</span>
            {c.lastSync != null && (
              <span className="text-text-3"> · tools checked {relativeTime(c.lastSync)}</span>
            )}
            {!c.approved && (
              <span className="text-warning-text" style={{ display: "block" }}>
                Ask won&apos;t call its tools.
              </span>
            )}
          </>
        }
        control={
          <>
            <Button
              variant="outline"
              size="sm"
              disabled={busy !== null}
              startIcon={<RefreshCw size={13} strokeWidth={1.75} />}
              onClick={() => void sync()}
            >
              {busy === "sync" ? "Refreshing…" : "Refresh tools"}
            </Button>
            <Button
              variant="outline"
              size="sm"
              style={{ color: "var(--error-text)", borderColor: "var(--error)" }}
              disabled={busy !== null}
              onClick={() => void remove()}
            >
              Remove
            </Button>
          </>
        }
      >
        {msg && <Message msg={msg} />}
      </SettingsRow>

      {c.signedIn !== null && (
        <SettingsRow
          title="Sign-in"
          helper={
            busy === "signin" ? (
              <span role="status">Waiting for you to sign in in your browser…</span>
            ) : c.signedIn ? (
              "Signed in"
            ) : (
              "Not signed in. Ask can't use it until you sign in."
            )
          }
          helperTone={busy === "signin" ? "default" : c.signedIn ? "success" : "warning"}
          control={
            c.signedIn ? (
              <Button
                variant="outline"
                size="sm"
                disabled={busy !== null}
                onClick={() => void signOut()}
              >
                Sign out
              </Button>
            ) : (
              <Button
                variant="primary"
                size="sm"
                disabled={busy !== null}
                onClick={() => void signIn()}
              >
                {busy === "signin" ? "Signing in…" : "Sign in"}
              </Button>
            )
          }
        >
          {auth && auth.scopes.length > 0 && (
            <p className="text-text-3" style={SMALL}>
              Asks for: <span style={MONO}>{auth.scopes.join(" ")}</span>
            </p>
          )}
        </SettingsRow>
      )}

      {c.env.map((v) => (
        <EnvRow key={v.name} connectorId={c.id} name={v.name} set={v.set} onChange={onChange} />
      ))}

      {c.tools.length === 0 ? (
        <SettingsRow
          title="Tools"
          helper="None listed yet. Refresh tools to connect and list what it offers."
        />
      ) : (
        c.tools.map((t) => (
          <ToolRow
            key={t.name}
            connectorId={c.id}
            tool={t}
            value={draftValue(t, draft)}
            disabled={busy !== null}
            onSet={(patch) => setTool(t, patch)}
            onChange={onChange}
            onForgetDraft={() => forgetDraft(t.name)}
          />
        ))
      )}

      {(changes.length > 0 || saveMsg) && (
        <div
          className="fly-srow"
          style={{
            display: "flex",
            alignItems: "center",
            flexWrap: "wrap",
            gap: 10,
            background: "var(--surface-2)",
          }}
        >
          <div style={{ flex: "1 1 200px", minWidth: 0 }}>
            {changes.length > 0 && (
              <p className="text-text" style={{ ...SMALL, fontWeight: 600 }}>
                {changes.length} unsaved tool change{changes.length === 1 ? "" : "s"}
              </p>
            )}
            {changes.length > 0 && trust && (
              <p className="text-text-2" style={SMALL}>
                {SYSTEM_DIALOG_NOTE}
              </p>
            )}
            <Message msg={saveMsg} />
          </div>
          {changes.length > 0 && (
            <div className={BUTTONS}>
              <Button variant="ghost" size="sm" disabled={busy !== null} onClick={discard}>
                Discard
              </Button>
              <Button
                variant="primary"
                size="sm"
                disabled={busy !== null}
                onClick={() => void save()}
              >
                {busy === "save" ? "Saving…" : "Save"}
              </Button>
            </div>
          )}
        </div>
      )}
    </SettingsGroup>
  );
}

/* ------------------------------------------------------------- variables */

function EnvRow({
  connectorId,
  name,
  set,
  onChange,
}: {
  connectorId: string;
  name: string;
  set: boolean;
  onChange: (c: Connector) => void;
}) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<Msg>(null);
  const store = async (v: string) => {
    setBusy(true);
    setMsg(null);
    try {
      onChange(await api.setConnectorEnv(connectorId, name, v));
      setValue("");
      setMsg({ text: v ? "Saved to your system keychain." : "Cleared.", tone: "muted" });
    } catch (e) {
      setMsg({ text: String(e), tone: "error" });
    } finally {
      setBusy(false);
    }
  };
  return (
    <SettingsRow
      title={<span style={{ ...MONO, fontSize: 12.5, overflowWrap: "anywhere" }}>{name}</span>}
      badge={set ? <Badge tone="success">Set</Badge> : <Badge tone="warning">Not set</Badge>}
      helper="A value for the local program, kept in your system keychain. It isn't shown again."
      control={
        <>
          <Input
            type="password"
            autoComplete="off"
            spellCheck={false}
            aria-label={`Value for ${name}`}
            placeholder={set ? "Enter to replace" : "Value"}
            style={{ width: 170 }}
            value={value}
            disabled={busy}
            onChange={(e) => setValue(e.target.value)}
          />
          <Button
            variant="outline"
            size="sm"
            disabled={busy || !value}
            onClick={() => void store(value)}
          >
            Save
          </Button>
          {set && (
            <Button variant="ghost" size="sm" disabled={busy} onClick={() => void store("")}>
              Clear
            </Button>
          )}
        </>
      }
    >
      {msg && <Message msg={msg} />}
    </SettingsRow>
  );
}

/* ------------------------------------------------------------- one tool */

function ToolRow({
  connectorId,
  tool,
  value,
  disabled,
  onSet,
  onChange,
  onForgetDraft,
}: {
  connectorId: string;
  tool: ConnectorTool;
  value: { enabled: boolean; readOnly: boolean };
  disabled: boolean;
  onSet: (patch: Partial<{ enabled: boolean; readOnly: boolean }>) => void;
  onChange: (c: Connector) => void;
  onForgetDraft: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [reviewing, setReviewing] = useState(false);
  const [trying, setTrying] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<Msg>(null);
  const readOnlyNoteId = useId();

  const status = toolStatusLabel(tool);
  const kind = effectiveKind(tool, value.readOnly);
  // A tool that isn't usable as offered can be turned off, not on.
  const canTurnOn = tool.status === "ok";
  const long = tool.description.length > 160 || tool.description.split("\n").length > 2;

  const accept = async () => {
    if (!tool.pendingHash) return;
    setBusy(true);
    setMsg(null);
    try {
      const r = await api.acceptConnectorToolChange(connectorId, tool.name, tool.pendingHash);
      if (r.saved) {
        if (r.connector) onChange(r.connector);
        setReviewing(false);
        setMsg({ text: "Change accepted.", tone: "success" });
      } else {
        setMsg({ text: "Not saved.", tone: "muted" });
      }
    } catch (e) {
      setMsg({ text: String(e), tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  /** Close the review; if the tool is on, turn it off (that grants nothing,
   *  so no system dialog). */
  const keepOff = async () => {
    onForgetDraft();
    if (!tool.enabled) {
      setReviewing(false);
      return;
    }
    setBusy(true);
    setMsg(null);
    try {
      const r = await api.saveConnectorTools(connectorId, [
        { tool: tool.name, enabled: false, readOnly: tool.readOnly },
      ]);
      if (r.saved && r.connector) onChange(r.connector);
      setReviewing(false);
      setMsg(
        r.saved ? { text: "Turned off.", tone: "muted" } : { text: "Not saved.", tone: "muted" },
      );
    } catch (e) {
      setMsg({ text: String(e), tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <SettingsRow
      data-tool={tool.name}
      title={
        <span style={{ display: "inline-flex", flexWrap: "wrap", alignItems: "baseline", gap: 6 }}>
          <span style={{ overflowWrap: "anywhere" }}>{tool.title ?? tool.name}</span>
          {tool.title && (
            <span className="text-text-3" style={{ ...MONO, overflowWrap: "anywhere" }}>
              {tool.name}
            </span>
          )}
        </span>
      }
      badge={
        <>
          <Badge tone={kind === "read" ? "neutral" : "warning"}>{kindLabel(kind)}</Badge>
          {tool.destructive && <Badge tone="danger">Destructive</Badge>}
          <Badge tone={status.tone}>{status.label}</Badge>
        </>
      }
      control={
        <>
          <span
            title={tool.canMarkReadOnly ? undefined : "The server says this tool can change data."}
          >
            <Checkbox
              label="Read-only"
              checked={tool.canMarkReadOnly ? value.readOnly : false}
              disabled={disabled || busy || !tool.canMarkReadOnly}
              aria-describedby={tool.canMarkReadOnly ? undefined : readOnlyNoteId}
              onChange={(e) => onSet({ readOnly: e.target.checked })}
            />
          </span>
          <span className="text-text-2" style={{ fontSize: 12.5 }} aria-hidden="true">
            On
          </span>
          <Switch
            label={`On: ${tool.name}`}
            checked={value.enabled}
            disabled={disabled || busy || (!value.enabled && !canTurnOn)}
            onChange={(on) => onSet({ enabled: on })}
          />
        </>
      }
    >
      <div style={COLUMN}>
        {tool.description && (
          <div>
            <p
              className="text-text-2"
              style={{
                ...SMALL,
                fontSize: 12.5,
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
                style={{ ...SMALL, background: "none", border: 0, padding: 0 }}
                aria-expanded={expanded}
                onClick={() => setExpanded((x) => !x)}
              >
                {expanded ? "Show less" : "Show more"}
              </button>
            )}
          </div>
        )}
        {!tool.canMarkReadOnly && (
          <p id={readOnlyNoteId} className="text-text-3" style={SMALL}>
            The server says this tool can change data, so it can&apos;t be marked read-only.
          </p>
        )}
        {tool.status === "changed" && (
          <p className="text-warning-text" style={SMALL}>
            Its definition changed since you approved it. Ask can&apos;t use it until you review the
            change.
          </p>
        )}
        <div className={BUTTONS}>
          <span className="text-text-3" style={MONO}>
            pinned {tool.pinnedHash}
          </span>
          {tool.status === "changed" && (
            <Button
              variant="soft"
              size="xs"
              aria-expanded={reviewing}
              disabled={busy}
              onClick={() => setReviewing((r) => !r)}
            >
              {reviewing ? "Hide change" : "Review change"}
            </Button>
          )}
          {tool.enabled && tool.status === "ok" && (
            <Button
              variant="outline"
              size="xs"
              aria-expanded={trying}
              onClick={() => setTrying((t) => !t)}
            >
              {trying ? "Close tester" : "Try a tool"}
            </Button>
          )}
        </div>

        {reviewing && tool.status === "changed" && (
          <div style={COLUMN} aria-label={`Changes to ${tool.name}`} role="group">
            {tool.diff.length === 0 && (
              <p className="text-text-2" style={SMALL}>
                The server didn&apos;t say what changed.
              </p>
            )}
            {tool.diff.map((d, i) => (
              <div key={`${i}-${d.field}`} style={COLUMN}>
                <p className="text-text" style={{ ...SMALL, fontWeight: 600 }}>
                  {d.field}
                </p>
                <div className="grid gap-2 sm:grid-cols-2">
                  <div style={{ display: "flex", flexDirection: "column", gap: 4, minWidth: 0 }}>
                    <span className="text-text-3" style={SMALL}>
                      Before
                    </span>
                    <pre style={PRE} data-diff="before">
                      {d.before}
                    </pre>
                  </div>
                  <div style={{ display: "flex", flexDirection: "column", gap: 4, minWidth: 0 }}>
                    <span className="text-text-3" style={SMALL}>
                      After
                    </span>
                    <pre style={PRE} data-diff="after">
                      {d.after}
                    </pre>
                  </div>
                </div>
              </div>
            ))}
            <div className={BUTTONS}>
              <Button
                variant="primary"
                size="sm"
                disabled={busy || !tool.pendingHash}
                onClick={() => void accept()}
              >
                Accept change
              </Button>
              <Button variant="outline" size="sm" disabled={busy} onClick={() => void keepOff()}>
                Keep it off
              </Button>
              <span className="text-text-3" style={SMALL}>
                {SYSTEM_DIALOG_NOTE}
              </span>
            </div>
          </div>
        )}

        {trying && tool.enabled && tool.status === "ok" && (
          <TryTool connectorId={connectorId} tool={tool} />
        )}
        {msg && <Message msg={msg} />}
      </div>
    </SettingsRow>
  );
}

/* ------------------------------------------------------------- try a tool */

function TryTool({ connectorId, tool }: { connectorId: string; tool: ConnectorTool }) {
  const [text, setText] = useState(() => prettyArgs(argsTemplate(tool.inputSchema)));
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<ConnectorTestResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const parsed = parseArgs(text);
  const argsId = useId();

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
      style={{
        ...COLUMN,
        padding: 12,
        border: "1px solid var(--line-2)",
        borderRadius: "var(--radius-lg)",
        background: "var(--surface)",
      }}
    >
      <label htmlFor={argsId} className="fly-field-label">
        Arguments (JSON)
      </label>
      <textarea
        id={argsId}
        value={text}
        rows={Math.min(10, Math.max(3, text.split("\n").length))}
        spellCheck={false}
        disabled={running}
        aria-invalid={!parsed.ok || undefined}
        onChange={(e) => setText(e.target.value)}
        style={{ ...TEXTAREA, borderColor: parsed.ok ? "var(--line)" : "var(--error)" }}
      />
      {!parsed.ok && (
        <p className="text-error-text" style={SMALL}>
          {parsed.error}
        </p>
      )}
      <div className={BUTTONS}>
        <Button
          variant="primary"
          size="sm"
          disabled={running || !parsed.ok}
          onClick={() => void run()}
        >
          {running ? "Waiting…" : "Run"}
        </Button>
        {running && (
          <span role="status" className="text-text-2" style={SMALL}>
            Check the call in the card that opened, then choose Run or Don&apos;t run.
          </span>
        )}
      </div>
      {result && (
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <span
            className={result.ok ? "text-success-text" : "text-error-text"}
            style={{ ...SMALL, fontWeight: 600 }}
          >
            {/* not ok = declined, refused or a server error; the text says which */}
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
      {error && <Message msg={{ text: error, tone: "error" }} />}
    </div>
  );
}

/* ------------------------------------------------------------- add */

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", alignItems: "flex-start", gap: 6 }}>
      <span className="fly-field-label">{label}</span>
      {children}
    </div>
  );
}

function AddConnector({
  empty,
  notice,
  onOpen,
  onAdded,
}: {
  empty: boolean;
  notice: Msg;
  onOpen: () => void;
  onAdded: (c: Connector) => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [d, setD] = useState<AddDraft>(emptyAddDraft);
  const [errors, setErrors] = useState<AddErrors>({});
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<Msg>(null);

  const set = (patch: Partial<AddDraft>) => {
    setD((x) => ({ ...x, ...patch }));
    setErrors({});
    setMsg(null);
  };

  const close = () => {
    setOpen(false);
    setD(emptyAddDraft());
    setErrors({});
    setMsg(null);
  };

  const submit = async () => {
    const { config, errors: found } = validateAddDraft(d);
    setErrors(found);
    if (!config) return;
    setBusy(true);
    setMsg(null);
    try {
      const r = await api.addConnector(config);
      if (!r.saved || !r.connector) {
        // declined in the native dialog
        setMsg({ text: "Not saved.", tone: "muted" });
        return;
      }
      close();
      await onAdded(r.connector);
    } catch (e) {
      setMsg({ text: String(e), tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <SettingsGroup>
      <SettingsRow
        title="Add a connector"
        helper={
          empty
            ? "No connectors yet. Add a web address or a program on this computer that speaks MCP."
            : "A web address or a program on this computer that speaks MCP."
        }
        control={
          !open && (
            <Button
              variant="outline"
              size="sm"
              startIcon={<Plus size={13} strokeWidth={1.75} />}
              onClick={() => {
                onOpen();
                setOpen(true);
              }}
            >
              Add a connector
            </Button>
          )
        }
      >
        {(open || notice) && (
          <div style={{ ...COLUMN, gap: 12 }}>
            {notice && !open && <Message msg={notice} />}
            {open && (
              <form
                style={{ ...COLUMN, gap: 12 }}
                onSubmit={(e) => {
                  e.preventDefault();
                  void submit();
                }}
              >
                <Input
                  label="Name"
                  value={d.name}
                  placeholder="Acme CRM"
                  autoComplete="off"
                  error={errors.name}
                  onChange={(e) => set({ name: e.target.value })}
                />
                <Field label="Type">
                  <Segmented
                    label="Type"
                    value={d.kind}
                    onChange={(kind) => set({ kind })}
                    options={[
                      { value: "http", label: "Web address" },
                      { value: "stdio", label: "Local program" },
                    ]}
                  />
                </Field>
                {d.kind === "http" ? (
                  <>
                    <Input
                      label="Web address"
                      value={d.url}
                      placeholder="https://mcp.example.com/mcp"
                      spellCheck={false}
                      autoComplete="off"
                      style={MONO}
                      error={errors.url}
                      onChange={(e) => set({ url: e.target.value })}
                    />
                    <Field label="Sign-in">
                      <Segmented
                        label="Sign-in"
                        value={d.auth}
                        onChange={(auth) => set({ auth })}
                        options={[
                          { value: "none", label: "None" },
                          { value: "oauth", label: "OAuth" },
                        ]}
                      />
                    </Field>
                    {d.auth === "oauth" && (
                      <>
                        <Input
                          label="Scopes (optional)"
                          hint="Separate them with spaces or commas."
                          value={d.scopes}
                          spellCheck={false}
                          autoComplete="off"
                          style={MONO}
                          onChange={(e) => set({ scopes: e.target.value })}
                        />
                        <Input
                          label="Client ID (optional)"
                          hint="Only if the service gave you one."
                          value={d.clientId}
                          spellCheck={false}
                          autoComplete="off"
                          style={MONO}
                          onChange={(e) => set({ clientId: e.target.value })}
                        />
                        <Input
                          label="Client secret (optional)"
                          hint="Kept in your system keychain."
                          type="password"
                          value={d.clientSecret}
                          autoComplete="off"
                          style={MONO}
                          error={errors.clientSecret}
                          onChange={(e) => set({ clientSecret: e.target.value })}
                        />
                      </>
                    )}
                  </>
                ) : (
                  <>
                    <Input
                      label="Program"
                      value={d.command}
                      placeholder="docs-mcp"
                      spellCheck={false}
                      autoComplete="off"
                      style={MONO}
                      error={errors.command}
                      onChange={(e) => set({ command: e.target.value })}
                    />
                    <Input
                      label="Arguments (optional)"
                      hint='Separate them with spaces; put "double quotes" around one with spaces.'
                      value={d.args}
                      spellCheck={false}
                      autoComplete="off"
                      style={MONO}
                      error={errors.args}
                      onChange={(e) => set({ args: e.target.value })}
                    />
                    <Input
                      label="Environment variable names (optional)"
                      hint="Names only, like DOCS_TOKEN. You set their values after adding it."
                      value={d.envNames}
                      spellCheck={false}
                      autoComplete="off"
                      style={MONO}
                      error={errors.envNames}
                      onChange={(e) => set({ envNames: e.target.value })}
                    />
                  </>
                )}
                <div className={BUTTONS}>
                  <Button type="submit" variant="primary" size="sm" disabled={busy}>
                    {busy ? "Adding…" : "Add connector"}
                  </Button>
                  <Button type="button" variant="ghost" size="sm" disabled={busy} onClick={close}>
                    Cancel
                  </Button>
                  <span className="text-text-3" style={SMALL}>
                    {SYSTEM_DIALOG_NOTE}
                  </span>
                </div>
                <Message msg={msg} />
              </form>
            )}
          </div>
        )}
      </SettingsRow>
    </SettingsGroup>
  );
}

/* ------------------------------------------------------------- call log */

const TH: CSSProperties = {
  textAlign: "left",
  fontWeight: 600,
  padding: "6px 8px",
  whiteSpace: "nowrap",
  borderBottom: "1px solid var(--line)",
};
const TD: CSSProperties = {
  padding: "6px 8px",
  verticalAlign: "top",
  borderBottom: "1px solid var(--line-2)",
  // break-word (not anywhere) keeps columns from squeezing to one character
  overflowWrap: "break-word",
};

function CallLog() {
  const confirm = useConfirm();
  const [open, setOpen] = useState(false);
  const [log, setLog] = useState<ConnectorCallLogEntry[] | null>(null);
  // "5 min ago" is measured from when the log was read.
  const [readAt, setReadAt] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open) return undefined;
    let live = true;
    let unlisten: (() => void) | null = null;
    const load = () =>
      api
        .connectorCallLog()
        .then((l) => {
          if (live) {
            setLog(l);
            setReadAt(Date.now());
            setError(null);
          }
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
  }, [open]);

  const clear = async () => {
    const ok = await confirm({
      title: "Clear the call log?",
      message:
        "The record of every connector call goes. Connectors and their tools stay as they are.",
      confirmLabel: "Clear log",
      tone: "danger",
    });
    if (!ok) return;
    setBusy(true);
    setError(null);
    try {
      await api.clearConnectorCallLog();
      setLog([]);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const rows = log ? newestFirst(log) : [];

  return (
    <SettingsGroup>
      <SettingsRow
        title="Call log"
        helper="Every call Ask proposed, what you chose and how it went. It never keeps arguments or results."
        control={
          <Button
            variant="outline"
            size="sm"
            aria-expanded={open}
            onClick={() => setOpen((o) => !o)}
          >
            {open ? "Hide" : "Show"}
          </Button>
        }
      >
        {open && (
          <div style={COLUMN}>
            {error && <Message msg={{ text: error, tone: "error" }} />}
            {log === null && !error && (
              <p className="text-text-3" style={SMALL}>
                Loading…
              </p>
            )}
            {log !== null && rows.length === 0 && (
              <p className="text-text-3" style={SMALL}>
                No calls yet.
              </p>
            )}
            {rows.length > 0 && (
              <div style={{ overflowX: "auto" }}>
                <table
                  className="text-text-2"
                  style={{ width: "100%", borderCollapse: "collapse", fontSize: 12 }}
                >
                  <thead className="text-text-3">
                    <tr>
                      <th style={TH}>When</th>
                      <th style={TH}>Connector</th>
                      <th style={TH}>Tool</th>
                      <th style={TH}>Kind</th>
                      <th style={TH}>Outcome</th>
                      <th style={TH}>Took</th>
                      <th style={TH}>Result</th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((e, i) => (
                      <tr key={`${e.at}-${i}`} data-log-row>
                        <td
                          style={{ ...TD, whiteSpace: "nowrap" }}
                          title={new Date(e.at * 1000).toLocaleString()}
                        >
                          {relativeTime(e.at, readAt)}
                        </td>
                        <td style={TD}>{e.server}</td>
                        <td style={{ ...TD, ...MONO, whiteSpace: "nowrap" }}>
                          {e.tool}
                          {e.argsEdited && (
                            <>
                              {" "}
                              <Badge tone="primary" title="You edited the arguments">
                                edited
                              </Badge>
                            </>
                          )}
                        </td>
                        <td style={{ ...TD, whiteSpace: "nowrap" }}>{kindLabel(e.kind)}</td>
                        <td
                          style={{ ...TD, minWidth: 150 }}
                          className={e.outcome === "ran" ? "text-text" : undefined}
                        >
                          {outcomeLabel(e)}
                        </td>
                        <td style={{ ...TD, whiteSpace: "nowrap" }}>
                          {formatDuration(e.durationMs)}
                        </td>
                        <td style={{ ...TD, whiteSpace: "nowrap" }}>
                          {e.outcome === "ran" || e.resultChars > 0
                            ? formatChars(e.resultChars, e.truncated)
                            : "—"}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
            {log !== null && rows.length > 0 && (
              <div>
                <Button variant="outline" size="sm" disabled={busy} onClick={() => void clear()}>
                  Clear log
                </Button>
              </div>
            )}
          </div>
        )}
      </SettingsRow>
    </SettingsGroup>
  );
}
