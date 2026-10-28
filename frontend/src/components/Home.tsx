import { useEffect, useState } from "react";
import { Mic, TriangleAlert } from "lucide-react";
import {
  CALENDAR_NAMES,
  eventGuests,
  greeting,
  nextVideoEvent,
  upNextState,
  upNextWhen,
} from "../homeModel";
import type { CalendarEvent, CalendarStatus } from "../types";
import { Avatar, Badge, Button, SectionLabel } from "./ui";

type Provider = "google" | "msgraph";

interface Props {
  /** Audio is recording: the record buttons give way to a status. */
  recording: boolean;
  upcoming: CalendarEvent[];
  /** null until the first calendar-status fetch. */
  calendar: CalendarStatus | null;
  needsReconnect: Provider[];
  /** Provider whose sign-in window is open. */
  calendarBusy: string | null;
  /** false only when the user has no notes at all (the empty state). */
  hasNotes: boolean | null;
  onRecord: () => void;
  onStartFromEvent: (ev: CalendarEvent) => void;
  onConnectCalendar: (provider: Provider) => void;
  onOpenSettings: () => void;
  onImport: () => void;
  onNewNote: () => void;
}

const MAX_AVATARS = 4;

/** Home: what shows while no note is open. An Up next card (the next video
 *  meeting, or a prompt to record or connect a calendar), any expired
 *  calendar, and, with no notes yet, the ways in. "Bring notes from another
 *  computer" is left out until there's an import for it (spec §3.2.3). */
export default function Home({
  recording,
  upcoming,
  calendar,
  needsReconnect,
  calendarBusy,
  hasNotes,
  onRecord,
  onStartFromEvent,
  onConnectCalendar,
  onOpenSettings,
  onImport,
  onNewNote,
}: Props) {
  // Clock for "In 4 min" and the greeting, refreshed each 30 s (render
  // stays pure).
  const [now, setNow] = useState<number | null>(null);
  useEffect(() => {
    setNow(Date.now());
    const t = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(t);
  }, []);

  const connected = calendar ? calendar.google_connected || calendar.ms_connected : null;
  const next = now == null ? null : nextVideoEvent(upcoming, now);
  const state = upNextState(connected, needsReconnect, next);
  const connectable: Provider[] = calendar
    ? (["google", "msgraph"] as const).filter((p) =>
        p === "google" ? calendar.google_configured : calendar.ms_configured,
      )
    : [];

  const recordButton = recording ? (
    <Badge tone="live" size="md">
      Recording
    </Badge>
  ) : (
    <Button variant="record" size="lg" onClick={onRecord}>
      Record
    </Button>
  );

  return (
    <div className="flex flex-1 flex-col overflow-y-auto bg-surface">
      <div className="mx-auto flex w-full max-w-[720px] flex-col gap-5 px-10 pb-24 pt-10">
        {now != null && (
          <header className="flex flex-col gap-1">
            <h1 className="m-0 font-display text-[30px] font-extrabold tracking-[-0.02em] text-text">
              {greeting(new Date(now))}
            </h1>
            <span className="text-[14px] text-text-3">
              {new Date(now).toLocaleDateString(undefined, {
                weekday: "long",
                month: "long",
                day: "numeric",
              })}
            </span>
          </header>
        )}

        {needsReconnect.map((p) => (
          <div
            key={p}
            role="status"
            className="flex flex-wrap items-center gap-3 rounded-[14px] border border-line bg-warning-soft px-4 py-3"
          >
            <TriangleAlert size={16} strokeWidth={1.75} className="flex-none text-warning-text" />
            <span className="min-w-0 flex-1 text-[13.5px] text-text">
              <b className="font-semibold">{CALENDAR_NAMES[p]} disconnected.</b> Sign in again to
              keep Up next current.
            </span>
            <Button
              variant="outline"
              size="sm"
              disabled={calendarBusy !== null}
              onClick={() => onConnectCalendar(p)}
            >
              {calendarBusy === p ? "Waiting for browser…" : "Reconnect"}
            </Button>
          </div>
        ))}

        <section
          aria-label="Up next"
          className="flex flex-wrap items-center gap-5 rounded-[18px] border border-line bg-surface px-6 py-[22px]"
          style={{ boxShadow: "var(--shadow-sm)" }}
        >
          {state === "event" && next ? (
            <>
              <div className="flex min-w-[220px] flex-1 flex-col gap-2">
                <SectionLabel>{now != null ? upNextWhen(next, now) : "Up next"}</SectionLabel>
                <span className="font-display text-[22px] font-bold tracking-[-0.01em] text-text">
                  {next.title}
                </span>
                <GuestRow guests={eventGuests(next)} />
              </div>
              {recording ? (
                recordButton
              ) : (
                <Button variant="record" size="lg" onClick={() => onStartFromEvent(next)}>
                  Start recording
                </Button>
              )}
            </>
          ) : (
            <>
              <div className="flex min-w-[220px] flex-1 flex-col gap-1.5">
                <span className="font-display text-[22px] font-bold tracking-[-0.01em] text-text">
                  Ready when your call starts
                </span>
                {state === "no-calendar" ? (
                  <>
                    <span className="text-[13.5px] text-text-2">
                      Press Record when you join. Connect a calendar to see your meetings here.
                    </span>
                    <span className="mt-1 flex flex-wrap gap-2">
                      {connectable.length > 0 ? (
                        connectable.map((p) => (
                          <Button
                            key={p}
                            variant="outline"
                            size="sm"
                            disabled={calendarBusy !== null}
                            onClick={() => onConnectCalendar(p)}
                          >
                            {calendarBusy === p
                              ? "Waiting for browser…"
                              : `Connect ${CALENDAR_NAMES[p]}`}
                          </Button>
                        ))
                      ) : (
                        <Button variant="outline" size="sm" onClick={onOpenSettings}>
                          Set up a calendar in Settings
                        </Button>
                      )}
                    </span>
                  </>
                ) : state === "expired" ? (
                  <span className="text-[13.5px] text-text-2">
                    Press Record when you join. Your meetings show here again once you reconnect.
                  </span>
                ) : (
                  <span className="text-[13.5px] text-text-2">
                    No video meetings for the rest of today. Up next shows events with a join link.
                  </span>
                )}
              </div>
              {recordButton}
            </>
          )}
        </section>

        {hasNotes === false && (
          <section
            aria-label="Get started"
            className="flex flex-col items-center gap-3 rounded-[18px] border border-dashed border-line-strong px-10 py-10 text-center"
          >
            <span className="grid h-12 w-12 place-items-center rounded-full bg-primary-soft text-primary-soft-text">
              <Mic size={22} strokeWidth={1.75} />
            </span>
            <span className="font-display text-[20px] font-bold text-text">No meetings yet</span>
            <span className="text-[14px] text-text-2">
              Record a call, import one you already have, or start with a blank note.
            </span>
            <div className="flex flex-wrap justify-center gap-2">
              <Button variant="outline" size="md" onClick={onImport}>
                Import recordings
              </Button>
              <Button variant="ghost" size="md" onClick={onNewNote}>
                New note
              </Button>
            </div>
          </section>
        )}
      </div>
    </div>
  );
}

/** Invitees as avatars plus a count ("3 people" includes you). */
function GuestRow({ guests }: { guests: string[] }) {
  if (guests.length === 0) return null;
  const people = guests.length + 1;
  return (
    <div className="flex items-center gap-2">
      <Avatar name="You" self shape="circle" size="sm" />
      {guests.slice(0, MAX_AVATARS).map((g, i) => (
        <Avatar key={`${g}-${i}`} name={g} index={i + 1} shape="circle" size="sm" />
      ))}
      {guests.length > MAX_AVATARS && (
        <span className="text-[12px] font-semibold text-text-3">
          +{guests.length - MAX_AVATARS}
        </span>
      )}
      <span className="text-[12.5px] text-text-3">{people} people</span>
    </div>
  );
}
