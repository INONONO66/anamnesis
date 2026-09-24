import { z } from "zod";
import { countBudget } from "./dynamics/budget.ts";
import { EmbeddingConfig, EmbeddingError, httpStatusReason, transportDetail, validateVector, type EmbeddingProfile, type EmbeddingProvider } from "./embedding.ts";

export type OpenAiEmbeddingProviderOptions = {
  baseUrl: string;
  apiKey?: string;
  model: string;
  profile: EmbeddingProfile;
  fetch?: (input: string | URL | Request, init?: RequestInit) => Promise<Response>;
  timeoutMs?: number;
};
export type OpenAiEmbeddingResult = number[] & { readonly meta: { readonly normalized: boolean } };

/** Keep the existing store/RPC reason vocabulary while exposing OpenAI adapter codes. */
class OpenAiEmbeddingError extends EmbeddingError {
  readonly retryable: boolean;
  constructor(readonly code: EmbeddingError["reason"] | "provider_mismatch", detail: string | null = null) {
    super(code === "provider_mismatch" ? "profile_mismatch" : code, detail);
    this.message = code;
    this.retryable = code === "provider_unavailable";
  }
}

/** OpenAI-compatible transport; the operator's profile pins the model identity.
 * Normalization metadata belongs to each returned array, not shared provider state. */
export class OpenAiEmbeddingProvider implements EmbeddingProvider {
  readonly profile: EmbeddingProfile;
  private readonly config: EmbeddingConfig;
  private readonly apiKey: string | undefined;
  private readonly fetch: NonNullable<OpenAiEmbeddingProviderOptions["fetch"]>;

  constructor(options: OpenAiEmbeddingProviderOptions) {
    this.config = EmbeddingConfig.parse({ endpoint: `${options.baseUrl.replace(/\/+$/, "")}/v1/embeddings`,
      profile: options.profile, timeout_ms: options.timeoutMs });
    if (options.model !== this.config.profile.model) throw new TypeError("model must match the embedding profile");
    this.profile = Object.freeze(this.config.profile);
    countBudget(this.profile.document_prefix, "utf8_bytes");
    countBudget(this.profile.query_prefix, "utf8_bytes");
    this.apiKey = options.apiKey;
    this.fetch = options.fetch ?? globalThis.fetch;
  }

  async embed(text: string, purpose: "document" | "query"): Promise<OpenAiEmbeddingResult> {
    const input = (purpose === "document" ? this.profile.document_prefix : this.profile.query_prefix) + text;
    const bytes = countBudget(input, "utf8_bytes");
    if (bytes > this.profile.max_input_bytes) throw new OpenAiEmbeddingError("input_too_large", `${bytes} bytes > ${this.profile.max_input_bytes}`);
    const headers: Record<string, string> = { "content-type": "application/json" };
    if (this.apiKey !== undefined) headers["authorization"] = `Bearer ${this.apiKey}`;
    const signal = AbortSignal.timeout(this.config.timeout_ms);
    const chunks: Uint8Array[] = [];
    try {
      const response = await this.fetch(this.config.endpoint, { method: "POST", headers, signal, redirect: "error",
        body: JSON.stringify({ model: this.profile.model, input: [input] }) });
      if (!response.ok) {
        await response.body?.cancel();
        throw new OpenAiEmbeddingError(httpStatusReason(response.status), `http ${response.status}`);
      }
      if (!response.body) throw new OpenAiEmbeddingError("provider_mismatch", "empty body");
      const reader = response.body.getReader();
      let received = 0;
      try {
        for (;;) {
          const next = await reader.read();
          if (next.done) break;
          received += next.value.byteLength;
          if (received > 256 * 1024) {
            await reader.cancel();
            throw new OpenAiEmbeddingError("provider_mismatch", "body over 256 KiB");
          }
          chunks.push(next.value);
        }
      } finally { reader.releaseLock(); }
    } catch (error) {
      if (error instanceof OpenAiEmbeddingError) throw error;
      throw new OpenAiEmbeddingError("provider_unavailable", transportDetail(error, this.config.timeout_ms));
    }
    let body: unknown;
    try { body = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(chunks))); }
    catch { throw new OpenAiEmbeddingError("provider_mismatch", "invalid json"); }
    const envelope = z.object({ data: z.array(z.object({ embedding: z.unknown() })).min(1) }).safeParse(body);
    if (!envelope.success || envelope.data.data[0]!.embedding === undefined) throw new OpenAiEmbeddingError("provider_mismatch", "envelope");
    const parsed = z.array(z.number()).length(this.profile.dimensions).safeParse(envelope.data.data[0]!.embedding);
    if (!parsed.success) throw new OpenAiEmbeddingError("invalid_vector", "dimensions");
    const norm = Math.hypot(...parsed.data);
    if (!Number.isFinite(norm) || norm === 0) throw new OpenAiEmbeddingError("invalid_vector", "norm");
    const normalized = Math.abs(norm - 1) > this.profile.norm_tolerance;
    let vector: number[];
    try { vector = validateVector(normalized ? parsed.data.map(value => value / norm) : parsed.data, this.profile); }
    catch (error) {
      if (error instanceof EmbeddingError) throw new OpenAiEmbeddingError(error.reason, error.detail);
      throw error;
    }
    const result = Object.assign(vector, { meta: Object.freeze({ normalized }) });
    Object.defineProperty(result, "meta", { enumerable: false, writable: false });
    return result;
  }
}
