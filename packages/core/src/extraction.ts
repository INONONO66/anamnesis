import { createHash } from "node:crypto";
import { z } from "zod";
import { canonicalExtractionBody, extractionBodyDigest, ExtractionModelOutput, ExtractionOutput, ExtractionSpan, type ExtractionFailureDetail } from "../../protocol/src/extraction.ts";

export const ExtractionProviderConfig = z.strictObject({ endpoint: z.url().refine(v => ["http:", "https:"].includes(new URL(v).protocol)), model: z.string().min(1).max(256), model_incarnation: z.string().regex(/^[0-9a-f]{64}$/), timeout_ms: z.number().int().min(1).max(30000).default(5000) });
export type ExtractionProviderConfig = z.infer<typeof ExtractionProviderConfig>;
/** `judge_relations` compares one validated new Fact (`text`) against the
 * bounded candidates in `relation_context`; no Episode text is supplied. */
export type ExtractionProviderInput = { text: string; task: "claim" | "judge" | "judge_claims" | "judge_relations";
  claim_context?: import('../../protocol/src/extraction-audit.ts').ExtractionClaimContext;
  relation_context?: import('../../protocol/src/extraction-audit.ts').FactRelationContext };
export interface ExtractionProvider { readonly model: string; readonly modelIncarnation: string; extract(input: ExtractionProviderInput): Promise<unknown>; }
/** `detail` names the check a `provider_mismatch` failed; it is recorded on the attempt (#218). */
export class ExtractionProviderError extends Error {
  constructor(readonly reason: "provider_unavailable" | "provider_rejected" | "provider_mismatch" | "output_too_large" | "input_too_large", readonly detail?: ExtractionFailureDetail) { super(reason); }
}
const envelope = z.strictObject({ model: z.string(), model_incarnation: z.string(), output: z.unknown() });
export class HttpExtractionProvider implements ExtractionProvider {
  readonly model: string;
  readonly modelIncarnation: string;
  private readonly config: ExtractionProviderConfig;
  constructor(config: ExtractionProviderConfig) {
    this.config = ExtractionProviderConfig.parse(config);
    this.model = this.config.model; this.modelIncarnation = this.config.model_incarnation;
  }
  async extract(input: ExtractionProviderInput): Promise<unknown> {
    if (Buffer.byteLength(input.text, "utf8") > 65536) throw new ExtractionProviderError("input_too_large");
    const requestBody = JSON.stringify({ model: this.model, model_incarnation: this.modelIncarnation, ...input });
    if (Buffer.byteLength(requestBody, "utf8") > 262144) throw new ExtractionProviderError("input_too_large");
    const signal = AbortSignal.timeout(this.config.timeout_ms);
    let response: Response;
    try {
      response = await fetch(this.config.endpoint, { method: "POST", signal, headers: { "content-type": "application/json" }, redirect: "error", body: requestBody });
    } catch { throw new ExtractionProviderError("provider_unavailable"); }
    if (!response.ok) {
      await response.body?.cancel();
      throw new ExtractionProviderError(response.status === 429 || response.status >= 500 ? "provider_unavailable" : "provider_rejected");
    }
    let body: unknown;
    try {
      if (!response.body) throw new ExtractionProviderError("provider_rejected");
      const reader = response.body.getReader();
      try {
        const chunks: Uint8Array[] = []; let total = 0;
        for (;;) {
          const part = await reader.read();
          if (part.done) break;
          total += part.value.byteLength;
          if (total > 65536) { await reader.cancel(); throw new ExtractionProviderError("output_too_large"); }
          chunks.push(part.value);
        }
        body = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(chunks, total)));
      } finally { reader.releaseLock(); }
    } catch (error) {
      if (signal.aborted) throw new ExtractionProviderError("provider_unavailable");
      if (error instanceof ExtractionProviderError) throw error;
      throw new ExtractionProviderError("provider_rejected");
    }
    const parsed = envelope.safeParse(body);
    if (!parsed.success) throw new ExtractionProviderError("provider_mismatch", "envelope");
    if (parsed.data.model !== this.model) throw new ExtractionProviderError("provider_mismatch", "model");
    if (parsed.data.model_incarnation !== this.modelIncarnation) throw new ExtractionProviderError("provider_mismatch", "incarnation");
    validateModelOutput(parsed.data.output, input.task);
    return parsed.data.output;
  }
}
/** Wiring-only deterministic provider; never advertised as extraction capability. */
export class DeterministicExtractionProvider implements ExtractionProvider {
  readonly model = "fixture";
  readonly modelIncarnation = createHash("sha256").update("fixture-v1").digest("hex");
  async extract(input: ExtractionProviderInput): Promise<unknown> {
    if (input.task === "judge_claims") return { task: "judge_claims", claim_body_digest: input.claim_context?.body_digest,
      decisions: input.claim_context?.claims.map((claim, claim_index) => ({ claim_index, disposition: "unknown", evidence: claim.evidence })), language: "und", modality: "unknown" };
    return input.task === "claim" ? { task: "claim", claims: [], language: "und", modality: "unknown" }
      : { task: "judge", disposition: "unknown", spans: [], language: "und", modality: "unknown" };
  }
}
export function validateProviderOutput(output: unknown): { canonical_body: string; body_digest: string } {
  const canonical_body = canonicalExtractionBody(output);
  if (Buffer.byteLength(canonical_body, "utf8") > 65536) throw new ExtractionProviderError("output_too_large");
  return { canonical_body, body_digest: extractionBodyDigest(output) };
}
export function validateModelOutput(value: unknown, task: ExtractionProviderInput["task"]) {
  // Canonical domain/depth admission precedes recursive ABI validation.
  let encoded: ReturnType<typeof validateProviderOutput>;
  try { encoded = validateProviderOutput(value); }
  catch (error) { if (error instanceof ExtractionProviderError) throw error; throw new ExtractionProviderError("provider_mismatch", "json"); }
  const parsed = ExtractionModelOutput.safeParse(value);
  if (!parsed.success || parsed.data.task !== task) throw new ExtractionProviderError("provider_mismatch", "normalize");
  const body = parsed.data;
  const spans = body.task === "claim" ? body.claims.map(claim => claim.evidence) : body.task === "judge_claims" ? body.decisions.map(decision => decision.evidence) : body.task === "judge_relations" ? [] : body.spans;
  // A mixed per-claim audit never elects a semantic winner; relation verdicts are not dispositions either.
  const disposition = body.task === "claim" ? body.claims.length ? "retain" : "suppress" : body.task === "judge_claims" || body.task === "judge_relations" ? "unknown" : body.disposition;
  if ((disposition === "retain" || disposition === "correct") && spans.length === 0) throw new ExtractionProviderError("provider_mismatch", "normalize");
  return { output: ExtractionOutput.parse({ ...encoded, spans, language: body.language, modality: body.modality }), disposition, spans };
}
export function validateSourceSpans(content: string, spans: z.infer<typeof ExtractionSpan>[]): void {
  const bytes = Buffer.from(content, "utf8");
  const decoder = new TextDecoder("utf-8", { fatal: true });
  for (const input of spans) {
    const span = ExtractionSpan.parse(input);
    if (span.end > bytes.length) throw new Error("span_mismatch");
    let text: string;
    try { text = decoder.decode(bytes.subarray(span.start, span.end)); }
    catch { throw new Error("span_mismatch"); }
    if (text !== span.text || !bytes.subarray(span.start, span.end).equals(Buffer.from(span.text, "utf8"))) throw new Error("span_mismatch");
  }
}
