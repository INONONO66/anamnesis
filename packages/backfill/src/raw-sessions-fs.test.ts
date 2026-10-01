import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { collectGjcRaw } from "./gjcraw.ts";
import { collectOmoRaw } from "./omoraw.ts";
import { walkSorted } from "./walk.ts";

const line = (value: object | string) => JSON.stringify(value);
const header = (id: string | number) => line({ type: "session", id });
const message = (id: string, timestamp: string, role: string, text: string) => line({ type: "message", id, timestamp, message: { role, content: [{ type: "text", text }] } });
const compaction = (id: string, timestamp: string, summary: string) => line({ type: "compaction", id, timestamp, summary });
const transcript = (session: string, ...events: string[]) => [header(session), ...events].join("\n");

let root: string;
const at = (...parts: string[]) => join(root, ...parts);

async function walkFixture(): Promise<void> {
  const dir = at("walk");
  await mkdir(join(dir, "b"), { recursive: true });
  await mkdir(join(dir, "a"), { recursive: true });
  await writeFile(join(dir, "z.txt"), "");
  await writeFile(join(dir, "._z.txt"), "");
  await writeFile(join(dir, "b", "2.txt"), "");
  await writeFile(join(dir, "b", "1.txt"), "");
  await writeFile(join(dir, "a", "x.txt"), "");
  await symlink(join(dir, "b"), join(dir, "c-link"));
}

async function omoFixture(): Promise<void> {
  const home = at("omo");
  const derived = join(home, "Develop", "proj", ".omo", "runtime", "transcripts", "s-derived");
  await mkdir(join(home, ".omo", "sessions"), { recursive: true });
  await mkdir(derived, { recursive: true });
  await writeFile(join(home, ".omo", "sessions", "native.jsonl"), [
    message("before-header", "2026-01-01T00:00:00Z", "user", "dropped: no session yet"),
    header("s-native"),
    "",
    "{not json",
    line(["an", "array"]),
    line({ type: "message", id: "no-time", message: { role: "user", content: [{ type: "text", text: "dropped" }] } }),
    message("m-2", "2026-01-01T00:00:02Z", "assistant", "second"),
    compaction("c-1", "2026-01-01T00:00:03Z", "summary"),
    header(""),
    message("m-orphan", "2026-01-01T00:00:04Z", "user", "dropped: header without id"),
  ].join("\n"));
  await writeFile(join(home, ".omo", "sessions", "._native.jsonl"), transcript("s-sidecar", message("side-1", "2026-01-01T00:00:00Z", "user", "AppleDouble twin")));
  await writeFile(join(home, ".omo", "sessions", "notes.txt"), transcript("s-notes", message("t-1", "2026-01-01T00:00:00Z", "user", "never opened")));
  await writeFile(join(derived, "transcript.jsonl"), transcript("s-derived", message("d-1", "2026-01-01T00:00:00Z", "user", "derived copy")));
  await writeFile(join(derived, "child.jsonl"), transcript("s-child", message("m-1", "2026-01-01T00:00:01Z", "user", "first")));

  const conversations = new Database(join(home, ".omo", "conversations.db"));
  conversations.run("CREATE TABLE messages (id TEXT, session_id TEXT, role TEXT, content TEXT, created_at TEXT)");
  const insert = conversations.prepare("INSERT INTO messages VALUES (?, ?, ?, ?, ?)");
  const rows: [string | null, string | null, string | null, string | Uint8Array | null, string | null][] = [
    ["db-3", "s-db", "user", "plain text", "2026-01-01T00:00:00Z"],
    ["db-4", "s-db", "assistant", JSON.stringify([{ type: "text", text: "part one" }, { type: "thinking", text: "hidden" }, { type: "text", text: "part two" }]), "2026-01-01T00:00:05Z"],
    ["db-5", "s-db", "user", JSON.stringify({ text: "object text" }), "2026-01-01T00:00:06Z"],
    ["db-6", "s-db", "user", "[not json", "2026-01-01T00:00:07Z"],
    ["db-7", "s-db", "user", JSON.stringify({ note: "no text member" }), "2026-01-01T00:00:08Z"],
    ["db-8", "s-db", "user", JSON.stringify([{ type: "toolCall", text: "only plumbing" }]), "2026-01-01T00:00:09Z"],
    ["db-9", "s-db", "toolResult", "wrong role", "2026-01-01T00:00:10Z"],
    ["db-10", "s-db", "user", "   ", "2026-01-01T00:00:11Z"],
    ["db-11", "s-db", "user", "bad time", "not a date"],
    [null, "s-db", "user", "no id", "2026-01-01T00:00:12Z"],
    ["db-13", null, "user", "no session", "2026-01-01T00:00:13Z"],
    ["db-14", "s-db", null, "no role", "2026-01-01T00:00:14Z"],
    ["db-15", "s-db", "user", new TextEncoder().encode("blob content"), "2026-01-01T00:00:15Z"],
    ["db-16", "s-db", "user", "no time", null],
    ["db-17", "s-db", "user", "", "2026-01-01T00:00:16Z"],
  ];
  for (const row of rows) insert.run(...row);
  conversations.close();
  const index = new Database(join(home, "Develop", "proj", "codegraph.db"));
  index.run("CREATE TABLE nodes (id TEXT)");
  index.run("INSERT INTO nodes VALUES ('ignored')");
  index.close();
}

async function gjcFixture(): Promise<void> {
  const home = at("gjc");
  const sessions = join(home, "home", ".gjc", "agent", "sessions");
  await mkdir(join(sessions, "ws-a", "sub"), { recursive: true });
  await mkdir(join(sessions, "ws-b"), { recursive: true });
  await writeFile(join(sessions, "ws-a", "parent.jsonl"), [
    message("pre", "2026-02-01T00:00:00Z", "user", "dropped before header"),
    header("s-parent"),
    "",
    line("a string record"),
    message("a-2", "2026-02-01T00:00:02Z", "user", "parent turn"),
    line({ type: "message", id: "a-tool", timestamp: "2026-02-01T00:00:03Z", message: { role: "toolResult", content: [{ type: "text", text: "plumbing" }] } }),
    header(7),
    message("a-after", "2026-02-01T00:00:04Z", "user", "dropped: header without string id"),
  ].join("\n"));
  await writeFile(join(sessions, "ws-a", "1.bash.log"), "not a transcript");
  await writeFile(join(sessions, "ws-a", "sub", "child.jsonl"), transcript("s-child", message("a-1", "2026-02-01T00:00:02Z", "assistant", "child turn"), compaction("z-0", "2026-02-01T00:00:01Z", "child summary")));
  await writeFile(join(sessions, "ws-b", "._other.jsonl"), transcript("s-sidecar", message("side-1", "2026-02-01T00:00:00Z", "user", "AppleDouble twin")));
  await writeFile(join(sessions, "ws-b", "torn.jsonl"), [header("s-torn"), "{not json", message("t-1", "2026-02-01T00:00:05Z", "user", "after the torn line")].join("\n"));
  await writeFile(join(home, "home", "stray.jsonl"), transcript("s-stray", message("x-1", "2026-02-01T00:00:00Z", "user", "outside sessions root")));
}

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), "raw-sessions-"));
  await Promise.all([walkFixture(), omoFixture(), gjcFixture()]);
});
afterAll(async () => { await rm(root, { recursive: true, force: true }); });

describe("walkSorted", () => {
  test("visits files depth-first in name order, skips AppleDouble sidecars, hands a symlink over without descending it, and treats a missing root as empty", async () => {
    const dir = at("walk");
    const seen: [string, boolean][] = [];
    await walkSorted(dir, (entry, path) => seen.push([path.slice(dir.length + 1), entry.isFile()]));
    expect(seen).toEqual([["a/x.txt", true], ["b/1.txt", true], ["b/2.txt", true], ["c-link", false], ["z.txt", true]]);
    const none: string[] = [];
    await walkSorted(at("missing"), (_, path) => none.push(path));
    expect(none).toEqual([]);
  });
});

describe("collectOmoRaw", () => {
  test("reads native transcripts and sqlite message tables from both homes, skips derived copies, sidecars and junk, and sorts by event time across sources", async () => {
    const episodes = await collectOmoRaw(at("omo"));
    expect(episodes.map(e => [e.input.origin.record, e.input.origin.session, e.input.origin.actor, e.input.content])).toEqual([
      ["db-3", "s-db", "user", "plain text"],
      ["m-1", "s-child", "user", "first"],
      ["m-2", "s-native", "assistant", "second"],
      ["c-1", "s-native", "unknown", "summary"],
      ["db-4", "s-db", "assistant", "part one\npart two"],
      ["db-5", "s-db", "user", "object text"],
      ["db-6", "s-db", "user", "[not json"],
    ]);
    expect(episodes.map(e => e.input.properties)).toEqual([
      { kind: "message", role: "user" }, { kind: "message", role: "user" }, { kind: "message", role: "assistant" }, { kind: "compaction" },
      { kind: "message", role: "assistant" }, { kind: "message", role: "user" }, { kind: "message", role: "user" },
    ]);
    expect(episodes.map(e => e.input.time?.value)).toEqual(["2026-01-01T00:00:00.000Z", "2026-01-01T00:00:01.000Z", "2026-01-01T00:00:02.000Z", "2026-01-01T00:00:03.000Z", "2026-01-01T00:00:05.000Z", "2026-01-01T00:00:06.000Z", "2026-01-01T00:00:07.000Z"]);
    expect(episodes.map(e => e.input.origin.source)).toEqual(Array<string>(7).fill("omo"));
  });

  test("a missing snapshot yields no episodes", async () => {
    expect(await collectOmoRaw(at("absent"))).toEqual([]);
  });
});

describe("collectGjcRaw", () => {
  test("walks home/.gjc/agent/sessions recursively, keys events by the session header, skips sidecars and torn lines, and orders by event time before record", async () => {
    const episodes = await collectGjcRaw(at("gjc"));
    expect(episodes.map(e => [e.input.origin.record, e.input.origin.session, e.input.content, e.input.properties])).toEqual([
      ["z-0", "s-child", "child summary", { canonical_kind: "compaction", kind: "compaction" }],
      ["a-1", "s-child", "child turn", { canonical_kind: "agent_message", kind: "message" }],
      ["a-2", "s-parent", "parent turn", { canonical_kind: "agent_message", kind: "message" }],
      ["t-1", "s-torn", "after the torn line", { canonical_kind: "agent_message", kind: "message" }],
    ]);
    expect(episodes.map(e => e.input.time?.value)).toEqual(["2026-02-01T00:00:01.000Z", "2026-02-01T00:00:02.000Z", "2026-02-01T00:00:02.000Z", "2026-02-01T00:00:05.000Z"]);
    expect(episodes.map(e => e.input.origin.source)).toEqual(Array<string>(4).fill("gjc"));
  });

  test("a missing snapshot yields no episodes", async () => {
    expect(await collectGjcRaw(at("absent"))).toEqual([]);
  });
});
