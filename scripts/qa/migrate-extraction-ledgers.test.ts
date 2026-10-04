import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import neo4j, { type Driver } from "neo4j-driver";
import { main } from "../migrate-extraction-ledgers.ts";

const TEST_DB = {
  uri: process.env["ANAMNESIS_TEST_NEO4J_URI"] ?? "",
  user: process.env["ANAMNESIS_TEST_NEO4J_USER"] ?? "neo4j",
  password: process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"] ?? "",
};
if (!TEST_DB.uri || !TEST_DB.password) throw new Error("ANAMNESIS_TEST_NEO4J_URI and ANAMNESIS_TEST_NEO4J_PASSWORD are required");

const LABELS = [
  "ModelTask", "ExtractionAttempt", "ExtractionPipeline", "ExtractionJudgeInput", "ExtractionDisposition",
] as const;
const LEGACY_CONSTRAINTS = [
  "extraction_attempt_id", "extraction_disposition_key", "extraction_judge_input_id",
  "extraction_pipeline_id", "extraction_pipeline_judge", "model_task_id", "model_task_work_key",
] as const;
const PRESERVED_CONSTRAINTS = ["extraction_generation_id", "extraction_coverage_key"] as const;
let driver: Driver;

async function count(label: string): Promise<number> {
  const result = await driver.executeQuery(`MATCH (n:${label}) RETURN count(n) AS count`);
  return result.records[0]?.get("count") ?? 0;
}

async function constraints(): Promise<string[]> {
  const result = await driver.executeQuery("SHOW CONSTRAINTS YIELD name RETURN name");
  return result.records.map((record) => record.get("name"));
}

beforeEach(async () => {
  driver = neo4j.driver(TEST_DB.uri, neo4j.auth.basic(TEST_DB.user, TEST_DB.password), { disableLosslessIntegers: true });
  await driver.executeQuery("MATCH (n) DETACH DELETE n");
  await driver.executeQuery("CREATE CONSTRAINT model_task_id IF NOT EXISTS FOR (n:ModelTask) REQUIRE n.id IS UNIQUE");
  await driver.executeQuery("CREATE CONSTRAINT model_task_work_key IF NOT EXISTS FOR (n:ModelTask) REQUIRE n.work_key IS UNIQUE");
  await driver.executeQuery("CREATE CONSTRAINT extraction_attempt_id IF NOT EXISTS FOR (n:ExtractionAttempt) REQUIRE n.id IS UNIQUE");
  await driver.executeQuery("CREATE CONSTRAINT extraction_pipeline_id IF NOT EXISTS FOR (n:ExtractionPipeline) REQUIRE n.id IS UNIQUE");
  await driver.executeQuery("CREATE CONSTRAINT extraction_pipeline_judge IF NOT EXISTS FOR (n:ExtractionPipeline) REQUIRE n.judge_task_id IS UNIQUE");
  await driver.executeQuery("CREATE CONSTRAINT extraction_judge_input_id IF NOT EXISTS FOR (n:ExtractionJudgeInput) REQUIRE n.id IS UNIQUE");
  await driver.executeQuery("CREATE CONSTRAINT extraction_disposition_key IF NOT EXISTS FOR (n:ExtractionDisposition) REQUIRE (n.judge_attempt_id, n.claim_index) IS UNIQUE");
  await driver.executeQuery("CREATE CONSTRAINT extraction_generation_id IF NOT EXISTS FOR (n:ExtractionGeneration) REQUIRE n.id IS UNIQUE");
  await driver.executeQuery("CREATE CONSTRAINT extraction_coverage_key IF NOT EXISTS FOR (n:ExtractionCoverage) REQUIRE n.key IS UNIQUE");
  for (const label of LABELS) {
    await driver.executeQuery(`
      UNWIND [1, 2] AS i
      CREATE (n:${label} {
        id: $label + toString(i),
        work_key: $label + toString(i),
        judge_task_id: $label + toString(i),
        judge_attempt_id: $label + toString(i),
        claim_index: i
      })
    `, { label });
  }
  await driver.executeQuery("CREATE (:ExtractionGeneration {id: 'preserved-generation'})");
  await driver.executeQuery(`UNWIND ['episodes', 'active_extraction'] AS partition
    CREATE (:ExtractionCoverage {key: 'preserved-generation:' + partition, generation_id: 'preserved-generation', partition: partition, covered_ingest_seq: 7})`);
});

afterEach(async () => {
  await driver.executeQuery("MATCH (n) DETACH DELETE n");
  for (const name of LEGACY_CONSTRAINTS) await driver.executeQuery(`DROP CONSTRAINT ${name} IF EXISTS`);
  await driver.close();
});

describe.serial("extraction ledger migration", () => {
  test.serial("reports legacy counts and schema without modifying them on dry-run", async () => {
    const lines: string[] = [];

    const result = await main({ args: ["--dry-run", "--batch", "1"], ...TEST_DB, output: (line) => lines.push(line) });

    expect(result.dry_run).toBe(true);
    expect(result.constraints_dropped).toEqual([]);
    expect(result.counts_before).toEqual({
      ModelTask: 2, ExtractionAttempt: 2, ExtractionPipeline: 2,
      ExtractionJudgeInput: 2, ExtractionDisposition: 2,
    });
    expect(result.counts_after).toEqual(result.counts_before);
    expect(result.coverage).toEqual([{ generation: "preserved-generation", episodes: 7, active_extraction: 7 }]);
    for (const label of LABELS) expect(await count(label)).toBe(2);
    const names = await constraints();
    for (const name of LEGACY_CONSTRAINTS) expect(names).toContain(name);
    const steps = lines.map((line) => JSON.parse(line));
    expect(steps.map((step) => step.step)).toEqual(["count_legacy", "check_coverage", "drop_schema", "delete_legacy_ledgers", "verify"]);
    expect(steps[0].counts).toEqual(result.counts_before);
    expect(steps[1].split).toEqual([]);
    expect(steps[2].constraints.slice().sort()).toEqual([...LEGACY_CONSTRAINTS].sort());
  });

  test.serial("refuses to delete anything while a generation's coverage partitions disagree", async () => {
    // A daemon stopped between the two coverage commits leaves `episodes` ahead of `active_extraction`. The old
    // build reconciles that from the graph ledgers; the no-seed migration must not take them away first.
    await driver.executeQuery("MATCH (c:ExtractionCoverage {key: 'preserved-generation:active_extraction'}) SET c.covered_ingest_seq = 6");
    const lines: string[] = [];

    for (const args of [["--dry-run"], []]) {
      await expect(main({ args, ...TEST_DB, output: (line) => lines.push(line) })).rejects.toThrow("Extraction coverage partitions disagree");
    }

    for (const label of LABELS) expect(await count(label)).toBe(2);
    const names = await constraints();
    for (const name of LEGACY_CONSTRAINTS) expect(names).toContain(name);
    const steps = lines.map((line) => JSON.parse(line));
    expect(steps.map((step) => step.step)).toEqual(["count_legacy", "check_coverage", "count_legacy", "check_coverage"]);
    expect(steps[1].split).toEqual([{ generation: "preserved-generation", episodes: 7, active_extraction: 6 }]);
  });

  test.serial("deletes only legacy nodes and constraints on live run", async () => {
    const lines: string[] = [];

    const result = await main({ args: ["--batch", "1"], ...TEST_DB, output: (line) => lines.push(line) });

    expect(result.dry_run).toBe(false);
    expect(result.constraints_dropped.slice().sort()).toEqual([...LEGACY_CONSTRAINTS].sort());
    expect(result.counts_after).toEqual({
      ModelTask: 0, ExtractionAttempt: 0, ExtractionPipeline: 0,
      ExtractionJudgeInput: 0, ExtractionDisposition: 0,
    });
    for (const label of LABELS) expect(await count(label)).toBe(0);
    const names = await constraints();
    for (const name of LEGACY_CONSTRAINTS) expect(names).not.toContain(name);
    for (const name of PRESERVED_CONSTRAINTS) expect(names).toContain(name);
    expect(await count("ExtractionGeneration")).toBe(1);
    expect(await count("ExtractionCoverage")).toBe(2);
    expect(lines.map((line) => JSON.parse(line).step)).toEqual([
      "count_legacy", "check_coverage", "drop_schema", ...LABELS.map(() => "delete_legacy_ledgers"), "verify",
    ]);
  });
});
