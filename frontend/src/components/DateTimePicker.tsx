import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronLeft, ChevronRight } from "lucide-react";
import { Button, Modal, SectionLabel } from "./ui";

/* ============================================================
   Meeting date & time editor — Material Design 3 layouts (modal
   date picker month grid with a year switcher, then the time-input
   step with a 12-hour AM/PM toggle), built entirely from the Fly
   design tokens/primitives. Times are edited in the machine's
   local timezone; the caller gets a UTC ISO string back.
   ============================================================ */

const WEEKDAYS = ["S", "M", "T", "W", "T", "F", "S"];
const YEAR_MIN = 1970;
const YEAR_MAX = new Date().getFullYear() + 10;

interface Props {
  /** Current value (ISO datetime, any offset). */
  value: string;
  saving?: boolean;
  error?: string | null;
  onCancel: () => void;
  /** Called with the edited moment as a UTC ISO string. */
  onSave: (isoUtc: string) => void;
}

export default function DateTimePicker({ value, saving = false, error, onCancel, onSave }: Props) {
  const initial = useMemo(() => {
    const d = new Date(value);
    return Number.isNaN(d.getTime()) ? new Date() : d;
  }, [value]);

  const [step, setStep] = useState<"date" | "time">("date");
  const [yearView, setYearView] = useState(false);
  const [viewYear, setViewYear] = useState(initial.getFullYear());
  const [viewMonth, setViewMonth] = useState(initial.getMonth()); // 0-based
  const [sel, setSel] = useState({
    y: initial.getFullYear(),
    m: initial.getMonth(),
    d: initial.getDate(),
  });
  const [hourStr, setHourStr] = useState(String(((initial.getHours() + 11) % 12) + 1));
  const [minStr, setMinStr] = useState(String(initial.getMinutes()).padStart(2, "0"));
  const [pm, setPm] = useState(initial.getHours() >= 12);

  const hour = parseInt(hourStr, 10);
  const minute = parseInt(minStr, 10);
  const timeValid = hour >= 1 && hour <= 12 && minute >= 0 && minute <= 59;

  const today = new Date();
  const selDate = new Date(sel.y, sel.m, sel.d);
  const headline = selDate.toLocaleDateString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    year: "numeric",
  });
  const monthLabel = new Date(viewYear, viewMonth, 1).toLocaleDateString(undefined, {
    month: "long",
    year: "numeric",
  });

  const moveMonth = (delta: number) => {
    const next = new Date(viewYear, viewMonth + delta, 1);
    setViewYear(next.getFullYear());
    setViewMonth(next.getMonth());
  };

  // Year switcher scrolls its selected year into view when opened.
  const yearRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (yearView) yearRef.current?.scrollIntoView({ block: "center" });
  }, [yearView]);

  const save = () => {
    if (!timeValid) return;
    const h24 = (hour % 12) + (pm ? 12 : 0);
    onSave(new Date(sel.y, sel.m, sel.d, h24, minute, 0, 0).toISOString());
  };

  const firstDow = new Date(viewYear, viewMonth, 1).getDay();
  const daysInMonth = new Date(viewYear, viewMonth + 1, 0).getDate();
  const isSel = (d: number) => sel.y === viewYear && sel.m === viewMonth && sel.d === d;
  const isToday = (d: number) =>
    today.getFullYear() === viewYear && today.getMonth() === viewMonth && today.getDate() === d;

  const timePreview = timeValid
    ? `${hour}:${String(minute).padStart(2, "0")} ${pm ? "PM" : "AM"}`
    : "—";

  const digits = (s: string) => s.replace(/\D/g, "").slice(0, 2);
  const timeBox =
    "h-[68px] w-[88px] rounded-[var(--radius-md)] border border-line bg-surface-2 text-center " +
    "font-display text-[30px] font-semibold text-text outline-none transition-colors " +
    "focus:border-primary focus:bg-primary-soft focus:text-primary-soft-text";
  const ampmBtn = (active: boolean) =>
    `h-[34px] w-[46px] cursor-pointer border-0 text-[12px] font-semibold transition-colors ${
      active
        ? "bg-primary-soft text-primary-soft-text"
        : "bg-surface text-text-3 hover:bg-surface-3 hover:text-text"
    }`;

  return (
    <Modal
      width={332}
      onClose={onCancel}
      closeOnOverlay={false}
      title="Edit date & time"
      footer={
        <>
          {error && (
            <span className="mr-auto max-w-[180px] text-[11.5px] leading-snug text-error-text">
              {error}
            </span>
          )}
          {step === "date" ? (
            <>
              <Button variant="ghost" size="sm" onClick={onCancel}>
                Cancel
              </Button>
              <Button variant="primary" size="sm" onClick={() => setStep("time")}>
                Next
              </Button>
            </>
          ) : (
            <>
              <Button variant="ghost" size="sm" onClick={() => setStep("date")} disabled={saving}>
                Back
              </Button>
              <Button variant="primary" size="sm" onClick={save} disabled={!timeValid || saving}>
                {saving ? "Saving…" : "Save"}
              </Button>
            </>
          )}
        </>
      }
    >
      {/* MD3 header: overline + selected-value headline */}
      <SectionLabel>{step === "date" ? "Select date" : "Select time"}</SectionLabel>
      <div className="mt-1.5 mb-3 border-b border-line pb-3 font-display text-[22px] font-bold tracking-[-0.01em] text-text">
        {step === "date" ? headline : `${headline} · ${timePreview}`}
      </div>

      {step === "date" ? (
        <div>
          {/* month/year row: label toggles the year switcher */}
          <div className="mb-2 flex items-center justify-between">
            <button
              onClick={() => setYearView((v) => !v)}
              className="inline-flex cursor-pointer items-center gap-1 rounded-[var(--radius-sm)] border-0 bg-transparent px-2 py-1 text-[13px] font-semibold text-text-2 hover:bg-surface-3 hover:text-text"
              aria-label="Switch year"
            >
              {monthLabel}
              <ChevronDown
                size={14}
                strokeWidth={2}
                className={`transition-transform ${yearView ? "rotate-180" : ""}`}
              />
            </button>
            {!yearView && (
              <span className="flex items-center gap-1">
                <button
                  onClick={() => moveMonth(-1)}
                  aria-label="Previous month"
                  className="inline-flex h-7 w-7 cursor-pointer items-center justify-center rounded-[var(--radius-sm)] border-0 bg-transparent text-text-3 hover:bg-surface-3 hover:text-text"
                >
                  <ChevronLeft size={16} strokeWidth={2} />
                </button>
                <button
                  onClick={() => moveMonth(1)}
                  aria-label="Next month"
                  className="inline-flex h-7 w-7 cursor-pointer items-center justify-center rounded-[var(--radius-sm)] border-0 bg-transparent text-text-3 hover:bg-surface-3 hover:text-text"
                >
                  <ChevronRight size={16} strokeWidth={2} />
                </button>
              </span>
            )}
          </div>

          {yearView ? (
            <div className="grid max-h-[252px] grid-cols-3 gap-1.5 overflow-y-auto pr-1">
              {Array.from({ length: YEAR_MAX - YEAR_MIN + 1 }, (_, i) => YEAR_MIN + i).map((y) => {
                const active = y === viewYear;
                return (
                  <button
                    key={y}
                    ref={active ? yearRef : undefined}
                    onClick={() => {
                      setViewYear(y);
                      setYearView(false);
                    }}
                    className={`h-8 cursor-pointer rounded-[var(--radius-pill)] border-0 text-[13px] transition-colors ${
                      active
                        ? "bg-primary font-semibold text-on-primary"
                        : y === today.getFullYear()
                          ? "bg-transparent font-semibold text-primary-text hover:bg-surface-3"
                          : "bg-transparent text-text-2 hover:bg-surface-3"
                    }`}
                  >
                    {y}
                  </button>
                );
              })}
            </div>
          ) : (
            <div className="grid grid-cols-7 justify-items-center gap-y-0.5">
              {WEEKDAYS.map((w, i) => (
                <span
                  key={`${w}${i}`}
                  className="flex h-8 w-9 items-center justify-center text-[11px] font-medium text-text-3"
                >
                  {w}
                </span>
              ))}
              {Array.from({ length: firstDow }, (_, i) => (
                <span key={`pad${i}`} className="h-9 w-9" />
              ))}
              {Array.from({ length: daysInMonth }, (_, i) => i + 1).map((d) => (
                <button
                  key={d}
                  onClick={() => setSel({ y: viewYear, m: viewMonth, d })}
                  className={`h-9 w-9 cursor-pointer rounded-full text-[13px] transition-colors ${
                    isSel(d)
                      ? "border-0 bg-primary font-semibold text-on-primary"
                      : isToday(d)
                        ? "border border-primary bg-transparent font-semibold text-primary-text hover:bg-surface-3"
                        : "border-0 bg-transparent text-text hover:bg-surface-3"
                  }`}
                >
                  {d}
                </button>
              ))}
            </div>
          )}
        </div>
      ) : (
        <div className="flex items-start justify-center gap-2.5 py-3">
          <span className="flex flex-col items-center gap-1.5">
            <input
              value={hourStr}
              onChange={(e) => setHourStr(digits(e.target.value))}
              onBlur={() => {
                if (hour >= 1 && hour <= 12) setHourStr(String(hour));
              }}
              onFocus={(e) => e.target.select()}
              inputMode="numeric"
              aria-label="Hour"
              className={timeBox}
            />
            <span className="text-[11px] text-text-3">Hour</span>
          </span>
          <span className="pt-[14px] font-display text-[30px] font-semibold text-text-3">:</span>
          <span className="flex flex-col items-center gap-1.5">
            <input
              value={minStr}
              onChange={(e) => setMinStr(digits(e.target.value))}
              onBlur={() => {
                if (minute >= 0 && minute <= 59) setMinStr(String(minute).padStart(2, "0"));
              }}
              onFocus={(e) => e.target.select()}
              inputMode="numeric"
              aria-label="Minute"
              className={timeBox}
            />
            <span className="text-[11px] text-text-3">Minute</span>
          </span>
          <span
            className="ml-1.5 flex flex-col overflow-hidden rounded-[var(--radius-md)] border border-line"
            role="group"
            aria-label="AM or PM"
          >
            <button onClick={() => setPm(false)} className={ampmBtn(!pm)} aria-pressed={!pm}>
              AM
            </button>
            <span className="h-px bg-line" />
            <button onClick={() => setPm(true)} className={ampmBtn(pm)} aria-pressed={pm}>
              PM
            </button>
          </span>
        </div>
      )}
    </Modal>
  );
}
