import { afterAll, beforeAll, describe, expect, spyOn, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ZodError } from "zod";
import type { RememberInput } from "./engine.ts";
import { EpisodeJournal, journaledRemember } from "./journal.ts";
import { legacyHashes, legacyLines } from "./legacy-journal.fixture.ts";
import type { PutResult } from "./store.ts";

const hash = (bytes: string | Uint8Array) => createHash("sha256").update(bytes).digest("hex");
const episode = (record: string): RememberInput => ({
  time: { value: "2026-09-02T10:00:00Z", precision: "second" },
  content: `Journal fixture ${record}`,
  origin: { source: "journal-test", session: "pure", actor: "test", record },
});
const persisted = (record: string) => ({ ...episode(record), schema: "anamnesis.original-message/1", mass: 0.5, properties: {} });

class RecordingEngine {
  readonly inputs: RememberInput[] = [];
  shouldFail = false;
  async remember(input: RememberInput): Promise<PutResult> {
    this.inputs.push(input);
    if (this.shouldFail) throw new Error("ingestion failed");
    return { id: `recorded-${this.inputs.length}`, created: true };
  }
}

const pendingForever = () => {
  const records: string[] = [];
  let started!: () => void;
  const firstCall = new Promise<void>(resolve => { started = resolve; });
  const engine = {
    async remember(input: RememberInput): Promise<PutResult> {
      records.push(input.origin.record);
      started();
      return new Promise<PutResult>(() => {});
    },
  };
  return { engine, firstCall, records };
};

let root: string;
let next = 0;
const directory = async () => { const dir = join(root, `j${next++}`); await mkdir(dir); return dir; };
const journalWith = async (bytes: string | Buffer) => {
  const dir = await directory();
  await writeFile(join(dir, "journal-2026-09.jsonl"), bytes);
  return new EpisodeJournal(dir);
};
beforeAll(async () => { root = await mkdtemp(join(tmpdir(), "journal-pure-")); });
afterAll(async () => { await rm(root, { recursive: true, force: true }); });

describe("append and replay", () => {
  test("appends one validated LF-terminated line per remember, with defaults filled and payload bytes as integers", async () => {
    const dir = await directory();
    const journal = new EpisodeJournal(dir, () => new Date("2026-09-02T12:34:56.000Z"));
    await journal.append({ ...episode("happy"), payload: Uint8Array.from([0, 127, 255]), payload_media_type: "application/octet-stream" });
    await journal.append(episode("plain"));
    const lines = (await readFile(join(dir, "journal-2026-09.jsonl"), "utf8")).split("\n");
    expect(lines).toHaveLength(3);
    expect(lines[2]).toBe("");
    expect(JSON.parse(lines[0]!)).toEqual({ recordedAt: "2026-09-02T12:34:56.000Z", element: { ...persisted("happy"), payload: [0, 127, 255], payload_media_type: "application/octet-stream" } });
    expect(JSON.parse(lines[1]!)).toEqual({ recordedAt: "2026-09-02T12:34:56.000Z", element: persisted("plain") });
    const engine = new RecordingEngine();
    expect(await journal.replay(engine)).toBe(2);
    expect(engine.inputs.map(i => i.origin.record)).toEqual(["happy", "plain"]);
    expect(engine.inputs[0]?.payload).toEqual(Uint8Array.from([0, 127, 255]));
    expect(engine.inputs[1]).not.toHaveProperty("payload");
  });

  test("rejects an untyped object that is not a RememberInput with the validation error before creating the journal directory", async () => {
    const parent = await directory();
    const dir = join(parent, "not-created");
    await expect(new EpisodeJournal(dir).append({ content: "missing origin and time" })).rejects.toBeInstanceOf(ZodError);
    expect(await readdir(parent)).toEqual([]);
  });

  test("rolls files by UTC month, replays months in order, and ignores files that are not journal months", async () => {
    const dir = await directory();
    const dates = [new Date("2026-10-01T00:00:00Z"), new Date("2026-09-30T23:59:59Z")];
    const journal = new EpisodeJournal(dir, () => dates.shift()!);
    await journal.append(episode("october"));
    await journal.append(episode("september"));
    expect(dates).toEqual([]);
    await writeFile(join(dir, "journal-2026-09.jsonl.bak"), "not a journal\n");
    await writeFile(join(dir, "notes.txt"), "{not json\n");
    expect((await readdir(dir)).sort()).toEqual(["journal-2026-09.jsonl", "journal-2026-09.jsonl.bak", "journal-2026-10.jsonl", "notes.txt"]);
    const engine = new RecordingEngine();
    expect(await journal.replay(engine)).toBe(2);
    expect(engine.inputs.map(i => i.origin.record)).toEqual(["september", "october"]);
  });

  test("journaledRemember persists first, so a failed ingestion is replayable", async () => {
    const dir = await directory();
    const journal = new EpisodeJournal(dir);
    const engine = new RecordingEngine();
    engine.shouldFail = true;
    await expect(journaledRemember(journal, engine, episode("failed"))).rejects.toThrow("ingestion failed");
    expect(await readdir(dir)).toHaveLength(1);
    engine.shouldFail = false;
    expect(await journaledRemember(journal, engine, episode("ok"))).toEqual({ id: "recorded-2", created: true });
    const replayer = new RecordingEngine();
    expect(await journal.replay(replayer)).toBe(2);
    expect(replayer.inputs.map(i => i.origin.record)).toEqual(["failed", "ok"]);
  });

  test("a persisted line with an unrecognized element key stops replay with that strict-parse issue after earlier entries were replayed", async () => {
    const journal = await journalWith([persisted("first"), { ...persisted("second"), extra: true }].map(element => JSON.stringify({ recordedAt: "2026-09-02T00:00:00.000Z", element }) + "\n").join(""));
    const engine = new RecordingEngine();
    const replay = journal.replay(engine);
    await expect(replay).rejects.toBeInstanceOf(ZodError);
    await expect(replay).rejects.toMatchObject({ issues: [{ code: "unrecognized_keys", keys: ["extra"], path: ["element"] }] });
    expect(engine.inputs.map(i => i.origin.record)).toEqual(["first"]);
  });
});

describe("replay abort", () => {
  test("an already-aborted signal rejects with its reason before the first remember", async () => {
    const dir = await directory();
    const journal = new EpisodeJournal(dir);
    await journal.append(episode("abort"));
    const controller = new AbortController();
    const reason = new Error("stop now");
    controller.abort(reason);
    const engine = new RecordingEngine();
    await expect(journal.replay(engine, { signal: controller.signal })).rejects.toBe(reason);
    expect(engine.inputs).toEqual([]);
  });

  test("a signal aborted between the pre-check and the remember race still rejects with its reason and removes the listener it added", async () => {
    const dir = await directory();
    const journal = new EpisodeJournal(dir);
    await journal.append(episode("racing"));
    const controller = new AbortController();
    const reason = new Error("raced");
    const added = spyOn(controller.signal, "addEventListener");
    const removed = spyOn(controller.signal, "removeEventListener");
    const engine = { remember(): Promise<PutResult> { controller.abort(reason); return new Promise<PutResult>(() => {}); } };
    await expect(journal.replay(engine, { signal: controller.signal })).rejects.toBe(reason);
    expect(added.mock.calls.map(call => call[0])).toEqual(["abort"]);
    expect(removed.mock.calls.map(call => call[0])).toEqual(["abort"]);
    expect(removed.mock.calls[0]?.[1]).toBe(added.mock.calls[0]?.[1]);
  });

  test("an explicit null abort reason is passed through verbatim, matching throwIfAborted", async () => {
    const dir = await directory();
    const journal = new EpisodeJournal(dir);
    await journal.append(episode("pending"));
    const controller = new AbortController();
    const { engine, firstCall } = pendingForever();
    const replay = journal.replay(engine, { signal: controller.signal });
    await firstCall;
    controller.abort(null);
    expect(controller.signal.reason).toBeNull();
    await expect(replay).rejects.toBeNull();
  });

  test("aborting while a remember is pending rejects with the abort reason itself and does not start the next entry", async () => {
    const dir = await directory();
    const journal = new EpisodeJournal(dir);
    await journal.append(episode("pending"));
    await journal.append(episode("never"));
    const controller = new AbortController();
    const { engine, firstCall, records } = pendingForever();
    const replay = journal.replay(engine, { signal: controller.signal });
    await firstCall;
    const reason = new Error("operator abort");
    controller.abort(reason);
    await expect(replay).rejects.toBe(reason);
    expect(records).toEqual(["pending"]);
  });

  test("an abort without an explicit reason rejects with the signal's own AbortError", async () => {
    const dir = await directory();
    const journal = new EpisodeJournal(dir);
    await journal.append(episode("pending"));
    const controller = new AbortController();
    const { engine, firstCall } = pendingForever();
    const replay = journal.replay(engine, { signal: controller.signal });
    await firstCall;
    controller.abort();
    expect(controller.signal.reason).toBeInstanceOf(DOMException);
    await expect(replay).rejects.toBe(controller.signal.reason);
  });

  test("a signal that never fires lets every entry replay, and each abort listener is removed after its remember settles", async () => {
    const dir = await directory();
    const journal = new EpisodeJournal(dir);
    await journal.append(episode("a"));
    await journal.append(episode("b"));
    const { signal } = new AbortController();
    const added = spyOn(signal, "addEventListener");
    const removed = spyOn(signal, "removeEventListener");
    const engine = new RecordingEngine();
    expect(await journal.replay(engine, { signal })).toBe(2);
    expect(engine.inputs.map(i => i.origin.record)).toEqual(["a", "b"]);
    expect(added.mock.calls.map(call => call[0])).toEqual(["abort", "abort"]);
    expect(removed.mock.calls.map(call => call[0])).toEqual(["abort", "abort"]);
    expect(removed.mock.calls[0]?.[1]).toBe(added.mock.calls[0]?.[1]);
    expect(removed.mock.calls[1]?.[1]).toBe(added.mock.calls[1]?.[1]);
    expect(added.mock.calls[0]?.[1]).not.toBe(added.mock.calls[1]?.[1]);
  });
});

describe("inspect", () => {
  test("returns every historical line with its exact bytes, offset, sha256, decoded entry and eligibility, across months in order", async () => {
    const dir = await directory();
    await writeFile(join(dir, "journal-2026-09.jsonl"), legacyLines[0] + legacyLines[1]);
    await writeFile(join(dir, "journal-2026-08.jsonl"), legacyLines[2]);
    await writeFile(join(dir, "journal-2026-08.jsonl.bak"), "ignored\n");
    const rows = await new EpisodeJournal(dir).inspect("post167-pre194");
    expect(rows.map(r => [r.file, r.offset, r.sha256, r.eligibility])).toEqual([
      ["journal-2026-08.jsonl", 0, legacyHashes[2], ["invalid-sub-kind"]],
      ["journal-2026-09.jsonl", 0, legacyHashes[0], ["missing-time"]],
      ["journal-2026-09.jsonl", Buffer.byteLength(legacyLines[0]), legacyHashes[1], ["missing-time"]],
    ]);
    expect(rows.map(r => r.raw)).toEqual([Buffer.from(legacyLines[2]), Buffer.from(legacyLines[0]), Buffer.from(legacyLines[1])]);
    expect(rows.map(r => hash(r.raw))).toEqual([legacyHashes[2], legacyHashes[0], legacyHashes[1]]);
    expect(rows.map(r => r.entry)).toEqual([JSON.parse(legacyLines[2]), JSON.parse(legacyLines[0]), JSON.parse(legacyLines[1])]);
  });

  test("an empty directory inspects to no rows", async () => {
    expect(await new EpisodeJournal(await directory()).inspect("post167-pre194")).toEqual([]);
  });

  test("refuses any other legacy format by name", async () => {
    await expect(new EpisodeJournal(await directory()).inspect("post194")).rejects.toThrow("unsupported-legacy-format: post194");
  });

  test("an unterminated final line is reported with its file and byte offset", async () => {
    const journal = await journalWith(legacyLines[0] + legacyLines[1].slice(0, -1));
    await expect(journal.inspect("post167-pre194")).rejects.toThrow(`incomplete-legacy-line: journal-2026-09.jsonl:${Buffer.byteLength(legacyLines[0])}`);
  });

  test("a line that is not valid UTF-8 fails in the fatal decoder, not in the schema", async () => {
    const journal = await journalWith(Buffer.concat([Buffer.from('{"recordedAt":"2026-09-02T12:34:56.000Z","element":"'), Buffer.from([0xff]), Buffer.from('"}\n')]));
    const inspect = journal.inspect("post167-pre194");
    await expect(inspect).rejects.toBeInstanceOf(TypeError);
    await expect(inspect).rejects.not.toBeInstanceOf(ZodError);
  });

  test("an element with empty content is a decoding error at element.content", async () => {
    const journal = await journalWith('{"recordedAt":"2026-09-02T12:34:56.000Z","element":{"schema":"anamnesis.claim/1","content":"","origin":{"source":"s","session":"c","actor":"a","record":"r"},"mass":0.5,"properties":{}}}\n');
    await expect(journal.inspect("post167-pre194")).rejects.toMatchObject({ issues: [{ code: "too_small", path: ["element", "content"] }] });
  });

  test("an element without mass is a decoding error; inspect never synthesizes historical defaults", async () => {
    const journal = await journalWith('{"recordedAt":"2026-09-02T12:34:56.000Z","element":{"schema":"anamnesis.claim/1","content":"c","origin":{"source":"s","session":"c","actor":"a","record":"r"},"properties":{}}}\n');
    await expect(journal.inspect("post167-pre194")).rejects.toMatchObject({ issues: [{ code: "invalid_type", path: ["element", "mass"] }] });
  });
});
