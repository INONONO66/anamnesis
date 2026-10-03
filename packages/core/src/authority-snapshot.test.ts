import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { extractionBodyDigest } from "@anamnesis/protocol";
import { Engine } from "./engine.ts";
import { canonicalJson } from "./store/digest.ts";

const context = { principal: "installation", commit_mode: "receipt" } as const;
const owner = uuidv7();
const id = (n: number) => `${owner.slice(0, 13)}-7${owner.slice(15, 18)}-${owner.slice(19, 23)}-${owner.slice(24, 31)}${n.toString(16).padStart(5, "0")}`;
const size = 100_000;
const batchSize = 5_000;
const digest = (items: Iterable<Parameters<typeof canonicalJson>[0]>) => {
  const hash = createHash("sha256").update("[");
  let count = 0;
  for (const item of items) {
    if (count++) hash.update(",");
    hash.update(canonicalJson(item));
  }
  return { count, sha256: hash.update("]").digest("hex") };
};

test("authority snapshot streams ordered inventory beyond the former cap", async () => {
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI;
  const password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
  if (!uri || !password) throw new Error("owned runner required");
  const root = await mkdtemp(join(tmpdir(), "authority-snapshot-"));
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const engine = new Engine({ uri, password, objectsRoot: root });
  const run = async (cypher: string, params: Record<string, unknown> = {}) => { await driver.executeQuery(cypher, params); };
  try {
    await engine.init();
    await engine.claimWriterEpoch();
    await expect(engine.store.authoritySnapshot(context)).rejects.toMatchObject({
      code: "authority_snapshot_unavailable", message: expect.stringContaining("member identity inventory is incomplete"),
    });

    for (let first = 1; first <= size; first += batchSize) {
      const ids = Array.from({ length: Math.min(batchSize, size - first + 1) }, (_, offset) => id(first + offset));
      await run("UNWIND $ids AS id CREATE (:Element:AuthoritySnapshotFixture {id:id,snapshot_owner:$owner})", { ids, owner });
    }
    // Episode order deliberately disagrees with hash order.
    await run(`MATCH (a:AuthoritySnapshotFixture {id:$a}),(b:AuthoritySnapshotFixture {id:$b})
      SET a:Episode,a.digest=$high,b:Episode,b.digest=$low`, {
      a: id(1), b: id(2), high: "f".repeat(64), low: "0".repeat(64),
    });
    await run(`MATCH (a:AuthoritySnapshotFixture {id:$a}),(b:AuthoritySnapshotFixture {id:$b})
      CREATE (a)-[:NEXT_EPISODE {id:$later}]->(b),
        (b)-[:DERIVED_FROM {id:$earlier}]->(a),
        (b)-[:INVALIDATES {id:$invalidLater,target_id:$a,effective_time_utc:$time}]->(a),
        (a)-[:INVALIDATES {id:$invalidEarlier,target_id:$b,effective_time_utc:$time}]->(b)`, {
      a: id(1), b: id(2), later: id(200_002), earlier: id(200_001),
      invalidLater: id(300_002), invalidEarlier: id(300_001), time: "2026-09-01T00:00:00.000Z",
    });

    const first = await engine.store.authoritySnapshot(context);
    const second = await engine.store.authoritySnapshot(context);
    expect(second).toEqual(first);
    expect(first.retained_generations).toEqual([]);
    expect(first.members).toEqual(digest(Array.from({ length: size }, (_, index) => id(index + 1))));
    expect(first.members.count).toBe(size);
    expect(first.physical_links).toEqual(digest([
      { id: id(200_001), from: id(2), to: id(1), role: "DERIVED_FROM" },
      { id: id(200_002), from: id(1), to: id(2), role: "ConductingArc" },
    ]));
    const time = "2026-09-01T00:00:00.000Z";
    expect(first.invalidation_evidence).toEqual(digest([
      { id: id(300_001), source_hash: "f".repeat(64), outcome_hash: extractionBodyDigest({
        id: id(300_001), from: id(1), to: id(2), target_id: id(2), effective_time_utc: time, generation: null,
      }) },
      { id: id(300_002), source_hash: "0".repeat(64), outcome_hash: extractionBodyDigest({
        id: id(300_002), from: id(2), to: id(1), target_id: id(1), effective_time_utc: time, generation: null,
      }) },
    ]));
    expect(first.source_hashes).toEqual(digest(["f".repeat(64), "0".repeat(64)]));

    await run("MATCH (e:AuthoritySnapshotFixture {id:$old}) SET e.id=$next", { old: id(50_000), next: id(size + 1) });
    const changed = await engine.store.authoritySnapshot(context);
    expect(changed.members).toEqual(digest(Array.from({ length: size }, (_, index) =>
      index === 49_999 ? id(size + 1) : id(index + 1)).sort()));
    expect(changed.members.count).toBe(size);
    expect(changed.members.sha256).not.toBe(first.members.sha256);
    expect(changed.physical_links).toEqual(first.physical_links);
    expect(changed.invalidation_evidence).toEqual(first.invalidation_evidence);
    expect(changed.source_hashes).toEqual(first.source_hashes);
    expect(changed.coverage).toEqual(first.coverage);
    expect(changed.retained_generations).toEqual(first.retained_generations);
    await run("MATCH (m:Meta {key:'meta'}) DELETE m");
    await expect(engine.store.authoritySnapshot(context)).rejects.toMatchObject({ code: "authority_snapshot_unavailable" });
  } finally {
    await engine.close();
    for (let batch = 0; batch < size / batchSize; batch++) {
      await run("MATCH (e:AuthoritySnapshotFixture {snapshot_owner:$owner}) WITH e LIMIT $limit DETACH DELETE e",
        { owner, limit: neo4j.int(batchSize) });
    }
    await driver.close();
    await rm(root, { recursive: true, force: true });
  }
}, 900_000);
