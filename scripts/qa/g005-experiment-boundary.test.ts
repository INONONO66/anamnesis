import { expect, test } from "bun:test";
import { createManifest, verifyResult, executeExperiment, type ExperimentSpec } from "./g005-experiment-boundary.ts";

const spec: ExperimentSpec = {
  name: "fixture-check",
  command: ["/usr/bin/printf", "ok"],
  image: { reference: "fixture-image@sha256:" + "a".repeat(64), digest: "sha256:" + "a".repeat(64) },
  source: { files: [{ path: "fixture.txt", sha256: "b".repeat(64) }] },
  input: { bytes: 2, records: 1 },
  resources: { maxBytes: 1024, maxSeconds: 2 },
  expected: { artifact: "result.json", sha256: "c".repeat(64) },
};

test("creates an immutable, deterministic manifest identity", () => {
  const a = createManifest(spec, "2026-09-10T00:00:00.000Z");
  const b = createManifest(spec, "2026-09-10T00:00:00.000Z");
  expect(a).toEqual(b);
  expect(a.run_id).toMatch(/^run-[0-9a-f]{64}$/);
  expect(Object.isFrozen(a)).toBe(true);
  expect(a.command_sha256).toHaveLength(64);
});

test("refuses unsafe or unpinned commands before execution", async () => {
  await expect(executeExperiment({ ...spec, command: ["printf", "ok"] }, () => Promise.resolve({ output: "ok", artifacts: {} }))).rejects.toThrow(/pinned|absolute/);
  await expect(executeExperiment({ ...spec, command: ["/bin/sh", "-c", "echo unsafe"] }, () => Promise.resolve({ output: "ok", artifacts: {} }))).rejects.toThrow(/unsafe/);
});

test("never passes missing, partial, or mismatched artifacts", () => {
  const manifest = createManifest(spec, "2026-09-10T00:00:00.000Z");
  expect(verifyResult(manifest, { output: "ok", artifacts: {} }).outcome).toBe("unknown");
  expect(verifyResult(manifest, { output: "ok", artifacts: { "result.json": "partial" } }).outcome).toBe("fail");
  expect(verifyResult(manifest, { output: "ok", artifacts: { "result.json": "complete" } }).outcome).toBe("fail");
});

test("records cleanup ownership as an explicit verification", () => {
  const manifest = createManifest(spec, "2026-09-10T00:00:00.000Z");
  const result = verifyResult(manifest, { output: "ok", artifacts: { "result.json": "x" }, cleanup: { owned: true, clean: true } });
  expect(result.cleanup).toEqual({ owned: true, clean: true });
  expect(result.outcome).toBe("fail");
});
