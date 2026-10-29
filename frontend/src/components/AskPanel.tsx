import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ReactNode, RefObject } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  ArrowUp,
  CheckCheck,
  ChevronDown,
  CircleHelp,
  ListTodo,
  Mail,
  PanelRightClose,
  PanelRightOpen,
  Sparkles,
  Square,
  TriangleAlert,
  X,
} from "lucide-react";
import type {
  AskEventPayload,
  AskMessage,
  AskReply,
  AskSource,
  AskStage,
  AskStreamEvent,
  LlmSettings,
  Note,
} from "../types";
import { api, ASK_EVENT, LLM_SETTINGS_CHANGED_EVENT } from "../api";
import {
  CLAUDE_MODELS,
  QUICK_PROMPTS,
  RUNS_ON_HELPER,
  describeAskError,
  isCancelled,
  providerName,
  runsOnPlace,
  runsOnSwitched,
} from "../askModel";
import { citedIds } from "../askCitations";
import {
  ALL_SCOPE,
  mentionAt,
  mentionOptions,
  mentionScope,
  removeMention,
  sameScope,
  thisScope,
} from "../askScope";
import type { MentionCatalog, MentionOption, ScopeChoice } from "../askScope";
import { providerLabel } from "../settingsModel";
import { useCloudConsent } from "./CloudConsent";
import { useConfirm } from "./confirm";
import { Button, Notice, Select, Sheet, useEscapeLayer } from "./ui";
import { AskTurnView } from "./AskAnswer";
import type { OpenSource, ProposalState } from "./AskAnswer";

/* Ask over the Ask harness (fly-ask): a composer docked by the view switcher
   (or on Home, for every meeting), answers in a sheet over the note (or
   pinned beside it on wide windows). Answers stream in, then show as checked
   sentences with citation chips; the chat is ephemeral, and nothing reaches
   a note until you click. Every question goes through `ask_meeting`, so
   `build_provider`'s cloud consent and Keep local apply as before; Runs on
   only changes the saved provider, the same setting Settings › Ask and
   Enhance writes. */

const PROMPT_ICONS: Record<string, ReactNode> = {
  "What did I miss?": <CircleHelp size={15} strokeWidth={1.75} />,
  "What decisions were made?": <CheckCheck size={15} strokeWidth={1.75} />,
  "List the action items": <ListTodo size={15} strokeWidth={1.75} />,
  "Draft a follow-up email": <Mail size={15} strokeWidth={1.75} />,
  "What are my open action items?": <ListTodo size={15} strokeWidth={1.75} />,
  "What was decided this week?": <CheckCheck size={15} strokeWidth={1.75} />,
  "What did I miss last week?": <CircleHelp size={15} strokeWidth={1.75} />,
};

/** Quick prompts for Ask over every meeting (global Ask). */
export const GLOBAL_PROMPTS = [
  "What are my open action items?",
  "What was decided this week?",
  "What did I miss last week?",
] as const;

/** The sheet can pin beside the note from this width up. */
export const PIN_MIN_WIDTH = 1440;

/** The composer grows with what you type, up to this height, then scrolls. */
const COMPOSER_MAX_PX = 132;

/** Window event that opens Ask from anywhere (the sidebar entry, Ctrl/Cmd+J):
 *  the open note's Ask, or Ask over every meeting on Home. `detail.scope`
 *  "all" starts it on All meetings. */
export const OPEN_ASK_EVENT = "fotw:open-ask";
export type OpenAskDetail = { scope?: "all" };
export const openAsk = (detail: OpenAskDetail = {}) =>
  window.dispatchEvent(new CustomEvent<OpenAskDetail>(OPEN_ASK_EVENT, { detail }));

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

/** One question and what came back. `id` is the ask id: the stream
 *  (`ask:event`), Stop and connector cards key on it. */
export interface AskTurn {
  id: string;
  question: string;
  scope: ScopeChoice;
  /** running: streaming; done: `reply` is set; stopped: you pressed Stop. */
  phase: "running" | "done" | "stopped";
  stage: AskStage | null;
  /** The reply so far (sanitized, no citations yet). */
  frame: string;
  frameTurn: number | null;
  /** What the prefetch found, the moment it finished. */
  found: { sources: AskSource[]; keptLocalNotSearched: boolean } | null;
  reply: AskReply | null;
  /** The saved provider and model when it was sent (the run log names no
   *  model). */
  provider: string;
  model: string | null;
}

/** Apply one streamed event to its turn. Frames are whole texts, not
 *  deltas; a new model turn supersedes the last one's frames, and turn 0 is
 *  the final, validated answer. */
export function applyAskEvent(turn: AskTurn, ev: AskStreamEvent): AskTurn {
  switch (ev.type) {
    case "stage":
      return { ...turn, stage: ev.stage };
    case "sources":
      return {
        ...turn,
        found: { sources: ev.sources, keptLocalNotSearched: ev.kept_local_not_searched > 0 },
      };
    case "turn_start":
      return { ...turn, frameTurn: ev.turn, frame: "" };
    case "frame":
      if (ev.turn === 0 || turn.frameTurn === null || ev.turn >= turn.frameTurn) {
        return { ...turn, frame: ev.text, frameTurn: ev.turn === 0 ? turn.frameTurn : ev.turn };
      }
      return turn;
    default:
      return turn;
  }
}

/** The chat as `ask_meeting` takes it: earlier answered turns (with where
 *  each ran and what it cited), then the question. */
export function askHistory(turns: AskTurn[], question: string): AskMessage[] {
  const out: AskMessage[] = [];
  for (const t of turns) {
    if (t.phase !== "done" || !t.reply) continue;
    out.push({ role: "user", content: t.question });
    out.push({
      role: "assistant",
      content: t.reply.text,
      on_device: t.reply.on_device,
      citations: citedIds(t.reply.sentences),
    });
  }
  out.push({ role: "user", content: question });
  return out;
}

let askSeq = 0;
const newAskId = () =>
  typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `ask-${Date.now()}-${++askSeq}`;

export interface AskChat {
  /** The note Ask was opened from; null = Ask over every meeting (Home). */
  noteId: string | null;
  turns: AskTurn[];
  input: string;
  setInput: (value: string) => void;
  busy: boolean;
  /** The last failure, the question and scope it was for (to retry) and the
   *  provider it went to (read when it was asked). */
  error: { raw: string; question: string; scope: ScopeChoice; provider: string } | null;
  /** Turns (by id) whose answer was already inserted into the note. */
  inserted: ReadonlySet<string>;
  /** Proposal approvals (by proposal id), kept while the sheet is closed so
   *  one can never be approved twice. */
  proposals: ReadonlyMap<string, ProposalState>;
  setProposal: (id: string, state: ProposalState) => void;
  /** What the next question searches. */
  scope: ScopeChoice;
  /** The scope Ask starts with here (this note, or every meeting). */
  defaultScope: ScopeChoice;
  /** null = back to the default. */
  setScope: (scope: ScopeChoice | null) => void;
  send: (text: string, scope?: ScopeChoice) => void;
  /** Stop the answer being written (and ask the backend to stop it). */
  stop: () => void;
  retry: () => void;
  dismissError: () => void;
  markInserted: (turnId: string) => void;
  /** The SAVED LLM settings. Re-read when the sheet opens, before each
   *  question and before Runs on writes, so a change made in Settings is
   *  never undone from a stale copy. */
  llm: LlmSettings | null;
  /** After a Runs on write: show `next`, and ignore any re-read that started
   *  before it. */
  setLlm: (next: LlmSettings) => void;
  refreshLlm: () => Promise<LlmSettings | null>;
  /** The answer sheet's id (the composer's aria-controls). */
  sheetId: string;
  /** Polite live-region text: an answer arrived, or Ask couldn't answer. */
  announcement: string;
}

/** The composer's input, shared so the sheet can hand focus back to it. */
export type AskInputRef = RefObject<HTMLTextAreaElement | null>;

/** One ephemeral chat per open note (or one for Home, `noteId` null):
 *  switching notes starts a fresh one, stops any answer still being written
 *  for the old note and drops its reply. */
export function useAskChat(noteId: string | null, { hasMeeting = true } = {}): AskChat {
  const [turns, setTurns] = useState<AskTurn[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<AskChat["error"]>(null);
  const [inserted, setInserted] = useState<ReadonlySet<string>>(new Set());
  const [proposals, setProposals] = useState<ReadonlyMap<string, ProposalState>>(new Map());
  const [scopePick, setScopePick] = useState<ScopeChoice | null>(null);
  const [llm, setLlm] = useState<LlmSettings | null>(null);
  const [announcement, setAnnouncement] = useState("");
  const sheetId = useId();
  const seq = useRef(0);
  // A question is on its way: guards double sends inside one render (two
  // Enters, Enter + a quick prompt) that the `busy` state can't see yet.
  const inFlight = useRef(false);
  // The ask id being written, so a note switch or Stop can cancel it.
  const running = useRef<string | null>(null);

  const cancelRunning = useCallback(() => {
    const id = running.current;
    running.current = null;
    if (id) void api.cancelAsk(id).catch(() => {});
  }, []);

  useEffect(() => {
    seq.current += 1;
    inFlight.current = false;
    setTurns([]);
    setInput("");
    setBusy(false);
    setError(null);
    setInserted(new Set());
    setProposals(new Map());
    setScopePick(null);
    setAnnouncement("");
    return cancelRunning;
  }, [noteId, cancelRunning]);

  // The stream: route each event to the turn it belongs to.
  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | null = null;
    listen<AskEventPayload>(ASK_EVENT, (e) => {
      if (!live) return;
      const { ask_id, event } = e.payload;
      setTurns((ts) =>
        ts.some((t) => t.id === ask_id && t.phase === "running")
          ? ts.map((t) => (t.id === ask_id ? applyAskEvent(t, event) : t))
          : ts,
      );
    })
      .then((un) => {
        if (live) unlisten = un;
        else un();
      })
      .catch(() => {});
    return () => {
      live = false;
      unlisten?.();
    };
  }, []);

  // Bumped by every Runs on write: a re-read that started before the last
  // write is older than what we already show, so it isn't applied.
  const writes = useRef(0);
  const refreshLlm = useCallback(async () => {
    const at = writes.current;
    try {
      const s = await api.getLlmSettings();
      if (writes.current === at) setLlm(s);
      return s;
    } catch {
      return null;
    }
  }, []);
  const setLlmAfterWrite = useCallback((next: LlmSettings) => {
    writes.current += 1;
    setLlm(next);
  }, []);

  const defaultScope = useMemo(
    () => (noteId ? thisScope(noteId, hasMeeting) : ALL_SCOPE),
    [noteId, hasMeeting],
  );
  const scope = scopePick ?? defaultScope;

  const send = (text: string, scopeArg?: ScopeChoice) => {
    const content = text.trim();
    if (!content || inFlight.current) return;
    inFlight.current = true;
    const asked = scopeArg ?? scope;
    const history = askHistory(turns, content);
    const mine = ++seq.current;
    const id = newAskId();
    running.current = id;
    const current = llm?.providers.find((p) => p.id === llm.provider);
    const providerAtSend = llm?.provider ?? "ollama";
    // Re-read the saved provider alongside the question: Runs on stays
    // current, and an error names the provider the question went to.
    const settings = refreshLlm();
    setTurns((ts) => [
      ...ts,
      {
        id,
        question: content,
        scope: asked,
        phase: "running",
        stage: null,
        frame: "",
        frameTurn: null,
        found: null,
        reply: null,
        provider: providerAtSend,
        model: current ? current.model || current.default_model : null,
      },
    ]);
    setBusy(true);
    setError(null);
    setAnnouncement("");
    // Clear the composer only if it holds this question (not something
    // typed meanwhile, or when a quick prompt was clicked).
    setInput((cur) => (cur.trim() === content ? "" : cur));
    api
      .askMeeting(noteId ?? "", history, asked.scope, id)
      .then(
        (reply) => {
          if (mine !== seq.current) return;
          // Keep where the answer ran and what it cited: the backend never
          // sends an on-device answer to an off-device model next turn.
          setTurns((ts) => ts.map((t) => (t.id === id ? { ...t, phase: "done", reply } : t)));
          const n = turns.filter((t) => t.phase === "done").length + 1;
          setAnnouncement(`Answer ${n} is ready.`);
        },
        async (e) => {
          const s = await settings;
          if (mine !== seq.current) return;
          if (isCancelled(e)) {
            setTurns((ts) => ts.map((t) => (t.id === id ? { ...t, phase: "stopped" } : t)));
            return;
          }
          // Drop the question (the backend needs the chat to end on a user
          // turn) and hand it back to the composer.
          setTurns((ts) => ts.filter((t) => t.id !== id));
          setInput((cur) => (cur.trim() ? cur : content));
          setError({
            raw: String(e),
            question: content,
            scope: asked,
            provider: s?.provider ?? providerAtSend,
          });
          setAnnouncement("Ask couldn't answer. The reason is in the answer sheet.");
        },
      )
      .finally(() => {
        if (mine !== seq.current) return;
        if (running.current === id) running.current = null;
        inFlight.current = false;
        setBusy(false);
      });
  };

  const stop = () => {
    const id = running.current;
    if (!id) return;
    // The reply (or the backend's "cancelled") is dropped when it lands.
    seq.current += 1;
    cancelRunning();
    inFlight.current = false;
    setBusy(false);
    setTurns((ts) => ts.map((t) => (t.id === id ? { ...t, phase: "stopped" } : t)));
    setAnnouncement("Stopped. Nothing from that answer was kept.");
  };

  return {
    noteId,
    turns,
    input,
    setInput,
    busy,
    error,
    inserted,
    proposals,
    setProposal: (pid, state) =>
      setProposals((m) => {
        const next = new Map(m);
        next.set(pid, state);
        return next;
      }),
    scope,
    defaultScope,
    setScope: setScopePick,
    send,
    stop,
    retry: () => {
      if (error) send(error.question, error.scope);
    },
    dismissError: () => setError(null),
    markInserted: (tid) => setInserted((s) => new Set(s).add(tid)),
    llm,
    setLlm: setLlmAfterWrite,
    refreshLlm,
    sheetId,
    announcement,
  };
}

/** What @ can pick, read once when you first type @: recent meetings and
 *  folders from the backend, people from what's on screen. */
function useMentionCatalog(people: string[]): {
  catalog: MentionCatalog;
  load: () => void;
} {
  const [loaded, setLoaded] = useState<Omit<MentionCatalog, "people"> | null>(null);
  const started = useRef(false);
  const load = useCallback(() => {
    if (started.current) return;
    started.current = true;
    void Promise.all([
      api.listRecentNotes(200).catch(() => []),
      api.listFolders().catch(() => []),
    ]).then(([notes, folders]) =>
      setLoaded({
        meetings: notes
          .filter((n) => n.meeting_id)
          .map((n) => ({ id: n.id, title: n.title.trim() || "Untitled" })),
        folders: folders.map((f) => ({ id: f.id, name: f.name })),
      }),
    );
  }, []);
  const catalog = useMemo(
    () => ({ meetings: loaded?.meetings ?? [], folders: loaded?.folders ?? [], people }),
    [loaded, people],
  );
  return { catalog, load };
}

const MENTION_GROUP: Record<MentionOption["kind"], string> = {
  meeting: "Meetings",
  person: "People",
  folder: "Folders",
};

/** The composer's runs-on label: where the saved provider runs. */
function RunsOnLabel({ llm }: { llm: LlmSettings | null }) {
  if (!llm) return null;
  const current = llm.providers.find((p) => p.id === llm.provider);
  const place = current
    ? runsOnPlace(current)
    : { kind: "cloud" as const, label: providerName(llm.provider) };
  const text =
    place.kind === "local"
      ? "On this computer"
      : place.kind === "remote"
        ? "Another computer"
        : place.label;
  return (
    <span
      className="mb-[9px] flex-none whitespace-nowrap text-[11.5px] @max-[460px]:hidden"
      style={{ color: place.kind === "local" ? "var(--success-text)" : "var(--warning-text)" }}
      title={
        place.kind === "local"
          ? "Ask runs on this computer."
          : `Ask sends what it needs to ${place.kind === "remote" ? "another computer" : place.label}.`
      }
      data-testid="ask-runs-on-label"
    >
      {text}
    </span>
  );
}

/** The scope chip: what the next question searches. A click offers this
 *  meeting / all meetings; an @ pick shows here with an × to drop it. */
function ScopeChip({ chat, disabled }: { chat: AskChat; disabled: boolean }) {
  const [open, setOpen] = useState(false);
  const menuId = useId();
  const btnRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  useEscapeLayer(() => {
    setOpen(false);
    btnRef.current?.focus();
  }, open);
  useEffect(() => {
    if (!open) return undefined;
    menuRef.current?.querySelector<HTMLElement>("[aria-checked=true]")?.focus();
    const away = (e: PointerEvent) => {
      const t = e.target as Node;
      if (!menuRef.current?.contains(t) && !btnRef.current?.contains(t)) setOpen(false);
    };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  }, [open]);
  const choices = chat.noteId ? [chat.defaultScope, ALL_SCOPE] : [ALL_SCOPE];
  const mention = chat.scope.origin === "mention";
  const pick = (c: ScopeChoice) => {
    chat.setScope(sameScope(c.scope, chat.defaultScope.scope) ? null : c);
    setOpen(false);
    btnRef.current?.focus();
  };
  return (
    <span className="relative mb-[5px] flex flex-none items-center">
      <button
        ref={btnRef}
        type="button"
        disabled={disabled}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-label={`Searches: ${chat.scope.label}. Change what Ask searches`}
        onClick={() => setOpen((o) => !o)}
        className="inline-flex max-w-[180px] cursor-pointer items-center gap-1 rounded-full border-0 py-[3px] pl-2.5 text-[12px] font-semibold disabled:cursor-default"
        style={{
          background: "var(--primary-soft)",
          color: "var(--primary-text)",
          paddingRight: mention ? 4 : 8,
        }}
        data-testid="ask-scope-chip"
      >
        <span className="truncate">{chat.scope.label}</span>
        {!mention && <ChevronDown size={12} strokeWidth={2} aria-hidden="true" />}
      </button>
      {mention && (
        <button
          type="button"
          aria-label={`Stop searching ${chat.scope.label.slice(1)}; search ${chat.defaultScope.label.toLowerCase()}`}
          title={`Back to ${chat.defaultScope.label.toLowerCase()}`}
          disabled={disabled}
          onClick={() => chat.setScope(null)}
          className="-ml-1 grid h-[22px] w-[22px] cursor-pointer place-items-center rounded-full border-0 bg-transparent"
          style={{ color: "var(--primary-text)" }}
        >
          <X size={12} strokeWidth={2} />
        </button>
      )}
      {open && (
        <div
          ref={menuRef}
          id={menuId}
          role="menu"
          aria-label="What Ask searches"
          className="absolute bottom-[calc(100%+8px)] left-0 z-20 flex w-[230px] flex-col gap-0.5 p-1.5"
          style={{
            background: "var(--surface)",
            border: "1px solid var(--line)",
            borderRadius: "var(--radius-xl)",
            boxShadow: "var(--shadow-pop)",
          }}
          onKeyDown={(e) => {
            if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
            e.preventDefault();
            const items = [
              ...(menuRef.current?.querySelectorAll<HTMLElement>("[role=menuitemradio]") ?? []),
            ];
            const i = items.indexOf(document.activeElement as HTMLElement);
            const next = items[(i + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length];
            next?.focus();
          }}
        >
          {choices.map((c) => {
            const on = sameScope(c.scope, chat.scope.scope);
            return (
              <button
                key={c.label}
                type="button"
                role="menuitemradio"
                aria-checked={on}
                onClick={() => pick(c)}
                className="flex cursor-pointer items-center justify-between rounded-lg border-0 bg-transparent px-2.5 py-1.5 text-left text-[13px] text-text hover:bg-surface-3"
              >
                {c.label}
                {on && <CheckCheck size={14} strokeWidth={1.75} aria-hidden="true" />}
              </button>
            );
          })}
          <p className="m-0 px-2.5 pb-1 pt-1.5 text-[11.5px]" style={{ color: "var(--text-3)" }}>
            Type @ to pick a meeting, person or folder.
          </p>
        </div>
      )}
    </span>
  );
}

/** The docked composer. A click, typing or Enter opens the answer sheet
 *  (focus alone doesn't, so tabbing past it or getting focus back when the
 *  sheet closes never reopens it). Enter sends; Shift+Enter adds a line. @
 *  suggests a meeting, person or folder to search. While an answer is being
 *  written, Send becomes Stop. */
export function AskComposer({
  chat,
  inputRef,
  open,
  onOpen,
  placeholder,
  people = [],
}: {
  chat: AskChat;
  inputRef: AskInputRef;
  open: boolean;
  onOpen: () => void;
  placeholder: string;
  /** Names @ suggests (the open meeting's attendees and speakers). */
  people?: string[];
}) {
  const [focused, setFocused] = useState(false);
  const [mention, setMention] = useState<{ start: number; query: string } | null>(null);
  const [active, setActive] = useState(0);
  const listId = useId();
  const { catalog, load } = useMentionCatalog(people);
  const options = useMemo(
    () => (mention ? mentionOptions(mention.query, catalog) : []),
    [mention, catalog],
  );
  const listOpen = mention !== null && options.length > 0;
  useEscapeLayer(() => setMention(null), listOpen);
  const canSend = chat.input.trim() !== "" && !chat.busy;
  const submit = () => {
    onOpen();
    setMention(null);
    chat.send(chat.input);
  };

  // Grow with the text (one line when empty), up to COMPOSER_MAX_PX, and
  // again when its width changes (the window or the dock resized).
  useLayoutEffect(() => {
    const el = inputRef.current;
    if (!el) return undefined;
    const fit = () => {
      el.style.height = "auto";
      el.style.height = `${Math.min(el.scrollHeight, COMPOSER_MAX_PX)}px`;
      // scroll only once it's at full height (no stray scrollbar on one line)
      el.style.overflowY = el.scrollHeight > COMPOSER_MAX_PX ? "auto" : "hidden";
    };
    fit();
    if (typeof ResizeObserver === "undefined") return undefined;
    let width = el.clientWidth;
    const ro = new ResizeObserver(() => {
      if (el.clientWidth === width) return;
      width = el.clientWidth;
      fit();
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [chat.input, inputRef]);

  // Where it runs shows before the first question, and follows a provider
  // change made in Settings.
  const { refreshLlm } = chat;
  useEffect(() => {
    void refreshLlm();
    const again = () => void refreshLlm();
    window.addEventListener(LLM_SETTINGS_CHANGED_EVENT, again);
    return () => window.removeEventListener(LLM_SETTINGS_CHANGED_EVENT, again);
  }, [refreshLlm]);

  const track = (el: HTMLTextAreaElement) => {
    const m = mentionAt(el.value, el.selectionStart ?? el.value.length);
    if (m) load();
    setMention((cur) => {
      if (!m) return null;
      if (!cur || cur.start !== m.start) setActive(0);
      return m;
    });
  };

  const pick = (o: MentionOption) => {
    const el = inputRef.current;
    if (!mention || !el) return;
    const caret = el.selectionStart ?? chat.input.length;
    const text = removeMention(chat.input, mention.start, caret);
    chat.setInput(text);
    chat.setScope(mentionScope(o));
    setMention(null);
    requestAnimationFrame(() => {
      el.focus();
      el.setSelectionRange(mention.start, mention.start);
    });
  };

  const optionId = (i: number) => `${listId}-o${i}`;

  return (
    <form
      aria-label="Ask"
      className="@container relative flex min-w-[240px] flex-1 items-end gap-2 rounded-[22px] border py-1 pl-2 pr-1"
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
      <ScopeChip chat={chat} disabled={chat.busy} />
      <textarea
        ref={inputRef}
        rows={1}
        value={chat.input}
        onChange={(e) => {
          chat.setInput(e.target.value);
          track(e.target);
          onOpen();
        }}
        onClick={(e) => {
          onOpen();
          track(e.currentTarget);
        }}
        onKeyDown={(e) => {
          if (listOpen) {
            if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              e.preventDefault();
              const n = options.length;
              setActive((i) => (i + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
              return;
            }
            if ((e.key === "Enter" || e.key === "Tab") && !e.nativeEvent.isComposing) {
              e.preventDefault();
              pick(options[Math.min(active, options.length - 1)]);
              return;
            }
          }
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            submit();
          }
        }}
        onKeyUp={(e) => {
          if (
            e.key === "ArrowLeft" ||
            e.key === "ArrowRight" ||
            e.key === "Home" ||
            e.key === "End"
          ) {
            track(e.currentTarget);
          }
        }}
        onFocus={() => setFocused(true)}
        onBlur={() => {
          setFocused(false);
          setMention(null);
        }}
        placeholder={placeholder}
        aria-label={placeholder}
        aria-autocomplete="list"
        // The sheet this composer fills, or the @ suggestions while they're
        // open. (aria-expanded isn't valid on a text box; the sheet
        // announces itself as a labelled dialog.)
        aria-controls={listOpen ? listId : open ? chat.sheetId : undefined}
        aria-activedescendant={listOpen ? optionId(active) : undefined}
        className="block min-w-0 flex-1 resize-none overflow-hidden border-0 bg-transparent py-1.5 text-[14px] leading-[1.45] text-text outline-none placeholder:text-text-3"
        // the pill around it shows the focus ring
        style={{ boxShadow: "none", maxHeight: COMPOSER_MAX_PX }}
      />
      <RunsOnLabel llm={chat.llm} />
      {chat.busy ? (
        <button
          type="button"
          aria-label="Stop the answer"
          title="Stop the answer"
          onClick={() => {
            chat.stop();
            inputRef.current?.focus();
          }}
          className="grid h-8 w-8 flex-none cursor-pointer place-items-center rounded-full border-0"
          // the design's `contrast` button: ink on the surface color
          style={{ background: "var(--text)", color: "var(--surface)" }}
          data-testid="ask-stop"
        >
          <Square size={12} strokeWidth={2.5} fill="currentColor" />
        </button>
      ) : (
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
      )}
      {listOpen && (
        <div
          id={listId}
          role="listbox"
          aria-label="Search a meeting, person or folder"
          className="absolute bottom-[calc(100%+8px)] left-2 z-20 flex max-h-[300px] w-[300px] max-w-[calc(100%-16px)] flex-col overflow-y-auto p-1.5"
          style={{
            background: "var(--surface)",
            border: "1px solid var(--line)",
            borderRadius: "var(--radius-xl)",
            boxShadow: "var(--shadow-pop)",
          }}
          data-testid="ask-mentions"
        >
          {(["meeting", "person", "folder"] as const).map((kind) => {
            const inGroup = options.map((o, i) => ({ o, i })).filter(({ o }) => o.kind === kind);
            if (inGroup.length === 0) return null;
            return (
              <div key={kind} role="group" aria-label={MENTION_GROUP[kind]}>
                <div
                  aria-hidden="true"
                  className="px-2.5 pb-0.5 pt-1.5 text-[10.5px] font-bold uppercase tracking-[0.06em]"
                  style={{ color: "var(--text-3)" }}
                >
                  {MENTION_GROUP[kind]}
                </div>
                {inGroup.map(({ o, i }) => (
                  <div
                    key={`${o.id}-${o.typed ? "typed" : "known"}`}
                    id={optionId(i)}
                    role="option"
                    aria-selected={i === active}
                    // keep focus (and the caret) in the composer
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={() => pick(o)}
                    onMouseEnter={() => setActive(i)}
                    className="cursor-pointer truncate rounded-lg px-2.5 py-1.5 text-[13px] text-text"
                    style={{ background: i === active ? "var(--surface-3)" : "transparent" }}
                  >
                    {o.typed ? `Meetings with “${o.label}”` : o.label}
                  </div>
                ))}
              </div>
            );
          })}
        </div>
      )}
      <span className="sr-only" aria-live="polite" data-testid="ask-live">
        {chat.announcement}
      </span>
    </form>
  );
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
  const confirm = useConfirm();
  // Consent before a cloud provider first gets meeting text: the same words
  // and setting as Settings; the backend enforces it too.
  const askCloudConsent = useCloudConsent();
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
    if (!(await askCloudConsent(fresh.providers, id))) {
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
      !(await confirm({
        title: `Switch to ${providerLabel("anthropic")}?`,
        message:
          `Ask now runs on ${providerLabel(fresh.provider)} (changed in Settings).\n\n` +
          `Switch Ask, Enhance, transcript cleanup and action items to ` +
          `${providerLabel("anthropic")} with ${modelLabel(model)}?`,
        confirmLabel: "Switch",
        cancelLabel: "Keep current setting",
      }))
    ) {
      setMessage({ text: `Kept ${providerLabel(fresh.provider)}.`, tone: "info" });
      return;
    }
    if (!(await askCloudConsent(fresh.providers, "anthropic"))) {
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

/** The answer sheet: over the note above the dock, or pinned to its side
 *  (or over Home, for Ask over every meeting). */
export function AskSheet({
  chat,
  inputRef,
  hasMeeting,
  keptLocal,
  pinned,
  canPin,
  onTogglePin,
  onClose,
  insertNoteId = null,
  onNoteChanged,
  onOpenSource,
  onShowInserted,
  turnExtras,
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
  /** The note "Insert into note" adds an answer to (as an AI block with its
   *  sources); null offers no Insert (Ask on Home). */
  insertNoteId?: string | null;
  /** A click wrote to a note (Insert, or an approved proposal). */
  onNoteChanged?: (note: Note) => void;
  /** A citation chip or source line: open the transcript at those lines. */
  onOpenSource?: OpenSource;
  /** "Show" after an insert: open the note's Enhanced view. */
  onShowInserted?: () => void;
  /** Extra content for a turn (connector approval cards). */
  turnExtras?: (turn: AskTurn) => ReactNode;
}) {
  const endRef = useRef<HTMLDivElement>(null);
  const { refreshLlm } = chat;
  const askCloudConsent = useCloudConsent();

  // The saved provider may have changed in Settings since last time.
  useEffect(() => {
    void refreshLlm();
  }, [refreshLlm]);

  // Keep the newest message in view (scroll the sheet body only). While an
  // answer streams in, follow it only if you're already near the bottom.
  const { turns, busy, error } = chat;
  const last = turns[turns.length - 1];
  useEffect(() => {
    const body = endRef.current?.closest(".fly-sheet-body");
    if (body) body.scrollTop = body.scrollHeight;
  }, [turns.length, busy, error, last?.phase]);
  const frameLen = last?.frame.length ?? 0;
  useEffect(() => {
    const body = endRef.current?.closest(".fly-sheet-body");
    if (body && body.scrollHeight - body.scrollTop - body.clientHeight < 160) {
      body.scrollTop = body.scrollHeight;
    }
  }, [frameLen]);

  const global = chat.noteId === null;
  const what = global ? "Ask your meetings" : hasMeeting ? "Ask this meeting" : "Ask this note";
  // Buttons that vanish when clicked (Insert, Try again, Dismiss) hand focus
  // back to the composer instead of dropping it on the page.
  const thenFocusComposer = (action: () => void) => () => {
    inputRef.current?.focus();
    action();
  };
  const lastAnswered = turns.map((t) => t.phase === "done").lastIndexOf(true);
  const errorView = error ? describeAskError(error.raw, error.provider) : null;
  const prompts: readonly string[] =
    chat.scope.scope.kind === "all" && global ? GLOBAL_PROMPTS : QUICK_PROMPTS;

  // "Isn't confirmed yet": ask for consent here, then ask the question again.
  const allowAndRetry = async () => {
    if (!error) return;
    const fresh = await chat.refreshLlm();
    if (fresh && (await askCloudConsent(fresh.providers, error.provider))) chat.retry();
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
            {global
              ? "Not saved · clears when you close the app"
              : "Not saved · clears when you leave this note"}
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
      {turns.length === 0 && (
        <div className="flex flex-col gap-2">
          <p className="m-0 text-[12.5px]" style={{ color: "var(--text-3)" }}>
            {global
              ? "Answers come from all your meetings. Type @ to pick a meeting, person or folder."
              : hasMeeting
                ? "Answers come from this meeting's transcript and your notes."
                : "Answers come from your notes."}
          </p>
          {prompts.map((q) => (
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

      {turns.map((turn, i) => (
        <AskTurnView
          key={turn.id}
          turn={turn}
          chat={chat}
          isLastAnswer={i === lastAnswered}
          showScope={!sameScope(turn.scope.scope, chat.defaultScope.scope)}
          insertNoteId={insertNoteId}
          onNoteChanged={onNoteChanged}
          onOpenSource={onOpenSource}
          onShowInserted={onShowInserted}
          focusComposer={() => inputRef.current?.focus()}
          extras={turnExtras?.(turn)}
        />
      ))}

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
