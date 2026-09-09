import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { EventEmitter, once } from "node:events";
import { randomUUID } from "node:crypto";
import { v7 as uuidv7 } from "uuid";
import neo4j, { type ManagedTransaction, type RecordShape } from "neo4j-driver";
import { Engine, type RememberInput } from "./engine.ts";
import { Store } from "./store.ts";

const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("Isolated test database credentials required");
const root = await mkdtemp(join(tmpdir(), "writer-fence-"));
const options = { uri, user: "neo4j", password, objectsRoot: root };
const oldEngine = new Engine(options);
const newEngine = new Engine(options);
const admin = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });

async function sequence(): Promise<number> {
  const result = await admin.executeQuery(
    "MATCH (m:Meta {key: 'meta'}) RETURN m.ingest_seq AS seq",
  );
  return result.records[0]!.get("seq");
}
const input: RememberInput = {
  content: "fenced write",
  time: { value: "2026-09-09T00:00:00Z", precision: "second" },
  origin: { source: "writer-fence", session: root, actor: "test", record: "one" },
};

beforeAll(async () => {
  await oldEngine.init();
  await oldEngine.claimWriterEpoch();
});
afterAll(async () => {
  await oldEngine.close();
  await newEngine.close();
  await admin.close();
  await rm(root, { recursive: true });
});

test("a newly claimed epoch fences stale writes without consuming sequence", async () => {
  const before = await oldEngine.status();
  const seq = await sequence();
  const firstEpoch = await oldEngine.claimWriterEpoch();
  expect(await newEngine.claimWriterEpoch()).toBeGreaterThan(firstEpoch);
  await expect(oldEngine.remember(input)).rejects.toThrow("stale_writer_epoch");
  expect(await oldEngine.status()).toEqual(before);
  expect(await sequence()).toBe(seq);
});

test("a concurrent epoch claim cannot overtake a validated write or consume stale sequence", async () => {
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const store = new Store(options, driver);
  const tag = randomUUID();
  const events = new EventEmitter();
  const controller = new AbortController();
  const deadline = setTimeout(() => controller.abort(), 10_000);
  // Both subscriptions exist before the writer is triggered. Only its first
  // real query result is paused; no database operation or result is fabricated.
  const validated = once(events, "validated", { signal: controller.signal });
  const resume = once(events, "resume", { signal: controller.signal });
  const openSession = driver.session.bind(driver);
  let armed = false;
  driver.session = (config) => {
    const session = openSession(config);
    const executeWrite = session.executeWrite.bind(session);
    session.executeWrite = (work, txConfig) => executeWrite(async (tx) => {
      const run = tx.run.bind(tx);
      tx.run = function<R extends RecordShape>(...args: Parameters<ManagedTransaction["run"]>) {
        const result = run<R>(...args);
        if (armed) {
          armed = false;
          const then = result.then.bind(result);
          result.then = (fulfilled, rejected) => then(async (value) => {
            events.emit("validated");
            await resume;
            return value;
          }).then(fulfilled, rejected);
        }
        return result;
      };
      return work(tx);
    }, { ...txConfig, metadata: { ...txConfig?.metadata, writerFence: tag } });
    return session;
  };
  const controlSession = admin.session();
  const control = controlSession.beginTransaction({ metadata: { writerFence: `${tag}-read` } });
  let writing: Promise<unknown> | undefined;
  let claiming: Promise<number> | undefined;
  try {
    const epoch = await store.claimWriterEpoch();
    await control.run(`MATCH (m:Meta {key: 'meta'}) WHERE m.writer_epoch = $epoch
      RETURN m.writer_epoch AS epoch`, { epoch });
    const before = await sequence();
    const counts = await store.counts();
    armed = true;
    writing = store.putElement({ ...input, id: uuidv7(), schema: "anamnesis.original-message/1",
      origin: { ...input.origin, record: tag } }, { enqueue: true });
    await validated;
    const transactions = await admin.executeQuery(
      `SHOW TRANSACTIONS YIELD metaData, activeLockCount
       WHERE metaData.writerFence IN [$tag, $readTag]
       RETURN metaData.writerFence AS tag, activeLockCount AS locks`, { tag, readTag: `${tag}-read` });
    expect(transactions.records).toHaveLength(2);
    const locks = transactions.records.find((record) => record.get("tag") === tag)!.get("locks");
    const readLocks = transactions.records.find((record) => record.get("tag") === `${tag}-read`)!.get("locks");
    let sequenceAtClaim = -1;
    claiming = newEngine.claimWriterEpoch().then(async (epoch) => {
      sequenceAtClaim = await sequence();
      return epoch;
    });
    // Without a lock, deterministically let the successor commit before the
    // old writer resumes. With the lock, the successor must wait for that write.
    if (locks === readLocks) await claiming;
    events.emit("resume");
    await Promise.all([writing, claiming]);
    const after = await sequence();
    console.log(JSON.stringify({ fenceLocks: locks, readLocks, before, sequenceAtClaim, after }));
    expect(after).toBe(sequenceAtClaim);
    expect(after).toBe(before + 1);
    expect(locks).toBeGreaterThan(readLocks);
    expect(await store.counts()).toEqual({ ...counts, elements: counts.elements + 1, pending: counts.pending + 1 });
    await expect(store.putElement({ ...input, id: uuidv7(), schema: "anamnesis.original-message/1",
      origin: { ...input.origin, record: `${tag}-stale` } }, { enqueue: true })).rejects.toThrow("stale_writer_epoch");
    expect(await sequence()).toBe(after);
    expect(await store.counts()).toEqual({ ...counts, elements: counts.elements + 1, pending: counts.pending + 1 });
  } finally {
    events.emit("resume");
    clearTimeout(deadline);
    try { await Promise.all([writing, claiming]); } finally {
      await control.rollback();
      await controlSession.close();
      await store.close();
    }
  }
}, 20_000);

test("concurrent claims allocate distinct consecutive epochs without advancing sequence", async () => {
  const engines = Array.from({ length: 4 }, () => new Engine(options));
  try {
    const first = await newEngine.claimWriterEpoch();
    const seq = await sequence();
    const epochs = await Promise.all(engines.map((engine) => engine.claimWriterEpoch()));
    expect([...epochs].sort((a, b) => a - b)).toEqual([first + 1, first + 2, first + 3, first + 4]);
    expect(await sequence()).toBe(seq);
    for (const [index, engine] of engines.entries()) {
      if (epochs[index] !== first + 4) {
        await expect(engine.remember(input)).rejects.toThrow("stale_writer_epoch");
      }
    }
    expect(await sequence()).toBe(seq);
  } finally {
    await Promise.all(engines.map((engine) => engine.close()));
  }
});


test("all claimed mutation paths reject stale epochs while legacy callers remain unfenced", async () => {
  const legacy = new Engine(options);
  try {
    await oldEngine.claimWriterEpoch();
    await newEngine.claimWriterEpoch();
    const saved = await legacy.remember({ ...input, origin: { ...input.origin, record: randomUUID() } });
    const before = await sequence();
    const counts = await oldEngine.status();
    const mutations = [
      () => oldEngine.remember(input),
      () => oldEngine.put({ ...input, id: uuidv7(), schema: "anamnesis.claim/1" }),
      () => oldEngine.link({ id: uuidv7(), from: saved.id, to: uuidv7(), role: "NEXT_EPISODE", content: "fenced" }),
      () => oldEngine.store.markProcessed([saved.id]),
      () => oldEngine.requeueEpisodes(),
      () => oldEngine.store.rebuildTopology(),
    ];
    for (const mutate of mutations) {
      await expect(mutate()).rejects.toThrow("stale_writer_epoch");
      expect(await sequence()).toBe(before);
      expect(await oldEngine.status()).toEqual(counts);
    }
    expect(await newEngine.remember({ ...input, origin: { ...input.origin, record: randomUUID() } })).toHaveProperty("created", true);
    expect(await sequence()).toBe(before + 1);
  } finally {
    await legacy.close();
  }
});
