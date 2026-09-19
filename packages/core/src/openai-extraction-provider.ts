import { createHash } from "node:crypto";
import { z } from "zod";
import {
  ExtractionProviderError,
  type ExtractionProvider,
  type ExtractionProviderInput,
} from "./extraction.ts";
import { chatExtractionSchema, normalizeChatExtraction } from "./extraction-chat-output.ts";

export type ExtractionDialect = "openai_chat" | "anthropic_messages";
export type OpenAiChatExtractionProviderOptions = {
  dialect?: ExtractionDialect;
  baseUrl: string;
  apiKey: string;
  model: string;
  systemPrompt: string;
  fetch?: (input: string | URL | Request, init?: RequestInit) => Promise<Response>;
  timeoutMs?: number;
};

class OpenAiChatExtractionError extends ExtractionProviderError {
  readonly code: ExtractionProviderError["reason"];
  readonly retryable: boolean;
  constructor(readonly reason: ExtractionProviderError["reason"]) {
    super(reason);
    this.code = reason;
    this.retryable = reason === "provider_unavailable";
  }
}

const responseEnvelope = z.looseObject({
  model: z.string().min(1),
  system_fingerprint: z.string().nullable().optional(),
  choices: z.array(z.looseObject({ message: z.looseObject({ content: z.string() }) })).min(1),
});
const anthropicEnvelope = z.looseObject({
  model: z.string().min(1),
  content: z.array(z.looseObject({ type: z.literal("text"), text: z.string() })).min(1),
});
const errorEnvelope = z.looseObject({ error: z.looseObject({ code: z.string() }) });

/** Chat extraction with shared validation/error mapping for OpenAI and Anthropic. */
export class OpenAiChatExtractionProvider implements ExtractionProvider {
  readonly model: string;
  readonly modelIncarnation: string;
  reportedModelIncarnation: string | undefined;
  private readonly dialect: ExtractionDialect;
  private readonly endpoint: string;
  private readonly apiKey: string;
  private readonly systemPrompt: string;
  private readonly timeoutMs: number;
  private readonly fetch: NonNullable<OpenAiChatExtractionProviderOptions["fetch"]>;

  constructor(options: OpenAiChatExtractionProviderOptions) {
    let baseUrl: URL;
    try { baseUrl = new URL(options.baseUrl); }
    catch { throw new TypeError("baseUrl must be an HTTP(S) URL"); }
    if (!["http:", "https:"].includes(baseUrl.protocol)) throw new TypeError("baseUrl must be an HTTP(S) URL");
    if (typeof options.model !== "string" || options.model.length === 0 || options.model.length > 256) throw new TypeError("model must be non-empty");
    if (typeof options.apiKey !== "string") throw new TypeError("apiKey must be a string");
    if (typeof options.systemPrompt !== "string") throw new TypeError("systemPrompt must be a string");
    const timeoutMs = options.timeoutMs ?? 5000;
    if (!Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30000) throw new TypeError("timeoutMs out of range");
    this.model = options.model;
    this.dialect = options.dialect ?? "openai_chat";
    // Stable task identity, not an attestation of immutable upstream weights.
    // A response must not change the identity between a claim and its judge.
    this.modelIncarnation = createHash("sha256").update(JSON.stringify([
      this.dialect, baseUrl.href, this.model, options.systemPrompt,
    ])).digest("hex");
    this.endpoint = `${options.baseUrl.replace(/\/+$/, "")}/v1/${this.dialect === "anthropic_messages" ? "messages" : "chat/completions"}`;
    this.apiKey = options.apiKey;
    this.systemPrompt = options.systemPrompt;
    this.timeoutMs = timeoutMs;
    this.fetch = options.fetch ?? globalThis.fetch;
  }

  async extract(input: ExtractionProviderInput): Promise<unknown> {
    const signal = AbortSignal.timeout(this.timeoutMs);
    const headers: Record<string, string> = {
      "content-type": "application/json",
      authorization: `Bearer ${this.apiKey}`,
    };
    if (this.dialect === "anthropic_messages") {
      headers["x-api-key"] = this.apiKey;
      headers["anthropic-version"] = "2023-06-01";
    }
    const body = JSON.stringify(this.dialect === "anthropic_messages" ? {
      model: this.model,
      max_tokens: 8192,
      system: `${this.systemPrompt}\n\nRespond with ONLY a JSON object matching this schema. The schema is authoritative for field names and the requested task. Evidence values are exact quote strings copied from the input text; do not generate offsets.\n${JSON.stringify(chatExtractionSchema)}`,
      messages: [{ role: "user", content: JSON.stringify(input) }],
    } : {
      model: this.model,
      messages: [
        { role: "system", content: this.systemPrompt },
        { role: "user", content: JSON.stringify(input) },
      ],
      response_format: {
        type: "json_schema",
        json_schema: { name: "extraction_output", strict: true, schema: chatExtractionSchema },
      },
      temperature: 0,
    });

    let response: Response;
    try {
      response = await this.fetch(this.endpoint, { method: "POST", headers, signal, redirect: "error", body });
    } catch {
      throw new OpenAiChatExtractionError("provider_unavailable");
    }
    if (!response.ok) {
      await response.body?.cancel();
      throw new OpenAiChatExtractionError("provider_unavailable");
    }

    let raw: string;
    try { raw = await response.text(); }
    catch { throw new OpenAiChatExtractionError("provider_unavailable"); }
    let parsedBody: unknown;
    try { parsedBody = JSON.parse(raw); }
    catch { throw new OpenAiChatExtractionError("provider_mismatch"); }
    if (errorEnvelope.safeParse(parsedBody).success) {
      const error = errorEnvelope.parse(parsedBody);
      if (error.error.code === "upstream_quota_exhausted") throw new OpenAiChatExtractionError("provider_unavailable");
    }
    let content: string;
    let incarnation: string;
    if (this.dialect === "anthropic_messages") {
      const parsed = anthropicEnvelope.safeParse(parsedBody);
      if (!parsed.success) throw new OpenAiChatExtractionError("provider_mismatch");
      content = parsed.data.content[0]!.text.trim();
      // Some Messages models append an explanation after their fenced JSON.
      // Only unwrap a leading, complete fence; the JSON object stays strict.
      const fenced = /^```(?:json)?[ \t]*\r?\n([\s\S]*?)\r?\n```(?:\s+[\s\S]*)?$/i.exec(content);
      if (fenced) content = fenced[1]!;
      incarnation = parsed.data.model;
    } else {
      const parsed = responseEnvelope.safeParse(parsedBody);
      if (!parsed.success) throw new OpenAiChatExtractionError("provider_mismatch");
      content = parsed.data.choices[0]!.message.content;
      incarnation = `${parsed.data.model}:${parsed.data.system_fingerprint ?? "nofp"}`;
    }
    let output: unknown;
    try { output = JSON.parse(content); }
    catch { throw new OpenAiChatExtractionError("provider_mismatch"); }
    try { output = normalizeChatExtraction(output, input); }
    catch { throw new OpenAiChatExtractionError("provider_mismatch"); }
    if (this.reportedModelIncarnation !== undefined && this.reportedModelIncarnation !== incarnation) {
      throw new OpenAiChatExtractionError("provider_mismatch");
    }
    this.reportedModelIncarnation = incarnation;
    return output;
  }
}
