import { createContext, useCallback, useContext, useEffect, useId, useRef, useState } from "react";
import type { MouseEvent, ReactNode } from "react";
import { Button, EscapeLayerScope, Modal } from "./ui";

/* In-app confirm dialog. Never use window.confirm / window.alert: in the app
   tauri-plugin-dialog replaces them with async versions that return a
   Promise (always truthy, so a sync guard goes ahead without asking), and
   the IPC behind them isn't granted, so no dialog ever shows. The lint
   config bans both; ask with `await confirm({...})` from useConfirm(). */

export interface ConfirmOptions {
  /** The question, in sentence case ("Delete folder “Q3”?"). */
  title: string;
  /** What happens either way. In a string, a blank line starts a new paragraph. */
  message?: ReactNode;
  /** The yes button says what it does ("Delete folder"), never "OK". */
  confirmLabel: string;
  cancelLabel?: string;
  /** "danger" for something you can't take back: the yes button turns red
   *  and moves to the far side, away from where the safe choice sits. */
  tone?: "default" | "danger";
}

/** Asks and resolves true only on an explicit yes. Esc, the backdrop, the
 *  close button and Cancel all resolve false. */
export type ConfirmFn = (options: ConfirmOptions) => Promise<boolean>;

const ConfirmContext = createContext<ConfirmFn | null>(null);

/** The ask function from the nearest <ConfirmProvider>. */
export function useConfirm(): ConfirmFn {
  const confirm = useContext(ConfirmContext);
  if (!confirm) throw new Error("useConfirm needs a <ConfirmProvider> above it");
  return confirm;
}

interface Pending {
  id: number;
  options: ConfirmOptions;
  resolve: (ok: boolean) => void;
  answer: Promise<boolean>;
  /** Same text = same question (null when the message isn't plain text). */
  signature: string | null;
}

function signatureOf(o: ConfirmOptions): string | null {
  if (o.message !== undefined && typeof o.message !== "string") return null;
  return JSON.stringify([
    o.title,
    o.message ?? "",
    o.confirmLabel,
    o.cancelLabel ?? "",
    o.tone ?? "",
  ]);
}

/* A click on the yes button this soon after the dialog opened is the tail of
   the click that opened it (or of a double click), not an answer. */
export const CONFIRM_CLICK_GUARD_MS = 400;

/* Above every other Escape layer (Settings, sheets, modals inside them), so
   Esc answers the question first wherever it was asked from. */
const CONFIRM_LAYER_DEPTH = 1000;

/** Renders one confirm dialog at a time for everything inside it; a second
 *  question waits until the first is answered. Mount once, near the root. */
export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [queue, setQueue] = useState<Pending[]>([]);
  const seq = useRef(0);
  // Every unanswered question, so unmounting answers them all with "no"
  // instead of leaving a caller waiting forever.
  const pending = useRef(new Set<Pending>());

  useEffect(() => {
    const open = pending.current;
    return () => {
      for (const p of open) p.resolve(false);
      open.clear();
    };
  }, []);

  const confirm = useCallback<ConfirmFn>((options) => {
    // The same question already waiting (a double click on Delete): one
    // dialog, one answer for both callers.
    const signature = signatureOf(options);
    if (signature !== null) {
      for (const p of pending.current) if (p.signature === signature) return p.answer;
    }
    let resolve!: (ok: boolean) => void;
    const answer = new Promise<boolean>((r) => {
      resolve = r;
    });
    const item: Pending = { id: ++seq.current, options, resolve, answer, signature };
    pending.current.add(item);
    setQueue((q) => [...q, item]);
    return answer;
  }, []);

  const current = queue[0];
  const answer = (item: Pending, ok: boolean) => {
    if (!pending.current.delete(item)) return; // already answered
    item.resolve(ok);
    setQueue((q) => q.filter((p) => p !== item));
  };

  return (
    <ConfirmContext.Provider value={confirm}>
      {children}
      {current && (
        <EscapeLayerScope depth={CONFIRM_LAYER_DEPTH}>
          <ConfirmDialog
            key={current.id}
            options={current.options}
            onAnswer={(ok) => answer(current, ok)}
          />
        </EscapeLayerScope>
      )}
    </ConfirmContext.Provider>
  );
}

function ConfirmDialog({
  options,
  onAnswer,
}: {
  options: ConfirmOptions;
  onAnswer: (ok: boolean) => void;
}) {
  const { title, message, confirmLabel, cancelLabel = "Cancel", tone = "default" } = options;
  const danger = tone === "danger";
  const messageId = useId();
  const cancel = () => onAnswer(false);
  const openedAt = useRef(0);
  useEffect(() => {
    openedAt.current = performance.now();
  }, []);
  // A mouse click (detail ≥ 1) right after opening, or the second click of a
  // double click, isn't a yes. Keyboard activation (detail 0) always counts:
  // focus starts on Cancel, so reaching yes already took a deliberate Tab.
  const yes = (e: MouseEvent<HTMLButtonElement>) => {
    const tooSoon = performance.now() - openedAt.current < CONFIRM_CLICK_GUARD_MS;
    if (e.detail > 1 || (e.detail >= 1 && tooSoon)) return;
    onAnswer(true);
  };
  const body =
    typeof message === "string"
      ? message
          .split(/\n\s*\n/)
          .map((p) => p.trim())
          .filter(Boolean)
          .map((p, i) => (
            <p key={i} style={{ margin: i === 0 ? 0 : "10px 0 0" }}>
              {p}
            </p>
          ))
      : message;

  // Cancel always starts with focus, so Enter or Space never says yes by accident.
  const cancelButton = (
    <Button variant={danger ? "outline" : "ghost"} size="sm" data-autofocus onClick={cancel}>
      {cancelLabel}
    </Button>
  );
  const confirmButton = danger ? (
    <Button
      variant="outline"
      size="sm"
      // Same look as Settings' "Delete all…": red text on an outline.
      style={{ color: "var(--error-text)", borderColor: "var(--error)" }}
      className="mr-auto"
      onClick={yes}
    >
      {confirmLabel}
    </Button>
  ) : (
    <Button variant="primary" size="sm" onClick={yes}>
      {confirmLabel}
    </Button>
  );

  return (
    <Modal
      open
      onClose={cancel}
      title={title}
      width={460}
      role="alertdialog"
      aria-describedby={message ? messageId : undefined}
      data-testid="confirm-dialog"
      data-tone={tone}
      footer={
        danger ? (
          <>
            {confirmButton}
            {cancelButton}
          </>
        ) : (
          <>
            {cancelButton}
            {confirmButton}
          </>
        )
      }
    >
      {message ? (
        <div
          id={messageId}
          className="text-text-2"
          style={{ fontSize: 13.5, lineHeight: 1.55, whiteSpace: "pre-line" }}
        >
          {body}
        </div>
      ) : null}
    </Modal>
  );
}
