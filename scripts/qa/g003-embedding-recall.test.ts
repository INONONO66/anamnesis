import { expect, test } from "bun:test";
import { startProcess } from "./runtime-scenarios.ts";
import { Engine } from "../../packages/core/src/engine.ts";
import { EmbeddingProfile, embeddingProfileId, type EmbeddingProvider } from "../../packages/core/src/embedding.ts";
import { mkdtemp, rm, readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { pathToFileURL } from "node:url";
import neo4j from "neo4j-driver";

test("configured provider, recovery, hybrid recall and exact budgets on real Node/UDS/Neo4j", async () => {
  const root = await mkdtemp("/tmp/g003-receipt-contract-");
  try {
    const bundle = `${root}/receipt-digest.mjs`;
    const build = await startProcess(process.execPath, ["build", "packages/core/src/receipt-digest.ts", "--target=node", "--outfile", bundle], { deadlineMs: 30000 }).done;
    expect(build.timedOut).toBe(false); expect(build.code).toBe(0);
    const hash = async () => createHash("sha256").update(await readFile(bundle)).digest("hex");
    const before = await hash();
    const child = startProcess("node", ["app/anamnesis/embedding-recall.surface.mjs"], {
      deadlineMs: 120000, onOutput: text => process.stdout.write(text),
      env: { ...process.env, RECEIPT_DIGEST_BUNDLE: pathToFileURL(bundle).href },
    });
    const result = await child.done;
    expect(await hash()).toBe(before);
    expect(result.timedOut).toBe(false); expect(result.code).toBe(0);
  } finally { await rm(root, { recursive: true }); }
}, 130000);

test("model-scoped vectors and an event-gated policy race preserve server-derived authority", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"]!, password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"]!;
  if (!uri || !password) throw new Error("isolated graph credentials required");
  const root = await mkdtemp("/tmp/g003-models-");
  const profile = EmbeddingProfile.parse({ model: "unit-fixture", model_incarnation: "c".repeat(64), dimensions: 2,
    query_prefix: "", document_prefix: "", max_input_bytes: 65536, norm: "unit_l2", norm_tolerance: 0.001 });
  let announce: (() => void) | undefined, release: (() => void) | undefined;
  let gate: Promise<void> | undefined;
  const provider: EmbeddingProvider = { profile, async embed() { announce?.(); await gate; return [1, 0]; } };
  const context = { principal: "installation", commit_mode: "receipt" } as const;
  const first = new Engine({ uri, password, objectsRoot: root, embeddingProvider: provider });
  const otherProfile = { ...profile, model_incarnation: "d".repeat(64) };
  const second = new Engine({ uri, password, objectsRoot: root, embeddingProvider: { profile: otherProfile, async embed() { return [0, 1]; } } });
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  try {
    await first.init();
    const id = (await first.remember({ content: "gated model fixture", mass: 0,
      time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: root, session: "s", actor: "a", record: "one" } })).id;
    const operation_id = Bun.randomUUIDv7();
    const attempt = await first.recoverEmbedding({ episode_id: id, operation_id }, context);
    expect(attempt.state).toBe("succeeded");
    await second.init();
    await expect(second.recoverEmbedding({ episode_id: id, operation_id }, context)).rejects.toThrow("idempotency_conflict");
    expect((await second.recallHybrid({ query: "" }, context)).results).toEqual([]);
    expect((await second.recoverEmbedding({ episode_id: id, operation_id: Bun.randomUUIDv7() }, context)).profile_id).toBe(embeddingProfileId(otherProfile));
    expect((await second.recallHybrid({ query: "" }, context)).results.map(item => item.id)).toEqual([id]);
    const vectors = await driver.executeQuery("MATCH (v:EmbeddingVector {episode_id:$id}) RETURN v.profile_id AS profile,v.input_revision AS revision,v.vector AS vector ORDER BY profile", { id });
    expect(vectors.records).toHaveLength(2);
    expect(vectors.records.map(row => row.get("revision"))).toEqual([attempt.input_revision, attempt.input_revision]);
    const started = new Promise<void>(resolve => { announce = resolve; });
    gate = new Promise<void>(resolve => { release = resolve; });
    const recalling = first.recallHybrid({ query: "", limit: 1 }, context);
    await Promise.race([started, new Promise<never>((_, reject) => { AbortSignal.timeout(10000).addEventListener("abort", () => reject(new Error("provider event deadline")), { once: true }); })]);
    const policy = { policy_id: Bun.randomUUIDv7(), selector: { episode_id: id }, scope: "content" as const };
    await first.setPolicy(policy, context);
    release!();
    const result = await recalling;
    expect(result.results).toEqual([]);
    expect((await first.getReceipt(result.recall_id))!.primaries).toEqual([]);
    await first.revokePolicy({ policy_id: policy.policy_id }, context);
    const allowed = await first.recallHybrid({ query: "", limit: 1 }, context);
    expect(allowed.results[0]!.sources).toEqual([id]);
    await first.setPolicy({ ...policy, policy_id: Bun.randomUUIDv7() }, context);
    const feedback = { operation_id: Bun.randomUUIDv7(), recall_id: allowed.recall_id, adopted: [id], reward: 1 };
    await expect(first.commitReceipt(feedback, context)).rejects.toThrow("policy_denied");
    const hits = await driver.executeQuery("MATCH (h:Hit {namespace:$id}) RETURN count(h) AS n", { id: allowed.recall_id });
    expect(hits.records[0]!.get("n")).toBe(0);
  } finally { release?.(); await first.close(); await second.close(); await driver.close(); await rm(root, { recursive: true }); }
}, 60000);
