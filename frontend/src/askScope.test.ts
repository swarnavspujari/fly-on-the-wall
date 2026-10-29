import { describe, expect, it } from "vitest";
import {
  ALL_SCOPE,
  mentionAt,
  mentionOptions,
  mentionScope,
  namedSpeakers,
  removeMention,
  sameScope,
  thisScope,
} from "./askScope";

const CATALOG = {
  meetings: [
    { id: "n1", title: "Q3 planning offsite" },
    { id: "n2", title: "Weekly 1:1 with Dana" },
    { id: "n3", title: "Design review" },
  ],
  people: ["Dana Osei", "dana osei", "Marc"],
  folders: [{ id: "f1", name: "Clients" }],
};

describe("mentionAt", () => {
  it("finds the @ being typed at the caret", () => {
    expect(mentionAt("@q3", 3)).toEqual({ start: 0, query: "q3" });
    expect(mentionAt("about @Dana O", 13)).toEqual({ start: 6, query: "Dana O" });
    expect(mentionAt("@", 1)).toEqual({ start: 0, query: "" });
  });

  it("ignores an @ inside a word, after a newline or a double space", () => {
    expect(mentionAt("mail dana@example.com", 21)).toBeNull();
    expect(mentionAt("@dana\nnext", 10)).toBeNull();
    expect(mentionAt("@dana  then", 11)).toBeNull();
    expect(mentionAt("@ space", 7)).toBeNull();
    expect(mentionAt("no at here", 10)).toBeNull();
  });
});

describe("mentionOptions", () => {
  it("matches the start of each word, meetings then people then folders", () => {
    // (the typed name comes last, for asking about a person by name)
    expect(mentionOptions("q3 pl", CATALOG).map((o) => o.label)).toEqual([
      "Q3 planning offsite",
      "q3 pl",
    ]);
    const dana = mentionOptions("dana", CATALOG);
    expect(dana.map((o) => [o.kind, o.label])).toEqual([
      ["meeting", "Weekly 1:1 with Dana"],
      ["person", "Dana Osei"],
      ["person", "dana"],
    ]);
    expect(dana[2].typed).toBe(true);
  });

  it("puts a typed name last, and leaves it out when it's an exact match", () => {
    const opts = mentionOptions("cli", CATALOG);
    expect(opts[0]).toMatchObject({ kind: "folder", id: "f1" });
    expect(opts[opts.length - 1]).toMatchObject({ kind: "person", typed: true, id: "cli" });
    expect(mentionOptions("Marc", CATALOG).some((o) => o.typed)).toBe(false);
    expect(mentionOptions("m", CATALOG).some((o) => o.typed)).toBe(false); // too short
  });

  it("offers a few of each before you type", () => {
    const opts = mentionOptions("", CATALOG);
    expect(opts.filter((o) => o.kind === "meeting")).toHaveLength(3);
    // duplicates (by case) collapse
    expect(opts.filter((o) => o.kind === "person").map((o) => o.label)).toEqual([
      "Dana Osei",
      "Marc",
    ]);
  });
});

describe("scope", () => {
  it("removes the @ query from the text", () => {
    expect(removeMention("@q3", 0, 3)).toBe("");
    expect(removeMention("what about @q3 then", 11, 14)).toBe("what about then");
    expect(removeMention("ask @da", 4, 7)).toBe("ask ");
  });

  it("a pick becomes the harness scope", () => {
    expect(mentionScope({ kind: "meeting", id: "n1", label: "Q3" })).toEqual({
      scope: { kind: "note", id: "n1" },
      label: "@Q3",
      origin: "mention",
    });
    expect(mentionScope({ kind: "folder", id: "f1", label: "Clients" }).scope).toEqual({
      kind: "folder",
      id: "f1",
    });
    expect(mentionScope({ kind: "person", id: "Dana", label: "Dana" }).scope).toEqual({
      kind: "person",
      id: "Dana",
    });
  });

  it("this meeting / this note / all meetings", () => {
    expect(thisScope("n1", true).label).toBe("This meeting");
    expect(thisScope("n1", false).label).toBe("This note");
    expect(sameScope(thisScope("n1", true).scope, { kind: "note", id: "n1" })).toBe(true);
    expect(sameScope(ALL_SCOPE.scope, { kind: "note", id: "n1" })).toBe(false);
  });

  it("suggests named speakers only", () => {
    expect(namedSpeakers(["You", "Speaker 2", "Dana", " ", "unknown", "Marc"])).toEqual([
      "Dana",
      "Marc",
    ]);
  });
});
