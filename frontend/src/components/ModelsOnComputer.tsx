import type { CSSProperties } from "react";
import { useEffect, useId, useState } from "react";
import { api } from "../api";
import {
  formatBytes,
  freeUpPlan,
  freeUpSummary,
  outcomeMessage,
  rowDetail,
  rowStatus,
  shortName,
} from "../modelStorage";
import type {
  ArtifactUsage,
  AsrSettings,
  DeleteOutcome,
  DiskEntry,
  ModelProgress,
  ModelStatus,
  ModelStorageReport,
} from "../types";
import { WHISPER_ENGINE_ID } from "../types";
import { useConfirm } from "./confirm";
import { Badge, Button, ProgressBar } from "./ui";

/* Settings › Transcripts › More options › Models on this computer: every
   downloaded model and tool with its size on disk and what uses it, a
   Delete for the unused ones, and one "Free up" for all of them plus
   download leftovers. What's in use comes from the backend
   (model_cleanup.rs), which also re-checks every delete. */

const LIST_ROW =
  "flex items-center justify-between gap-3 border-t border-line-2 py-2 first:border-t-0";
const NAME: CSSProperties = { fontFamily: "var(--font-mono)", fontSize: "12.5px" };
const DETAIL: CSSProperties = { fontSize: 11, lineHeight: 1.5 };
const MONO_SMALL: CSSProperties = { fontFamily: "var(--font-mono)", fontSize: 11 };
const HEADING: CSSProperties = { fontSize: 12, fontWeight: 600, margin: "12px 0 2px" };

interface Props {
  /** Saved transcript settings: the model list, engine state. A new object
   *  (after a save, install or delete) refreshes the report. */
  settings: AsrSettings;
  /** Id of the download running from Settings, if any. */
  downloading: string | null;
  downloadErrors: Record<string, string>;
  modelProgress: ModelProgress | null;
  onDownload: (id: string) => void;
  /** After a delete: reload the saved settings (installed flags changed). */
  onDeleted: () => void;
}

/** A not-yet-reported row, from the settings list (sizes, no labels). */
function fallbackRow(m: ModelStatus): ArtifactUsage {
  return {
    id: m.id,
    display: m.display,
    installed: m.installed,
    size_on_disk: m.installed ? m.bytes : 0,
    download_bytes: m.bytes,
    in_use: null,
    note: null,
    can_delete: false,
    blocked: null,
  };
}

export default function ModelsOnComputer({
  settings,
  downloading,
  downloadErrors,
  modelProgress,
  onDownload,
  onDeleted,
}: Props) {
  const confirm = useConfirm();
  const [report, setReport] = useState<ModelStorageReport | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [msg, setMsg] = useState<{ text: string; tone: "default" | "error" } | null>(null);
  const idBase = useId();

  // In-use depends on the SAVED settings, so refresh whenever they reload.
  useEffect(() => {
    let live = true;
    api
      .modelStorageReport()
      .then((r) => {
        if (live) setReport(r);
      })
      .catch((e) => {
        if (live) setMsg({ text: `Couldn't check model sizes: ${String(e)}`, tone: "error" });
      });
    return () => {
      live = false;
    };
  }, [settings]);

  const reported = (id: string) => report?.artifacts.find((a) => a.id === id);
  // Settings' order (whisper models, then tools and speaker models), then
  // anything only the report lists (Parakeet weights, once downloaded).
  const listed = settings.models.filter((m) => m.id !== WHISPER_ENGINE_ID);
  const rows: ArtifactUsage[] = [
    ...[
      ...listed.filter((m) => m.id.startsWith("ggml-")),
      ...listed.filter((m) => !m.id.startsWith("ggml-")),
    ].map((m) => reported(m.id) ?? fallbackRow(m)),
    ...(report?.artifacts.filter(
      (a) => a.id !== WHISPER_ENGINE_ID && !settings.models.some((m) => m.id === a.id),
    ) ?? []),
  ];
  const engine = reported(WHISPER_ENGINE_ID);
  const allRows = report?.artifacts ?? rows;
  const plan = report ? freeUpPlan(report) : null;

  const apply = (o: DeleteOutcome) => {
    setReport(o.report);
    setMsg(outcomeMessage(o));
    onDeleted();
  };

  const run = async (ids: string[], paths: string[]) => {
    setDeleting(true);
    setMsg(null);
    try {
      apply(await api.deleteModelFiles(ids, paths));
    } catch (e) {
      setMsg({ text: String(e), tone: "error" });
      api.modelStorageReport().then(setReport, () => {});
    } finally {
      setDeleting(false);
    }
  };

  const deleteOne = async (a: ArtifactUsage) => {
    const name = shortName(a, allRows);
    const ok = await confirm({
      title: `Delete ${name}?`,
      message: "It's removed from this computer. You can download it again later.",
      confirmLabel: "Delete",
      tone: "danger",
    });
    if (ok) await run([a.id], []);
  };

  const freeUp = async () => {
    if (!plan || plan.items.length === 0) return;
    const total = formatBytes(plan.bytes);
    const ok = await confirm({
      title: `Free up ${total}?`,
      message: (
        <div data-testid="free-up-list">
          <p style={{ margin: 0 }}>These are removed from this computer:</p>
          <ul style={{ listStyle: "disc", paddingLeft: 20, margin: "6px 0" }}>
            {plan.items.map((i, n) => (
              <li key={n}>
                {i.label} · {formatBytes(i.bytes)}
              </li>
            ))}
          </ul>
          <p style={{ margin: 0 }}>
            Anything you need later downloads again. Files the app doesn&apos;t recognise are left
            alone.
          </p>
        </div>
      ),
      confirmLabel: `Free up ${total}`,
      tone: "danger",
    });
    if (ok) await run(plan.ids, plan.paths);
  };

  /** Badge + Delete for an installed row. */
  const installedControls = (a: ArtifactUsage, detailId: string) => {
    const status = rowStatus(a);
    const disabled = deleting || !a.can_delete;
    return (
      <>
        {status === "in_use" ? (
          <Badge tone="success" dot>
            in use
          </Badge>
        ) : (
          <Badge tone="neutral">not in use</Badge>
        )}
        <Button
          variant="ghost"
          size="xs"
          disabled={disabled}
          aria-label={`Delete ${shortName(a, allRows)}`}
          aria-describedby={disabled ? detailId : undefined}
          title={a.blocked ?? (report ? "Remove from this computer" : "Checking what's in use…")}
          onClick={() => void deleteOne(a)}
        >
          Delete
        </Button>
      </>
    );
  };

  const progressFor = (id: string) =>
    downloading === id && modelProgress && modelProgress.id === id && modelProgress.total > 0
      ? Math.round((modelProgress.downloaded / modelProgress.total) * 100)
      : null;

  const downloadingControls = (id: string) => {
    const pct = progressFor(id);
    return pct != null ? (
      <span className="flex items-center gap-2">
        <ProgressBar value={pct} style={{ width: 96 }} />
        <span className="text-text-3" style={MONO_SMALL}>
          {pct}%
        </span>
      </span>
    ) : (
      <span className="text-text-3" style={MONO_SMALL}>
        downloading…
      </span>
    );
  };

  const engineDetailId = `${idBase}-engine`;
  const entryRow = (e: DiskEntry, kept: boolean) => (
    <div key={e.path} className={LIST_ROW} data-testid={kept ? "unrecognised-row" : "leftover-row"}>
      <div className="min-w-0">
        <div className="truncate text-text" style={{ fontSize: 12.5 }}>
          {kept ? e.path : e.label}
        </div>
        {!kept && (
          <div className="truncate text-text-3" style={MONO_SMALL}>
            {e.path}
          </div>
        )}
      </div>
      <span className="flex shrink-0 items-center gap-2">
        <span className="text-text-3" style={MONO_SMALL}>
          {formatBytes(e.bytes)}
        </span>
        {kept && <Badge tone="neutral">left alone</Badge>}
      </span>
    </div>
  );

  return (
    <div data-testid="models-on-computer">
      {/* Transcription engine — the whisper.cpp binary that runs the weights
          below. Weights alone can't transcribe, so its state is explicit. */}
      <div className={LIST_ROW}>
        <div className="min-w-0">
          <div className="truncate text-text" style={NAME}>
            Transcription engine (whisper.cpp)
          </div>
          <div id={engineDetailId} className="text-text-3" style={DETAIL}>
            {settings.engine_installed
              ? engine?.installed
                ? `${formatBytes(engine.size_on_disk)} on disk · ${engine.in_use ?? "Installed. Runs the models below on this computer."}`
                : "Installed. Runs the models below on this computer."
              : settings.engine_managed
                ? "Required to transcribe. Downloaded models can't run without it."
                : "Required to transcribe. Install whisper.cpp yourself: on macOS, brew install whisper-cpp; on Linux, your package manager or build from source."}
          </div>
          {downloadErrors[WHISPER_ENGINE_ID] && (
            <div style={{ fontSize: 11, color: "var(--error-text)" }}>
              {downloadErrors[WHISPER_ENGINE_ID]}
            </div>
          )}
        </div>
        <span className="flex shrink-0 items-center gap-2">
          {settings.engine_installed ? (
            engine?.installed ? (
              installedControls(engine, engineDetailId)
            ) : (
              <Badge tone="success" dot>
                ready
              </Badge>
            )
          ) : downloading === WHISPER_ENGINE_ID ? (
            downloadingControls(WHISPER_ENGINE_ID)
          ) : settings.engine_managed ? (
            <Button variant="outline" size="sm" onClick={() => onDownload(WHISPER_ENGINE_ID)}>
              Install
            </Button>
          ) : (
            <Badge tone="warning">missing</Badge>
          )}
        </span>
      </div>

      {rows.map((a) => {
        const detailId = `${idBase}-${a.id}`;
        return (
          <div key={a.id} className={LIST_ROW} data-testid={`model-row-${a.id}`}>
            <div className="min-w-0">
              <div className="truncate text-text" style={NAME}>
                {a.display}
              </div>
              <div id={detailId} className="text-text-3" style={DETAIL}>
                {rowDetail(a)}
              </div>
              {downloadErrors[a.id] && (
                <div style={{ fontSize: 11, color: "var(--error-text)" }}>
                  {downloadErrors[a.id]}
                </div>
              )}
            </div>
            <span className="flex shrink-0 items-center gap-2">
              {a.installed ? (
                installedControls(a, detailId)
              ) : downloading === a.id ? (
                downloadingControls(a.id)
              ) : (
                <Button variant="outline" size="sm" onClick={() => onDownload(a.id)}>
                  Install
                </Button>
              )}
            </span>
          </div>
        );
      })}

      {report && report.leftovers.length > 0 && (
        <>
          <p className="text-text-2" style={HEADING}>
            Leftovers
          </p>
          {report.leftovers.map((e) => entryRow(e, false))}
        </>
      )}
      {report && report.unrecognised.length > 0 && (
        <>
          <p className="text-text-2" style={HEADING}>
            Not recognised
          </p>
          <p className="text-text-3" style={{ ...DETAIL, margin: 0 }}>
            The app didn&apos;t put these here, so it leaves them alone.
          </p>
          {report.unrecognised.map((e) => entryRow(e, true))}
        </>
      )}

      {plan && (
        <div
          className="mt-3 flex items-center justify-between gap-3 border-t border-line-2 pt-3"
          data-testid="free-up"
        >
          <div className="min-w-0 text-text-3" style={DETAIL}>
            <div>{freeUpSummary(plan)}</div>
            {report?.busy && <div className="text-warning-text">{report.busy}</div>}
          </div>
          <Button
            variant="outline"
            size="sm"
            disabled={deleting || plan.items.length === 0 || !!report?.busy}
            onClick={() => void freeUp()}
          >
            {deleting
              ? "Deleting…"
              : plan.bytes > 0
                ? `Free up ${formatBytes(plan.bytes)}`
                : "Free up"}
          </Button>
        </div>
      )}
      {msg && (
        <p
          role="status"
          style={{
            ...DETAIL,
            margin: "6px 0 0",
            color: msg.tone === "error" ? "var(--error-text)" : "var(--text-2)",
          }}
        >
          {msg.text}
        </p>
      )}
    </div>
  );
}
