import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";
import { ExtractionScheduler } from "./extraction-scheduler.ts";
import type { ExtractionProviderInput } from "./extraction.ts";
import { extractionBodyDigest } from "../../protocol/src/extraction.ts";

const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const model = "qa-scheduler", incarnation = extractionBodyDigest(model);

/** Owned harness Neo4j, an Engine and a scheduler that share one injected clock, and a provider whose every call
 * advances that clock past the task lease before answering: the answer is valid, the lease is not. No timers. */
async function setup() {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "scheduler-"));
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI!, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD!;
  if (!uri || !password) throw new Error("owned runner required");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async (cypher: string, params: Record<string, unknown> = {}) =>
    (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  let offset = 0, calls = 0;
  const clock = () => Date.now() + offset;
  const provider = { model, modelIncarnation: incarnation, async extract(input: ExtractionProviderInput) {
    calls++;
    offset += 10 * 60 * 1000; // Every provider round trip outlives the lease.
    const evidence = { start: 0, end: Buffer.byteLength(input.text), text: input.text };
    return { model, model_incarnation: incarnation, output: { task: "claim", language: "en", modality: "text",
      claims: [{ text: input.text, evidence, confidence: 0.9, entities: [] }] } };
  } };
  const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider: provider, clock });
  await query("MATCH (n) DETACH DELETE n");
  await engine.init(); await engine.claimWriterEpoch();
  const wakes: number[] = [];
  const scheduler = new ExtractionScheduler(engine, { provider, context, clock, maxAttempts: 2, maxInFlight: 1,
    read: async (cypher, params) => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject()) as never,
    wake: () => { wakes.push(clock()); } });
  return { engine, scheduler, query, wakes, calls: () => calls, root,
    async close() { await scheduler.close(); await engine.close(); await driver.close(); await rm(root, { recursive: true, force: true }); } };
}

test("a claim whose lease keeps expiring is sealed as a durable omission after the attempt budget, and coverage advances past it", async () => {
  const f = await setup();
  try {
    await f.engine.remember({ content: "the lease of this claim always expires", time: { value: "2026-09-10T00:00:00Z", precision: "day" as const },
      origin: { source: f.root, session: f.root, actor: "user", record: "1" }, source_revision: "v1", expected_previous_revision_key: null },
      { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
    // Every turn is one bounded unit of writer work; a drive settles between turns because the provider never awaits.
    // Budget: create+lease (1), settle expired + retry + lease (2), settle + seal (3), cover (4), cutover (5); a few spare.
    let turns = 0, status = f.scheduler.status();
    while (turns++ < 10) {
      const settled = [...(f.scheduler as unknown as { inFlight: Map<string, Promise<void>> }).inFlight.values()];
      await Promise.allSettled(settled);
      await f.scheduler.turn();
      await Promise.allSettled([...(f.scheduler as unknown as { inFlight: Map<string, Promise<void>> }).inFlight.values()]);
      status = f.scheduler.status();
      if (status.state !== "starting" && status.covered_ingest_seq === 1 && status.in_flight === 0) break;
    }
    expect(status).toMatchObject({ state: "active", covered_ingest_seq: 1, live_ingest_seq: 1, in_flight: 0, completed_total: 0, failed_total: 1 });
    expect(f.calls()).toBe(2); // maxAttempts leases, each answered after its lease ran out
    const tasks = await f.query("MATCH (t:ModelTask) RETURN t.state AS state, t.body AS body");
    expect(tasks).toHaveLength(1);
    const task = JSON.parse(String(tasks[0]!.body)) as { state: string; attempts: number; attempt_id: string };
    expect(task.state).toBe("cancelled");
    expect(task.attempts).toBe(2);
    const attempts = await f.query("MATCH (a:ExtractionAttempt) RETURN a.state AS state ORDER BY a.id");
    expect(attempts.map(row => row.state)).toEqual(["expired", "expired", "cancelled"]);
    const coverage = await f.query("MATCH (c:ExtractionCoverage) RETURN c.covered_ingest_seq AS covered ORDER BY c.key");
    expect(coverage).toEqual([{ covered: 1 }, { covered: 1 }]);
    expect(await f.query("MATCH (f:Fact) RETURN count(f) AS n")).toEqual([{ n: 0 }]);
  } finally { await f.close(); }
});
