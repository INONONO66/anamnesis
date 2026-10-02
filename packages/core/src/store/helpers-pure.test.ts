import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import neo4j from "neo4j-driver";
import { EpisodeLineageError, SCHEMA_ID, type SemanticClaim, type SemanticReviewPremises, type SemanticSourceContext, extractionBodyDigest, validateSemanticClaim } from "@anamnesis/protocol";
import { CANONICAL_DIGEST, StorageContractError, canonicalJson, elementDigest, linkIdemKey, originKey, semanticClaimTime, sessionKey, sha256, tupleHash, validatedFactTime, verifyLineageRetry } from "./digest.ts";
import { decodeHistoricalElement, nodeProps, recordsToObjects, relProps, toElement } from "./records.ts";
import { arcIdentity, arcTuple, conductingPartition, topologyExpectations, type TopologyRow } from "./conducting.ts";
import { type CacheEvidence, cacheExpectations, cacheMatches } from "./hit-cache.ts";
import { CommitReceiptInput, ReceiptError, ReceiptHit, luceneQuery } from "./receipts.ts";
import { policyBody, policySelector, requireInstallation } from "./policy.ts";
import { ADOPTION_NUMERIC_VERSION } from "../dynamics/adoption-numeric.ts";
import { replayDynamics } from "../dynamics/state.ts";

const uuid = (n: number) => `018f5b5e-7b1e-7abc-8def-${String(n).padStart(12, "0")}`;
const hex = (s: string) => createHash("sha256").update(s).digest("hex");
const origin = { source: "chat", session: "s1", actor: "alice", record: "r1" };

describe("digest", () => {
  test("semanticClaimTime floors the UTC instant to the claimed precision", () => {
    const value = "2026-09-10T12:34:56Z";
    const at = (iso: string) => Date.parse(iso);
    expect(semanticClaimTime({ value, precision: "second" })).toEqual({ time_value: value, time_utc: at(value), time_precision: "instant", resolution: "explicit", anchor_time_utc: null });
    expect(semanticClaimTime({ value, precision: "minute" }).time_precision).toBe("instant");
    expect(semanticClaimTime({ value, precision: "day" })).toMatchObject({ time_utc: at("2026-09-10T00:00:00Z"), time_precision: "day" });
    expect(semanticClaimTime({ value, precision: "month" })).toMatchObject({ time_utc: at("2026-09-01T00:00:00Z"), time_precision: "month" });
    expect(semanticClaimTime({ value, precision: "year" })).toMatchObject({ time_utc: at("2026-01-01T00:00:00Z"), time_precision: "year" });
  });

  test("validatedFactTime keeps the Episode precision for inherited time and the claim's own otherwise", () => {
    const sourceTime = { time_value: "2026-09-10T12:00:00Z", time_utc: Date.parse("2026-09-10T12:00:00Z"), time_precision: "instant" as const };
    const lineage = { episode_id: uuid(1), lineage_mode: "direct" as const, parent_recall_ids: [], context_digests: [], root_episode_ids: [uuid(1)], echo_depth: 0, complete: true };
    const content = "Alice moved to Busan.";
    const context: SemanticSourceContext = {
      generation: uuid(3), fact_language_policy: "source", allow_no_single_locus: false,
      episode: { id: uuid(1), schema: "anamnesis.original-message/1", revision_key: "a".repeat(64), content_digest: hex(content), content, content_language: "en",
        ingest_seq: 7, time: { ...sourceTime }, speaker: null, provenance: { episode_digest_version: 2, origin_role: "user", lineage, lineage_digest: extractionBodyDigest(lineage) } },
      entity_resolutions: [{ status: "existing", mention: "Alice", entity_id: uuid(2) }], attribution_speakers: [],
    };
    const claim = (time: SemanticClaim["time"]): SemanticClaim => ({
      content, content_language: "en", sub_kind: "event", modality: "asserted", confidence: 0.8, time,
      entities: [{ mention: "Alice", entity_id: uuid(2) }], subject_keys: [uuid(2)], predicate_text: "moved to",
      scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] },
      scope_complete: true, evidence: { kind: "source_locus", quote: content },
    });
    const source: SemanticReviewPremises["source"] = { ...context.episode, time: { ...sourceTime, time_precision: "day" } };
    const inherited = validateSemanticClaim(claim({ ...sourceTime, time_precision: "inherited", resolution: "inherited", anchor_time_utc: sourceTime.time_utc }), context);
    expect(validatedFactTime(inherited, source)).toEqual({ value: "2026-09-10T12:00:00.000Z", precision: "day" });
    expect(validatedFactTime(inherited, { ...source, time: sourceTime })).toEqual({ value: "2026-09-10T12:00:00.000Z", precision: "second" });
    const explicit = validateSemanticClaim(claim({ time_value: "2026-03", time_utc: Date.parse("2026-03-01T00:00:00Z"), time_precision: "month", resolution: "explicit", anchor_time_utc: null }), context);
    expect(validatedFactTime(explicit, source)).toEqual({ value: "2026-03-01T00:00:00.000Z", precision: "month" });
    const instant = validateSemanticClaim(claim({ ...sourceTime, resolution: "explicit", anchor_time_utc: null }), context);
    expect(validatedFactTime(instant, source).precision).toBe("second");
  });

  test("sha256 and tupleHash are the node:crypto hex digests of the bytes and the JSON tuple", () => {
    expect(sha256("abc")).toBe("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    expect(sha256(new Uint8Array([97, 98, 99]))).toBe(sha256("abc"));
    expect(tupleHash(["a", "b"])).toBe(hex('["a","b"]'));
    expect(originKey(origin)).toBe(hex('["chat","s1","alice","r1"]'));
    expect(sessionKey(origin)).toBe(hex('["chat","s1"]'));
    expect(linkIdemKey({ from: "f", to: "t", role: "MENTIONS", content: "c" }, true)).toBe(hex('["f","t","MENTIONS"]'));
    expect(linkIdemKey({ from: "f", to: "t", role: "MENTIONS", content: "c" }, false)).toBe(hex('["f","t","MENTIONS","c"]'));
  });

  test("canonicalJson sorts members by UTF-16 key order at every depth and refuses lone surrogates", () => {
    expect(canonicalJson({ b: 1, a: [true, null, { z: "x", "10": 2, "9": 3 }], "\u00e9": "e" })).toBe('{"a":[true,null,{"10":2,"9":3,"z":"x"}],"b":1,"é":"e"}');
    expect(canonicalJson("a\"b")).toBe('"a\\"b"');
    expect(canonicalJson(null)).toBe("null");
    expect(canonicalJson(1.5)).toBe("1.5");
    expect(() => canonicalJson("\uD83D")).toThrow(new StorageContractError("invalid_canonical_json", "lone surrogate"));
    expect(() => canonicalJson({ ok: ["\uDC00"] })).toThrow(/invalid_canonical_json/);
    expect(() => canonicalJson({ "\uD83D": 1 })).toThrow(/invalid_canonical_json/);
    expect(canonicalJson("\uD83D\uDE00")).toBe('"\uD83D\uDE00"');
  });

  test("elementDigest hashes the hand-written canonical body under the stored format, legacy insertion order under null, and refuses unknown formats", () => {
    const episode = { schema: SCHEMA_ID.ORIGINAL_MESSAGE, time: { value: "2026-09-10T12:00:00Z", precision: "second" as const }, content: "hi", properties: { b: 1, a: 2, payload_hash: "x" } };
    const time = '"time":{"precision":"second","value":"2026-09-10T12:00:00Z"}';
    const canonical = `{"content":"hi","payload_hash":null,"previous_revision_key":null,"properties":{"a":2,"b":1},"schema":"anamnesis.original-message/1",${time}}`;
    const legacy = '{"schema":"anamnesis.original-message/1","content":"hi","properties":{"b":1,"a":2},"time":{"value":"2026-09-10T12:00:00Z","precision":"second"},"payload_hash":null,"previous_revision_key":null}';
    expect(elementDigest(episode)).toBe(hex(canonical));
    expect(elementDigest(episode, { format: CANONICAL_DIGEST })).toBe(hex(canonical));
    expect(elementDigest(episode, { format: null })).toBe(hex(legacy));
    expect(hex(legacy)).not.toBe(hex(canonical));
    expect(elementDigest(episode, { payloadHash: "p", previousRevisionKey: "k" })).toBe(hex(canonical.replace('"payload_hash":null,"previous_revision_key":null', '"payload_hash":"p","previous_revision_key":"k"')));
    expect(elementDigest({ schema: SCHEMA_ID.ENTITY, time: episode.time, content: "e" })).toBe(hex('{"content":"e","payload_hash":null,"previous_revision_key":null,"properties":{},"schema":"anamnesis.entity/1","time":null}'));
    expect(elementDigest({ schema: SCHEMA_ID.ORIGINAL_MESSAGE, content: "no time" })).toBe(hex('{"content":"no time","payload_hash":null,"previous_revision_key":null,"properties":{},"schema":"anamnesis.original-message/1","time":null}'));
    expect(() => elementDigest(episode, { format: "rfc8785-v0" })).toThrow(new StorageContractError("unsupported_digest_format", "rfc8785-v0"));
    expect(() => elementDigest(episode, { format: 7 })).toThrow(/unsupported_digest_format: 7/);
    expect(() => elementDigest(episode, { format: "episode-rfc8785-v2" })).toThrow(new StorageContractError("unsupported_digest_format", "episode-rfc8785-v2"));
  });

  test("elementDigest version 2 binds the origin role and lineage digest and demands the v2 format marker", () => {
    const episode = { schema: SCHEMA_ID.ORIGINAL_MESSAGE, time: { value: "2026-09-10T12:00:00Z", precision: "second" as const }, content: "hi", properties: {} };
    const v2 = (originRole: string, lineageDigest: string) => `{"content":"hi","episode_digest_version":2,"lineage_digest":${lineageDigest},"origin_role":${originRole},"payload_hash":null,"previous_revision_key":null,"properties":{},"schema":"anamnesis.original-message/1","time":{"precision":"second","value":"2026-09-10T12:00:00Z"}}`;
    expect(elementDigest(episode, { episodeDigestVersion: 2, format: "episode-rfc8785-v2", originRole: "user", lineageDigest: "l" })).toBe(hex(v2('"user"', '"l"')));
    expect(elementDigest(episode, { episodeDigestVersion: 2, format: "episode-rfc8785-v2" })).toBe(hex(v2("null", "null")));
    expect(() => elementDigest(episode, { episodeDigestVersion: 2, format: CANONICAL_DIGEST })).toThrow(new EpisodeLineageError("unsupported_digest_version"));
    expect(() => elementDigest(episode, { episodeDigestVersion: 1, format: "episode-rfc8785-v2" })).toThrow(EpisodeLineageError);
  });

  test("verifyLineageRetry accepts a retry whose role, mode and parents (in any order) match the retained lineage", () => {
    const parents = [uuid(5), uuid(6)];
    const lineage = { episode_id: uuid(1), lineage_mode: "receipts" as const, parent_recall_ids: parents, context_digests: ["a".repeat(64), "b".repeat(64)], root_episode_ids: [uuid(9)], echo_depth: 1, complete: true };
    expect(verifyLineageRetry({ origin_role: "user", lineage_mode: "receipts", parent_recall_ids: [uuid(6), uuid(5)] }, "user", lineage)).toBeUndefined();
    const conflict = new StorageContractError("revision_conflict", uuid(1));
    expect(() => verifyLineageRetry({ origin_role: "assistant", lineage_mode: "receipts", parent_recall_ids: parents }, "user", lineage)).toThrow(conflict);
    expect(() => verifyLineageRetry({ origin_role: "user", lineage_mode: "receipts", parent_recall_ids: [uuid(5), uuid(7)] }, "user", lineage)).toThrow(conflict);
    const direct = { ...lineage, lineage_mode: "direct" as const, parent_recall_ids: [], context_digests: [], root_episode_ids: [uuid(1)], echo_depth: 0 };
    expect(() => verifyLineageRetry({ origin_role: "user", lineage_mode: "receipts", parent_recall_ids: parents }, "user", direct)).toThrow(conflict);
    expect(() => verifyLineageRetry({ origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, null, direct)).toThrow(conflict);
    expect(() => verifyLineageRetry({ origin_role: "nobody" }, "user", lineage)).toThrow(new EpisodeLineageError("invalid_params"));
  });
});

describe("records", () => {
  const stored = { id: uuid(1), schema: SCHEMA_ID.ORIGINAL_MESSAGE, content: "hi", mass: 0.5, properties: '{"k":"v"}', time_value: "2026-09-10T12:00:00Z", time_precision: "second",
    origin_source: "chat", origin_session: "s1", origin_actor: "alice", origin_record: "r1", payload_hash: null };

  test("recordsToObjects, nodeProps and relProps unwrap driver records", () => {
    const record = new neo4j.types.Record(["n", "k"], [1, "v"]);
    expect(recordsToObjects([record])).toEqual([{ n: 1, k: "v" }]);
    expect(recordsToObjects([])).toEqual([]);
    const node = new neo4j.types.Node(1, ["Element"], { id: uuid(1) });
    expect(nodeProps(node)).toEqual({ id: uuid(1) });
    const rel = new neo4j.types.Relationship(2, 1, 3, "MENTIONS", { idem_key: "x" });
    expect(relProps(rel)).toEqual({ idem_key: "x" });
  });

  test("toElement rebuilds the element, adding payload_hash and dropping an absent time", () => {
    expect(toElement(stored)).toEqual({ id: uuid(1), schema: SCHEMA_ID.ORIGINAL_MESSAGE, time: { value: "2026-09-10T12:00:00Z", precision: "second" }, content: "hi", origin, mass: 0.5, properties: { k: "v" } });
    expect(toElement({ ...stored, payload_hash: "p".repeat(64) }).properties).toEqual({ k: "v", payload_hash: "p".repeat(64) });
    const entity = { ...stored, schema: SCHEMA_ID.ENTITY, time_value: null, time_precision: null, properties: null };
    expect(toElement(entity)).toMatchObject({ schema: SCHEMA_ID.ENTITY, properties: {} });
    expect(toElement(entity)).not.toHaveProperty("time");
    expect(toElement({ ...entity, time_value: "" })).not.toHaveProperty("time");
    expect(toElement({ ...entity, time_precision: "second" })).not.toHaveProperty("time");
    expect(() => toElement({ ...stored, mass: 2 })).toThrow();
  });

  test("decodeHistoricalElement reads the frozen historical shape and keeps a half-present time, unlike toElement", () => {
    const decoded = decodeHistoricalElement({ ...stored, schema: SCHEMA_ID.CLAIM, properties: '{"speaker_key":null}' });
    expect(decoded).toMatchObject({ id: uuid(1), schema: SCHEMA_ID.CLAIM, properties: { speaker_key: null }, time: { value: "2026-09-10T12:00:00Z", precision: "second" } });
    expect(decodeHistoricalElement({ ...stored, time_value: null, time_precision: null })).not.toHaveProperty("time");
    expect(() => decodeHistoricalElement({ ...stored, time_value: null, time_precision: "second" })).toThrow();
    expect(() => decodeHistoricalElement({ ...stored, properties: null })).toThrow();
    expect(() => decodeHistoricalElement({ ...stored, id: "not-a-uuid" })).toThrow();
  });
});

describe("conducting", () => {
  test("partitions, identities and tuples are derived from the arc row", () => {
    expect(conductingPartition("NEXT_EPISODE", null)).toEqual({ stream: "cache", generation: 0 });
    expect(conductingPartition("HAS_MEMBER", 3)).toEqual({ stream: "community", generation: 3 });
    expect(conductingPartition("MENTIONS", 2)).toEqual({ stream: "extraction", generation: 2 });
    const row = { source_id: "s", link_id: "l", peer_id: "p", role: "MENTIONS", generation: 2, source_extraction_generation: null };
    expect(arcIdentity(row)).toBe('["s","l"]');
    expect(arcTuple(row)).toBe('["s","l","p","MENTIONS",2,null]');
  });

  test("topologyExpectations derives parents from the explicit previous record or the session order", () => {
    const row = (id: string, record: string, previousRecord: string | null, ingestSeq: number, sessionKey = "k1"): TopologyRow =>
      ({ id, sessionKey, record, previousRecord, timeUtc: "t", ingestSeq, version: null, actual: [] });
    const rows = [row("a", "r1", null, 1), row("b", "r2", "r1", 2), row("c", "r3", "r9", 3), row("d", "r1", "r2", 4), row("e", "r2", "r1", 5), row("x", "r1", null, 6, "k2"), row("y", "r2", null, 7, "k2")];
    expect(topologyExpectations(rows).map(r => [r.id, r.parents])).toEqual([
      ["a", []], ["b", ["a"]], ["c", []], ["d", ["b"]], ["e", ["a", "d"]], ["x", []], ["y", ["x"]],
    ]);
    expect(topologyExpectations([])).toEqual([]);
  });
});

describe("hit-cache", () => {
  const episode = uuid(1), namespace = uuid(2);
  const attribution = [{ id: episode, rank: 0, sources: [uuid(3)] }];
  const hit = (n: number, kind: "exposure" | "recall_hit" | "outcome", t: number): ReceiptHit => {
    const base = { id: uuid(10 + n), episode_id: episode, operation_id: uuid(20 + n), namespace, idem_key: tupleHash([namespace, episode, kind]), t, attribution, config_version: "g003-dynamics-v1" as const };
    if (kind === "recall_hit") return { ...base, kind, kappa_eff: 0.5 };
    if (kind === "outcome") return { ...base, kind, kappa_eff: 0, reward: 0.5, weight: 0.25 };
    return { ...base, kind, kappa_eff: 0 };
  };
  const stored = (h: ReceiptHit, targets = [h.episode_id]): CacheEvidence["hits"][number] => {
    const body = canonicalJson(h);
    return { props: { ...h, attribution: canonicalJson(h.attribution), body, body_digest: sha256(body) }, targets };
  };
  const evidence = (hits: CacheEvidence["hits"], caches: CacheEvidence["caches"] = []): CacheEvidence => ({ episodes: [{ id: episode, mass: 0.5, ingested_at: 1000 }], hits, caches });

  test("replays valid hits through the dynamics fold, where every event counts but only a recall hit moves the last-hit time", () => {
    const hits = [hit(1, "exposure", 1500), hit(2, "recall_hit", 2000), hit(3, "outcome", 3000)];
    const oracle = replayDynamics({ initialMass: 0.5, ingestedAt: 1000, priorRewardSum: 0, priorWeight: 0 }, [
      { id: uuid(11), at: 1500, kind: "exposure" }, { id: uuid(12), at: 2000, kind: "recall_hit", kappa: 0.5 }, { id: uuid(13), at: 3000, kind: "outcome", reward: 0.5, weight: 0.25 },
    ]);
    expect([oracle.hitCount, oracle.lastHit, oracle.weight]).toEqual([3, 2000, 0.25]);
    const { expected, issues } = cacheExpectations(evidence(hits.map(h => stored(h))));
    expect(issues).toEqual([]);
    expect(expected).toEqual([{ episode_id: episode, s: oracle.stability, t_last_hit: oracle.lastHit, hit_count: oracle.hitCount, utility_reward_sum: 0.125, utility_weight: oracle.weight,
      utility: oracle.utility, event_ids: [uuid(11), uuid(12), uuid(13)], config_version: "g003-dynamics-v1", numeric_version: ADOPTION_NUMERIC_VERSION }]);
    const cache = expected[0]!;
    expect(cacheMatches({ props: cache, targets: [episode] }, cache)).toBe(true);
    expect(cacheMatches({ props: { ...cache, hit_count: 2 }, targets: [episode] }, cache)).toBe(false);
    const { numeric_version: _dropped, ...unversioned } = cache;
    expect(cacheMatches({ props: unversioned, targets: [episode] }, cache)).toBe(false);
    expect(cacheMatches({ props: cache, targets: [uuid(9)] }, cache)).toBe(false);
    expect(cacheMatches({ props: cache, targets: [episode, episode] }, cache)).toBe(false);
    expect(cacheMatches(undefined, cache)).toBe(false);
    expect(cacheExpectations(evidence([]))).toEqual({ expected: [], issues: [] });
  });

  test("an episode with a stale cache row but no hits is still expected, as the empty history", () => {
    const stale = { episode_id: episode, s: 1, t_last_hit: 0, hit_count: 9, utility_reward_sum: 0, utility_weight: 0, utility: 0, event_ids: [], config_version: "g003-dynamics-v1" as const };
    const { expected } = cacheExpectations(evidence([], [{ props: stale, targets: [episode] }]));
    expect(expected).toHaveLength(1);
    expect(expected[0]).toMatchObject({ episode_id: episode, hit_count: 0, t_last_hit: 1000, event_ids: [] });
  });

  test("every tampered hit is reported as invalid evidence and excluded from the replay", () => {
    const good = hit(1, "exposure", 1500);
    const row = stored(good);
    const tampered: [string, CacheEvidence["hits"][number]][] = [
      ["body not json", { ...row, props: { ...row.props, body: "{" } }],
      ["body not a string", { ...row, props: { ...row.props, body: 42 } }],
      ["body not a hit", { ...row, props: { ...row.props, body: "null" } }],
      ["body not canonical", { ...row, props: { ...row.props, body: JSON.stringify({ ...good }) } }],
      ["digest mismatch", { ...row, props: { ...row.props, body_digest: sha256("x") } }],
      ["property drift", { ...row, props: { ...row.props, t: 1501 } }],
      ["attribution drift", { ...row, props: { ...row.props, attribution: "[]" } }],
      ["no target", { ...row, targets: [] }],
      ["wrong target", { ...row, targets: [uuid(9)] }],
      ["unknown episode", stored({ ...good, episode_id: uuid(9), idem_key: tupleHash([namespace, uuid(9), "exposure"]) })],
      ["idem key drift", stored({ ...good, idem_key: "f".repeat(64) })],
    ];
    for (const [label, bad] of tampered) {
      const { expected, issues } = cacheExpectations(evidence([bad]));
      expect([label, issues]).toEqual([label, [{ code: "invalid_hit_evidence", id: String(bad.props["id"]) }]]);
      expect([label, expected]).toEqual([label, []]);
    }
    const duplicateId = stored(hit(1, "recall_hit", 1600));
    const duplicateKey = stored(hit(2, "exposure", 1700));
    const { expected, issues } = cacheExpectations(evidence([row, duplicateId, duplicateKey]));
    expect(issues).toEqual([{ code: "invalid_hit_evidence", id: good.id }, { code: "invalid_hit_evidence", id: uuid(12) }]);
    expect(expected[0]?.event_ids).toEqual([good.id]);
  });

  test("a non-syntax failure while decoding propagates instead of being reported as evidence", () => {
    const row = stored(hit(1, "exposure", 1500));
    const props = new Proxy(row.props, { get: (target, key) => { if (key === "body") throw new RangeError("boom"); return Reflect.get(target, key); } });
    expect(() => cacheExpectations(evidence([{ ...row, props }]))).toThrow(new RangeError("boom"));
  });
});

describe("receipts and policy", () => {
  test("luceneQuery strips operator characters and collapses whitespace", () => {
    expect(luceneQuery('a+b -c (d) "e" f:g\\h/i ~j *k ?l !m &n |o ^p {q} [r]')).toBe("a b c d e f g h i j k l m n o p q r");
    expect(luceneQuery("  spaced\tout\nwords  ")).toBe("spaced out words");
    expect(luceneQuery("+-")).toBe("");
  });

  test("CommitReceiptInput needs adopted or reward, and distinct adopted IDs", () => {
    const base = { operation_id: uuid(1), recall_id: uuid(2) };
    expect(CommitReceiptInput.parse({ ...base, reward: 0.5 })).toEqual({ ...base, reward: 0.5 });
    expect(CommitReceiptInput.parse({ ...base, adopted: [uuid(3), uuid(4)] }).adopted).toEqual([uuid(3), uuid(4)]);
    expect(CommitReceiptInput.parse({ ...base, adopted: [] })).toEqual({ ...base, adopted: [] });
    expect(CommitReceiptInput.safeParse(base).error?.issues.map(i => i.message)).toEqual(["adopted or reward is required"]);
    expect(CommitReceiptInput.safeParse({ ...base, adopted: [uuid(3), uuid(3)] }).error?.issues.map(i => i.message)).toEqual(["adopted IDs must be distinct"]);
  });

  test("ReceiptError carries its code in the message and defaults the detail to the code", () => {
    expect(new ReceiptError("unknown_recall")).toMatchObject({ code: "unknown_recall", message: "unknown_recall: unknown_recall" });
    expect(new ReceiptError("policy_denied", "unknown_policy").message).toBe("policy_denied: unknown_policy");
  });

  test("policySelector drops absent members, policyBody is canonical, requireInstallation refuses other principals", () => {
    const base = { policy_id: uuid(1), scope: "content" as const };
    expect(policySelector({ episode_id: uuid(2) })).toEqual({ episode_id: uuid(2) });
    expect(policySelector({ source: "chat" })).toEqual({ source: "chat" });
    expect(policySelector({ source: "chat", episode_id: uuid(2) })).toEqual({ episode_id: uuid(2), source: "chat" });
    expect(policySelector({ source: "chat", episode_id: undefined })).toEqual({ source: "chat" });
    expect(policyBody({ ...base, selector: { source: "chat", episode_id: undefined } })).toBe(`{"policy_id":"${uuid(1)}","scope":"content","selector":{"source":"chat"}}`);
    expect(policyBody({ ...base, selector: { source: "chat", episode_id: uuid(2) } })).toBe(`{"policy_id":"${uuid(1)}","scope":"content","selector":{"episode_id":"${uuid(2)}","source":"chat"}}`);
    expect(policyBody({ ...base, selector: { source: "chat" }, action: "deny", principal: "installation", revision: 1, created_at: 5 })).toBe(`{"action":"deny","created_at":5,"policy_id":"${uuid(1)}","principal":"installation","revision":1,"scope":"content","selector":{"source":"chat"}}`);
    expect(requireInstallation({ principal: "installation", commit_mode: "auto" })).toBeUndefined();
    expect(() => requireInstallation({ principal: "client" as never, commit_mode: "auto" })).toThrow(new ReceiptError("unauthenticated"));
    expect(() => requireInstallation(undefined as never)).toThrow(new ReceiptError("unauthenticated"));
  });
});
