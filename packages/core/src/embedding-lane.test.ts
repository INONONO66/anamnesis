import { afterAll, beforeAll, describe, expect, setDefaultTimeout, test } from "bun:test";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j, { type RecordShape } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";
import { EmbeddingError, EmbeddingProfile, type EmbeddingProvider } from "./embedding.ts";

const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
const user = process.env["ANAMNESIS_TEST_NEO4J_USER"] ?? "neo4j";
const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("ANAMNESIS_TEST_NEO4J_URI and ANAMNESIS_TEST_NEO4J_PASSWORD are required");
setDefaultTimeout(120_000);

const context = { principal: "installation", commit_mode: "receipt" } as const;
const profile = EmbeddingProfile.parse({
  model: "embedding-lane-fixture", model_incarnation: "c".repeat(64), dimensions: 2,
  document_prefix: "", query_prefix: "", max_input_bytes: 8192,
  norm: "unit_l2", norm_tolerance: 0.001,
});
const state = { now: Date.UTC(2030, 0, 1), mode: "ok" as "ok" | "down" | "reject", calls: 0 };
const provider: EmbeddingProvider = {
  profile,
  async embed(text, purpose) {
    expect(purpose).toBe("document");
    state.calls++;
    if (state.mode === "down") throw new EmbeddingError("provider_unavailable", "http 503");
    if (state.mode === "reject" && text.includes("reject-me")) throw new EmbeddingError("provider_rejected", "http 400");
    return [1, 0];
  },
};
const root = await mkdtemp(join(tmpdir(), "anamnesis-embedding-lane-"));
const ledgerPath = join(root, "embedding-state.json");
const engine = new Engine({ uri, user, password, objectsRoot: root, embeddingLedgerPath: ledgerPath,
  embeddingProvider: provider, clock: () => state.now });
const admin = neo4j.driver(uri, neo4j.auth.basic(user, password), { disableLosslessIntegers: true });

async function query<Row extends RecordShape>(cypher: string, params: Record<string, string> = {}): Promise<Row[]> {
  const result = await admin.executeQuery<Row>(cypher, params);
  return result.records.map((row: { toObject(): Row }) => row.toObject());
}

async function ledger(): Promise<{ version: number; episodes: Record<string, {
  profile_id: string; state: string; deferrals: number; retry_after: number | null;
  attempts: { operation_id: string; reason: string | null }[];
}> }> {
  return JSON.parse(await readFile(ledgerPath, "utf8"));
}

async function episode(content: string): Promise<string> {
  return (await engine.remember({
    content, time: { value: "2026-09-01T00:00:00Z", precision: "second" },
    origin: { source: "chat-export", session: "embedding-fixture", actor: "test", record: uuidv7() },
  })).id;
}

beforeAll(async () => { await engine.init(); });
afterAll(async () => {
  await engine.close();
  await admin.close();
  await rm(root, { recursive: true });
});

describe.serial("embedding lane on isolated Neo4j", () => {
  test("C001 a provider 400 on one input is terminal on the first attempt", async () => {
    // Given five Episodes, exactly one of which the provider rejects.
    state.mode = "reject";
    const ids: string[] = [];
    for (let i = 0; i < 5; i++) ids.push(await episode(i === 2 ? "reject-me" : `valid-${i}`));

    // When the worker drains the missing-vector scan to completion.
    for (;;) {
      const result = await engine.drainEmbeddingOutbox(100);
      if ("reason" in result) throw new Error("embedding provider unexpectedly disabled");
      if (result.drained === 0 && result.deferred === 0) break;
    }

    // Then four vectors exist, the one rejected Episode has a single terminal ledger attempt,
    // and neither retired graph label has been created.
    const vectors = await query<{ count: number }>("MATCH (v:EmbeddingVector) RETURN count(v) AS count");
    expect(vectors[0]?.count).toBe(4);
    const entries = (await ledger()).episodes;
    const rejected = ids[2];
    if (!rejected) throw new Error("rejected Episode was not created");
    expect(Object.keys(entries)).toEqual([rejected]);
    expect(entries[rejected]?.state).toBe("quarantined");
    expect(entries[rejected]?.attempts).toHaveLength(1);
    expect(entries[rejected]?.attempts[0]?.reason).toBe("provider_rejected");
    const labels = await query<{ label: string }>("CALL db.labels() YIELD label RETURN label");
    expect(labels.map(row => row.label)).not.toContain("EmbeddingAttempt");
    expect(labels.map(row => row.label)).not.toContain("Outbox");
    expect((await engine.status()).pendingOutbox).toBe(0);

    // Restore an empty ledger for the independent C002 accounting below.
    state.mode = "ok";
    expect(await engine.requeueQuarantinedEmbeddings({ limit: 100 }, context)).toEqual({ requeued: 1 });
    await engine.drainEmbeddingOutbox(100);
    expect(Object.keys((await ledger()).episodes)).toHaveLength(0);
  });

  test("C002 transient failures defer with backoff, exhaust into quarantine, and requeue is idempotent", async () => {
    // Given one unembedded Episode and an unavailable provider.
    state.mode = "down";
    const id = await episode("transient-provider");
    const calls = state.calls;

    // When eight bounded deferrals occur, followed by a ninth transient failure.
    for (let deferrals = 1; deferrals <= 8; deferrals++) {
      const before = state.now;
      await engine.drainEmbeddingOutbox(100);
      const entry = (await ledger()).episodes[id];
      expect(entry?.state).toBe("deferred");
      expect(entry?.deferrals).toBe(deferrals);
      expect(entry?.retry_after).toBe(before + Math.min(30_000 * 2 ** (deferrals - 1), 3_600_000));
      expect(entry?.attempts).toHaveLength(deferrals);
      if (entry?.retry_after === null || entry?.retry_after === undefined) throw new Error("deferral has no retry deadline");
      state.now = entry.retry_after + 1;
    }
    await engine.drainEmbeddingOutbox(100);
    const exhausted = (await ledger()).episodes[id];
    expect(exhausted?.state).toBe("quarantined");
    expect(exhausted?.attempts).toHaveLength(9);
    expect(exhausted?.attempts[8]?.reason).toBe("provider_unavailable_exhausted");
    expect(state.calls - calls).toBe(9);

    // Then requeue clears quarantine once, resets its budget, and a later success
    // writes the vector while deleting its ledger entry.
    expect(await engine.requeueQuarantinedEmbeddings({ limit: 100 }, context)).toEqual({ requeued: 1 });
    expect((await ledger()).episodes[id]).toBeUndefined();
    await engine.drainEmbeddingOutbox(100);
    const retried = (await ledger()).episodes[id];
    expect(retried?.state).toBe("deferred");
    expect(retried?.deferrals).toBe(1);
    state.mode = "ok";
    if (retried?.retry_after === null || retried?.retry_after === undefined) throw new Error("retry has no deadline");
    state.now = retried.retry_after + 1;
    expect(await engine.drainEmbeddingOutbox(100)).toMatchObject({ drained: 1 });
    const vectors = await query<{ operation_id: string }>(
      "MATCH (v:EmbeddingVector {episode_id:$id}) RETURN v.operation_id AS operation_id", { id });
    expect(vectors).toHaveLength(1);
    expect(Object.keys((await ledger()).episodes)).toHaveLength(0);
    expect(await engine.requeueQuarantinedEmbeddings({ limit: 100 }, context)).toEqual({ requeued: 0 });
    const succeeded = vectors[0]?.operation_id;
    if (!succeeded) throw new Error("successful vector has no operation");
    expect(await engine.embeddingStatus(succeeded, context)).toMatchObject({
      state: "succeeded", episode_id: id, operation_id: succeeded,
    });
    const unknown = uuidv7();
    expect(await engine.embeddingStatus(unknown, context)).toEqual({ state: "unknown", operation_id: unknown });
  });
});
