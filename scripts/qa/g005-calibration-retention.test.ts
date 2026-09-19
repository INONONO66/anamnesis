import { expect, test } from "bun:test";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { RpcClient } from "../../app/anamnesis/client.ts";

type Unknown = { outcome: "UNKNOWN"; reason: string; evidence: string };
const evidenceRoot = process.env.ANAMNESIS_G005_EVIDENCE_ROOT ?? ".omo/evidence/g005";
const socket = process.env.ANAMNESIS_G005_SOCKET ?? process.env.ANAMNESIS_RUNTIME_SOCKET;
const token = process.env.ANAMNESIS_G005_TOKEN ?? process.env.ANAMNESIS_RUNTIME_TOKEN;
const owned = process.env.ANAMNESIS_G005_OWNED === "true";

async function record(name: string, value: unknown): Promise<void> {
  await mkdir(evidenceRoot, { recursive: true });
  await writeFile(join(evidenceRoot, `${name}.json`), JSON.stringify(value, null, 2) + "\n", { mode: 0o600 });
}
function digest(value: unknown): string {
  return createHash("sha256").update(JSON.stringify(value)).digest("hex");
}
function unavailable(reason: string): Unknown {
  return { outcome: "UNKNOWN", reason, evidence: "runtime-contract" };
}

/**
 * G005 deliberately has no local fixture path. This test only qualifies an
 * installation explicitly handed to it by its owner; an ordinary/shared UDS
 * is never treated as a disposable benchmark database.
 */
test("G005 calibration and bounded scale execute through authenticated Node/UDS", async () => {
  if (!socket || !token) {
    const result = unavailable("authenticated_owned_uds_not_provided");
    await record("calibration-scale", result);
    expect(result.outcome).toBe("UNKNOWN");
    return;
  }
  if (!owned) {
    const result = unavailable("owned_runtime_attestation_required");
    await record("calibration-scale", result);
    expect(result.outcome).toBe("UNKNOWN");
    return;
  }

  const timings: number[] = [];
  const client = await RpcClient.connect(socket, token, "receipt", event => {
    if (event.event === "response" && typeof event.elapsedMs === "number") timings.push(event.elapsedMs);
  });
  try {
    const status = await client.request("status", {});
    expect(status.storage).toBe("available");
    const source = `g005-owned-${randomUUID()}`;
    const started = performance.now();
    const ids: string[] = [];
    for (let i = 0; i < 8; i++) {
      const result = await client.request("remember", {
        episode: { schema: "anamnesis.original-message/1", time: { value: "2026-09-13T00:00:00Z", precision: "second" },
          content: `bounded G005 workload ${i}`, origin: { source, session: source, actor: "qa", record: String(i) }, mass: 1, properties: {} },
        source_revision: String(i), expected_previous_revision_key: null,
      });
      if (result.state === "committed") ids.push(result.id);
    }
    const recall = await client.request("recall", { query: "bounded G005 workload", limit: 4, T: Date.now() });
    const artifact = { schema: "g005-real-runtime/v1", records: ids.length, result_count: recall.results.length,
      elapsed_ms: performance.now() - started, rpc_latency_ms: timings, response_bytes: Buffer.byteLength(JSON.stringify(recall)),
      semantic_writes: true, authenticated: true };
    expect(ids.length).toBe(8);
    expect(artifact.elapsed_ms).toBeLessThan(30_000);
    expect(artifact.response_bytes).toBeGreaterThan(0);
    await record("scale", { ...artifact, sha256: digest(artifact) });
  } finally {
    await client.close();
  }
});

test("G005 calibration is typed UNKNOWN when no configured production path exists", async () => {
  const result = unavailable("calibration_rpc_and_configured_provider_contract_absent");
  await record("calibration", result);
  expect(result).toEqual({ outcome: "UNKNOWN", reason: expect.any(String), evidence: "runtime-contract" });
});

test("G005 retention-aged is typed UNKNOWN without server virtual-clock and age-policy APIs", async () => {
  const result = unavailable("virtual_clock_and_explicit_age_policy_not_exposed_by_authenticated_runtime");
  await record("retention-aged", result);
  expect(result).toEqual({ outcome: "UNKNOWN", reason: expect.any(String), evidence: "runtime-contract" });
});
