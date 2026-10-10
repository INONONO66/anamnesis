import { expect, test } from "bun:test";
import { Store } from "./store.ts";
import { EmbeddingError, EmbeddingProfile, HttpEmbeddingProvider, embeddingProfileId, httpStatusReason, validateVector } from "./embedding.ts";
import { admittedBudget, packRecall, renderContext, recallResponseBytes, canonicalContext } from "./recall.ts";
import { createHash } from "node:crypto";
import { receiptBodyDigestInput, canonicalReceiptJson } from "./receipt-digest.ts";
import { countBudget } from "./dynamics/budget.ts";
import { RpcEmbeddingAttempt, RpcRecallParams, RpcRecallResult, RpcRecallItem, RpcRequest, RpcOutputBudget } from "../../protocol/src/rpc.ts";

test("receipt digest projection preserves receipt-only and embedding serving fields", () => {
  const base = { recall_id: id(998), primary_ids: [id(1)], receipt_ttl_ms: 3600000 };
  const serving = { response: { context_text: "é🙂", diagnostics: { now: 7, T: 7 }, results: [] }, query: "é🙂", query_vector: [1, 0] };
  const receipt = receiptBodyDigestInput({ ...base, serving });
  const reordered = receiptBodyDigestInput({ serving: { query_vector: [1, 0], query: "é🙂", response: { diagnostics: { T: 7, now: 7 }, context_text: "é🙂", results: [] } }, ...base });
  expect(canonicalContext(receipt)).toBe(canonicalReceiptJson(receipt));
  expect(canonicalContext(receipt)).toBe(canonicalContext(reordered));
  expect(canonicalContext(receipt)).toContain('"serving"');
  expect(canonicalContext(receipt)).toContain('é🙂');
  expect(canonicalContext(receipt)).not.toContain('selection_digest');
});

test("persisted receipt projections retain exact bytes across serialization, not across distinct recalls", () => {
  const selection = [{ element_id: id(1), root_episode_ids: [], echo_depth: 0, complete: false }];
  const selectionBytes = canonicalContext(selection);
  expect(selectionBytes).toBe('[{"complete":false,"echo_depth":0,"element_id":"0192f3a1-5e7b-7c3d-9f21-000000000001","root_episode_ids":[]}]');
  const body = { recall_id: id(999), primary_ids: [id(1)], receipt_ttl_ms: 3600000,
    serving: { response: { ...base({ unit: "utf8_bytes", limit: 65536 }), results: [item(1)], expires_at: 3600123,
      diagnostics: { ...base({ unit: "utf8_bytes", limit: 65536 }).diagnostics, now: 123, T: 123 } } } };
  const bodyBytes = canonicalContext(body);
  const hash = (bytes: string) => createHash("sha256").update(bytes).digest("hex");
  const receipt = { ...body, lineage_selection: selection, selection_digest: hash(selectionBytes), body_digest: hash(bodyBytes) };
  let persisted = canonicalContext(receipt);
  for (let i = 0; i < 3; i++) {
    const restored = JSON.parse(persisted);
    const { recall_id, primary_ids, receipt_ttl_ms, serving } = restored;
    const bytes = canonicalContext({ recall_id, primary_ids, receipt_ttl_ms, serving });
    expect(Buffer.from(bytes)).toEqual(Buffer.from(bodyBytes));
    expect(hash(bytes)).toBe(restored.body_digest);
    expect(canonicalContext(restored.lineage_selection)).toBe(selectionBytes);
    expect(hash(canonicalContext(restored.lineage_selection))).toBe(restored.selection_digest);
    expect(canonicalContext(restored)).toBe(persisted);
    persisted = JSON.stringify(restored);
  }
  expect(hash(canonicalContext({ ...body, recall_id: id(998) }))).not.toBe(hash(bodyBytes));
  expect(hash(canonicalContext([{ ...selection[0], root_episode_ids: [id(1)], complete: true }]))).not.toBe(hash(selectionBytes));
});

test("embedding recovery and hybrid recall have real core and wire entry points", () => {
  expect(typeof Reflect.get(Store.prototype, "recoverEmbedding")).toBe("function");
  expect(typeof Reflect.get(Store.prototype, "recall")).toBe("function");
  expect(typeof Reflect.get(Store.prototype, "embeddingStatus")).toBe("function");
  expect(typeof Reflect.get(Store.prototype, "requeueQuarantinedEmbeddings")).toBe("function");
  expect(typeof Reflect.get(Store.prototype, "drainEmbeddingOutbox")).toBe("function");
  expect(RpcRequest.safeParse({ jsonrpc: "2.0", id: 1, method: "recall", params: { query: "A\né🙂", budget: { unit: "unicode_scalars", limit: 8 } } }).success).toBe(true);
  expect(RpcRequest.safeParse({ jsonrpc: "2.0", id: 2, method: "embedding.requeue", params: { limit: 5, reasons: ["provider_unavailable_exhausted"] } }).success).toBe(true);
  expect(RpcRequest.safeParse({ jsonrpc: "2.0", id: 3, method: "embedding.requeue", params: { limit: 0 } }).success).toBe(false);
  expect(RpcRequest.safeParse({ jsonrpc: "2.0", id: 4, method: "embedding.recover", params: { operation_id: id(1), fact_id: id(2) } }).success).toBe(true);
  expect(RpcRequest.safeParse({ jsonrpc: "2.0", id: 5, method: "embedding.recover", params: { operation_id: id(1), episode_id: id(2) } }).success).toBe(false);
  const recall: RpcRecallParams = { query: "context" };
  expect(RpcRecallParams.parse(recall).window).toBe(2);
  for (const window of [0, 1, 8]) expect(RpcRecallParams.parse({ query: "context", window }).window).toBe(window);
  for (const window of [-1, 1.5, 9]) expect(RpcRecallParams.safeParse({ query: "context", window }).success).toBe(false);
});

test("attempt rows carry the deferred state, the exhausted reason and transport detail; older rows parse without detail", () => {
  const base = { operation_id: id(1), fact_id: id(2), profile_id: "a".repeat(64), model: "wiring-fixture", model_incarnation: "b".repeat(64),
    dimensions: 2, input_revision: "c".repeat(64), input_digest: "d".repeat(64), created_at: 1, completed_at: 2 };
  expect(RpcEmbeddingAttempt.parse({ ...base, state: "deferred", reason: "provider_unavailable", detail: "http 503" })).toMatchObject({ state: "deferred", detail: "http 503" });
  expect(RpcEmbeddingAttempt.parse({ ...base, state: "quarantined", reason: "provider_unavailable_exhausted", detail: "timeout 30000ms" }).reason).toBe("provider_unavailable_exhausted");
  expect(RpcEmbeddingAttempt.parse({ ...base, state: "quarantined", reason: "provider_unavailable" }).detail).toBeNull();
  expect(RpcEmbeddingAttempt.safeParse({ ...base, state: "quarantined", reason: "provider_unavailable", detail: "x".repeat(257) }).success).toBe(false);
});

test("the HTTP provider classifies by the failing condition: a 503 is deferred, a 400 is terminal, a dead socket is unavailable", async () => {
  // A temporary outage (503) must defer within the transient budget, never quarantine; a deterministic 400 is terminal.
  for (const [status, reason] of [[503, "provider_unavailable"], [429, "provider_unavailable"], [400, "provider_rejected"], [404, "provider_rejected"]] as const) {
    const server = Bun.serve({ port: 0, hostname: "127.0.0.1", fetch: () => new Response("nope", { status }) });
    try {
      const error = await new HttpEmbeddingProvider({ endpoint: `http://127.0.0.1:${server.port}/v1/embeddings`, profile, timeout_ms: 5000 }).embed("hello", "document").then(() => null, error => error);
      expect(error).toBeInstanceOf(EmbeddingError);
      expect(error).toMatchObject({ reason, detail: `http ${status}` });
    } finally { await server.stop(true); }
  }
  expect([408, 425, 429, 500, 502, 503, 504].map(httpStatusReason)).toEqual(Array<"provider_unavailable">(7).fill("provider_unavailable"));
  expect([400, 401, 403, 404, 413, 422].map(httpStatusReason)).toEqual(Array<"provider_rejected">(6).fill("provider_rejected"));
  const closed = Bun.serve({ port: 0, hostname: "127.0.0.1", fetch: () => new Response("unused") }), port = closed.port;
  await closed.stop(true);
  const unavailable = await new HttpEmbeddingProvider({ endpoint: `http://127.0.0.1:${port}/v1/embeddings`, profile, timeout_ms: 5000 }).embed("hello", "document").then(() => null, error => error);
  expect(unavailable).toBeInstanceOf(EmbeddingError);
  expect(unavailable).toMatchObject({ reason: "provider_unavailable" });
  expect(typeof unavailable.detail).toBe("string");
  expect(unavailable.detail.length).toBeGreaterThan(0);
  const oversized = await new HttpEmbeddingProvider({ endpoint: `http://127.0.0.1:${port}/v1/embeddings`, profile, timeout_ms: 5000 }).embed("x".repeat(8193), "document").then(() => null, error => error);
  expect(oversized).toMatchObject({ reason: "input_too_large", detail: "8193 bytes > 8192" });
  expect(() => validateVector([1, 0, 0], profile)).toThrow(expect.objectContaining({ reason: "invalid_vector", detail: "dimensions" }));
  expect(() => validateVector([2, 0], profile)).toThrow(expect.objectContaining({ reason: "invalid_vector", detail: "norm" }));
});
const id = (n: number) => `0192f3a1-5e7b-7c3d-9f21-${String(n).padStart(12, "0")}`;
const profile = EmbeddingProfile.parse({ model: "wiring-fixture", model_incarnation: "a".repeat(64), dimensions: 2,
  document_prefix: "", query_prefix: "", max_input_bytes: 8192, norm: "unit_l2", norm_tolerance: 0.001 });
function item(n: number, content = "A\né🙂"): RpcRecallItem {
  return { id: id(n), kind: "Episode", schema: "anamnesis.original-message/1", epistemic: "observed", content,
    time: { value: "2026-09-01T00:00:00Z", precision: "second" }, score: 0.01, relevance: 0.01, mass: 1, utility: 0,
    sources: [id(n)], provenance: { derived_from: [{ id: id(n), kind: "Episode", visible_at_T: true }], supersedes: [],
      supersedes_redacted: false, contrasts: [], warnings: [] }, channels: ["bm25"] };
}
function base(budget: RpcOutputBudget): RpcRecallResult {
  return { recall_id: id(999), expires_at: 100, results: [], companions: [], entities: [], context_text: "", used_budget: 0,
    renderer: "canonical-jsonl-v1", budget, diagnostics: { pipeline: "derived-hybrid-v1", now: 0, T: 0, policy_revision: 0,
      channels_used: ["bm25"], vector_reason: "not_configured", embedding_profile_id: null, candidate_count: 2, skipped_bundles: 0,
      ppr_used: false, identity_mode: "exact_episode_id" } };
}

test("model incarnation, dimensions, finite unit norm and profile templates are strict", () => {
  expect(validateVector([1, 0], profile)).toEqual([1, 0]);
  for (const vector of [[1], [1, 0, 0], [0, 0], [2, 0], [Infinity, 0], [NaN, 0], ["1", 0]]) expect(() => validateVector(vector, profile)).toThrow();
  for (const patch of [{ dimensions: 0 }, { dimensions: 1.5 }, { model_incarnation: "latest" }, { norm: "any" }, { norm_tolerance: 1 }, { extra: 1 }])
    expect(EmbeddingProfile.safeParse({ ...profile, ...patch }).success).toBe(false);
  expect(embeddingProfileId(profile)).not.toBe(embeddingProfileId({ ...profile, model_incarnation: "b".repeat(64) }));
  expect(embeddingProfileId(profile)).not.toBe(embeddingProfileId({ ...profile, query_prefix: "query: " }));
});
test("exact final UTF8/scalar budgets include source provenance, escapes and separators", () => {
  const bundles = [1, 2].map(n => ({ primary: item(n), companions: [], optional: [] }));
  for (const unit of ["utf8_bytes", "unicode_scalars"] as const) {
    const all = packRecall(bundles, base({ unit, limit: 65536 }), 2, new Map());
    const exact = unit === "utf8_bytes" ? Buffer.byteLength(all.context_text) : [...all.context_text].length;
    expect(all.used_budget).toBe(exact); expect(all.context_text).toBe(renderContext(all.results, all.companions));
    expect(all.context_text.split("\n")).toHaveLength(2);
    expect(packRecall(bundles, base({ unit, limit: exact }), 2, new Map()).results).toHaveLength(2);
    expect(packRecall(bundles, base({ unit, limit: exact - 1 }), 2, new Map()).results).toHaveLength(1);
    const zero = packRecall(bundles, base({ unit, limit: 0 }), 2, new Map());
    expect([zero.results, zero.companions, zero.context_text, zero.used_budget]).toEqual([[], [], "", 0]);
  }
  expect(countBudget("A\né🙂", "utf8_bytes")).toBe(8); expect(countBudget("A\né🙂", "unicode_scalars")).toBe(4);
  expect(() => Reflect.apply(countBudget, undefined, ["x", "bytes", () => 1])).toThrow();
  expect(() => countBudget("\ud800", "unicode_scalars")).toThrow();
});
test("oversized complete bundles are skipped, never stripped; companion promotion rerenders actual ranks", () => {
  const primary = item(1), peer = item(2);
  primary.provenance.contrasts = [peer.id];
  primary.provenance.warnings = [{ code: "supersedes_withheld", content: "required warning" }];
  const bundles = [{ primary, companions: [peer], optional: [] }, { primary: peer, companions: [], optional: [] }];
  const all = packRecall(bundles, base({ unit: "utf8_bytes", limit: 65536 }), 2, new Map());
  expect(all.companions).toEqual([]); expect(all.results.map(x => x.rank)).toEqual([0, 1]);
  expect(all.context_text.split("\n").map(line => JSON.parse(line).rank)).toEqual([0, 1]);
  const smallOnly = renderContext([{ ...peer, rank: 0 }], []);
  const skipped = packRecall(bundles, base({ unit: "utf8_bytes", limit: Buffer.byteLength(smallOnly) }), 2, new Map());
  expect(skipped.results.map(x => x.id)).toEqual([peer.id]); expect(skipped.diagnostics.skipped_bundles).toBe(1);
  const huge = packRecall([{ primary: item(3, "x".repeat(600000)), companions: [], optional: [] }, { primary: peer, companions: [], optional: [] }],
    base({ unit: "utf8_bytes", limit: 1048576 }), 2, new Map());
  expect(huge.results.map(x => x.id)).toEqual([peer.id]); expect(recallResponseBytes(huge)).toBeLessThanOrEqual(1048576);
  expect(() => packRecall([], { ...base({ unit: "utf8_bytes", limit: 1 }), context_text: "x".repeat(1048576) }, 0, new Map())).toThrow();
});
test("token budgets require installed pinned encoders and count the whole prospective string", () => {
  const tokenizer_id = "fixture-v1@sha256:" + "a".repeat(64), budget = { unit: "tokens" as const, limit: 4, tokenizer_id };
  expect(() => admittedBudget(budget, new Map())).toThrow();
  const inputs: string[] = [], tokenizers = new Map([[tokenizer_id, (text: string) => { inputs.push(text); return text.includes("\n") ? 4 : 3; }]]);
  const result = packRecall([1, 2].map(n => ({ primary: item(n), companions: [], optional: [] })), base(budget), 2, tokenizers);
  expect(result.results).toHaveLength(2); expect(result.used_budget).toBe(4); expect(inputs.at(-1)).toBe(result.context_text);
  expect(inputs).toHaveLength(2);
  for (const patch of [{ unit: "bytes" }, { limit: -1 }, { limit: 1.5 }, { limit: Infinity }, { limit: Number.MAX_SAFE_INTEGER + 1 }])
    expect(() => admittedBudget({ ...budget, ...patch }, tokenizers)).toThrow();
});

test("tokenizer identity requires a version/digest, never a bare hash", () => {
  expect(RpcOutputBudget.safeParse({ unit: "tokens", limit: 1, tokenizer_id: "a".repeat(64) }).success).toBe(false);
  expect(RpcOutputBudget.safeParse({ unit: "tokens", limit: 1, tokenizer_id: "fixture-v1@sha256:" + "a".repeat(64) }).success).toBe(true);
});

function factBundle() {
  const source = item(3);
  const primary: Extract<RpcRecallItem, { kind: "Fact" }> = {
    ...source, id: id(10), kind: "Fact", schema: "anamnesis.claim/1", epistemic: "derived",
    provenance: { ...source.provenance, derived_from: [{ id: source.id, kind: "Episode", visible_at_T: true }] },
    window: { source: source.id, before: [id(2), id(1)], after: [id(4), id(5)] }, channels: ["vector"],
  };
  const companions: RpcRecallItem[] = [];
  return { primary, companions, optional: [5, 1, 4, 2].map(n => item(n)) };
}

test("Fact bundles render the full window when the exact budget has room", () => {
  // Given four optional Episodes in farthest-first drop order.
  const bundle = factBundle();

  const result = packRecall([bundle], base({ unit: "utf8_bytes", limit: 65536 }), 1, new Map());

  expect(RpcRecallResult.safeParse(result).success).toBe(true);
  expect(result.results[0]).toMatchObject({ id: bundle.primary.id, window: bundle.primary.window });
  expect(result.companions.map(item => item.id)).toEqual([1, 2, 4, 5].map(id));
  expect(result.context_text.split("\n").map(line => JSON.parse(line).id)).toEqual([10, 2, 1, 4, 5].map(id));
  expect(result.used_budget).toBe(Buffer.byteLength(result.context_text));
  expect(result.context_text).toBe(renderContext(result.results, result.companions));
});

test("tight Fact budgets drop the farthest window Episode first and rewrite delivered IDs", () => {
  const tokenizer_id = "window-fixture@sha256:" + "a".repeat(64);
  const tokenizers = new Map([[tokenizer_id, (text: string) => [...text].length]]);
  for (const [before, after] of [[[2, 1], [4]], [[2], [4]], [[2], []], [[], []]] as const) {
    for (const unit of ["utf8_bytes", "unicode_scalars", "tokens"] as const) {
      const bundle = factBundle();
      const expected = { ...bundle.primary, rank: 0, window: { source: id(3), before: before.map(id), after: after.map(id) } };
      const companions = [...before, ...after].map(n => item(n));
      const text = renderContext([expected], companions);
      const limit = countBudget(text, unit, tokenizers.get(tokenizer_id));

      const budget: RpcOutputBudget = unit === "tokens" ? { unit, limit, tokenizer_id } : { unit, limit };
      const result = packRecall([bundle], base(budget), 1, tokenizers);

      expect(result.results).toEqual([expected]);
      expect(result.companions.map(item => item.id)).toEqual(companions.map(item => item.id).sort());
      expect(result.context_text).toBe(text);
      expect(result.used_budget).toBe(limit);
      expect(result.diagnostics.skipped_bundles).toBe(0);
      expect(bundle.primary.window).toEqual({ source: id(3), before: [id(2), id(1)], after: [id(4), id(5)] });
    }
  }
});

test("a Fact whose mandatory bundle cannot fit is skipped without leaking its window", () => {
  const bundle = factBundle(), peer = item(6, "mandatory contrast".repeat(100)), later = item(7);
  bundle.primary.provenance.contrasts = [peer.id];
  bundle.primary.provenance.warnings = [{ code: "supersedes_incomplete", content: "Required provenance warning." }];
  bundle.companions = [peer];
  const primaryOnly = { ...bundle.primary, rank: 0, window: { source: id(3), before: [], after: [] },
    provenance: { ...bundle.primary.provenance, contrasts: [] } };
  const limit = Buffer.byteLength(renderContext([primaryOnly], []));

  const result = packRecall([bundle, { primary: later, companions: [], optional: [] }],
    base({ unit: "utf8_bytes", limit }), 2, new Map());

  expect(result.results).toEqual([{ ...later, rank: 0 }]);
  expect(result.companions).toEqual([]);
  expect(result.context_text).toBe(renderContext(result.results, []));
  expect(result.diagnostics.skipped_bundles).toBe(1);
  const empty = packRecall([bundle], base({ unit: "utf8_bytes", limit }), 1, new Map());
  expect([empty.results, empty.companions, empty.context_text, empty.used_budget]).toEqual([[], [], "", 0]);
});
