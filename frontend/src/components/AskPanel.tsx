import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { AskMessage } from "../types";
import { X } from "lucide-react";
import { api } from "../api";
import { Button, SectionLabel } from "./ui";

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
  // Answers already saved to the note (by history index) — their button stops
  // pulsing and reads "Saved".
  const [saved, setSaved] = useState<Set<number>>(new Set());

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
    <div
      className="print:hidden flex shrink-0 flex-col border-l border-line"
      style={{ width: "var(--askpanel-w)", background: "var(--bg)" }}
    >
      <div className="flex items-center justify-between border-b border-line px-3 py-2.5">
        <SectionLabel>Ask this meeting</SectionLabel>
        <Button variant="ghost" size="xs" onClick={onClose} aria-label="Close">
          <X size={14} strokeWidth={1.75} />
        </Button>
      </div>
      <div
        className="border-b px-3 py-2.5 text-[12px] font-medium leading-snug"
        style={{
          color: "var(--primary-soft-text)",
          background: "var(--primary-soft)",
          borderColor: "var(--primary-border)",
        }}
      >
        This chat isn’t saved — it clears when you leave. Click <strong>Save to note</strong> under
        an answer to keep it.
      </div>
      <div className="flex flex-1 flex-col gap-3 overflow-y-auto p-3">
        {history.length === 0 && (
          <div className="flex flex-wrap gap-1.5">
            {QUICK_PROMPTS.map((q) => (
              <Button key={q} variant="soft" size="xs" onClick={() => void send(q)}>
                {q}
              </Button>
            ))}
          </div>
        )}
        {history.map((m, i) => (
          <div
            key={i}
            className={`flex max-w-[88%] flex-col gap-1.5 ${
              m.role === "user" ? "items-end self-end" : "items-start self-start"
            }`}
          >
            <div
              className="rounded-[14px] border border-line px-3.5 py-2.5 text-left text-[14px] leading-relaxed"
              style={{
                color: "var(--text)",
                background: m.role === "user" ? "var(--primary-soft)" : "var(--surface)",
              }}
            >
              <div className="[&_a]:text-primary-text [&_a]:underline [&_li]:ml-4 [&_li]:list-disc [&_p]:my-1">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{m.content}</ReactMarkdown>
              </div>
            </div>
            {m.role === "assistant" &&
              (saved.has(i) ? (
                <span
                  className="px-1 text-[11.5px] font-semibold"
                  style={{ color: "var(--success-text)" }}
                >
                  ✓ Saved to note
                </span>
              ) : (
                <Button
                  variant="soft"
                  size="xs"
                  title="Append this answer to your note so it isn’t lost"
                  onClick={() => {
                    onInsert(m.content);
                    setSaved((s) => new Set(s).add(i));
                  }}
                  style={
                    // The newest answer breathes gently so it's clear this is
                    // how you keep it; earlier answers sit quiet.
                    i === history.length - 1 && !busy
                      ? { animation: "fly-pulse-glow 2s ease-in-out infinite" }
                      : undefined
                  }
                >
                  Save to note
                </Button>
              ))}
          </div>
        ))}
        {busy && (
          <div
            className="flex items-center gap-2 self-start px-1 text-xs font-medium"
            style={{ color: "var(--primary-soft-text)" }}
          >
            <span className="flex items-center gap-1">
              {[0, 0.2, 0.4].map((d) => (
                <span
                  key={d}
                  className="h-1.5 w-1.5 rounded-full"
                  style={{
                    background: "var(--primary)",
                    animation: `fly-pulse-dot 1s ease-in-out ${d}s infinite`,
                  }}
                />
              ))}
            </span>
            thinking…
          </div>
        )}
        {error && (
          <div className="text-xs font-medium" style={{ color: "var(--error-text)" }}>
            {error}
          </div>
        )}
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
          onFocus={(e) => (e.currentTarget.style.borderColor = "var(--primary)")}
          onBlur={(e) => (e.currentTarget.style.borderColor = "var(--line)")}
          className="flex-1 resize-none rounded-[11px] border px-3.5 py-2.5 text-[14px] outline-none placeholder:text-text-3"
          style={{ background: "var(--surface)", borderColor: "var(--line)", color: "var(--text)" }}
        />
        <Button variant="primary" size="md" onClick={() => void send(input)}>
          Send
        </Button>
      </div>
    </div>
  );
}
