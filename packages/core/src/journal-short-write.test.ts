import { expect, spyOn, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, open, readFile, rm, stat, type FileHandle } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import neo4j from "neo4j-driver";
import { Engine, type RememberInput } from "./engine.ts";
import { EpisodeJournal, journaledRemember } from "./journal.ts";

const clock = () => new Date("2026-09-02T12:34:56.000Z");
const input: RememberInput = {
  time: { value: "2026-09-02T10:00:00Z", precision: "second" },
  content: "Journal \u20ac \ud83d\ude80",
  origin: { source: "journal-short-write", session: "fixed", actor: "test", record: "one" },
  source_revision: "revision-1",
  payload_media_type: "application/octet-stream",
  payload: Uint8Array.from([0, 127, 255]),
};
// Independent literal: do not derive this oracle from production serialization.
const expected = Buffer.from('{"recordedAt":"2026-09-02T12:34:56.000Z","element":{"schema":"anamnesis.original-message/1","time":{"value":"2026-09-02T10:00:00Z","precision":"second"},"content":"Journal \u20ac \ud83d\ude80","origin":{"source":"journal-short-write","session":"fixed","actor":"test","record":"one"},"mass":0.5,"properties":{},"payload_media_type":"application/octet-stream","source_revision":"revision-1","payload":[0,127,255]}}\n');
const prefixLength = expected.indexOf(Buffer.from("\u20ac")) + 1;
const sha256 = (bytes: Uint8Array | string) => createHash("sha256").update(bytes).digest("hex");

async function fixture(run: (directory: string, path: string) => Promise<void>) {
  const directory = await mkdtemp(join(tmpdir(), "journal-short-write-"));
  try {
    await run(directory, join(directory, "journal-2026-09.jsonl"));
  } finally {
    await rm(directory, { recursive: true });
    await expect(stat(directory)).rejects.toHaveProperty("code", "ENOENT");
    console.log(JSON.stringify({ cleanup: directory, absent: true }));
  }
}

async function writes(path: string, mode: "short" | "zero" | "error", run: (observed: {
  counts: number[]; events: string[]; error?: unknown;
}) => Promise<void>) {
  const probe = await open(path, "a");
  const identity = await probe.stat();
  const prototype = Object.getPrototypeOf(probe);
  await probe.close();
  const originalWrite: (this: FileHandle, buffer: Uint8Array, offset: number, length: number, position: null) => Promise<{ bytesWritten: number; buffer: Uint8Array }> = prototype.write;
  const originalSync = prototype.sync;
  const originalClose = prototype.close;
  const handles = new Set<FileHandle>();
  let shortWrites = 0;
  const observed: { counts: number[]; events: string[]; error?: unknown } = { counts: [], events: [] };
  const write = spyOn(prototype, "write").mockImplementation(async function(this: FileHandle, ...args: unknown[]) {
    const info = await this.stat();
    if (info.dev !== identity.dev || info.ino !== identity.ino) return Reflect.apply(originalWrite, this, args);
    handles.add(this);
    const data = args[0];
    if (typeof data !== "string" && !(data instanceof Uint8Array)) throw new Error("Unexpected journal write overload");
    const bytes = typeof data === "string" ? Buffer.from(data) : Buffer.from(data.buffer, data.byteOffset, data.byteLength);
    const offset = typeof args[1] === "number" ? args[1] : 0;
    const length = typeof args[2] === "number" ? args[2] : bytes.length - offset;
    const remaining = bytes.subarray(offset, offset + length);
    const count = mode === "zero" && observed.counts.length === 0 ? 0
      : mode === "short" && observed.counts.length === 0 ? prefixLength
      : mode === "short" && observed.counts.length === 1 ? 2 : remaining.length;
    if (mode === "error") await Reflect.apply(originalClose, this, []);
    try {
      const result = await originalWrite.call(this, remaining.subarray(0, count), 0, count, null);
      observed.counts.push(result.bytesWritten);
      if (result.bytesWritten > 0 && result.bytesWritten < remaining.length) shortWrites++;
      observed.events.push(`write:${result.bytesWritten}`);
      return result;
    } catch (error) {
      observed.error = error;
      throw error;
    }
  });
  const sync = spyOn(prototype, "sync").mockImplementation(async function(this: FileHandle) {
    const result = await Reflect.apply(originalSync, this, []);
    if (handles.has(this)) observed.events.push("sync");
    return result;
  });
  const close = spyOn(prototype, "close").mockImplementation(async function(this: FileHandle) {
    const result = await Reflect.apply(originalClose, this, []);
    if (handles.has(this)) observed.events.push("close");
    return result;
  });
  try {
    await run(observed);
  } finally {
    write.mockRestore(); sync.mockRestore(); close.mockRestore();
    const closed = [...handles].every(handle => handle.fd === -1);
    console.log(JSON.stringify({ mode, ...observed, expectedBytes: expected.length,
      expectedSha256: sha256(expected), observedSha256: sha256(await readFile(path)),
      shortWrites,
      handlesClosed: closed, spiesRestored: prototype.write === originalWrite && prototype.sync === originalSync && prototype.close === originalClose }));
    expect(closed).toBe(true);
    expect(prototype.write).toBe(originalWrite);
    expect(prototype.sync).toBe(originalSync);
    expect(prototype.close).toBe(originalClose);
  }
}

test("physically short UTF8 writes complete the exact LF-terminated journal before success", async () => {
  await fixture(async (directory, path) => {
    await writes(path, "short", async observed => {
      await new EpisodeJournal(directory, clock).append(input);
      expect(observed.counts[0]).toBe(prefixLength);
      expect(prefixLength).toBeLessThan(expected.length);
      expect(await readFile(path)).toEqual(expected);
      expect(sha256(await readFile(path))).toBe(sha256(expected));
      expect(observed.counts).toEqual([prefixLength, 2, expected.length - prefixLength - 2]);
      expect(observed.events).toEqual([`write:${prefixLength}`, "write:2", `write:${expected.length - prefixLength - 2}`, "sync", "close"]);
    });
    const replayed: RememberInput[] = [];
    const reopened = new EpisodeJournal(directory, clock);
    expect(await reopened.replay({ remember: async value => { replayed.push(value); return { id: "recorded", created: true }; } })).toBe(1);
    expect(replayed).toEqual([{ ...input, schema: "anamnesis.original-message/1", mass: 0.5, properties: {} }]);
    await reopened.append(input);
    expect(await readFile(path)).toEqual(Buffer.concat([expected, expected]));
  });
});

test("ordinary append and reopen preserve exact bytes and replay order", async () => {
  await fixture(async (directory, path) => {
    await new EpisodeJournal(directory, clock).append(input);
    expect(await readFile(path)).toEqual(expected);
    await new EpisodeJournal(directory, clock).append(input);
    expect(await readFile(path)).toEqual(Buffer.concat([expected, expected]));
    const replayed: RememberInput[] = [];
    expect(await new EpisodeJournal(directory).replay({ remember: async value => { replayed.push(value); return { id: "recorded", created: true }; } })).toBe(2);
    expect(replayed.map(value => value.payload)).toEqual([input.payload, input.payload]);
    expect(await readFile(path)).toEqual(Buffer.concat([expected, expected]));
  });
});

test("real zero-byte progress rejects without syncing or deriving graph state", async () => {
  await fixture(async (directory, path) => {
    await writes(path, "zero", async observed => {
      let remembers = 0;
      await expect(journaledRemember(new EpisodeJournal(directory, clock), {
        remember: async () => { remembers++; return { id: "must-not-commit", created: true }; },
      }, input)).rejects.toThrow("Journal write made no progress");
      expect(remembers).toBe(0);
      expect(observed.counts).toEqual([0]);
      expect(observed.events).toEqual(["write:0", "close"]);
      expect(await readFile(path)).toEqual(Buffer.alloc(0));
    });
  });
});

test("real write errors retain their identity and close the journal handle", async () => {
  await fixture(async (directory, path) => {
    await writes(path, "error", async observed => {
      const outcome = await new EpisodeJournal(directory, clock).append(input).then(
        () => ({ error: undefined }), error => ({ error }),
      );
      expect(outcome.error).toBeDefined();
      expect(outcome.error).toBe(observed.error);
      expect(outcome.error).toHaveProperty("code", "EBADF");
      expect(observed.events).toEqual(["close"]);
      expect(await readFile(path)).toEqual(Buffer.alloc(0));
    });
  });
});

const dbExplicitlyRequested = process.env["JOURNAL_SHORT_WRITE_DB"] === "1";
const dbCredentialsPresent = Boolean(
  process.env["ANAMNESIS_TEST_NEO4J_URI"] && process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"],
);

if (dbExplicitlyRequested || dbCredentialsPresent) {
  test("real DB replay of short-written journal preserves original revision ID and payload", async () => {
    const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
    const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
    if (!uri || !password) throw new Error("Owned test DB credentials required");
    const user = process.env["ANAMNESIS_TEST_NEO4J_USER"] ?? "neo4j";
    await fixture(async (directory, path) => {
      const engine = new Engine({ uri, user, password, objectsRoot: join(directory, "objects") });
      const driver = neo4j.driver(uri, neo4j.auth.basic(user, password));
      try {
        await engine.init();
        await writes(path, "short", async observed => {
          const first = await journaledRemember(new EpisodeJournal(directory, clock), engine, input);
          expect(first.created).toBe(true);
          expect(observed.counts).toEqual([prefixLength, 2, expected.length - prefixLength - 2]);
          expect(await readFile(path)).toEqual(expected);
          const reopened = new EpisodeJournal(directory);
          expect(await reopened.replay(engine)).toBe(1);
          expect(await reopened.replay(engine)).toBe(1);
          expect(await engine.remember(input)).toEqual({ id: first.id, created: false });
          const originKey = sha256(JSON.stringify([input.origin.source, input.origin.session, input.origin.actor, input.origin.record]));
          const revisionKey = sha256(JSON.stringify([originKey, input.source_revision]));
          const payloadHash = sha256(Uint8Array.from([0, 127, 255]));
          const result = await driver.executeQuery(
            `MATCH (e:Element:Episode {revision_key: $revisionKey})
             OPTIONAL MATCH (e)-[:HAS_PAYLOAD]->(p:Payload)
             RETURN collect(e.id) AS ids, collect(p.hash) AS payloadHashes`, { revisionKey },
          );
          expect(result.records).toHaveLength(1);
          expect(result.records[0]!.get("ids")).toEqual([first.id]);
          expect(result.records[0]!.get("payloadHashes")).toEqual([payloadHash]);
          expect(await engine.store.getPayload(payloadHash)).toEqual(Uint8Array.from([0, 127, 255]));
          expect(await readFile(path)).toEqual(expected);
          console.log(JSON.stringify({ db: "real", id: first.id, revisionKey, payloadHash, replayCounts: [1, 1], journalSha256: sha256(expected) }));
        });
      } finally {
        try { await engine.close(); } finally { await driver.close(); }
        console.log(JSON.stringify({ dbDriversClosed: true }));
      }
    });
  });
} else {
  console.log("J1 DB proof not requested: filesystem-only run");
}
