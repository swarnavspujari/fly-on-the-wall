import { useEffect, useId, useState } from "react";
import type { ReactNode } from "react";
import { ArrowLeft, Plus, ShieldAlert, TriangleAlert, X } from "lucide-react";
import { api } from "../api";
import {
  CUSTOM_TRUST_NOTE,
  KEYCHAIN_NOTE,
  LOCAL_WARNING,
  MASK,
  UNLABELLED_CHIP,
  candidateSummary,
  customDraftFrom,
  directoryInput,
  emptyCustomDraft,
  emptyLocalDraft,
  filterDirectory,
  localDraftFrom,
  parseCommand,
  probeSummary,
  sameAddress,
  validateCustom,
  validateLocal,
  willRunPreview,
  type CustomDraft,
  type FormErrors,
  type LocalDraft,
  type SignInChoice,
} from "../connectorsModel";
import type {
  Connector,
  ConnectorClaudeImport,
  ConnectorDirectoryEntry,
  ConnectorProbe,
} from "../types";
import {
  COLUMN,
  Callout,
  ConnectorIcon,
  ErrorText,
  KVRows,
  MONO,
  PRE,
  SMALL,
  SecretField,
  SignInWaiting,
  newRowId,
  useSignIn,
} from "./connectorParts";
import { Badge, Button, Checkbox, Input, Modal, Segmented } from "./ui";

/* The ways to add a connector: Browse (the directory, with a preview),
   Add custom (a web address), Add local (a program on this computer) and
   Import from Claude Desktop. The same forms edit a connector. Every add
   or edit is approved by the backend in a native dialog; a decline leaves
   the form as it was, with nothing alarming. */

export interface AddCallbacks {
  onClose: () => void;
  /** Added or saved: open its detail page, with an error to show if the
   *  follow-up step (listing tools, sign-in) failed. */
  onDone: (c: Connector, notice: string | null) => void;
  /** The connector changed while the sheet is open (a sign-in finished). */
  onChanged: (c: Connector) => void;
}

const H3 = { margin: 0, fontSize: 13.5, fontWeight: 600 } as const;

/* ------------------------------------------------------------- browse */

/** Tools a probe found, grouped like the detail page. */
function ToolPreview({ probe }: { probe: ConnectorProbe }) {
  const read = probe.tools.filter((t) => t.kind === "read");
  const write = probe.tools.filter((t) => t.kind !== "read");
  if (probe.tools.length === 0) {
    return (
      <p className="text-text-2" style={SMALL}>
        {probeSummary(probe)}
      </p>
    );
  }
  const group = (title: string, tools: ConnectorProbe["tools"]) =>
    tools.length > 0 && (
      <div style={COLUMN}>
        <h4 style={H3}>
          {title} ({tools.length})
        </h4>
        <ul style={{ margin: 0, paddingLeft: 0, listStyle: "none", ...COLUMN }}>
          {tools.map((t) => (
            <li key={t.name} style={{ display: "flex", flexDirection: "column", gap: 2 }}>
              <span style={{ fontSize: 13, fontWeight: 600, overflowWrap: "anywhere" }}>
                {t.title ?? t.name}{" "}
                {t.title && (
                  <span className="text-text-3" style={{ ...MONO, fontWeight: 400 }}>
                    {t.name}
                  </span>
                )}
              </span>
              {!t.labelled && (
                <span>
                  <Badge tone="warning">{UNLABELLED_CHIP}</Badge>
                </span>
              )}
              {t.description && (
                <span className="text-text-2" style={{ ...SMALL, overflowWrap: "anywhere" }}>
                  {t.description}
                </span>
              )}
            </li>
          ))}
        </ul>
      </div>
    );
  return (
    <div style={{ ...COLUMN, gap: 12 }}>
      <p className="text-text-2" style={SMALL}>
        {probeSummary(probe)}
      </p>
      {group("Read-only tools", read)}
      {group("Tools that can change or delete", write)}
    </div>
  );
}

function Preview({
  entry,
  onBack,
  onDone,
  onChanged,
}: { entry: ConnectorDirectoryEntry; onBack: () => void } & Omit<AddCallbacks, "onClose">) {
  const [probe, setProbe] = useState<
    | { state: "idle" }
    | { state: "checking" }
    | { state: "done"; probe: ConnectorProbe }
    | { state: "error"; error: string }
  >({ state: entry.auth === "api_key" ? "idle" : "checking" });
  const [apiKey, setApiKey] = useState("");
  const [keyError, setKeyError] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [added, setAdded] = useState<Connector | null>(null);
  const signIn = useSignIn(onChanged);
  const host = (() => {
    try {
      return new URL(entry.url).host;
    } catch {
      return entry.url;
    }
  })();

  const check = async (key: string) => {
    setProbe({ state: "checking" });
    try {
      setProbe({ state: "done", probe: await api.probeConnector(directoryInput(entry, key)) });
    } catch (e) {
      setProbe({ state: "error", error: String(e) });
    }
  };
  useEffect(() => {
    if (entry.auth !== "api_key") void check("");
    // check once per entry
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [entry.id]);

  const connect = async () => {
    if (entry.auth === "api_key" && !apiKey.trim()) {
      setKeyError("Enter your key.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const r = await api.addConnector(directoryInput(entry, apiKey));
      if (!r.saved || !r.connector) return; // declined: nothing changes
      setAdded(r.connector);
      if (entry.auth === "oauth") {
        const s = await signIn.start(r.connector.id);
        if (!s.cancelled) onDone(s.result?.connector ?? r.connector, s.error);
        return;
      }
      onDone(r.connector, r.error);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div style={{ ...COLUMN, gap: 14 }}>
      <div>
        <Button
          variant="ghost"
          size="sm"
          startIcon={<ArrowLeft size={14} strokeWidth={1.75} />}
          disabled={busy}
          onClick={onBack}
        >
          All connectors
        </Button>
      </div>
      <div className="flex items-center gap-3">
        <ConnectorIcon name={entry.name} />
        <div style={{ minWidth: 0 }}>
          <h3 style={{ margin: 0, fontSize: 16, fontWeight: 700 }}>{entry.name}</h3>
          <p className="text-text-3" style={SMALL}>
            Made by {entry.vendor}
          </p>
        </div>
      </div>
      <p className="text-text" style={SMALL}>
        {entry.description}
      </p>
      <p className="text-text-2" style={SMALL}>
        Connects to <span style={MONO}>{host}</span>
      </p>
      {entry.auth === "oauth" && (
        <p className="text-text-2" style={SMALL}>
          Sign-in happens in your browser.
        </p>
      )}
      {entry.auth === "api_key" && (
        <div style={COLUMN}>
          <SecretField
            label={entry.keyLabel ?? "API key"}
            value={apiKey}
            stored={false}
            error={keyError}
            hint={KEYCHAIN_NOTE}
            onChange={(v) => {
              setApiKey(v);
              setKeyError(undefined);
            }}
          />
          <div>
            <Button
              variant="outline"
              size="xs"
              disabled={!apiKey.trim() || probe.state === "checking"}
              onClick={() => void check(apiKey)}
            >
              Show its tools
            </Button>
          </div>
        </div>
      )}
      <section aria-label="What it can do" style={COLUMN}>
        <h4 style={H3}>What it can do</h4>
        {probe.state === "idle" && (
          <p className="text-text-3" style={SMALL}>
            Enter your key to see its tools.
          </p>
        )}
        {probe.state === "checking" && (
          <p role="status" className="text-text-2" style={SMALL}>
            Checking…
          </p>
        )}
        {probe.state === "error" && <ErrorText error={probe.error} />}
        {probe.state === "done" && <ToolPreview probe={probe.probe} />}
        <p className="text-text-3" style={SMALL}>
          Every tool starts on Needs approval.
        </p>
      </section>
      {signIn.waiting && (
        <SignInWaiting
          onCancel={() => {
            signIn.cancel();
            if (added) onDone(added, null);
          }}
        />
      )}
      {error && <ErrorText error={error} />}
      <div className="flex justify-end gap-2">
        <Button
          variant="primary"
          size="sm"
          disabled={busy || signIn.waiting}
          onClick={() => void connect()}
        >
          {busy ? "Connecting…" : "Connect"}
        </Button>
      </div>
    </div>
  );
}

/** Browse connectors: search, cards with [+], and a preview before Connect. */
export function BrowseSheet({
  directory,
  connectors,
  onCustom,
  ...cb
}: {
  directory: ConnectorDirectoryEntry[];
  connectors: Connector[];
  /** "Add custom connector" from the no-results state. */
  onCustom: () => void;
} & AddCallbacks) {
  const [q, setQ] = useState("");
  const [picked, setPicked] = useState<ConnectorDirectoryEntry | null>(null);
  const results = filterDirectory(directory, q);
  const isAdded = (e: ConnectorDirectoryEntry) =>
    connectors.some((c) => c.transport.kind === "http" && sameAddress(c.transport.url, e.url));

  return (
    <Modal open onClose={cb.onClose} title="Browse connectors" width={640} closeOnOverlay={false}>
      {picked ? (
        <Preview
          entry={picked}
          onBack={() => setPicked(null)}
          onDone={cb.onDone}
          onChanged={cb.onChanged}
        />
      ) : (
        <div style={{ ...COLUMN, gap: 14 }}>
          <Input
            type="search"
            aria-label="Search connectors"
            placeholder="Search connectors"
            value={q}
            data-autofocus
            onChange={(e) => setQ(e.target.value)}
          />
          {results.length === 0 ? (
            <div style={COLUMN}>
              <p className="text-text-2" style={SMALL}>
                Nothing matches &lsquo;{q.trim()}&rsquo;.
              </p>
              <div>
                <Button variant="outline" size="sm" onClick={onCustom}>
                  Add custom connector
                </Button>
              </div>
            </div>
          ) : (
            <ul
              className="grid gap-2 sm:grid-cols-2"
              style={{ margin: 0, padding: 0, listStyle: "none" }}
            >
              {results.map((e) => (
                <li
                  key={e.id}
                  data-directory-id={e.id}
                  className="flex items-start gap-3 rounded-xl border border-line bg-surface p-3"
                >
                  <ConnectorIcon name={e.name} />
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <p style={{ margin: 0, fontSize: 13.5, fontWeight: 600 }}>{e.name}</p>
                    <p className="text-text-2" style={{ ...SMALL, fontSize: 12 }}>
                      {e.description}
                    </p>
                    <p className="text-text-3" style={{ ...SMALL, fontSize: 11.5 }}>
                      Made by {e.vendor}
                    </p>
                  </div>
                  {isAdded(e) ? (
                    <Badge tone="success">Added</Badge>
                  ) : (
                    <Button
                      variant="outline"
                      size="xs"
                      aria-label={`Add ${e.name}`}
                      onClick={() => setPicked(e)}
                    >
                      <Plus size={14} strokeWidth={2} aria-hidden="true" />
                    </Button>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </Modal>
  );
}

/* ------------------------------------------------------------- custom */

const SIGN_IN_OPTIONS: { value: Exclude<SignInChoice, "">; label: string }[] = [
  { value: "oauth", label: "Sign in with my browser (recommended)" },
  { value: "api_key", label: "API key or token" },
  { value: "none", label: "No sign-in" },
];

/** Add custom connector (a web address), or edit one. */
export function CustomSheet({ editing, ...cb }: { editing?: Connector | null } & AddCallbacks) {
  const [d, setD] = useState<CustomDraft>(() =>
    editing ? customDraftFrom(editing, newRowId) : emptyCustomDraft(),
  );
  const [errors, setErrors] = useState<FormErrors>({});
  const [advanced, setAdvanced] = useState(
    () => !!editing && (d.headers.length > 0 || !!d.clientId),
  );
  const [phase, setPhase] = useState<"form" | "checking" | "adding">("form");
  const [summary, setSummary] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [added, setAdded] = useState<Connector | null>(null);
  const signIn = useSignIn(cb.onChanged);
  const advId = useId();
  const stored = editing
    ? { apiKeySet: editing.secrets.apiKeySet, clientSecretSet: editing.secrets.clientSecretSet }
    : {};

  const set = (p: Partial<CustomDraft>) => {
    setD((x) => ({ ...x, ...p }));
    setErrors({});
    setError(null);
    setSummary(null);
  };

  const submit = async () => {
    const { input, errors: found } = validateCustom(d, stored);
    setErrors(found);
    if (!input) return;
    setError(null);
    if (editing) {
      setPhase("adding");
      try {
        const r = await api.updateConnector(editing.id, input);
        if (r.saved && r.connector) cb.onDone(r.connector, r.error);
      } catch (e) {
        setError(String(e));
      } finally {
        setPhase("form");
      }
      return;
    }
    setPhase("checking");
    setSummary(null);
    let needsSignIn = false;
    try {
      const probe = await api.probeConnector(input);
      setSummary(probeSummary(probe));
      needsSignIn = probe.needsSignIn;
    } catch (e) {
      setError(String(e));
      setPhase("form");
      return;
    }
    if (needsSignIn && d.signIn !== "oauth") {
      setError("This connector asks you to sign in. Choose “Sign in with my browser”.");
      setPhase("form");
      return;
    }
    setPhase("adding");
    try {
      const r = await api.addConnector(input);
      if (!r.saved || !r.connector) {
        setPhase("form"); // declined in the native dialog
        return;
      }
      setAdded(r.connector);
      if (d.signIn === "oauth") {
        const s = await signIn.start(r.connector.id);
        if (!s.cancelled) cb.onDone(s.result?.connector ?? r.connector, s.error);
        return;
      }
      cb.onDone(r.connector, r.error);
    } catch (e) {
      setError(String(e));
      setPhase("form");
    }
  };

  const busy = phase !== "form" || signIn.waiting;
  return (
    <Modal
      open
      onClose={cb.onClose}
      title={editing ? `Edit ${editing.name}` : "Add custom connector"}
      width={600}
      closeOnOverlay={false}
      footer={
        <>
          <Button variant="ghost" size="sm" onClick={cb.onClose}>
            Cancel
          </Button>
          <Button variant="primary" size="sm" disabled={busy} onClick={() => void submit()}>
            {phase === "checking"
              ? "Checking…"
              : phase === "adding"
                ? editing
                  ? "Saving…"
                  : "Adding…"
                : editing
                  ? "Save changes"
                  : "Continue"}
          </Button>
        </>
      }
    >
      <form
        style={{ ...COLUMN, gap: 14 }}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Input
          label="Name"
          value={d.name}
          placeholder="Team wiki"
          autoComplete="off"
          error={errors.name}
          onChange={(e) => set({ name: e.target.value })}
        />
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
        <fieldset style={{ border: 0, margin: 0, padding: 0, ...COLUMN }}>
          <legend className="fly-field-label" style={{ marginBottom: 6 }}>
            How does it sign in?
          </legend>
          {SIGN_IN_OPTIONS.map((o) => (
            <Checkbox
              key={o.value}
              type="radio"
              name="connector-sign-in"
              label={o.label}
              checked={d.signIn === o.value}
              onChange={() => set({ signIn: o.value })}
            />
          ))}
          {errors.signIn && (
            <p className="text-error-text" style={{ ...SMALL, fontSize: 12 }}>
              {errors.signIn}
            </p>
          )}
        </fieldset>
        {d.signIn === "api_key" && (
          <SecretField
            label="API key or token"
            value={d.apiKey}
            stored={!!stored.apiKeySet}
            error={errors.apiKey}
            hint={`${KEYCHAIN_NOTE}. It's sent as “Authorization: Bearer …”.`}
            onChange={(apiKey) => set({ apiKey })}
          />
        )}
        <div style={COLUMN}>
          <div>
            <button
              type="button"
              className="cursor-pointer font-semibold text-primary-text"
              style={{ ...SMALL, background: "none", border: 0, padding: 0 }}
              aria-expanded={advanced}
              aria-controls={advId}
              onClick={() => setAdvanced((a) => !a)}
            >
              {advanced ? "Hide advanced settings" : "Advanced"}
            </button>
          </div>
          {advanced && (
            <div id={advId} style={{ ...COLUMN, gap: 12 }}>
              <div style={COLUMN}>
                <span className="fly-field-label">Request headers</span>
                <KVRows
                  rows={d.headers}
                  errors={errors}
                  kindLabel="Header"
                  addLabel="Add header"
                  onChange={(headers) => set({ headers })}
                />
              </div>
              {d.signIn === "oauth" && (
                <>
                  <Input
                    label="OAuth client ID (optional)"
                    hint="Only if the service gave you one."
                    value={d.clientId}
                    spellCheck={false}
                    autoComplete="off"
                    style={MONO}
                    onChange={(e) => set({ clientId: e.target.value })}
                  />
                  <SecretField
                    label="OAuth client secret (optional)"
                    value={d.clientSecret}
                    stored={!!stored.clientSecretSet}
                    error={errors.clientSecret}
                    hint={KEYCHAIN_NOTE}
                    onChange={(clientSecret) => set({ clientSecret })}
                  />
                </>
              )}
            </div>
          )}
        </div>
        <Callout role="note" icon={<ShieldAlert size={15} strokeWidth={1.75} />}>
          {CUSTOM_TRUST_NOTE}
        </Callout>
        {phase === "checking" && (
          <p role="status" className="text-text-2" style={SMALL}>
            Checking…
          </p>
        )}
        {summary && (
          <p role="status" className="text-text" style={SMALL}>
            {summary}
          </p>
        )}
        {signIn.waiting && (
          <SignInWaiting
            onCancel={() => {
              signIn.cancel();
              if (added) cb.onDone(added, null);
            }}
          />
        )}
        {error && <ErrorText error={error} />}
      </form>
    </Modal>
  );
}

/* ------------------------------------------------------------- local */

const PROGRAM_OPTIONS: { value: Exclude<LocalDraft["program"], "">; label: string }[] = [
  { value: "npx", label: "npx (Node)" },
  { value: "uvx", label: "uvx (Python)" },
  { value: "file", label: "Choose a file…" },
];

/** Add local connector (a program on this computer), or edit one. */
export function LocalSheet({ editing, ...cb }: { editing?: Connector | null } & AddCallbacks) {
  const [d, setD] = useState<LocalDraft>(() =>
    editing ? localDraftFrom(editing, newRowId) : emptyLocalDraft(),
  );
  const [errors, setErrors] = useState<FormErrors>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pasteOpen, setPasteOpen] = useState(false);
  const [pasteText, setPasteText] = useState("");
  const [pasteError, setPasteError] = useState<string | null>(null);
  const pasteId = useId();
  const preview = willRunPreview(d);

  const set = (p: Partial<LocalDraft>) => {
    setD((x) => ({ ...x, ...p }));
    setErrors({});
    setError(null);
  };

  const chooseFile = async () => {
    try {
      const file = await api.pickConnectorProgram();
      if (file) set({ program: "file", file });
    } catch (e) {
      setError(String(e));
    }
  };

  const fillIn = () => {
    const parsed = parseCommand(pasteText, newRowId);
    if ("error" in parsed) {
      setPasteError(parsed.error);
      return;
    }
    setPasteError(null);
    set(parsed);
    setPasteOpen(false);
    setPasteText("");
  };

  const submit = async () => {
    const { input, errors: found } = validateLocal(d);
    setErrors(found);
    if (!input) return;
    setBusy(true);
    setError(null);
    try {
      const r = editing
        ? await api.updateConnector(editing.id, input)
        : await api.addConnector(input);
      if (r.saved && r.connector) cb.onDone(r.connector, r.error);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open
      onClose={cb.onClose}
      title={editing ? `Edit ${editing.name}` : "Add local connector"}
      width={640}
      closeOnOverlay={false}
      footer={
        <>
          <Button variant="ghost" size="sm" onClick={cb.onClose}>
            Cancel
          </Button>
          <Button variant="primary" size="sm" disabled={busy} onClick={() => void submit()}>
            {busy ? (editing ? "Saving…" : "Adding…") : editing ? "Save changes" : "Add connector"}
          </Button>
        </>
      }
    >
      <div style={{ ...COLUMN, gap: 14 }}>
        <Callout role="note" tone="warning" icon={<TriangleAlert size={15} strokeWidth={1.75} />}>
          {LOCAL_WARNING}
        </Callout>
        <Input
          label="Name"
          value={d.name}
          placeholder="Docs search"
          autoComplete="off"
          error={errors.name}
          onChange={(e) => set({ name: e.target.value })}
        />
        <div style={COLUMN}>
          <span className="fly-field-label">Program</span>
          <div>
            <Segmented<LocalDraft["program"]>
              label="Program"
              value={d.program}
              options={PROGRAM_OPTIONS}
              onChange={(v) => (v === "file" ? void chooseFile() : set({ program: v }))}
            />
          </div>
          {d.program === "file" && (
            <div className="flex flex-wrap items-center gap-2">
              <span style={{ ...MONO, overflowWrap: "anywhere" }}>{d.file}</span>
              <Button variant="outline" size="xs" onClick={() => void chooseFile()}>
                Choose another file…
              </Button>
            </div>
          )}
          {errors.program && (
            <p className="text-error-text" style={{ ...SMALL, fontSize: 12 }}>
              {errors.program}
            </p>
          )}
        </div>
        <div style={COLUMN}>
          <span className="fly-field-label">Arguments</span>
          {d.args.map((a, i) => (
            <div key={a.id} className="flex items-center gap-2">
              <Input
                aria-label={`Argument ${i + 1}`}
                value={a.value}
                spellCheck={false}
                autoComplete="off"
                style={{ ...MONO, width: 360 }}
                onChange={(e) =>
                  set({
                    args: d.args.map((x) => (x.id === a.id ? { ...x, value: e.target.value } : x)),
                  })
                }
              />
              <button
                type="button"
                className="fly-modal-x"
                aria-label={`Remove argument ${i + 1}`}
                onClick={() => set({ args: d.args.filter((x) => x.id !== a.id) })}
              >
                <X size={14} strokeWidth={2} aria-hidden="true" />
              </button>
            </div>
          ))}
          {errors.args && (
            <p className="text-error-text" style={{ ...SMALL, fontSize: 12 }}>
              {errors.args}
            </p>
          )}
          <div>
            <Button
              variant="ghost"
              size="sm"
              startIcon={<Plus size={13} strokeWidth={1.75} />}
              onClick={() => set({ args: [...d.args, { id: newRowId(), value: "" }] })}
            >
              Add argument
            </Button>
          </div>
        </div>
        <div style={COLUMN}>
          <span className="fly-field-label">Settings (environment)</span>
          <p className="text-text-3" style={{ ...SMALL, fontSize: 12 }}>
            Secret values go to your system keychain.
          </p>
          <KVRows
            rows={d.env}
            errors={errors}
            kindLabel="Setting"
            addLabel="Add setting"
            onChange={(env) => set({ env })}
          />
        </div>
        <div style={COLUMN}>
          <div>
            <button
              type="button"
              className="cursor-pointer font-semibold text-primary-text"
              style={{ ...SMALL, background: "none", border: 0, padding: 0 }}
              aria-expanded={pasteOpen}
              aria-controls={pasteId}
              onClick={() => setPasteOpen((o) => !o)}
            >
              Paste a command
            </button>
          </div>
          {pasteOpen && (
            <div id={pasteId} style={COLUMN}>
              <textarea
                aria-label="Command to paste"
                value={pasteText}
                rows={2}
                spellCheck={false}
                placeholder="npx -y @example/server"
                onChange={(e) => {
                  setPasteText(e.target.value);
                  setPasteError(null);
                }}
                style={{ ...PRE, width: "100%", resize: "vertical", outline: "none" }}
              />
              <p className="text-text-3" style={{ ...SMALL, fontSize: 12 }}>
                It&apos;s only read to fill in the rows above, never run.
              </p>
              {pasteError && (
                <p className="text-error-text" style={{ ...SMALL, fontSize: 12 }}>
                  {pasteError}
                </p>
              )}
              <div>
                <Button variant="outline" size="sm" onClick={fillIn}>
                  Fill in
                </Button>
              </div>
            </div>
          )}
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <span className="fly-field-label">Will run</span>
          <pre data-will-run style={PRE}>
            {preview.command}
          </pre>
          {preview.settings.length > 0 && (
            <p className="text-text-2" style={{ ...SMALL, ...MONO, overflowWrap: "anywhere" }}>
              With settings: {preview.settings.join(", ")}
            </p>
          )}
        </div>
        {error && <ErrorText error={error} />}
      </div>
    </Modal>
  );
}

/* ------------------------------------------------------------- import */

function CandidateDetails({ c }: { c: ConnectorClaudeImport["candidates"][number] }) {
  const secretNames = [...c.envNames, ...c.headerNames];
  return (
    <div style={{ ...COLUMN, gap: 4, paddingLeft: 28 }}>
      <p className="text-text-2" style={SMALL}>
        {c.kind === "web" ? "Address" : "Command"}:{" "}
        <span style={{ ...MONO, overflowWrap: "anywhere" }}>{candidateSummary(c)}</span>
      </p>
      {secretNames.length > 0 && (
        <p className="text-text-2" style={SMALL}>
          {c.kind === "web" ? "Headers" : "Settings"}:{" "}
          <span style={MONO}>{secretNames.map((n) => `${n}=${MASK}`).join(", ")}</span> (will move
          to your keychain)
        </p>
      )}
    </div>
  );
}

/** Import from Claude Desktop: pick, then review one at a time, each with
 *  its own native dialog. Claude Desktop's file is only read. */
export function ImportSheet({
  onClose,
  onDone,
}: {
  onClose: () => void;
  /** Finished (or closed after some were added): how many were added. */
  onDone: (added: number) => void;
}) {
  const [data, setData] = useState<ConnectorClaudeImport | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [checked, setChecked] = useState<ReadonlySet<string>>(() => new Set());
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const [step, setStep] = useState<number | null>(null);
  const [addedCount, setAddedCount] = useState(0);
  const [busy, setBusy] = useState(false);
  const [stepError, setStepError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api
      .claudeDesktopCandidates()
      .then((r) => {
        if (live) setData(r);
      })
      .catch((e) => {
        if (live) setLoadError(String(e));
      });
    return () => {
      live = false;
    };
  }, []);

  const candidates = data?.candidates ?? [];
  const selected = candidates.filter((c) => checked.has(c.key));
  const toggle = (set: ReadonlySet<string>, key: string) => {
    const next = new Set(set);
    if (!next.delete(key)) next.add(key);
    return next;
  };

  const next = (added: number) => {
    setStepError(null);
    if (step === null) return;
    if (step + 1 >= selected.length) onDone(added);
    else setStep(step + 1);
  };

  const importOne = async () => {
    if (step === null) return;
    setBusy(true);
    setStepError(null);
    try {
      const r = await api.importClaudeDesktopConnector(selected[step].key);
      const added = addedCount + (r.saved ? 1 : 0);
      setAddedCount(added);
      next(added);
    } catch (e) {
      setStepError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const close = () => (addedCount > 0 ? onDone(addedCount) : onClose());
  let body: ReactNode;
  let footer: ReactNode = (
    <Button variant="ghost" size="sm" onClick={close}>
      Close
    </Button>
  );
  if (loadError) body = <ErrorText error={loadError} />;
  else if (!data)
    body = (
      <p role="status" className="text-text-2" style={SMALL}>
        Looking for Claude Desktop…
      </p>
    );
  else if (!data.found)
    body = (
      <p className="text-text-2" style={SMALL}>
        No Claude Desktop setup found on this computer.
      </p>
    );
  else if (candidates.length === 0)
    body = (
      <p className="text-text-2" style={SMALL}>
        Claude Desktop has no connectors set up.
      </p>
    );
  else if (step !== null) {
    const c = selected[step];
    body = (
      <div style={{ ...COLUMN, gap: 12 }}>
        <p className="text-text-3" style={SMALL} aria-live="polite">
          {step + 1} of {selected.length}
        </p>
        <div className="flex items-center gap-3">
          <ConnectorIcon name={c.name} />
          <h3 style={{ margin: 0, fontSize: 15, fontWeight: 700, overflowWrap: "anywhere" }}>
            {c.name}
          </h3>
          <Badge>{c.kind === "web" ? "Custom" : "Local"}</Badge>
        </div>
        <CandidateDetails c={c} />
        <p className="text-text-3" style={SMALL}>
          Permissions aren&apos;t imported: every tool starts on Needs approval.
        </p>
        {stepError && <ErrorText error={stepError} />}
      </div>
    );
    footer = (
      <>
        <Button variant="ghost" size="sm" disabled={busy} onClick={() => next(addedCount)}>
          Skip
        </Button>
        <Button variant="primary" size="sm" disabled={busy} onClick={() => void importOne()}>
          {busy ? "Importing…" : "Import"}
        </Button>
      </>
    );
  } else {
    body = (
      <div style={{ ...COLUMN, gap: 12 }}>
        <p className="text-text" style={SMALL}>
          We found {candidates.length} connector{candidates.length === 1 ? "" : "s"} in Claude
          Desktop.
        </p>
        <ul style={{ margin: 0, padding: 0, listStyle: "none", ...COLUMN }}>
          {candidates.map((c) => (
            <li key={c.key} data-candidate={c.key} style={COLUMN}>
              <div className="flex flex-wrap items-center gap-2">
                <Checkbox
                  label={c.name}
                  checked={checked.has(c.key)}
                  onChange={() => setChecked((s) => toggle(s, c.key))}
                />
                <Badge>{c.kind === "web" ? "Custom" : "Local"}</Badge>
                <button
                  type="button"
                  className="cursor-pointer text-primary-text"
                  style={{ ...SMALL, background: "none", border: 0, padding: 0 }}
                  aria-expanded={expanded.has(c.key)}
                  aria-label={`Details for ${c.name}`}
                  onClick={() => setExpanded((s) => toggle(s, c.key))}
                >
                  {expanded.has(c.key) ? "Hide" : "Details"}
                </button>
              </div>
              {expanded.has(c.key) && <CandidateDetails c={c} />}
            </li>
          ))}
        </ul>
        <p className="text-text-3" style={SMALL}>
          Fly on the Wall only reads Claude Desktop&apos;s setup and never changes it. Permissions
          aren&apos;t imported: every tool starts on Needs approval.
        </p>
      </div>
    );
    footer = (
      <>
        <Button variant="ghost" size="sm" onClick={close}>
          Cancel
        </Button>
        <Button
          variant="primary"
          size="sm"
          disabled={selected.length === 0}
          onClick={() => setStep(0)}
        >
          Review {selected.length}
        </Button>
      </>
    );
  }

  return (
    <Modal
      open
      onClose={close}
      title="Import from Claude Desktop"
      width={600}
      closeOnOverlay={false}
      footer={footer}
    >
      {body}
    </Modal>
  );
}
