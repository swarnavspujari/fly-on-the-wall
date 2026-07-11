import { useState } from "react";
import { Btn, ModalShell } from "./ui";

interface Props {
  onAccept: () => void;
}

/** First-run consent + legal note (spec §9): recording laws differ by
 *  jurisdiction; the user acknowledges once before the first recording
 *  feature is available. */
export default function FirstRunNotice({ onAccept }: Props) {
  const [checked, setChecked] = useState(false);

  return (
    <ModalShell className="w-[520px] p-6">
      <h2 className="mb-2 font-display text-xl font-bold tracking-tight text-ink">
        Before you record
      </h2>
      <p className="mb-3 text-[14px] leading-relaxed text-ink-2">
        Fly on the Wall records your microphone <em>and</em> the audio of other meeting participants (system
        audio). Everything is stored{" "}
        <span className="font-medium text-ink">only on this computer</span> — nothing is uploaded
        unless you explicitly enable a cloud provider.
      </p>
      <div className="mb-3 rounded-[12px] border border-line bg-peach-2 p-4 text-[13px] leading-relaxed text-ink-2">
        <p className="mb-1 font-semibold text-ink">Recording laws vary by jurisdiction.</p>
        <p>
          Many places require the consent of <em>all</em> participants before recording a
          conversation. It is your responsibility to know the rules that apply to you and to tell
          people they are being recorded. Fly on the Wall shows a persistent on-screen indicator while
          recording.
        </p>
      </div>
      <label className="mb-4 flex items-start gap-2 text-[13px] text-ink">
        <input
          type="checkbox"
          className="mt-0.5 accent-coral"
          style={{ accentColor: "var(--color-coral)" }}
          checked={checked}
          onChange={(e) => setChecked(e.target.checked)}
        />
        I understand that I am responsible for complying with the recording-consent laws that apply
        to my meetings.
      </label>
      <div className="flex justify-end">
        <Btn variant="primary" size="md" disabled={!checked} onClick={onAccept}>
          Got it
        </Btn>
      </div>
    </ModalShell>
  );
}
