import { expect, test } from "bun:test";
import { HttpExtractionProvider } from "./extraction.ts";
import { Engine } from "./engine.ts";
const incarnation = "a".repeat(64);
test("Engine exposes model-task admission and generation-bound serving seams", () => {
  expect(typeof Reflect.get(Engine.prototype, "createModelTask")).toBe("function");
  expect(typeof Reflect.get(Engine.prototype, "cutoverExtractionGenerationBound")).toBe("function");
  expect(typeof Reflect.get(Engine.prototype, "readExtractionCoverageBound")).toBe("function");
});
async function response(body: unknown, run: (provider: HttpExtractionProvider) => Promise<void>) {
  const server = Bun.serve({ port: 0, fetch: () => Response.json(body) });
  try { await run(new HttpExtractionProvider({ endpoint: server.url.toString(), model: "test", model_incarnation: incarnation, timeout_ms: 1000 })); }
  finally { await server.stop(true); }
}
test("HTTP envelope is strict before retention", async () => {
  await response({ model: "test", model_incarnation: incarnation, output: null, ignored: "data" }, async p => {
    await expect(p.extract({ text: "hello", task: "claim" })).rejects.toMatchObject({ message: "provider_mismatch", detail: "envelope" });
  });
});
test("HTTP envelope identity drift names the failed check (#218)", async () => {
  await response({ model: "other", model_incarnation: incarnation, output: null }, async p => {
    await expect(p.extract({ text: "hello", task: "claim" })).rejects.toMatchObject({ message: "provider_mismatch", detail: "model" });
  });
  await response({ model: "test", model_incarnation: "b".repeat(64), output: null }, async p => {
    await expect(p.extract({ text: "hello", task: "claim" })).rejects.toMatchObject({ message: "provider_mismatch", detail: "incarnation" });
  });
});
test("HTTP task output is ABI checked, not just arbitrary canonical JSON", async () => {
  await response({ model: "test", model_incarnation: incarnation, output: null }, async p => {
    await expect(p.extract({ text: "hello", task: "claim" })).rejects.toMatchObject({ message: "provider_mismatch", detail: "normalize" });
  });
  // A lone surrogate survives JSON transport but is outside the canonical body domain.
  await response({ model: "test", model_incarnation: incarnation, output: "\ud800" }, async p => {
    await expect(p.extract({ text: "hello", task: "claim" })).rejects.toMatchObject({ message: "provider_mismatch", detail: "json" });
  });
});
