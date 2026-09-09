import { afterEach, expect, spyOn, test } from "bun:test";
import { createHash } from "node:crypto";
import * as fs from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { DurableSpool, type SpoolRecord } from "./spool.ts";

const roots: string[] = [];
const hash = (value: string | Uint8Array) => createHash("sha256").update(value).digest("hex");
const input = (sequence: number): SpoolRecord => ({ origin: "memory", revision: String(sequence), predecessor: null, body: "x".repeat(20 + sequence % 17), incarnation: "i" });
// Independent canonical frame oracle: literal field order, not production serialization.
function frame(sequence: number) {
  const record = input(sequence);
  const body = Buffer.from(JSON.stringify({ body: record.body, incarnation: "i", origin: "memory", predecessor: null, revision: String(sequence) }));
  const header = Buffer.alloc(4); header.writeUInt32BE(body.length);
  return Buffer.concat([header, body, Buffer.from(hash(body), "hex")]);
}
async function fixture(count = 26_000, version = 2) {
  const root = await fs.mkdtemp(join(tmpdir(), "spool-memory-")); roots.push(root);
  const bytes = Buffer.concat(Array.from({ length: count }, (_, i) => frame(i + 1)));
  const completed = Array.from({ length: Math.floor(count / 2) }, (_, i) => (i + 1) * 2);
  const payload = JSON.stringify(version === 1 ? { version, sequences: completed } : { version, frontier: 0, completed });
  await fs.writeFile(join(root, "spool.journal"), bytes);
  await fs.writeFile(join(root, "spool.durable"), String(bytes.length));
  await fs.writeFile(join(root, "spool.durable.sha256"), hash(Buffer.concat([Buffer.from(`spool.durable.v1\0${bytes.length}\0`), bytes])));
  await fs.writeFile(join(root, "spool.done"), JSON.stringify({ payload, checksum: hash(payload) }));
  return { root, bytes, count, completed };
}
afterEach(async () => {
  for (const root of roots.splice(0)) {
    await fs.rm(root, { recursive: true, force: true });
    await expect(fs.stat(root)).rejects.toHaveProperty("code", "ENOENT");
    console.log(JSON.stringify({ cleanup: root, absent: true }));
  }
});

async function measured<T>(root: string, run: () => Promise<T>) {
  const probe = await fs.open(join(root, "spool.journal"), "r");
  const prototype = Object.getPrototypeOf(probe);
  const read: (this: fs.FileHandle, buffer: Buffer, offset: number, length: number, position: number | null) => Promise<{ bytesRead: number; buffer: Buffer }> = prototype.read;
  const handleReadFile = prototype.readFile;
  const concat = Buffer.concat, alloc = Buffer.alloc, unsafe = Buffer.allocUnsafe, from = Buffer.from;
  const push = Array.prototype.push, add = Set.prototype.add, parse = JSON.parse;
  const resource = { maxRead: 0, actualRead: 0, maxAllocation: 0, wholeFileReads: 0, maxEntries: 0, maxNumbers: 0, maxSet: 0, maxParse: 0 };
  const allocation = (size: number) => { resource.maxAllocation = Math.max(resource.maxAllocation, size); };
  const readSpy = spyOn(prototype, "read").mockImplementation(async function(this: fs.FileHandle, ...args: unknown[]) {
    if (typeof args[2] !== "number") throw new Error("unexpected read overload");
    resource.maxRead = Math.max(resource.maxRead, args[2]);
    const result = await Reflect.apply(read, this, args); resource.actualRead += result.bytesRead;
    return result;
  });
  const fileSpy = spyOn(prototype, "readFile").mockImplementation(function(this: fs.FileHandle, ...args: unknown[]) {
    resource.wholeFileReads++; return Reflect.apply(handleReadFile, this, args);
  });
  const concatSpy = spyOn(Buffer, "concat").mockImplementation((list, length) => {
    allocation(length ?? list.reduce((sum, value) => sum + value.byteLength, 0)); return concat(list, length);
  });
  const allocSpy = spyOn(Buffer, "alloc").mockImplementation(new Proxy(alloc, { apply(target, receiver, args) { allocation(args[0]); return Reflect.apply(target, receiver, args); } }));
  const unsafeSpy = spyOn(Buffer, "allocUnsafe").mockImplementation(size => { allocation(size); return unsafe(size); });
  const fromSpy = spyOn(Buffer, "from").mockImplementation(new Proxy(from, { apply(target, receiver, args) {
    const result = Reflect.apply(target, receiver, args); allocation(result.byteLength); return result;
  } }));
  const pushSpy = spyOn(Array.prototype, "push").mockImplementation(function(this: unknown[], ...items: unknown[]) {
    const result = Reflect.apply(push, this, items);
    if (items.some(item => typeof item === "object" && item !== null && "origin" in item && item.origin === "memory")) resource.maxEntries = Math.max(resource.maxEntries, this.length);
    if (items.some(item => typeof item === "number")) resource.maxNumbers = Math.max(resource.maxNumbers, this.length);
    return result;
  });
  const addSpy = spyOn(Set.prototype, "add").mockImplementation(function(this: Set<unknown>, value: unknown) {
    const result = Reflect.apply(add, this, [value]);
    if (typeof value === "number") resource.maxSet = Math.max(resource.maxSet, this.size);
    return result;
  });
  const parseSpy = spyOn(JSON, "parse").mockImplementation((text, reviver) => {
    resource.maxParse = Math.max(resource.maxParse, text.length); return parse(text, reviver);
  });
  try { return { value: await run(), resource }; }
  finally {
    readSpy.mockRestore(); fileSpy.mockRestore(); concatSpy.mockRestore(); allocSpy.mockRestore(); unsafeSpy.mockRestore(); fromSpy.mockRestore(); pushSpy.mockRestore(); addSpy.mockRestore(); parseSpy.mockRestore();
    await probe.close(); console.log(JSON.stringify({ resource }));
  }
}

for (const operation of ["status", "append", "complete", "page"] as const) {
  test(`${operation} bounds real journal and large sparse completion working memory`, async () => {
    const { root, bytes, count, completed } = await fixture();
    expect((await fs.stat(join(root, "spool.done"))).size).toBeGreaterThan(64 * 1024);
    const spool = new DurableSpool(root, { maxFrameBytes: 1024 });
    const { resource } = await measured(root, async () => {
      if (operation === "status") expect(await spool.status()).toEqual({ pending: count, nextSequence: count + 1, quarantined: false });
      if (operation === "append") expect(await spool.append(input(count + 1))).toBe(count + 1);
      if (operation === "complete") await spool.complete(1);
      if (operation === "page") {
        const first = await spool.page({ limit: 2, maxBytes: frame(1).length + frame(2).length });
        expect(first.entries).toEqual([{ ...input(1), sequence: 1 }, { ...input(2), sequence: 2 }]);
        const second = await spool.page({ cursor: first.nextCursor!, limit: 2 });
        expect(second.entries).toEqual([{ ...input(3), sequence: 3 }, { ...input(4), sequence: 4 }]);
      }
    });
    const expected = operation === "append" ? Buffer.concat([bytes, frame(count + 1)]) : bytes;
    expect(await fs.readFile(join(root, "spool.journal"))).toEqual(expected);
    expect(await fs.readFile(join(root, "spool.durable"), "utf8")).toBe(String(expected.length));
    expect(await fs.readFile(join(root, "spool.durable.sha256"), "utf8")).toBe(hash(Buffer.concat([Buffer.from(`spool.durable.v1\0${expected.length}\0`), expected])));
    if (operation === "complete") {
      const envelope = JSON.parse(await fs.readFile(join(root, "spool.done"), "utf8"));
      expect(envelope.checksum).toBe(hash(envelope.payload));
      expect(JSON.parse(envelope.payload)).toEqual({ version: 2, frontier: 2, completed: completed.slice(1) });
      const payload = JSON.stringify({ completed: completed.slice(1), frontier: 2, version: 2 });
      expect(await fs.readFile(join(root, "spool.done"), "utf8")).toBe(JSON.stringify({ payload, checksum: hash(payload) }));
    }
    expect(resource.actualRead).toBeGreaterThan(bytes.length);
    expect(resource.wholeFileReads).toBe(0);
    expect(resource.maxRead).toBeLessThanOrEqual(64 * 1024);
    expect(resource.maxAllocation).toBeLessThanOrEqual(64 * 1024);
    expect(resource.maxParse).toBeLessThanOrEqual(64 * 1024);
    expect(resource.maxEntries).toBeLessThanOrEqual(operation === "page" ? 2 : 0);
    expect(resource.maxNumbers).toBeLessThanOrEqual(16);
    expect(resource.maxSet).toBe(0);
  }, 120_000);
}

async function writeDone(root: string, payload: string) {
  await fs.writeFile(join(root, "spool.done"), JSON.stringify({ payload, checksum: hash(payload) }));
}
test.each(["spool.journal", "spool.done"])("real short reads and read errors on %s neither lose data nor swallow errors", async filename => {
  const { root, bytes } = await fixture(8);
  const probe = await fs.open(join(root, filename), "r");
  const identity = await probe.stat(), prototype = Object.getPrototypeOf(probe);
  await probe.close();
  const read: (this: fs.FileHandle, buffer: Buffer, offset: number, length: number, position: number | null) => Promise<{ bytesRead: number; buffer: Buffer }> = prototype.read;
  const close = prototype.close;
  let mode: "short" | "error" = "short", reads = 0, observedError: unknown;
  const handles = new Set<fs.FileHandle>();
  const readSpy = spyOn(prototype, "read").mockImplementation(async function(this: fs.FileHandle, ...args: unknown[]) {
    const info = await this.stat();
    if (info.dev !== identity.dev || info.ino !== identity.ino) return Reflect.apply(read, this, args);
    handles.add(this);
    const buffer = args[0], offset = args[1], length = args[2], position = args[3];
    if (!Buffer.isBuffer(buffer) || typeof offset !== "number" || typeof length !== "number" || (position !== null && typeof position !== "number")) throw new Error("unexpected read overload");
    if (mode === "error") await Reflect.apply(close, this, []);
    try { const result = await read.call(this, buffer, offset, Math.min(7, length), position); reads++; return result; }
    catch (error) { observedError = error; throw error; }
  });
  const spool = new DurableSpool(root);
  try {
    expect((await spool.page({ limit: 2 })).entries).toEqual([{ ...input(1), sequence: 1 }, { ...input(2), sequence: 2 }]);
    expect(reads).toBeGreaterThan(10);
    mode = "error";
    const failure = await spool.status().then(() => undefined, error => error);
    expect(failure).toBe(observedError); expect(failure).toHaveProperty("code", "EBADF");
  } finally { readSpy.mockRestore(); }
  expect([...handles].every(handle => handle.fd === -1)).toBe(true);
  expect(await spool.status()).toEqual({ pending: 8, nextSequence: 9, quarantined: false });
  expect(await fs.readFile(join(root, "spool.journal"))).toEqual(bytes);
  await expect(fs.stat(join(root, "spool.quarantine"))).rejects.toHaveProperty("code", "ENOENT");
});

test.each(["temp-sync", "rename", "directory-sync"])("completion %s failure exposes only an atomic old or new envelope", async stage => {
  const { root, bytes } = await fixture(8);
  const path = join(root, "spool.done"), before = await fs.readFile(path);
  const temp = await fs.open(path + ".tmp", "w");
  const tempIdentity = await temp.stat(), prototype = Object.getPrototypeOf(temp);
  await temp.close();
  const rootIdentity = await fs.stat(root);
  const obstacle = join(root, "rename-obstacle"); await fs.mkdir(obstacle); await fs.writeFile(join(obstacle, "keep"), "occupied");
  const sync = prototype.sync, close = prototype.close, rename = fs.rename;
  let observedError: unknown;
  const syncSpy = spyOn(prototype, "sync").mockImplementation(async function(this: fs.FileHandle) {
    const info = await this.stat();
    const selected = stage === "temp-sync" ? tempIdentity : stage === "directory-sync" ? rootIdentity : null;
    if (selected && info.dev === selected.dev && info.ino === selected.ino) await Reflect.apply(close, this, []);
    try { return await Reflect.apply(sync, this, []); }
    catch (error) { observedError = error; throw error; }
  });
  const renameSpy = spyOn(fs, "rename").mockImplementation(async (oldPath, newPath) => {
    try { return await rename(oldPath, stage === "rename" && String(newPath) === path ? obstacle : newPath); }
    catch (error) { observedError = error; throw error; }
  });
  const spool = new DurableSpool(root);
  try {
    const failure = await spool.complete(1).then(() => undefined, error => error);
    expect(failure).toBeDefined(); expect(failure).toBe(observedError);
    if (stage !== "rename") expect(failure).toHaveProperty("code", "EBADF");
  } finally { syncSpy.mockRestore(); renameSpy.mockRestore(); }
  if (stage === "directory-sync") expect(await doneValue(root)).toEqual({ version: 2, frontier: 2, completed: [4, 6, 8] });
  else expect(await fs.readFile(path)).toEqual(before);
  expect(await fs.readFile(join(root, "spool.journal"))).toEqual(bytes);
  await spool.complete(1);
  expect(await doneValue(root)).toEqual({ version: 2, frontier: 2, completed: [4, 6, 8] });
});

test("legacy pending slice semantics retain only caller-selected output", async () => {
  const { root } = await fixture(8);
  const spool = new DurableSpool(root);
  for (const size of [0, NaN, -1, -0.5, -9, Infinity, -Infinity, 1.5, Number.MAX_SAFE_INTEGER]) {
    const expected = Array.from({ length: 8 }, (_, i) => i + 1).slice(0, size);
    expect((await spool.pending(size)).map(entry => entry.sequence)).toEqual(expected);
  }
  await spool.complete(1);
  for (const size of [0, NaN, -1, -4, 1.5, 3]) {
    const expected = Array.from({ length: 8 }, (_, i) => i + 1).slice(2, 2 + size);
    expect((await spool.pending(size)).map(entry => entry.sequence)).toEqual(expected);
  }
});

async function doneValue(root: string) {
  const envelope = JSON.parse(await fs.readFile(join(root, "spool.done"), "utf8"));
  expect(envelope.checksum).toBe(hash(envelope.payload));
  return JSON.parse(envelope.payload);
}

test("larger legacy sparse metadata migrates without arrays and full gap closure is streamed", async () => {
  const { root, bytes, count, completed } = await fixture(52_000, 1);
  const spool = new DurableSpool(root, { maxFrameBytes: 1024 });
  const { resource } = await measured(root, async () => {
    await spool.complete(3); // Insert between durable hints 2 and 4, still behind gap 1.
    await spool.complete(1); // Consume 1,2,3,4, retaining the rest exactly.
    expect(await spool.status()).toEqual({ pending: count - 4, nextSequence: count + 1, quarantined: false });
    expect((await spool.pending(2)).map(entry => entry.sequence)).toEqual([5, 6]);
  });
  expect(await doneValue(root)).toEqual({ version: 2, frontier: 4, completed: completed.slice(2) });
  expect(resource.maxAllocation).toBeLessThanOrEqual(64 * 1024);
  expect(resource.maxRead).toBeLessThanOrEqual(64 * 1024);
  expect(resource.maxParse).toBeLessThanOrEqual(64 * 1024);
  expect(resource.maxEntries).toBe(2); expect(resource.maxSet).toBe(0);
  expect(resource.maxNumbers).toBeLessThanOrEqual(16);
  const payload = JSON.stringify({ version: 1, sequences: Array.from({ length: count - 1 }, (_, i) => i + 2) });
  await writeDone(root, payload);
  const closure = await measured(root, () => new DurableSpool(root).complete(1));
  expect(closure.resource.maxAllocation).toBeLessThanOrEqual(64 * 1024);
  expect(closure.resource.maxEntries).toBe(0); expect(closure.resource.maxSet).toBe(0);
  expect(await doneValue(root)).toEqual({ version: 2, frontier: count, completed: [] });
  expect(await fs.readFile(join(root, "spool.journal"))).toEqual(bytes);
}, 120_000);

test.each([1, 2])("v%s completion decoder preserves reordered, escaped, whitespace and duplicate-array JSON", async version => {
  const { root } = await fixture(8);
  const payload = version === 1
    ? '{ "sequences": [8], "unknown": {"nested":[true,null,"\\uD83D\\uDE80"]}, "sequences": [1e0,3,4,6], "version":1 }'
    : '{ "completed": [8], "frontier":0, "unknown": [false,{"value":"ignored"}], "completed": [3,4,6], "version":2,"frontier":1 }';
  // The envelope's payload and checksum keys themselves may be escaped/reordered.
  const envelope = ` { "checksum":${JSON.stringify(hash(payload))}, "pay\\u006coad":${JSON.stringify(payload)} } `;
  await fs.writeFile(join(root, "spool.done"), envelope);
  const spool = new DurableSpool(root);
  expect((await spool.page()).entries.map(entry => entry.sequence)).toEqual([2, 3, 4, 5, 6, 7, 8]);
  await spool.complete(2);
  expect(await doneValue(root)).toEqual({ version: 2, frontier: 4, completed: [6] });
  await spool.complete(8); await spool.complete(7); await spool.complete(5);
  expect(await doneValue(root)).toEqual({ version: 2, frontier: 8, completed: [] });
});

test("metadata strings and UTF8/surrogate escapes spanning chunks hash exactly without retention", async () => {
  const { root } = await fixture(6);
  const unknown = "x".repeat(4090) + "\ud83d\ude80" + "界".repeat(70_000) + "\ud800".repeat(9000);
  const payload = JSON.stringify({ version: 2, completed: [2, 4, 6], frontier: 0, unknown });
  await writeDone(root, payload);
  const { resource } = await measured(root, async () => {
    const spool = new DurableSpool(root); await spool.complete(1);
    expect((await spool.page()).entries.map(entry => entry.sequence)).toEqual([3, 4, 5, 6]);
  });
  expect(resource.maxAllocation).toBeLessThanOrEqual(64 * 1024);
  expect(resource.maxParse).toBeLessThanOrEqual(64 * 1024);
  expect(await doneValue(root)).toEqual({ version: 2, frontier: 2, completed: [4, 6] });
});

for (const operation of ["status", "append", "complete", "page"] as const) {
  test(`${operation} validates corrupt metadata past the first chunk before truncation`, async () => {
    const { root, bytes, completed } = await fixture();
    completed.push(26_001);
    await writeDone(root, JSON.stringify({ version: 2, frontier: 0, completed }));
    const damaged = Buffer.concat([bytes, Buffer.from([0, 0])]);
    await fs.writeFile(join(root, "spool.journal"), damaged);
    const spool = new DurableSpool(root);
    if (operation === "status") expect((await spool.status()).quarantined).toBe(true);
    if (operation === "append") await expect(spool.append(input(26_001))).rejects.toThrow("quarantined");
    if (operation === "complete") await expect(spool.complete(1)).rejects.toThrow("quarantined");
    if (operation === "page") await expect(spool.page({ limit: 1 })).rejects.toThrow("quarantined");
    expect(await fs.readFile(join(root, "spool.journal"))).toEqual(damaged);
    expect((await new DurableSpool(root).status()).quarantined).toBe(true);
  }, 120_000);
}

test.each(["checksum", "truncated", "trailing", "unsorted"])("large completion %s corruption preserves the entire journal and suffix", async damage => {
  const { root, bytes, completed } = await fixture();
  if (damage === "unsorted") completed.push(2);
  const payload = JSON.stringify({ version: 2, frontier: 0, completed });
  const envelope = JSON.stringify({ payload, checksum: damage === "checksum" ? "0".repeat(64) : hash(payload) });
  await fs.writeFile(join(root, "spool.done"), damage === "truncated" ? envelope.slice(0, -2) : damage === "trailing" ? envelope + "x" : envelope);
  const retained = Buffer.concat([bytes, Buffer.from([0])]);
  await fs.writeFile(join(root, "spool.journal"), retained);
  const { value, resource } = await measured(root, () => new DurableSpool(root).status());
  expect(value.quarantined).toBe(true);
  expect(resource.maxAllocation).toBeLessThanOrEqual(64 * 1024);
  expect(resource.maxEntries).toBe(0); expect(resource.maxSet).toBe(0);
  expect(await fs.readFile(join(root, "spool.journal"))).toEqual(retained);
}, 120_000);

test.each(["spool.durable", "spool.durable.sha256"])("oversized %s rejects at its fixed admission cap", async name => {
  const { root, bytes } = await fixture(6);
  await fs.writeFile(join(root, name), "1".repeat(128 * 1024));
  const { value, resource } = await measured(root, () => new DurableSpool(root).status());
  expect(value.quarantined).toBe(true);
  expect(resource.maxAllocation).toBeLessThanOrEqual(64 * 1024);
  expect(resource.actualRead).toBeLessThan(100);
  expect(await fs.readFile(join(root, "spool.journal"))).toEqual(bytes);
});

for (const filename of ["spool.journal", "spool.done.tmp", "spool.durable.sha256.tmp", "spool.durable.tmp"] as const) {
  for (const mode of ["short", "zero", "error"] as const) {
    test(`${filename} real ${mode} writes preserve publication and release the queue`, async () => {
      const { root, bytes } = await fixture(6);
      const before = await fs.readFile(join(root, "spool.done"));
      const path = join(root, filename), probe = await fs.open(path, "a");
      const identity = await probe.stat(), prototype = Object.getPrototypeOf(probe);
      await probe.close();
      const write: (this: fs.FileHandle, buffer: Uint8Array, offset: number, length: number, position: null) => Promise<{ bytesWritten: number; buffer: Uint8Array }> = prototype.write;
      const sync = prototype.sync, close = prototype.close;
      const handles = new Set<fs.FileHandle>(), counts: number[] = [], events: string[] = [];
      let observedError: unknown;
      const writeSpy = spyOn(prototype, "write").mockImplementation(async function(this: fs.FileHandle, ...args: unknown[]) {
        const info = await this.stat();
        if (info.dev !== identity.dev || info.ino !== identity.ino) return Reflect.apply(write, this, args);
        handles.add(this);
        const buffer = args[0], offset = args[1], length = args[2];
        if (!(buffer instanceof Uint8Array) || typeof offset !== "number" || typeof length !== "number") throw new Error("unexpected write overload");
        if (mode === "error" && counts.length === 1) await Reflect.apply(close, this, []);
        const requested = counts.length === 0 ? mode === "zero" ? 0 : filename === "spool.durable.tmp" ? 1 : 3 : length;
        try {
          const result = await write.call(this, buffer, offset, requested, null);
          counts.push(result.bytesWritten); events.push(`write:${result.bytesWritten}`); return result;
        } catch (error) { observedError = error; throw error; }
      });
      const syncSpy = spyOn(prototype, "sync").mockImplementation(async function(this: fs.FileHandle) {
        const result = await Reflect.apply(sync, this, []);
        if (handles.has(this)) events.push("sync"); return result;
      });
      const spool = new DurableSpool(root);
      let error: unknown;
      try {
        try { if (filename === "spool.done.tmp") await spool.complete(1); else await spool.append(input(7)); }
        catch (failure) { error = failure; }
      } finally { writeSpy.mockRestore(); syncSpy.mockRestore(); }
      expect([...handles].every(handle => handle.fd === -1)).toBe(true);
      console.log(JSON.stringify({ filename, mode, counts, events, code: error instanceof Error && "code" in error ? error.code : null, closed: true }));
      if (mode === "short") {
        expect(error).toBeUndefined(); expect(counts[0]).toBe(filename === "spool.durable.tmp" ? 1 : 3);
        expect(counts.length).toBeGreaterThan(1); expect(events.at(-1)).toBe("sync");
        if (filename === "spool.done.tmp") expect(await doneValue(root)).toEqual({ version: 2, frontier: 2, completed: [4, 6] });
        else {
          const expected = Buffer.concat([bytes, frame(7)]);
          expect(await fs.readFile(join(root, "spool.journal"))).toEqual(expected);
          expect(await fs.readFile(join(root, "spool.durable"), "utf8")).toBe(String(expected.length));
          expect(await fs.readFile(join(root, "spool.durable.sha256"), "utf8")).toBe(hash(Buffer.concat([Buffer.from(`spool.durable.v1\0${expected.length}\0`), expected])));
        }
      } else {
        expect(error).toBeDefined(); expect(events).not.toContain("sync");
        if (mode === "error") { expect(error).toBe(observedError); expect(error).toHaveProperty("code", "EBADF"); }
        else expect(error).toHaveProperty("message", "spool write made no progress");
        expect(await fs.readFile(join(root, "spool.done"))).toEqual(before);
        expect(await fs.readFile(join(root, "spool.durable"), "utf8")).toBe(String(bytes.length));
        if (filename === "spool.durable.tmp") {
          // Preserve the existing two-file publication contract: new proof + old
          // decimal boundary fails closed, and never truncates acknowledged data.
          expect((await spool.status()).quarantined).toBe(true);
          expect(await fs.readFile(join(root, "spool.journal"))).toEqual(Buffer.concat([bytes, frame(7)]));
          await expect(spool.append(input(8))).rejects.toThrow("quarantined");
        } else {
          expect(await spool.status()).toEqual({ pending: 6, nextSequence: 7, quarantined: false });
          expect(await fs.readFile(join(root, "spool.journal"))).toEqual(bytes);
          if (filename === "spool.done.tmp") { await spool.complete(1); expect(await doneValue(root)).toEqual({ version: 2, frontier: 2, completed: [4, 6] }); }
          else expect(await spool.append(input(7))).toBe(7);
        }
      }
    });
  }
}
