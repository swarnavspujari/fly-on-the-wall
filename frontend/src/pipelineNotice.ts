/** Pure notice-selection + error-briefing logic for pipeline errors: the
 *  transcript panel's inline notices and the app notice stack's copy for
 *  failures of notes that aren't open. Kept free of React so it can be
 *  unit-tested directly. */

export type PipelineNoticeKind = "none" | "engine-missing" | "download-failed" | "generic";

export interface PipelineNoticeInput {
  /** Raw pipeline error, if a transcribe (or in-app engine install) failed. */
  pipelineError: string | null;
  /** True while an in-app engine install is streaming. */
  engineInstalling: boolean;
  /** whisper-cli resolvable right now; null until the first settings fetch. */
  engineInstalled: boolean | null;
}

export interface PipelineNotice {
  kind: PipelineNoticeKind;
  /** engine-missing only: a failed in-app install's (briefed) error to show
   *  inside the notice instead of silently dropping it. */
  installError: string | null;
}

/** Collapse a pipeline error to its human part: signed CDN URLs run to
 *  hundreds of characters and carry no meaning for the user. */
export function briefError(error: string, max = 260): string {
  const brief = error
    .replace(/https?:\/\/\S+/g, "")
    .replace(/\s+/g, " ")
    .trim();
  return brief.length > max ? `${brief.slice(0, max)}…` : brief;
}

/** The engine resolver's error (models.rs `ensure_tool`) — the one signal
 *  that the ENGINE, not weights or the network, is what's missing. */
const ENGINE_MISSING_ERROR = /whisper-cli is not installed/i;
/** Download errors (models.rs `ensure`): offline, CDN outage, checksum. */
const DOWNLOAD_ERROR = /download (failed|interrupted)/i;
/** An in-app engine install that failed for ANY reason (App.tsx tags it) —
 *  extraction/disk errors carry no "download" keyword but must keep the
 *  actionable engine notice rather than dropping to a bare error box. */
const ENGINE_INSTALL_ERROR = /^engine install failed/i;

/** Which notice a pipeline failure deserves. Matching on the error CONTENT
 *  (not merely "engine absent + some error") keeps Groq failures and other
 *  unrelated errors from being mislabeled as an engine problem. */
export function selectPipelineNotice({
  pipelineError,
  engineInstalling,
  engineInstalled,
}: PipelineNoticeInput): PipelineNotice {
  if (engineInstalling) return { kind: "engine-missing", installError: null };
  if (!pipelineError) return { kind: "none", installError: null };
  if (ENGINE_INSTALL_ERROR.test(pipelineError) && engineInstalled !== true) {
    return { kind: "engine-missing", installError: briefError(pipelineError) };
  }
  const isDownload = DOWNLOAD_ERROR.test(pipelineError);
  // The pipeline itself said the engine can't be resolved. Stale case
  // (engine has appeared since, e.g. brew install + settings refresh) falls
  // through to the generic box — offering "Install engine" then would lie.
  if (ENGINE_MISSING_ERROR.test(pipelineError) && engineInstalled !== true) {
    return { kind: "engine-missing", installError: null };
  }
  // Engine absent AND a download error: the engine download itself failed
  // (in-app install or the pipeline's managed attempt) — keep the actionable
  // engine notice and surface the failure inside it.
  if (isDownload && engineInstalled === false) {
    return { kind: "engine-missing", installError: briefError(pipelineError) };
  }
  if (isDownload) return { kind: "download-failed", installError: null };
  return { kind: "generic", installError: null };
}

export interface PipelineFailureCopyInput {
  kind: Exclude<PipelineNoticeKind, "none">;
  /** Raw pipeline error (briefed into the details). */
  error: string;
  /** AppInfo.os: "windows" | "macos" | "linux"; null before it loads. */
  os: string | null;
  /** A managed engine download exists for this OS; null until known. */
  engineManaged: boolean | null;
  /** Title of the failed recording's note, when the list knows it. */
  noteTitle: string | null;
}

export interface PipelineFailureCopy {
  title: string;
  cause: string;
  steps: string[];
  detailsLabel: string;
  details: string;
}

/** Copy for a transcription that failed while its note wasn't open (the
 *  open note shows the inline, actionable notice instead). Opening the note
 *  moves the failure there, so the steps point at its buttons. */
export function pipelineFailureCopy({
  kind,
  error,
  os,
  engineManaged,
  noteTitle,
}: PipelineFailureCopyInput): PipelineFailureCopy {
  const what = noteTitle ? `“${noteTitle}” wasn’t transcribed.` : "A recording wasn’t transcribed.";
  const transcribe = "Open the note and choose Transcribe recording.";
  const details = briefError(error);
  if (kind === "engine-missing") {
    let steps: string[];
    if (engineManaged !== false) {
      steps = ["Open the note and choose Install engine.", "Transcribe the recording."];
    } else if (os === "macos") {
      steps = ["In Terminal, run: brew install whisper-cpp", transcribe];
    } else if (os === "linux") {
      steps = [
        "Install whisper.cpp from your package manager, or build it from source.",
        transcribe,
      ];
    } else {
      steps = ["Set up the engine in Settings › Transcripts.", transcribe];
    }
    return {
      title: "Transcription engine isn’t installed",
      cause: `${what} The recording is saved, but the engine that transcribes it is missing.`,
      steps,
      detailsLabel: "Details",
      details,
    };
  }
  if (kind === "download-failed") {
    return {
      title: "Model download didn’t finish",
      cause: `${what} The speech model didn’t download, so transcription couldn’t start.`,
      steps: ["Check that you’re online.", "Open the note and choose Try again."],
      detailsLabel: "Details",
      details,
    };
  }
  return {
    title: "Transcription didn’t finish",
    cause: `${what} The recording is saved.`,
    steps: ["Open the note and transcribe it again."],
    detailsLabel: "Details",
    details,
  };
}
