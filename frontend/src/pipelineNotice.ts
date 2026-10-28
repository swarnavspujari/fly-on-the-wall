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
  /** The note already had a transcript (so this was a re-run: transcription
   *  or a speaker re-analysis). null = unknown. */
  hadTranscript: boolean | null;
}

export interface PipelineFailureCopy {
  title: string;
  cause: string;
  steps: string[];
  detailsLabel: string;
  details: string;
}

/** Copy for a run that failed while its note wasn't open; the notice stays
 *  in the stack until a retry starts or it's dismissed.
 *  - First transcription (`hadTranscript === false`): the notice has Try
 *    again, so the steps end there.
 *  - A re-run, or unknown: the failed run may have been re-transcription or
 *    re-analyzing speakers (both end the same way), so the copy says the
 *    existing transcript is kept and points to the note to run it again. */
export function pipelineFailureCopy({
  kind,
  error,
  os,
  engineManaged,
  noteTitle,
  hadTranscript,
}: PipelineFailureCopyInput): PipelineFailureCopy {
  const first = hadTranscript === false;
  const name = noteTitle ? `“${noteTitle}”` : null;
  const what = first
    ? `${name ?? "A recording"} wasn’t transcribed. The recording is saved.`
    : hadTranscript
      ? `${name ?? "A meeting"} didn’t finish processing. Its transcript is unchanged.`
      : `${name ?? "A meeting"} didn’t finish processing. The recording and any transcript are kept.`;
  const last = first ? "Choose Try again." : "Open the note and run it again from its transcript.";
  const details = briefError(error);
  if (kind === "engine-missing") {
    let install: string;
    if (engineManaged !== false) install = "Install the engine in Settings › Transcripts.";
    else if (os === "macos") install = "In Terminal, run: brew install whisper-cpp";
    else if (os === "linux") {
      install = "Install whisper.cpp from your package manager, or build it from source.";
    } else install = "Set up the engine in Settings › Transcripts.";
    return {
      title: "Transcription engine isn’t installed",
      cause: `${what} The engine that transcribes it is missing.`,
      steps: [install, last],
      detailsLabel: "Details",
      details,
    };
  }
  if (kind === "download-failed") {
    return {
      title: "Model download didn’t finish",
      cause: `${what} A model it needs didn’t download.`,
      steps: ["Check that you’re online.", last],
      detailsLabel: "Details",
      details,
    };
  }
  return {
    title: first ? "Transcription didn’t finish" : "Processing didn’t finish",
    cause: what,
    steps: [last],
    detailsLabel: "Details",
    details,
  };
}
