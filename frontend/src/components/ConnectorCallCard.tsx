import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent as ReactKeyboardEvent } from "react";
import { listen } from "@tauri-apps/api/event";
import { Trash2, TriangleAlert } from "lucide-react";
import { api, CONNECTORS_PROPOSAL_EVENT, CONNECTORS_SETTLED_EVENT } from "../api";
import {
  argsFromFields,
  decidedLabel,
  fieldValuesFrom,
  mergeProposals,
  parseArgs,
  prettyArgs,
  sameArgs,
  schemaFields,
  splitDetails,
} from "../connectorsModel";
import type { ConnectorCallProposal, ConnectorCardAction, ConnectorSettled } from "../types";
import { Button, Checkbox, Input, Select } from "./ui";

/* The cards for connector calls. Every call Ask proposes for a tool on
   Needs approval waits on one, with its exact details, until you decide.
   Everything in a proposal comes from the model or the server, so it is
   shown as plain text only. A call that can change data is confirmed once
   more by the backend in a native dialog after "Review and send…". */

/** How long the buttons that allow a call stay off after a card appears (or
 *  the visible card changes), so a double click on one card can't approve
 *  the next one. */
export const ARM_DELAY_MS = 700;
/** How long a decided card stays as its one-line summary. */
export const COLLAPSE_MS = 4000;

const SMALL: CSSProperties = { margin: 0, fontSize: 12.5, lineHeight: 1.5 };
const MONO: CSSProperties = { fontFamily: "var(--font-mono)", fontSize: 11.5 };
const LINK =
  "cursor-pointer text-primary-text underline disabled:cursor-not-allowed disabled:opacity-50";

/** Pending proposals: what's pending when it mounts, then every
 *  `connectors:proposal` / `connectors:settled`. With `turnId`, only that
 *  Ask turn's. `markSettled` drops one at once (after you decided it). */
export function useConnectorProposals(turnId?: string) {
  const [all, setAll] = useState<ConnectorCallProposal[]>([]);
  // Settled ids, so a pending list read before a settle can't bring a
  // decided card back.
  const settled = useRef(new Set<string>());

  const markSettled = useCallback((proposalId: string) => {
    settled.current.add(proposalId);
    setAll((cur) => cur.filter((p) => p.proposalId !== proposalId));
  }, []);

  useEffect(() => {
    let live = true;
    const unlisten: (() => void)[] = [];
    const follow = <T,>(event: string, handler: (payload: T) => void) =>
      listen<T>(event, (e) => {
        if (live) handler(e.payload);
      })
        .then((un) => {
          if (live) unlisten.push(un);
          else un();
        })
        .catch(() => {});
    // Listen first, then read what's pending, so nothing falls in between.
    void Promise.all([
      follow<ConnectorCallProposal>(CONNECTORS_PROPOSAL_EVENT, (p) =>
        setAll((cur) => mergeProposals(cur, [p], settled.current)),
      ),
      follow<ConnectorSettled>(CONNECTORS_SETTLED_EVENT, (s) => markSettled(s.proposalId)),
    ]).then(() =>
      api
        .pendingConnectorCalls()
        .then((list) => {
          if (live) setAll((cur) => mergeProposals(cur, list, settled.current));
        })
        .catch(() => {}),
    );
    return () => {
      live = false;
      for (const un of unlisten) un();
    };
  }, [markSettled]);

  const proposals = turnId === undefined ? all : all.filter((p) => p.turnId === turnId);
  return { proposals, markSettled };
}

/** False for `delay` ms after `key` first shows (and again whenever it
 *  changes). */
export function useArmed(key: string, delay = ARM_DELAY_MS): boolean {
  const [armedKey, setArmedKey] = useState<string | null>(null);
  useEffect(() => {
    const t = window.setTimeout(() => setArmedKey(key), delay);
    return () => window.clearTimeout(t);
  }, [key, delay]);
  return armedKey === key;
}

/** Someone is typing (an input, textarea or contenteditable has focus). */
function isTyping(el: Element | null): boolean {
  if (!el) return false;
  if (el instanceof HTMLTextAreaElement) return true;
  if (el instanceof HTMLInputElement)
    return !["checkbox", "radio", "button", "submit", "reset", "range", "color", "file"].includes(
      el.type,
    );
  return el instanceof HTMLElement && el.isContentEditable;
}

/** The card heading's words. */
export function cardHeading(p: ConnectorCallProposal): string {
  const verb =
    p.kind === "write" ? "Ask wants to change something in" : "Ask wants to look something up in";
  return `${verb} ${p.serverName} · ${p.toolTitle ?? p.tool}`;
}

/** One proposed call: who, what, where it goes, and its editable details. */
export function ConnectorCallCard({
  proposal,
  onDecided,
}: {
  proposal: ConnectorCallProposal;
  /** The call was decided (it leaves the stack as a summary line). */
  onDecided: (proposalId: string, action: ConnectorCardAction, atMs: number) => void;
}) {
  const write = proposal.kind === "write";
  const armed = useArmed(proposal.proposalId);
  const fields = useMemo(
    () => schemaFields(proposal.inputSchema, proposal.args),
    [proposal.inputSchema, proposal.args],
  );
  const [mode, setMode] = useState<"fields" | "code">(fields ? "fields" : "code");
  const [values, setValues] = useState(() =>
    fields ? fieldValuesFrom(fields, proposal.args) : new Map<string, string | boolean>(),
  );
  const [text, setText] = useState(() => prettyArgs(proposal.args));
  const [busy, setBusy] = useState<ConnectorCardAction | null>(null);
  const [error, setError] = useState<string | null>(null);

  const current =
    mode === "fields" && fields ? argsFromFields(fields, values, proposal.args) : parseArgs(text);

  const headingId = useId();
  const subId = useId();
  const codeId = useId();
  const cardRef = useRef<HTMLDivElement>(null);

  // Focus starts on Deny, unless you're typing somewhere (then the card is
  // only announced).
  useEffect(() => {
    if (!isTyping(document.activeElement))
      cardRef.current?.querySelector<HTMLButtonElement>("[data-deny]")?.focus();
  }, []);

  const decide = async (action: ConnectorCardAction) => {
    if (busy) return;
    if (action !== "deny" && (!armed || !current.ok)) return;
    setBusy(action);
    setError(null);
    const args =
      action === "deny" || !current.ok || sameArgs(current.value, proposal.args)
        ? null
        : current.value;
    try {
      const r = await api.decideConnectorCall(proposal.proposalId, action, args);
      // Deny is always final: nothing runs without an explicit yes.
      if (r.ok || action === "deny") {
        onDecided(proposal.proposalId, action, Date.now());
        return;
      }
      // error null: you said no in the native dialog; the card stays.
      setError(r.error);
    } catch (e) {
      if (action === "deny") {
        onDecided(proposal.proposalId, action, Date.now());
        return;
      }
      setError(String(e));
    }
    setBusy(null);
  };

  const toggleMode = () => {
    setError(null);
    if (mode === "fields") {
      setText(prettyArgs(current.ok ? current.value : proposal.args));
      setMode("code");
      return;
    }
    const parsed = parseArgs(text);
    const asFields = parsed.ok ? schemaFields(proposal.inputSchema, parsed.value) : null;
    if (!parsed.ok || !asFields) {
      setError("These details can't be shown as fields. Edit them as code.");
      return;
    }
    setValues(fieldValuesFrom(asFields, parsed.value));
    setMode("fields");
  };

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "Escape" || e.defaultPrevented || busy) return;
    e.preventDefault();
    e.stopPropagation();
    void decide("deny");
  };

  const setValue = (name: string, v: string | boolean) => setValues((m) => new Map(m).set(name, v));
  const locked = busy !== null;

  return (
    <div
      ref={cardRef}
      role="group"
      aria-labelledby={headingId}
      aria-describedby={subId}
      data-proposal-id={proposal.proposalId}
      data-kind={proposal.kind}
      onKeyDown={onKeyDown}
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 10,
        maxHeight: "calc(100vh - 140px)",
        overflowY: "auto",
        padding: "14px 16px",
        background: "var(--surface)",
        color: "var(--text)",
        border: "1px solid var(--line)",
        // amber edge for a call that can change data (plus the marker text)
        borderLeft: write ? "5px solid var(--warning)" : "1px solid var(--line)",
        borderRadius: "var(--radius-2xl)",
        boxShadow: "var(--shadow-lg)",
      }}
    >
      {write && (
        <p
          className="text-warning-text"
          style={{ ...SMALL, display: "flex", flexWrap: "wrap", alignItems: "center", gap: 6 }}
        >
          <TriangleAlert size={14} strokeWidth={1.9} aria-hidden="true" />
          <strong>Can change data</strong>
          {proposal.destructive && (
            <>
              <span aria-hidden="true">·</span>
              <Trash2 size={14} strokeWidth={1.9} aria-hidden="true" />
              <strong className="text-error-text">Destructive</strong>
            </>
          )}
        </p>
      )}
      <div style={{ display: "flex", flexDirection: "column", gap: 3 }}>
        <h2
          id={headingId}
          style={{
            margin: 0,
            fontSize: 14,
            fontWeight: 600,
            lineHeight: 1.4,
            overflowWrap: "anywhere",
          }}
        >
          {cardHeading(proposal)}
        </h2>
        {proposal.toolTitle && proposal.toolTitle !== proposal.tool && (
          <p className="text-text-3" style={{ ...SMALL, ...MONO, overflowWrap: "anywhere" }}>
            tool: {proposal.tool}
          </p>
        )}
        <p id={subId} className="text-text-2" style={{ ...SMALL, overflowWrap: "anywhere" }}>
          Sends to {proposal.destination}
        </p>
      </div>

      {mode === "fields" && fields ? (
        fields.length === 0 ? (
          <p className="text-text-3" style={SMALL}>
            No details to send.
          </p>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            {fields.map((f) =>
              f.kind === "boolean" ? (
                <Checkbox
                  key={f.name}
                  label={f.label}
                  description={f.description ?? undefined}
                  checked={values.get(f.name) === true}
                  disabled={locked}
                  onChange={(e) => setValue(f.name, e.target.checked)}
                />
              ) : f.options ? (
                <label key={f.name} style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <span className="fly-field-label">
                    {f.label}
                    {!f.required && " (optional)"}
                  </span>
                  <Select
                    aria-label={f.label}
                    value={String(values.get(f.name) ?? "")}
                    disabled={locked}
                    onChange={(e) => setValue(f.name, e.target.value)}
                  >
                    {!f.required && <option value="">—</option>}
                    {f.options.map((o) => (
                      <option key={o} value={o}>
                        {o}
                      </option>
                    ))}
                  </Select>
                </label>
              ) : (
                <Input
                  key={f.name}
                  label={`${f.label}${f.required ? "" : " (optional)"}`}
                  hint={f.description ?? undefined}
                  value={String(values.get(f.name) ?? "")}
                  inputMode={f.kind === "string" ? undefined : "decimal"}
                  spellCheck={false}
                  autoComplete="off"
                  disabled={locked}
                  onChange={(e) => setValue(f.name, e.target.value)}
                />
              ),
            )}
          </div>
        )
      ) : (
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <label htmlFor={codeId} className="fly-field-label">
            Details (JSON)
          </label>
          <textarea
            id={codeId}
            value={text}
            rows={Math.min(12, Math.max(3, text.split("\n").length))}
            spellCheck={false}
            disabled={locked}
            aria-invalid={!current.ok || undefined}
            onChange={(e) => {
              setText(e.target.value);
              setError(null);
            }}
            style={{
              ...MONO,
              width: "100%",
              lineHeight: 1.5,
              color: "var(--text)",
              background: "var(--surface-2)",
              border: `1px solid ${current.ok ? "var(--line)" : "var(--error)"}`,
              borderRadius: "var(--radius-md)",
              padding: "8px 10px",
              outline: "none",
              resize: "vertical",
              whiteSpace: "pre",
            }}
          />
        </div>
      )}
      {fields && (
        <div>
          <button
            type="button"
            className={LINK}
            style={{ ...SMALL, background: "none", border: 0, padding: 0 }}
            disabled={locked}
            onClick={toggleMode}
          >
            {mode === "fields" ? "Show as code" : "Show as fields"}
          </button>
        </div>
      )}
      {!current.ok && (
        <p className="text-error-text" style={SMALL}>
          {current.error}
        </p>
      )}
      {error && (
        <p role="alert" className="text-error-text" style={{ ...SMALL, overflowWrap: "anywhere" }}>
          {splitDetails(error).message}
        </p>
      )}

      <div style={{ display: "flex", justifyContent: "flex-end", gap: 8 }}>
        <Button
          data-deny
          variant="outline"
          size="sm"
          disabled={locked}
          onClick={() => void decide("deny")}
        >
          Deny
        </Button>
        <Button
          variant="primary"
          size="sm"
          disabled={locked || !armed || !current.ok}
          onClick={() => void decide("allow_once")}
        >
          {write
            ? busy === "allow_once"
              ? "Waiting…"
              : "Review and send…"
            : busy === "allow_once"
              ? "Allowing…"
              : "Allow once"}
        </Button>
      </div>
      {!write && (
        <div style={{ display: "flex", justifyContent: "flex-end" }}>
          <button
            type="button"
            className={LINK}
            style={{ ...SMALL, background: "none", border: 0, padding: 0 }}
            disabled={locked || !armed || !current.ok}
            onClick={() => void decide("always_allow")}
          >
            Always allow this tool…
          </button>
        </div>
      )}
    </div>
  );
}

interface Decided {
  id: string;
  title: string;
  label: string;
}

/** Every pending proposal (or one Ask turn's), bottom-right above the rest
 *  of the app. One card at a time, oldest first; a decided card stays as a
 *  one-line summary for a few seconds. Mount once at the top level. */
export function ConnectorCallStack({
  turnId,
  hideTurnIds,
}: {
  turnId?: string;
  /** Ask turns whose cards an open answer sheet shows in place
   *  (AskConnectors.tsx), so a call never has two cards. */
  hideTurnIds?: ReadonlySet<string>;
}) {
  const { proposals: pending, markSettled } = useConnectorProposals(turnId);
  const proposals = hideTurnIds?.size ? pending.filter((p) => !hideTurnIds.has(p.turnId)) : pending;
  const [decided, setDecided] = useState<Decided[]>([]);
  const [announcement, setAnnouncement] = useState("");
  const timers = useRef<number[]>([]);
  useEffect(() => {
    const list = timers.current;
    return () => list.forEach((t) => window.clearTimeout(t));
  }, []);

  const first = proposals[0] ?? null;
  const waiting = Math.max(0, proposals.length - 1);
  const firstId = first?.proposalId ?? null;
  const firstHeading = first ? cardHeading(first) : "";
  // Announce each new card politely; it doesn't take focus while you type.
  useEffect(() => {
    if (firstId) setAnnouncement(firstHeading);
  }, [firstId, firstHeading]);

  const onDecided = (id: string, action: ConnectorCardAction, atMs: number) => {
    const p = proposals.find((x) => x.proposalId === id);
    markSettled(id);
    const label = decidedLabel(action, atMs);
    setAnnouncement(label);
    setDecided((d) => [
      ...d,
      { id, title: p ? `${p.serverName} · ${p.toolTitle ?? p.tool}` : "", label },
    ]);
    timers.current.push(
      window.setTimeout(() => setDecided((d) => d.filter((x) => x.id !== id)), COLLAPSE_MS),
    );
  };

  return (
    <>
      <div role="status" aria-live="polite" className="sr-only">
        {announcement}
      </div>
      {(first || decided.length > 0) && (
        <div
          role="region"
          aria-label="Connector calls"
          className="print:hidden"
          style={{
            position: "fixed",
            right: 16,
            bottom: 48,
            // above Settings (55) and notices (50), below in-app dialogs (60)
            zIndex: 58,
            width: 400,
            maxWidth: "calc(100vw - 32px)",
            display: "flex",
            flexDirection: "column",
            gap: 8,
          }}
        >
          {decided.map((d) => (
            <div
              key={d.id}
              data-decided={d.id}
              className="text-text-2"
              style={{
                ...SMALL,
                padding: "8px 12px",
                borderRadius: "var(--radius-xl)",
                background: "var(--surface)",
                border: "1px solid var(--line)",
                boxShadow: "var(--shadow-sm)",
              }}
            >
              <span className="text-text" style={{ fontWeight: 600 }}>
                {d.label}
              </span>
              {d.title && <span style={{ overflowWrap: "anywhere" }}> — {d.title}</span>}
            </div>
          ))}
          {first && (
            <ConnectorCallCard key={first.proposalId} proposal={first} onDecided={onDecided} />
          )}
          {waiting > 0 && (
            <p
              className="text-text-2"
              style={{
                ...SMALL,
                alignSelf: "flex-end",
                padding: "4px 10px",
                borderRadius: "var(--radius-pill)",
                background: "var(--surface)",
                border: "1px solid var(--line)",
              }}
            >
              {waiting} more call{waiting === 1 ? "" : "s"} waiting
            </p>
          )}
        </div>
      )}
    </>
  );
}
