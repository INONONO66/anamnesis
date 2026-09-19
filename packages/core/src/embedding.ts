import { createHash } from "node:crypto";
import { z } from "zod";
import { countBudget } from "./dynamics/budget.ts";

const hash = z.string().regex(/^[0-9a-f]{64}$/);
const text = z.string().min(1).max(1024).refine(value => !/[\uD800-\uDFFF]/u.test(value), "malformed Unicode");
/** Incarnation identifies frozen weights/runtime/pooling/templates, not a mutable alias.
 * The provider must echo this digest; an operator remains responsible for its truth. */
export const EmbeddingProfile = z.strictObject({
  model: text, model_incarnation: hash, dimensions: z.number().int().min(1).max(4096),
  document_prefix: z.string().max(1024).refine(value => !/[\uD800-\uDFFF]/u.test(value), "malformed Unicode"),
  query_prefix: z.string().max(1024).refine(value => !/[\uD800-\uDFFF]/u.test(value), "malformed Unicode"),
  max_input_bytes: z.number().int().min(1).max(65536),
  norm: z.literal("unit_l2"), norm_tolerance: z.number().positive().max(0.01),
});
export type EmbeddingProfile = z.infer<typeof EmbeddingProfile>;
export const EmbeddingConfig = z.strictObject({
  endpoint: z.url().refine(value => ["http:", "https:"].includes(new URL(value).protocol)),
  profile: EmbeddingProfile,
  timeout_ms: z.number().int().min(1).max(30000).default(5000),
});
export type EmbeddingConfig = z.infer<typeof EmbeddingConfig>;
export const embeddingProfileId = (profile: EmbeddingProfile): string => createHash("sha256").update(JSON.stringify(
  Object.fromEntries(Object.entries(EmbeddingProfile.parse(profile)).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)),
)).digest("hex");
export class EmbeddingError extends Error {
  constructor(readonly reason: "provider_unavailable" | "provider_rejected" | "profile_mismatch" | "invalid_vector" | "input_too_large") { super(reason); }
}
export function validateVector(value: unknown, profile: EmbeddingProfile): number[] {
  const parsed = z.array(z.number().finite()).length(profile.dimensions).safeParse(value);
  if (!parsed.success) throw new EmbeddingError("invalid_vector");
  const norm = Math.hypot(...parsed.data);
  if (!Number.isFinite(norm) || Math.abs(norm - 1) > profile.norm_tolerance) throw new EmbeddingError("invalid_vector");
  return parsed.data; // Never truncate, pad or normalize a rejected vector.
}
export interface EmbeddingProvider {
  readonly profile: EmbeddingProfile;
  embed(text: string, purpose: "document" | "query"): Promise<number[]>;
}
/** Explicit configured HTTP contract. No built-in hash/text fixture is a semantic provider.
 * Compatible servers/proxies echo model + model_incarnation and accept truncate:false.
 * Bounded streamed reads apply even when a provider omits Content-Length. */
export class HttpEmbeddingProvider implements EmbeddingProvider {
  readonly profile: EmbeddingProfile;
  private readonly config: EmbeddingConfig;
  constructor(input: EmbeddingConfig) {
    this.config = EmbeddingConfig.parse(input); this.profile = Object.freeze(this.config.profile);
    countBudget(this.profile.document_prefix, "utf8_bytes"); countBudget(this.profile.query_prefix, "utf8_bytes");
  }
  async embed(text: string, purpose: "document" | "query"): Promise<number[]> {
    const input = (purpose === "document" ? this.profile.document_prefix : this.profile.query_prefix) + text;
    if (countBudget(input, "utf8_bytes") > this.profile.max_input_bytes) throw new EmbeddingError("input_too_large");
    let body: unknown;
    try {
      const response = await fetch(this.config.endpoint, { method: "POST", signal: AbortSignal.timeout(this.config.timeout_ms),
        headers: { "content-type": "application/json" }, body: JSON.stringify({ input, model: this.profile.model,
          model_incarnation: this.profile.model_incarnation, dimensions: this.profile.dimensions, truncate: false }) });
      if (!response.ok) { await response.body?.cancel(); throw new EmbeddingError("provider_rejected"); }
      if (!response.body) throw new EmbeddingError("provider_rejected");
      const reader = response.body.getReader(), chunks: Uint8Array[] = [];
      let bytes = 0;
      try {
        for (;;) {
          const next = await reader.read(); if (next.done) break;
          bytes += next.value.length;
          if (bytes > 256 * 1024) { await reader.cancel(); throw new EmbeddingError("provider_rejected"); }
          chunks.push(next.value);
        }
      } finally { reader.releaseLock(); }
      body = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(chunks)));
    } catch (error) {
      if (error instanceof EmbeddingError) throw error;
      if (error instanceof SyntaxError) throw new EmbeddingError("provider_rejected");
      throw new EmbeddingError("provider_unavailable");
    }
    const parsed = z.object({ model: z.string(), model_incarnation: hash,
      data: z.array(z.object({ index: z.literal(0), embedding: z.unknown() })).length(1) }).safeParse(body);
    if (!parsed.success || parsed.data.model !== this.profile.model || parsed.data.model_incarnation !== this.profile.model_incarnation)
      throw new EmbeddingError("profile_mismatch");
    return validateVector(parsed.data.data[0]!.embedding, this.profile);
  }
}
