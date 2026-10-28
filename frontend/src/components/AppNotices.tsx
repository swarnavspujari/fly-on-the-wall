import { CircleAlert } from "lucide-react";
import { NOTICE_PRIORITY, appErrorCopy, updateNoticeVisible } from "../notices";
import { pipelineFailureCopy, selectPipelineNotice } from "../pipelineNotice";
import type { Updater } from "../updater";
import SystemAudioNotice from "./SystemAudioNotice";
import UpdateBanner from "./UpdateBanner";
import { Button, Notice, NoticeStack, type StackedNotice } from "./ui";

/** A transcription that failed while its note wasn't open. Opening the note
 *  hands the error to its inline notice and drops it from the stack. */
export interface PipelineFailure {
  meetingId: string;
  noteId: string | null;
  noteTitle: string | null;
  error: string;
}

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
  onDismissPipelineFailure: (meetingId: string) => void;
  onOpenEngineSettings: () => void;
  updater: Updater;
}

/** Every app-wide notice in one corner (spec §3.7): system audio > errors >
 *  update, the top one shown and the rest behind "+N more", so notices
 *  never overlap. Recording is sacred: the launch-time system-audio warning
 *  and the update prompt wait until it stops (the recording bar carries the
 *  live capture warnings); errors still show. */
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
  onDismissPipelineFailure,
  onOpenEngineSettings,
  updater,
}: Props) {
  const notices: StackedNotice[] = [];

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

  // The user's own action failing ranks ahead of a background failure of
  // the same priority (equal priorities keep this order).
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
    });
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
              {failure.noteId && (
                <Button variant="primary" size="sm" onClick={() => onOpenFailedNote(failure)}>
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

  if (!recordingActive && updateNoticeVisible(updater)) {
    notices.push({
      id: "update",
      priority: NOTICE_PRIORITY.update,
      node: <UpdateBanner updater={updater} />,
    });
  }

  return <NoticeStack notices={notices} className="print:hidden" style={STACK_STYLE} />;
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
