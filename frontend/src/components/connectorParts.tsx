import { useEffect, useId, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";
import { ChevronDown, Lock, Plus, X } from "lucide-react";
import { api } from "../api";
import { SAVED_IN_KEYCHAIN, splitDetails, type FormErrors, type KVRow } from "../connectorsModel";
import type { Connector, ConnectorSaveResult } from "../types";
import { Avatar, Button, Input, Switch, useEscapeLayer } from "./ui";

/* Small pieces shared by Settings › Connectors (list, detail, add forms).
   Server text passed in here is shown as plain text only. */

export const SMALL: CSSProperties = { margin: 0, fontSize: 12.5, lineHeight: 1.5 };
export const MONO: CSSProperties = { fontFamily: "var(--font-mono)", fontSize: 11.5 };
export const PRE: CSSProperties = {
  ...MONO,
  margin: 0,
  padding: "8px 10px",
  lineHeight: 1.5,
  whiteSpace: "pre-wrap",
  overflowWrap: "anywhere",
  maxHeight: 240,
  overflow: "auto",
  color: "var(--text)",
  background: "var(--surface-2)",
  border: "1px solid var(--line-2)",
  borderRadius: "var(--radius-md)",
};
export const COLUMN: CSSProperties = { display: "flex", flexDirection: "column", gap: 8 };

let rowSeq = 0;
/** Ids for form rows (arguments, headers, settings). */
export const newRowId = () => `row-${++rowSeq}`;

/** A connector's icon: its initials on a tile. */
export function ConnectorIcon({ name, size = "md" }: { name: string; size?: "sm" | "md" }) {
  return <Avatar name={name} shape="square" size={size} colorKey={name} aria-hidden="true" />;
}

/** An error string; text after "\n---\n" (program output) goes under
 *  [Show details] in a <pre>. */
export function ErrorText({ error }: { error: unknown }) {
  const { message, details } = splitDetails(error);
  const [open, setOpen] = useState(false);
  const id = useId();
  return (
    <div role="alert" style={{ display: "flex", flexDirection: "column", gap: 6 }}>
      <p className="text-error-text" style={{ ...SMALL, overflowWrap: "anywhere" }}>
        {message}
        {details !== null && (
          <>
            {" "}
            <button
              type="button"
              className="cursor-pointer font-semibold text-primary-text underline"
              style={{ background: "none", border: 0, padding: 0, font: "inherit" }}
              aria-expanded={open}
              aria-controls={id}
              onClick={() => setOpen((o) => !o)}
            >
              {open ? "Hide details" : "Show details"}
            </button>
          </>
        )}
      </p>
      {details !== null && open && (
        <pre id={id} style={PRE}>
          {details}
        </pre>
      )}
    </div>
  );
}

export interface MenuItem {
  label: string;
  onSelect: () => void;
  disabled?: boolean;
  tone?: "danger";
  /** For a choice menu (menuitemradio). */
  checked?: boolean;
}

/** A button that opens a menu (role="menu"). Arrow keys move, Esc closes
 *  and returns focus to the button. */
export function MenuButton({
  label,
  ariaLabel,
  items,
  disabled = false,
  icon = true,
  className = "",
}: {
  label: ReactNode;
  ariaLabel?: string;
  items: MenuItem[];
  disabled?: boolean;
  icon?: boolean;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const menuId = useId();
  const choice = items.some((i) => i.checked !== undefined);

  const close = (refocus = true) => {
    setOpen(false);
    if (refocus) buttonRef.current?.focus();
  };
  useEscapeLayer(() => close(), open);

  useEffect(() => {
    if (!open) return undefined;
    const first =
      menuRef.current?.querySelector<HTMLButtonElement>(
        '[role^="menuitem"][aria-checked="true"]',
      ) ?? menuRef.current?.querySelector<HTMLButtonElement>('[role^="menuitem"]:not([disabled])');
    first?.focus();
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (!menuRef.current?.contains(t) && !buttonRef.current?.contains(t)) setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  const onMenuKey = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const all = [
      ...(menuRef.current?.querySelectorAll<HTMLButtonElement>(
        '[role^="menuitem"]:not([disabled])',
      ) ?? []),
    ];
    const at = all.indexOf(document.activeElement as HTMLButtonElement);
    let next = -1;
    if (e.key === "ArrowDown") next = (at + 1) % all.length;
    else if (e.key === "ArrowUp") next = (at - 1 + all.length) % all.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = all.length - 1;
    else if (e.key === "Tab") setOpen(false);
    if (next >= 0 && all.length > 0) {
      e.preventDefault();
      all[next].focus();
    }
  };

  return (
    <span style={{ position: "relative", display: "inline-flex" }}>
      <button
        ref={buttonRef}
        type="button"
        className={`fly-btn fly-btn--outline fly-btn--sm ${className}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-label={ariaLabel}
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
      >
        {label}
        {icon && <ChevronDown size={13} strokeWidth={1.75} aria-hidden="true" />}
      </button>
      {open && (
        <div
          ref={menuRef}
          id={menuId}
          role="menu"
          aria-label={ariaLabel ?? (typeof label === "string" ? label : undefined)}
          onKeyDown={onMenuKey}
          style={{
            position: "absolute",
            top: "calc(100% + 4px)",
            right: 0,
            zIndex: 20,
            minWidth: 200,
            display: "flex",
            flexDirection: "column",
            padding: 4,
            background: "var(--surface)",
            border: "1px solid var(--line)",
            borderRadius: "var(--radius-lg)",
            boxShadow: "var(--shadow-lg)",
          }}
        >
          {items.map((item) => (
            <button
              key={item.label}
              type="button"
              role={choice ? "menuitemradio" : "menuitem"}
              aria-checked={choice ? !!item.checked : undefined}
              tabIndex={-1}
              disabled={item.disabled}
              className={`cursor-pointer rounded-md px-3 py-2 text-left text-[13px] hover:bg-surface-3 focus-visible:bg-surface-3 disabled:cursor-not-allowed disabled:opacity-50 ${
                item.tone === "danger" ? "text-error-text" : "text-text"
              }`}
              style={{ border: 0, background: "none", outline: "none" }}
              onClick={() => {
                close();
                item.onSelect();
              }}
            >
              {choice && (
                <span aria-hidden="true" style={{ display: "inline-block", width: 16 }}>
                  {item.checked ? "✓" : ""}
                </span>
              )}
              {item.label}
            </button>
          ))}
        </div>
      )}
    </span>
  );
}

/** "Saved in your keychain" + [Replace], or a password field. */
export function SecretField({
  label,
  value,
  onChange,
  stored,
  error,
  hint,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  /** A value is already in the keychain (editing). */
  stored: boolean;
  error?: string;
  hint?: ReactNode;
}) {
  const [replacing, setReplacing] = useState(false);
  if (stored && !replacing) {
    return (
      <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
        <span className="fly-field-label">{label}</span>
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-text-2" style={{ ...SMALL, display: "inline-flex", gap: 6 }}>
            <Lock size={13} strokeWidth={1.75} aria-hidden="true" />
            {SAVED_IN_KEYCHAIN}
          </span>
          <Button
            variant="outline"
            size="xs"
            aria-label={`Replace ${label}`}
            onClick={() => setReplacing(true)}
          >
            Replace
          </Button>
        </div>
      </div>
    );
  }
  return (
    <Input
      label={label}
      type="password"
      autoComplete="off"
      spellCheck={false}
      value={value}
      error={error}
      hint={hint ?? (stored ? "Leave empty to keep the saved value." : undefined)}
      style={MONO}
      onChange={(e) => onChange(e.target.value)}
    />
  );
}

/** Key/value rows (request headers, or a program's settings), each with a
 *  Secret switch. Secret values go to the keychain; a saved one shows
 *  "Saved in your keychain" + [Replace], never the value. */
export function KVRows({
  rows,
  onChange,
  errors,
  kindLabel,
  addLabel,
}: {
  rows: KVRow[];
  onChange: (rows: KVRow[]) => void;
  errors: FormErrors;
  /** "Header" or "Setting". */
  kindLabel: string;
  addLabel: string;
}) {
  const [replacing, setReplacing] = useState<ReadonlySet<string>>(() => new Set());
  const patch = (id: string, p: Partial<KVRow>) =>
    onChange(rows.map((r) => (r.id === id ? { ...r, ...p } : r)));
  return (
    <div style={COLUMN}>
      {rows.map((r, i) => {
        const n = i + 1;
        const showStored = r.secret && r.stored && !replacing.has(r.id);
        const err = errors[`row:${r.id}`];
        return (
          <div key={r.id} style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <div className="flex flex-wrap items-center gap-2">
              <Input
                aria-label={`${kindLabel} ${n} name`}
                placeholder="Name"
                value={r.name}
                spellCheck={false}
                autoComplete="off"
                invalid={!!err}
                style={{ ...MONO, width: 170 }}
                onChange={(e) => patch(r.id, { name: e.target.value })}
              />
              {showStored ? (
                <span className="flex items-center gap-2" style={{ minWidth: 200 }}>
                  <span
                    className="text-text-2"
                    style={{ ...SMALL, display: "inline-flex", gap: 6 }}
                  >
                    <Lock size={13} strokeWidth={1.75} aria-hidden="true" />
                    {SAVED_IN_KEYCHAIN}
                  </span>
                  <Button
                    variant="outline"
                    size="xs"
                    aria-label={`Replace ${kindLabel.toLowerCase()} ${n} value`}
                    onClick={() => setReplacing((s) => new Set(s).add(r.id))}
                  >
                    Replace
                  </Button>
                </span>
              ) : (
                <Input
                  aria-label={`${kindLabel} ${n} value`}
                  placeholder={r.stored ? "Leave empty to keep" : "Value"}
                  type={r.secret ? "password" : "text"}
                  autoComplete="off"
                  spellCheck={false}
                  value={r.value}
                  invalid={!!err}
                  style={{ ...MONO, width: 200 }}
                  onChange={(e) => patch(r.id, { value: e.target.value })}
                />
              )}
              <span className="flex items-center gap-1.5 text-text-2" style={{ fontSize: 12 }}>
                <Switch
                  label={`${kindLabel} ${n} is secret`}
                  checked={r.secret}
                  onChange={(secret) => patch(r.id, { secret, stored: secret && r.stored })}
                />
                <span aria-hidden="true">Secret</span>
              </span>
              <button
                type="button"
                className="fly-modal-x"
                aria-label={`Remove ${kindLabel.toLowerCase()} ${n}`}
                onClick={() => onChange(rows.filter((x) => x.id !== r.id))}
              >
                <X size={14} strokeWidth={2} aria-hidden="true" />
              </button>
            </div>
            {err && (
              <p className="text-error-text" style={{ ...SMALL, fontSize: 12 }}>
                {err}
              </p>
            )}
          </div>
        );
      })}
      <div>
        <Button
          variant="ghost"
          size="sm"
          startIcon={<Plus size={13} strokeWidth={1.75} />}
          onClick={() =>
            onChange([
              ...rows,
              { id: newRowId(), name: "", value: "", secret: true, stored: false },
            ])
          }
        >
          {addLabel}
        </Button>
      </div>
    </div>
  );
}

/** Browser sign-in with a waiting state. `cancel` only hides the waiting
 *  state: a sign-in that finishes later still updates the connector. */
export function useSignIn(onChanged: (c: Connector) => void) {
  const [waiting, setWaiting] = useState(false);
  const seq = useRef(0);
  const start = async (
    id: string,
  ): Promise<{ result: ConnectorSaveResult | null; error: string | null; cancelled: boolean }> => {
    const mine = ++seq.current;
    setWaiting(true);
    try {
      const r = await api.signInConnector(id);
      if (r.connector) onChanged(r.connector);
      if (mine !== seq.current) return { result: r, error: null, cancelled: true };
      setWaiting(false);
      return { result: r, error: r.error, cancelled: false };
    } catch (e) {
      if (mine !== seq.current) return { result: null, error: null, cancelled: true };
      setWaiting(false);
      return { result: null, error: String(e), cancelled: false };
    }
  };
  const cancel = () => {
    seq.current++;
    setWaiting(false);
  };
  return { waiting, start, cancel };
}

/** "Finish signing in in your browser… [Cancel]". */
export function SignInWaiting({ onCancel }: { onCancel: () => void }) {
  return (
    <div
      role="status"
      className="flex flex-wrap items-center gap-3 rounded-xl border border-line bg-surface-2 px-3.5 py-3"
    >
      <span className="text-text" style={SMALL}>
        Finish signing in in your browser…
      </span>
      <Button variant="outline" size="xs" onClick={onCancel}>
        Cancel
      </Button>
    </div>
  );
}

/** A note with an icon (never colour alone). */
export function Callout({
  icon,
  tone = "neutral",
  children,
  role,
}: {
  icon: ReactNode;
  tone?: "neutral" | "warning";
  children: ReactNode;
  role?: string;
}) {
  return (
    <div
      role={role}
      className={tone === "warning" ? "text-warning-text" : "text-text-2"}
      style={{
        display: "flex",
        alignItems: "flex-start",
        gap: 8,
        padding: "10px 12px",
        borderRadius: "var(--radius-lg)",
        background: tone === "warning" ? "var(--warning-soft)" : "var(--surface-2)",
        border: "1px solid var(--line-2)",
        fontSize: 12.5,
        lineHeight: 1.5,
      }}
    >
      <span aria-hidden="true" style={{ flex: "none", marginTop: 1 }}>
        {icon}
      </span>
      <div style={{ minWidth: 0 }}>{children}</div>
    </div>
  );
}
