import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import neo4j, { type Driver } from "neo4j-driver";
import { canonicalExtractionBody, extractionBodyDigest } from "@anamnesis/protocol";
import { main } from "../migrate-materialization-ledgers.ts";

const db = {
  uri: process.env["ANAMNESIS_TEST_NEO4J_URI"] ?? "",
  user: process.env["ANAMNESIS_TEST_NEO4J_USER"] ?? "neo4j",
  password: process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"] ?? "",
};
if (!db.uri || !db.password) throw new Error("ANAMNESIS_TEST_NEO4J_URI and ANAMNESIS_TEST_NEO4J_PASSWORD are required");
const lineage = {
  episode_id: "018f0d8d-7b6a-7cc0-8b42-000000000001",
  lineage_mode: "direct",
  parent_recall_ids: [],
  context_digests: [],
  root_episode_ids: ["018f0d8d-7b6a-7cc0-8b42-000000000001"],
  echo_depth: 0,
  complete: true,
};
let driver: Driver;
const migrate = (options: Parameters<typeof main>[0] = {}) => main({ ...db, ...options });
const legacyConstraints = ["echo_lineage_episode", "origin_head_key", "materialization_operation_id"] as const;
const newIndexes = ["episode_origin_head", "entity_witness"] as const;

async function count(label: string): Promise<number> {
  const result = await driver.executeQuery(`MATCH (n:${label}) RETURN count(n) AS count`);
  return result.records[0]?.get("count") ?? 0;
}
async function graphSnapshot(): Promise<unknown[]> {
  const result = await driver.executeQuery(`
    MATCH (n)
    OPTIONAL MATCH (n)-[r]->(target)
    WITH n,r,target ORDER BY elementId(r)
    WITH n,collect(CASE WHEN r IS NULL THEN null ELSE {
      id:elementId(r),type:type(r),target:elementId(target),props:properties(r)
    } END) AS raw_edges
    RETURN elementId(n) AS id,labels(n) AS labels,properties(n) AS props,
      [edge IN raw_edges WHERE edge IS NOT NULL] AS edges
    ORDER BY id`);
  return result.records.map(record => record.toObject());
}
async function seed(legacy = true): Promise<void> {
  await driver.executeQuery("MATCH (n) DETACH DELETE n");
  await driver.executeQuery("CREATE (:ExtractionGeneration {id:'018f0d8d-7b6a-7cc0-8b42-000000000011'})");
  await driver.executeQuery("CREATE (:ExtractionCoverage {generation_id:'018f0d8d-7b6a-7cc0-8b42-000000000011',partition:'episodes',covered_ingest_seq:10})");
  await driver.executeQuery("CREATE (:ExtractionCoverage {generation_id:'018f0d8d-7b6a-7cc0-8b42-000000000011',partition:'active_extraction',covered_ingest_seq:10})");
  await driver.executeQuery("CREATE (:Episode {id:$id,ingest_seq:1})", { id: lineage.episode_id });
  await driver.executeQuery("CREATE (:Entity {id:'018f0d8d-7b6a-7cc0-8b42-000000000002'})");
  await driver.executeQuery("CREATE (:Fact {id:'018f0d8d-7b6a-7cc0-8b42-000000000003',generation:'018f0d8d-7b6a-7cc0-8b42-000000000011',meaning_digest:'digest',primary_episode_id:$id})", { id: lineage.episode_id });
  if (!legacy) return;
  await driver.executeQuery("CREATE (:EchoLineage $props)", { props: { ...lineage, body: canonicalExtractionBody(lineage), digest: extractionBodyDigest(lineage) } });
  await driver.executeQuery("CREATE (:EntityWitness {entity_id:'018f0d8d-7b6a-7cc0-8b42-000000000002',generation:'018f0d8d-7b6a-7cc0-8b42-000000000011',policy_revision:1,state:'COMPLETE'})");
  await driver.executeQuery("CREATE (:MaterializationOperation {id:'018f0d8d-7b6a-7cc0-8b42-000000000010',digest:$digest,result:$result,occurrence_key:$occurrence,source_episode_id:$id,generation:$generation})",
    { id: lineage.episode_id, digest: "a".repeat(64), result: canonicalExtractionBody({ created: false, facts: 0, refused: [] }),
      occurrence: "legacy-custody", generation: "018f0d8d-7b6a-7cc0-8b42-000000000011" });
  await driver.executeQuery("CREATE (:OriginHead {origin_key:'origin'})");
  await driver.executeQuery("CREATE (:FactRelationVerdict {occurrence_key:'verdict'})");
}

beforeEach(async () => {
  driver = neo4j.driver(db.uri, neo4j.auth.basic(db.user, db.password), { disableLosslessIntegers: true });
  await seed();
  await driver.executeQuery("CREATE CONSTRAINT echo_lineage_episode IF NOT EXISTS FOR (n:EchoLineage) REQUIRE n.episode_id IS UNIQUE");
  await driver.executeQuery("CREATE CONSTRAINT origin_head_key IF NOT EXISTS FOR (n:OriginHead) REQUIRE n.origin_key IS UNIQUE");
  await driver.executeQuery("CREATE CONSTRAINT materialization_operation_id IF NOT EXISTS FOR (n:MaterializationOperation) REQUIRE n.id IS UNIQUE");
  await driver.executeQuery("CREATE INDEX relation_input_occurrence IF NOT EXISTS FOR (n:FactRelationInput) ON (n.occurrence_key)");
}, 300000);
afterEach(async () => {
  await driver.executeQuery("MATCH (n) DETACH DELETE n");
  for (const name of legacyConstraints) await driver.executeQuery(`DROP CONSTRAINT ${name} IF EXISTS`);
  for (const name of [...newIndexes, "relation_input_occurrence"]) await driver.executeQuery(`DROP INDEX ${name} IF EXISTS`);
  await driver.executeQuery("DROP CONSTRAINT fact_identity IF EXISTS");
  await driver.close();
}, 300000);

describe.serial("materialization ledger migration", () => {
  test.serial("dry-run reports gates and changes nothing", async () => {
    const lines: string[] = [];
    const result = await migrate({ args: ["--dry-run"], output: line => lines.push(line) });
    expect(result.dry_run).toBe(true);
    expect(await count("EchoLineage")).toBe(1);
    expect(await count("EntityWitness")).toBe(1);
    const steps = lines.map(line => JSON.parse(line));
    expect(steps.map(step => step.step)).toEqual(["count_legacy", "copy_lineage", "copy_witness", "check_coverage", "check_inflight", "fact_duplicates", "drop_schema", "create_schema", "delete_legacy_ledgers", "verify"]);
    expect(steps[0].constraints.sort()).toEqual([...legacyConstraints].sort());
    expect(steps[0].indexes).toContain("relation_input_occurrence");
    expect(steps[3].split).toEqual([]);
    expect(steps[3].uncovered).toBe(0);
    expect(steps[4].count).toBe(0);
    expect(steps[4].malformed_seals).toBe(0);
    expect(steps[4].terminal_zero_candidate).toBe(0);
    expect(steps[4].sealed_exhausted).toBe(0);
    expect(steps[4].pending).toBe(0);
    expect(steps[5].count).toBe(0);
    expect(steps.slice(6).every(step => step.dry_run === true)).toBe(true);
    const episode = await driver.executeQuery("MATCH (e:Episode {id:$id}) RETURN e.lineage_mode AS mode", { id: lineage.episode_id });
    expect(episode.records[0]?.get("mode")).toBeNull();
    const schema = await driver.executeQuery("SHOW CONSTRAINTS YIELD name RETURN name");
    for (const name of legacyConstraints) expect(schema.records.map(row => row.get("name"))).toContain(name);
  }, 300000);

  test.serial("refuses when an EchoLineage Episode is missing", async () => {
    await driver.executeQuery("MATCH (e:Episode) DETACH DELETE e");
    await expect(migrate({ output: () => undefined })).rejects.toThrow("copy_mismatch");
    expect(await count("EchoLineage")).toBe(1);
  }, 300000);

  test.serial("refuses an in-flight operation", async () => {
    await driver.executeQuery("MATCH (o:MaterializationOperation) SET o.result=null,o.occurrence_key=null");
    await expect(migrate({ output: () => undefined })).rejects.toThrow("In-flight");
    expect(await count("MaterializationOperation")).toBe(1);
    expect(await count("EchoLineage")).toBe(1);
  }, 300000);

  test.serial("allows zero-candidate inputs without a verdict", async () => {
    await driver.executeQuery("CREATE (:FactRelationInput {occurrence_key:'zero',candidates:0,generation:'018f0d8d-7b6a-7cc0-8b42-000000000011',source_episode_id:$source})", { source: lineage.episode_id });
    const lines: string[] = [];
    await migrate({ args: ["--dry-run"], output: line => lines.push(line) });
    const gate = lines.map(line => JSON.parse(line)).find(step => step.step === "check_inflight");
    expect(gate).toMatchObject({ count: 0, terminal_zero_candidate: 1, sealed_exhausted: 0, pending: 0 });
    expect(await count("FactRelationInput")).toBe(1);
  }, 300000);

  test.serial("allows sealed exhausted inputs as terminal", async () => {
    const generation = "018f0d8d-7b6a-7cc0-8b42-000000000011";
    const occurrence = extractionBodyDigest([generation, lineage.episode_id]);
    const relationOccurrence = "a".repeat(64);
    const result = { created: false, facts: 0, refused: [], omitted: "relation_judge_exhausted", failures: 4, occurrences: [relationOccurrence] };
    await driver.executeQuery("CREATE (:FactRelationInput {occurrence_key:$relationOccurrence,candidates:1,generation:$generation,source_episode_id:$source})",
      { generation, relationOccurrence, source: lineage.episode_id });
    await driver.executeQuery("CREATE (:MaterializationOperation {id:$id,digest:$digest,occurrence_key:$occurrence,result:$result,generation:$generation,source_episode_id:$source})",
      { id: "018f0d8d-7b6a-7cc0-8b42-000000000012", digest: "b".repeat(64), occurrence,
        result: canonicalExtractionBody(result), generation, source: lineage.episode_id });
    const lines: string[] = [];
    await migrate({ args: ["--dry-run"], output: line => lines.push(line) });
    const gate = lines.map(line => JSON.parse(line)).find(step => step.step === "check_inflight");
    expect(gate).toMatchObject({ count: 0, terminal_zero_candidate: 0, sealed_exhausted: 1, pending: 0 });
    expect(await count("MaterializationOperation")).toBe(2);
  }, 300000);

  test.serial("refuses sealed custody whose source is beyond the coverage cursor without graph changes", async () => {
    const generation = "018f0d8d-7b6a-7cc0-8b42-000000000011";
    const occurrence = extractionBodyDigest([generation, lineage.episode_id]);
    const result = { created: false, facts: 0, refused: [], omitted: "relation_judge_exhausted", failures: 4, occurrences: ["a".repeat(64)] };
    await driver.executeQuery("CREATE (:FactRelationInput {occurrence_key:$relationOccurrence,candidates:1,generation:$generation,source_episode_id:$source})",
      { generation, relationOccurrence: "a".repeat(64), source: lineage.episode_id });
    await driver.executeQuery("CREATE (:MaterializationOperation {id:$id,digest:$digest,occurrence_key:$occurrence,result:$result,generation:$generation,source_episode_id:$source})",
      { id: "018f0d8d-7b6a-7cc0-8b42-000000000012", digest: "b".repeat(64), occurrence, result: canonicalExtractionBody(result), generation, source: lineage.episode_id });
    // The engine keeps this source's journal entry (source_ingest_seq > covered_ingest_seq), so deleting its custody would replay the judge.
    await driver.executeQuery("MATCH (e:Episode {id:$id}) SET e.ingest_seq=11", { id: lineage.episode_id });
    const before = await graphSnapshot();
    const lines: string[] = [];
    await expect(migrate({ output: line => lines.push(line) })).rejects.toThrow("coverage_gap");
    const steps = lines.map(line => JSON.parse(line));
    // The seeded legacy custody and the sealed operation share one source, so each label reports that source once.
    expect(steps.find(step => step.step === "check_coverage")).toMatchObject({ split: [], uncovered: 2,
      uncovered_sources: [`MaterializationOperation:${generation}:${lineage.episode_id}:seq=11:covered=10`, `FactRelationInput:${generation}:${lineage.episode_id}:seq=11:covered=10`] });
    expect(steps.some(step => step.step === "check_inflight")).toBe(false);
    expect(await graphSnapshot()).toEqual(before);
  }, 300000);

  test.serial("refuses custody whose source Episode is missing", async () => {
    await driver.executeQuery("CREATE (:FactRelationInput {occurrence_key:'orphan',candidates:0,generation:'018f0d8d-7b6a-7cc0-8b42-000000000011',source_episode_id:'018f0d8d-7b6a-7cc0-8b42-0000000000ff'})");
    const lines: string[] = [];
    await expect(migrate({ args: ["--dry-run"], output: line => lines.push(line) })).rejects.toThrow("coverage_gap");
    expect(lines.map(line => JSON.parse(line)).find(step => step.step === "check_coverage"))
      .toMatchObject({ uncovered: 1, uncovered_sources: ["FactRelationInput:018f0d8d-7b6a-7cc0-8b42-000000000011:018f0d8d-7b6a-7cc0-8b42-0000000000ff:seq=null:covered=10"] });
    expect(await count("FactRelationInput")).toBe(1);
  }, 300000);

  test.serial("refuses a malformed seal instead of skipping it", async () => {
    const generation = "018f0d8d-7b6a-7cc0-8b42-000000000011";
    await driver.executeQuery("CREATE (:FactRelationVerdict {occurrence_key:'c-verdict'})");
    await driver.executeQuery("CREATE (:FactRelationInput {occurrence_key:'c-verdict',candidates:1,generation:$generation,source_episode_id:$source})", { generation, source: lineage.episode_id });
    await driver.executeQuery("CREATE (:MaterializationOperation {id:$id,digest:$digest,occurrence_key:$occurrence,result:$result,generation:$generation,source_episode_id:$source})",
      { id: "018f0d8d-7b6a-7cc0-8b42-000000000013", digest: "c".repeat(64), occurrence: extractionBodyDigest([generation, lineage.episode_id]),
        result: '{"omitted":"relation_judge_exhausted",', generation, source: lineage.episode_id });
    const before = await graphSnapshot();
    const lines: string[] = [];
    await expect(migrate({ output: line => lines.push(line) })).rejects.toThrow("malformed_seal");
    expect(lines.map(line => JSON.parse(line)).find(step => step.step === "check_inflight"))
      .toMatchObject({ count: 0, pending: 0, sealed_exhausted: 0, malformed_seals: 1, malformed: [`018f0d8d-7b6a-7cc0-8b42-000000000013:${generation}:${lineage.episode_id}`] });
    expect(await graphSnapshot()).toEqual(before);
  }, 300000);

  test.serial("refuses a candidate-bearing input without verdict or seal", async () => {
    await driver.executeQuery("CREATE (:FactRelationInput {occurrence_key:'pending',candidates:1,generation:'018f0d8d-7b6a-7cc0-8b42-000000000011',source_episode_id:$source})",
      { source: lineage.episode_id });
    const lines: string[] = [];
    await expect(migrate({ output: line => lines.push(line) })).rejects.toThrow("In-flight");
    expect(lines.map(line => JSON.parse(line)).find(step => step.step === "check_inflight"))
      .toMatchObject({ count: 1, pending: 1, sealed_exhausted: 0 });
    expect(await count("FactRelationInput")).toBe(1);
    expect(await count("EchoLineage")).toBe(1);
  }, 300000);

  test.serial("refuses corrupt lineage on a later validation page without graph changes", async () => {
    const entries = Array.from({ length: 1000 }, (_, offset) => {
      const id = `018f0d8d-7b6a-7cc0-8b42-${String(offset + 2).padStart(12, "0")}`;
      const body = { ...lineage, episode_id: id, root_episode_ids: [id] };
      return { id, props: { ...body, body: canonicalExtractionBody(body), digest: offset === 999 ? "f".repeat(64) : extractionBodyDigest(body) } };
    });
    await driver.executeQuery("UNWIND $entries AS entry CREATE (e:Episode {id:entry.id,ingest_seq:2}) CREATE (l:EchoLineage) SET l = entry.props", { entries });
    const before = await graphSnapshot();
    // 1001 rows at --batch 100 puts the corrupt row (highest episode_id) on the eleventh keyset page.
    await expect(migrate({ args: ["--dry-run", "--batch", "100"], output: () => undefined })).rejects.toThrow("copy_mismatch");
    expect(await graphSnapshot()).toEqual(before);
  }, 300000);

  test.serial("refuses Episode and ledger lineage digest disagreement without graph changes", async () => {
    await driver.executeQuery("MATCH (e:Episode {id:$id}) SET e.lineage_digest=$digest", { id: lineage.episode_id, digest: "f".repeat(64) });
    const before = await graphSnapshot();
    await expect(migrate({ args: ["--dry-run"], output: () => undefined })).rejects.toThrow("copy_mismatch");
    expect(await graphSnapshot()).toEqual(before);
  }, 300000);

  test.serial("copies and removes all six ledgers on live run", async () => {
    const lines: string[] = [];
    const result = await migrate({ args: ["--batch", "1"], output: line => lines.push(line) });
    expect(result.counts_after).toEqual({
      MaterializationOperation: 0, OriginHead: 0, EchoLineage: 0,
      FactRelationInput: 0, FactRelationVerdict: 0, EntityWitness: 0,
    });
    const episode = await driver.executeQuery("MATCH (e:Episode {id:$id}) RETURN e.lineage_mode AS mode,e.parent_recall_ids AS parents,e.context_digests AS digests,e.root_episode_ids AS roots,e.echo_depth AS depth,e.lineage_complete AS complete", { id: lineage.episode_id });
    expect(episode.records[0]?.get("mode")).toBe("direct");
    expect(episode.records[0]?.get("parents")).toEqual([]);
    expect(episode.records[0]?.get("digests")).toEqual([]);
    expect(episode.records[0]?.get("roots")).toEqual([lineage.episode_id]);
    expect(episode.records[0]?.get("depth")).toBe(0);
    expect(episode.records[0]?.get("complete")).toBe(true);
    const witness = await driver.executeQuery("MATCH (e:Entity) RETURN e.witness_generation AS generation,e.witness_policy_revision AS revision");
    expect(witness.records[0]?.get("generation")).toBe("018f0d8d-7b6a-7cc0-8b42-000000000011");
    expect(witness.records[0]?.get("revision")).toBe(1);
    const constraints = await driver.executeQuery("SHOW CONSTRAINTS YIELD name RETURN name");
    expect(constraints.records.map(row => row.get("name"))).toContain("fact_identity");
    for (const name of legacyConstraints) expect(constraints.records.map(row => row.get("name"))).not.toContain(name);
    const indexes = await driver.executeQuery("SHOW INDEXES YIELD name RETURN name");
    for (const name of newIndexes) expect(indexes.records.map(row => row.get("name"))).toContain(name);
    expect(indexes.records.map(row => row.get("name"))).not.toContain("relation_input_occurrence");
    expect(JSON.parse(lines[lines.length - 1] ?? "{}").step).toBe("verify");
    expect(await count("Fact")).toBe(1);
  }, 300000);
});
