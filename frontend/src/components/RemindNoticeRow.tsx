import { useEffect, useState } from "react";
import { api } from "../api";
import { REMIND_NOTICE_KEY, remindNoticeOn } from "../recordingBar";
import { SettingsRow, Switch } from "./ui";

/** Settings › Recording › "Remind me to tell people" (on unless turned off).
 *  The recording bar reads the same key when each recording starts. Saved at
 *  once, like the other Recording rows. */
export default function RemindNoticeRow() {
  const [on, setOn] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api
      .getAppSetting(REMIND_NOTICE_KEY)
      .then((v) => {
        if (live) setOn(remindNoticeOn(v));
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);

  const change = (next: boolean) => {
    setOn(next);
    setError(null);
    api.setAppSetting(REMIND_NOTICE_KEY, String(next)).catch((e) => {
      setOn(!next);
      setError(String(e));
    });
  };

  return (
    <SettingsRow
      title="Remind me to tell people"
      helper={
        error ?? "When you start recording, a reminder offers the notice to paste in the chat."
      }
      helperTone={error ? "error" : "default"}
      control={<Switch label="Remind me to tell people" checked={on} onChange={change} />}
    />
  );
}
