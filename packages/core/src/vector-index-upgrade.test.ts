import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { Engine } from "./engine.ts";
import { EmbeddingProfile, embeddingProfileId, type EmbeddingProvider } from "./embedding.ts";

const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
if (!uri || !password) throw new Error("owned Neo4j runner required");
const root = await mkdtemp(join(tmpdir(), "anamnesis-vector-index-upgrade-"));
const provider: EmbeddingProvider = {
  profile: EmbeddingProfile.parse({ model: "vector-index-upgrade-fixture", model_incarnation: "e".repeat(64), dimensions: 2,
    document_prefix: "", query_prefix: "", max_input_bytes: 8192, norm: "unit_l2", norm_tolerance: 0.001 }),
  async embed() { return [1, 0]; },
};
const id = embeddingProfileId(provider.profile);
const admin = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });

async function vectorIndexes(): Promise<{ name: string; labels: string[] }[]> {
  const result = await admin.executeQuery(`SHOW VECTOR INDEXES YIELD name, labelsOrTypes AS labels RETURN name, labels ORDER BY name`);
  return result.records.map(row => ({ name: row.get("name") as string, labels: row.get("labels") as string[] }));
}

beforeAll(async () => {
  await admin.executeQuery("MATCH (n) DETACH DELETE n");
  for (const index of await vectorIndexes()) await admin.executeQuery(`DROP INDEX \`${index.name}\` IF EXISTS`);
  // The pre-D56 schema: the same (label, property) vector index under the Episode-era name.
  await admin.executeQuery(`CREATE VECTOR INDEX vec_episode_${id} IF NOT EXISTS FOR (v:Embedding_${id}) ON (v.vector)
    OPTIONS {indexConfig: {\`vector.dimensions\`: 2, \`vector.similarity_function\`: 'cosine'}}`);
  await admin.executeQuery("CALL db.awaitIndexes(60)");
});
afterAll(async () => { await admin.close(); await rm(root, { recursive: true }); });

test("init replaces the Episode-era vector index with vec_fact_<profileId> and leaves other profiles alone", async () => {
  const other = `vec_episode_${"f".repeat(64)}`;
  await admin.executeQuery(`CREATE VECTOR INDEX ${other} IF NOT EXISTS FOR (v:Embedding_${"f".repeat(64)}) ON (v.vector)
    OPTIONS {indexConfig: {\`vector.dimensions\`: 2, \`vector.similarity_function\`: 'cosine'}}`);
  expect((await vectorIndexes()).map(index => index.name)).toEqual([`vec_episode_${id}`, other].sort());

  const engine = new Engine({ uri, password, objectsRoot: root, embeddingProvider: provider });
  try {
    await engine.init();
    expect((await vectorIndexes()).map(index => index.name)).toEqual([`vec_fact_${id}`, other].sort());
    await engine.init();
    expect((await vectorIndexes()).map(index => index.name)).toEqual([`vec_fact_${id}`, other].sort());
    const probe = await admin.executeQuery(`CALL db.index.vector.queryNodes($index, 1, $vector) YIELD node RETURN count(node) AS n`,
      { index: `vec_fact_${id}`, vector: [1, 0] });
    expect(probe.records[0]?.get("n")).toBe(0);
  } finally {
    await engine.close();
    await admin.executeQuery(`DROP INDEX ${other} IF EXISTS`);
  }
}, 120000);
