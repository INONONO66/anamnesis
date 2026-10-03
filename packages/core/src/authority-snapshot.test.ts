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
/** Episodes, links and invalidation rows each exceed one 5,000-row page, so every collection's keyset query turns a page. */
const linked = 6_002;
const time = "2026-09-01T00:00:00.000Z";
// Episode order deliberately disagrees with hash order for the first two.
const episodeDigest = (n: number) => n === 1 ? "f".repeat(64) : n === 2 ? "0".repeat(64) : createHash("sha256").update(`episode-${n}`).digest("hex");
const extra = Array.from({ length: linked - 2 }, (_, index) => index + 3);
const link = (k: number) => ({ id: id(200_000 + k), from: id(k), to: id(k - 1) });
const invalidation = (k: number) => ({ id: id(300_000 + k), from: id(k), to: id(k - 1) });
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
    await run(`UNWIND $rows AS row MATCH (e:Element {id:row.id}) SET e:Episode,e.digest=row.digest`,
      { rows: Array.from({ length: linked }, (_, index) => ({ id: id(index + 1), digest: episodeDigest(index + 1) })) });
    await run(`MATCH (a:AuthoritySnapshotFixture {id:$a}),(b:AuthoritySnapshotFixture {id:$b})
      CREATE (a)-[:NEXT_EPISODE {id:$later}]->(b),
        (b)-[:DERIVED_FROM {id:$earlier}]->(a),
        (b)-[:INVALIDATES {id:$invalidLater,target_id:$a,effective_time_utc:$time}]->(a),
        (a)-[:INVALIDATES {id:$invalidEarlier,target_id:$b,effective_time_utc:$time}]->(b)`, {
      a: id(1), b: id(2), later: id(200_002), earlier: id(200_001),
      invalidLater: id(300_002), invalidEarlier: id(300_001), time,
    });
    // Two roles share the link page so the role UNION pages as one ordered stream.
    await run(`UNWIND $rows AS row MATCH (a:Element {id:row.from}),(b:Element {id:row.to}) CREATE (a)-[:DERIVED_FROM {id:row.id}]->(b)`, { rows: extra.filter(k => k % 2).map(link) });
    await run(`UNWIND $rows AS row MATCH (a:Element {id:row.from}),(b:Element {id:row.to}) CREATE (a)-[:MENTIONS {id:row.id}]->(b)`, { rows: extra.filter(k => !(k % 2)).map(link) });
    await run(`UNWIND $rows AS row MATCH (a:Element {id:row.from}),(b:Element {id:row.to})
      CREATE (a)-[:INVALIDATES {id:row.id,target_id:row.to,effective_time_utc:$time}]->(b)`, { rows: extra.map(invalidation), time });

    const first = await engine.store.authoritySnapshot(context);
    const second = await engine.store.authoritySnapshot(context);
    expect(second).toEqual(first);
    expect(first.retained_generations).toEqual([]);
    expect(first.members).toEqual(digest(Array.from({ length: size }, (_, index) => id(index + 1))));
    expect(first.members.count).toBe(size);
    expect(first.physical_links).toEqual(digest([
      { id: id(200_001), from: id(2), to: id(1), role: "DERIVED_FROM" },
      { id: id(200_002), from: id(1), to: id(2), role: "ConductingArc" },
      ...extra.map(k => ({ ...link(k), role: k % 2 ? "DERIVED_FROM" : "ConductingArc" })),
    ]));
    expect(first.physical_links.count).toBe(linked);
    const evidence = (row: { id: string; from: string; to: string }, source_hash: string) => ({ id: row.id, source_hash, outcome_hash: extractionBodyDigest({
      id: row.id, from: row.from, to: row.to, target_id: row.to, effective_time_utc: time, generation: null,
    }) });
    expect(first.invalidation_evidence).toEqual(digest([
      evidence({ id: id(300_001), from: id(1), to: id(2) }, "f".repeat(64)),
      evidence({ id: id(300_002), from: id(2), to: id(1) }, "0".repeat(64)),
      ...extra.map(k => evidence(invalidation(k), episodeDigest(k))),
    ]));
    expect(first.invalidation_evidence.count).toBe(linked);
    expect(first.source_hashes).toEqual(digest(Array.from({ length: linked }, (_, index) => episodeDigest(index + 1))));
    expect(first.source_hashes.count).toBe(linked);

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
