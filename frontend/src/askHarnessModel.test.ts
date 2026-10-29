import { describe, expect, it } from "vitest";
import {
  KEPT_LOCAL_NOT_SEARCHED,
  answerRunsOn,
  describeAskError,
  flagCopy,
  isCancelled,
  notFoundCopy,
  proposalExpired,
  refusalCopy,
  stageCopy,
  tierLabel,
  traceDetails,
  traceSummary,
} from "./askModel";
import type { AskReply } from "./types";

/* The Ask harness's copy (askModel.ts): built from counts and codes only. */

const reply = (extra: Partial<AskReply> = {}): AskReply => ({
  status: "answered",
  text: "Secret plan for Project Falcon.",
  sentences: [{ text: "Secret plan for Project Falcon.", citations: ["s1"], status: "sourced" }],
  sources: [
    {
      meeting_id: "m1",
      meeting_title: "Falcon sync",
      started_at: null,
      label: "Falcon sync",
      lines: [
        { segment_id: "s1", speaker: "Dana", start_ms: 0, end_ms: 10, snippet: "Secret plan" },
      ],
    },
  ],
  proposals: [],
  refusals: [],
  trace_lines: [],
  on_device: true,
  trace: {
    tier: "M",
    scope: "all",
    kept_local_not_searched: 2,
    meetings_in_scope: 7,
    passages: 8,
    items: 1,
    tool_calls: [
      { tool: "tool_search_passages", ok: true },
      { tool: "tool_get_briefing", ok: false },
    ],
    retried: true,
    context_truncated: true,
    floor_not_found: false,
    budget_forced: true,
  },
  timing: { first_token_ms: null, total_ms: 4200 },
  run_log: {
    reach: "reach_on_device",
    provider: "provider_ollama",
    prefetch: { searches: 1, semantic: false, memory_facts: 0 },
  },
  ...extra,
});

describe("the harness's model failures", () => {
  it.each([
    [
      "The model's answer was cut off before it finished. Try again, or ask a narrower question.",
      "The answer was cut off",
    ],
    [
      "The model returned an empty answer. Try again, or switch to another model in Settings.",
      "The answer came back empty",
    ],
    [
      "Ask used up its budget before the model answered.",
      "Ask ran out of room before the model answered",
    ],
    [
      "The model's tool calls couldn't be understood, so there's no answer. Try again, or switch to another model.",
      "The model didn't follow Ask's steps",
    ],
    ["Couldn't read your meetings to answer that.", "Ask couldn't read your meetings"],
  ])("%s → %s", (raw, title) => {
    const v = describeAskError(raw, "ollama");
    expect(v.title).toBe(title);
    expect(v.raw).toBe(raw);
  });

  it("knows the backend's cancel", () => {
    expect(isCancelled("Ask was cancelled.")).toBe(true);
    expect(isCancelled("The model returned an empty answer.")).toBe(false);
  });
});

describe("trace", () => {
  it("summarizes from counts: meetings, passages, looking further, memory", () => {
    expect(traceSummary(reply())).toBe(
      "Searched 7 meetings · read 8 passages · looked further 2 times · used memory",
    );
    expect(
      traceSummary(
        reply({
          trace: { ...reply().trace!, scope: "note", tool_calls: [], passages: 1 },
          run_log: { prefetch: { memory_facts: 0 } },
        }),
      ),
    ).toBe("Searched this meeting · read 1 passage");
    expect(traceSummary(reply({ trace: { ...reply().trace!, floor_not_found: true } }))).toContain(
      "nothing matched",
    );
  });

  it("lists every step and never any meeting text", () => {
    const r = reply();
    const lines = traceDetails(r, answerRunsOn(r, "ollama"), "llama3.1");
    expect(lines).toEqual([
      "Searched 7 meetings",
      "Ran 1 search by keyword",
      "Read 8 passages and 1 extracted item",
      "search_passages · ok",
      "get_briefing · failed",
      "Asked the model again",
      "Stopped looking to stay within the answer's budget",
      "The model may not have read every excerpt",
      "Model: llama3.1",
      "Multi-step: the model could search again, up to 3 times.",
      "Ran on this computer",
      "Answered in 4.2 s",
    ]);
    const all = [traceSummary(r), ...lines].join(" ");
    for (const meetingText of ["Secret", "Falcon", "Dana"]) expect(all).not.toContain(meetingText);
  });

  it("never states the kept-local count", () => {
    expect(KEPT_LOCAL_NOT_SEARCHED).not.toMatch(/\d/);
    expect(traceDetails(reply(), answerRunsOn(reply(), "ollama"), null).join(" ")).not.toContain(
      "kept-local",
    );
  });
});

describe("where an answer ran", () => {
  it("reads the run log's reach, naming the vendor for the cloud", () => {
    expect(answerRunsOn(reply(), "ollama")).toEqual({ kind: "local", label: "On this computer" });
    expect(
      answerRunsOn(reply({ run_log: { reach: "reach_local_remote" }, on_device: false }), "ollama"),
    ).toEqual({ kind: "remote", label: "On another computer" });
    expect(
      answerRunsOn(
        reply({
          run_log: { reach: "reach_cloud", provider: "provider_anthropic" },
          on_device: false,
        }),
        "openai",
      ),
    ).toEqual({ kind: "cloud", label: "Anthropic" });
    // an older reply without a run log: on_device and the provider it went to
    expect(answerRunsOn(reply({ run_log: undefined, on_device: false }), "nim")).toEqual({
      kind: "cloud",
      label: "NVIDIA",
    });
  });

  it("says the tier plainly", () => {
    expect(tierLabel("S")).toBe("one step");
    expect(tierLabel("M")).toBe("multi-step");
    expect(tierLabel("C")).toBe("multi-step");
    expect(tierLabel(null)).toBeNull();
  });
});

describe("copy", () => {
  it("stages", () => {
    expect(stageCopy("searching", { kind: "note", id: "n" })).toBe("Searching this meeting…");
    expect(stageCopy("searching", { kind: "all" })).toBe("Searching your meetings…");
    expect(stageCopy(null, { kind: "all" })).toBe("Getting ready…");
  });

  it("not found offers to widen only a narrow scope", () => {
    expect(notFoundCopy({ kind: "note", id: "n" })).toMatchObject({
      title: "Not found in this meeting",
      widen: true,
    });
    expect(notFoundCopy({ kind: "person", id: "Dana" })).toMatchObject({
      title: "Not found in your meetings",
      widen: false,
    });
  });

  it("flags say which claim failed", () => {
    expect(flagCopy({ kind: "number", detail: "84000" })).toBe(
      "The number “84000” isn't in the lines it cites.",
    );
    expect(flagCopy({ kind: "date" })).toBe("A date here isn't in the lines it cites.");
    expect(flagCopy({ kind: "uncited_claim" })).toBe("This states a fact without a source.");
    expect(flagCopy({ kind: "invalid_citation" })).toBe("It pointed to a line Ask didn't read.");
  });

  it("refusals are said, except duplicates", () => {
    expect(refusalCopy({ tool: "insert_into_note", reason: "outside_scope" })).toBe(
      "Ask suggested adding to a note but didn't offer it: it pointed outside what you asked about.",
    );
    expect(refusalCopy({ tool: "add_action_item", reason: "duplicate" })).toBeNull();
    expect(refusalCopy({ tool: null, reason: "bad_arguments" })).toBe(
      "Ask tried a step that doesn't exist, so it was skipped.",
    );
  });

  it("proposals expire", () => {
    const now = Date.parse("2026-10-29T12:00:00Z");
    expect(proposalExpired("2026-10-29T11:59:59Z", now)).toBe(true);
    expect(proposalExpired("2026-10-29T12:30:00Z", now)).toBe(false);
    expect(proposalExpired("not a date", now)).toBe(false);
  });
});
