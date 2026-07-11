import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { AskMessage } from "../types";
import { api } from "../api";
import { Btn, SectionLabel } from "./ui";

interface Props {
  noteId: string;
  onInsert: (content: string) => void;
  onClose: () => void;
}

const QUICK_PROMPTS = [
  "What did I miss?",
  "What decisions were made?",
  "List the action items",
  "Draft a follow-up email",
];

/** Ephemeral chat grounded in the meeting (transcript + notes). Nothing is
 *  saved unless you insert an answer into the note. */
export default function AskPanel({ noteId, onInsert, onClose }: Props) {
  const [history, setHistory] = useState<AskMessage[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const send = async (text: string) => {
    const content = text.trim();
    if (!content || busy) return;
    setError(null);
    const next: AskMessage[] = [...history, { role: "user", content }];
    setHistory(next);
    setInput("");
    setBusy(true);
    try {
      const reply = await api.askMeeting(noteId, next);
      setHistory([...next, { role: "assistant", content: reply }]);
    } catch (e) {
      setError(String(e));
      setHistory(history); // roll back the optimistic user message
      setInput(content);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="print:hidden flex w-80 shrink-0 flex-col border-l border-line bg-cream">
      <div className="flex items-center justify-between border-b border-line px-3 py-2">
        <SectionLabel>Ask Fly on the Wall</SectionLabel>
        <Btn variant="ghost" size="xs" onClick={onClose}>
          ✕
        </Btn>
      </div>
      <div className="border-b border-line px-3 py-1.5 text-[11px] leading-snug text-mute">
        Chat is ephemeral — it disappears when you close it unless you insert an answer.
      </div>
      <div className="flex flex-1 flex-col gap-3 overflow-y-auto p-3">
        {history.length === 0 && (
          <div className="flex flex-wrap gap-1.5">
            {QUICK_PROMPTS.map((q) => (
              <Btn key={q} variant="soft" size="xs" onClick={() => void send(q)}>
                {q}
              </Btn>
            ))}
          </div>
        )}
        {history.map((m, i) => (
          <div
            key={i}
            className={`flex max-w-[85%] flex-col gap-1.5 ${
              m.role === "user" ? "items-end self-end" : "items-start self-start"
            }`}
          >
            <div
              className={`rounded-[14px] border border-line px-3.5 py-2.5 text-left text-[14px] leading-relaxed text-ink ${
                m.role === "user" ? "bg-peach" : "bg-surface"
              }`}
            >
              <div className="[&_a]:text-clay [&_a]:underline [&_li]:ml-4 [&_li]:list-disc [&_p]:my-1">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{m.content}</ReactMarkdown>
              </div>
            </div>
            {m.role === "assistant" && (
              <Btn variant="soft" size="xs" onClick={() => onInsert(m.content)}>
                ↳ Insert into note
              </Btn>
            )}
          </div>
        ))}
        {busy && (
          <div className="flex items-center gap-2 self-start px-1 text-xs font-medium text-clay">
            <span className="flex items-center gap-1">
              <span className="h-1.5 w-1.5 animate-[pulse-dot_1s_ease-in-out_infinite] rounded-full bg-coral" />
              <span className="h-1.5 w-1.5 animate-[pulse-dot_1s_ease-in-out_0.2s_infinite] rounded-full bg-coral" />
              <span className="h-1.5 w-1.5 animate-[pulse-dot_1s_ease-in-out_0.4s_infinite] rounded-full bg-coral" />
            </span>
            thinking…
          </div>
        )}
        {error && <div className="text-xs font-medium text-clay">⚠ {error}</div>}
      </div>
      <div className="flex gap-2 border-t border-line p-3">
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void send(input);
            }
          }}
          placeholder="Ask about this meeting…"
          rows={1}
          className="flex-1 resize-none rounded-[11px] border border-line bg-surface px-3.5 py-2.5 text-[14px] text-ink outline-none placeholder:text-mute focus:border-coral"
        />
        <Btn variant="primary" size="md" onClick={() => void send(input)}>
          Send
        </Btn>
      </div>
    </div>
  );
}
