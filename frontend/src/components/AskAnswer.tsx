import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Lock, RotateCcw, TriangleAlert } from "lucide-react";
import type { AskProposal, AskReply, AskSentence, Note, Transcript } from "../types";
import { api } from "../api";
import {
  KEPT_LOCAL_NOT_SEARCHED,
  UNVERIFIED_COPY,
  answerRunsOn,
  flagCopy,
  isWideScope,
  notFoundCopy,
  proposalExpired,
  refusalCopy,
  stageCopy,
  tierDetail,
  tierLabel,
  traceDetails,
  traceSummary,
} from "../askModel";
import {
  CITE_STATUS_COPY,
  citeGroups,
  citedIds,
  clock,
  layoutSentences,
  locateLine,
  worstStatus,
} from "../askCitations";
import type { CiteGroup, CiteStatus, LocatedLine } from "../askCitations";
import { ALL_SCOPE } from "../askScope";
import { KEEP_LOCAL_NOTICE } from "../connectorsModel";
import { AskConnectorCards } from "./AskConnectors";
import { Button, CitationChip } from "./ui";
import { SAFE_A, SAFE_IMG } from "./markdownSafe";
import type { AskChat, AskTurn } from "./AskPanel";

/* One Ask turn in the answer sheet: your question, then the trace pill and
   where it ran, then the answer as it streams and, once checked, as
   sentences with citation chips (unsourced and flagged lines marked), the
   sources, the changes Ask proposes (buttons; nothing runs on its own) and
   Insert. Everything shown came back sanitized from the harness (no links,
   no images, no reasoning) and is rendered without raw HTML. */

/** Open a meeting's transcript at these lines (a chip or a source line). */
export type OpenSource = (target: { meetingId: string; segmentIds: string[] }) => void;

/** A proposal's approval, kept in the chat (not the sheet) so closing and
 *  reopening the sheet can never approve it twice. */
export type ProposalState =
  { phase: "pending" } | { phase: "done" } | { phase: "failed"; message: string };

/** Inline Markdown only (a sentence is never a block), safe links/images. */
const INLINE_MD = {
  p: ({ children }: { children?: ReactNode }) => <>{children}</>,
  a: SAFE_A,
  img: SAFE_IMG,
};
const INLINE_ALLOWED = ["p", "strong", "em", "code", "del", "a", "br"];
function InlineMd({ text }: { text: string }) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      components={INLINE_MD}
      allowedElements={INLINE_ALLOWED}
      unwrapDisallowed
    >
      {text}
    </ReactMarkdown>
  );
}

const BLOCK_MD = { a: SAFE_A, img: SAFE_IMG };

/** Whether each cited line still says what Ask read, checked against the
 *  meeting's transcript now (see askCitations.ts). Read when the answer
 *  arrives and again on every chip click. */
function useLineStatus(reply: AskReply | null) {
  const [located, setLocated] = useState<ReadonlyMap<string, LocatedLine>>(new Map());
  const check = useCallback(
    async (meetingId: string): Promise<ReadonlyMap<string, LocatedLine>> => {
      const src = reply?.sources.find((s) => s.meeting_id === meetingId);
      if (!src) return new Map();
      let transcript: Transcript | null;
      try {
        transcript = await api.getTranscript(meetingId);
      } catch {
        return new Map(); // can't check: claim nothing either way
      }
      const out = new Map<string, LocatedLine>();
      for (const l of src.lines) {
        out.set(
          l.segment_id,
          transcript ? locateLine(l, transcript) : { status: "changed", segmentId: null },
        );
      }
      setLocated((m) => {
        const next = new Map(m);
        out.forEach((v, k) => next.set(k, v));
        return next;
      });
      return out;
    },
    [reply],
  );
  useEffect(() => {
    for (const s of reply?.sources ?? []) void check(s.meeting_id);
  }, [reply, check]);
  return { located, check };
}

const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;

/** A chip for one meeting's lines in a sentence. Its title previews them;
 *  a moved or changed line shows on the chip itself. */
function AskCiteChip({
  group,
  status,
  onOpen,
}: {
  group: CiteGroup;
  status: CiteStatus;
  onOpen?: () => void;
}) {
  const n = group.lines.length;
  const preview = group.lines
    .slice(0, 3)
    .map((l) => `${l.speaker} · ${clock(l.start_ms)} — “${l.snippet}”`)
    .join("\n");
  const statusWord = status === "changed" ? "changed" : status === "reanchored" ? "moved" : null;
  const label =
    `${plural(n, "source")} from ${group.meetingTitle}` +
    (status === "changed" ? ". Source changed since Ask read it, check it" : "") +
    (status === "reanchored" ? ". Moved since Ask read it" : "") +
    (onOpen ? ". Open in transcript" : "");
  return (
    <CitationChip
      onClick={onOpen ? () => onOpen() : undefined}
      aria-label={label}
      title={`${group.meetingTitle}\n${preview}${statusWord ? `\n\n${CITE_STATUS_COPY[status as "changed"]}` : ""}`}
      className="ml-1 align-baseline"
      data-cite-status={status}
      style={
        status === "ok"
          ? undefined
          : { boxShadow: "inset 0 0 0 1px var(--warning)", color: "var(--warning-text)" }
      }
    >
      {plural(n, "source")}
      {statusWord && ` · ${statusWord}`}
    </CitationChip>
  );
}

const SENTENCE_STYLE: Record<AskSentence["status"], React.CSSProperties | undefined> = {
  sourced: undefined,
  unsourced: {
    background: "var(--warning-soft)",
    borderRadius: 4,
    padding: "1px 3px",
    WebkitBoxDecorationBreak: "clone",
    boxDecorationBreak: "clone",
  },
  flagged: {
    background: "var(--warning-soft)",
    borderRadius: 4,
    padding: "1px 3px",
    borderBottom: "2px solid var(--warning)",
    WebkitBoxDecorationBreak: "clone",
    boxDecorationBreak: "clone",
  },
};

/** One checked sentence: its text, its chips, and why it's marked. */
function Sentence({
  sentence,
  text,
  reply,
  located,
  onOpenGroup,
}: {
  sentence: AskSentence;
  text: string;
  reply: AskReply;
  located: ReadonlyMap<string, LocatedLine>;
  onOpenGroup?: (g: CiteGroup) => void;
}) {
  const reasonId = useId();
  const groups = citeGroups(sentence, reply.sources);
  const flags = sentence.flags ?? [];
  const reasons =
    sentence.status === "flagged"
      ? [...new Set(flags.map(flagCopy))]
      : sentence.status === "unsourced"
        ? ["No source"]
        : [];
  return (
    <span data-sentence-status={sentence.status}>
      <span
        style={SENTENCE_STYLE[sentence.status]}
        aria-describedby={reasons.length ? reasonId : undefined}
      >
        <InlineMd text={text} />
      </span>
      {groups.map((g) => (
        <AskCiteChip
          key={g.meetingId}
          group={g}
          status={worstStatus(
            g.lines.map((l) => located.get(l.segment_id)?.status ?? ("ok" as CiteStatus)),
          )}
          onOpen={onOpenGroup ? () => onOpenGroup(g) : undefined}
        />
      ))}
      {reasons.length > 0 && (
        <span
          id={reasonId}
          className="ml-1.5 inline-flex items-center gap-1 align-baseline text-[11.5px] font-semibold"
          style={{ color: "var(--warning-text)" }}
          data-testid={sentence.status === "flagged" ? "ask-flag" : "ask-unsourced"}
        >
          <TriangleAlert size={12} strokeWidth={2} aria-hidden="true" />
          {sentence.status === "flagged" ? `Check: ${reasons.join(" ")}` : reasons[0]}
        </span>
      )}{" "}
    </span>
  );
}

/** The trace pill: collapsed "how this was answered", expanding to the
 *  steps. Counts and codes only, never meeting text. */
function TracePill({ reply, turn }: { reply: AskReply; turn: AskTurn }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  const runsOn = answerRunsOn(reply, turn.provider);
  return (
    <>
      <button
        type="button"
        aria-expanded={open}
        aria-controls={id}
        onClick={() => setOpen((o) => !o)}
        className="flex cursor-pointer items-center gap-2 rounded-full border border-line px-2.5 py-1 text-[12px]"
        style={{
          background: "var(--surface-2)",
          color: "var(--text-2)",
          fontFamily: "var(--font-sans)",
        }}
        data-testid="ask-trace"
      >
        <span aria-hidden="true">{open ? "▾" : "▸"}</span>
        <span>{traceSummary(reply)}</span>
        <span className="sr-only">. How Ask answered</span>
      </button>
      {open && (
        <div
          id={id}
          className="flex w-full flex-col gap-1 rounded-[10px] px-3 py-2.5 font-mono text-[11.5px]"
          style={{ background: "var(--surface-2)", color: "var(--text-2)" }}
          data-testid="ask-trace-details"
        >
          {traceDetails(reply, runsOn, turn.model).map((l, i) => (
            <span key={i}>{l}</span>
          ))}
        </div>
      )}
    </>
  );
}

/** Where the answer ran (this computer or a cloud vendor) and the tier the
 *  harness picked. */
function RunsOnPill({ reply, turn }: { reply: AskReply; turn: AskTurn }) {
  const where = answerRunsOn(reply, turn.provider);
  const tier = tierLabel(reply.trace?.tier);
  return (
    <span
      className="inline-flex items-center gap-1.5 rounded-full border border-line px-2.5 py-1 text-[12px]"
      style={{ color: where.kind === "local" ? "var(--success-text)" : "var(--warning-text)" }}
      title={tierDetail(reply.trace?.tier) ?? undefined}
      data-testid="ask-runs-on"
    >
      <span
        aria-hidden="true"
        className="h-1.5 w-1.5 rounded-full"
        style={{ background: where.kind === "local" ? "var(--success)" : "var(--warning)" }}
      />
      <span className="sr-only">Ran </span>
      {where.label}
      {tier && <span style={{ color: "var(--text-3)" }}>· {tier}</span>}
    </span>
  );
}

/** While an answer is written: what Ask is doing, what it found, and the
 *  text so far (not checked yet, so no chips). */
function Streaming({ turn, onStop }: { turn: AskTurn; onStop: () => void }) {
  const titles = [...new Set((turn.found?.sources ?? []).map((s) => s.meeting_title))];
  return (
    <div className="flex flex-col gap-2" aria-busy="true" data-testid="ask-streaming">
      <div className="flex flex-wrap items-center gap-2">
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
          {stageCopy(turn.stage, turn.scope.scope)}
        </div>
        <Button variant="ghost" size="xs" onClick={onStop} data-testid="ask-stop-inline">
          Stop
        </Button>
      </div>
      {titles.length > 0 && (
        <p className="m-0 px-1 text-[12px]" style={{ color: "var(--text-3)" }}>
          Reading {titles.slice(0, 2).join(" · ")}
          {titles.length > 2 ? ` and ${plural(titles.length - 2, "more meeting")}` : ""}
        </p>
      )}
      {turn.frame && (
        <div
          className="whitespace-pre-wrap px-1 text-[14.5px] leading-[1.6]"
          style={{ color: "var(--text-2)" }}
          data-testid="ask-frame"
        >
          {turn.frame}
          <span
            aria-hidden="true"
            className="ml-0.5 inline-block h-[1em] w-[2px] align-text-bottom"
            style={{
              background: "var(--primary)",
              animation: "fly-pulse-dot 1s steps(2) infinite",
            }}
          />
        </div>
      )}
    </div>
  );
}

/** A change Ask proposes: shown with exactly what it would do, approved
 *  only by a click, and never twice. */
function ProposalCard({
  proposal: p,
  state,
  current,
  meetingTitle,
  onApprove,
}: {
  proposal: AskProposal;
  state: ProposalState | undefined;
  /** It targets the note that's open. */
  current: boolean;
  meetingTitle: string | null;
  onApprove: () => void;
}) {
  const insert = p.tool === "insert_into_note";
  const expired = proposalExpired(p.expires_at);
  const blocked = p.gate === "blocked";
  const pending = state?.phase === "pending";
  const done = state?.phase === "done";
  const action = insert ? "Insert into note" : "Add action item";
  return (
    <div
      className="flex flex-col gap-2 rounded-[12px] border border-line px-3.5 py-3"
      style={{ background: "var(--surface-2)" }}
      data-testid="ask-proposal"
      data-proposal-tool={p.tool}
    >
      <span className="text-[12px] font-semibold" style={{ color: "var(--text-2)" }}>
        {insert
          ? current || !meetingTitle
            ? "Ask suggests adding this to your note"
            : `Ask suggests adding this to ${meetingTitle}`
          : "Ask suggests an action item"}
      </span>
      {insert ? (
        <div className="max-h-[160px] overflow-y-auto text-[13.5px] leading-[1.55] text-text [&_li]:ml-4 [&_li]:list-disc [&_p]:my-1">
          <ReactMarkdown remarkPlugins={[remarkGfm]} components={BLOCK_MD}>
            {p.markdown ?? ""}
          </ReactMarkdown>
        </div>
      ) : (
        <div className="flex flex-col gap-0.5 text-[13.5px] text-text">
          <span>{p.text}</span>
          <span className="text-[12px]" style={{ color: "var(--text-3)" }}>
            {p.owner ? `Owner: ${p.owner}` : "No owner"}
            {meetingTitle ? ` · ${meetingTitle}` : ""}
          </span>
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2">
        {done ? (
          <span
            className="inline-flex items-center gap-1 text-[12px] font-semibold"
            style={{ color: "var(--success-text)" }}
            role="status"
          >
            <Check size={13} strokeWidth={2} aria-hidden="true" />
            {insert ? "Added to the note" : "Added to action items"}
          </span>
        ) : (
          <Button
            variant="soft"
            size="xs"
            disabled={pending || expired || blocked}
            onClick={onApprove}
            aria-describedby={undefined}
          >
            {pending ? "Adding…" : state?.phase === "failed" ? "Try again" : action}
          </Button>
        )}
        {!done && (blocked || expired) && (
          <span className="text-[12px]" style={{ color: "var(--text-3)" }}>
            {blocked
              ? "Ask can't make this kind of change."
              : "This suggestion expired. Ask again for a fresh one."}
          </span>
        )}
        {state?.phase === "failed" && (
          <span className="text-[12px]" style={{ color: "var(--error-text)" }} role="alert">
            Couldn't add it, so nothing changed. ({state.message})
          </span>
        )}
      </div>
    </div>
  );
}

export function AskTurnView({
  turn,
  chat,
  isLastAnswer,
  showScope,
  insertNoteId,
  onNoteChanged,
  onOpenSource,
  onShowInserted,
  focusComposer,
}: {
  turn: AskTurn;
  chat: AskChat;
  /** The newest answered turn: Insert breathes and says it's the only way
   *  Ask writes. */
  isLastAnswer: boolean;
  /** The question's scope differs from the chat's default: say what it
   *  searched. */
  showScope: boolean;
  insertNoteId: string | null;
  onNoteChanged?: (note: Note) => void;
  onOpenSource?: OpenSource;
  onShowInserted?: () => void;
  focusComposer: () => void;
}) {
  const reply = turn.phase === "done" ? turn.reply : null;
  const { located, check } = useLineStatus(reply);
  const [inserting, setInserting] = useState(false);
  const [insertError, setInsertError] = useState<string | null>(null);
  const approving = useRef(new Set<string>());

  const openGroup = onOpenSource
    ? async (g: CiteGroup) => {
        const now = await check(g.meetingId);
        const ids = g.lines.map((l) => now.get(l.segment_id)?.segmentId ?? l.segment_id);
        onOpenSource({ meetingId: g.meetingId, segmentIds: [...new Set(ids)] });
      }
    : undefined;

  const wide = isWideScope(turn.scope.scope);
  const keptLocalOut =
    wide && (turn.found?.keptLocalNotSearched || (reply?.trace?.kept_local_not_searched ?? 0) > 0);

  const insert = async () => {
    if (!insertNoteId || !reply || inserting) return;
    setInserting(true);
    setInsertError(null);
    try {
      const note = await api.insertAskAnswer(insertNoteId, reply.text, citedIds(reply.sentences));
      onNoteChanged?.(note);
      chat.markInserted(turn.id);
      focusComposer();
    } catch (e) {
      setInsertError(String(e));
    } finally {
      setInserting(false);
    }
  };

  const approve = async (p: AskProposal) => {
    const state = chat.proposals.get(p.id);
    if (approving.current.has(p.id) || state?.phase === "pending" || state?.phase === "done")
      return;
    approving.current.add(p.id);
    chat.setProposal(p.id, { phase: "pending" });
    try {
      if (p.tool === "insert_into_note") {
        const note = await api.insertAskAnswer(p.note_id, p.markdown ?? "", p.segment_ids);
        // only the open note is on screen; another note just changed on disk
        if (note.id === insertNoteId) onNoteChanged?.(note);
      } else {
        await api.addActionItem(p.meeting_id, p.text ?? "", p.owner ?? null, p.segment_ids);
      }
      chat.setProposal(p.id, { phase: "done" });
      focusComposer();
    } catch (e) {
      chat.setProposal(p.id, { phase: "failed", message: String(e) });
    } finally {
      approving.current.delete(p.id);
    }
  };

  const titleOf = (meetingId: string) =>
    reply?.sources.find((s) => s.meeting_id === meetingId)?.meeting_title ?? null;
  const answered = reply && (reply.status === "answered" || reply.status === "unverified");
  const refusals = (reply?.refusals ?? []).map(refusalCopy).filter((s): s is string => !!s);

  return (
    <div className="mb-4 flex flex-col gap-2" data-turn={turn.id} data-phase={turn.phase}>
      <div className="flex flex-col items-end gap-1">
        <span
          className="max-w-[80%] whitespace-pre-wrap rounded-[14px] px-3.5 py-2 text-[14px] text-text"
          style={{ background: "var(--surface-2)" }}
        >
          {turn.question}
        </span>
        {showScope && (
          <span className="text-[11.5px]" style={{ color: "var(--text-3)" }}>
            Searched: {turn.scope.label}
          </span>
        )}
      </div>

      {turn.phase === "running" && <Streaming turn={turn} onStop={chat.stop} />}

      {turn.phase === "stopped" && (
        <div className="flex flex-wrap items-center gap-2 px-1" data-testid="ask-stopped">
          <span className="text-[13px]" style={{ color: "var(--text-2)" }}>
            You stopped this answer. Nothing from it was kept.
          </span>
          <Button
            variant="ghost"
            size="xs"
            disabled={chat.busy}
            startIcon={<RotateCcw size={13} strokeWidth={1.75} />}
            onClick={() => {
              focusComposer();
              chat.send(turn.question, turn.scope);
            }}
          >
            Ask again
          </Button>
        </div>
      )}

      {reply && (
        <div className="flex flex-wrap items-center gap-2">
          <TracePill reply={reply} turn={turn} />
          <RunsOnPill reply={reply} turn={turn} />
        </div>
      )}

      {keptLocalOut && (
        <p
          className="m-0 flex items-center gap-1.5 px-1 text-[12px]"
          style={{ color: "var(--text-3)" }}
          data-testid="ask-kept-local"
        >
          <Lock size={12} strokeWidth={1.75} aria-hidden="true" />
          {KEPT_LOCAL_NOT_SEARCHED}
        </p>
      )}

      {reply?.status === "not_found" &&
        (() => {
          const copy = notFoundCopy(turn.scope.scope);
          return (
            <div
              className="flex flex-col gap-1.5 rounded-[12px] border border-line px-3.5 py-3"
              style={{ background: "var(--surface-2)" }}
              data-testid="ask-not-found"
            >
              <p className="m-0 text-[14px] font-semibold text-text">{copy.title}</p>
              <p className="m-0 text-[13px]" style={{ color: "var(--text-2)" }}>
                {copy.body}
              </p>
              {copy.widen && (
                <Button
                  variant="outline"
                  size="xs"
                  className="self-start"
                  disabled={chat.busy}
                  onClick={() => {
                    focusComposer();
                    chat.setScope(ALL_SCOPE);
                    chat.send(turn.question, ALL_SCOPE);
                  }}
                >
                  Search all meetings
                </Button>
              )}
            </div>
          );
        })()}

      {reply?.status === "refused" && (
        <div
          className="flex flex-col gap-1 rounded-[12px] border border-line px-3.5 py-3"
          style={{ background: "var(--surface-2)" }}
          data-testid="ask-refused"
        >
          <p className="m-0 text-[14px] font-semibold text-text">Ask can't search this</p>
          <p className="m-0 text-[13px]" style={{ color: "var(--text-2)" }}>
            {reply.text}
          </p>
        </div>
      )}

      {answered && (
        <div className="flex flex-col gap-2" data-testid="ask-answer" data-status={reply.status}>
          {reply.status === "unverified" && (
            <p
              className="m-0 flex items-start gap-1.5 rounded-[10px] px-3 py-2 text-[12.5px]"
              style={{ background: "var(--warning-soft)", color: "var(--warning-text)" }}
              role="note"
            >
              <TriangleAlert
                size={14}
                strokeWidth={1.75}
                aria-hidden="true"
                className="mt-0.5 flex-none"
              />
              {UNVERIFIED_COPY}
            </p>
          )}
          <div className="text-[14.5px] leading-[1.7] text-text">
            {(() => {
              const blocks = layoutSentences(reply.sentences);
              const out: ReactNode[] = [];
              let bullets: ReactNode[] = [];
              const flush = (key: string) => {
                if (bullets.length)
                  out.push(
                    <ul key={key} className="my-1 ml-5 list-disc">
                      {bullets}
                    </ul>,
                  );
                bullets = [];
              };
              blocks.forEach((b, bi) => {
                const body = b.sentences.map(({ index, text }) => (
                  <Sentence
                    key={index}
                    sentence={reply.sentences[index]}
                    text={text}
                    reply={reply}
                    located={located}
                    onOpenGroup={openGroup}
                  />
                ));
                if (b.bullet) {
                  bullets.push(<li key={bi}>{body}</li>);
                } else {
                  flush(`ul-${bi}`);
                  out.push(
                    <p key={bi} className="my-1">
                      {body}
                    </p>,
                  );
                }
              });
              flush("ul-end");
              return out;
            })()}
          </div>
        </div>
      )}

      {reply && reply.sources.length > 0 && answered && (
        <details className="group px-1 text-[12.5px]" data-testid="ask-sources">
          <summary className="cursor-pointer select-none" style={{ color: "var(--text-2)" }}>
            Sources ·{" "}
            {plural(
              reply.sources.reduce((n, s) => n + s.lines.length, 0),
              "line",
            )}{" "}
            from {plural(reply.sources.length, "meeting")}
          </summary>
          <div className="mt-1.5 flex flex-col gap-2.5">
            {reply.sources.map((s) => (
              <div key={s.meeting_id} className="flex flex-col gap-1">
                <span className="text-[12px] font-semibold text-text">{s.meeting_title}</span>
                {s.lines.map((l) => {
                  const st = located.get(l.segment_id)?.status ?? "ok";
                  const content = (
                    <>
                      <span className="font-mono text-[11px]" style={{ color: "var(--text-3)" }}>
                        {clock(l.start_ms)}
                      </span>{" "}
                      <span className="font-semibold">{l.speaker}</span> “{l.snippet}”
                      {st !== "ok" && (
                        <span
                          className="ml-1 font-semibold"
                          style={{ color: "var(--warning-text)" }}
                        >
                          {CITE_STATUS_COPY[st]}
                        </span>
                      )}
                    </>
                  );
                  return openGroup ? (
                    <button
                      key={l.segment_id}
                      type="button"
                      onClick={() =>
                        void openGroup({
                          meetingId: s.meeting_id,
                          meetingTitle: s.meeting_title,
                          lines: [l],
                        })
                      }
                      className="cursor-pointer rounded-md border-0 bg-transparent px-1 py-0.5 text-left text-[12.5px] leading-[1.5] hover:bg-surface-3"
                      style={{ color: "var(--text-2)", fontFamily: "var(--font-sans)" }}
                      aria-label={`${l.speaker} at ${clock(l.start_ms)}: ${l.snippet}${st !== "ok" ? `. ${CITE_STATUS_COPY[st]}` : ""}. Open in transcript`}
                    >
                      {content}
                    </button>
                  ) : (
                    <span
                      key={l.segment_id}
                      className="px-1 leading-[1.5]"
                      style={{ color: "var(--text-2)" }}
                    >
                      {content}
                    </span>
                  );
                })}
              </div>
            ))}
          </div>
        </details>
      )}

      {reply &&
        reply.proposals.map((p) => (
          <ProposalCard
            key={p.id}
            proposal={p}
            state={chat.proposals.get(p.id)}
            current={p.note_id === insertNoteId}
            meetingTitle={titleOf(p.meeting_id)}
            onApprove={() => void approve(p)}
          />
        ))}

      {refusals.length > 0 && (
        <ul className="m-0 flex list-none flex-col gap-0.5 px-1" data-testid="ask-refusals">
          {refusals.map((r, i) => (
            <li key={i} className="text-[12px]" style={{ color: "var(--text-3)" }}>
              {r}
            </li>
          ))}
        </ul>
      )}

      {/* connector calls Ask proposes for this question (connectors beta) */}
      <AskConnectorCards turnId={turn.id} />
      {reply?.trace?.connectors_off_keep_local && (
        <p
          className="m-0 flex items-start gap-1.5 px-1 text-[12px]"
          style={{ color: "var(--text-3)" }}
          data-testid="ask-connectors-off"
        >
          <Lock size={12} strokeWidth={1.75} aria-hidden="true" className="mt-0.5 flex-none" />
          {KEEP_LOCAL_NOTICE}
        </p>
      )}

      {answered && insertNoteId && (
        <div className="flex flex-wrap items-center gap-2">
          {chat.inserted.has(turn.id) ? (
            <span
              className="inline-flex items-center gap-2 px-1 text-[12px] font-semibold"
              style={{ color: "var(--success-text)" }}
            >
              ✓ Added to your note as an AI block with its sources
              {onShowInserted && (
                <button
                  type="button"
                  onClick={onShowInserted}
                  className="cursor-pointer border-0 bg-transparent p-0 text-[12px] font-semibold underline"
                  style={{ color: "var(--primary-text)" }}
                >
                  Show
                </button>
              )}
            </span>
          ) : (
            <Button
              variant="soft"
              size="xs"
              title="Add this answer to your note, with its sources"
              disabled={inserting}
              onClick={() => void insert()}
              style={
                // The newest answer breathes gently so it's clear this is
                // how you keep it; earlier answers sit quiet.
                isLastAnswer && !chat.busy
                  ? { animation: "fly-pulse-glow 2s ease-in-out infinite" }
                  : undefined
              }
            >
              {inserting ? "Inserting…" : "Insert into note"}
            </Button>
          )}
          {isLastAnswer && !chat.inserted.has(turn.id) && (
            <span className="text-[12px]" style={{ color: "var(--text-3)" }}>
              Ask only changes your note when you click this.
            </span>
          )}
          {insertError && (
            <span className="text-[12px]" style={{ color: "var(--error-text)" }} role="alert">
              Couldn't insert it, so nothing changed. ({insertError})
            </span>
          )}
        </div>
      )}
    </div>
  );
}
