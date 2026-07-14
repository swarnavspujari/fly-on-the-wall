import { useCallback, useEffect, useRef, useState } from "react";
import { Check, Plus, RefreshCw, X } from "lucide-react";
import type { Attendee, Meeting, Transcript } from "../types";
import { api } from "../api";
import { Avatar } from "./ui";

/* ============================================================
   Attendee pill → popover editor (design: templates/attendee-editor).
   One component, five states:
     1. empty dashed "Add attendees" pill
     2. popover editor — "Just you" toggle, fixed You row, name rows
     3. calendar-seeded rows (name prefilled with the email, select-all on
        focus; the original email is kept as a quiet caption after rename)
     4. post-save "Re-analyze speakers" offer + undo (toast + footer, 10 min)
     5. (transcript speaker dropdown lives in TranscriptPanel)
   Keyboard: tab reaches the pill · enter opens · esc closes · ↵ adds a row
   · ⌘Z undoes re-analysis while the undo window is open.
   ============================================================ */

/** How long the undo affordance stays reachable after a re-analyze. */
const UNDO_WINDOW_MS = 10 * 60_000;

const displayName = (a: Attendee): string => (a.name.trim() ? a.name.trim() : (a.email ?? ""));

const CSS = `
.fotw-att-pill:focus-visible{outline:none;box-shadow:var(--focus-ring)}
.fotw-att-pill:hover{background:var(--surface-3);color:var(--text)}
.fotw-att-row-x{opacity:0}
.fotw-att-row:hover .fotw-att-row-x,.fotw-att-row-x:focus-visible{opacity:1}
`;
if (typeof document !== "undefined" && !document.getElementById("fotw-att-css")) {
  const el = document.createElement("style");
  el.id = "fotw-att-css";
  el.textContent = CSS;
  document.head.appendChild(el);
}

interface RowDraft {
  /** Stable per-draft key so removing rows doesn't reshuffle inputs. */
  key: number;
  name: string;
  email: string | null;
  /** Came in with a calendar email → renaming keeps it as a quiet caption. */
  seeded: boolean;
}

interface Props {
  meeting: Meeting;
  /** A transcript exists → saving offers "Re-analyze speakers". */
  hasTranscript: boolean;
  onMeetingChanged: (m: Meeting) => void;
  /** Called with the restored transcript after an undo. */
  onTranscriptRestored: (t: Transcript) => void;
}

export default function AttendeeEditor({
  meeting,
  hasTranscript,
  onMeetingChanged,
  onTranscriptRestored,
}: Props) {
  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<RowDraft[]>([]);
  const [justYou, setJustYou] = useState(false);
  const [savedFlash, setSavedFlash] = useState(false);
  const [busy, setBusy] = useState<"saving" | "analyzing" | "reverting" | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Undo affordance: present while a snapshot exists and is younger than the
  // 10-minute window. `undoAt`/`undoChanged` mirror the backend snapshot.
  const [undoAt, setUndoAt] = useState<number | null>(null);
  const [undoChanged, setUndoChanged] = useState(0);
  const [toast, setToast] = useState(false);
  // clock state (not Date.now() in render) so the window expires on screen
  const [now, setNow] = useState(() => Date.now());
  const rowKey = useRef(1);
  const popRef = useRef<HTMLDivElement>(null);
  const rowsRef = useRef<HTMLDivElement>(null);

  const undoLive = undoAt != null && now - undoAt < UNDO_WINDOW_MS;

  // Load undo availability for this meeting (survives popover close/reopen
  // and app restarts — the snapshot lives in storage).
  useEffect(() => {
    let stale = false;
    setUndoAt(null);
    setToast(false);
    api
      .speakerUndoState(meeting.id)
      .then((u) => {
        if (stale || !u) return;
        setUndoAt(new Date(u.taken_at).getTime());
        setUndoChanged(u.changed_segments);
      })
      .catch(() => {});
    return () => {
      stale = true;
    };
  }, [meeting.id]);

  // Tick the clock while an undo exists so the affordance disappears when
  // the window closes without any interaction.
  useEffect(() => {
    if (undoAt == null) return;
    setNow(Date.now());
    const t = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(t);
  }, [undoAt]);

  const openEditor = () => {
    setRows(
      meeting.attendees.map((a) => ({
        key: rowKey.current++,
        name: displayName(a),
        email: a.email ?? null,
        seeded: a.email != null,
      })),
    );
    setJustYou(meeting.attendees_confirmed && meeting.attendees.length === 0);
    setSavedFlash(false);
    setError(null);
    setOpen(true);
  };

  // esc closes (only the popover — never steals from an open menu elsewhere)
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open]);

  const revert = useCallback(async () => {
    if (busy) return;
    setBusy("reverting");
    setError(null);
    try {
      const t = await api.revertSpeakerAssignment(meeting.id);
      onTranscriptRestored(t);
      setUndoAt(null);
      setToast(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }, [busy, meeting.id, onTranscriptRestored]);

  // ⌘Z / Ctrl+Z while the undo window is open (skipped inside text editing).
  useEffect(() => {
    if (!undoLive) return;
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== "z") return;
      const t = e.target as HTMLElement | null;
      if (t && (t.isContentEditable || t.tagName === "INPUT" || t.tagName === "TEXTAREA")) return;
      e.preventDefault();
      void revert();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [undoLive, revert]);

  const addRow = () => {
    setRows((r) => [...r, { key: rowKey.current++, name: "", email: null, seeded: false }]);
    // focus the new row's name input once it exists
    window.setTimeout(() => {
      const inputs = rowsRef.current?.querySelectorAll<HTMLInputElement>("input[data-name-input]");
      inputs?.[inputs.length - 1]?.focus();
    }, 0);
  };

  const save = async () => {
    setBusy("saving");
    setError(null);
    try {
      const attendees: Attendee[] = justYou
        ? []
        : rows
            .map((r) => ({ name: r.name.trim(), email: r.email }))
            .filter((a) => a.name || a.email)
            .map((a) => ({ name: a.name || (a.email ?? ""), email: a.email }));
      const updated = await api.updateMeetingAttendees(meeting.id, attendees);
      onMeetingChanged(updated);
      if (hasTranscript) {
        setSavedFlash(true); // keep the popover open and offer re-analysis
      } else {
        setOpen(false);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const reanalyze = async () => {
    setBusy("analyzing");
    setError(null);
    try {
      const out = await api.reDiarizeMeeting(meeting.id);
      setUndoAt(Date.now());
      setUndoChanged(out.changed_segments);
      setToast(true);
      setSavedFlash(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  /* ---------- the pill ---------- */
  const atts = meeting.attendees;
  const isJustYou = meeting.attendees_confirmed && atts.length === 0;
  const pillLabel =
    atts.length === 0
      ? isJustYou
        ? "Just you"
        : "Add attendees"
      : `${displayName(atts[0])}${atts.length > 1 ? ` +${atts.length - 1}` : ""}`;
  const dashed = atts.length === 0 && !isJustYou;

  const pill = (
    <button
      onClick={() => (open ? setOpen(false) : openEditor())}
      title="Attendees — who was on this meeting"
      aria-haspopup="dialog"
      aria-expanded={open}
      className="fotw-att-pill inline-flex cursor-pointer items-center gap-1.5 rounded-full py-1 text-[12.5px] font-medium"
      style={{
        border: dashed ? "1px dashed var(--line-strong)" : "1px solid var(--line)",
        background: open ? "var(--primary-soft)" : dashed ? "transparent" : "var(--surface-2)",
        color: open ? "var(--primary-soft-text)" : "var(--text-2)",
        borderColor: open ? "var(--primary-border)" : undefined,
        padding: atts.length > 0 ? "3px 10px 3px 6px" : "4px 11px",
      }}
    >
      {atts.length > 0 ? (
        <span className="flex">
          {atts.slice(0, 3).map((a, i) => (
            <Avatar
              key={i}
              name={displayName(a)}
              index={i}
              shape="circle"
              size="xs"
              style={{ marginLeft: i ? -6 : 0, boxShadow: "0 0 0 2px var(--surface-2)" }}
            />
          ))}
        </span>
      ) : (
        <Plus size={13} strokeWidth={2} style={{ color: "var(--text-3)" }} />
      )}
      {pillLabel}
    </button>
  );

  /* ---------- popover rows ---------- */
  const rowEls = rows.map((row, i) => {
    const seeded = row.seeded && row.name === row.email;
    const renamedFromEmail = row.seeded && row.email != null && row.name !== row.email;
    return (
      <div key={row.key} className="fotw-att-row flex items-start gap-2 px-1.5 py-[5px]">
        {row.name || row.email ? (
          <Avatar
            name={row.name || row.email || "?"}
            index={i}
            shape="circle"
            size="sm"
            style={{ width: 24, height: 24, fontSize: 10, marginTop: 3 }}
          />
        ) : (
          <span
            className="grid flex-none place-items-center rounded-full font-mono"
            style={{
              width: 24,
              height: 24,
              marginTop: 3,
              fontSize: 10,
              fontWeight: 600,
              color: "var(--text-3)",
              border: "1.5px dashed var(--line-strong)",
            }}
          >
            ?
          </span>
        )}
        <span className="min-w-0 flex-1">
          <input
            data-name-input
            value={row.name}
            placeholder="Name"
            aria-label="Name"
            onFocus={(e) => {
              // calendar-seeded: the whole email is selected so typing
              // replaces it in one stroke
              if (seeded) e.currentTarget.select();
            }}
            onChange={(e) =>
              setRows((rs) =>
                rs.map((r) => (r.key === row.key ? { ...r, name: e.target.value } : r)),
              )
            }
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                if (i === rows.length - 1) addRow();
                else {
                  const inputs =
                    rowsRef.current?.querySelectorAll<HTMLInputElement>("input[data-name-input]");
                  inputs?.[i + 1]?.focus();
                }
              }
            }}
            className="w-full rounded-md border border-line bg-surface px-2 py-1.5 text-[13px] font-medium text-text outline-none focus:border-primary"
            style={{ boxSizing: "border-box" }}
          />
          {renamedFromEmail && (
            <span
              className="mt-[3px] block font-mono text-[10px]"
              style={{ color: "var(--text-3)", marginLeft: 2 }}
            >
              {row.email} — kept
            </span>
          )}
          {!row.seeded && (
            <input
              value={row.email ?? ""}
              placeholder="Email — optional"
              aria-label="Email (optional)"
              onChange={(e) =>
                setRows((rs) =>
                  rs.map((r) => (r.key === row.key ? { ...r, email: e.target.value || null } : r)),
                )
              }
              className="mt-1 w-full rounded-md border bg-surface px-2 py-1 font-mono text-[11px] outline-none"
              style={{
                borderColor: "var(--line-2)",
                color: "var(--text-2)",
                boxSizing: "border-box",
              }}
            />
          )}
        </span>
        <button
          title="Remove"
          aria-label={`Remove ${row.name || "attendee"}`}
          onClick={() => setRows((rs) => rs.filter((r) => r.key !== row.key))}
          className="fotw-att-row-x mt-1 grid h-6 w-6 flex-none cursor-pointer place-items-center rounded border-0 bg-transparent text-text-3 hover:bg-surface-3 hover:text-text"
        >
          <X size={12} strokeWidth={2} />
        </button>
      </div>
    );
  });

  const othersCount = justYou ? 0 : rows.filter((r) => r.name.trim() || r.email).length;
  const seededCount = rows.filter((r) => r.email != null).length;

  return (
    <span className="relative inline-flex">
      {pill}

      {open && (
        <>
          <span className="fixed inset-0 z-10" onClick={() => setOpen(false)} aria-hidden="true" />
          <div
            ref={popRef}
            role="dialog"
            aria-label="Attendees"
            className="absolute left-0 z-20"
            style={{
              top: "calc(100% + 8px)",
              width: 344,
              background: "var(--surface)",
              border: "1px solid var(--line)",
              borderRadius: "var(--radius-2xl)",
              boxShadow: "var(--shadow-pop)",
            }}
          >
            {/* header */}
            <div className="flex items-center justify-between px-3.5 pt-3">
              <span
                className="text-[10.5px] font-semibold uppercase"
                style={{ letterSpacing: ".09em", color: "var(--text-3)" }}
              >
                Attendees
              </span>
              {savedFlash ? (
                <span
                  className="inline-flex items-center gap-1 text-[11px] font-semibold"
                  style={{ color: "var(--success-text)" }}
                >
                  <Check size={12} strokeWidth={2.5} /> Saved
                </span>
              ) : !meeting.attendees_confirmed && seededCount > 0 ? (
                <span
                  className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase"
                  style={{
                    letterSpacing: ".05em",
                    background: "var(--primary-soft)",
                    color: "var(--primary-soft-text)",
                  }}
                >
                  From calendar · {seededCount}
                </span>
              ) : (
                <span className="font-mono text-[10.5px]" style={{ color: "var(--text-3)" }}>
                  esc closes
                </span>
              )}
            </div>

            {/* Just-you toggle */}
            <button
              onClick={() => setJustYou((v) => !v)}
              role="switch"
              aria-checked={justYou}
              className="mx-3.5 my-2.5 flex w-[calc(100%-28px)] cursor-pointer items-center justify-between gap-2.5 rounded-lg border px-2.5 py-2 text-left"
              style={{
                borderColor: justYou ? "var(--primary-border)" : "var(--line)",
                background: justYou ? "var(--primary-soft)" : "var(--surface-2)",
              }}
            >
              <span>
                <span className="block text-[13px] font-semibold text-text">Just you</span>
                <span
                  className="block text-[11px]"
                  style={{ color: justYou ? "var(--primary-soft-text)" : "var(--text-3)" }}
                >
                  {justYou ? "Everything is attributed to you" : "No one else on the mic"}
                </span>
              </span>
              <span
                className="relative flex-none rounded-full"
                style={{
                  width: 34,
                  height: 20,
                  background: justYou ? "var(--primary)" : "var(--line-strong)",
                  transition: "background-color var(--dur-fast) var(--ease-out)",
                }}
              >
                <span
                  className="absolute top-[2px] rounded-full"
                  style={{
                    width: 16,
                    height: 16,
                    left: justYou ? 16 : 2,
                    background: justYou ? "var(--on-primary)" : "var(--surface)",
                    transition: "left var(--dur-fast) var(--ease-out)",
                  }}
                />
              </span>
            </button>

            {/* You row — always included, never removable */}
            <div
              className="flex items-center gap-2 px-3.5"
              style={{ paddingBottom: justYou ? 12 : 2 }}
            >
              <Avatar
                self
                name="You"
                shape="circle"
                size="sm"
                style={{ width: 24, height: 24, fontSize: 10 }}
              />
              <span className="text-[13px] font-semibold text-text">You</span>
              {!justYou && (
                <span className="ml-auto text-[11px]" style={{ color: "var(--text-3)" }}>
                  always included
                </span>
              )}
            </div>

            {/* attendee rows + add (collapsed entirely by Just-you) */}
            {!justYou && (
              <>
                <div ref={rowsRef} className="px-2">
                  {rowEls}
                </div>
                <div className="flex items-center justify-between px-3.5 pb-2 pt-1.5">
                  <button
                    onClick={addRow}
                    className="inline-flex cursor-pointer items-center gap-1.5 border-0 bg-transparent p-0 text-[12.5px] font-semibold"
                    style={{ color: "var(--primary-text)" }}
                  >
                    <Plus size={13} strokeWidth={2.25} /> Add person
                  </button>
                  <span className="font-mono text-[10.5px]" style={{ color: "var(--text-3)" }}>
                    {seededCount > 0 && !meeting.attendees_confirmed
                      ? "didn't show? remove them"
                      : "↵ adds a row"}
                  </span>
                </div>
              </>
            )}

            {error && (
              <div
                className="mx-3.5 mb-2 rounded-lg px-2.5 py-1.5 text-[12px]"
                style={{ background: "var(--error-soft)", color: "var(--error-text)" }}
                role="alert"
              >
                {error}
              </div>
            )}

            {/* re-analyze offer — after a save on a transcribed meeting */}
            {savedFlash && hasTranscript && (
              <div className="border-t border-line px-3.5 py-3">
                <button
                  onClick={() => void reanalyze()}
                  disabled={busy != null}
                  className="inline-flex cursor-pointer items-center gap-1.5 rounded-md border px-3 py-1.5 text-[12.5px] font-semibold"
                  style={{
                    borderColor: "var(--primary-border)",
                    background: "var(--primary-soft)",
                    color: "var(--primary-soft-text)",
                    opacity: busy ? 0.7 : 1,
                  }}
                >
                  <RefreshCw
                    size={13}
                    strokeWidth={2}
                    className={busy === "analyzing" ? "animate-spin" : undefined}
                  />
                  {busy === "analyzing" ? "Re-analyzing…" : "Re-analyze speakers"}
                </button>
                <div
                  className="mt-1.5 text-[11px] leading-[1.45]"
                  style={{ color: "var(--text-3)" }}
                >
                  Uses the attendee count to improve who-said-what. Takes ~seconds. The transcript
                  text is untouched.
                </div>
              </div>
            )}

            {/* footer: undo strip (10 min) or summary + actions */}
            {undoLive && !savedFlash ? (
              <div
                className="flex items-center gap-2 border-t border-line px-3.5 py-2.5"
                style={{
                  background: "var(--success-soft)",
                  borderRadius: "0 0 var(--radius-2xl) var(--radius-2xl)",
                }}
              >
                <span
                  className="text-[12px] font-semibold"
                  style={{ color: "var(--success-text)" }}
                >
                  ✓ Speakers re-analyzed
                </span>
                <span className="mr-auto text-[11px]" style={{ color: "var(--text-3)" }}>
                  {fmtAgo(now, undoAt!)}
                </span>
                <button
                  onClick={() => void revert()}
                  disabled={busy != null}
                  className="cursor-pointer border-0 bg-transparent px-1.5 py-1 text-[12.5px] font-semibold underline"
                  style={{ color: "var(--primary-text)", textUnderlineOffset: 2 }}
                >
                  {busy === "reverting" ? "Undoing…" : "Undo"}
                </button>
              </div>
            ) : (
              <div className="flex items-center gap-2 border-t border-line px-3.5 py-2.5">
                <span className="mr-auto text-[11.5px]" style={{ color: "var(--text-3)" }}>
                  {justYou
                    ? "Just you"
                    : `You + ${othersCount} other${othersCount === 1 ? "" : "s"}`}
                </span>
                <button
                  onClick={() => setOpen(false)}
                  className="cursor-pointer rounded-md border-0 bg-transparent px-2.5 py-1.5 text-[12.5px] font-semibold text-text-2 hover:bg-surface-3"
                >
                  Cancel
                </button>
                <button
                  onClick={() => void save()}
                  disabled={busy != null}
                  className="cursor-pointer rounded-md border-0 px-3.5 py-1.5 text-[12.5px] font-semibold"
                  style={{
                    background: "var(--primary)",
                    color: "var(--on-primary)",
                    opacity: busy ? 0.7 : 1,
                  }}
                >
                  {busy === "saving" ? "Saving…" : "Save"}
                </button>
              </div>
            )}
          </div>
        </>
      )}

      {/* success toast — dismissing it doesn't strand the undo (it stays in
          the popover footer for the same 10 minutes) */}
      {toast && undoLive && (
        <div
          className="print:hidden fixed bottom-5 right-5 z-50 flex items-center gap-3 rounded-xl border border-line bg-surface py-2.5 pl-3.5 pr-3"
          style={{ boxShadow: "var(--shadow-lg)" }}
          role="status"
        >
          <span
            className="grid h-[22px] w-[22px] flex-none place-items-center rounded-full text-[11px]"
            style={{ background: "var(--success-soft)", color: "var(--success-text)" }}
          >
            ✓
          </span>
          <span>
            <span className="block text-[12.5px] font-semibold text-text">
              Speakers re-analyzed — {undoChanged} line{undoChanged === 1 ? "" : "s"} re-attributed
            </span>
            <span
              className="mt-px block font-mono text-[10.5px]"
              style={{ color: "var(--text-3)" }}
            >
              undo stays available for 10 min · {navigator.platform.includes("Mac") ? "⌘" : "Ctrl+"}
              Z
            </span>
          </span>
          <button
            onClick={() => void revert()}
            disabled={busy != null}
            className="cursor-pointer rounded-md border border-line bg-transparent px-3 py-1 text-[12px] font-semibold text-text hover:bg-surface-3"
          >
            {busy === "reverting" ? "Undoing…" : "Undo"}
          </button>
          <button
            title="Dismiss"
            aria-label="Dismiss"
            onClick={() => setToast(false)}
            className="cursor-pointer border-0 bg-transparent p-1 text-text-3 hover:text-text"
          >
            <X size={12} strokeWidth={2} />
          </button>
        </div>
      )}
    </span>
  );
}

function fmtAgo(now: number, ts: number): string {
  const mins = Math.floor((now - ts) / 60_000);
  if (mins <= 0) return "just now";
  return `${mins} min ago`;
}
