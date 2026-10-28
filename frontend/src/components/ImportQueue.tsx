import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { GripVertical, Mic, Monitor, X } from "lucide-react";
import type { ImportFile, ImportProgress, ImportStaged } from "../types";
import {
  fmtSize,
  mapProgress,
  parseTranscribingDetail,
  rollingStatus,
  type ImportFileStatus,
} from "../importProgress";
import { briefError } from "../pipelineNotice";
import { Button } from "./ui";

/** Shown in place of the notes editor while a note created from imported
 *  media hasn't been transcribed yet (design: ImportQueue in app-editor.jsx).
 *  Rows drag to reorder while idle; transcription runs top-to-bottom with a
 *  per-file status derived from the real pipeline events: `import:progress`
 *  marks the conversion of each file, and the global ASR % from
 *  `pipeline:progress` is mapped onto files via the concatenation boundaries. */

interface Props {
  staged: ImportStaged;
  pipeStage: string | null;
  pipeDetail: string | null;
  pipelineError: string | null;
  /** Start transcription with the queue's current order (file ids). */
  onTranscribe: (order: string[]) => void;
  /** Re-enqueue after a pipeline failure (the files are already staged). */
  onRetry: () => void;
  /** Stop the running transcription (finished batches stay checkpointed). */
  onCancel: () => void;
}

/** Header copy per running stage (import-flow wording, not the transcript
 *  panel's — the queue frames everything as processing the files above). */
const RUNNING_LABELS: Record<string, string> = {
  waiting: "Waiting to transcribe (recording comes first)…",
  starting: "Starting…",
  "ensuring-models": "Preparing models…",
  benchmarking: "Testing GPU vs CPU speed (one time)…",
  "preparing-audio": "Preparing audio…",
  transcribing: "Transcribing… files are processed top to bottom.",
  diarizing: "Detecting speakers…",
  aligning: "Assigning words to speakers…",
  saving: "Saving transcript…",
  polishing: "AI cleanup — polishing the transcript…",
};

export default function ImportQueue({
  staged,
  pipeStage,
  pipeDetail,
  pipelineError,
  onTranscribe,
  onRetry,
  onCancel,
}: Props) {
  const [files, setFiles] = useState<ImportFile[]>(staged.files);
  const [clicked, setClicked] = useState(false);
  // conversion markers from import:progress ("converting" until "converted")
  const [converting, setConverting] = useState<string | null>(null);
  // files whose conversion failed during this run (file id -> cause); the
  // rest of the batch carries on, and the returned queue keeps the causes
  const [failedLive, setFailedLive] = useState<Record<string, string>>({});
  const dragIdx = useRef<number | null>(null);

  // Re-sync the local (reorderable) list whenever the backend hands back a
  // fresh staged object (note switch, transcribe start, failed-start reset).
  useEffect(() => {
    setFiles(staged.files);
    setClicked(false);
    setFailedLive({});
  }, [staged]);

  useEffect(() => {
    const un = listen<ImportProgress>("import:progress", (e) => {
      const p = e.payload;
      if (p.meeting_id !== staged.meeting_id) return;
      setConverting(p.stage === "converting" ? p.file_id : null);
      if (p.stage === "failed") {
        const cause = p.error ?? "Its audio couldn't be converted.";
        setFailedLive((m) => ({ ...m, [p.file_id]: cause }));
      }
    });
    return () => {
      void un.then((f) => f());
    };
  }, [staged.meeting_id]);

  const running = staged.started || clicked;
  const idle = !running;
  // A failure belongs to a run in progress. An idle queue (after Cancel, or a
  // job left parked as failed) is edited and transcribed afresh, never
  // retried underneath a queue that looks idle.
  const runError = running ? pipelineError : null;
  // A conversion failure belongs to its file: while a run is going only this
  // run's failures count (an earlier cause is being retried); when idle the
  // queue shows the causes the last run left behind.
  const failure = (f: ImportFile): string | null =>
    running ? (failedLive[f.id] ?? null) : (f.convert_error ?? failedLive[f.id] ?? null);
  const supported = files.filter((f) => !f.error && !failure(f));
  const failed = files.filter((f) => !f.error && failure(f));

  // Transcribe goes ahead without files that failed to convert (the backend
  // drops them from the queue, as it does any file left out of the order).
  const start = () => {
    setClicked(true);
    setFiles((fs) => fs.filter((f) => f.error || !failure(f)));
    onTranscribe(supported.map((f) => f.id));
  };
  // Retry runs the whole queue again, failed files included: finished
  // conversions are reused, so only the failed ones are converted again.
  const retry = () => {
    setClicked(true);
    onTranscribe(files.filter((f) => !f.error).map((f) => f.id));
  };

  const removeFile = (id: string) => setFiles((fs) => fs.filter((f) => f.id !== id));

  const onDragOverRow = (i: number) => {
    const from = dragIdx.current;
    if (from == null || from === i) return;
    setFiles((fs) => {
      const a = fs.slice();
      const [m] = a.splice(from, 1);
      a.splice(i, 0, m);
      return a;
    });
    dragIdx.current = i;
  };

  // ---- derive per-file statuses from the real pipeline state ----
  const afterAsr =
    pipeStage != null && ["diarizing", "aligning", "saving", "polishing"].includes(pipeStage);
  let statuses: Record<string, ImportFileStatus> = {};
  let activePct: number | null = null;
  // a run that gave up has no per-file progress: the header says why
  if (running && !runError) {
    if (afterAsr) {
      for (const f of supported) statuses[f.id] = "done";
    } else if (pipeStage === "transcribing") {
      // Cloud runs now report % too; a detail without one (first instants)
      // maps as 0% — first file active, the rest waiting — never the
      // everything-is-spinning fallback that read as parallel work.
      const pct = parseTranscribingDetail(pipeDetail).pct ?? 0;
      ({ statuses, activePct } = mapProgress(supported, staged.boundaries_ms, pct));
    } else {
      for (const f of supported) statuses[f.id] = converting === f.id ? "preparing" : "waiting";
    }
  }

  const noneConverted =
    failed.length === 1
      ? "The file couldn't be converted."
      : failed.length === 2
        ? "Neither file could be converted."
        : `None of the ${failed.length} files could be converted.`;
  const failedNote =
    supported.length === 0
      ? `${noneConverted} See why below, then retry.`
      : `${failed.length} file${failed.length === 1 ? "" : "s"} couldn't be converted. Retry, or transcribe the other ${supported.length === 1 ? "file" : `${supported.length} files`} without ${failed.length === 1 ? "it" : "them"}.`;
  const subtitle = runError
    ? "Transcription didn't finish. Retry picks up where it stopped."
    : idle && failed.length > 0
      ? failedNote
      : idle
        ? "Files are transcribed in this order — drag to reorder, then hit Transcribe."
        : pipeStage === "transcribing"
          ? (rollingStatus(supported, staged.boundaries_ms, pipeDetail) ??
            RUNNING_LABELS.transcribing)
          : ((pipeStage && RUNNING_LABELS[pipeStage]) ??
            (converting != null ? "Preparing audio…" : "Starting…"));

  // 1-based position among the transcribable rows (error rows show "!")
  const rowNumber = (i: number) =>
    files.slice(0, i + 1).filter((f) => !f.error && !failure(f)).length;

  return (
    <div
      className="overflow-hidden rounded-xl border border-line"
      style={{ background: "var(--surface-2)" }}
    >
      <div className="flex items-center justify-between gap-2 border-b border-line px-4 py-3">
        <div className="min-w-0">
          <div className="text-[13.5px] font-bold text-text">Imported media</div>
          <div
            className="mt-0.5 text-[12px]"
            style={{
              color:
                runError || (idle && failed.length > 0) ? "var(--error-text)" : "var(--text-3)",
            }}
          >
            {subtitle}
          </div>
          {runError && (
            <div className="mt-0.5 text-[11.5px] text-text-3">{briefError(runError)}</div>
          )}
        </div>
        <div className="flex flex-none items-center gap-2">
          {idle && failed.length > 0 && (
            <Button
              variant="outline"
              size="sm"
              onClick={retry}
              title={`Convert the failed ${failed.length === 1 ? "file" : "files"} again, then transcribe the whole queue`}
            >
              {failed.length === 1 ? "Retry failed file" : "Retry failed files"}
            </Button>
          )}
          {idle && (
            <Button
              variant="primary"
              size="sm"
              onClick={start}
              disabled={supported.length === 0}
              title="Transcribe the files above, top to bottom"
            >
              Transcribe{supported.length > 1 ? ` ${supported.length} files` : ""}
            </Button>
          )}
          {runError && (
            <Button variant="primary" size="sm" onClick={onRetry} title="Try transcribing again">
              Retry
            </Button>
          )}
          {running && !runError && !afterAsr && (
            <Button
              variant="ghost"
              size="sm"
              onClick={onCancel}
              title="Stop transcribing — finished parts are kept and resume next time"
            >
              Cancel
            </Button>
          )}
        </div>
      </div>
      <div>
        {files.map((f, i) => {
          const cause = f.error ? null : failure(f);
          const bad = f.error != null || cause != null;
          const st = bad ? null : statuses[f.id];
          const n = rowNumber(i);
          const Icon = f.kind === "video" ? Monitor : Mic;
          return (
            <div
              key={f.id}
              draggable={idle && !bad}
              onDragStart={(e) => {
                dragIdx.current = i;
                e.dataTransfer.effectAllowed = "move";
              }}
              onDragEnd={() => {
                dragIdx.current = null;
              }}
              onDragOver={(e) => {
                e.preventDefault();
                if (idle) onDragOverRow(i);
              }}
              className={`flex items-center gap-2.5 bg-surface px-4 py-2.5 ${i ? "border-t border-line" : ""}`}
              style={{ cursor: idle && !bad ? "grab" : "default", opacity: bad ? 0.92 : 1 }}
            >
              {idle && !bad ? (
                <GripVertical size={14} strokeWidth={1.75} className="flex-none text-text-3" />
              ) : (
                <span className="w-3.5 flex-none" />
              )}
              <span
                className="w-5 flex-none text-center text-[12px] font-bold tabular-nums"
                style={{ color: bad ? "var(--error-text)" : "var(--text-3)" }}
              >
                {bad ? "!" : n}
              </span>
              <Icon
                size={15}
                strokeWidth={1.75}
                className="flex-none"
                style={{ color: bad ? "var(--error-text)" : "var(--text-3)" }}
              />
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13px] font-semibold text-text">{f.file_name}</div>
                {f.error || cause ? (
                  <div className="mt-px text-[11.5px]" style={{ color: "var(--error-text)" }}>
                    {f.error ?? cause}
                  </div>
                ) : (
                  <div className="mt-px text-[11.5px] text-text-3">{fmtSize(f.size)}</div>
                )}
              </div>
              {bad && idle && (
                <Button
                  variant="ghost"
                  size="xs"
                  title="Remove file"
                  aria-label={`Remove ${f.file_name}`}
                  onClick={() => removeFile(f.id)}
                >
                  <X size={13} strokeWidth={2} />
                </Button>
              )}
              {cause && running && (
                <span className="text-[12px] font-semibold" style={{ color: "var(--error-text)" }}>
                  Couldn&apos;t convert
                </span>
              )}
              {st === "waiting" && <span className="text-[12px] text-text-3">Waiting</span>}
              {st === "preparing" && (
                <span
                  className="inline-flex items-center gap-1.5 text-[12px] font-semibold"
                  style={{ color: "var(--primary-text)" }}
                >
                  <span
                    className="h-[7px] w-[7px] rounded-full"
                    style={{
                      background: "var(--primary)",
                      animation: "fly-pulse-dot 1.2s ease infinite",
                    }}
                  />
                  Preparing…
                </span>
              )}
              {st === "transcribing" && (
                <span
                  className="inline-flex items-center gap-1.5 text-[12px] font-semibold"
                  style={{ color: "var(--primary-text)" }}
                >
                  <span
                    className="h-[7px] w-[7px] rounded-full"
                    style={{
                      background: "var(--primary)",
                      animation: "fly-pulse-dot 1.2s ease infinite",
                    }}
                  />
                  Transcribing…{activePct != null ? ` ${activePct}%` : ""}
                </span>
              )}
              {st === "done" && (
                <span
                  className="text-[12px] font-semibold"
                  style={{ color: "var(--success-text)" }}
                >
                  ✓ Done
                </span>
              )}
            </div>
          );
        })}
        {files.length === 0 && (
          <div className="bg-surface px-4 py-4 text-[12.5px] text-text-3">
            No files left — close this note and import again.
          </div>
        )}
      </div>
    </div>
  );
}
