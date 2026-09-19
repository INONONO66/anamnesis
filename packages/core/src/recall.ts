import { RpcOutputBudget, RPC_LIMITS, type RpcRecallItem, type RpcRecallResult } from "../../protocol/src/rpc.ts";
import { countBudget } from "./dynamics/budget.ts";

export class RecallError extends Error {
  constructor(readonly code: "invalid_budget" | "resource_exhausted" | "embedding_not_configured" | "receipt_unavailable") { super(code); }
}
export type Tokenizers = ReadonlyMap<string, (text: string) => number>;
export function admittedBudget(input: unknown, tokenizers: Tokenizers, defaultBytes = 65536): RpcOutputBudget {
  const parsed = RpcOutputBudget.safeParse(input ?? { unit: "utf8_bytes", limit: defaultBytes });
  if (!parsed.success || (parsed.data.unit === "tokens" && !tokenizers.has(parsed.data.tokenizer_id))) throw new RecallError("invalid_budget");
  return parsed.data;
}
/** RFC 8785 serialization for already JSON-only renderer values. */
export function canonicalContext(value: unknown): string {
  if (typeof value === "string") countBudget(value, "unicode_scalars");
  if (typeof value === "number" && !Number.isFinite(value)) throw new RangeError("nonfinite JSON number");
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalContext).join(",")}]`;
  return `{${Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)
    .map(([key, member]) => `${canonicalContext(key)}:${canonicalContext(member)}`).join(",")}}`;
}
export function renderContext(results: RpcRecallItem[], companions: RpcRecallItem[]): string {
  const byId = new Map([...companions, ...results].map(item => [item.id, item]));
  const emitted = new Set<string>(), records: string[] = [];
  const emit = (id: string) => {
    if (emitted.has(id)) return;
    const item = byId.get(id); if (!item) throw new Error("missing mandatory companion");
    emitted.add(id); records.push(canonicalContext(item));
  };
  for (const item of results) { emit(item.id); for (const id of [...item.provenance.contrasts].sort()) emit(id); }
  return records.join("\n");
}
export type RecallBundle = { primary: RpcRecallItem; companions: RpcRecallItem[] };
/** Full response reserve includes the maximum request-ID escaping and all unknown
 * envelope numbers. Actual wire encoding is checked again before socket write. */
export function recallResponseBytes(result: RpcRecallResult): number {
  return Buffer.byteLength(JSON.stringify({ jsonrpc: "2.0", method: "recall", id: "\u0000".repeat(128),
    server_time: Number.MAX_SAFE_INTEGER, structure_revision: Number.MAX_SAFE_INTEGER,
    policy_revision: Number.MAX_SAFE_INTEGER, result }));
}
export function packRecall(bundles: RecallBundle[], base: RpcRecallResult, limit: number, tokenizers: Tokenizers): RpcRecallResult {
  if (!Number.isSafeInteger(limit) || limit < 0 || limit > 64 || bundles.length > 177) throw new RangeError("unbounded recall");
  const budget = admittedBudget(base.budget, tokenizers);
  const encode = budget.unit === "tokens" ? tokenizers.get(budget.tokenizer_id) : undefined;
  let result = base;
  if (recallResponseBytes(result) > RPC_LIMITS.frame_bytes) throw new RecallError("resource_exhausted");
  if (limit === 0 || budget.limit === 0) return result;
  let skipped = 0;
  for (const bundle of bundles) {
    if (result.results.length === limit) break;
    if (result.results.some(item => item.id === bundle.primary.id)) continue;
    const results = [...result.results, { ...bundle.primary, rank: result.results.length }];
    const companions = [...new Map([...result.companions, ...bundle.companions].map(item => [item.id, item])).values()]
      .filter(item => !results.some(primary => primary.id === item.id)).sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
    const context_text = renderContext(results, companions);
    if (companions.length > 256 || Buffer.byteLength(context_text) > RPC_LIMITS.frame_bytes) { skipped++; continue; }
    const used_budget = countBudget(context_text, budget.unit, encode);
    const prospective = { ...result, results, companions, context_text, used_budget,
      diagnostics: { ...result.diagnostics, skipped_bundles: 177 } }; // reserve final counter width
    if (used_budget > budget.limit || recallResponseBytes(prospective) > RPC_LIMITS.frame_bytes) { skipped++; continue; }
    result = prospective;
  }
  return { ...result, diagnostics: { ...result.diagnostics, skipped_bundles: skipped } };
}
