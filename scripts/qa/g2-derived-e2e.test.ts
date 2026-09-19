import { expect, test } from "bun:test";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "../../packages/core/node_modules/uuid/dist/esm/index.js";
import { Engine } from "../../packages/core/src/engine.ts";
import type { EmbeddingProvider } from "../../packages/core/src/embedding.ts";
import { ExtractionProviderError, type ExtractionProvider } from "../../packages/core/src/extraction.ts";
import type { InstallationContext } from "../../packages/core/src/store.ts";
import { Generation, extractionBodyDigest } from "../../packages/protocol/src/extraction.ts";

const context: InstallationContext = { principal: "installation", commit_mode: "auto" };
const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("owned runner credentials required");

const extractionProvider: ExtractionProvider = {
  model: "derived-fixture",
  modelIncarnation: "d".repeat(64),
  async extract(input) {
    if (input.task === "claim") {
      const text = input.text, end = Buffer.byteLength(text, "utf8");
      return { task: "claim", claims: [{ text, evidence: { start: 0, end, text } }], language: "und", modality: "text" };
    }
    const claims = input.claim_context?.claims ?? [];
    return { task: "judge_claims", claim_body_digest: input.claim_context?.body_digest,
      decisions: claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })), language: "und", modality: "text" };
  },
};
const profile = { model: "derived-embedding-fixture", model_incarnation: "e".repeat(64), dimensions: 2,
  document_prefix: "", query_prefix: "", max_input_bytes: 8192, norm: "unit_l2" as const, norm_tolerance: 0.001 };
const embeddingProvider: EmbeddingProvider = { profile, async embed() { return [1, 0]; } };

async function clearDatabase() {
  const driver = neo4j.driver(uri!, neo4j.auth.basic("neo4j", password!));
  try { await driver.executeQuery("MATCH (n) DETACH DELETE n"); }
  finally { await driver.close(); }
}

async function exercise(embedding: boolean) {
  const engine = new Engine({ uri: uri!, user: "neo4j", password: password!, ...(embedding ? { embeddingProvider } : {}), extractionProvider });
  await engine.init(); await engine.claimWriterEpoch();
  const episodes: string[] = [];
  for (let i = 0; i < 20; i++) {
    const remembered = await engine.remember({ schema: "anamnesis.original-message/1", content: `grounded claim ${i} prefers option ${i % 2}`,
      time: { value: `2026-09-01T00:00:${String(i).padStart(2, "0")}Z`, precision: "second" },
      origin: { source: "derived-fixture", session: "session", actor: "fixture", record: String(i) }, mass: 1 });
    episodes.push(remembered.id);
  }
  const generation = Generation.parse({ id: uuidv7(), stream: "extraction", incarnation: "f".repeat(64), state: "catching_up",
    covered_ingest_seq: 0, created_at: Date.now(), updated_at: Date.now() });
  await engine.store.createExtractionGeneration(generation, context);
  for (const source_id of episodes) {
    const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id }, context);
    await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "derived-fixture", lease_ms: 30000 }, context);
  }
  for (const partition of ["episodes", "active_extraction"] as const)
    await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 20 }, context);
  if (embedding) expect(await engine.drainEmbeddingOutbox(100)).toMatchObject({ drained: 20 });
  else expect(await engine.drainEmbeddingOutbox(100)).toEqual({ drained: 0, reason: "embeddings_disabled" });
  const selection = await engine.readExtractionSelection(context);
  const active = await engine.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: selection.generation_id, expected_selector_version: selection.selector_version }, context);
  expect(active.state).toBe("active");
  const result = await engine.recallHybrid({ query: "grounded claim 1", limit: 20, budget: { unit: "utf8_bytes", limit: 65536 } }, context);
  expect(result.results.some(item => item.kind === "Fact")).toBe(true);
  expect(result.diagnostics.pipeline).toBe("derived-hybrid-v1");
  expect(result.diagnostics.ppr_used).toBe(true);
  if (embedding) expect(result.diagnostics.channels_used).toContain("vector");
  else expect(result.diagnostics.channels_used).not.toContain("vector");
  await engine.setPolicy({ policy_id: uuidv7(), selector: { episode_id: episodes[1] }, scope: "content" }, context);
  const denied = await engine.recallHybrid({ query: "grounded claim 1", limit: 20, budget: { unit: "utf8_bytes", limit: 65536 } }, context);
  expect(denied.results.some(item => item.kind === "Fact" && item.sources.includes(episodes[1]!))).toBe(false);
  await engine.close();
}

test("derived materialization, optional embeddings, cutover, recall and policy authorization", async () => {
  await exercise(false);
  await clearDatabase();
  await exercise(true);
}, 900000);

test("coverage lag is named before activation", async () => {
  await clearDatabase();
  const engine = new Engine({ uri: uri!, user: "neo4j", password: password!, extractionProvider });
  await engine.init(); await engine.claimWriterEpoch();
  const remembered = await engine.remember({ schema: "anamnesis.original-message/1", content: "coverage lag", time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: "coverage", session: "s", actor: "fixture", record: "1" }, mass: 1 });
  const generation = Generation.parse({ id: uuidv7(), stream: "extraction", incarnation: "c".repeat(64), state: "catching_up", covered_ingest_seq: 0, created_at: Date.now(), updated_at: Date.now() });
  await engine.store.createExtractionGeneration(generation, context);
  for (const partition of ["episodes", "active_extraction"] as const)
    await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 0 }, context);
  const selection = await engine.readExtractionSelection(context);
  await expect(engine.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: selection.generation_id, expected_selector_version: selection.selector_version }, context)).rejects.toThrow("coverage_incomplete");
  expect(remembered.created).toBe(true);
  await engine.close();
}, 120000);
test("terminal failed judge is a sealed omission, not a Fact or a permanent generation blocker", async () => {
  await clearDatabase();
  let now = Date.now();
  const provider: ExtractionProvider = { ...extractionProvider, async extract(input) {
    if (input.task === "judge_claims" && input.text.includes("failed-source")) throw new ExtractionProviderError("provider_mismatch");
    return extractionProvider.extract(input);
  } };
  const engine = new Engine({ uri: uri!, user: "neo4j", password: password!, extractionProvider: provider, clock: () => now });
  try {
    await engine.init(); await engine.claimWriterEpoch();
    const sources: string[] = [];
    for (const content of ["grounded claim retained-source", "grounded claim failed-source"])
      sources.push((await engine.remember({ content, time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: "omission", session: "s", actor: "fixture", record: content } })).id);
    const generation = Generation.parse({ id: uuidv7(), stream: "extraction", incarnation: "a".repeat(64), state: "catching_up", covered_ingest_seq: 0, created_at: now, updated_at: now });
    await engine.store.createExtractionGeneration(generation, context);
    const good = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: sources[0]! }, context);
    await engine.runExtractionPipeline({ task_id: good.id, expected_version: good.version, worker_id: "omission", lease_ms: 30000 }, context);
    const bad = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: sources[1]! }, context);
    await engine.runExtractionTask({ task_id: bad.id, expected_version: bad.version, worker_id: "omission", lease_ms: 30000 }, context);
    const coverage = { generation_id: generation.id, partition: "episodes" as const, expected_covered_ingest_seq: 0, covered_ingest_seq: 2 };
    await expect(engine.store.recordExtractionCoverage(coverage, context)).rejects.toThrow("extraction_audit_incomplete"); // Missing judge.
    const judge = await engine.store.createExtractionJudgeTask({ claim_task_id: bad.id }, context);
    await expect(engine.store.recordExtractionCoverage(coverage, context)).rejects.toThrow("extraction_audit_incomplete"); // Queued.
    const leased = await engine.store.leaseModelTask({ task_id: judge.id, expected_version: judge.version, worker_id: "omission", lease_ms: 30000 }, context);
    await expect(engine.store.recordExtractionCoverage(coverage, context)).rejects.toThrow("extraction_audit_incomplete");
    now += 30001;
    const expired = await engine.store.settleModelTask({ task_id: leased.id, expected_version: leased.version, lease_epoch: leased.lease!.epoch, reason: "expired" }, context);
    await expect(engine.store.recordExtractionCoverage(coverage, context)).rejects.toThrow("extraction_audit_incomplete");
    let queued = await engine.store.retryModelTask({ task_id: expired.id, expected_version: expired.version }, context);
    const lostLease = await engine.store.leaseModelTask({ task_id: queued.id, expected_version: queued.version, worker_id: "lost-worker", lease_ms: 30000 }, context);
    await engine.claimWriterEpoch();
    const lost = await engine.store.settleModelTask({ task_id: lostLease.id, expected_version: lostLease.version, lease_epoch: lostLease.lease!.epoch, reason: "worker_lost" }, context);
    await expect(engine.store.recordExtractionCoverage(coverage, context)).rejects.toThrow("extraction_audit_incomplete");
    queued = await engine.store.retryModelTask({ task_id: lost.id, expected_version: lost.version }, context);
    for (let attempt = 0; attempt < 2; attempt++) {
      await engine.runExtractionTask({ task_id: queued.id, expected_version: queued.version, worker_id: "omission", lease_ms: 30000 }, context);
      const state = await engine.store.readExtractionPipeline(bad.id, context);
      expect(state.state).toBe("known"); if (state.state !== "known") throw new Error("unknown pipeline");
      expect(state.claim.state).toBe("succeeded"); expect(state.judge?.state).toBe("failed");
      expect(state.judge?.attempt_id).toBe(state.judge_attempt?.id);
      if (attempt === 0) queued = await engine.store.retryModelTask({ task_id: state.judge!.id, expected_version: state.judge!.version }, context);
    }
    for (const partition of ["episodes", "active_extraction"] as const) await engine.store.recordExtractionCoverage({ ...coverage, partition }, context);
    const selection = await engine.readExtractionSelection(context);
    expect((await engine.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: selection.generation_id, expected_selector_version: selection.selector_version }, context)).state).toBe("active");
    const recall = await engine.recallHybrid({ query: "grounded claim", limit: 20, budget: { unit: "utf8_bytes", limit: 65536 } }, context);
    expect(recall.results.some(item => item.kind === "Fact" && item.sources.includes(sources[0]!))).toBe(true);
    const driver = neo4j.driver(uri!, neo4j.auth.basic("neo4j", password!));
    try { expect((await driver.executeQuery("MATCH (f:Fact)-[:DERIVED_FROM]->(e:Episode {id:$source}) RETURN f.id", { source: sources[1] })).records).toHaveLength(0); }
    finally { await driver.close(); }
  } finally { await engine.close(); }
}, 120000);

test("failed and cancelled claims and judges seal content-free omissions and freeze retries", async () => {
  await clearDatabase();
  const provider: ExtractionProvider = { ...extractionProvider, async extract(input) {
    if ((input.task === "claim" && input.text === "claim-failed") || (input.task === "judge_claims" && input.text === "judge-failed")) throw new ExtractionProviderError("provider_mismatch");
    return extractionProvider.extract(input);
  } };
  const engine = new Engine({ uri: uri!, user: "neo4j", password: password!, extractionProvider: provider });
  try {
    await engine.init(); await engine.claimWriterEpoch();
    const sources: { content: string; id: string }[] = [];
    for (const content of ["retained", "claim-failed", "claim-cancelled", "judge-failed", "judge-cancelled"])
      sources.push({ content, id: (await engine.remember({ content, time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: "terminal-omissions", session: "s", actor: "fixture", record: content } })).id });
    const generation = Generation.parse({ id: uuidv7(), stream: "extraction", incarnation: "c".repeat(64), state: "catching_up", covered_ingest_seq: 0, created_at: Date.now(), updated_at: Date.now() });
    await engine.store.createExtractionGeneration(generation, context);
    let digest = extractionBodyDigest([]);
    const failed: { task_id: string; expected_version: number }[] = [];
    for (const source of sources) {
      const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: source.id }, context);
      if (source.content === "claim-cancelled") await engine.store.cancelModelTask({ task_id: task.id, expected_version: task.version }, context);
      else if (source.content === "judge-cancelled") {
        await engine.runExtractionTask({ task_id: task.id, expected_version: task.version, worker_id: "terminal", lease_ms: 30000 }, context);
        const judge = await engine.store.createExtractionJudgeTask({ claim_task_id: task.id }, context);
        await engine.store.cancelModelTask({ task_id: judge.id, expected_version: judge.version }, context);
      } else await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "terminal", lease_ms: 30000 }, context);
      const pipeline = await engine.store.readExtractionPipeline(task.id, context);
      if (pipeline.state !== "known") throw new Error("unknown pipeline");
      if (source.content === "retained") digest = extractionBodyDigest({ prior: digest, pipeline_id: task.id, judge_attempt_id: pipeline.judge_attempt!.id, decisions: pipeline.decisions.map(decision => decision.disposition) });
      else {
        const stage = source.content.startsWith("claim-") ? "claim" : "judge", attempt = stage === "claim" ? pipeline.claim_attempt! : pipeline.judge_attempt!, terminal = stage === "claim" ? pipeline.claim : pipeline.judge!;
        expect(terminal.attempt_id).toBe(attempt.id); expect(terminal.state).toBe(attempt.state);
        expect(attempt.output).toBeNull(); expect(attempt.spans).toEqual([]);
        digest = extractionBodyDigest({ prior: digest, pipeline_id: task.id, stage, attempt_id: attempt.id, state: attempt.state, reason: attempt.reason });
        if (terminal.state === "failed") failed.push({ task_id: terminal.id, expected_version: terminal.version });
      }
    }
    for (const partition of ["episodes", "active_extraction"] as const) expect((await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: sources.length }, context)).omission_digest).toBe(digest);
    for (const task of failed) await expect(engine.store.retryModelTask(task, context)).rejects.toThrow("coverage_frozen");
    const selection = await engine.readExtractionSelection(context);
    expect((await engine.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: selection.generation_id, expected_selector_version: selection.selector_version }, context)).state).toBe("active");
    const driver = neo4j.driver(uri!, neo4j.auth.basic("neo4j", password!));
    try { const facts = await driver.executeQuery("MATCH (:Fact)-[:DERIVED_FROM]->(e:Episode) RETURN e.id AS source"); expect(facts.records.map(row => row.get("source"))).toEqual([sources[0]!.id]); }
    finally { await driver.close(); }
  } finally { await engine.close(); }
}, 120000);

test("two hundred sources can activate multiple retained Facts per source", async () => {
  await clearDatabase();
  const provider: ExtractionProvider = { ...extractionProvider, async extract(input) {
    if (input.task !== "claim") return extractionProvider.extract(input);
    const claims = input.text.split(" | ").map(text => {
      const start = Buffer.from(input.text).indexOf(Buffer.from(text));
      return { text, evidence: { text, start, end: start + Buffer.byteLength(text) } };
    });
    return { task: "claim", claims, language: "en", modality: "text" };
  } };
  const engine = new Engine({ uri: uri!, user: "neo4j", password: password!, extractionProvider: provider });
  try {
    await engine.init(); await engine.claimWriterEpoch();
    const sources: string[] = [];
    for (let i = 0; i < 200; i++) sources.push((await engine.remember({
      content: `project ${i} uses SQLite | project ${i} retains local backups`,
      time: { value: new Date(Date.UTC(2026, 8, 1) + i * 1000).toISOString(), precision: "second" },
      origin: { source: "derived-scale", session: "s", actor: "fixture", record: String(i) },
    })).id);
    const generation = Generation.parse({ id: uuidv7(), stream: "extraction", incarnation: "b".repeat(64), state: "catching_up", covered_ingest_seq: 0, created_at: Date.now(), updated_at: Date.now() });
    await engine.store.createExtractionGeneration(generation, context);
    for (const source_id of sources) {
      const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id }, context);
      await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "scale", lease_ms: 30000 }, context);
    }
    // The audit bridge records one operation for the source's whole claim batch.
    // Seed the supported per-Fact custody shape for each second retained Fact;
    // every added operation references an actual Engine-created evidence link.
    const custody = neo4j.driver(uri!, neo4j.auth.basic("neo4j", password!), { disableLosslessIntegers: true });
    try {
      const extra = await custody.executeQuery(`MATCH (o:MaterializationOperation {generation:$generation})
        MATCH (f:Fact {generation:$generation})-[l:DERIVED_FROM]->(e:Episode {id:o.source_episode_id})
        WHERE f.id <> o.fact_id RETURN o.id AS original,f.id AS fact,l.id AS link,e.id AS source`, { generation: generation.id });
      expect(extra.records).toHaveLength(200);
      for (const row of extra.records) {
        const fact = row.get("fact"), link = row.get("link"), source = row.get("source");
        await custody.executeQuery(`MATCH (o:MaterializationOperation {id:$original})
          CREATE (:MaterializationOperation {id:$id,occurrence_key:$key,digest:$digest,result:$result,fact_id:$fact,link_id:$link,
            generation:o.generation,source_episode_id:o.source_episode_id,semantic_profile_id:o.semantic_profile_id})`, {
          original: row.get("original"), id: uuidv7(), key: extractionBodyDigest([generation.id, source, fact]),
          digest: extractionBodyDigest({ generation: generation.id, source, fact, link }),
          result: JSON.stringify({ created: true, fact_id: fact, link_id: link }), fact, link,
        });
      }
      expect((await custody.executeQuery("MATCH (o:MaterializationOperation {generation:$generation}) RETURN count(o) AS count", { generation: generation.id })).records[0]!.get("count")).toBe(400);
    } finally { await custody.close(); }
    for (const partition of ["episodes", "active_extraction"] as const) await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 200 }, context);
    const selection = await engine.readExtractionSelection(context);
    expect((await engine.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: selection.generation_id, expected_selector_version: selection.selector_version }, context)).state).toBe("active");
    const driver = neo4j.driver(uri!, neo4j.auth.basic("neo4j", password!), { disableLosslessIntegers: true });
    try { expect((await driver.executeQuery("MATCH (f:Fact {generation:$generation}) RETURN count(f) AS count", { generation: generation.id })).records[0]!.get("count")).toBe(400); }
    finally { await driver.close(); }
  } finally { await engine.close(); }
}, 900000);

test("stale writer epochs reject derived-surface writes", async () => {
  await clearDatabase();
  const first = new Engine({ uri: uri!, user: "neo4j", password: password!, extractionProvider });
  const second = new Engine({ uri: uri!, user: "neo4j", password: password!, extractionProvider });
  await first.init(); await first.claimWriterEpoch(); await second.claimWriterEpoch();
  await expect(first.store.putElement({ id: uuidv7(), schema: "anamnesis.original-message/1", content: "stale", time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: "stale", session: "s", actor: "fixture", record: "1" }, mass: 1 })).rejects.toThrow("stale_writer_epoch");
  await first.close(); await second.close();
}, 120000);
