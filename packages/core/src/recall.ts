import { RpcOutputBudget, RPC_LIMITS, type RpcRecallItem, type RpcRecallResult } from "@anamnesis/protocol";
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
  for (const item of results) {
    emit(item.id);
    for (const id of [...item.provenance.contrasts].sort()) emit(id);
    if (item.kind === "Fact") for (const id of [...item.window.before, ...item.window.after]) emit(id);
  }
  return records.join("\n");
}
export type RecallBundle = { primary: RpcRecallItem; companions: RpcRecallItem[]; optional: RpcRecallItem[] };
/** Full response reserve includes the maximum request-ID escaping and all unknown
 * envelope numbers. Actual wire encoding is checked again before socket write. */
export function recallResponseBytes(result: RpcRecallResult): number {
  return Buffer.byteLength(JSON.stringify({ jsonrpc: "2.0", method: "recall", id: "\u0000".repeat(128),
    server_time: Number.MAX_SAFE_INTEGER, structure_revision: Number.MAX_SAFE_INTEGER,
    policy_revision: Number.MAX_SAFE_INTEGER, result }));
}
/** Render the actual deduplicated delivery, including only window IDs that survived packing. */
function prospectiveRecall(result: RpcRecallResult, bundle: RecallBundle, optional: RpcRecallItem[], budget: RpcOutputBudget,
  encode: ((text: string) => number) | undefined): RpcRecallResult | null {
  let primary = { ...bundle.primary, rank: result.results.length };
  const results = [...result.results, primary];
  const companions = [...new Map([...result.companions, ...bundle.companions, ...optional].map(item => [item.id, item])).values()]
    .filter(item => !results.some(itemPrimary => itemPrimary.id === item.id)).sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
  if (companions.length > 256) return null;
  if (primary.kind === "Fact") {
    const delivered = new Set([...results, ...companions].map(item => item.id));
    primary = { ...primary, window: { source: primary.window.source,
      before: primary.window.before.filter(id => delivered.has(id)), after: primary.window.after.filter(id => delivered.has(id)) } };
    results[results.length - 1] = primary;
  }
  const context_text = renderContext(results, companions);
  if (Buffer.byteLength(context_text) > RPC_LIMITS.frame_bytes) return null;
  const used_budget = countBudget(context_text, budget.unit, encode);
  const prospective = { ...result, results, companions, context_text, used_budget,
    diagnostics: { ...result.diagnostics, skipped_bundles: 177 } }; // reserve final counter width
  return used_budget > budget.limit || recallResponseBytes(prospective) > RPC_LIMITS.frame_bytes ? null : prospective;
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
    let prospective: RpcRecallResult | null = null;
    for (let dropped = 0; dropped <= bundle.optional.length; dropped++) {
      prospective = prospectiveRecall(result, bundle, bundle.optional.slice(dropped), budget, encode);
      if (prospective) break;
    }
    if (prospective) result = prospective;
    else skipped++;
  }
  return { ...result, diagnostics: { ...result.diagnostics, skipped_bundles: skipped } };
}
