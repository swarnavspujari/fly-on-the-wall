import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { api } from "../api";
import { formatBytes } from "../privacy";
import { COMPRESS_EVENT, compressLine, videoBytes, videoLine } from "../storageModel";
import type { CompressStatus, StorageReport } from "../types";
import { SettingsRow } from "./ui";

/** Settings › Your files and privacy: how much space recordings and videos
 *  take, and the background lossless compression's progress (live from the
 *  storage:compress event). Report only: no controls. */
export default function StorageRows() {
  const [report, setReport] = useState<StorageReport | null>(null);

  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | null = null;
    const refresh = () =>
      api
        .storageReport()
        .then((r) => {
          if (live) setReport(r);
        })
        .catch(() => {});
    void refresh();
    listen<CompressStatus>(COMPRESS_EVENT, (e) => {
      setReport((r) => (r ? { ...r, compress: e.payload } : r));
      // Sizes changed once a meeting finishes; re-measure at the end.
      if (e.payload.state === "done") void refresh();
    })
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

  if (!report) return null;
  return (
    <>
      <SettingsRow
        title="Recordings"
        helper={compressLine(report.compress)}
        control={
          <span className="text-text-2" data-testid="storage-audio">
            {formatBytes(report.audio_bytes)}
          </span>
        }
      />
      <SettingsRow
        title="Videos"
        helper={videoLine(report)}
        control={
          <span className="text-text-2" data-testid="storage-video">
            {formatBytes(videoBytes(report))}
          </span>
        }
      />
    </>
  );
}
