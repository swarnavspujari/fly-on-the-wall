import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { api } from "../api";
import { formatBytes } from "../privacy";
import {
  COMPRESS_EVENT,
  VIDEO_EVENT,
  audioOnlyConfirm,
  compressLine,
  importedVideosLine,
  screenLine,
  shrinkConfirm,
  videoJobLine,
} from "../storageModel";
import type { CompressStatus, StorageReport, VideoJobStatus } from "../types";
import { useConfirm } from "./confirm";
import { Button, Segmented, SettingsRow } from "./ui";

const SIZE = "text-text-2";

/** Settings › Your files and privacy: how much space recordings and videos
 *  take, the background lossless compression's progress (live from
 *  storage:compress), and the two opt-in video choices (storage:videos):
 *  imported videos can keep only their audio, screen recordings can be
 *  shrunk with a visually lossless re-encode. Both ask first. */
export default function StorageRows() {
  const confirm = useConfirm();
  const [report, setReport] = useState<StorageReport | null>(null);
  const [estimating, setEstimating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    const unlisten: (() => void)[] = [];
    const refresh = () =>
      api
        .storageReport()
        .then((r) => {
          if (live) setReport(r);
        })
        .catch(() => {});
    void refresh();
    const sub = <T,>(
      event: string,
      apply: (r: StorageReport, p: T) => StorageReport,
      done: (p: T) => boolean,
    ) =>
      listen<T>(event, (e) => {
        setReport((r) => (r ? apply(r, e.payload) : r));
        // sizes changed once a job finishes; re-measure
        if (done(e.payload)) void refresh();
      })
        .then((un) => {
          if (live) unlisten.push(un);
          else un();
        })
        .catch(() => {});
    void sub<CompressStatus>(
      COMPRESS_EVENT,
      (r, compress) => ({ ...r, compress }),
      (p) => p.state === "done",
    );
    void sub<VideoJobStatus>(
      VIDEO_EVENT,
      (r, videos) => ({ ...r, videos }),
      (p) => p.state === "done" || p.state === "stopped" || p.files_done > 0,
    );
    return () => {
      live = false;
      unlisten.forEach((un) => un());
    };
  }, []);

  if (!report) return null;
  const v = report.videos;
  const running = v.state === "working" || v.state === "paused";
  const job = videoJobLine(v);
  const jobFor = (kind: VideoJobStatus["kind"]) => (v.kind === kind ? job : null);

  const chooseImportPolicy = async (policy: "keep" | "audio_only") => {
    setError(null);
    if (policy === "audio_only") {
      const copy = audioOnlyConfirm(report);
      const ok = await confirm({
        title: copy.title,
        message: copy.message,
        confirmLabel: "Keep audio only",
        tone: "danger",
      });
      if (!ok) return;
    }
    try {
      const videos = await api.setImportVideoPolicy(policy);
      setReport((r) => (r ? { ...r, videos } : r));
    } catch (e) {
      setError(String(e));
    }
  };

  const shrink = async () => {
    setError(null);
    setEstimating(true);
    let estimate;
    try {
      estimate = await api.estimateScreenShrink();
    } catch (e) {
      setError(String(e));
      return;
    } finally {
      setEstimating(false);
    }
    if (!estimate) return;
    const copy = shrinkConfirm(estimate);
    const ok = await confirm({
      title: copy.title,
      message: copy.message,
      confirmLabel: "Shrink recordings",
    });
    if (!ok) return;
    try {
      const videos = await api.startVideoJob("shrink_screen");
      setReport((r) => (r ? { ...r, videos } : r));
    } catch (e) {
      setError(String(e));
    }
  };

  const stop = () => void api.cancelVideoJob().catch((e) => setError(String(e)));
  const stopButton = (
    <Button variant="outline" size="sm" onClick={stop}>
      Stop
    </Button>
  );
  const detail = (line: string | null) =>
    line ? (
      <p role="status" className="text-text-2" style={{ fontSize: 12.5, margin: 0 }}>
        {line}
      </p>
    ) : null;

  return (
    <>
      <SettingsRow
        title="Recordings"
        helper={compressLine(report.compress)}
        control={
          <span className={SIZE} data-testid="storage-audio">
            {formatBytes(report.audio_bytes)}
          </span>
        }
      />
      <SettingsRow
        title="Imported videos"
        helper={importedVideosLine(report)}
        control={
          running && v.kind === "audio_only" ? (
            stopButton
          ) : (
            <Segmented
              label="Imported videos"
              value={v.import_policy}
              disabled={running}
              onChange={(p) => void chooseImportPolicy(p)}
              options={[
                { value: "keep", label: "Keep video" },
                { value: "audio_only", label: "Audio only" },
              ]}
            />
          )
        }
      >
        {detail(jobFor("audio_only"))}
      </SettingsRow>
      <SettingsRow
        title="Screen recordings"
        helper={screenLine(report)}
        control={
          running && v.kind === "shrink_screen" ? (
            stopButton
          ) : (
            <Button
              variant="outline"
              size="sm"
              disabled={running || estimating || report.screen_recording_files === 0}
              onClick={() => void shrink()}
            >
              {estimating ? "Estimating…" : "Shrink…"}
            </Button>
          )
        }
      >
        {detail(jobFor("shrink_screen"))}
        {error && (
          <p role="alert" className="text-danger" style={{ fontSize: 12.5, margin: 0 }}>
            {error}
          </p>
        )}
      </SettingsRow>
    </>
  );
}
