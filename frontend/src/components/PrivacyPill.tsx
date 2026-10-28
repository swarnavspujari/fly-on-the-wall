import { useId, useState } from "react";
import { Cloud, Lock } from "lucide-react";
import type { PrivacySummary } from "../homeModel";
import { useEscapeLayer } from "./ui";

interface Props {
  summary: PrivacySummary;
  /** Opens Settings at the top ("Where your data goes"). */
  onOpen: () => void;
}

/** Sidebar footer pill: "On this computer" or "Cloud on · <provider>".
 *  Hover or focus lists what uses the cloud and where it goes, plus the
 *  Keep-local exception; click opens Settings. */
export default function PrivacyPill({ summary, onOpen }: Props) {
  const [open, setOpen] = useState(false);
  const tipId = useId();
  useEscapeLayer(() => setOpen(false), open);
  const off = summary.offDevice;

  return (
    <div
      className="relative"
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
    >
      <button
        type="button"
        onClick={onOpen}
        onFocus={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        aria-describedby={tipId}
        className={`flex w-full cursor-pointer items-center gap-2 rounded-[10px] border border-line bg-surface px-2.5 py-2 text-left text-[12px] font-semibold hover:border-line-strong focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)] ${
          off ? "text-warning-text" : "text-success-text"
        }`}
      >
        {off ? (
          <Cloud size={14} strokeWidth={1.75} className="flex-none" />
        ) : (
          <Lock size={14} strokeWidth={1.75} className="flex-none" />
        )}
        <span className="min-w-0 flex-1 truncate">{summary.label}</span>
      </button>
      <div
        id={tipId}
        role="tooltip"
        hidden={!open}
        className="absolute bottom-full left-0 z-30 mb-2 w-[264px] rounded-xl border border-line bg-surface p-3 text-[12.5px] leading-[1.45] text-text-2"
        style={{ boxShadow: "var(--shadow-pop)" }}
      >
        {off ? (
          <>
            {summary.uses.map((u) => (
              <div key={u.where} className="mb-2">
                <div className="font-semibold text-text">Sent to {u.where}</div>
                <ul className="m-0 mt-0.5 list-disc pl-4">
                  {u.what.map((w) => (
                    <li key={w}>{w}</li>
                  ))}
                </ul>
              </div>
            ))}
            <div className="text-text-3">{summary.footnote}</div>
          </>
        ) : (
          <div>{summary.footnote}</div>
        )}
        <div className="mt-2 text-text-3">Change this in Settings.</div>
      </div>
    </div>
  );
}
