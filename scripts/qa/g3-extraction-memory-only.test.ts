import { expect, test } from "bun:test";
import { once, EventEmitter } from "node:events";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "../../packages/core/src/engine.ts";
import { ExtractionScheduler } from "../../packages/core/src/extraction-scheduler.ts";
import type { ExtractionProviderInput } from "../../packages/core/src/extraction.ts";
import { extractionBodyDigest } from "../../packages/protocol/src/extraction.ts";

const labelsToRemove = ["ModelTask", "ExtractionAttempt", "ExtractionPipeline", "ExtractionJudgeInput", "ExtractionDisposition"] as const;
const constraintsToRemove = ["extraction_attempt_id", "extraction_disposition_key", "extraction_judge_input_id",
  "extraction_pipeline_id", "extraction_pipeline_judge", "model_task_id", "model_task_work_key"] as const;
const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const model = "g3-memory-only";

test("five Episodes produce distinct Facts with no graph extraction ledgers and a pruned journal", async () => {
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI;
  const password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
  if (!uri || !password) throw new Error("owned runner required");
  const root = await mkdtemp(join(tmpdir(), "g3-memory-only-"));
  const journalPath = join(root, "extraction-state.json");
  const driver = neo4j.driver(uri, neo4j.auth.basic(process.env.ANAMNESIS_TEST_NEO4J_USER ?? "neo4j", password), { disableLosslessIntegers: true });
  const events = new EventEmitter();
  const audit: { event: string; fields: Record<string, unknown> }[] = [];
  const provider = {
    model, modelIncarnation: extractionBodyDigest(model),
    async extract(input: ExtractionProviderInput) {
      const evidence = { start: 0, end: Buffer.byteLength(input.text), text: input.text };
      if (input.task === "claim") return { task: "claim", language: "en", modality: "text",
        claims: [{ text: input.text, evidence, confidence: 0.9, entities: [{ mention: "Alice", normalized_name: "Alice", entity_kind: "person" }] }] };
      if (input.task === "judge_claims") return { task: "judge_claims", language: "en", modality: "text",
        claim_body_digest: input.claim_context!.body_digest,
        decisions: input.claim_context!.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence, confidence: 0.85 })) };
      return { task: "judge_relations", language: "en", modality: "text", relation_context_digest: input.relation_context!.body_digest,
        judgements: input.relation_context!.candidates.map(candidate => ({ candidate_id: candidate.id, relation: "unrelated", confidence: 0.3, reason: "scripted" })) };
    },
  };
  const clock = () => Date.now();
  const engine = new Engine({ uri, password, objectsRoot: root, extractionJournalPath: journalPath, extractionProvider: provider, clock,
    audit: (event, fields) => { audit.push({ event, fields }); } });
  const query = async (cypher: string, params: Record<string, unknown> = {}) =>
    (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  const scheduler = new ExtractionScheduler(engine, { provider, context, clock, maxInFlight: 1,
    read: async (cypher, params) => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject()) as never,
    wake: () => { events.emit("wake"); } });
  try {
    await query("MATCH (n) DETACH DELETE n");
    await engine.init();
    await engine.claimWriterEpoch();
    const sourceIds: string[] = [];
    for (let i = 0; i < 5; i++) {
      const episode = await engine.remember({ content: `Alice remembers distinct detail ${i}`, time: { value: "2026-09-10T00:00:00Z", precision: "day" },
        origin: { source: root, session: root, actor: "user", record: String(i) },
        source_revision: "v1", expected_previous_revision_key: null },
      { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
      sourceIds.push(episode.id);
    }
    let status = scheduler.status();
    for (let turns = 0; turns < 40; turns++) {
      const controller = new AbortController();
      const deadline = setTimeout(() => controller.abort(new Error("scheduler wake deadline")), 60000);
      const awakened = once(events, "wake", { signal: controller.signal });
      void awakened.catch(() => {});
      await scheduler.turn();
      if (scheduler.inFlightCount) await awakened;
      controller.abort();
      clearTimeout(deadline);
      status = scheduler.status();
      if (status.state === "active" && status.covered_ingest_seq === 5 && status.live_ingest_seq === 5 && status.in_flight === 0) break;
    }
    expect(status).toMatchObject({ state: "active", covered_ingest_seq: 5, live_ingest_seq: 5, in_flight: 0, completed_total: 5, failed_total: 0 });
    const counts = (await query("MATCH (f:Fact) RETURN count(f) AS facts, count(DISTINCT f.meaning_digest) AS distinct_digests"))[0];
    const facts = counts?.facts, distinct_digests = counts?.distinct_digests;
    expect(facts).toBe(5);
    expect(distinct_digests).toBe(5);
    const labels = (await query("CALL db.labels() YIELD label RETURN label")).map(row => String(row.label));
    const constraints = (await query("SHOW CONSTRAINTS YIELD name RETURN name")).map(row => String(row.name));
    expect(labels.filter(label => new Set<string>(labelsToRemove).has(label))).toEqual([]);
    expect(constraints.filter(name => new Set<string>(constraintsToRemove).has(name))).toEqual([]);
    const journal = JSON.parse(await readFile(journalPath, "utf8")) as { pipelines: Record<string, { source_id: string }> };
    const journalEntries = Object.values(journal.pipelines);
    expect(journalEntries.filter(entry => sourceIds.includes(entry.source_id))).toEqual([]);
    for (const event of ["extraction.task.leased", "extraction.attempt.recorded"]) {
      const sources = new Set(audit.filter(row => row.event === event).map(row => row.fields.source_id));
      expect(sourceIds.every(id => sources.has(id))).toBe(true);
    }
    const evidence = { episodes: sourceIds.length, facts, distinct_digests, labels, constraints, journal_entries: journalEntries.length };
    const evidenceRoot = resolve(".omo/evidence/foundation/g3-qa");
    await mkdir(evidenceRoot, { recursive: true });
    await writeFile(join(evidenceRoot, "c001.json"), JSON.stringify(evidence, null, 2) + "\n");
  } finally {
    await scheduler.close();
    await engine.close();
    await driver.close();
    await rm(root, { recursive: true, force: true });
  }
}, 300000);
