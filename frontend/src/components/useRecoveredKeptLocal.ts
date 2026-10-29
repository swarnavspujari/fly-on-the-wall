import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import type { RecoveredMeeting } from "../types";

/** Event the backend sends after self-heal (privacy.rs
 *  RECOVERED_KEPT_LOCAL_EVENT), with the whole current list. */
export const RECOVERED_KEPT_LOCAL_EVENT = "privacy:recovered-kept-local";

/** The notice's words. Review opens the meetings one by one. */
export function recoveredKeptLocalCopy(titles: readonly string[]): {
  title: string;
  cause: string;
  action: string;
} {
  const one = titles.length === 1;
  return {
    title: one
      ? "A recovered meeting is set to Keep local"
      : "Recovered meetings are set to Keep local",
    cause:
      (one
        ? `"${titles[0]}" was set to Keep local because its privacy setting`
        : `${titles.length} recovered meetings were set to Keep local because their privacy setting`) +
      " couldn't be confirmed. Review and turn Keep local off where it isn't needed.",
    action: one ? "Review" : "Review the first",
  };
}

export interface RecoveredKeptLocal {
  /** Meetings restored this launch and set to Keep local because their
   *  privacy setting couldn't be confirmed, oldest first. */
  meetings: RecoveredMeeting[];
  /** Take the first one off the list and return it (to open it). */
  reviewNext: () => Promise<RecoveredMeeting | null>;
  /** Take them all off the list. Keep local itself doesn't change. */
  dismiss: () => void;
}

/** Backs the "recovered meetings were set to Keep local" notice: catches up
 *  with `recovered_kept_local` at mount, then follows the event. */
export function useRecoveredKeptLocal(): RecoveredKeptLocal {
  const [meetings, setMeetings] = useState<RecoveredMeeting[]>([]);

  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | null = null;
    api
      .recoveredKeptLocal()
      .then((list) => {
        if (live) setMeetings(list);
      })
      .catch(() => {});
    listen<RecoveredMeeting[]>(RECOVERED_KEPT_LOCAL_EVENT, (e) => setMeetings(e.payload))
      .then((un) => {
        if (live) unlisten = un;
        else un();
      })
      .catch(() => {});
    return () => {
      live = false;
      unlisten?.();
    };
  }, []);

  const reviewNext = async () => {
    const first = meetings[0] ?? null;
    if (!first) return null;
    try {
      setMeetings(await api.dismissRecoveredKeptLocal(first.meeting_id));
    } catch {
      setMeetings((list) => list.slice(1));
    }
    return first;
  };

  const dismiss = () => {
    setMeetings([]);
    void api.dismissRecoveredKeptLocal(null).catch(() => {});
  };

  return { meetings, reviewNext, dismiss };
}
