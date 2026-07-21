import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import { Badge } from "./ui";
import { fmtElapsed } from "./RecordingBar";

interface LiveSegment {
  meeting_id: string;
  channel: "you" | "them";
  text: string;
  start_ms: number;
}

interface LiveStatus {
  meeting_id: string;
  state: "ready" | "unavailable";
  detail: string;
}

/** Live partial transcript while a recording runs (beta): channel-level
 *  attribution only — the full diarized transcript replaces this after Stop.
 *  Rendered inside the Transcript view as the design's "Listening" banner. */
export default function LivePane({ meetingId }: { meetingId: string }) {
  const [segments, setSegments] = useState<LiveSegment[]>([]);
  const [status, setStatus] = useState<LiveStatus | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    setSegments([]);
    setStatus(null);
    const unSeg = listen<LiveSegment>("live:segment", (e) => {
      if (e.payload.meeting_id !== meetingId) return;
      setSegments((prev) =>
        [...prev, e.payload].sort((a, b) => a.start_ms - b.start_ms).slice(-200),
      );
    });
    const unStatus = listen<LiveStatus>("live:status", (e) => {
      if (e.payload.meeting_id !== meetingId) return;
      setStatus(e.payload);
    });
    // Catch up on a status emitted before this pane mounted (the live loop
    // can fail within the first second of a recording, ahead of the poll
    // that mounts this pane) — events alone would leave "Listening…" up
    // forever. Late events still win via the listener above.
    api
      .liveStatus(meetingId)
      .then((s) => {
        if (s && s.meeting_id === meetingId) {
          setStatus((prev) => prev ?? s);
        }
      })
      .catch(() => {});
    return () => {
      void unSeg.then((f) => f());
      void unStatus.then((f) => f());
    };
  }, [meetingId]);

  useEffect(() => {
    scrollRef.current?.lastElementChild?.scrollIntoView({ behavior: "smooth", block: "nearest" });
  }, [segments]);

  return (
    <div className="print:hidden">
      {/* Listening banner — primary-soft with a small violet waveform + beta badge */}
      <div
        className="mb-4 flex items-center gap-3 rounded-lg border px-3.5 py-3"
        style={{ background: "var(--primary-soft)", borderColor: "var(--primary-border)" }}
      >
        <span className="inline-flex h-4 items-end gap-[3px]" aria-hidden="true">
          {[0, 1, 2, 3].map((i) => (
            <span
              key={i}
              className="w-[3px] rounded-full"
              style={{
                height: 16,
                background: "var(--primary)",
                transformOrigin: "bottom",
                transform: "scaleY(.32)",
                animation: `fly-wave .9s ease-in-out ${i * 0.12}s infinite`,
              }}
            />
          ))}
        </span>
        <span
          className="flex-1 text-[12.5px] font-semibold"
          style={{ color: "var(--primary-soft-text)" }}
        >
          {status?.state === "unavailable"
            ? status.detail
            : "Listening — live, rough transcript. Full diarized transcript arrives after Stop."}
        </span>
        <Badge tone="primary" uppercase>
          beta
        </Badge>
      </div>

      {/* Live segments — channel-level bubbles */}
      <div ref={scrollRef}>
        {segments.length === 0 && status?.state !== "unavailable" && (
          <div
            className="flex items-center gap-2 py-2 text-[13px]"
            style={{ color: "var(--primary-soft-text)" }}
          >
            <span
              className="h-2 w-2 rounded-full"
              style={{
                background: "var(--primary)",
                animation: "fly-pulse-dot 1.2s ease infinite",
              }}
            />
            Listening — first passage lands after ~15 s of speech.
          </div>
        )}
        {segments.map((s, i) => {
          const self = s.channel === "you";
          return (
            <div
              key={`${s.start_ms}-${i}`}
              className={`mb-3.5 flex ${self ? "justify-end" : "justify-start"}`}
              style={{ animation: "fly-fade-up .3s ease both" }}
            >
              <div className="min-w-0 max-w-[86%]">
                <div
                  className={`mb-1 flex items-center gap-1.5 text-[11px] font-semibold ${self ? "justify-end" : ""}`}
                  style={{ color: self ? "var(--spk-self)" : "var(--spk-teal)" }}
                >
                  {self ? "You" : "Them"}
                  <span
                    className="font-mono text-[10px] font-normal"
                    style={{ color: "var(--text-3)" }}
                  >
                    {fmtElapsed(s.start_ms)}
                  </span>
                </div>
                <div
                  className="rounded-xl border px-3 py-1.5 text-[13.5px] leading-normal"
                  style={{
                    borderColor: "var(--line)",
                    color: "var(--text)",
                    background: self ? "var(--primary-soft)" : "var(--surface-2)",
                  }}
                >
                  {s.text}
                </div>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
