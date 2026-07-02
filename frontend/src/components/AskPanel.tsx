import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { AskMessage } from "../types";
import { api } from "../api";

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
    <div className="flex w-80 shrink-0 flex-col border-l border-zinc-800 bg-zinc-950">
      <div className="flex items-center justify-between border-b border-zinc-800 px-3 py-2">
        <span className="text-sm font-medium text-zinc-200">💬 Ask this meeting</span>
        <button className="text-zinc-500 hover:text-zinc-200" onClick={onClose}>
          ✕
        </button>
      </div>
      <div className="border-b border-zinc-800 px-3 py-1.5 text-[11px] text-zinc-500">
        Chat is ephemeral — it disappears when you close it unless you insert an answer.
      </div>
      <div className="flex-1 overflow-y-auto p-3">
        {history.length === 0 && (
          <div className="flex flex-col gap-1.5">
            {QUICK_PROMPTS.map((q) => (
              <button
                key={q}
                onClick={() => void send(q)}
                className="rounded-md border border-zinc-800 px-2 py-1.5 text-left text-xs text-zinc-300 hover:border-indigo-600 hover:text-indigo-200"
              >
                {q}
              </button>
            ))}
          </div>
        )}
        {history.map((m, i) => (
          <div key={i} className={`mb-3 ${m.role === "user" ? "text-right" : ""}`}>
            <div
              className={`inline-block max-w-[95%] rounded-lg px-2.5 py-1.5 text-left text-xs ${
                m.role === "user" ? "bg-indigo-600/40 text-indigo-100" : "bg-zinc-800 text-zinc-200"
              }`}
            >
              <div className="[&_li]:ml-4 [&_li]:list-disc [&_p]:my-1">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{m.content}</ReactMarkdown>
              </div>
              {m.role === "assistant" && (
                <button
                  onClick={() => onInsert(m.content)}
                  className="mt-1 text-[11px] text-indigo-400 hover:text-indigo-300"
                >
                  ↳ insert into note
                </button>
              )}
            </div>
          </div>
        ))}
        {busy && <div className="text-xs text-zinc-500">thinking…</div>}
        {error && <div className="text-xs text-red-400">⚠ {error}</div>}
      </div>
      <div className="border-t border-zinc-800 p-2">
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
          rows={2}
          className="w-full resize-none rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1.5 text-xs text-zinc-200 outline-none placeholder:text-zinc-600 focus:border-indigo-500"
        />
      </div>
    </div>
  );
}
