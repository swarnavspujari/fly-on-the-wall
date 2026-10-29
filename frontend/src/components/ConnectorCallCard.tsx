import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { listen } from "@tauri-apps/api/event";
import { TriangleAlert } from "lucide-react";
import { api, CONNECTORS_PROPOSAL_EVENT, CONNECTORS_SETTLED_EVENT } from "../api";
import { kindLabel, mergeProposals, parseArgs, prettyArgs, sameArgs } from "../connectorsModel";
import type { ConnectorCallProposal, ConnectorSettled } from "../types";
import { Badge, Button, EscapeLayerScope, useDialogFocus, useEscapeLayer } from "./ui";

/* The confirm card for connector calls. Every call Ask (or Settings' "Try a
   tool") proposes waits here, with its exact arguments, until you choose
   "Run" or "Don't run". Everything in a proposal comes from the model or the
   server, so it is shown as plain text only. A write call is confirmed once
   more by the backend in a native dialog after "Run". */

/** Above Settings (depth 1) and sheets, below the in-app confirm (1000), so
 *  Esc answers the card before it closes whatever is under it. */
const CARD_LAYER_DEPTH = 500;

const SMALL: CSSProperties = { margin: 0, fontSize: 12, lineHeight: 1.5 };
const MONO: CSSProperties = { fontFamily: "var(--font-mono)", fontSize: 11.5 };

/** Pending proposals: what's pending when it mounts, then every
 *  `connectors:proposal` / `connectors:settled`. With `turnId`, only that
 *  Ask turn's. `markSettled` drops one at once (after you decided it). */
export function useConnectorProposals(turnId?: string) {
  const [all, setAll] = useState<ConnectorCallProposal[]>([]);
  // Settled ids, so a pending list that was read before a settle can't bring
  // a decided card back.
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

/** One proposed call: who, what, where it goes, and the editable arguments. */
export function ConnectorCallCard({
  proposal,
  onSettled,
}: {
  proposal: ConnectorCallProposal;
  /** Called once the call is decided (it leaves the stack). */
  onSettled: (proposalId: string) => void;
}) {
  const original = useMemo(() => prettyArgs(proposal.args), [proposal.args]);
  const [text, setText] = useState(original);
  const [busy, setBusy] = useState<"run" | "skip" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const parsed = parseArgs(text);
  const edited = text !== original;

  const headingId = useId();
  const subId = useId();
  const argsId = useId();
  const parseErrorId = useId();
  const ref = useRef<HTMLDivElement>(null);
  // Focus starts on "Don't run"; it goes back where it was only if you were
  // still in the card (you may have moved on to something else).
  useDialogFocus(ref, true, { restoreFocus: "inside" });

  const decide = async (run: boolean) => {
    if (busy) return;
    if (run && !parsed.ok) return;
    setBusy(run ? "run" : "skip");
    setError(null);
    // null = run it exactly as proposed (the log records whether you edited)
    const args = run && parsed.ok && !sameArgs(parsed.value, proposal.args) ? parsed.value : null;
    try {
      const r = await api.decideConnectorCall(proposal.proposalId, run, args);
      // Not running is always final: nothing runs without an explicit Run.
      if (r.ok || !run) {
        onSettled(proposal.proposalId);
        return;
      }
      setError(r.error ?? "The call wasn't accepted. Check the arguments.");
    } catch (e) {
      if (!run) {
        onSettled(proposal.proposalId);
        return;
      }
      setError(String(e));
    }
    setBusy(null);
  };

  const depth = useEscapeLayer(() => void decide(false), busy === null);

  const title = proposal.toolTitle ?? proposal.tool;
  const write = proposal.kind === "write";
  const rows = Math.min(12, Math.max(3, text.split("\n").length));

  return (
    <EscapeLayerScope depth={depth}>
      <div
        ref={ref}
        role="alertdialog"
        aria-labelledby={headingId}
        aria-describedby={subId}
        data-proposal-id={proposal.proposalId}
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 10,
          maxHeight: "calc(100vh - 120px)",
          overflowY: "auto",
          padding: "14px 16px",
          background: "var(--surface)",
          color: "var(--text)",
          border: `1px solid ${proposal.destructive ? "var(--error)" : "var(--line)"}`,
          borderRadius: "var(--radius-2xl)",
          boxShadow: "var(--shadow-lg)",
        }}
      >
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
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
            {proposal.serverName} wants to run {title}
          </h2>
          {proposal.toolTitle && proposal.toolTitle !== proposal.tool && (
            <p className="text-text-3" style={{ ...SMALL, ...MONO, overflowWrap: "anywhere" }}>
              tool: {proposal.tool}
            </p>
          )}
          <p id={subId} className="text-text-2" style={{ ...SMALL, overflowWrap: "anywhere" }}>
            Sends to {proposal.destination}
          </p>
          <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginTop: 2 }}>
            <Badge tone={write ? "warning" : "neutral"}>{kindLabel(proposal.kind)}</Badge>
          </div>
        </div>

        {proposal.destructive && (
          <div
            role="note"
            className="text-error-text"
            style={{
              display: "flex",
              alignItems: "center",
              gap: 8,
              padding: "8px 10px",
              borderRadius: "var(--radius-md)",
              background: "var(--error-soft)",
              fontSize: 12.5,
              lineHeight: 1.45,
            }}
          >
            <TriangleAlert size={15} strokeWidth={1.75} aria-hidden="true" />
            <Badge tone="danger">Destructive</Badge>
            <span>It can delete or overwrite data.</span>
          </div>
        )}

        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
            <label htmlFor={argsId} className="fly-field-label">
              Arguments
            </label>
            {edited && (
              <Button
                variant="ghost"
                size="xs"
                disabled={busy !== null}
                onClick={() => {
                  setText(original);
                  setError(null);
                }}
              >
                Undo edits
              </Button>
            )}
          </div>
          <textarea
            id={argsId}
            value={text}
            rows={rows}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            disabled={busy !== null}
            aria-invalid={!parsed.ok || undefined}
            aria-describedby={!parsed.ok ? parseErrorId : undefined}
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
              border: `1px solid ${parsed.ok ? "var(--line)" : "var(--error)"}`,
              borderRadius: "var(--radius-md)",
              padding: "8px 10px",
              outline: "none",
              resize: "vertical",
              whiteSpace: "pre",
            }}
          />
          {!parsed.ok && (
            <p id={parseErrorId} className="text-error-text" style={SMALL}>
              {parsed.error}
            </p>
          )}
          {error && (
            <p
              role="alert"
              className="text-error-text"
              style={{ ...SMALL, overflowWrap: "anywhere" }}
            >
              {error}
            </p>
          )}
        </div>

        {write && (
          <p className="text-text-2" style={SMALL}>
            You&apos;ll confirm once more in a system dialog.
          </p>
        )}

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8 }}>
          <Button
            variant="outline"
            size="sm"
            data-autofocus
            disabled={busy !== null}
            onClick={() => void decide(false)}
          >
            Don&apos;t run
          </Button>
          <Button
            variant="primary"
            size="sm"
            disabled={busy !== null || !parsed.ok}
            onClick={() => void decide(true)}
          >
            {busy === "run" ? "Running…" : "Run"}
          </Button>
        </div>
      </div>
    </EscapeLayerScope>
  );
}

/** Every pending proposal (or one Ask turn's), bottom-right above the rest
 *  of the app. One card at a time, oldest first, so a new proposal never
 *  takes focus from the one you're deciding. Mount once at the top level. */
export function ConnectorCallStack({ turnId }: { turnId?: string }) {
  const { proposals, markSettled } = useConnectorProposals(turnId);
  if (proposals.length === 0) return null;
  const [first] = proposals;
  const waiting = proposals.length - 1;
  return (
    <EscapeLayerScope depth={CARD_LAYER_DEPTH}>
      <div
        role="region"
        aria-label="Connector calls waiting for you"
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
        <ConnectorCallCard key={first.proposalId} proposal={first} onSettled={markSettled} />
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
    </EscapeLayerScope>
  );
}
