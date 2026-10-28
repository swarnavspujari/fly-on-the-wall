import { useEffect, useId, useRef, useState } from "react";
import type { ReactNode, RefObject } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  ArrowUp,
  CheckCheck,
  CircleHelp,
  ListTodo,
  Mail,
  PanelRightClose,
  PanelRightOpen,
  Sparkles,
  TriangleAlert,
} from "lucide-react";
import type { AskMessage, LlmSettings } from "../types";
import { api } from "../api";
import {
  CLAUDE_MODELS,
  QUICK_PROMPTS,
  cloudConsentMessage,
  describeAskError,
  providerName,
} from "../askModel";
import { providerLabel } from "../settingsModel";
import { Button, Notice, Select, Sheet } from "./ui";
import { SAFE_A, SAFE_IMG } from "./markdownSafe";

/* Ask over today's backend: a composer docked by the view switcher, answers
   in a sheet over the note (or pinned beside it on wide windows). The chat is
   ephemeral, and nothing reaches the note until you click Insert. Every
   question goes through `ask_meeting`, so `build_provider`'s cloud consent and
   Keep local apply as before; Runs on only changes the saved provider, the
   same setting Settings › Ask and Enhance writes. */

const ASK_MD = { a: SAFE_A, img: SAFE_IMG };

const PROMPT_ICONS: Record<(typeof QUICK_PROMPTS)[number], ReactNode> = {
  "What did I miss?": <CircleHelp size={15} strokeWidth={1.75} />,
  "What decisions were made?": <CheckCheck size={15} strokeWidth={1.75} />,
  "List the action items": <ListTodo size={15} strokeWidth={1.75} />,
  "Draft a follow-up email": <Mail size={15} strokeWidth={1.75} />,
};

/** The sheet can pin beside the note from this width up. */
export const PIN_MIN_WIDTH = 1440;

/** True while the window is at least PIN_MIN_WIDTH wide. */
export function useWideWindow(): boolean {
  const query = `(min-width: ${PIN_MIN_WIDTH}px)`;
  const [wide, setWide] = useState(
    () => typeof window !== "undefined" && !!window.matchMedia?.(query).matches,
  );
  useEffect(() => {
    const mq = window.matchMedia?.(query);
    if (!mq) return undefined;
    const on = () => setWide(mq.matches);
    on();
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, [query]);
  return wide;
}

export interface AskChat {
  history: AskMessage[];
  input: string;
  setInput: (value: string) => void;
  busy: boolean;
  /** The last failure and the question it was for (to retry). */
  error: { raw: string; question: string } | null;
  /** Answers (by history index) already inserted into the note. */
  inserted: ReadonlySet<number>;
  send: (text: string) => void;
  retry: () => void;
  dismissError: () => void;
  markInserted: (index: number) => void;
}

/** The composer's input, shared so the sheet can hand focus back to it. */
export type AskInputRef = RefObject<HTMLInputElement | null>;

/** One ephemeral chat per open note: switching notes starts a fresh one and
 *  drops any reply still on its way for the old note. */
export function useAskChat(noteId: string): AskChat {
  const [history, setHistory] = useState<AskMessage[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<AskChat["error"]>(null);
  const [inserted, setInserted] = useState<ReadonlySet<number>>(new Set());
  const seq = useRef(0);

  useEffect(() => {
    seq.current += 1;
    setHistory([]);
    setInput("");
    setBusy(false);
    setError(null);
    setInserted(new Set());
  }, [noteId]);

  const send = (text: string) => {
    const content = text.trim();
    if (!content || busy) return;
    const before = history;
    const next: AskMessage[] = [...before, { role: "user", content }];
    const mine = ++seq.current;
    setHistory(next);
    setBusy(true);
    setError(null);
    // Clear the composer only if it holds this question (not something
    // typed meanwhile, or when a quick prompt was clicked).
    setInput((cur) => (cur.trim() === content ? "" : cur));
    api
      .askMeeting(noteId, next)
      .then(
        (reply) => {
          if (mine !== seq.current) return;
          setHistory([...next, { role: "assistant", content: reply }]);
        },
        (e) => {
          if (mine !== seq.current) return;
          // Roll back the question (the backend needs the chat to end on a
          // user turn) and hand it back to the composer.
          setHistory(before);
          setInput((cur) => (cur.trim() ? cur : content));
          setError({ raw: String(e), question: content });
        },
      )
      .finally(() => {
        if (mine === seq.current) setBusy(false);
      });
  };

  return {
    history,
    input,
    setInput,
    busy,
    error,
    inserted,
    send,
    retry: () => {
      if (error) send(error.question);
    },
    dismissError: () => setError(null),
    markInserted: (i) => setInserted((s) => new Set(s).add(i)),
  };
}

/** The docked composer: focusing it opens the answer sheet. */
export function AskComposer({
  chat,
  inputRef,
  open,
  onOpen,
  placeholder,
}: {
  chat: AskChat;
  inputRef: AskInputRef;
  open: boolean;
  onOpen: () => void;
  placeholder: string;
}) {
  const [focused, setFocused] = useState(false);
  const canSend = chat.input.trim() !== "" && !chat.busy;
  return (
    <form
      aria-label="Ask"
      className="flex min-w-[240px] flex-1 items-center gap-2 rounded-full border py-1 pl-3.5 pr-1"
      style={{
        background: "var(--surface)",
        borderColor: focused || open ? "var(--primary-border)" : "var(--line)",
        boxShadow: focused ? "var(--focus-ring), var(--shadow-lg)" : "var(--shadow-lg)",
      }}
      onSubmit={(e) => {
        e.preventDefault();
        onOpen();
        chat.send(chat.input);
      }}
    >
      <Sparkles
        size={16}
        strokeWidth={1.75}
        aria-hidden="true"
        style={{ color: "var(--primary-text)", flex: "none" }}
      />
      <input
        ref={inputRef}
        value={chat.input}
        onChange={(e) => {
          chat.setInput(e.target.value);
          onOpen();
        }}
        // Tabbing in, a click or typing opens the sheet. Focus with no
        // relatedTarget is the sheet handing focus back as it closes (Esc,
        // ×, note switch), which must not reopen it; a click from the page
        // body also has none, so onClick covers that case.
        onFocus={(e) => {
          setFocused(true);
          if (e.relatedTarget) onOpen();
        }}
        onClick={onOpen}
        onBlur={() => setFocused(false)}
        placeholder={placeholder}
        aria-label={placeholder}
        className="min-w-0 flex-1 border-0 bg-transparent py-1.5 text-[14px] text-text outline-none placeholder:text-text-3"
        // the pill around it shows the focus ring
        style={{ boxShadow: "none" }}
      />
      <button
        type="submit"
        aria-label="Send"
        title="Send"
        disabled={!canSend}
        className="grid h-8 w-8 flex-none place-items-center rounded-full border-0"
        style={{
          background: canSend ? "var(--primary)" : "var(--surface-3)",
          color: canSend ? "var(--on-primary)" : "var(--text-3)",
          cursor: canSend ? "pointer" : "default",
        }}
      >
        <ArrowUp size={16} strokeWidth={2} />
      </button>
    </form>
  );
}

/** Ask for consent before a cloud provider first gets meeting text, as
 *  Settings does (same words, same setting); the backend enforces it too. */
async function confirmCloudProvider(llm: LlmSettings, id: string): Promise<boolean> {
  const info = llm.providers.find((p) => p.id === id);
  if (!info || info.is_local) return true;
  const key = `consent.cloud_llm.${id}`;
  try {
    if ((await api.getAppSetting(key)) === "true") return true;
  } catch {
    /* fall through to asking */
  }
  const ok = window.confirm(cloudConsentMessage(providerName(id)));
  if (ok) await api.setAppSetting(key, "true").catch(() => {});
  return ok;
}

/** "Runs on": every provider (not a local/cloud binary), plus the Claude
 *  model when Anthropic is picked. It changes the saved provider, which
 *  Enhance, transcript cleanup and action items use too. */
function RunsOn({
  llm,
  onChange,
  disabled,
  keptLocal,
}: {
  llm: LlmSettings;
  onChange: (next: LlmSettings) => void;
  disabled: boolean;
  keptLocal: boolean;
}) {
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const selectId = useId();
  const current = llm.providers.find((p) => p.id === llm.provider) ?? null;
  const cloud = !!current && !current.is_local;
  const claudeModel = current?.model || current?.default_model || CLAUDE_MODELS[0].value;
  const claudeOptions = CLAUDE_MODELS.some((m) => m.value === claudeModel)
    ? CLAUDE_MODELS
    : [...CLAUDE_MODELS, { value: claudeModel, label: claudeModel }];

  const pickProvider = async (id: string) => {
    setMessage(null);
    if (!(await confirmCloudProvider(llm, id))) {
      setMessage("Kept your current provider. Nothing was sent anywhere.");
      return;
    }
    const info = llm.providers.find((p) => p.id === id);
    setSaving(true);
    try {
      await api.setLlmSettings({
        provider: id,
        model: info?.model ?? null,
        base_url: info?.base_url ?? null,
        api_key: null,
      });
      onChange({ ...llm, provider: id });
    } catch (e) {
      setMessage(`Couldn't switch: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const pickClaudeModel = async (model: string) => {
    setMessage(null);
    if (!(await confirmCloudProvider(llm, "anthropic"))) return;
    const info = llm.providers.find((p) => p.id === "anthropic");
    setSaving(true);
    try {
      await api.setLlmSettings({
        provider: "anthropic",
        model,
        base_url: info?.base_url ?? null,
        api_key: null,
      });
      onChange({
        ...llm,
        providers: llm.providers.map((p) => (p.id === "anthropic" ? { ...p, model } : p)),
      });
    } catch (e) {
      setMessage(`Couldn't switch: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const warning =
    cloud && keptLocal
      ? `This note is kept local, so Ask won't send it to ${providerName(llm.provider)}.`
      : cloud && current && !current.has_key
        ? `No API key for ${providerName(llm.provider)} yet. Add one in Settings › Ask and Enhance.`
        : null;

  return (
    <div className="flex min-w-0 flex-col gap-1">
      <div className="flex flex-wrap items-center gap-2">
        <span
          aria-hidden="true"
          className="h-2 w-2 flex-none rounded-full"
          style={{ background: cloud ? "var(--warning)" : "var(--success)" }}
        />
        <label htmlFor={selectId} className="text-[12px] font-semibold text-text-2">
          Runs on
        </label>
        <Select
          id={selectId}
          value={llm.provider}
          disabled={disabled || saving}
          title="Also used for Enhance, transcript cleanup and action items"
          onChange={(e) => void pickProvider(e.target.value)}
          options={llm.providers.map((p) => ({ value: p.id, label: providerLabel(p.id) }))}
          className="!py-1 !pl-2.5 !text-[12.5px]"
          style={{ width: "auto" }}
        />
        {llm.provider === "anthropic" ? (
          <Select
            aria-label="Claude model"
            value={claudeModel}
            disabled={disabled || saving}
            onChange={(e) => void pickClaudeModel(e.target.value)}
            options={claudeOptions}
            className="!py-1 !pl-2.5 !text-[12.5px]"
            style={{ width: "auto" }}
          />
        ) : current ? (
          <span className="font-mono text-[11.5px]" style={{ color: "var(--text-3)" }}>
            {current.model || current.default_model}
          </span>
        ) : null}
      </div>
      {message && (
        <span className="text-[12px]" style={{ color: "var(--text-3)" }} role="status">
          {message}
        </span>
      )}
      {warning && (
        <span className="text-[12px]" style={{ color: "var(--warning-text)" }}>
          {warning}
        </span>
      )}
    </div>
  );
}

function Thinking({ label }: { label: string }) {
  return (
    <div
      className="flex items-center gap-2 px-1 text-[13px] font-medium"
      style={{ color: "var(--primary-soft-text)" }}
      role="status"
    >
      <span className="flex items-center gap-1" aria-hidden="true">
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
      {label}
    </div>
  );
}

/** The answer sheet: over the note above the dock, or pinned to its side. */
export function AskSheet({
  chat,
  inputRef,
  hasMeeting,
  keptLocal,
  pinned,
  canPin,
  onTogglePin,
  onClose,
  onInsert,
}: {
  chat: AskChat;
  inputRef: AskInputRef;
  hasMeeting: boolean;
  keptLocal: boolean;
  pinned: boolean;
  /** Pinning is offered on windows at least PIN_MIN_WIDTH wide. */
  canPin: boolean;
  onTogglePin: () => void;
  onClose: () => void;
  /** Insert an answer into the note (today: appended to your notes). */
  onInsert: (content: string) => void;
}) {
  const [llm, setLlm] = useState<LlmSettings | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    api
      .getLlmSettings()
      .then((s) => live && setLlm(s))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);

  // Keep the newest message in view (scroll the sheet body only).
  const { history, busy, error } = chat;
  useEffect(() => {
    const body = endRef.current?.closest(".fly-sheet-body");
    if (body) body.scrollTop = body.scrollHeight;
  }, [history.length, busy, error]);

  const what = hasMeeting ? "Ask this meeting" : "Ask this note";
  // Buttons that vanish when clicked (Insert, Try again, Dismiss) hand focus
  // back to the composer instead of dropping it on the page.
  const thenFocusComposer = (action: () => void) => () => {
    inputRef.current?.focus();
    action();
  };
  const lastAnswer = history.map((m) => m.role).lastIndexOf("assistant");
  const errorView = error ? describeAskError(error.raw, llm?.provider ?? "ollama") : null;

  return (
    <Sheet
      open
      onClose={onClose}
      side={pinned ? "right" : "bottom"}
      autoFocus={false}
      aria-label={what}
      data-testid="ask-sheet"
      className={
        pinned ? "fly-ask-sheet fly-ask-sheet--pinned print:hidden" : "fly-ask-sheet print:hidden"
      }
      title={
        <span className="flex flex-wrap items-baseline gap-x-2.5 gap-y-0.5">
          <span className="inline-flex items-center gap-1.5 text-[14px]">
            <Sparkles
              size={15}
              strokeWidth={1.75}
              aria-hidden="true"
              style={{ color: "var(--primary-text)" }}
            />
            {what}
          </span>
          <span className="font-sans text-[11.5px] font-normal" style={{ color: "var(--text-3)" }}>
            Not saved · clears when you leave this note
          </span>
        </span>
      }
      footer={
        <div className="flex w-full flex-wrap items-center gap-2">
          <div className="mr-auto min-w-0">
            {llm && <RunsOn llm={llm} onChange={setLlm} disabled={busy} keptLocal={keptLocal} />}
          </div>
          {canPin && (
            <Button
              variant="ghost"
              size="xs"
              onClick={onTogglePin}
              aria-pressed={pinned}
              title={pinned ? "Show answers over the note" : "Pin answers beside the note"}
              startIcon={
                pinned ? (
                  <PanelRightClose size={14} strokeWidth={1.75} />
                ) : (
                  <PanelRightOpen size={14} strokeWidth={1.75} />
                )
              }
            >
              {pinned ? "Unpin" : "Pin to side"}
            </Button>
          )}
        </div>
      }
    >
      {history.length === 0 && !busy && (
        <div className="flex flex-col gap-2">
          <p className="m-0 text-[12.5px]" style={{ color: "var(--text-3)" }}>
            {hasMeeting
              ? "Answers come from this meeting's transcript and your notes."
              : "Answers come from your notes."}
          </p>
          {QUICK_PROMPTS.map((q) => (
            <button
              key={q}
              type="button"
              onClick={() => {
                // the prompt list goes away: keep focus in the composer
                inputRef.current?.focus();
                chat.send(q);
              }}
              className="flex cursor-pointer items-center gap-2.5 rounded-[10px] border border-line px-3 py-2 text-left text-[13.5px] text-text hover:bg-surface-3"
              style={{ background: "var(--surface-2)", fontFamily: "var(--font-sans)" }}
            >
              <span aria-hidden="true" style={{ color: "var(--text-3)", display: "flex" }}>
                {PROMPT_ICONS[q]}
              </span>
              {q}
            </button>
          ))}
        </div>
      )}

      {history.map((m, i) =>
        m.role === "user" ? (
          <div key={i} className="mb-3 flex justify-end">
            <span
              className="max-w-[80%] whitespace-pre-wrap rounded-[14px] px-3.5 py-2 text-[14px] text-text"
              style={{ background: "var(--surface-2)" }}
            >
              {m.content}
            </span>
          </div>
        ) : (
          <div key={i} className="mb-4 flex flex-col gap-2" data-testid="ask-answer">
            <div className="text-[14.5px] leading-[1.6] text-text [&_a]:text-primary-text [&_a]:underline [&_li]:ml-4 [&_li]:list-disc [&_ol>li]:list-decimal [&_p]:my-1">
              <ReactMarkdown remarkPlugins={[remarkGfm]} components={ASK_MD}>
                {m.content}
              </ReactMarkdown>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              {chat.inserted.has(i) ? (
                <span
                  className="px-1 text-[12px] font-semibold"
                  style={{ color: "var(--success-text)" }}
                >
                  ✓ Inserted into your notes
                </span>
              ) : (
                <Button
                  variant="soft"
                  size="xs"
                  title="Add this answer to the end of your notes"
                  onClick={thenFocusComposer(() => {
                    onInsert(m.content);
                    chat.markInserted(i);
                  })}
                  style={
                    // The newest answer breathes gently so it's clear this is
                    // how you keep it; earlier answers sit quiet.
                    i === lastAnswer && !busy
                      ? { animation: "fly-pulse-glow 2s ease-in-out infinite" }
                      : undefined
                  }
                >
                  Insert into note
                </Button>
              )}
              {i === lastAnswer && !chat.inserted.has(i) && (
                <span className="text-[12px]" style={{ color: "var(--text-3)" }}>
                  Ask only changes your note when you click this.
                </span>
              )}
            </div>
          </div>
        ),
      )}

      {busy && <Thinking label={hasMeeting ? "Reading the transcript…" : "Reading your notes…"} />}

      {errorView && (
        <Notice
          tone="danger"
          icon={<TriangleAlert size={16} strokeWidth={1.75} />}
          title={errorView.title}
          details={errorView.raw}
          className="!shadow-none"
          data-testid="ask-error"
          actions={
            <>
              <Button variant="outline" size="xs" onClick={thenFocusComposer(chat.retry)}>
                Try again
              </Button>
              <Button variant="ghost" size="xs" onClick={thenFocusComposer(chat.dismissError)}>
                Dismiss
              </Button>
            </>
          }
        >
          {errorView.fix}
        </Notice>
      )}
      <div ref={endRef} />
    </Sheet>
  );
}
