import { afterEach, expect, spyOn, test } from "bun:test";
import { createHash } from "node:crypto";
import * as fs from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { DurableSpool, type SpoolRecord } from "./spool.ts";

const roots: string[] = [];
const record: SpoolRecord = { origin: "paging", revision: "1", predecessor: null, body: { value: "x" }, incarnation: "i" };
async function fixture(count = 5, body: unknown = record.body) {
  const root = await fs.mkdtemp(join(tmpdir(), "spool-page-")); roots.push(root);
  const spool = new DurableSpool(root, { maxFrameBytes: 16 * 1024 });
  for (let i = 1; i <= count; i++) await spool.append({ ...record, body, revision: String(i) });
  return { root, spool };
}
afterEach(async () => {
  for (const root of roots.splice(0)) {
    await fs.rm(root, { recursive: true, force: true });
    await expect(fs.stat(root)).rejects.toHaveProperty("code", "ENOENT");
    console.log(JSON.stringify({ cleanup: root, absent: true }));
  }
});

test("bounded journal allocations and reads on first page and continuation", async () => {
  const { root } = await fixture(24, { value: "x".repeat(8000) });
  const journal = join(root, "spool.journal");
  const handle = await fs.open(journal, "r");
  const prototype = Object.getPrototypeOf(handle);
  const read = prototype.read;
  const concat = Buffer.concat;
  const allocUnsafe = Buffer.allocUnsafe;
  const push = Array.prototype.push;
  const readFile = fs.readFile;
  let maxRead = 0, totalRead = 0, maxAllocation = 0, wholeFileReads = 0, maxEntryCollection = 0;
  const readSpy = spyOn(prototype, "read").mockImplementation(function (this: typeof handle, ...args: unknown[]) {
    const length = args[2];
    if (typeof length !== "number") throw new Error("unexpected read overload in resource instrumentation");
    maxRead = Math.max(maxRead, length); totalRead += length;
    return Reflect.apply(read, this, args);
  });
  const concatSpy = spyOn(Buffer, "concat").mockImplementation((list, length) => {
    maxAllocation = Math.max(maxAllocation, length ?? list.reduce((sum, value) => sum + value.byteLength, 0));
    return concat(list, length);
  });
  const allocSpy = spyOn(Buffer, "allocUnsafe").mockImplementation(size => {
    maxAllocation = Math.max(maxAllocation, size); return allocUnsafe(size);
  });
  const pushSpy = spyOn(Array.prototype, "push").mockImplementation(function (this: unknown[], ...items: unknown[]) {
    const result = Reflect.apply(push, this, items);
    if (items.some(item => typeof item === "object" && item !== null && "sequence" in item && "origin" in item && item.origin === "paging")) {
      maxEntryCollection = Math.max(maxEntryCollection, this.length);
    }
    return result;
  });
  const fileSpy = spyOn(fs, "readFile").mockImplementation(new Proxy(readFile, {
    apply(target, receiver, args) {
      if (String(args[0]) === journal) wholeFileReads++;
      return Reflect.apply(target, receiver, args);
    },
  }));
  let sequences: number[] = [];
  try {
    const spool = new DurableSpool(root, { maxFrameBytes: 16 * 1024 });
    const first = await spool.page({ limit: 2, maxBytes: 20 * 1024 });
    const second = await spool.page({ cursor: first.nextCursor!, limit: 2, maxBytes: 20 * 1024 });
    sequences = [...first.entries, ...second.entries].map(entry => entry.sequence);
  } finally {
    readSpy.mockRestore(); concatSpy.mockRestore(); allocSpy.mockRestore(); pushSpy.mockRestore(); fileSpy.mockRestore(); await handle.close();
  }
  console.log(JSON.stringify({ resource: { maxRead, totalRead, maxAllocation, wholeFileReads, maxEntryCollection } }));
  expect(sequences).toEqual([1, 2, 3, 4]);
  expect(totalRead).toBeGreaterThan(0);
  expect(wholeFileReads).toBe(0);
  expect(maxRead).toBeLessThanOrEqual(64 * 1024);
  expect(maxAllocation).toBeLessThanOrEqual(64 * 1024);
  expect(maxEntryCollection).toBe(2);
});

test("pages a finite snapshot exactly once across append, full completion, and reopen", async () => {
  const { root, spool } = await fixture();
  await spool.complete(4);
  const journal = await fs.readFile(join(root, "spool.journal"));
  const first = await spool.page({ limit: 2 }); expect(first.entries.map(e => e.sequence)).toEqual([1, 2]);
  for (const sequence of [1, 2, 3, 5]) await spool.complete(sequence);
  expect(await fs.readFile(join(root, "spool.journal"))).toEqual(journal);
  await spool.append({ ...record, revision: "6" });
  const reopened = new DurableSpool(root);
  const second = await reopened.page({ cursor: first.nextCursor!, limit: 2 });
  const third = await reopened.page({ cursor: second.nextCursor!, limit: 2 });
  expect([...first.entries, ...second.entries, ...third.entries].map(e => e.sequence)).toEqual([1, 2, 3, 4, 5]);
  expect(third.nextCursor).toBeNull();
  expect((await reopened.page()).entries.map(e => e.sequence)).toEqual([6]);
});

test("encoded frame budget is exact and never silently skips an oversized next frame", async () => {
  const { root, spool } = await fixture(3, { text: "界".repeat(20) });
  const bytes = await fs.readFile(join(root, "spool.journal"));
  const frameSize = bytes.readUInt32BE(0) + 36;
  const first = await spool.page({ limit: 3, maxBytes: frameSize });
  expect(first.entries.map(e => e.sequence)).toEqual([1]);
  await expect(spool.page({ cursor: first.nextCursor!, maxBytes: frameSize - 1 })).rejects.toThrow("too small");
  expect((await spool.page({ cursor: first.nextCursor!, maxBytes: frameSize })).entries.map(e => e.sequence)).toEqual([2]);
});

test("a non-fitting middle frame cannot be skipped for a smaller later frame", async () => {
  const { root, spool } = await fixture(1);
  await spool.append({ ...record, revision: "2", body: { value: "x".repeat(1000) } });
  await spool.append({ ...record, revision: "3" });
  const bytes = await fs.readFile(join(root, "spool.journal"));
  const smallSize = bytes.readUInt32BE(0) + 36;
  const first = await spool.page({ limit: 3, maxBytes: smallSize * 2 });
  expect(first.entries.map(entry => entry.sequence)).toEqual([1]);
  await expect(spool.page({ cursor: first.nextCursor!, maxBytes: smallSize * 2 })).rejects.toThrow("too small");
  expect((await spool.page({ cursor: first.nextCursor!, maxBytes: 4096 })).entries.map(entry => entry.sequence)).toEqual([2, 3]);
});

test("empty and fully completed spools have explicit EOF", async () => {
  const { spool } = await fixture(0);
  expect(await spool.page()).toEqual({ entries: [], nextCursor: null });
  await spool.append(record); await spool.complete(1);
  expect(await spool.page()).toEqual({ entries: [], nextCursor: null });
});

test.each(["spool.durable", "spool.durable.sha256", "spool.journal"])("missing %s quarantines rather than returning EOF", async filename => {
  const { root, spool } = await fixture(1);
  await fs.rm(join(root, filename));
  await expect(spool.page()).rejects.toThrow("quarantined");
  expect((await spool.status()).quarantined).toBe(true);
});

test("rejects invalid limits, budgets, and cursors", async () => {
  const { spool } = await fixture(1);
  for (const limit of [0, -1, 1.5, NaN, Infinity, 10_001]) await expect(spool.page({ limit })).rejects.toThrow("limit");
  for (const maxBytes of [0, -1, 1.5, NaN, Infinity, 16 * 1024 * 1024 + 1]) await expect(spool.page({ maxBytes })).rejects.toThrow("budget");
  for (const cursor of ["bad", "x".repeat(4097)]) await expect(spool.page({ cursor })).rejects.toThrow("cursor");
});

test("cursor rejects another root and validly replaced snapshot bytes", async () => {
  const { root, spool } = await fixture(3);
  const other = await fixture(3);
  const first = await spool.page({ limit: 1 });
  await expect(other.spool.page({ cursor: first.nextCursor! })).rejects.toThrow("stale");
  const path = join(root, "spool.journal");
  const bytes = await fs.readFile(path);
  const length = bytes.readUInt32BE(0);
  const body = bytes.subarray(4, 4 + length);
  const replacement = Buffer.from(body.toString().replace('"revision":"1"', '"revision":"z"'));
  replacement.copy(bytes, 4);
  createHash("sha256").update(replacement).digest().copy(bytes, 4 + length);
  await fs.writeFile(path, bytes);
  await fs.writeFile(join(root, "spool.durable.sha256"), createHash("sha256").update(`spool.durable.v1\0${bytes.length}\0`).update(bytes).digest("hex"));
  await expect(spool.page({ cursor: first.nextCursor! })).rejects.toThrow("stale");
  expect((await spool.page({ limit: 1 })).entries[0]?.revision).toBe("z");
});

test("quarantine beyond the requested page is not EOF and preserves a torn suffix", async () => {
  const { root, spool } = await fixture();
  const first = await spool.page({ limit: 1 });
  const path = join(root, "spool.journal");
  const bytes = await fs.readFile(path); bytes[bytes.length - 1] = bytes[bytes.length - 1]! ^ 1;
  const damaged = Buffer.concat([bytes, Buffer.from([0, 0])]);
  await fs.writeFile(path, damaged);
  await expect(spool.page({ cursor: first.nextCursor! })).rejects.toThrow("quarantined");
  await expect(new DurableSpool(root).page()).rejects.toThrow("quarantined");
  expect(await fs.readFile(path)).toEqual(damaged);
});

test.each([
  { version: 2, frontier: 0, completed: [1] },
  { version: 2, frontier: 5, completed: [] },
  { version: 2, frontier: 0, completed: [2, 2] },
  { version: 2, frontier: 0, completed: [5] },
  { version: 1, sequences: [2, 1] },
])("invalid checksummed completion metadata %j fails closed on page path", async value => {
  const { root, spool } = await fixture(4);
  const payload = JSON.stringify(value);
  await fs.writeFile(join(root, "spool.done"), JSON.stringify({ payload, checksum: createHash("sha256").update(payload).digest("hex") }));
  await expect(spool.page()).rejects.toThrow("quarantined");
});

test("legacy completion metadata retains suffixes and validates before recovery", async () => {
  const { root, spool } = await fixture(4);
  const payload = JSON.stringify({ version: 1, sequences: [1, 3, 4] });
  await fs.writeFile(join(root, "spool.done"), JSON.stringify({ payload, checksum: createHash("sha256").update(payload).digest("hex") }));
  expect((await spool.page()).entries.map(e => e.sequence)).toEqual([2, 3, 4]);
  const path = join(root, "spool.journal");
  const bytes = await fs.readFile(path);
  await fs.writeFile(path, Buffer.concat([bytes, Buffer.from([0])]));
  await spool.page(); expect(await fs.readFile(path)).toEqual(bytes);
  await fs.writeFile(join(root, "spool.done"), "invalid");
  const damaged = Buffer.concat([bytes, Buffer.from([0])]); await fs.writeFile(path, damaged);
  await expect(spool.page()).rejects.toThrow("quarantined");
  expect(await fs.readFile(path)).toEqual(damaged);
});
