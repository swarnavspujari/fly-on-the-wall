import type {
  ButtonHTMLAttributes,
  HTMLAttributes,
  InputHTMLAttributes,
  ReactNode,
  SelectHTMLAttributes,
} from "react";
import { useEffect, useId } from "react";

/* ============================================================
   Fly on the Wall — shared primitives
   ------------------------------------------------------------
   A faithful TS port of the design system's 12 components
   (design-system/project/components/**). They are token-driven:
   every color, radius, font and shadow reads a var(--…) defined
   in index.css, so the primitives switch with data-theme for free.

   Stateful styling (hover / focus-visible / :checked / ::placeholder
   / keyframes) lives in a single injected stylesheet — the same
   approach the design ships, since those states can't be expressed
   with inline styles. Screens compose Tailwind utilities (also
   token-mapped) on top.

   Backward-compat exports (Btn, ModalShell, speakerColor,
   speakerInitials) are kept at the bottom for the not-yet-restyled
   screens; remove them once every screen adopts the new API.
   ============================================================ */

const CSS = `
/* Button */
.fly-btn{display:inline-flex;align-items:center;justify-content:center;
  font-family:var(--font-sans);font-weight:var(--fw-semibold);white-space:nowrap;cursor:pointer;
  border:1px solid transparent;transition:background-color var(--dur-fast) var(--ease-out),
  color var(--dur-fast) var(--ease-out),border-color var(--dur-fast) var(--ease-out),
  filter var(--dur-fast) var(--ease-out),box-shadow var(--dur-fast) var(--ease-out)}
.fly-btn:focus-visible{outline:none;box-shadow:var(--focus-ring)}
.fly-btn:disabled{opacity:.5;pointer-events:none}
.fly-btn--xs{gap:4px;border-radius:var(--radius-sm);padding:3px 7px;font-size:11px;line-height:1.3}
.fly-btn--sm{gap:6px;border-radius:var(--radius-sm);padding:5px 11px;font-size:12px;line-height:1.35}
.fly-btn--md{gap:8px;border-radius:var(--radius-md);padding:7px 15px;font-size:13px;line-height:1.4}
.fly-btn--lg{gap:9px;border-radius:var(--radius-lg);padding:11px 20px;font-size:15px;line-height:1.4}
.fly-btn--primary{background:var(--primary);color:var(--on-primary)}
.fly-btn--primary:hover{filter:brightness(1.06)}
.fly-btn--primary:active{filter:brightness(.94)}
.fly-btn--soft{background:var(--primary-soft);color:var(--primary-soft-text);border-color:var(--line)}
.fly-btn--soft:hover{background:color-mix(in srgb,var(--primary-soft) 82%,var(--primary))}
.fly-btn--outline{background:var(--surface);color:var(--text-2);border-color:var(--line)}
.fly-btn--outline:hover{background:var(--surface-3);color:var(--text);border-color:var(--line-strong)}
.fly-btn--ghost{background:transparent;color:var(--text-2)}
.fly-btn--ghost:hover{background:var(--surface-3);color:var(--text)}
.fly-btn--contrast{background:var(--text);color:var(--surface)}
.fly-btn--contrast:hover{filter:brightness(1.15)}
.fly-btn--record{background:var(--rec);color:var(--on-rec)}
.fly-btn--record:hover{filter:brightness(1.06)}

/* Input */
.fly-field-wrap{display:flex;flex-direction:column;gap:6px;min-width:0}
.fly-field-label{font-family:var(--font-sans);font-size:var(--text-ui-sm);font-weight:var(--fw-medium);color:var(--text-2)}
.fly-input{width:100%;font-family:var(--font-sans);font-size:var(--text-ui);color:var(--text);
  background:var(--surface);border:1px solid var(--line);border-radius:var(--radius-md);padding:8px 12px;outline:none;
  transition:border-color var(--dur-fast) var(--ease-out),box-shadow var(--dur-fast) var(--ease-out)}
.fly-input::placeholder{color:var(--text-3)}
.fly-input:hover{border-color:var(--line-strong)}
.fly-input:focus-visible,.fly-input:focus{border-color:var(--primary);box-shadow:var(--focus-ring-tight)}
.fly-input:disabled{opacity:.55;cursor:not-allowed}
.fly-input--invalid{border-color:var(--error)}
.fly-input--invalid:focus{box-shadow:0 0 0 2px color-mix(in srgb,var(--error) 45%,transparent)}
.fly-field-msg{font-size:var(--text-micro);color:var(--text-3)}
.fly-field-msg--err{color:var(--error-text)}

/* Select */
.fly-select-wrap{position:relative;display:inline-flex;align-items:center;width:100%}
.fly-select{appearance:none;-webkit-appearance:none;width:100%;
  font-family:var(--font-sans);font-size:var(--text-ui);font-weight:var(--fw-medium);color:var(--text);
  background:var(--surface);border:1px solid var(--line);border-radius:var(--radius-md);
  padding:8px 34px 8px 12px;cursor:pointer;outline:none;
  transition:border-color var(--dur-fast) var(--ease-out),box-shadow var(--dur-fast) var(--ease-out),background-color var(--dur-fast) var(--ease-out)}
.fly-select:hover{border-color:var(--line-strong);background:var(--surface-3)}
.fly-select:focus-visible,.fly-select:focus{border-color:var(--primary);box-shadow:var(--focus-ring-tight)}
.fly-select:disabled{opacity:.55;cursor:not-allowed}
.fly-select-chevron{position:absolute;right:11px;pointer-events:none;color:var(--text-3);display:flex}

/* Checkbox / radio */
.fly-check{display:inline-flex;align-items:flex-start;gap:9px;cursor:pointer;font-family:var(--font-sans);
  font-size:var(--text-body-sm);color:var(--text);line-height:1.45}
.fly-check--disabled{opacity:.55;cursor:not-allowed}
.fly-check input{position:absolute;opacity:0;width:0;height:0}
.fly-check-box{flex:none;width:18px;height:18px;margin-top:1px;display:grid;place-items:center;
  background:var(--surface);border:1.5px solid var(--line-strong);
  transition:background-color var(--dur-fast) var(--ease-out),border-color var(--dur-fast) var(--ease-out),box-shadow var(--dur-fast) var(--ease-out)}
.fly-check-box--checkbox{border-radius:6px}
.fly-check-box--radio{border-radius:50%}
.fly-check-box svg,.fly-check-box .fly-radio-dot{opacity:0;transition:opacity var(--dur-fast) var(--ease-out)}
.fly-radio-dot{width:8px;height:8px;border-radius:50%;background:var(--on-primary)}
.fly-check input:checked + .fly-check-box{background:var(--primary);border-color:var(--primary)}
.fly-check input:checked + .fly-check-box svg,
.fly-check input:checked + .fly-check-box .fly-radio-dot{opacity:1}
.fly-check input:focus-visible + .fly-check-box{box-shadow:var(--focus-ring)}
.fly-check-body{display:flex;flex-direction:column;gap:1px;min-width:0}
.fly-check-desc{font-size:var(--text-ui-sm);color:var(--text-3)}

/* Badge */
.fly-badge{display:inline-flex;align-items:center;gap:5px;font-family:var(--font-sans);
  font-weight:var(--fw-semibold);border-radius:var(--radius-sm);white-space:nowrap;line-height:1}
.fly-badge--sm{font-size:9.5px;padding:3px 6px;letter-spacing:.04em}
.fly-badge--md{font-size:11px;padding:4px 8px;letter-spacing:.02em}
.fly-badge--upper{text-transform:uppercase;letter-spacing:.07em}
.fly-badge-dot{width:6px;height:6px;border-radius:50%;background:currentColor;flex:none}
.fly-badge--live .fly-badge-dot{animation:fly-pulse-dot 1.2s ease infinite}
.fly-badge--neutral{background:var(--surface-3);color:var(--text-2)}
.fly-badge--primary{background:var(--primary-soft);color:var(--primary-soft-text)}
.fly-badge--success{background:var(--success-soft);color:var(--success-text)}
.fly-badge--warning{background:var(--warning-soft);color:var(--warning-text)}
.fly-badge--danger{background:var(--error-soft);color:var(--error-text)}
.fly-badge--live{background:var(--rec);color:var(--on-rec)}
.fly-badge--accent{background:var(--accent);color:var(--on-accent)}

/* ProgressBar */
.fly-progress{display:block;width:100%;overflow:hidden;background:var(--line);border-radius:var(--radius-pill)}
.fly-progress--sm{height:6px}
.fly-progress--md{height:8px}
.fly-progress-bar{display:block;height:100%;border-radius:var(--radius-pill);
  background:var(--primary);transition:width var(--dur-slow) var(--ease-out)}
.fly-progress-bar--indeterminate{width:35% !important;animation:fly-progress-slide 1.15s var(--ease-in-out) infinite}
@keyframes fly-progress-slide{0%{margin-left:-35%}100%{margin-left:100%}}

/* RecordingIndicator */
.fly-rec{display:inline-flex;align-items:center;gap:9px;font-family:var(--font-sans);font-size:var(--text-ui);color:var(--text)}
.fly-rec-dot{flex:none;border-radius:50%}
.fly-rec-dot--rec{background:var(--rec);animation:fly-pulse-dot 1.2s ease infinite}
.fly-rec-dot--paused{background:var(--warning)}
.fly-rec-dot--sm{width:9px;height:9px}
.fly-rec-dot--md{width:11px;height:11px}
.fly-rec-label{font-weight:var(--fw-semibold)}
.fly-rec-time{font-family:var(--font-mono);font-variant-numeric:tabular-nums;font-size:var(--text-ui);color:var(--text)}
.fly-rec-wave{display:inline-flex;align-items:flex-end;gap:3px}
.fly-rec-wave--sm{height:14px}
.fly-rec-wave--md{height:18px}
.fly-rec-wave span{display:block;width:3px;border-radius:var(--radius-pill);background:var(--primary);
  transform:scaleY(.32);transform-origin:bottom;height:100%}
.fly-rec-wave--animate span{animation:fly-wave .9s ease-in-out infinite}

/* CitationChip */
.fly-cite{display:inline-flex;align-items:center;justify-content:center;gap:3px;
  font-family:var(--font-mono);font-weight:var(--fw-semibold);font-size:10px;line-height:1;
  min-width:17px;height:17px;padding:0 5px;border-radius:var(--radius-xs);
  background:var(--primary-soft);color:var(--primary-soft-text);border:1px solid transparent;cursor:pointer;
  transition:filter var(--dur-fast) var(--ease-out),box-shadow var(--dur-fast) var(--ease-out)}
.fly-cite:hover{filter:brightness(.97);box-shadow:inset 0 0 0 1px var(--primary-border)}
.fly-cite:focus-visible{outline:none;box-shadow:var(--focus-ring)}
.fly-cite--static{cursor:default}
.fly-cite--static:hover{filter:none;box-shadow:none}

/* Modal + backward-compat ModalShell */
.fly-modal-overlay{position:fixed;inset:0;z-index:60;display:flex;align-items:center;justify-content:center;
  padding:24px;background:var(--overlay);animation:fly-fade-in var(--dur) var(--ease-out)}
.fly-modal{display:flex;flex-direction:column;max-height:100%;width:100%;
  background:var(--surface);color:var(--text);border:1px solid var(--line);
  border-radius:var(--radius-2xl);box-shadow:var(--shadow-lg);overflow:hidden;
  animation:fly-fade-up var(--dur-slow) var(--ease-out)}
.fly-modal-head{display:flex;align-items:center;justify-content:space-between;gap:12px;
  padding:20px 24px 16px;border-bottom:1px solid var(--line)}
.fly-modal-title{margin:0;font-family:var(--font-display);font-weight:var(--fw-bold);
  font-size:var(--text-title);letter-spacing:var(--tracking-title);color:var(--text)}
.fly-modal-body{padding:20px 24px;overflow-y:auto}
.fly-modal-foot{display:flex;align-items:center;justify-content:flex-end;gap:8px;padding:16px 24px;border-top:1px solid var(--line)}
.fly-modal-x{display:inline-flex;align-items:center;justify-content:center;width:30px;height:30px;
  border:none;background:transparent;color:var(--text-3);border-radius:var(--radius-sm);cursor:pointer;
  transition:background-color var(--dur-fast) var(--ease-out),color var(--dur-fast) var(--ease-out)}
.fly-modal-x:hover{background:var(--surface-3);color:var(--text)}
.fly-modal-x:focus-visible{outline:none;box-shadow:var(--focus-ring)}
`;

if (typeof document !== "undefined" && !document.getElementById("fly-ds-css")) {
  const el = document.createElement("style");
  el.id = "fly-ds-css";
  el.textContent = CSS;
  document.head.appendChild(el);
}

const cx = (...parts: (string | false | null | undefined)[]) => parts.filter(Boolean).join(" ");

/* ------------------------------------------------------------------ core */

const BTN_VARIANTS = ["primary", "soft", "outline", "ghost", "contrast", "record"] as const;
const BTN_SIZES = ["xs", "sm", "md", "lg"] as const;
export type ButtonVariant = (typeof BTN_VARIANTS)[number];
export type ButtonSize = (typeof BTN_SIZES)[number];

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  startIcon?: ReactNode;
  endIcon?: ReactNode;
}

/** The one interactive primitive. Six variants, four sizes. */
export function Button({
  variant = "outline",
  size = "sm",
  startIcon = null,
  endIcon = null,
  className = "",
  children,
  ...rest
}: ButtonProps) {
  const v = BTN_VARIANTS.includes(variant) ? variant : "outline";
  const s = BTN_SIZES.includes(size) ? size : "sm";
  return (
    <button className={cx("fly-btn", `fly-btn--${v}`, `fly-btn--${s}`, className)} {...rest}>
      {startIcon}
      {children}
      {endIcon}
    </button>
  );
}

interface SectionLabelProps extends HTMLAttributes<HTMLElement> {
  as?: React.ElementType;
}

/** Tiny uppercase micro-heading for control groups / panel regions. */
export function SectionLabel({
  as: Tag = "p",
  className = "",
  style,
  children,
  ...rest
}: SectionLabelProps) {
  return (
    <Tag
      className={className}
      style={{
        margin: 0,
        fontFamily: "var(--font-sans)",
        fontSize: "var(--text-label)",
        fontWeight: 600,
        lineHeight: 1,
        textTransform: "uppercase",
        letterSpacing: "var(--tracking-label)",
        color: "var(--text-3)",
        ...style,
      }}
      {...rest}
    >
      {children}
    </Tag>
  );
}

const CARD_PADS = { none: "0", sm: "12px", md: "16px", lg: "20px" } as const;
const CARD_RADII = {
  md: "var(--radius-xl)",
  lg: "var(--radius-2xl)",
  xl: "var(--radius-3xl)",
} as const;

interface CardProps extends HTMLAttributes<HTMLDivElement> {
  tone?: "surface" | "muted" | "invert";
  pad?: keyof typeof CARD_PADS;
  radius?: keyof typeof CARD_RADII;
  interactive?: boolean;
}

/** Padded surface container — hairline border + faint shadow (flat by design). */
export function Card({
  tone = "surface",
  pad = "md",
  radius = "lg",
  interactive = false,
  className = "",
  style,
  children,
  ...rest
}: CardProps) {
  const tones = {
    surface: {
      background: "var(--surface)",
      color: "var(--text)",
      border: "1px solid var(--line)",
    },
    muted: {
      background: "var(--surface-2)",
      color: "var(--text)",
      border: "1px solid var(--line-2)",
    },
    invert: { background: "var(--text)", color: "var(--surface)", border: "1px solid transparent" },
  } as const;
  const t = tones[tone] || tones.surface;
  return (
    <div
      className={className}
      style={{
        ...t,
        borderRadius: CARD_RADII[radius] || CARD_RADII.lg,
        padding: CARD_PADS[pad] ?? CARD_PADS.md,
        boxShadow: tone === "invert" ? "var(--shadow-md)" : "var(--shadow-sm)",
        transition: interactive
          ? "box-shadow var(--dur) var(--ease-out), border-color var(--dur) var(--ease-out), transform var(--dur) var(--ease-out)"
          : undefined,
        cursor: interactive ? "pointer" : undefined,
        ...style,
      }}
      {...rest}
    >
      {children}
    </div>
  );
}

/* ------------------------------------------------------------------ forms */

interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  label?: ReactNode;
  hint?: ReactNode;
  error?: ReactNode;
  invalid?: boolean;
}

/** Single-line text field with optional label, hint and error. */
export function Input({
  label,
  hint,
  error,
  invalid = false,
  id,
  className = "",
  style,
  ...rest
}: InputProps) {
  const autoId = useId();
  const fieldId = id || (label ? autoId : undefined);
  const isBad = invalid || !!error;
  const input = (
    <input
      id={fieldId}
      className={cx("fly-input", isBad && "fly-input--invalid", className)}
      aria-invalid={isBad || undefined}
      style={style}
      {...rest}
    />
  );
  if (!label && !hint && !error) return input;
  return (
    <span className="fly-field-wrap">
      {label && (
        <label className="fly-field-label" htmlFor={fieldId}>
          {label}
        </label>
      )}
      {input}
      {(error || hint) && (
        <span className={cx("fly-field-msg", error ? "fly-field-msg--err" : null)}>
          {error || hint}
        </span>
      )}
    </span>
  );
}

interface SelectOption {
  value: string;
  label: string;
}
interface SelectProps extends SelectHTMLAttributes<HTMLSelectElement> {
  options?: SelectOption[];
}

/** Native dropdown styled to match Input, with a custom chevron. */
export function Select({ options, className = "", style, children, ...rest }: SelectProps) {
  return (
    <span className="fly-select-wrap" style={style}>
      <select className={cx("fly-select", className)} {...rest}>
        {options
          ? options.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))
          : children}
      </select>
      <span className="fly-select-chevron" aria-hidden="true">
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="m6 9 6 6 6-6" />
        </svg>
      </span>
    </span>
  );
}

interface CheckboxProps extends InputHTMLAttributes<HTMLInputElement> {
  label?: ReactNode;
  description?: ReactNode;
}

/** Labelled checkbox (or radio via type="radio") with a violet indicator. */
export function Checkbox({
  type = "checkbox",
  label,
  description,
  disabled = false,
  className = "",
  style,
  ...rest
}: CheckboxProps) {
  const isRadio = type === "radio";
  return (
    <label className={cx("fly-check", disabled && "fly-check--disabled", className)} style={style}>
      <input type={type} disabled={disabled} {...rest} />
      <span
        className={cx("fly-check-box", `fly-check-box--${isRadio ? "radio" : "checkbox"}`)}
        aria-hidden="true"
      >
        {isRadio ? (
          <span className="fly-radio-dot" />
        ) : (
          <svg
            width="12"
            height="12"
            viewBox="0 0 24 24"
            fill="none"
            stroke="var(--on-primary)"
            strokeWidth="3.2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M20 6 9 17l-5-5" />
          </svg>
        )}
      </span>
      {(label || description) && (
        <span className="fly-check-body">
          {label && <span>{label}</span>}
          {description && <span className="fly-check-desc">{description}</span>}
        </span>
      )}
    </label>
  );
}

/* ------------------------------------------------------------------ feedback */

interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  tone?: "neutral" | "primary" | "success" | "warning" | "danger" | "accent" | "live";
  size?: "sm" | "md";
  uppercase?: boolean;
  dot?: boolean;
}

/** Small status pill. tone="live" is the pulsing red LIVE marker. */
export function Badge({
  tone = "neutral",
  size = "sm",
  uppercase = false,
  dot = false,
  className = "",
  children,
  ...rest
}: BadgeProps) {
  const showDot = dot || tone === "live";
  return (
    <span
      className={cx(
        "fly-badge",
        `fly-badge--${tone}`,
        `fly-badge--${size}`,
        uppercase && "fly-badge--upper",
        className,
      )}
      {...rest}
    >
      {showDot && <span className="fly-badge-dot" aria-hidden="true" />}
      {children}
    </span>
  );
}

interface ProgressBarProps extends HTMLAttributes<HTMLSpanElement> {
  value?: number | null;
  size?: "sm" | "md";
}

/** Thin determinate/indeterminate bar. Pass value 0–100, or omit for the sweep. */
export function ProgressBar({
  value = null,
  size = "sm",
  className = "",
  style,
  ...rest
}: ProgressBarProps) {
  const indeterminate = value == null;
  const pct = indeterminate ? 0 : Math.max(0, Math.min(100, value));
  return (
    <span
      className={cx("fly-progress", `fly-progress--${size}`, className)}
      role="progressbar"
      aria-valuenow={indeterminate ? undefined : pct}
      aria-valuemin={0}
      aria-valuemax={100}
      style={style}
      {...rest}
    >
      <span
        className={cx("fly-progress-bar", indeterminate && "fly-progress-bar--indeterminate")}
        style={indeterminate ? undefined : { width: `${pct}%` }}
      />
    </span>
  );
}

const REC_DELAYS = ["0s", ".15s", ".3s", ".45s", ".6s"];

function fmtElapsedMs(ms?: number): string {
  const s = Math.floor((ms || 0) / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}

interface RecordingIndicatorProps extends HTMLAttributes<HTMLSpanElement> {
  state?: "recording" | "paused";
  size?: "sm" | "md";
  label?: ReactNode;
  elapsedMs?: number | null;
  showWave?: boolean;
  showTimer?: boolean;
}

/** Signature capture marker: pulsing dot + live waveform + timer. */
export function RecordingIndicator({
  state = "recording",
  size = "md",
  label,
  elapsedMs,
  showWave = true,
  showTimer = true,
  className = "",
  ...rest
}: RecordingIndicatorProps) {
  const recording = state === "recording";
  return (
    <span className={cx("fly-rec", className)} role="status" {...rest}>
      <span
        className={cx(
          "fly-rec-dot",
          `fly-rec-dot--${recording ? "rec" : "paused"}`,
          `fly-rec-dot--${size}`,
        )}
        aria-hidden="true"
      />
      {label && <span className="fly-rec-label">{label}</span>}
      {showTimer && elapsedMs != null && (
        <span className="fly-rec-time">{fmtElapsedMs(elapsedMs)}</span>
      )}
      {showWave && (
        <span
          className={cx(
            "fly-rec-wave",
            `fly-rec-wave--${size}`,
            recording && "fly-rec-wave--animate",
          )}
          aria-hidden="true"
        >
          {REC_DELAYS.map((d) => (
            <span key={d} style={{ animationDelay: d }} />
          ))}
        </span>
      )}
    </span>
  );
}

/* ------------------------------------------------------------------ notes */

const SPK_ROTATION = [
  "var(--spk-teal)",
  "var(--spk-blue)",
  "var(--spk-amber)",
  "var(--spk-pink)",
  "var(--spk-green)",
  "var(--spk-indigo)",
];
const AVATAR_SIZES = { xs: 20, sm: 26, md: 34, lg: 44 } as const;
const AVATAR_FONT = { xs: 9, sm: 11, md: 13, lg: 16 } as const;

function hashIndex(key: string): number {
  let h = 0;
  for (let i = 0; i < String(key).length; i++) h = (h * 31 + String(key).charCodeAt(i)) | 0;
  return Math.abs(h);
}

export function speakerInitials(label = ""): string {
  const words = String(label).trim().split(/\s+/).filter(Boolean);
  if (words.length === 0) return "?";
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[words.length - 1][0]).toUpperCase();
}

interface AvatarProps extends HTMLAttributes<HTMLSpanElement> {
  name?: string;
  self?: boolean;
  index?: number;
  colorKey?: string;
  color?: string;
  shape?: "circle" | "square";
  size?: keyof typeof AVATAR_SIZES;
}

/** Identity chip (initials on a rotation color). Mic/self = brand violet. */
export function Avatar({
  name = "",
  self = false,
  index,
  colorKey,
  color,
  shape = "square",
  size = "md",
  className = "",
  style,
  ...rest
}: AvatarProps) {
  const px = AVATAR_SIZES[size] || AVATAR_SIZES.md;
  let bg = color;
  if (!bg) {
    if (self) bg = "var(--spk-self)";
    else {
      const i = index != null ? index : colorKey != null ? hashIndex(colorKey) : hashIndex(name);
      bg = SPK_ROTATION[((i % SPK_ROTATION.length) + SPK_ROTATION.length) % SPK_ROTATION.length];
    }
  }
  return (
    <span
      className={className}
      title={name || undefined}
      style={{
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        flex: "none",
        width: px,
        height: px,
        borderRadius: shape === "circle" ? "50%" : "calc(var(--radius-md) - 1px)",
        background: bg,
        color: "var(--on-speaker)",
        fontFamily: "var(--font-sans)",
        fontWeight: 600,
        fontSize: AVATAR_FONT[size] || AVATAR_FONT.md,
        lineHeight: 1,
        userSelect: "none",
        ...style,
      }}
      {...rest}
    >
      {speakerInitials(name)}
    </span>
  );
}

interface CitationChipProps extends Omit<HTMLAttributes<HTMLSpanElement>, "onClick"> {
  count?: number;
  label?: ReactNode;
  onClick?: (e: React.MouseEvent | React.KeyboardEvent) => void;
}

/** Provenance marker linking an AI line to its transcript source. */
export function CitationChip({
  count,
  label,
  onClick,
  className = "",
  children,
  ...rest
}: CitationChipProps) {
  const isButton = typeof onClick === "function";
  const text =
    children ?? label ?? (count != null ? `${count} source${count === 1 ? "" : "s"}` : "•");
  return (
    <span
      className={cx("fly-cite", !isButton && "fly-cite--static", className)}
      role={isButton ? "button" : undefined}
      tabIndex={isButton ? 0 : undefined}
      onClick={onClick}
      onKeyDown={
        isButton
          ? (e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onClick!(e);
              }
            }
          : undefined
      }
      title="Show the transcript this came from"
      {...rest}
    >
      {text}
    </span>
  );
}

interface ModalProps extends Omit<HTMLAttributes<HTMLDivElement>, "title"> {
  open?: boolean;
  onClose?: () => void;
  title?: ReactNode;
  footer?: ReactNode;
  width?: number | string;
  closeOnOverlay?: boolean;
  children?: ReactNode;
}

/** Overlay + card shell for dialogs (Settings, consent). Esc / click-outside close. */
export function Modal({
  open = true,
  onClose,
  title,
  footer,
  width = 560,
  closeOnOverlay = true,
  className = "",
  children,
  ...rest
}: ModalProps) {
  useEffect(() => {
    if (!open) return undefined;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose && onClose();
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;
  return (
    <div className="fly-modal-overlay" onClick={closeOnOverlay ? onClose : undefined}>
      <div
        className={cx("fly-modal", className)}
        role="dialog"
        aria-modal="true"
        style={{ maxWidth: `min(${typeof width === "number" ? `${width}px` : width}, 92vw)` }}
        onClick={(e) => e.stopPropagation()}
        {...rest}
      >
        {(title || onClose) && (
          <div className="fly-modal-head">
            {title ? <h2 className="fly-modal-title">{title}</h2> : <span />}
            {onClose && (
              <button className="fly-modal-x" onClick={onClose} aria-label="Close">
                <svg
                  width="16"
                  height="16"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2.2"
                  strokeLinecap="round"
                >
                  <path d="M18 6 6 18M6 6l12 12" />
                </svg>
              </button>
            )}
          </div>
        )}
        <div className="fly-modal-body">{children}</div>
        {footer && <div className="fly-modal-foot">{footer}</div>}
      </div>
    </div>
  );
}

/* ============================================================
   BACKWARD-COMPAT SHIMS — for screens not yet restyled.
   Remove once every screen adopts the new API above (next session).
   ============================================================ */

type LegacyVariant = ButtonVariant | "dark";

interface BtnProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, never> {
  variant?: LegacyVariant;
  size?: ButtonSize;
  startIcon?: ReactNode;
  endIcon?: ReactNode;
}

/** @deprecated Use `Button`. Kept so un-restyled screens keep working
 *  (maps the old `dark` variant to `contrast`). */
export function Btn({ variant = "outline", size = "sm", ...rest }: BtnProps) {
  const mapped: ButtonVariant = variant === "dark" ? "contrast" : variant;
  return <Button variant={mapped} size={size} {...rest} />;
}

/** @deprecated Use `Modal`. Drop-in for the old shell — width still comes
 *  from the caller's className; only the scrim is now theme-correct. */
export function ModalShell({
  children,
  className = "",
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-6"
      style={{ background: "var(--overlay)" }}
    >
      <div
        className={cx(
          "max-h-full overflow-y-auto rounded-2xl border border-line bg-surface shadow-warm",
          className,
        )}
        role="dialog"
        aria-modal="true"
      >
        {children}
      </div>
    </div>
  );
}

/** @deprecated Speaker fill: mic/self = brand violet; others rotate the speaker palette. */
export function speakerColor(speakerKey: string, index: number): string {
  if (speakerKey === "mic" || speakerKey === "you" || speakerKey === "self")
    return "var(--spk-self)";
  return SPK_ROTATION[((index % SPK_ROTATION.length) + SPK_ROTATION.length) % SPK_ROTATION.length];
}
