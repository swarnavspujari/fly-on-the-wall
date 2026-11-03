/* Ask's chats, kept for the app's lifetime and keyed by note ("home" for
   Ask over every meeting). App owns one store, so opening a source in
   another note, going Home and coming back keeps each conversation; an
   answer still being written keeps going and lands in the chat it was asked
   from. Nothing here is saved: the chats clear when the app closes.

   The store also owns the one `ask:event` listener (while any Ask view is
   mounted) and a per-meeting transcript cache for the citation checks. */

import { listen } from "@tauri-apps/api/event";
import { api, ASK_EVENT } from "./api";
import { citedIds } from "./askCitations";
import type { ScopeChoice } from "./askScope";
import type { ProposalState } from "./components/AskAnswer";
import type {
  AskEventPayload,
  AskMessage,
  AskReply,
  AskSource,
  AskStage,
  AskStreamEvent,
  Transcript,
} from "./types";

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

export interface ChatError {
  raw: string;
  question: string;
  scope: ScopeChoice;
  provider: string;
}

/** One chat's state. */
export interface ChatData {
  turns: AskTurn[];
  /** The last failure, the question and scope it was for (to retry) and
   *  the provider it went to. */
  error: ChatError | null;
  /** Turns (by id) whose answer was inserted into the note. */
  inserted: ReadonlySet<string>;
  /** Proposal approvals by proposal id: one is never approved twice. */
  proposals: ReadonlyMap<string, ProposalState>;
  /** The scope picked for the next question; null = the default. */
  scopePick: ScopeChoice | null;
  /** The ask id being written (one at a time per chat). */
  running: string | null;
}

export const EMPTY_CHAT: ChatData = Object.freeze({
  turns: [],
  error: null,
  inserted: new Set<string>(),
  proposals: new Map<string, ProposalState>(),
  scopePick: null,
  running: null,
});

/** The key of Ask over every meeting (Home). */
export const HOME_KEY = "home";

export class AskChatStore {
  private chats = new Map<string, ChatData>();
  /** Unsent composer text per chat (no re-render on each keystroke). */
  private drafts = new Map<string, string>();
  private subs = new Set<() => void>();
  private transcripts = new Map<string, Promise<Transcript | null>>();
  private holders = 0;
  private listenGen = 0;
  private unlisten: (() => void) | null = null;

  /** A chat's state; the same object until that chat changes. */
  get = (key: string): ChatData => this.chats.get(key) ?? EMPTY_CHAT;

  update(key: string, fn: (d: ChatData) => ChatData) {
    this.chats.set(key, fn(this.get(key)));
    this.subs.forEach((s) => s());
  }

  subscribe = (s: () => void) => {
    this.subs.add(s);
    return () => {
      this.subs.delete(s);
    };
  };

  draft = (key: string) => this.drafts.get(key) ?? "";
  setDraft = (key: string, text: string) => {
    if (text) this.drafts.set(key, text);
    else this.drafts.delete(key);
  };

  /** Route a stream event to the running turn with its ask id. */
  applyEvent({ ask_id, event }: AskEventPayload) {
    for (const [key, d] of this.chats) {
      if (!d.turns.some((t) => t.id === ask_id && t.phase === "running")) continue;
      this.update(key, (c) => ({
        ...c,
        turns: c.turns.map((t) => (t.id === ask_id ? applyAskEvent(t, event) : t)),
      }));
      return;
    }
  }

  /** Listen to the stream while any Ask view is mounted; returns the
   *  release. */
  retain(): () => void {
    this.holders += 1;
    if (this.holders === 1) {
      const gen = ++this.listenGen;
      listen<AskEventPayload>(ASK_EVENT, (e) => this.applyEvent(e.payload))
        .then((un) => {
          if (gen === this.listenGen && this.holders > 0) this.unlisten = un;
          else un();
        })
        .catch(() => {});
    }
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.holders -= 1;
      if (this.holders === 0) {
        this.listenGen += 1;
        this.unlisten?.();
        this.unlisten = null;
      }
    };
  }

  /** A meeting's transcript for the citation checks, read once per chat
   *  session (`fresh` re-reads it, as a chip click does, so a line that
   *  moved since is seen). A failed read isn't kept. */
  transcript(meetingId: string, fresh = false): Promise<Transcript | null> {
    let p = fresh ? undefined : this.transcripts.get(meetingId);
    if (!p) {
      p = api.getTranscript(meetingId);
      const kept = p;
      this.transcripts.set(meetingId, kept);
      kept.catch(() => {
        if (this.transcripts.get(meetingId) === kept) this.transcripts.delete(meetingId);
      });
    }
    return p;
  }
}
