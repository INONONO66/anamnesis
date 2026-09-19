import { expect, test } from "bun:test";
import { Store } from "./store.ts";
import { EmbeddingProfile, embeddingProfileId, validateVector } from "./embedding.ts";
import { admittedBudget, packRecall, renderContext, recallResponseBytes, canonicalContext } from "./recall.ts";
import { createHash } from "node:crypto";
import { receiptBodyDigestInput, canonicalReceiptJson } from "./receipt-digest.ts";
import { countBudget } from "./dynamics/budget.ts";
import { RpcRecallResult, RpcRecallItem, RpcRequest, RpcOutputBudget } from "../../protocol/src/rpc.ts";

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
  expect(RpcRequest.safeParse({ jsonrpc: "2.0", id: 1, method: "recall", params: { query: "A\né🙂", budget: { unit: "unicode_scalars", limit: 8 } } }).success).toBe(true);
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
    renderer: "canonical-jsonl-v1", budget, diagnostics: { pipeline: "originals-hybrid-v1", now: 0, T: 0, policy_revision: 0,
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
  const bundles = [1, 2].map(n => ({ primary: item(n), companions: [] }));
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
  const bundles = [{ primary, companions: [peer] }, { primary: peer, companions: [] }];
  const all = packRecall(bundles, base({ unit: "utf8_bytes", limit: 65536 }), 2, new Map());
  expect(all.companions).toEqual([]); expect(all.results.map(x => x.rank)).toEqual([0, 1]);
  expect(all.context_text.split("\n").map(line => JSON.parse(line).rank)).toEqual([0, 1]);
  const smallOnly = renderContext([{ ...peer, rank: 0 }], []);
  const skipped = packRecall(bundles, base({ unit: "utf8_bytes", limit: Buffer.byteLength(smallOnly) }), 2, new Map());
  expect(skipped.results.map(x => x.id)).toEqual([peer.id]); expect(skipped.diagnostics.skipped_bundles).toBe(1);
  const huge = packRecall([{ primary: item(3, "x".repeat(600000)), companions: [] }, { primary: peer, companions: [] }],
    base({ unit: "utf8_bytes", limit: 1048576 }), 2, new Map());
  expect(huge.results.map(x => x.id)).toEqual([peer.id]); expect(recallResponseBytes(huge)).toBeLessThanOrEqual(1048576);
  expect(() => packRecall([], { ...base({ unit: "utf8_bytes", limit: 1 }), context_text: "x".repeat(1048576) }, 0, new Map())).toThrow();
});
test("token budgets require installed pinned encoders and count the whole prospective string", () => {
  const tokenizer_id = "fixture-v1@sha256:" + "a".repeat(64), budget = { unit: "tokens" as const, limit: 4, tokenizer_id };
  expect(() => admittedBudget(budget, new Map())).toThrow();
  const inputs: string[] = [], tokenizers = new Map([[tokenizer_id, (text: string) => { inputs.push(text); return text.includes("\n") ? 4 : 3; }]]);
  const result = packRecall([1, 2].map(n => ({ primary: item(n), companions: [] })), base(budget), 2, tokenizers);
  expect(result.results).toHaveLength(2); expect(result.used_budget).toBe(4); expect(inputs.at(-1)).toBe(result.context_text);
  expect(inputs).toHaveLength(2);
  for (const patch of [{ unit: "bytes" }, { limit: -1 }, { limit: 1.5 }, { limit: Infinity }, { limit: Number.MAX_SAFE_INTEGER + 1 }])
    expect(() => admittedBudget({ ...budget, ...patch }, tokenizers)).toThrow();
});

test("tokenizer identity requires a version/digest, never a bare hash", () => {
  expect(RpcOutputBudget.safeParse({ unit: "tokens", limit: 1, tokenizer_id: "a".repeat(64) }).success).toBe(false);
  expect(RpcOutputBudget.safeParse({ unit: "tokens", limit: 1, tokenizer_id: "fixture-v1@sha256:" + "a".repeat(64) }).success).toBe(true);
});
