import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { ExtractionGateError } from "./extraction-gate.ts";
import { JevGate, JEV_MODEL } from "./jev-gate.ts";

async function fixtureKey(value: unknown = { bearer: "fixture-bearer" }): Promise<{ root: string; file: string }> {
  const root = await fs.mkdtemp(join(tmpdir(), "jev-gate-"));
  const file = join(root, "key.json");
  await fs.writeFile(file, JSON.stringify(value), { mode: 0o600 });
  return { root, file };
}
function response(model: string, content: string, status = 200): Response {
  return new Response(JSON.stringify({ model, choices: [{ message: { content } }] }), { status });
}

test("construction refuses another model before reading the key", async () => {
  let reads = 0;
  const original = fs.readFile;
  fs.readFile = (async (...args: Parameters<typeof fs.readFile>) => { reads++; return original(...args); }) as typeof fs.readFile;
  try { await assert.rejects(JevGate.create({ keyFile: "/never-read", model: "other" }), /only serves/); }
  finally { fs.readFile = original; }
  assert.equal(reads, 0);
});

test("construction enforces HTTPS and the Jev allowlist", async () => {
  const fixture = await fixtureKey({ bearer: "fixture", allowed_models: ["other"] });
  try {
    await assert.rejects(JevGate.create({ keyFile: fixture.file, baseUrl: "http://localhost" }), /https/);
    await assert.rejects(JevGate.create({ keyFile: fixture.file }), /does not allow/);
  } finally { await fs.rm(fixture.root, { recursive: true, force: true }); }
});

test("score sends the fixed model, bearer, truncates input, and clamps scores", async () => {
  const fixture = await fixtureKey({ bearer: "fixture-bearer", allowed_models: [JEV_MODEL, "other"] });
  let request: RequestInit | undefined;
  try {
    const gate = await JevGate.create({ keyFile: fixture.file, fetch: async (_url, init) => {
      request = init;
      return response(JEV_MODEL, "[-1,0.5,2]");
    } });
    const scores = await gate.score([{ id: "a", text: "a".repeat(3000) }, { id: "b", text: "b" }, { id: "c", text: "c" }]);
    assert.deepEqual(scores, [0, 0.5, 1]);
    const body = JSON.parse(String(request?.body)) as { model: string; messages: [{ content: string }, { content: string }] };
    assert.equal(body.model, JEV_MODEL);
    assert.equal(JSON.parse(body.messages[1].content)[0].text.length, 2048);
    assert.equal((request?.headers as Record<string, string>).authorization, "Bearer fixture-bearer");
  } finally { await fs.rm(fixture.root, { recursive: true, force: true }); }
});

test("score maps response, transport, and HTTP failures without exposing secrets", async () => {
  const fixture = await fixtureKey();
  try {
    for (const [status, reason] of [[200, "gate_mismatch"], [429, "gate_unavailable"], [500, "gate_unavailable"], [400, "gate_rejected"]] as const) {
      const gate = await JevGate.create({ keyFile: fixture.file, fetch: async () => status === 200 ? response("other", "[]") : response(JEV_MODEL, "[]", status) });
      await assert.rejects(gate.score([{ id: "a", text: "x" }]), error => error instanceof ExtractionGateError && error.reason === reason);
    }
    const gate = await JevGate.create({ keyFile: fixture.file, fetch: async () => { throw new Error("network fixture"); } });
    await assert.rejects(gate.score([{ id: "a", text: "x" }]), error => error instanceof ExtractionGateError && error.reason === "gate_unavailable");
    const mismatch = await JevGate.create({ keyFile: fixture.file, fetch: async () => response(JEV_MODEL, "[]") });
    await assert.rejects(mismatch.score([{ id: "a", text: "x" }]), /gate_mismatch/);
    assert.ok(!JSON.stringify(mismatch).includes("fixture-bearer"));
    assert.ok(!Bun.inspect(mismatch).includes("fixture-bearer"));
  } finally { await fs.rm(fixture.root, { recursive: true, force: true }); }
});
