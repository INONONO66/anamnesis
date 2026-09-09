import { expect, spyOn, test } from "bun:test";
import { createHash } from "node:crypto";
import { createMiscRawParser, collectMiscRaw } from "./miscraw.ts";
import { Database } from "bun:sqlite";
import * as fs from "node:fs/promises";
import { lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const time = 1782794590501;
const hash = (text: string) => createHash("sha256").update(text).digest("hex");
test("Aside preserves nonblank ordinal, index metadata, prose-only extraction and native revision", () => {
  const parse = createMiscRawParser({ source: "aside", session: "native", properties: { session_title: "title", cwd: "/work" } }, true);
  expect(parse(" ")).toEqual([]);
  expect(parse(JSON.stringify({ role: "system-message", content: "harness" }))).toEqual([]);
  const raw = "use ghp_0123456789012345678901234567890123456789";
  const [e] = parse(JSON.stringify({ role: "assistant", timestamp: time, content: [{ type: "thinking", text: "hidden" }, { type: "text", text: raw }, { type: "toolCall", text: "hidden" }] }));
  expect(e!.input.origin).toEqual({ source: "aside", session: "native", actor: "assistant", record: "native:1" });
  expect(e!.input.properties).toEqual({ kind: "message", session_title: "title", cwd: "/work" });
  expect(e!.input.content).toBe("use [REDACTED]"); expect(e!.redactions).toBe(1);
  expect(e!.input.source_revision).toBe(hash(new Date(time).toISOString() + "\n" + raw));
});
test("Antigravity uses producer step ID and request body, excludes tools and harness", () => {
  const parse = createMiscRawParser({ source: "gemini-antigravity", session: "brain" }, true);
  const event = { type: "USER_INPUT", step_index: 42, created_at: new Date(time).toISOString(), content: "<USER_REQUEST>\nactual\n</USER_REQUEST>\nmetadata" };
  expect(parse(JSON.stringify(event))[0]!.input).toMatchObject({ content: "actual", origin: { record: "brain:42", actor: "user" }, properties: { kind: "USER_INPUT" } });
  for (const type of ["RUN_COMMAND", "EPHEMERAL_MESSAGE", "SYSTEM_MESSAGE"]) expect(parse(JSON.stringify({ ...event, type }))).toEqual([]);
});
test("OpenCode preserves file event time, paste text and mode; large body is masked payload", () => {
  const parse = createMiscRawParser({ source: "opencode", occurredAt: time }, true);
  const text = "x".repeat(5000);
  const [e] = parse(JSON.stringify({ input: "paste", parts: [{ type: "text", text }, { type: "file", text: "hidden" }], mode: "normal" }));
  expect(e!.input.origin).toEqual({ source: "opencode", session: "prompt-history", record: "prompt-history:0", actor: "user" });
  expect(e!.input.time!.value).toBe(new Date(time).toISOString());
  expect(Buffer.from(e!.input.payload!).toString()).toBe("paste\n" + text);
  expect(e!.input.content).toHaveLength(4000); expect(e!.input.properties).toEqual({ kind: "prompt", mode: "normal" });
});
for (const mode of ["success", "wal", "query-error", "open-error"] as const) {
  test(`collectMiscRaw releases exact SQLite scratch: ${mode}`, async () => {
    const root = await mkdtemp(join(tmpdir(), "miscraw-cleanup-test-"));
    const ownedScratch: string[] = [];
    let restore: (() => void) | undefined;
    try {
      const user = join(root, "aside", "home", ".aside", "u", "0");
      const session = join(user, "sessions", "2026-01-01_session");
      await mkdir(session, { recursive: true });
      const transcript = join(session, "messages.jsonl");
      await writeFile(transcript, JSON.stringify({ role: "user", timestamp: 1, content: "hello" }) + "\n");
      const state = join(user, "state.db");
      if (mode === "open-error") await writeFile(state, "not a SQLite database");
      else {
        const db = new Database(state);
        try {
          if (mode === "wal") db.run("pragma journal_mode = wal");
          if (mode === "query-error") db.run("create table unrelated (value text)");
          else {
            db.run("create table sessions (id text primary key, title text, cwd text)");
            db.run("insert into sessions values ('session', 'title', '/work')");
          }
        } finally { db.close(false); }
      }
      const names = (await readdir(user)).sort();
      if (mode === "wal") expect(names).toContain("state.db-wal");
      const paths = [transcript, ...names.filter(name => name.startsWith("state.db")).map(name => join(user, name))];
      const before = await Promise.all(paths.map(async path => ({ path, bytes: await readFile(path), info: await lstat(path) })));
      // Observe real allocations, without replacing the filesystem or SQLite.
      const allocate = fs.mkdtemp;
      const observer = spyOn(fs, "mkdtemp").mockImplementation(allocate);
      restore = () => observer.mockRestore();
      try {
        if (mode === "query-error" || mode === "open-error") await expect(collectMiscRaw(root)).rejects.toThrow();
        else {
          const episodes = await collectMiscRaw(root);
          expect(episodes).toHaveLength(1);
          expect(episodes[0]!.input.properties).toEqual({ kind: "message", session_title: "title", cwd: "/work" });
        }
      } finally {
        for (const result of observer.mock.results) {
          if (result.type === "return") {
            const path = await result.value;
            if (typeof path !== "string") throw new Error("unexpected scratch path encoding");
            ownedScratch.push(path);
          }
        }
        restore();
      }
      expect(ownedScratch).toHaveLength(1);
      for (const entry of before) {
        expect(await readFile(entry.path)).toEqual(entry.bytes);
        expect((await lstat(entry.path)).mtimeMs).toBe(entry.info.mtimeMs);
      }
      expect((await readdir(user)).sort()).toEqual(names);
      const remaining = [];
      for (const path of ownedScratch) {
        try { await lstat(path); remaining.push(path); }
        catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
      }
      console.log(JSON.stringify({ event: "miscraw_cleanup", mode, ownedScratch, remaining, source_bytes_unchanged: true }));
      expect(remaining).toEqual([]);
    } finally {
      restore?.();
      // A failing baseline control also owns its allocations; never sweep /tmp.
      await Promise.all([root, ...ownedScratch].map(path => rm(path, { recursive: true, force: true })));
    }
  });
}

test("strict parsing rejects malformed records while legacy parsing retains tolerant skip", () => {
  for (const text of ["{", "[]", '{"role":"user","content":"x","timestamp":"bad"}']) {
    expect(() => createMiscRawParser({ source: "aside", session: "s" }, true)(text)).toThrow();
    expect(createMiscRawParser({ source: "aside", session: "s" })(text)).toEqual([]);
  }
});
