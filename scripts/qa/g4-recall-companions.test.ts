import { expect, test } from "bun:test";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "../../packages/core/node_modules/uuid/dist/esm/index.js";
import { Engine } from "../../packages/core/src/engine.ts";
import type { ExtractionProvider } from "../../packages/core/src/extraction.ts";
import { Generation } from "../../packages/protocol/src/extraction.ts";

const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
if (!uri || !password) throw new Error("owned runner credentials required");
const context = { principal: "installation", commit_mode: "auto", client_binding: uuidv7() } as const;
const lineage = { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context } as const;
const provider: ExtractionProvider = {
  model: "contrast-fixture", modelIncarnation: "a".repeat(64),
  async extract(input) {
    const claims = (content: string) => content.split(" | ").map(text => {
      const start = Buffer.from(content).indexOf(Buffer.from(text));
      return { text, evidence: { text, start, end: start + Buffer.byteLength(text) }, confidence: 0.9 };
    });
    if (input.task === "claim") return { task: "claim", language: "en", modality: "text", claims: claims(input.text) };
    if (input.task === "claim_window") return { task: "claim_window", episodes: input.episodes.map(({ id, text }) => ({
      id, language: "en", modality: "text", claims: claims(text),
    })) };
    if (input.task === "judge_relations") return { task: "judge_relations", relation_context_digest: input.relation_context?.body_digest,
      judgements: input.relation_context?.candidates.map(candidate => ({
        candidate_id: candidate.id, relation: "unrelated", confidence: 0.9, reason: "distinct fixture claim",
      })), language: "en", modality: "text" };
    if (input.task !== "judge_claims") throw new Error("unexpected extraction task");
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
      sources.push((await engine.remember({ content, time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: "contrast", session: "s", actor: "fixture", record: String(i) } }, lineage)).id);
    const generation = Generation.parse({ id: uuidv7(), stream: "extraction", incarnation: "b".repeat(64), state: "catching_up", covered_ingest_seq: 0, created_at: Date.now(), updated_at: Date.now() });
    await engine.store.createExtractionGeneration(generation, context);
    // Coverage prunes terminal pipelines from the extraction journal, so each outcome is asserted as it settles.
    for (const source_id of sources) {
      const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id }, context);
      const pipeline = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "contrast", lease_ms: 30000 }, context);
      if (pipeline.state !== "known") throw new Error("pipeline_unknown");
      expect(pipeline.claim.state).toBe("succeeded"); expect(pipeline.judge?.state).toBe("succeeded");
    }
    for (const partition of ["episodes", "active_extraction"] as const) await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 20 }, context);
    expect(await engine.drainEmbeddingOutbox(1000)).toEqual({ drained: 0, reason: "embeddings_disabled" });
    const selection = await engine.readExtractionSelection(context);
    await engine.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: selection.generation_id, expected_selector_version: selection.selector_version }, context);
    // No embedding provider: all 21 materialized Facts still lack a vector.
    expect((await engine.status()).pendingOutbox).toBe(21);
    const rows = await driver.executeQuery("MATCH (f:Fact)-[:DERIVED_FROM]->(e:Episode) RETURN f.id AS id,f.content AS content,e.id AS source ORDER BY f.id");
    const visible = rows.records.filter(row => row.get("source") === sources[0]).map(row => String(row.get("id")));
    const hidden = String(rows.records.find(row => row.get("source") === sources[1])!.get("id"));
    await engine.link({ id: uuidv7(), from: visible[0]!, to: visible[1]!, role: "CONTRASTS", content: "visible fixture contrast", weight: 1 });
    await engine.link({ id: uuidv7(), from: visible[0]!, to: hidden, role: "CONTRASTS", content: "denied fixture contrast", weight: 1 });
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
    const contrastIds = new Set(result.results[0]!.provenance.contrasts);
    const contrastCompanions = result.companions.filter(item => contrastIds.has(item.id));
    expect(contrastCompanions.map(item => item.id)).toEqual(result.results[0]!.provenance.contrasts);
    expect(contrastCompanions.every(item => item.kind === "Fact" && item.sources.includes(sources[0]!))).toBe(true);
    expect(result.context_text.split("\n")).toHaveLength(1 + result.companions.length);
    expect(result.used_budget).toBe(Buffer.byteLength(result.context_text));
    expect(JSON.stringify(result).includes(hidden)).toBe(false);
    // Window companions are optional: one byte short of the full response drops one and keeps the Fact with its contrast.
    expect(result.companions.length).toBeGreaterThan(contrastCompanions.length);
    const trimmed = await engine.recallHybrid({ query, limit: 1, budget: { unit: "utf8_bytes", limit: result.used_budget - 1 } }, context);
    expect(trimmed.results.map(item => item.id)).toEqual(result.results.map(item => item.id));
    expect(trimmed.companions.filter(item => contrastIds.has(item.id)).map(item => item.id)).toEqual(result.results[0]!.provenance.contrasts);
    expect(trimmed.companions.length).toBeLessThan(result.companions.length);
    // A contrast is mandatory: a budget that holds only the contrast lines skips the whole Fact bundle.
    const contrastOnly = Buffer.byteLength(result.context_text.split("\n").slice(1, 1 + contrastIds.size).join("\n"));
    const small = await engine.recallHybrid({ query, limit: 1, budget: { unit: "utf8_bytes", limit: contrastOnly } }, context);
    expect(small.results.map(item => item.id)).not.toContain(result.results[0]!.id);
    expect(small.diagnostics.skipped_bundles).toBeGreaterThan(0);
  } finally { await engine.close(); await driver.close(); }
}, 120000);
