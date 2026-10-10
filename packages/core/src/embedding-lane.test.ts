import { afterAll, beforeAll, describe, expect, setDefaultTimeout, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j, { type RecordShape } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";
import { EmbeddingError, EmbeddingProfile, type EmbeddingProvider, embeddingProfileId } from "./embedding.ts";
import type { ExtractionProviderInput } from "./extraction.ts";
import { extractionBodyDigest, type Generation, type SemanticClaim, type SemanticReviewProvider } from "@anamnesis/protocol";
import { EmbeddingLedger } from "./store/embedding-ledger.ts";
import { tupleHash } from "./store/digest.ts";

const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
const user = process.env["ANAMNESIS_TEST_NEO4J_USER"] ?? "neo4j";
const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("ANAMNESIS_TEST_NEO4J_URI and ANAMNESIS_TEST_NEO4J_PASSWORD are required");
setDefaultTimeout(120_000);

const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const profile = EmbeddingProfile.parse({
  model: "embedding-lane-fixture", model_incarnation: "c".repeat(64), dimensions: 2,
  document_prefix: "", query_prefix: "", max_input_bytes: 8192,
  norm: "unit_l2", norm_tolerance: 0.001,
});
const state = { now: Date.UTC(2030, 0, 1), mode: "ok" as "ok" | "down" | "reject", calls: 0, inputs: [] as string[] };
const provider: EmbeddingProvider = {
  profile,
  async embed(text, purpose) {
    expect(purpose).toBe("document");
    state.calls++;
    state.inputs.push(text);
    if (state.mode === "down") throw new EmbeddingError("provider_unavailable", "http 503");
    if (state.mode === "reject" && text.includes("reject-me")) throw new EmbeddingError("provider_rejected", "http 400");
    return [1, 0];
  },
};
const extractionProvider = {
  model: "embedding-claim-fixture", modelIncarnation: extractionBodyDigest("embedding-claim-fixture"),
  async extract(input: ExtractionProviderInput) {
    if (input.task === "claim") {
      const text = input.text.split("\n")[0]!;
      return { task: "claim", language: "en", modality: "text",
        claims: [{ text, evidence: { start: 0, end: Buffer.byteLength(text), text } }] };
    }
    if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected task ${input.task}`);
    return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context.body_digest,
      decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
  },
};
const semanticReviewProvider: SemanticReviewProvider = {
  profileId: extractionBodyDigest("embedding-review-fixture"),
  async resolve() { return { entity_resolutions: [], attribution_speakers: [], allow_no_single_locus: false, content_language: "en" }; },
  async review(input) { return { disposition: "retain", semantic_claim: input.premises.request.semantic_claim, reason: "fixture retained" }; },
};
const generation: Generation = { id: uuidv7(), stream: "extraction", incarnation: extractionProvider.modelIncarnation,
  state: "catching_up", covered_ingest_seq: 0, created_at: state.now, updated_at: state.now };
const root = await mkdtemp(join(tmpdir(), "anamnesis-embedding-lane-"));
const ledgerPath = join(root, "embedding-state.json");
const engine = new Engine({ uri, user, password, objectsRoot: root, embeddingLedgerPath: ledgerPath,
  embeddingProvider: provider, extractionProvider, semanticReviewProvider, clock: () => state.now });
const admin = neo4j.driver(uri, neo4j.auth.basic(user, password), { disableLosslessIntegers: true });

async function query<Row extends RecordShape>(cypher: string, params: Record<string, string> = {}): Promise<Row[]> {
  const result = await admin.executeQuery<Row>(cypher, params);
  return result.records.map((row: { toObject(): Row }) => row.toObject());
}

async function ledger(): Promise<{ version: number; facts: Record<string, {
  profile_id: string; state: string; deferrals: number; retry_after: number | null;
  attempts: { operation_id: string; reason: string | null }[];
}> }> {
  return JSON.parse(await readFile(ledgerPath, "utf8"));
}

async function episode(content: string): Promise<string> {
  return (await engine.remember({
    content, time: { value: "2026-09-01T00:00:00Z", precision: "second" },
    origin: { source: "chat-export", session: "embedding-fixture", actor: "test", record: uuidv7() },
  }, { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context })).id;
}

async function fact(content: string, generationId = generation.id): Promise<string> {
  const sourceId = await episode(`${content}\nRaw context stays in the Episode.`);
  const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generationId, source_id: sourceId }, context);
  const audit = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "qa", lease_ms: 30000 }, context);
  if (audit.state !== "known" || audit.judge_attempt?.state !== "succeeded" || audit.semantic_writes)
    throw new Error("audit fixture must retain the claim without writing a Fact");
  const source = await engine.store.semanticEpisode(sourceId, context);
  const semantic: SemanticClaim = {
    content, content_language: "en", sub_kind: "fact", modality: "asserted", confidence: 0.7,
    time: { time_value: source.time.time_value, time_utc: source.time.time_utc, time_precision: "inherited",
      resolution: "inherited", anchor_time_utc: source.time.time_utc },
    entities: [], subject_keys: null, predicate_text: content, scope_complete: false,
    scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] },
    evidence: { kind: "source_locus", span: { start: 0, end: Buffer.byteLength(content) } },
  };
  const request = { proposal_id: uuidv7(), generation_id: generationId, source_episode_id: sourceId,
    judge_attempt_id: audit.judge_attempt.id, claim_index: 0, semantic_claim: semantic };
  await engine.proposeRetainedClaim(request, context);
  await engine.reviewRetainedClaim({ review_id: uuidv7(), proposal_id: request.proposal_id, action: "accept", reason: "fixture accepted" }, context);
  return (await engine.materializeRetainedClaim({ ...request, operation_id: uuidv7() }, context)).fact_id;
}

beforeAll(async () => {
  await engine.init();
  await engine.claimWriterEpoch();
  await engine.store.createExtractionGeneration(generation, context);
});
afterAll(async () => {
  await engine.close();
  await admin.close();
  await rm(root, { recursive: true });
});

describe.serial("embedding lane on isolated Neo4j", () => {
  test("C001 a provider 400 on one input is terminal on the first attempt", async () => {
    // Given five Facts, exactly one of which the provider rejects.
    state.mode = "reject";
    const ids: string[] = [];
    for (let i = 0; i < 5; i++) ids.push(await fact(i === 2 ? "reject-me" : `valid-${i}`));

    // When the worker drains the missing-vector scan to completion.
    for (;;) {
      const result = await engine.drainEmbeddingOutbox(100);
      if ("reason" in result) throw new Error("embedding provider unexpectedly disabled");
      if (result.drained === 0 && result.deferred === 0) break;
    }

    // Then four vectors exist, the one rejected Fact has a single terminal ledger attempt,
    // and neither retired graph label has been created.
    const vectors = await query<{ count: number }>("MATCH (v:EmbeddingVector) RETURN count(v) AS count");
    expect(vectors[0]?.count).toBe(4);
    const entries = (await ledger()).facts;
    const rejected = ids[2];
    if (!rejected) throw new Error("rejected Fact was not created");
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
    expect(Object.keys((await ledger()).facts)).toHaveLength(0);
  });

  test("C002 transient failures defer with backoff, exhaust into quarantine, and requeue is idempotent", async () => {
    // Given one unembedded Fact and an unavailable provider.
    state.mode = "down";
    const id = await fact("transient-provider");
    const calls = state.calls;

    // When eight bounded deferrals occur, followed by a ninth transient failure.
    for (let deferrals = 1; deferrals <= 8; deferrals++) {
      const before = state.now;
      await engine.drainEmbeddingOutbox(100);
      const entry = (await ledger()).facts[id];
      expect(entry?.state).toBe("deferred");
      expect(entry?.deferrals).toBe(deferrals);
      expect(entry?.retry_after).toBe(before + Math.min(30_000 * 2 ** (deferrals - 1), 3_600_000));
      expect(entry?.attempts).toHaveLength(deferrals);
      if (entry?.retry_after === null || entry?.retry_after === undefined) throw new Error("deferral has no retry deadline");
      state.now = entry.retry_after + 1;
    }
    await engine.drainEmbeddingOutbox(100);
    const exhausted = (await ledger()).facts[id];
    expect(exhausted?.state).toBe("quarantined");
    expect(exhausted?.attempts).toHaveLength(9);
    expect(exhausted?.attempts[8]?.reason).toBe("provider_unavailable_exhausted");
    expect(state.calls - calls).toBe(9);

    // Then requeue clears quarantine once, resets its budget, and a later success
    // writes the vector while deleting its ledger entry.
    expect(await engine.requeueQuarantinedEmbeddings({ limit: 100 }, context)).toEqual({ requeued: 1 });
    expect((await ledger()).facts[id]).toBeUndefined();
    await engine.drainEmbeddingOutbox(100);
    const retried = (await ledger()).facts[id];
    expect(retried?.state).toBe("deferred");
    expect(retried?.deferrals).toBe(1);
    state.mode = "ok";
    if (retried?.retry_after === null || retried?.retry_after === undefined) throw new Error("retry has no deadline");
    state.now = retried.retry_after + 1;
    expect(await engine.drainEmbeddingOutbox(100)).toMatchObject({ drained: 1 });
    const vectors = await query<{ operation_id: string }>(
      "MATCH (v:EmbeddingVector {fact_id:$id}) RETURN v.operation_id AS operation_id", { id });
    expect(vectors).toHaveLength(1);
    expect(Object.keys((await ledger()).facts)).toHaveLength(0);
    expect(await engine.requeueQuarantinedEmbeddings({ limit: 100 }, context)).toEqual({ requeued: 0 });
    const succeeded = vectors[0]?.operation_id;
    if (!succeeded) throw new Error("successful vector has no operation");
    expect(await engine.embeddingStatus(succeeded, context)).toMatchObject({
      state: "succeeded", fact_id: id, operation_id: succeeded,
      model: profile.model, model_incarnation: profile.model_incarnation, dimensions: profile.dimensions,
    });
    // A vector written before the lane recorded model metadata answers with its persisted profile's metadata.
    await query("MATCH (v:EmbeddingVector {operation_id:$id}) REMOVE v.model, v.incarnation, v.dimensions", { id: succeeded });
    expect(await engine.embeddingStatus(succeeded, context)).toMatchObject({
      state: "succeeded", operation_id: succeeded, profile_id: embeddingProfileId(profile),
      model: profile.model, model_incarnation: profile.model_incarnation, dimensions: profile.dimensions,
    });
    const unknown = uuidv7();
    expect(await engine.embeddingStatus(unknown, context)).toEqual({ state: "unknown", operation_id: unknown });
  });

  test("a recover under a fresh id for an embedded Fact answers with the vector's attempt and no provider call", async () => {
    // Given a Fact whose vector was written by the lane under its own operation id.
    state.mode = "ok";
    const id = await fact("already-embedded");
    expect(await engine.drainEmbeddingOutbox(100)).toMatchObject({ drained: 1 });
    const [vector] = await query<{ operation_id: string }>(
      "MATCH (v:EmbeddingVector {fact_id:$id}) RETURN v.operation_id AS operation_id", { id });
    if (!vector) throw new Error("drained Fact has no vector");
    const calls = state.calls;

    // When an operator recovers it twice under one fresh operation id.
    const fresh = uuidv7();
    const first = await engine.recoverEmbedding({ fact_id: id, operation_id: fresh }, context);
    const replay = await engine.recoverEmbedding({ fact_id: id, operation_id: fresh }, context);

    // Then both answers are the attempt that wrote the vector, the provider stays idle,
    // the Fact still holds one vector, and the fresh id did no work.
    expect(first).toMatchObject({ state: "succeeded", operation_id: vector.operation_id, fact_id: id });
    expect(replay).toEqual(first);
    expect(state.calls).toBe(calls);
    const count = await query<{ count: number }>("MATCH (v:EmbeddingVector {fact_id:$id}) RETURN count(v) AS count", { id });
    expect(count[0]?.count).toBe(1);
    expect(await engine.embeddingStatus(fresh, context)).toEqual({ state: "unknown", operation_id: fresh });
  });

  test("a drain drops a ledger entry whose Fact holds a vector and embeds past another profile's entry", async () => {
    // Given an embedded Fact and an unembedded one, and a ledger - as a process loss between the vector
    // commit and the entry's deletion, or a seeded migration, leaves it - that quarantines the embedded one
    // under this profile and the unembedded one under another profile.
    state.mode = "ok";
    const embedded = await fact("ledger-stale");
    const foreignHeld = await fact("ledger-foreign-held");
    expect(await engine.drainEmbeddingOutbox(100)).toMatchObject({ drained: 2 });
    const unembedded = await fact("ledger-foreign");
    const profileId = embeddingProfileId(profile), otherProfile = "f".repeat(64);
    const quarantine = (factId: string, profile_id: string) => ({
      profile_id, state: "quarantined", deferrals: 0, retry_after: null,
      attempts: [{
        operation_id: uuidv7(), fact_id: factId, profile_id,
        model: profile.model, model_incarnation: profile.model_incarnation, dimensions: profile.dimensions,
        input_revision: "a".repeat(64), input_digest: "b".repeat(64), created_at: state.now, completed_at: state.now,
        state: "quarantined", reason: "provider_rejected", detail: null,
      }],
    });
    // ...and a third, embedded under this profile, that another profile quarantined: that profile's history.
    const facts = {
      [embedded]: quarantine(embedded, profileId), [unembedded]: quarantine(unembedded, otherProfile),
      [foreignHeld]: quarantine(foreignHeld, otherProfile),
    };
    const staleOperation = facts[embedded]!.attempts[0]!.operation_id;
    const seededPath = join(root, "seeded-embedding-state.json");
    await writeFile(seededPath, JSON.stringify({ version: 2, facts }));
    const seeded = new Engine({ uri, user, password, objectsRoot: root, embeddingLedgerPath: seededPath,
      embeddingProvider: provider, clock: () => state.now });
    try {
      await seeded.init();
      expect(await seeded.embeddingStatus(staleOperation, context)).toMatchObject({ state: "quarantined", fact_id: embedded });

      // When the engine holding that ledger drains once.
      const result = await seeded.drainEmbeddingOutbox(100);

      // Then the other profile's entry did not hide its Fact, the stale entry is gone without a provider
      // call for its Fact, the stale operation id is no longer reported, and the other profile's entry
      // for the Fact this profile already serves is left as that profile's history.
      expect(result).toMatchObject({ drained: 1, quarantined: 0, deferred: 0 });
      const vectors = await query<{ id: string; count: number }>(
        "MATCH (v:EmbeddingVector {profile_id:$profile}) WHERE v.fact_id IN [$a,$b,$c] RETURN v.fact_id AS id, count(v) AS count ORDER BY id",
        { profile: profileId, a: embedded, b: unembedded, c: foreignHeld });
      expect(vectors).toEqual([embedded, unembedded, foreignHeld].sort().map(id => ({ id, count: 1 })));
      expect(JSON.parse(await readFile(seededPath, "utf8"))).toEqual({ version: 2, facts: { [foreignHeld]: facts[foreignHeld] } });
      expect(await seeded.embeddingStatus(staleOperation, context)).toEqual({ state: "unknown", operation_id: staleOperation });
      const calls = state.calls;
      const recovered = await seeded.recoverEmbedding({ fact_id: foreignHeld, operation_id: uuidv7() }, context);
      expect(recovered).toMatchObject({ state: "succeeded", fact_id: foreignHeld, profile_id: profileId });
      expect(state.calls).toBe(calls);
      expect(JSON.parse(await readFile(seededPath, "utf8"))).toEqual({ version: 2, facts: { [foreignHeld]: facts[foreignHeld] } });
    } finally {
      await seeded.close();
    }
  });

  test("a drain embeds every generation's Facts, never their source Episodes", async () => {
    // Given Facts in two unselected generations and an Episode with no Fact.
    const other = await engine.store.createExtractionGeneration({ ...generation, id: uuidv7() }, context);
    const first = await fact("first-generation");
    const second = await fact("second-generation", other.id);
    await episode("archive-only");
    expect((await engine.status()).pendingOutbox).toBe(2);
    const calls = state.calls;
    const profileId = embeddingProfileId(profile);

    // When the missing-vector scan drains without any generation being selected.
    expect(await engine.drainEmbeddingOutbox(100)).toMatchObject({ drained: 2, quarantined: 0, deferred: 0 });

    // Then the provider saw only Fact content and the graph covers every Fact.
    expect(state.calls - calls).toBe(2);
    expect(state.inputs.slice(calls).sort()).toEqual(["first-generation", "second-generation"]);
    const vectors = await query<{ id: string; key: string; revision: string; digest: string; input_revision: string; input_digest: string;
      labels: string[]; model: string; incarnation: string; dimensions: number; vector: number[] }>(
      `MATCH (f:Element:Fact) WHERE f.id IN [$a,$b]
       MATCH (v:EmbeddingVector {fact_id:f.id,profile_id:$profile})
       RETURN f.id AS id,v.key AS key,coalesce(f.revision_key,f.digest) AS revision,f.digest AS digest,
         v.input_revision AS input_revision,v.input_digest AS input_digest,labels(v) AS labels,
         v.model AS model,v.incarnation AS incarnation,v.dimensions AS dimensions,v.vector AS vector ORDER BY id`,
      { a: first, b: second, profile: profileId });
    expect(vectors).toHaveLength(2);
    for (const row of vectors) {
      expect(row.key).toBe(tupleHash([row.id, profileId]));
      expect(row.input_revision).toBe(row.revision);
      expect(row.input_digest).toBe(row.digest);
      expect(row.labels).toContain(`Embedding_${profileId}`);
      expect([row.model, row.incarnation, row.dimensions, row.vector]).toEqual([profile.model, profile.model_incarnation, 2, [1, 0]]);
    }
    expect(await query("SHOW INDEXES YIELD name,labelsOrTypes WHERE name=$index RETURN labelsOrTypes", { index: `vec_fact_${profileId}` }))
      .toEqual([{ labelsOrTypes: [`Embedding_${profileId}`] }]);
    expect(await query(`MATCH (f:Element:Fact) WHERE NOT EXISTS {
      MATCH (:EmbeddingVector {fact_id:f.id,profile_id:$profile}) } RETURN count(f) AS count`, { profile: profileId }))
      .toEqual([{ count: 0 }]);
    expect(await query("MATCH (v:EmbeddingVector) WHERE v.episode_id IS NOT NULL RETURN count(v) AS count")).toEqual([{ count: 0 }]);
    expect(await query("MATCH (e:Episode)--(v:EmbeddingVector) RETURN count(v) AS count")).toEqual([{ count: 0 }]);
    expect((await engine.status()).pendingOutbox).toBe(0);
    const unconfigured = new Engine({ uri, user, password, objectsRoot: root });
    try { expect((await unconfigured.status()).pendingOutbox).toBe(0); }
    finally { await unconfigured.close(); }
  });

  test("recover and status authorize the Fact's source Episodes", async () => {
    // Given a Fact whose source is denied before its first embedding.
    const id = await fact("policy-source");
    const [source] = await query<{ id: string }>(
      "MATCH (:Fact {id:$id})-[:DERIVED_FROM]->(e:Episode) RETURN e.id AS id", { id });
    if (!source) throw new Error("Fact has no source");
    const policyId = uuidv7(), operation = uuidv7(), calls = state.calls;
    await engine.setPolicy({ policy_id: policyId, selector: { episode_id: source.id }, scope: "content" }, context);
    try {
      // When the operator tries to embed it, authorization refuses before the provider.
      await expect(engine.recoverEmbedding({ fact_id: id, operation_id: operation }, context)).rejects.toThrow("policy_denied");
      expect(state.calls).toBe(calls);
    } finally { await engine.revokePolicy({ policy_id: policyId }, context); }
    const succeeded = await engine.recoverEmbedding({ fact_id: id, operation_id: operation }, context);
    const denied = uuidv7();
    await engine.setPolicy({ policy_id: denied, selector: { episode_id: source.id }, scope: "content" }, context);
    try {
      // Then the stored attempt is protected by the same source policy on every replay.
      await expect(engine.embeddingStatus(succeeded.operation_id, context)).rejects.toThrow("policy_denied");
      await expect(engine.recoverEmbedding({ fact_id: id, operation_id: operation }, context)).rejects.toThrow("policy_denied");
    } finally { await engine.revokePolicy({ policy_id: denied }, context); }
    await expect(engine.recoverEmbedding({ fact_id: source.id, operation_id: uuidv7() }, context)).rejects.toThrow("invalid_selection");
  });

  test("a Fact without a digest fails the storage contract before provider work", async () => {
    // Given a materialized Fact whose digest was damaged in storage.
    const id = await fact("missing-digest");
    const [stored] = await query<{ digest: string }>("MATCH (f:Fact {id:$id}) RETURN f.digest AS digest", { id });
    if (!stored) throw new Error("Fact was not created");
    await query("MATCH (f:Fact {id:$id}) REMOVE f.digest", { id });
    const calls = state.calls;
    try {
      // When recovery loads its input, no guessed revision reaches the provider.
      await expect(engine.recoverEmbedding({ fact_id: id, operation_id: uuidv7() }, context)).rejects.toThrow("storage contract: missing digest");
      expect(state.calls).toBe(calls);
    } finally { await query("MATCH (f:Fact {id:$id}) SET f.digest=$digest", { id, digest: stored.digest }); }
    await engine.drainEmbeddingOutbox(100);
  });

  test("a version-one Episode ledger reads empty and its next mutation writes version two", async () => {
    // Given an old ledger carrying an Episode's completed failure.
    const source = await episode("legacy-ledger"), operation = uuidv7(), path = join(root, "legacy-embedding-state.json");
    await writeFile(path, JSON.stringify({ version: 1, episodes: { [source]: {
      profile_id: embeddingProfileId(profile), state: "quarantined", deferrals: 0, retry_after: null,
      attempts: [{ operation_id: operation, episode_id: source, profile_id: embeddingProfileId(profile),
        model: profile.model, model_incarnation: profile.model_incarnation, dimensions: 2,
        input_revision: "a".repeat(64), input_digest: "b".repeat(64), created_at: state.now, completed_at: state.now,
        state: "quarantined", reason: "provider_rejected", detail: "http 400" }],
    } } }));
    const legacy = new EmbeddingLedger(path);

    // When the Fact lane reads and then mutates that ledger.
    expect(await legacy.list()).toEqual([]);
    expect(await legacy.get(source)).toBeUndefined();
    await legacy.delete(source);

    // Then Episode history is discarded rather than reinterpreted as Fact work.
    expect(JSON.parse(await readFile(path, "utf8"))).toEqual({ version: 2, facts: {} });
  });
});
