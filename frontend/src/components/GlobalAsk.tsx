import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { AskComposer, AskSheet, OPEN_ASK_EVENT } from "./AskPanel";
import type { AskChat, AskTurn } from "./AskPanel";
import type { OpenSource } from "./AskAnswer";

/* Ask over every meeting, from Home: the same composer and answer sheet as
   in a note, docked at the bottom of Home, starting on "All meetings". The
   chat lives in App (it survives opening a source and coming back) and is
   ephemeral like a note's. There's no Insert here (no note is open); a
   proposal still names the note it would change. */

export default function GlobalAsk({
  chat,
  onOpenSource,
  turnExtras,
}: {
  chat: AskChat;
  onOpenSource: OpenSource;
  turnExtras?: (turn: AskTurn) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // The sidebar entry and Ctrl/Cmd+J open it, ready to type.
  const { setScope } = chat;
  useEffect(() => {
    const onOpenAsk = () => {
      setScope(null); // every meeting is this Ask's default
      setOpen(true);
      inputRef.current?.focus();
    };
    window.addEventListener(OPEN_ASK_EVENT, onOpenAsk);
    return () => window.removeEventListener(OPEN_ASK_EVENT, onOpenAsk);
  }, [setScope]);

  return (
    // Like the note's dock: its empty space lets clicks through to Home.
    <div
      className="print:hidden pointer-events-none absolute bottom-5 left-1/2 z-[8] flex w-[min(760px,calc(100%-24px))] -translate-x-1/2 items-center justify-center [&>*]:pointer-events-auto"
      data-testid="global-ask"
    >
      <AskComposer
        chat={chat}
        inputRef={inputRef}
        open={open}
        onOpen={() => setOpen(true)}
        placeholder="Ask about your meetings"
      />
      {open && (
        <AskSheet
          chat={chat}
          inputRef={inputRef}
          hasMeeting
          keptLocal={false}
          pinned={false}
          canPin={false}
          onTogglePin={() => {}}
          onClose={() => setOpen(false)}
          onOpenSource={onOpenSource}
          turnExtras={turnExtras}
        />
      )}
    </div>
  );
}
