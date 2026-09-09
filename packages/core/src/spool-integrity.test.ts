import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { DurableSpool, type SpoolRecord } from "./spool.ts";

const roots: string[] = [];
const input: SpoolRecord = {
  origin: "session/user/record",
  revision: "revision-1",
  predecessor: null,
  incarnation: "dataset-1",
  body: { text: "accepted" },
};

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), "spool-integrity-"));
  roots.push(root);
  return { root, spool: new DurableSpool(root) };
}

afterEach(async () => {
  await Promise.all(roots.splice(0).map(root => rm(root, { recursive: true, force: true })));
});

describe("DurableSpool protected admission", () => {
  test("missing durable marker never infers acceptance from journal bytes", async () => {
    const { root, spool } = await fixture();
    await spool.append(input);
    const journal = await readFile(join(root, "spool.journal"));
    await rm(join(root, "spool.durable"));
    expect((await new DurableSpool(root).status()).quarantined).toBe(true);
    expect(await new DurableSpool(root).pending()).toEqual([]);
    expect(await readFile(join(root, "spool.journal"))).toEqual(journal);
  });

  test("a corrupted boundary cannot truncate already accepted bytes", async () => {
    const { root, spool } = await fixture();
    await spool.append(input);
    const journal = await readFile(join(root, "spool.journal"));
    await writeFile(join(root, "spool.durable"), "0");
    expect((await new DurableSpool(root).status()).quarantined).toBe(true);
    expect(await readFile(join(root, "spool.journal"))).toEqual(journal);
  });

  test("a structurally valid earlier boundary cannot shorten the committed prefix", async () => {
    const { root, spool } = await fixture();
    await spool.append(input);
    const previous = await readFile(join(root, "spool.durable"));
    await spool.append({ ...input, revision: "revision-2" });
    const journal = await readFile(join(root, "spool.journal"));
    await writeFile(join(root, "spool.durable"), previous);
    await expect(new DurableSpool(root).append(input)).rejects.toThrow("quarantined");
    expect(await readFile(join(root, "spool.journal"))).toEqual(journal);
    expect(await readFile(join(root, "spool.durable"))).toEqual(previous);
  });

  test.each(["", "NaN", "Infinity", "-1", "1.5", "9007199254740992", "999999"])("rejects malformed or out-of-range marker %j without truncating", async marker => {
    const { root, spool } = await fixture();
    await spool.append(input);
    const journal = await readFile(join(root, "spool.journal"));
    await writeFile(join(root, "spool.durable"), marker);
    expect((await new DurableSpool(root).status()).quarantined).toBe(true);
    expect(await readFile(join(root, "spool.journal"))).toEqual(journal);
  });

  test.each(["missing", "corrupt"])("a %s marker proof cannot authorize recovery", async damage => {
    const { root, spool } = await fixture();
    await spool.append(input);
    const path = join(root, "spool.journal");
    const journal = Buffer.concat([await readFile(path), Buffer.from([0, 0, 0])]);
    await writeFile(path, journal);
    const proof = join(root, "spool.durable.sha256");
    if (damage === "missing") await rm(proof);
    else await writeFile(proof, "0".repeat(64));
    expect((await new DurableSpool(root).status()).quarantined).toBe(true);
    expect(await readFile(path)).toEqual(journal);
  });

  test("a missing journal cannot discard an existing durable admission", async () => {
    const { root, spool } = await fixture();
    await spool.append(input);
    await rm(join(root, "spool.journal"));
    await expect(new DurableSpool(root).append(input)).rejects.toThrow("quarantined");
    await expect(readFile(join(root, "spool.journal"))).rejects.toHaveProperty("code", "ENOENT");
  });

  test("append rejects a quarantined spool without mutating its evidence", async () => {
    const { root, spool } = await fixture();
    await spool.append(input);
    const path = join(root, "spool.journal");
    const data = await readFile(path);
    const byte = data.at(-1);
    if (byte === undefined) throw new Error("expected a frame");
    data[data.length - 1] = byte ^ 1;
    await writeFile(path, data);
    expect((await spool.status()).quarantined).toBe(true);
    const marker = await readFile(join(root, "spool.durable"));
    await expect(spool.append({ ...input, revision: "revision-2" })).rejects.toThrow("quarantined");
    expect(await readFile(path)).toEqual(data);
    expect(await readFile(join(root, "spool.durable"))).toEqual(marker);
  });

  test("append recovers an unpublished torn suffix before publishing its frame", async () => {
    const { root, spool } = await fixture();
    await spool.append(input);
    const path = join(root, "spool.journal");
    await writeFile(path, Buffer.concat([await readFile(path), Buffer.from([0, 0, 0])]));
    const reopened = new DurableSpool(root);
    expect(await reopened.append({ ...input, revision: "revision-2" })).toBe(2);
    expect((await new DurableSpool(root).pending()).map(entry => entry.revision))
      .toEqual(["revision-1", "revision-2"]);
  });

  test("corrupt protected bytes prevent even unpublished suffix truncation", async () => {
    const { root, spool } = await fixture();
    await spool.append(input);
    const path = join(root, "spool.journal");
    const bytes = await readFile(path);
    bytes[0] = 255;
    const damaged = Buffer.concat([bytes, Buffer.from([0, 0, 0])]);
    await writeFile(path, damaged);
    expect((await new DurableSpool(root).status()).quarantined).toBe(true);
    expect(await readFile(path)).toEqual(damaged);
  });

  test("same-instance concurrent appends allocate different sequences", async () => {
    const { root, spool } = await fixture();
    const sequences = await Promise.all(
      Array.from({ length: 12 }, (_, i) => spool.append({ ...input, revision: `revision-${i}` })),
    );
    expect(sequences).toEqual(Array.from({ length: 12 }, (_, i) => i + 1));
    expect((await new DurableSpool(root).pending()).length).toBe(12);
  });

  test("an unauthenticated completion cursor cannot hide pending records", async () => {
    const { root, spool } = await fixture();
    await spool.append(input);
    await writeFile(join(root, "spool.done"), "[1]");
    expect((await new DurableSpool(root).status()).quarantined).toBe(true);
    await expect(new DurableSpool(root).append(input)).rejects.toThrow("quarantined");
  });

  test("a damaged completion payload cannot authorize suffix truncation", async () => {
    const { root, spool } = await fixture();
    await spool.append(input);
    await spool.append({ ...input, revision: "revision-2" });
    await spool.complete(1);
    const cursor = JSON.parse(await readFile(join(root, "spool.done"), "utf8"));
    cursor.payload = JSON.stringify({ version: 1, sequences: [1, 2] });
    await writeFile(join(root, "spool.done"), JSON.stringify(cursor));
    const path = join(root, "spool.journal");
    const bytes = Buffer.concat([await readFile(path), Buffer.from([0])]);
    await writeFile(path, bytes);
    expect((await new DurableSpool(root).status()).quarantined).toBe(true);
    expect(await readFile(path)).toEqual(bytes);
    await expect(spool.complete(2)).rejects.toThrow("quarantined");
  });

  test("repeated completion is idempotent but unadmitted completion rejects", async () => {
    const { root, spool } = await fixture();
    await spool.append(input);
    await spool.complete(1);
    const cursor = await readFile(join(root, "spool.done"));
    await expect(new DurableSpool(root).complete(1)).resolves.toBeUndefined();
    expect(await readFile(join(root, "spool.done"))).toEqual(cursor);
    await expect(spool.complete(2)).rejects.toThrow("not pending");
  });

  test("reads and completion serialize with append and rejected operations release the queue", async () => {
    const { root, spool } = await fixture();
    await expect(spool.complete(1)).rejects.toThrow("not pending");
    const first = spool.append(input);
    const pending = spool.pending();
    const second = spool.append({ ...input, revision: "revision-2" });
    const completed = Promise.all([spool.complete(1), spool.complete(2)]);
    expect(await first).toBe(1);
    expect((await pending).map(entry => entry.sequence)).toEqual([1]);
    expect(await second).toBe(2);
    await completed;
    expect(await new DurableSpool(root).status()).toEqual({ pending: 0, nextSequence: 3, quarantined: false });
  });

  test("invalid JSON values reject before creating a journal", async () => {
    const { root, spool } = await fixture();
    const cycle: unknown[] = []; cycle.push(cycle);
    const invalid = [NaN, Infinity, -Infinity, undefined, 1n, Symbol("x"), () => 1, new Date(), new Map(), new Set(), new Array(1), cycle,
      { [Symbol("key")]: "value" }, { get value() { throw new Error("getter must not run"); } }];
    for (const value of invalid) {
      await expect(spool.append({ ...input, body: { value } })).rejects.toThrow();
      await expect(readFile(join(root, "spool.journal"))).rejects.toHaveProperty("code", "ENOENT");
    }
    await spool.append(input);
    const path = join(root, "spool.journal");
    const bytes = Buffer.concat([await readFile(path), Buffer.from([0, 0, 0])]);
    await writeFile(path, bytes);
    const marker = await readFile(join(root, "spool.durable"));
    for (const value of invalid) {
      await expect(spool.append({ ...input, body: [value] })).rejects.toThrow();
      expect(await readFile(path)).toEqual(bytes);
      expect(await readFile(join(root, "spool.durable"))).toEqual(marker);
    }
  });

  test("finite nested JSON values still round trip", async () => {
    const { root, spool } = await fixture();
    const body = { values: [null, true, false, 0, -1.5, "text", { nested: [1, 2] }] };
    await spool.append({ ...input, body });
    expect(await new DurableSpool(root).pending()).toEqual([{ ...input, body, sequence: 1 }]);
  });
});
