import { CircleAlert, HardDrive, Sparkles } from "lucide-react";
import { NOTICE_PRIORITY, appErrorCopy, consentSkipsCopy, updateNoticeVisible } from "../notices";
import type { PipelineFailure } from "../pipelineFailures";
import { pipelineFailureCopy, selectPipelineNotice } from "../pipelineNotice";
import type { Updater } from "../updater";
import { compressNoticeCopy } from "../storageModel";
import { useConsentSkips } from "./CloudConsent";
import { useCompressNotice } from "./CompressNotice";
import SystemAudioNotice from "./SystemAudioNotice";
import UpdateBanner from "./UpdateBanner";
import { Button, Notice, NoticeStack, type StackedNotice } from "./ui";

interface Props {
  /** AppInfo.os — per-OS copy. */
  os: string | null;
  /** Something is recording (audio or screen). */
  recordingActive: boolean;
  /** The macOS launch preflight found system audio blocked. */
  systemAudioBlocked: boolean;
  onOpenPrivacySettings: () => void;
  onDismissSystemAudio: () => void;
  /** A command the user triggered failed (was the bottom-left error toast). */
  appError: string | null;
  onDismissAppError: () => void;
  pipelineFailures: PipelineFailure[];
  /** Transcription-engine readiness; null until the first settings fetch. */
  engine: { installed: boolean; managed: boolean } | null;
  onOpenFailedNote: (failure: PipelineFailure) => void;
  /** Re-run transcription for a failure (only offered for a first
   *  transcription: a re-run could have been a re-analysis). */
  onRetryFailure: (failure: PipelineFailure) => void;
  onDismissPipelineFailure: (meetingId: string) => void;
  onOpenEngineSettings: () => void;
  updater: Updater;
}

/** Every app-wide notice in one corner (spec §3.7): a failure that answers
 *  the user's click > system audio > pipeline errors > skipped AI cleanup >
 *  update, the top one
 *  shown and the rest behind "+N more", so notices never overlap. Recording
 *  is sacred: the launch-time system-audio warning and the update prompt
 *  wait until it stops (the recording bar carries the live capture
 *  warnings); errors still show. */
export default function AppNotices({
  os,
  recordingActive,
  systemAudioBlocked,
  onOpenPrivacySettings,
  onDismissSystemAudio,
  appError,
  onDismissAppError,
  pipelineFailures,
  engine,
  onOpenFailedNote,
  onRetryFailure,
  onDismissPipelineFailure,
  onOpenEngineSettings,
  updater,
}: Props) {
  const notices: StackedNotice[] = [];
  const skips = useConsentSkips();
  const compressed = useCompressNotice();

  if (systemAudioBlocked && !recordingActive) {
    notices.push({
      id: "system-audio",
      priority: NOTICE_PRIORITY.systemAudio,
      node: (
        <SystemAudioNotice
          os={os}
          onOpenSettings={os === "macos" ? onOpenPrivacySettings : undefined}
          onDismiss={onDismissSystemAudio}
        />
      ),
    });
  }

  // A failure that answers the user's own click outranks everything, so
  // its alert is never hidden behind "+N more".
  if (appError) {
    const copy = appErrorCopy(appError);
    notices.push({
      id: "app-error",
      priority: NOTICE_PRIORITY.appError,
      node: (
        <Notice
          tone="danger"
          icon={<CircleAlert size={18} strokeWidth={1.75} />}
          title={copy.title}
          details={copy.details ?? undefined}
          detailsLabel={copy.detailsLabel}
          onDismiss={onDismissAppError}
          dismissLabel="Dismiss error"
        >
          <span className="break-words">{copy.cause}</span>
        </Notice>
      ),
    });
  }

  for (const failure of pipelineFailures) {
    const { kind } = selectPipelineNotice({
      pipelineError: failure.error,
      engineInstalling: false,
      engineInstalled: engine ? engine.installed : null,
    });
    const shownKind = kind === "none" ? "generic" : kind;
    const copy = pipelineFailureCopy({
      kind: shownKind,
      error: failure.error,
      os,
      engineManaged: engine ? engine.managed : null,
      noteTitle: failure.noteTitle,
      hadTranscript: failure.hadTranscript,
    });
    // Only a first transcription can be retried from here: with a transcript
    // the failed run may have been a speaker re-analysis.
    const canRetry = failure.hadTranscript === false;
    notices.push({
      id: `pipeline:${failure.meetingId}`,
      priority: NOTICE_PRIORITY.pipeline,
      node: (
        <Notice
          tone="danger"
          icon={<CircleAlert size={18} strokeWidth={1.75} />}
          title={copy.title}
          steps={copy.steps}
          details={copy.details}
          detailsLabel={copy.detailsLabel}
          actions={
            <>
              {canRetry && (
                <Button variant="primary" size="sm" onClick={() => onRetryFailure(failure)}>
                  Try again
                </Button>
              )}
              {failure.noteId && (
                <Button
                  variant={canRetry ? "outline" : "primary"}
                  size="sm"
                  onClick={() => onOpenFailedNote(failure)}
                >
                  Open note
                </Button>
              )}
              {shownKind !== "generic" && (
                <Button variant="outline" size="sm" onClick={onOpenEngineSettings}>
                  Settings
                </Button>
              )}
              <Button
                variant="ghost"
                size="sm"
                onClick={() => onDismissPipelineFailure(failure.meetingId)}
              >
                Dismiss
              </Button>
            </>
          }
        >
          {copy.cause}
        </Notice>
      ),
    });
  }

  // Cleanup and action items the backend skipped for missing cloud consent:
  // ask for it here, then run them. Not urgent, so it waits out a recording.
  if (skips.count > 0 && !recordingActive) {
    const copy = consentSkipsCopy(skips.count, skips.vendor);
    notices.push({
      id: "consent-skips",
      priority: NOTICE_PRIORITY.consentSkips,
      node: (
        <Notice
          tone="warning"
          icon={<Sparkles size={18} strokeWidth={1.75} />}
          title={copy.title}
          actions={
            <>
              <Button
                variant="primary"
                size="sm"
                disabled={skips.busy}
                onClick={() => void skips.run()}
              >
                {copy.action}
              </Button>
              <Button variant="ghost" size="sm" onClick={skips.dismiss}>
                Dismiss
              </Button>
            </>
          }
        >
          {copy.cause}
        </Notice>
      ),
    });
  }

  // Recordings were compressed losslessly (once, after an upgrade). Good
  // news only, so it waits out a recording.
  if (compressed.savedBytes !== null && !recordingActive) {
    const copy = compressNoticeCopy(compressed.savedBytes);
    notices.push({
      id: "compressed",
      priority: NOTICE_PRIORITY.compressed,
      node: (
        <Notice
          tone="success"
          icon={<HardDrive size={18} strokeWidth={1.75} />}
          title={copy.title}
          onDismiss={compressed.dismiss}
          dismissLabel="Dismiss"
        >
          {copy.cause}
        </Notice>
      ),
    });
  }

  if (!recordingActive && updateNoticeVisible(updater)) {
    notices.push({
      id: "update",
      priority: NOTICE_PRIORITY.update,
      node: <UpdateBanner updater={updater} />,
    });
  }

  // The wrapper hides it in print: the primitive's unlayered display rule
  // beats a print:hidden class on the stack itself.
  return (
    <div className="print:hidden">
      <NoticeStack notices={notices} style={STACK_STYLE} />
    </div>
  );
}

/** Expanded on a short window the stack would run off the top: cap it and
 *  scroll. The 8px padding (offset by a negative margin, so the corner
 *  position is unchanged) keeps the notices' shadows and focus rings from
 *  being clipped by the scroll box. Inline because the primitive's CSS is
 *  unlayered and would beat Tailwind utilities. */
const STACK_STYLE = {
  width: 400,
  maxWidth: "calc(100vw - 16px)",
  maxHeight: "calc(100vh - 48px)",
  overflowY: "auto",
  overscrollBehavior: "contain",
  margin: -8,
  padding: 8,
} as const;
