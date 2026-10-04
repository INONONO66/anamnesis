import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j, { type Driver } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import type { RpcEmbeddingAttempt } from "../../packages/protocol/src/rpc.ts";
import { main } from "../migrate-embedding-ledgers.ts";

const TEST_DB = {
  uri: process.env["ANAMNESIS_TEST_NEO4J_URI"] ?? "",
  user: process.env["ANAMNESIS_TEST_NEO4J_USER"] ?? "neo4j",
  password: process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"] ?? "",
};
if (!TEST_DB.uri || !TEST_DB.password) throw new Error("ANAMNESIS_TEST_NEO4J_URI and ANAMNESIS_TEST_NEO4J_PASSWORD are required");

const PROFILE = "a".repeat(64);
const REVISION = "b".repeat(64);
const INCARNATION = "c".repeat(64);
let driver: Driver;
let runtimeRoot: string;

function attempt(episodeId: string, createdAt: number, state: "quarantined" | "succeeded"): RpcEmbeddingAttempt {
  return {
    operation_id: uuidv7(),
    episode_id: episodeId,
    profile_id: PROFILE,
    model: "test-embedder",
    model_incarnation: INCARNATION,
    dimensions: 3,
    input_revision: REVISION,
    input_digest: REVISION,
    created_at: createdAt,
    completed_at: createdAt + 1,
    state,
    reason: state === "quarantined" ? "provider_rejected" : null,
    detail: null,
  };
}

async function count(label: "EmbeddingAttempt" | "Outbox"): Promise<number> {
  const result = await driver.executeQuery(`MATCH (n:${label}) RETURN count(n) AS count`);
  return result.records[0]?.get("count") ?? 0;
}

async function schemaNames(command: "CONSTRAINTS" | "INDEXES"): Promise<string[]> {
  const session = driver.session();
  try {
    const result = await session.run<{ name: string }>(`SHOW ${command} YIELD name RETURN name`);
    return result.records.map((record) => record.get("name"));
  } finally {
    await session.close();
  }
}

beforeEach(async () => {
  driver = neo4j.driver(TEST_DB.uri, neo4j.auth.basic(TEST_DB.user, TEST_DB.password), { disableLosslessIntegers: true });
  runtimeRoot = await mkdtemp(join(tmpdir(), "anamnesis-embedding-ledger-"));
  await driver.executeQuery("MATCH (n) DETACH DELETE n");
  await driver.executeQuery("DROP CONSTRAINT embedding_attempt_id IF EXISTS");
  await driver.executeQuery("DROP INDEX outbox_pending IF EXISTS");
  await driver.executeQuery("CREATE CONSTRAINT embedding_attempt_id IF NOT EXISTS FOR (a:EmbeddingAttempt) REQUIRE a.operation_id IS UNIQUE");
  await driver.executeQuery("CREATE INDEX outbox_pending IF NOT EXISTS FOR (o:Outbox) ON (o.processed_at)");
});

afterEach(async () => {
  await driver.executeQuery("MATCH (n) DETACH DELETE n");
  await driver.close();
  await rm(runtimeRoot, { recursive: true });
});

describe.serial("embedding ledger migration", () => {
  test.serial("seeds only latest quarantines without vectors then removes legacy ledgers", async () => {
    const episodes = [uuidv7(), uuidv7(), uuidv7()];
    const [withVector, quarantined, succeeded] = episodes;
    if (!withVector || !quarantined || !succeeded) throw new Error("episode fixture setup failed");
    const skipped = attempt(withVector, 100, "quarantined");
    const earlier = attempt(quarantined, 200, "quarantined");
    const latest = attempt(quarantined, 300, "quarantined");
    const completed = attempt(succeeded, 400, "succeeded");
    // Distinct per Episode: the harness database keeps the `episode_revision` uniqueness constraint.
    await driver.executeQuery(`
      UNWIND $episodes AS episode
      CREATE (:Element:Episode {
        id: episode.id,
        revision_key: episode.revision,
        digest: episode.revision
      })
    `, { episodes: episodes.map((id, index) => ({ id, revision: REVISION.slice(1) + String(index) })) });
    await driver.executeQuery("CREATE (:EmbeddingVector { episode_id: $episodeId, profile_id: $profileId })", {
      episodeId: withVector,
      profileId: PROFILE,
    });
    await driver.executeQuery(`
      UNWIND $attempts AS attempt
      CREATE (:EmbeddingAttempt {
        operation_id: attempt.operation_id,
        episode_id: attempt.episode_id,
        profile_id: attempt.profile_id,
        state: attempt.state,
        reason: attempt.reason,
        body: attempt.body
      })
    `, {
      attempts: [skipped, earlier, latest, completed].map((value) => ({
        operation_id: value.operation_id,
        episode_id: value.episode_id,
        profile_id: value.profile_id,
        state: value.state,
        reason: value.reason,
        body: JSON.stringify(value),
      })),
    });
    await driver.executeQuery("UNWIND [1, 2] AS id CREATE (:Outbox { id: id, processed_at: 1 })");

    const lines: string[] = [];
    const result = await main({
      args: ["--runtime-root", runtimeRoot, "--batch", "2"],
      ...TEST_DB,
      output: (line) => lines.push(line),
    });

    const ledgerPath = join(runtimeRoot, "embedding-state.json");
    const firstBytes = await readFile(ledgerPath, "utf8");
    const ledger = JSON.parse(firstBytes);
    expect(ledger.version).toBe(1);
    expect(Object.keys(ledger.episodes)).toEqual([quarantined]);
    expect(ledger.episodes[quarantined]?.attempts[0]?.operation_id).toBe(latest.operation_id);
    expect(result).toMatchObject({ seeded: 1, skippedWithVector: 1, embeddingAttempts: 0, outbox: 0 });
    expect(await count("EmbeddingAttempt")).toBe(0);
    expect(await count("Outbox")).toBe(0);
    expect(await schemaNames("CONSTRAINTS")).not.toContain("embedding_attempt_id");
    expect(await schemaNames("INDEXES")).not.toContain("outbox_pending");
    expect(lines.map((line) => JSON.parse(line).step)).toEqual([
      "inspect",
      "seed_ledger",
      "drop_schema",
      "delete_legacy_ledgers",
      "final_counts",
    ]);

    const second = await main({
      args: ["--runtime-root", runtimeRoot],
      ...TEST_DB,
      output: () => undefined,
    });

    expect(second).toMatchObject({ seeded: 0, skippedWithVector: 0, alreadyPresent: 0, embeddingAttempts: 0, outbox: 0 });
    expect(await readFile(ledgerPath, "utf8")).toBe(firstBytes);
    expect(await count("EmbeddingAttempt")).toBe(0);
    expect(await count("Outbox")).toBe(0);
  });
});
