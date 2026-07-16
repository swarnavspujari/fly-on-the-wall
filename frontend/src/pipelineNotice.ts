/** Pure notice-selection + error-briefing logic for the transcript panel's
 *  pipeline errors. Kept free of React so it can be unit-tested directly. */

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
