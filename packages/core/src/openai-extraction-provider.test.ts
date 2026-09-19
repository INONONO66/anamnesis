import { ModelTask } from "../../protocol/src/extraction.ts";
import { describe, expect, test } from "bun:test";
import { ExtractionProviderError, type ExtractionProvider } from "./extraction.ts";
import { OpenAiChatExtractionProvider } from "./openai-extraction-provider.ts";

const output = {
  task: "claim",
  claims: [{ text: "The sky is blue.", evidence: { start: 0, end: 16, text: "The sky is blue." } }],
  language: "en",
  modality: "text",
} as const;
const response = (content: string, extra: Record<string, unknown> = {}) => Response.json({
  id: "chatcmpl-test", model: "gpt-test", system_fingerprint: "fp-test",
  choices: [{ index: 0, message: { role: "assistant", content }, finish_reason: "stop" }], ...extra,
});
const options = { baseUrl: "http://llm.test/", apiKey: "test-key", model: "gpt-test", systemPrompt: "Extract claims." };

async function expectFailure(instance: OpenAiChatExtractionProvider, code: string, retryable: boolean) {
  const error = await instance.extract({ text: "The sky is blue.", task: "claim" }).then(
    () => { throw new Error("expected provider rejection"); }, error => error,
  );
  expect(error).toBeInstanceOf(ExtractionProviderError);
  expect(error).toMatchObject({ code, reason: code, retryable });
}

describe("OpenAiChatExtractionProvider", () => {
  test("task identity is protocol-valid and stable across claim and judge requests", async () => {
    const instance = new OpenAiChatExtractionProvider({ ...options, fetch: async () => response(JSON.stringify(output)) });
    const identity = instance.modelIncarnation;
    expect(ModelTask.shape.model_incarnation.safeParse(identity).success).toBe(true);
    await instance.extract({ text: "The sky is blue.", task: "claim" });
    expect(instance.modelIncarnation).toBe(identity);
  });

  test("posts the configured chat contract and returns validated output", async () => {
    const calls: { url: string; init: RequestInit }[] = [];
    const instance = new OpenAiChatExtractionProvider({ ...options, fetch: async (url, init) => {
      calls.push({ url: String(url), init: init! });
      return response(JSON.stringify(output));
    } });
    const compatible: ExtractionProvider = instance;
    await expect(compatible.extract({ text: "The sky is blue.", task: "claim" })).resolves.toEqual(output);
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe("http://llm.test/v1/chat/completions");
    expect(calls[0]!.init.method).toBe("POST");
    expect(new Headers(calls[0]!.init.headers).get("authorization")).toBe("Bearer test-key");
    expect(new Headers(calls[0]!.init.headers).get("content-type")).toBe("application/json");
    const body = JSON.parse(String(calls[0]!.init.body));
    expect(body).toMatchObject({
      model: "gpt-test",
      messages: [
        { role: "system", content: "Extract claims." },
        { role: "user", content: JSON.stringify({ text: "The sky is blue.", task: "claim" }) },
      ],
      response_format: { type: "json_schema", json_schema: { name: "extraction_output", strict: true } },
      temperature: 0,
    });
    expect(body.response_format.json_schema.schema).toBeDefined();
    expect(calls[0]!.init.signal).toBeInstanceOf(AbortSignal);
    expect(instance.model).toBe("gpt-test");
    expect(instance.reportedModelIncarnation).toBe("gpt-test:fp-test");
  });

  test("uses no fingerprint marker when the response omits one", async () => {
    const instance = new OpenAiChatExtractionProvider({ ...options, fetch: async () => response(JSON.stringify(output), { system_fingerprint: null }) });
    await expect(instance.extract({ text: "The sky is blue.", task: "claim" })).resolves.toBeDefined();
    expect(instance.reportedModelIncarnation).toBe("gpt-test:nofp");
  });

  test("rejects upstream identity drift instead of changing admitted task identity", async () => {
    let call = 0;
    const instance = new OpenAiChatExtractionProvider({ ...options, fetch: async () => response(JSON.stringify(output), { system_fingerprint: ++call === 1 ? "first" : "changed" }) });
    await instance.extract({ text: "The sky is blue.", task: "claim" });
    await expectFailure(instance, "provider_mismatch", false);
  });

  test("maps quota errors and non-success responses to retryable provider_unavailable", async () => {
    await expectFailure(new OpenAiChatExtractionProvider({ ...options, fetch: async () => Response.json({ error: { code: "upstream_quota_exhausted" } }) }), "provider_unavailable", true);
    await expectFailure(new OpenAiChatExtractionProvider({ ...options, fetch: async () => new Response("failure", { status: 500 }) }), "provider_unavailable", true);
  });

  test("maps network failure and timeout to retryable provider_unavailable", async () => {
    await expectFailure(new OpenAiChatExtractionProvider({ ...options, fetch: async () => { throw new TypeError("connection refused"); } }), "provider_unavailable", true);
    let observedAbort = false;
    const instance = new OpenAiChatExtractionProvider({ ...options, timeoutMs: 1, fetch: async (_url, init) => {
      const signal = init!.signal!;
      return new Promise<Response>((_resolve, reject) => {
        const abort = () => { observedAbort = true; reject(signal.reason); };
        signal.addEventListener("abort", abort, { once: true });
        if (signal.aborted) abort();
      });
    } });
    await expectFailure(instance, "provider_unavailable", true);
    expect(observedAbort).toBe(true);
  }, 1000);

  test("maps malformed or schema-invalid content to non-retryable provider_mismatch", async () => {
    await expectFailure(new OpenAiChatExtractionProvider({ ...options, fetch: async () => response("not json") }), "provider_mismatch", false);
    await expectFailure(new OpenAiChatExtractionProvider({ ...options, fetch: async () => response(JSON.stringify({ task: "claim", claims: [], language: "en", modality: "invalid" })) }), "provider_mismatch", false);
  });

  test("validates constructor configuration", () => {
    expect(() => new OpenAiChatExtractionProvider({ ...options, baseUrl: "file:///tmp/provider" })).toThrow();
    expect(() => new OpenAiChatExtractionProvider({ ...options, model: "" })).toThrow();
    expect(() => new OpenAiChatExtractionProvider({ ...options, timeoutMs: 0 })).toThrow();
    expect(() => new OpenAiChatExtractionProvider({ ...options, timeoutMs: 30001 })).toThrow();
  });
});
