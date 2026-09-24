import { ExtractionModelOutput } from "../../protocol/src/extraction.ts";
import { readFile } from "node:fs/promises";
import { describe, expect, test } from "bun:test";
import { OpenAiChatExtractionProvider } from "./openai-extraction-provider.ts";
import { ExtractionProviderError, validateModelOutput, validateSourceSpans } from "./extraction.ts";

const input = { task: "claim", text: "Alice works here." } as const;
const output = { task: "claim", claims: [{ text: input.text, evidence: { start: 0, end: 17, text: input.text } }], language: "en", modality: "text" };
const options = { baseUrl: "http://llm.test/", apiKey: "fixture-key", model: "claude-haiku-4-5", systemPrompt: "Extract claims.", dialect: "anthropic_messages" as const };
const response = (text: string) => Response.json({ id: "msg_fixture", model: "claude-haiku-4-5-20251001", content: [{ type: "text", text }] });
async function failure(fetch: NonNullable<ConstructorParameters<typeof OpenAiChatExtractionProvider>[0]["fetch"]>, reason: string, timeoutMs = 5000, detail?: string) {
  const provider = new OpenAiChatExtractionProvider({ ...options, fetch, timeoutMs });
  const error = await provider.extract(input).then(() => { throw new Error("expected rejection"); }, error => error);
  expect(error).toBeInstanceOf(ExtractionProviderError);
  expect(error).toMatchObject({ reason, code: reason, retryable: reason === "provider_unavailable", detail });
}
describe("Anthropic Messages extraction dialect", () => {
  test("posts Messages headers, schema and user input; uses the dated model identity", async () => {
    const provider = new OpenAiChatExtractionProvider({ ...options, fetch: async (url, init) => {
      expect(String(url)).toBe("http://llm.test/v1/messages");
      expect(init!.method).toBe("POST");
      const headers = new Headers(init!.headers);
      expect(headers.get("authorization") === `Bearer ${options.apiKey}`).toBe(true);
      expect(headers.get("x-api-key") === options.apiKey).toBe(true);
      expect(headers.get("anthropic-version")).toBe("2023-06-01");
      const body = JSON.parse(String(init!.body));
      expect(body.model).toBe(options.model);
      expect(body.max_tokens).toBeGreaterThan(0);
      expect(body.messages).toEqual([{ role: "user", content: JSON.stringify(input) }]);
      expect(body.response_format).toBeUndefined();
      const schema = JSON.parse(body.system.slice(body.system.indexOf('{')));
      expect(schema.$schema).toBe("https://json-schema.org/draft/2020-12/schema");
      expect(schema.oneOf ?? schema.anyOf).toBeDefined();
      return response(JSON.stringify(output));
    } });
    expect(await provider.extract(input)).toEqual(output);
    expect(provider.reportedModelIncarnation).toBe("claude-haiku-4-5-20251001");
  });
  test("a Messages reply from an unrelated model is refused, naming the model check", async () => {
    await failure(async () => Response.json({ id: "msg_fixture", model: "claude-sonnet-4-5-20250929", content: [{ type: "text", text: JSON.stringify(output) }] }), "provider_mismatch", 5000, "model");
  });
  test("accepts fenced JSON without weakening output validation", async () => {
    const provider = new OpenAiChatExtractionProvider({ ...options, fetch: async () => response('```json\n' + JSON.stringify(output) + '\n```') });
    expect(await provider.extract(input)).toEqual(output);
  });
  test("normalizes the actual Haiku fenced response with trailing explanation", async () => {
    const raw = await readFile(new URL("./fixtures/haiku-extraction-response.txt", import.meta.url), "utf8");
    const provider = new OpenAiChatExtractionProvider({ ...options, fetch: async () => response(raw) });
    expect(await provider.extract(input)).toEqual({ task: "claim", claims: [], language: "English", modality: "text" });
  });
  test("computes evidence offsets from the actual Haiku quotes, not its incorrect offsets", async () => {
    const raw = await readFile(new URL("./fixtures/haiku-extraction-offsets.txt", import.meta.url), "utf8");
    const provider = new OpenAiChatExtractionProvider({ ...options, fetch: async () => response(raw) });
    const text = "Alice works on the Anamnesis project. Alice decided to store project backups every Friday.";
    const result = ExtractionModelOutput.parse(await provider.extract({ task: "claim", text }));
    if (result.task !== "claim") throw new Error("expected claim output");
    expect(result.claims).toHaveLength(2);
    expect(result.claims[0]!.evidence).toEqual({ start: 0, end: 37, text: "Alice works on the Anamnesis project." });
    expect(() => validateSourceSpans(text, result.claims.map(claim => claim.evidence))).not.toThrow();
  });
  test("quote-only evidence maps UTF-8 offsets, including a repeated quote", async () => {
    const text = "先頭🙂 Alice works here. Alice works here.";
    const provider = new OpenAiChatExtractionProvider({ ...options, fetch: async () => response(JSON.stringify({ ...output, claims: [{ text: input.text, evidence: input.text }] })) });
    const result = ExtractionModelOutput.parse(await provider.extract({ task: "claim", text }));
    if (result.task !== "claim") throw new Error("expected claim output");
    expect(result.claims[0]!.evidence).toEqual({ start: 11, end: 28, text: input.text });
    expect(() => validateSourceSpans(text, result.claims.map(claim => claim.evidence))).not.toThrow();
  });
  test("reanchors multi-byte evidence with wrong or garbage offsets and drops unsupported claims", async () => {
    const quote = "한글 텍스트 with emoji 🚀 and code";
    const text = `앞머리🙂 ${quote} / ${quote}`;
    const first = Buffer.from(text).indexOf(Buffer.from(quote));
    const last = Buffer.from(text).lastIndexOf(Buffer.from(quote));
    const provider = new OpenAiChatExtractionProvider({ ...options, fetch: async () => response(JSON.stringify({ ...output, claims: [
      { text: "first", evidence: { text: quote, start: text.indexOf(quote), end: text.indexOf(quote) + quote.length } },
      { text: "last", evidence: { text: quote, start: last + 1, end: -999 } },
      { text: "unsupported", evidence: { text: "not in source", start: -999, end: 999999 } },
    ] })) });
    const result = ExtractionModelOutput.parse(await provider.extract({ task: "claim", text }));
    if (result.task !== "claim") throw new Error("expected claims");
    expect(result.claims.map(claim => claim.evidence)).toEqual([
      { text: quote, start: first, end: first + Buffer.byteLength(quote) },
      { text: quote, start: last, end: last + Buffer.byteLength(quote) },
    ]);
    expect(() => validateSourceSpans(text, result.claims.map(claim => claim.evidence))).not.toThrow();
  });
  test("unsupported evidence suppresses the claim instead of inventing a source quote", async () => {
    const provider = new OpenAiChatExtractionProvider({ ...options, fetch: async () => response(JSON.stringify({ ...output, claims: [{ text: "unsupported", evidence: "not in source" }] })) });
    const result = await provider.extract(input);
    expect(result).toEqual({ ...output, claims: [] });
    expect(validateModelOutput(result, "claim").disposition).toBe("suppress");
  });
  test("rejects empty evidence", async () => {
    await failure(async () => response(JSON.stringify({ ...output, claims: [{ text: input.text, evidence: "" }] })), "provider_mismatch", 5000, "normalize");
  });
  test("fence normalization still rejects extra fields and invalid claim shapes", async () => {
    for (const invalid of [{ ...output, extra: true }, { ...output, claims: null }, { claims: output.claims }]) {
      await failure(async () => response('```json\n' + JSON.stringify(invalid) + '\n```\nExplanation.'), "provider_mismatch", 5000, "normalize");
    }
  });
  for (const status of [429, 500, 503]) test(`HTTP ${status} is retryable`, async () => {
    await failure(async () => new Response("unavailable", { status }), "provider_unavailable");
  });
  test("quota and network errors are retryable", async () => {
    await failure(async () => Response.json({ error: { code: "upstream_quota_exhausted" } }), "provider_unavailable");
    await failure(async () => { throw new TypeError("network failure"); }, "provider_unavailable");
  });
  test("timeout is retryable", async () => {
    await failure(async (_url, init) => new Promise((_resolve, reject) => {
      const signal = init!.signal!;
      signal.addEventListener("abort", () => reject(signal.reason), { once: true });
      if (signal.aborted) reject(signal.reason);
    }), "provider_unavailable", 1);
  }, 1000);
  test("malformed envelopes, JSON, schema and mismatched tasks fail closed, naming the failed check", async () => {
    // The attempt row records which check refused the response (#218): the content JSON, its shape, or the envelope.
    await failure(async () => response("not json"), "provider_mismatch", 5000, "json");
    for (const body of ["{}", JSON.stringify({ ...output, task: "judge" }), JSON.stringify({ ...output, modality: "invalid" })]) {
      await failure(async () => response(body), "provider_mismatch", 5000, "normalize");
    }
    await failure(async () => Response.json({ model: "claude", content: [] }), "provider_mismatch", 5000, "envelope");
    await failure(async () => new Response("not json"), "provider_mismatch", 5000, "envelope");
  });
});
