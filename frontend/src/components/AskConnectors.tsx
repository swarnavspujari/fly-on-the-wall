import { useEffect, useState, useSyncExternalStore } from "react";
import { decidedLabel } from "../connectorsModel";
import type { ConnectorCardAction } from "../types";
import { ConnectorCallCard, cardHeading, useConnectorProposals } from "./ConnectorCallCard";

/* Connector calls Ask proposes while it answers, shown in the turn they
   belong to instead of the app's corner stack (connectors beta, #113). The
   cards and every rule about them are ConnectorCallCard's; this only places
   them. While a turn's cards show here, the corner stack skips that turn, so
   a call never has two cards; close the sheet and they're back in the
   corner. */

const inline = new Map<string, number>();
let snapshot: ReadonlySet<string> = new Set();
const listeners = new Set<() => void>();
const changed = () => {
  snapshot = new Set(inline.keys());
  listeners.forEach((l) => l());
};

/** Show a turn's cards here (until the returned function runs). */
function showInline(turnId: string): () => void {
  inline.set(turnId, (inline.get(turnId) ?? 0) + 1);
  changed();
  return () => {
    const n = (inline.get(turnId) ?? 1) - 1;
    if (n > 0) inline.set(turnId, n);
    else inline.delete(turnId);
    changed();
  };
}

/** Ask turns whose connector cards are on screen in the answer sheet: the
 *  corner stack leaves those to it. */
export function useInlineAskTurns(): ReadonlySet<string> {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => snapshot,
  );
}

/** One Ask turn's connector calls: the oldest pending card, how many more
 *  wait, and what you decided. */
export function AskConnectorCards({ turnId }: { turnId: string }) {
  useEffect(() => showInline(turnId), [turnId]);
  const { proposals, markSettled } = useConnectorProposals(turnId);
  const [decided, setDecided] = useState<{ id: string; title: string; label: string }[]>([]);
  const [announcement, setAnnouncement] = useState("");
  const first = proposals[0] ?? null;
  const waiting = Math.max(0, proposals.length - 1);
  const firstId = first?.proposalId ?? null;
  const firstHeading = first ? cardHeading(first) : "";
  // Announce each new card politely; it never takes focus while you type.
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
  };

  if (!first && decided.length === 0) {
    return (
      <span className="sr-only" role="status" aria-live="polite">
        {announcement}
      </span>
    );
  }
  return (
    <div
      role="region"
      aria-label="Connector calls for this question"
      className="flex flex-col gap-2"
      data-testid="ask-connector-cards"
    >
      <span className="sr-only" role="status" aria-live="polite">
        {announcement}
      </span>
      {decided.map((d) => (
        <p key={d.id} className="m-0 px-1 text-[12.5px]" style={{ color: "var(--text-2)" }}>
          <span className="font-semibold text-text">{d.label}</span>
          {d.title && <span style={{ overflowWrap: "anywhere" }}> — {d.title}</span>}
        </p>
      ))}
      {first && <ConnectorCallCard key={first.proposalId} proposal={first} onDecided={onDecided} />}
      {waiting > 0 && (
        <p className="m-0 px-1 text-[12.5px]" style={{ color: "var(--text-2)" }}>
          {waiting} more call{waiting === 1 ? "" : "s"} waiting
        </p>
      )}
    </div>
  );
}
