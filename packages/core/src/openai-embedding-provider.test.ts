import { describe, expect, test } from "bun:test";
import { EmbeddingError, EmbeddingProfile, validateVector, type EmbeddingProvider } from "./embedding.ts";
import { OpenAiEmbeddingProvider } from "./openai-embedding-provider.ts";

const profile = EmbeddingProfile.parse({ model: "test-embedding", model_incarnation: "a".repeat(64), dimensions: 2,
  document_prefix: "document: ", query_prefix: "query: ", max_input_bytes: 8192, norm: "unit_l2", norm_tolerance: 0.001 });
type Fetch = NonNullable<ConstructorParameters<typeof OpenAiEmbeddingProvider>[0]["fetch"]>;
const options = { baseUrl: "http://embedding.test/", model: profile.model, profile };
const response = (embedding: unknown) => Response.json({ data: [{ index: 0, embedding }] });
const provider = (fetch: Fetch) => new OpenAiEmbeddingProvider({ ...options, fetch });

async function expectFailure(instance: OpenAiEmbeddingProvider, code: string, retryable: boolean, reason = code, detail?: string) {
  const error = await instance.embed("hello", "document").then(() => { throw new Error("expected provider rejection"); }, error => error);
  expect(error).toBeInstanceOf(EmbeddingError);
  expect(error).toMatchObject({ code, retryable, reason, ...(detail === undefined ? {} : { detail }) });
  return error as EmbeddingError;
}

describe("OpenAiEmbeddingProvider", () => {
  test("posts the configured model and prefixed input array with optional bearer authorization", async () => {
    const calls: { url: string; init: RequestInit }[] = [];
    const instance = new OpenAiEmbeddingProvider({ ...options, apiKey: "test-key", fetch: async (url, init) => {
      calls.push({ url: String(url), init: init! });
      return response([0.6, 0.8]);
    } });
    const compatible: EmbeddingProvider = instance;
    const vector = await compatible.embed("hello", "document");
    expect(vector).toEqual([0.6, 0.8]);
    expect(validateVector(vector, profile)).toEqual([0.6, 0.8]);
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe("http://embedding.test/v1/embeddings");
    expect(calls[0]!.init.method).toBe("POST");
    expect(new Headers(calls[0]!.init.headers).get("authorization")).toBe("Bearer test-key");
    expect(new Headers(calls[0]!.init.headers).get("content-type")).toBe("application/json");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ model: profile.model, input: ["document: hello"] });
    expect(calls[0]!.init.signal).toBeInstanceOf(AbortSignal);
    expect(instance.profile).toEqual(profile);
    expect(Object.isFrozen(instance.profile)).toBe(true);
  });

  test("omits authorization without a key and applies the query prefix", async () => {
    const instance = provider(async (_url, init) => {
      expect(new Headers(init!.headers).has("authorization")).toBe(false);
      expect(JSON.parse(String(init!.body))).toEqual({ model: profile.model, input: ["query: hello"] });
      return response([1, 0]);
    });
    expect((await instance.embed("hello", "query")).meta).toEqual({ normalized: false });
  });

  test("normalizes before validation and keeps metadata local to each result", async () => {
    const vectors = [[3, 4], [1, 0]];
    const instance = provider(async () => response(vectors.shift()));
    const normalized = await instance.embed("first", "document");
    const unchanged = await instance.embed("second", "query");
    expect<number[]>(normalized).toEqual([0.6, 0.8]);
    expect(Math.hypot(...normalized)).toBeCloseTo(1, 12);
    expect(normalized.meta).toEqual({ normalized: true });
    expect(unchanged.meta).toEqual({ normalized: false });
    expect(JSON.stringify(normalized)).toBe("[0.6,0.8]");
  });

  test("preserves vectors already within the profile tolerance", async () => {
    const vector = await provider(async () => response([1.0005, 0])).embed("hello", "query");
    expect<number[]>(vector).toEqual([1.0005, 0]);
    expect(vector.meta.normalized).toBe(false);
  });

  for (const vector of [[1], [1, 0, 0], [0, 0], ["1", 0], [null, 0], "not a vector"]) {
    test(`rejects invalid vector ${JSON.stringify(vector)}`, async () => {
      await expectFailure(provider(async () => response(vector)), "invalid_vector", false);
    });
  }

  test("rejects nonfinite numbers in JSON instead of normalizing them", async () => {
    await expectFailure(provider(async () => new Response('{"data":[{"embedding":[1e400,0]}]}')), "invalid_vector", false);
  });

  for (const body of [{}, { data: [] }, { data: [{}] }, { data: null }]) {
    test(`maps missing embedding ${JSON.stringify(body)} to provider_mismatch`, async () => {
      await expectFailure(provider(async () => Response.json(body)), "provider_mismatch", false, "profile_mismatch");
    });
  }

  test("maps malformed JSON to provider_mismatch", async () => {
    await expectFailure(provider(async () => new Response("not json")), "provider_mismatch", false, "profile_mismatch");
  });

  for (const status of [400, 401, 429, 500, 503]) {
    test(`maps HTTP ${status} to retryable provider_unavailable and records the status`, async () => {
      await expectFailure(provider(async () => new Response("failure", { status })), "provider_unavailable", true, "provider_unavailable", `http ${status}`);
    });
  }

  test("maps network failure to retryable provider_unavailable and records the socket code", async () => {
    await expectFailure(provider(async () => { throw new TypeError("connection refused"); }), "provider_unavailable", true, "provider_unavailable", "TypeError");
    await expectFailure(provider(async () => { throw new TypeError("fetch failed", { cause: Object.assign(new Error("refused"), { code: "ECONNREFUSED" }) }); }), "provider_unavailable", true, "provider_unavailable", "ECONNREFUSED");
  });

  test("maps an actual AbortSignal timeout to retryable provider_unavailable", async () => {
    let observedAbort = false;
    const instance = new OpenAiEmbeddingProvider({ ...options, timeoutMs: 1, fetch: async (_url, init) => {
      const signal = init!.signal!;
      return new Promise<Response>((_resolve, reject) => {
        const abort = () => { observedAbort = true; reject(signal.reason); };
        signal.addEventListener("abort", abort, { once: true });
        if (signal.aborted) abort();
      });
    } });
    await expectFailure(instance, "provider_unavailable", true, "provider_unavailable", "timeout 1ms");
    expect(observedAbort).toBe(true);
  }, 1000);

  test("maps a response stream failure to retryable provider_unavailable", async () => {
    await expectFailure(provider(async () => new Response(new ReadableStream({ start(controller) {
      controller.error(new TypeError("connection reset"));
    } }))), "provider_unavailable", true);
  });

  test("bounds response bodies even without a content-length header", async () => {
    let cancelled = false;
    await expectFailure(provider(async () => new Response(new ReadableStream({ start(controller) {
      controller.enqueue(new Uint8Array(256 * 1024 + 1));
    }, cancel() { cancelled = true; } }))), "provider_mismatch", false, "profile_mismatch");
    expect(cancelled).toBe(true);
  });

  test("rejects oversized prefixed UTF-8 input without contacting the provider", async () => {
    let called = false;
    const instance = new OpenAiEmbeddingProvider({ ...options, profile: { ...profile, max_input_bytes: 12 }, fetch: async () => {
      called = true;
      return response([1, 0]);
    } });
    await expectFailure(instance, "input_too_large", false);
    expect(called).toBe(false);
  });

  test("validates operator configuration at construction", () => {
    for (const patch of [{ baseUrl: "file:///tmp/provider" }, { model: "" }, { timeoutMs: 0 }, { timeoutMs: 30001 },
      { profile: { ...profile, dimensions: 0 } }, { model: "different-profile-model" }]) {
      expect(() => new OpenAiEmbeddingProvider({ ...options, ...patch })).toThrow();
    }
  });
});
