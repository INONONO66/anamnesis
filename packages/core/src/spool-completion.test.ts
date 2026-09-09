import { afterEach, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { DurableSpool, type SpoolRecord } from "./spool.ts";

const roots: string[] = [];
const record: SpoolRecord = { origin: "completion", revision: "one", predecessor: null, body: { text: "accepted" }, incarnation: "installation" };
async function fixture(count = 2) {
  const root = await mkdtemp(join(tmpdir(), "spool-completion-"));
  roots.push(root);
  const spool = new DurableSpool(root);
  for (let i = 1; i <= count; i++) expect(await spool.append({ ...record, revision: String(i) })).toBe(i);
  return { root, spool };
}
afterEach(async () => {
  for (const root of roots.splice(0)) {
    await rm(root, { recursive: true, force: true });
    await expect(stat(root)).rejects.toHaveProperty("code", "ENOENT");
    console.log(JSON.stringify({ cleanup: root, absent: true }));
  }
});
async function snapshot(spool: DurableSpool) {
  return { sequences: (await spool.pending()).map(entry => entry.sequence), status: await spool.status() };
}
async function cursor(root: string) {
  const envelope = JSON.parse(await readFile(join(root, "spool.done"), "utf8"));
  expect(envelope.checksum).toBe(createHash("sha256").update(envelope.payload).digest("hex"));
  return JSON.parse(envelope.payload);
}
async function writeCursor(root: string, value: unknown) {
  const payload = JSON.stringify(value);
  await writeFile(join(root, "spool.done"), JSON.stringify({ payload, checksum: createHash("sha256").update(payload).digest("hex") }));
}

test("reopened replay retains completed suffix until the contiguous gap closes", async () => {
  const { root, spool } = await fixture();
  const journal = await readFile(join(root, "spool.journal"));
  await spool.complete(2);
  const reopened = new DurableSpool(root);
  const behindGap = await snapshot(reopened);
  await reopened.complete(1);
  const closedGap = await snapshot(new DurableSpool(root));
  console.log(JSON.stringify({ behindGap, closedGap }));
  expect(await readFile(join(root, "spool.journal"))).toEqual(journal);
  expect({ behindGap, closedGap }).toEqual({
    behindGap: { sequences: [1, 2], status: { pending: 2, nextSequence: 3, quarantined: false } },
    closedGap: { sequences: [], status: { pending: 0, nextSequence: 3, quarantined: false } },
  });
});

test("done persists only a contiguous frontier separately from suffix hints", async () => {
  const { root, spool } = await fixture(5);
  const journal = await readFile(join(root, "spool.journal"));
  const admission = await readFile(join(root, "spool.durable"));
  const proof = await readFile(join(root, "spool.durable.sha256"));
  await spool.complete(4);
  await new DurableSpool(root).complete(2);
  expect(await cursor(root)).toEqual({ version: 2, frontier: 0, completed: [2, 4] });
  expect((await new DurableSpool(root).pending()).map(entry => entry.sequence)).toEqual([1, 2, 3, 4, 5]);
  await new DurableSpool(root).complete(1);
  expect(await cursor(root)).toEqual({ version: 2, frontier: 2, completed: [4] });
  expect((await new DurableSpool(root).pending()).map(entry => entry.sequence)).toEqual([3, 4, 5]);
  await new DurableSpool(root).complete(5);
  await new DurableSpool(root).complete(3);
  expect(await cursor(root)).toEqual({ version: 2, frontier: 5, completed: [] });
  expect(await new DurableSpool(root).status()).toEqual({ pending: 0, nextSequence: 6, quarantined: false });
  expect(await readFile(join(root, "spool.journal"))).toEqual(journal);
  expect(await readFile(join(root, "spool.durable"))).toEqual(admission);
  expect(await readFile(join(root, "spool.durable.sha256"))).toEqual(proof);
  expect(await new DurableSpool(root).append({ ...record, revision: "six" })).toBe(6);
  expect((await new DurableSpool(root).pending()).map(entry => entry.sequence)).toEqual([6]);
});

test("replayed suffix completions are idempotent without hiding the blocked head", async () => {
  const { root, spool } = await fixture(3);
  await spool.complete(3);
  const before = await readFile(join(root, "spool.done"));
  await new DurableSpool(root).complete(3);
  expect(await readFile(join(root, "spool.done"))).toEqual(before);
  // Runtime removes successes from its per-drain snapshot, not from the replay view.
  const remaining = new Map((await spool.pending()).map(entry => [entry.revision, entry]));
  for (const [key, entry] of remaining) {
    if (entry.sequence === 1) continue;
    await spool.complete(entry.sequence);
    remaining.delete(key);
  }
  expect([...remaining.values()].map(entry => entry.sequence)).toEqual([1]);
  expect(await new DurableSpool(root).status()).toEqual({ pending: 3, nextSequence: 4, quarantined: false });
  expect((await new DurableSpool(root).pending()).find(entry => entry.sequence === 3)?.revision).toBe("3");
  await spool.complete(1);
  const done = await readFile(join(root, "spool.done"));
  for (const sequence of [1, 2, 3]) await new DurableSpool(root).complete(sequence);
  expect(await readFile(join(root, "spool.done"))).toEqual(done);
});

test("legacy completed sets recover a contiguous prefix and retain suffix hints", async () => {
  const { root } = await fixture(4);
  await writeCursor(root, { version: 1, sequences: [1, 3, 4] });
  const legacy = await readFile(join(root, "spool.done"));
  const reopened = new DurableSpool(root);
  expect(await snapshot(reopened)).toEqual({ sequences: [2, 3, 4], status: { pending: 3, nextSequence: 5, quarantined: false } });
  await reopened.complete(3);
  expect(await readFile(join(root, "spool.done"))).toEqual(legacy);
  await reopened.complete(2);
  expect(await cursor(root)).toEqual({ version: 2, frontier: 4, completed: [] });
  expect(await new DurableSpool(root).pending()).toEqual([]);
});

test.each([0, -1, 1.5, NaN, Infinity, 3, Number.MAX_SAFE_INTEGER])("invalid completion %s cannot change metadata or admission", async sequence => {
  const { root, spool } = await fixture();
  await spool.complete(2);
  const before = await readFile(join(root, "spool.done"));
  await expect(spool.complete(sequence)).rejects.toThrow("not pending");
  expect(await readFile(join(root, "spool.done"))).toEqual(before);
  expect((await new DurableSpool(root).status()).nextSequence).toBe(3);
});

test.each([
  { version: 2, frontier: -1, completed: [] },
  { version: 2, frontier: 3, completed: [] },
  { version: 2, frontier: 0.5, completed: [] },
  { version: 2, frontier: 9007199254740992, completed: [] },
  { version: 2, completed: [] },
  { version: 2, frontier: 0, completed: [1] },
  { version: 2, frontier: 1, completed: [1] },
  { version: 2, frontier: 0, completed: [2, 2] },
  { version: 2, frontier: 0, completed: [3] },
  { version: 2, frontier: 0, completed: "2" },
  { version: 1, sequences: [2, 1] },
])("invalid completion state %j quarantines before suffix recovery", async value => {
  const { root, spool } = await fixture();
  await writeCursor(root, value);
  const path = join(root, "spool.journal");
  const bytes = Buffer.concat([await readFile(path), Buffer.from([0, 0])]);
  await writeFile(path, bytes);
  expect((await spool.status()).quarantined).toBe(true);
  expect(await readFile(path)).toEqual(bytes);
  await expect(spool.complete(1)).rejects.toThrow("quarantined");
  await expect(spool.append(record)).rejects.toThrow("quarantined");
});

test("missing completion metadata replays rather than inventing progress", async () => {
  const { root, spool } = await fixture();
  await spool.complete(2);
  await spool.complete(1);
  await rm(join(root, "spool.done"));
  expect(await snapshot(new DurableSpool(root))).toEqual({ sequences: [1, 2], status: { pending: 2, nextSequence: 3, quarantined: false } });
});

test("failed completion publication preserves the old frontier and releases the queue", async () => {
  const { root, spool } = await fixture();
  await spool.complete(2);
  const before = await readFile(join(root, "spool.done"));
  // A real filesystem open failure at the publication boundary, not a timing race.
  await mkdir(join(root, "spool.done.tmp"));
  await expect(spool.complete(1)).rejects.toHaveProperty("code", "EISDIR");
  expect(await readFile(join(root, "spool.done"))).toEqual(before);
  expect(await snapshot(new DurableSpool(root))).toEqual({ sequences: [1, 2], status: { pending: 2, nextSequence: 3, quarantined: false } });
  await rm(join(root, "spool.done.tmp"), { recursive: true });
  await spool.complete(1);
  expect(await new DurableSpool(root).pending()).toEqual([]);
});
