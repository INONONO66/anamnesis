import { expect, test } from "bun:test";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "../../packages/core/node_modules/uuid/dist/esm/index.js";
import { Engine } from "../../packages/core/src/engine.ts";
import type { ExtractionProvider } from "../../packages/core/src/extraction.ts";
import { Generation } from "../../packages/protocol/src/extraction.ts";

const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
if (!uri || !password) throw new Error("owned runner credentials required");
const context = { principal: "installation", commit_mode: "auto" } as const;
const provider: ExtractionProvider = {
  model: "contrast-fixture", modelIncarnation: "a".repeat(64),
  async extract(input) {
    if (input.task === "claim") return { task: "claim", language: "en", modality: "text", claims: input.text.split(" | ").map(text => {
      const start = Buffer.from(input.text).indexOf(Buffer.from(text));
      return { text, evidence: { text, start, end: start + Buffer.byteLength(text) } };
    }) };
    return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context!.body_digest,
      decisions: input.claim_context!.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
  },
};

test("digest then full-content top-20 recall preserves the fence and includes mandatory contrast companions", async () => {
  const driver = neo4j.driver(uri!, neo4j.auth.basic("neo4j", password!));
  const engine = new Engine({ uri: uri!, user: "neo4j", password: password!, extractionProvider: provider });
  try {
    await driver.executeQuery("MATCH (n) DETACH DELETE n");
    await engine.init(); await engine.claimWriterEpoch();
    const sources: string[] = [];
    const contents = ["Anamnesis keeps red backups | Anamnesis keeps blue backups", "Anamnesis keeps confidential backups",
      ...Array.from({ length: 18 }, (_, i) => `fixture task ${i} completed safely`)];
    for (const [i, content] of contents.entries())
      sources.push((await engine.remember({ content, time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: "contrast", session: "s", actor: "fixture", record: String(i) } })).id);
    const generation = Generation.parse({ id: uuidv7(), stream: "extraction", incarnation: "b".repeat(64), state: "catching_up", covered_ingest_seq: 0, created_at: Date.now(), updated_at: Date.now() });
    await engine.store.createExtractionGeneration(generation, context);
    const pipelines = new Map<string, string>();
    for (const source_id of sources) {
      const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id }, context);
      pipelines.set(source_id, task.id);
      await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "contrast", lease_ms: 30000 }, context);
    }
    for (const partition of ["episodes", "active_extraction"] as const) await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 20 }, context);
    expect(await engine.drainEmbeddingOutbox(1000)).toEqual({ drained: 0, reason: "embeddings_disabled" });
    const selection = await engine.readExtractionSelection(context);
    await engine.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: selection.generation_id, expected_selector_version: selection.selector_version }, context);
    expect((await engine.status()).pendingOutbox).toBe(20);
    expect(await engine.digest(async episode => {
      const pipeline = await engine.store.readExtractionPipeline(pipelines.get(episode.id)!, context);
      if (pipeline.state !== "known") throw new Error("pipeline_unknown");
      expect(pipeline.claim.state).toBe("succeeded"); expect(pipeline.judge?.state).toBe("succeeded");
    })).toBe(20);
    expect((await engine.status()).pendingOutbox).toBe(0);
    const rows = await driver.executeQuery("MATCH (f:Fact)-[:DERIVED_FROM]->(e:Episode) RETURN f.id AS id,f.content AS content,e.id AS source ORDER BY f.id");
    const visible = rows.records.filter(row => row.get("source") === sources[0]).map(row => String(row.get("id")));
    const hidden = String(rows.records.find(row => row.get("source") === sources[1])!.get("id"));
    await engine.link({ id: uuidv7(), from: visible[0]!, to: hidden, role: "CONTRASTS", content: "fixture contrast", weight: 1 });
    await engine.setPolicy({ policy_id: uuidv7(), selector: { episode_id: sources[1]! }, scope: "content" }, context);
    const query = String(rows.records.find(row => row.get("id") === visible[0])!.get("content"));
    const full = await engine.recallHybrid({ query, limit: 20, budget: { unit: "utf8_bytes", limit: 65536 } }, context);
    expect(full.results.some(item => item.kind === "Fact")).toBe(true); expect(full.diagnostics.ppr_used).toBe(true);
    const allIds = new Set([...full.results, ...full.companions].map(item => item.id));
    expect(full.results.every(item => item.provenance.contrasts.every(id => allIds.has(id)))).toBe(true);
    expect(JSON.stringify(full).includes(hidden)).toBe(false);
    const result = await engine.recallHybrid({ query, limit: 1, budget: { unit: "utf8_bytes", limit: 65536 } }, context);
    expect(result.results).toHaveLength(1); expect(result.results[0]!.kind).toBe("Fact");
    expect(result.results[0]!.provenance.contrasts).toHaveLength(1);
    expect(result.companions.map(item => item.id)).toEqual(result.results[0]!.provenance.contrasts);
    expect(result.companions.every(item => item.kind === "Fact" && item.sources.includes(sources[0]!))).toBe(true);
    expect(result.context_text.split("\n")).toHaveLength(2);
    expect(result.used_budget).toBe(Buffer.byteLength(result.context_text));
    expect(JSON.stringify(result).includes(hidden)).toBe(false);
    const small = await engine.recallHybrid({ query, limit: 1, budget: { unit: "utf8_bytes", limit: result.used_budget - 1 } }, context);
    expect(small.results.some(item => item.kind === "Fact")).toBe(false);
    expect(small.diagnostics.skipped_bundles).toBeGreaterThan(0);
  } finally { await engine.close(); await driver.close(); }
}, 120000);
