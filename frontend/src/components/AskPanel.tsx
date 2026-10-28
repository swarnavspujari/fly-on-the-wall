import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
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
  RUNS_ON_HELPER,
  cloudConsentMessage,
  describeAskError,
  providerName,
  runsOnPlace,
  runsOnSwitched,
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

/** The composer grows with what you type, up to this height, then scrolls. */
const COMPOSER_MAX_PX = 132;

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
  /** The last failure, the question it was for (to retry) and the provider
   *  it went to (read when it was asked). */
  error: { raw: string; question: string; provider: string } | null;
  /** Answers (by history index) already inserted into the note. */
  inserted: ReadonlySet<number>;
  send: (text: string) => void;
  retry: () => void;
  dismissError: () => void;
  markInserted: (index: number) => void;
  /** The SAVED LLM settings. Re-read when the sheet opens, before each
   *  question and before Runs on writes, so a change made in Settings is
   *  never undone from a stale copy. */
  llm: LlmSettings | null;
  setLlm: (next: LlmSettings) => void;
  refreshLlm: () => Promise<LlmSettings | null>;
  /** The answer sheet's id (the composer's aria-controls). */
  sheetId: string;
  /** Polite live-region text: an answer arrived, or Ask couldn't answer. */
  announcement: string;
}

/** The composer's input, shared so the sheet can hand focus back to it. */
export type AskInputRef = RefObject<HTMLTextAreaElement | null>;

/** One ephemeral chat per open note: switching notes starts a fresh one and
 *  drops any reply still on its way for the old note. */
export function useAskChat(noteId: string): AskChat {
  const [history, setHistory] = useState<AskMessage[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<AskChat["error"]>(null);
  const [inserted, setInserted] = useState<ReadonlySet<number>>(new Set());
  const [llm, setLlm] = useState<LlmSettings | null>(null);
  const [announcement, setAnnouncement] = useState("");
  const sheetId = useId();
  const seq = useRef(0);
  // A question is on its way: guards double sends inside one render (two
  // Enters, Enter + a quick prompt) that the `busy` state can't see yet.
  const inFlight = useRef(false);

  useEffect(() => {
    seq.current += 1;
    inFlight.current = false;
    setHistory([]);
    setInput("");
    setBusy(false);
    setError(null);
    setInserted(new Set());
    setAnnouncement("");
  }, [noteId]);

  const refreshLlm = useCallback(async () => {
    try {
      const s = await api.getLlmSettings();
      setLlm(s);
      return s;
    } catch {
      return null;
    }
  }, []);

  const send = (text: string) => {
    const content = text.trim();
    if (!content || inFlight.current) return;
    inFlight.current = true;
    const before = history;
    const next: AskMessage[] = [...before, { role: "user", content }];
    const mine = ++seq.current;
    const providerAtSend = llm?.provider ?? "ollama";
    // Re-read the saved provider alongside the question: Runs on stays
    // current, and an error names the provider the question went to.
    const settings = refreshLlm();
    setHistory(next);
    setBusy(true);
    setError(null);
    setAnnouncement("");
    // Clear the composer only if it holds this question (not something
    // typed meanwhile, or when a quick prompt was clicked).
    setInput((cur) => (cur.trim() === content ? "" : cur));
    api
      .askMeeting(noteId, next)
      .then(
        (reply) => {
          if (mine !== seq.current) return;
          setHistory([...next, { role: "assistant", content: reply }]);
          const n = next.filter((m) => m.role === "user").length;
          setAnnouncement(`Answer ${n} is ready.`);
        },
        async (e) => {
          const s = await settings;
          if (mine !== seq.current) return;
          // Roll back the question (the backend needs the chat to end on a
          // user turn) and hand it back to the composer.
          setHistory(before);
          setInput((cur) => (cur.trim() ? cur : content));
          setError({ raw: String(e), question: content, provider: s?.provider ?? providerAtSend });
          setAnnouncement("Ask couldn't answer. The reason is in the answer sheet.");
        },
      )
      .finally(() => {
        if (mine !== seq.current) return;
        inFlight.current = false;
        setBusy(false);
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
    llm,
    setLlm,
    refreshLlm,
    sheetId,
    announcement,
  };
}

/** The docked composer. A click, typing or Enter opens the answer sheet
 *  (focus alone doesn't, so tabbing past it or getting focus back when the
 *  sheet closes never reopens it). Enter sends; Shift+Enter adds a line. */
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
  const submit = () => {
    onOpen();
    chat.send(chat.input);
  };

  // Grow with the text (one line when empty), up to COMPOSER_MAX_PX.
  useLayoutEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, COMPOSER_MAX_PX)}px`;
    // scroll only once it's at full height (no stray scrollbar on one line)
    el.style.overflowY = el.scrollHeight > COMPOSER_MAX_PX ? "auto" : "hidden";
  }, [chat.input, inputRef]);

  return (
    <form
      aria-label="Ask"
      className="flex min-w-[240px] flex-1 items-end gap-2 rounded-[22px] border py-1 pl-3.5 pr-1"
      style={{
        background: "var(--surface)",
        borderColor: focused || open ? "var(--primary-border)" : "var(--line)",
        boxShadow: focused ? "var(--focus-ring), var(--shadow-lg)" : "var(--shadow-lg)",
      }}
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <Sparkles
        size={16}
        strokeWidth={1.75}
        aria-hidden="true"
        style={{ color: "var(--primary-text)", flex: "none", marginBottom: 9 }}
      />
      <textarea
        ref={inputRef}
        rows={1}
        value={chat.input}
        onChange={(e) => {
          chat.setInput(e.target.value);
          onOpen();
        }}
        onClick={onOpen}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            submit();
          }
        }}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        placeholder={placeholder}
        aria-label={placeholder}
        // The sheet this composer fills. (aria-expanded isn't valid on a
        // text box; the sheet announces itself as a labelled dialog.)
        aria-controls={open ? chat.sheetId : undefined}
        className="block min-w-0 flex-1 resize-none overflow-hidden border-0 bg-transparent py-1.5 text-[14px] leading-[1.45] text-text outline-none placeholder:text-text-3"
        // the pill around it shows the focus ring
        style={{ boxShadow: "none", maxHeight: COMPOSER_MAX_PX }}
      />
      <button
        type="submit"
        aria-label="Send"
        title="Send (Enter). Shift+Enter adds a line."
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
      <span className="sr-only" aria-live="polite" data-testid="ask-live">
        {chat.announcement}
      </span>
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

const modelLabel = (model: string) => CLAUDE_MODELS.find((m) => m.value === model)?.label ?? model;

/** "Runs on": every provider (not a local/cloud binary), plus the Claude
 *  model when Anthropic is picked. It changes the SAVED provider, which
 *  Enhance, transcript cleanup and action items use too, so it says so, and
 *  it re-reads the saved settings before every write. */
function RunsOn({
  chat,
  disabled,
  keptLocal,
}: {
  chat: AskChat;
  disabled: boolean;
  keptLocal: boolean;
}) {
  const llm = chat.llm!;
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<{ text: string; tone: "info" | "error" } | null>(null);
  const selectId = useId();
  const helperId = useId();
  const warningId = useId();
  const current = llm.providers.find((p) => p.id === llm.provider) ?? null;
  const place = current
    ? runsOnPlace(current)
    : { kind: "cloud" as const, label: providerName(llm.provider) };
  const claudeModel = current?.model || current?.default_model || CLAUDE_MODELS[0].value;
  const claudeOptions = CLAUDE_MODELS.some((m) => m.value === claudeModel)
    ? CLAUDE_MODELS
    : [...CLAUDE_MODELS, { value: claudeModel, label: claudeModel }];

  const write = async (
    fresh: LlmSettings,
    provider: string,
    model: string | null,
    done: (next: LlmSettings) => string,
  ) => {
    const info = fresh.providers.find((p) => p.id === provider);
    setSaving(true);
    try {
      await api.setLlmSettings({
        provider,
        model,
        base_url: info?.base_url ?? null,
        api_key: null,
      });
      const next: LlmSettings = {
        ...fresh,
        provider,
        providers: fresh.providers.map((p) => (p.id === provider ? { ...p, model } : p)),
      };
      chat.setLlm(next);
      setMessage({ text: done(next), tone: "info" });
    } catch (e) {
      setMessage({ text: `Couldn't switch, so nothing changed. (${String(e)})`, tone: "error" });
      void chat.refreshLlm();
    } finally {
      setSaving(false);
    }
  };

  // A pick writes over the saved provider, so it needs the saved settings as
  // they are now; if they can't be read, nothing is written.
  const readFresh = async () => {
    const fresh = await chat.refreshLlm();
    if (!fresh) {
      setMessage({
        text: "Couldn't read your current provider, so nothing changed.",
        tone: "error",
      });
    }
    return fresh;
  };

  const pickProvider = async (id: string) => {
    setMessage(null);
    const fresh = await readFresh();
    if (!fresh || fresh.provider === id) return;
    if (!(await confirmCloudProvider(fresh, id))) {
      setMessage({ text: "Kept your current provider. Nothing was sent anywhere.", tone: "info" });
      return;
    }
    const info = fresh.providers.find((p) => p.id === id);
    await write(fresh, id, info?.model ?? null, () => runsOnSwitched(providerLabel(id)));
  };

  const pickClaudeModel = async (model: string) => {
    setMessage(null);
    const fresh = await readFresh();
    if (!fresh) return;
    // Picking a model never switches the provider behind your back: if
    // Settings moved off Anthropic meanwhile, the select now shows that, and
    // switching back is a question.
    const switching = fresh.provider !== "anthropic";
    if (
      switching &&
      !window.confirm(
        `Ask now runs on ${providerLabel(fresh.provider)} (changed in Settings).\n\n` +
          `Switch Ask, Enhance, transcript cleanup and action items to ` +
          `${providerLabel("anthropic")} with ${modelLabel(model)}?`,
      )
    ) {
      setMessage({ text: `Kept ${providerLabel(fresh.provider)}.`, tone: "info" });
      return;
    }
    if (!(await confirmCloudProvider(fresh, "anthropic"))) {
      setMessage({ text: "Kept your current setting. Nothing was sent anywhere.", tone: "info" });
      return;
    }
    await write(fresh, "anthropic", model, () =>
      switching
        ? runsOnSwitched(`${providerLabel("anthropic")} · ${modelLabel(model)}`)
        : `Ask, Enhance, transcript cleanup and action items now use ${modelLabel(model)}.`,
    );
  };

  const warning =
    keptLocal && place.kind !== "local"
      ? place.kind === "remote"
        ? "This note is kept local, so Ask won't send it to another computer."
        : `This note is kept local, so Ask won't send it to ${providerName(llm.provider)}.`
      : place.kind === "cloud" && current && !current.has_key
        ? `No API key for ${providerName(llm.provider)} yet. Add one in Settings › Ask and Enhance.`
        : null;
  const describedBy = warning ? `${helperId} ${warningId}` : helperId;
  const refreshOnFocus = () => void chat.refreshLlm();

  return (
    <div className="flex min-w-0 flex-col gap-1">
      <div className="flex flex-wrap items-center gap-2">
        <span
          aria-hidden="true"
          className="h-2 w-2 flex-none rounded-full"
          style={{ background: place.kind === "local" ? "var(--success)" : "var(--warning)" }}
        />
        <label htmlFor={selectId} className="text-[12px] font-semibold text-text-2">
          Runs on
        </label>
        <Select
          id={selectId}
          value={llm.provider}
          disabled={disabled || saving}
          aria-describedby={describedBy}
          onFocus={refreshOnFocus}
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
            aria-describedby={describedBy}
            onFocus={refreshOnFocus}
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
        {place.kind === "remote" && (
          <span className="text-[12px]" style={{ color: "var(--warning-text)" }}>
            {place.label}
          </span>
        )}
      </div>
      <span id={helperId} className="text-[11.5px]" style={{ color: "var(--text-3)" }}>
        {RUNS_ON_HELPER}
      </span>
      {message && (
        <span
          className="text-[12px]"
          style={{ color: message.tone === "error" ? "var(--error-text)" : "var(--text-2)" }}
          role="status"
        >
          {message.text}
        </span>
      )}
      {warning && (
        <span id={warningId} className="text-[12px]" style={{ color: "var(--warning-text)" }}>
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
  const endRef = useRef<HTMLDivElement>(null);
  const { refreshLlm } = chat;

  // The saved provider may have changed in Settings since last time.
  useEffect(() => {
    void refreshLlm();
  }, [refreshLlm]);

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
  const errorView = error ? describeAskError(error.raw, error.provider) : null;

  // "Isn't confirmed yet": ask for consent here, then ask the question again.
  const allowAndRetry = async () => {
    if (!error) return;
    const fresh = await chat.refreshLlm();
    if (fresh && (await confirmCloudProvider(fresh, error.provider))) chat.retry();
    inputRef.current?.focus();
  };

  return (
    <Sheet
      open
      onClose={onClose}
      side={pinned ? "right" : "bottom"}
      autoFocus={false}
      restoreFocus="inside"
      id={chat.sheetId}
      aria-label={what}
      // Back from Settings with the sheet left open: re-read Runs on.
      onPointerEnter={() => void refreshLlm()}
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
            {chat.llm && <RunsOn chat={chat} disabled={busy} keptLocal={keptLocal} />}
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
              {errorView.action === "consent" ? (
                <Button variant="outline" size="xs" onClick={() => void allowAndRetry()}>
                  Review and allow {providerName(error!.provider)}
                </Button>
              ) : (
                <Button variant="outline" size="xs" onClick={thenFocusComposer(chat.retry)}>
                  Try again
                </Button>
              )}
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
