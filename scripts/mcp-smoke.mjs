#!/usr/bin/env node
// End-to-end smoke for the MCP server over real stdio: spawns the binary
// against a data dir and calls EVERY tool, asserting each answers sanely.
// The single write (set_speaker_label) is exercised as a no-op: it re-sets
// a speaker's existing label, so the data dir is byte-identical afterwards.
//
//   node scripts/mcp-smoke.mjs [dataDir] [--bin path\to\flyonthewall-mcp.exe]

import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { existsSync } from "node:fs";
import path from "node:path";

const args = process.argv.slice(2);
const binFlag = args.indexOf("--bin");
const bin =
  binFlag >= 0
    ? args[binFlag + 1]
    : path.join("target", "release", "flyonthewall-mcp.exe");
const dataDir =
  args.find((a, i) => !a.startsWith("--") && i !== binFlag + 1) ??
  path.join(process.env.APPDATA ?? "", "FlyOnTheWall");

if (!existsSync(bin)) {
  console.error(`server binary not found: ${bin} (build with: cargo build --release -p fly-mcp)`);
  process.exit(2);
}
console.log(`server: ${bin}\ndata:   ${dataDir}\n`);

const child = spawn(bin, ["--data-dir", dataDir], {
  stdio: ["pipe", "pipe", "inherit"],
});
const lines = createInterface({ input: child.stdout });
const pending = new Map();
let nextId = 1;
lines.on("line", (line) => {
  try {
    const msg = JSON.parse(line);
    const waiter = pending.get(msg.id);
    if (waiter) {
      pending.delete(msg.id);
      waiter(msg);
    }
  } catch {
    /* startup banner on stderr only; stdout should always be JSON */
  }
});

function rpc(method, params) {
  const id = nextId++;
  const p = new Promise((resolve, reject) => {
    pending.set(id, resolve);
    setTimeout(() => {
      if (pending.delete(id)) reject(new Error(`timeout waiting for ${method}`));
    }, 15000);
  });
  child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
  return p;
}
const toolText = async (name, args = {}) => {
  const resp = await rpc("tools/call", { name, arguments: args });
  if (resp.error) throw new Error(`${name}: rpc error ${JSON.stringify(resp.error)}`);
  const text = resp.result?.content?.[0]?.text ?? "";
  return { text, isError: resp.result?.isError === true };
};

let failures = 0;
const check = (name, ok, detail) => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? ` — ${detail}` : ""}`);
  if (!ok) failures++;
};

try {
  const init = await rpc("initialize", {
    protocolVersion: "2024-11-05",
    capabilities: {},
    clientInfo: { name: "smoke", version: "0" },
  });
  check(
    "initialize",
    init.result?.serverInfo?.name === "flyonthewall" &&
      init.result?.instructions?.includes("get_context"),
    `v${init.result?.serverInfo?.version}`
  );

  const tools = await rpc("tools/list", {});
  const names = (tools.result?.tools ?? []).map((t) => t.name);
  const expected = [
    "get_context", "whats_changed", "open_items", "query_items", "get_meeting_items",
    "search_notes", "list_folders", "get_note", "get_transcript", "get_transcripts",
    "get_meeting", "list_recent", "set_speaker_label",
  ];
  check(
    "tools/list",
    expected.every((n) => names.includes(n)),
    `${names.length} tools`
  );

  // ---- browse to find real ids ----
  const recent = await toolText("list_recent", { limit: 25 });
  check("list_recent", !recent.isError && recent.text.length > 0);
  const noteId = recent.text.match(/note_id: ([^,)]+)/)?.[1];
  const meetingId = recent.text.match(/meeting_id: ([^,)\s]+)/)?.[1];

  const folders = await toolText("list_folders");
  check("list_folders", !folders.isError);

  if (noteId) {
    const note = await toolText("get_note", { note_id: noteId });
    check("get_note", !note.isError && note.text.includes(noteId));
  } else {
    check("get_note", false, "no note_id found in list_recent");
  }

  if (meetingId) {
    const meeting = await toolText("get_meeting", { meeting_id: meetingId });
    check("get_meeting", !meeting.isError && meeting.text.includes(meetingId));

    const t = await toolText("get_transcript", {
      meeting_id: meetingId,
      with_segment_ids: true,
    });
    // a meeting may legitimately have no transcript yet
    check("get_transcript", t.text.length > 0, t.isError ? "no transcript (ok)" : "rendered");
    // parse ONLY the legend line ("Speakers: You (key: mic), …") so the
    // captured label can never span other lines of the transcript
    const legend = t.text.split("\n").find((l) => l.startsWith("Speakers: ")) ?? "";
    const pair = legend.replace("Speakers: ", "").match(/^\s*([^,(]+?) \(key: ([^)]+)\)/);
    const label = pair?.[1]?.trim();
    const speakerKey = pair?.[2];

    const many = await toolText("get_transcripts", { meeting_ids: [meetingId] });
    check("get_transcripts", many.text.length > 0);

    const items = await toolText("get_meeting_items", { meeting_id: meetingId });
    check("get_meeting_items", !items.isError, items.text.split("\n")[0]);

    if (speakerKey && label && !t.isError) {
      // no-op write: re-set the current label; data identical afterwards
      const w = await toolText("set_speaker_label", {
        meeting_id: meetingId,
        speaker_key: speakerKey,
        label,
      });
      check("set_speaker_label", !w.isError && w.text.includes(label), `${speakerKey} → "${label}" (no-op)`);
    } else {
      check("set_speaker_label", true, "skipped (no transcript on sampled meeting)");
    }
  } else {
    for (const n of ["get_meeting", "get_transcript", "get_transcripts", "get_meeting_items", "set_speaker_label"])
      check(n, false, "no meeting_id found in list_recent");
  }

  const search = await toolText("search_notes", { query: "meeting", limit: 5 });
  check("search_notes", !search.isError);

  const q = await toolText("query_items", { type: "action_item" });
  check("query_items", !q.isError, q.text.split("\n")[0]);

  const open = await toolText("open_items", {});
  check("open_items", !open.isError, open.text.split("\n")[0]);

  // context on the most recent meeting's title words
  const title = recent.text.match(/^- (.+?) \(note_id/m)?.[1] ?? "meeting";
  const ctx = await toolText("get_context", { query: title });
  check("get_context", !ctx.isError, ctx.text.split("\n")[0]);

  const wc = await toolText("whats_changed", { series: title, since: "2020-01-01" });
  check("whats_changed", !wc.isError, wc.text.split("\n")[0]);
} catch (e) {
  check("run", false, String(e));
} finally {
  child.stdin.end(); // EOF → server must exit on its own (lifecycle contract)
  const exited = await new Promise((resolve) => {
    const t = setTimeout(() => resolve(false), 5000);
    child.on("exit", () => {
      clearTimeout(t);
      resolve(true);
    });
  });
  check("exit-on-stdin-EOF", exited);
  console.log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILURE(S)`);
  process.exit(failures === 0 ? 0 : 1);
}
