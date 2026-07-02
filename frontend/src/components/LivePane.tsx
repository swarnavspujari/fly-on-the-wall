import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { SectionLabel } from "./ui";
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
 *  attribution only — the full diarized transcript replaces this after Stop. */
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
    return () => {
      void unSeg.then((f) => f());
      void unStatus.then((f) => f());
    };
  }, [meetingId]);

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, [segments]);

  return (
    <div className="flex max-h-72 flex-col border-b border-line bg-cream print:hidden">
      <div className="flex items-center gap-2 px-6 pb-1 pt-3">
        <SectionLabel>Live transcript</SectionLabel>
        <span className="rounded bg-peach px-1.5 py-0.5 text-[9.5px] font-semibold uppercase tracking-wide text-clay">
          beta
        </span>
        <span className="text-[11.5px] text-mute">
          {status?.state === "unavailable"
            ? status.detail
            : "rough, channel-level — the full transcript arrives after Stop"}
        </span>
      </div>
      <div ref={scrollRef} className="flex-1 overflow-y-auto px-6 pb-3 pt-1">
        {segments.length === 0 && status?.state !== "unavailable" && (
          <div className="flex items-center gap-2 py-2 text-[13px] text-clay">
            <span
              className="h-2 w-2 rounded-full bg-coral"
              style={{ animation: "pulse-dot 1s ease infinite" }}
            />
            Listening — first passage lands after ~15 s of speech.
          </div>
        )}
        {segments.map((s, i) => (
          <div
            key={`${s.start_ms}-${i}`}
            className={`mb-2 flex ${s.channel === "you" ? "justify-end" : "justify-start"}`}
            style={{ animation: "fade-up .3s ease both" }}
          >
            <div className="max-w-[86%]">
              <div
                className={`mb-0.5 flex items-center gap-1.5 text-[11px] font-semibold ${
                  s.channel === "you" ? "justify-end text-coral" : "text-spk-teal"
                }`}
              >
                {s.channel === "you" ? "You" : "Them"}
                <span className="font-mono text-[10px] font-normal text-mute">
                  {fmtElapsed(s.start_ms)}
                </span>
              </div>
              <div
                className={`rounded-xl border border-line px-3 py-1.5 text-[13.5px] leading-normal text-ink ${
                  s.channel === "you" ? "bg-peach" : "bg-surface"
                }`}
              >
                {s.text}
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
